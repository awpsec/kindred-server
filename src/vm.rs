use crate::config::Vm;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub fn ssh(config: &Vm) -> Command {
    ssh_with_forward(config, None)
}
/// Command and desktop channels must use the same profile-scoped SSH identity.
pub fn ssh_with_forward(config: &Vm, port: Option<u16>) -> Command {
    let mut cmd = Command::new("ssh");
    if !config.ssh_config.is_empty() {
        cmd.args(["-F", &config.ssh_config]);
    }
    cmd.args([
        "-T",
        "-oBatchMode=yes",
        "-oStrictHostKeyChecking=yes",
        "-oConnectTimeout=10",
        "-oServerAliveInterval=15",
        "-oServerAliveCountMax=2",
    ]);
    if let Some(port) = port {
        cmd.args(["-W", &format!("127.0.0.1:{port}")]);
    }
    cmd.args(["--", &config.ssh_alias]);
    cmd.kill_on_drop(true);
    cmd
}

/// Arguments are fixed or passed as argv. Model text is sent only on stdin to the guest.
pub async fn capture(
    cmd: Command,
    input: Option<Vec<u8>>,
    seconds: u64,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let output = capture_output(cmd, input, seconds, max_bytes).await?;
    ensure!(
        output.status.success(),
        "subprocess exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
pub async fn capture_output(
    mut cmd: Command,
    input: Option<Vec<u8>>,
    seconds: u64,
    max_bytes: usize,
) -> Result<std::process::Output> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn().context("start subprocess")?;
    let mut out = child.stdout.take().unwrap().take(max_bytes as u64 + 1);
    let mut err = child.stderr.take().unwrap().take(8193);
    let writer = async {
        if let Some(input) = input {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    // Finish stdin before borrowing child for wait. Guest requests are bounded to 64 KB.
    tokio::time::timeout(Duration::from_secs(10), writer).await??;
    let work = async {
        let read_out = async {
            let mut b = Vec::new();
            out.read_to_end(&mut b).await?;
            ensure!(b.len() <= max_bytes, "subprocess output limit exceeded");
            Ok::<_, anyhow::Error>(b)
        };
        let read_err = async {
            let mut b = Vec::new();
            err.read_to_end(&mut b).await?;
            ensure!(b.len() <= 8192, "subprocess error output limit exceeded");
            Ok::<_, anyhow::Error>(b)
        };
        let (stdout, stderr, status) = tokio::try_join!(read_out, read_err, async {
            Ok::<_, anyhow::Error>(child.wait().await?)
        })?;
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };
    match tokio::time::timeout(Duration::from_secs(seconds), work).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            bail!(
                "subprocess timed out after {seconds}s; a guest command may take up to its own timeout to stop"
            )
        }
    }
}

#[derive(Debug)]
pub struct UncertainGuestEffect {
    pub timed_out: bool,
    pub detail: String,
}
impl std::fmt::Display for UncertainGuestEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The computer action receipt was lost: {}. Its effect is uncertain. Do not repeat it; inspect the current page to reconcile the result.", self.detail)
    }
}
impl std::error::Error for UncertainGuestEffect {}
fn guest_receipt_error(tool: &str, error: anyhow::Error) -> anyhow::Error {
    // Missing local executable fails before dispatch, unlike a lost remote receipt.
    if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) { return error; }
    if matches!(tool, "computer_open_url" | "computer_click" | "computer_type" | "computer_key" | "computer_scroll" | "guest_exec" | "command_start") {
        UncertainGuestEffect { timed_out: error.to_string().contains("timed out"), detail: error.to_string() }.into()
    } else { error }
}

pub async fn guest(config: &Vm, tool: &str, args: Value) -> Result<Value> {
    guest_screen(config, 1, tool, args).await
}
pub async fn guest_screen(config: &Vm, screen: i64, tool: &str, mut args: Value) -> Result<Value> {
    ensure!((1..=32).contains(&screen), "Invalid screen");
    let mut cmd = ssh(config);
    cmd.arg(format!("{} guest-rpc", config.guest_binary));
    // Older guest binaries set DISPLAY for every shell command. Unset it inside
    // the command as well, so headless semantics do not depend on a guest upgrade.
    if matches!(tool, "guest_exec" | "command_start") && args["use_desktop"] != true {
        if let Some(command) = args["command"].as_str() {
            args["command"] = serde_json::json!(format!("unset DISPLAY WAYLAND_DISPLAY KINDRED_BROWSER_PROFILE; {command}"));
        }
    }
    let input = serde_json::to_vec(&serde_json::json!({"tool":tool,"args":args,"screen":screen}))?;
    ensure!(input.len() <= 65536, "guest request exceeds 64 KB");
    let output = capture(cmd, Some(input), 80, 8 * 1024 * 1024).await.map_err(|e| guest_receipt_error(tool, e))?;
    let value: Value = serde_json::from_slice(&output).context("guest returned invalid JSON").map_err(|e| guest_receipt_error(tool, e))?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        let rejection = anyhow::anyhow!("guest: {error}");
        // Legacy guest input failure can follow partial xdotool execution.
        if error.starts_with("Computer input failed:") { return Err(guest_receipt_error(tool, rejection)); }
        return Err(rejection);
    }
    if matches!(tool, "computer_open_url" | "computer_click" | "computer_type" | "computer_key" | "computer_scroll") && value["text"].as_str().is_none() {
        return Err(guest_receipt_error(tool, anyhow::anyhow!("guest returned no action receipt")));
    }
    Ok(value)
}

pub async fn control(config: &Vm, action: &str) -> Result<String> {
    ensure!(
        matches!(action, "domstate" | "start" | "shutdown" | "reboot"),
        "unsupported VM action"
    );
    if !config.managed_id.is_empty() {
        return Ok(managed(config, action, 600).await?["state"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned());
    }
    let mut cmd = if config.control_helper.is_empty() {
        Command::new("virsh")
    } else {
        Command::new("sudo")
    };
    if config.control_helper.is_empty() {
        cmd.args(["--connect", &config.uri, action, "--domain", &config.domain]);
    } else {
        cmd.args(["-n", "--", &config.control_helper, action]);
    }
    Ok(String::from_utf8(capture(cmd, None, 20, 16384).await?)?
        .trim()
        .to_owned())
}

pub async fn managed(config: &Vm, action: &str, timeout: u64) -> Result<Value> {
    ensure!(
        uuid::Uuid::parse_str(&config.managed_id).is_ok(),
        "Invalid managed computer"
    );
    let mut command = Command::new("python3");
    command.args([&config.manager, action, &config.managed_id]);
    let bytes = capture(command, None, timeout, 65536).await?;
    Ok(serde_json::from_slice(&bytes).context("Invalid computer manager response")?)
}

pub async fn resize_resources(config: &Vm, values: Value) -> Result<Value> {
    ensure!(
        uuid::Uuid::parse_str(&config.managed_id).is_ok(),
        "This computer is managed outside Kindred"
    );
    let mut command = Command::new("python3");
    command.args([&config.manager, "resize-resources", &config.managed_id]);
    let bytes = capture(command, Some(serde_json::to_vec(&values)?), 60, 65536).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn ensure_running(config: &Vm) -> Result<()> {
    if !config.managed_id.is_empty() {
        managed(config, "ensure", 2400).await?;
    }
    Ok(())
}

/// Account/model checks must not launch provider processes while first boot is
/// still installing them. Unlike ensure_running, this never boots a computer.
pub async fn provider_unavailable(config: &Vm, provider: &str) -> Result<Option<Value>> {
    if config.managed_id.is_empty() {
        return Ok(None);
    }
    let state = if std::path::Path::new(&config.ssh_config).is_file() {
        managed(config, "connection-status", 12).await?
    } else {
        serde_json::json!({"setup":"not_started"})
    };
    let setup = state["setup"].as_str().unwrap_or("");
    let message = match setup {
        "ready" => return Ok(None),
        "runtime_failed" if provider != "codex" => return Ok(None),
        "not_started" => "Your bot computer starts when you sign in or send its first message.",
        "stopped" => "Your bot computer is off. Choose Sign in to start it and reconnect.",
        "starting" => {
            "Your bot computer is starting. Account sign-in will be available when setup finishes."
        }
        "installing" => {
            "Your bot computer is installing its software. First setup can take several minutes; sign-in will continue when it is ready."
        }
        "failed" => {
            "Bot computer software setup failed. Ask the server administrator to check this profile computer’s setup log, then retry sign-in after it is repaired."
        }
        "runtime_failed" => {
            "Bot computer Codex tool runtime is incomplete. Memory and other Codex tools cannot run. Ask the server administrator to repair the complete guest Codex package; signing in again will not repair this."
        }
        _ => bail!("Computer manager returned an unknown setup state"),
    };
    Ok(Some(
        serde_json::json!({"installed":false,"connected":false,
        "preparing":matches!(setup,"starting"|"installing"),"setup":setup,
        "data":[],"message":message}),
    ))
}

#[cfg(test)]
mod computer_transport_tests {
    use super::*;
    #[tokio::test]
    async fn dispatched_input_timeout_keeps_uncertainty_but_missing_executable_does_not() {
        let marker = std::env::temp_dir().join(format!("kindred-effect-{}",crate::db::id()));
        let mut cmd=Command::new("python3");
        cmd.args(["-c","import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('applied once'); time.sleep(5)",marker.to_str().unwrap()]);
        let error = capture(cmd,None,1,1024).await.unwrap_err();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(),"applied once");
        let error = guest_receipt_error("computer_click",error);
        assert!(error.downcast_ref::<UncertainGuestEffect>().unwrap().timed_out);
        std::fs::remove_file(marker).unwrap();
        let mut cmd=Command::new("/nonexistent/kindred-fixture-input");cmd.arg("unused");
        let error=guest_receipt_error("computer_click",capture(cmd,None,1,1024).await.unwrap_err());
        assert!(error.downcast_ref::<UncertainGuestEffect>().is_none());
        assert!(guest_receipt_error("computer_screenshot",anyhow::anyhow!("timed out")).downcast_ref::<UncertainGuestEffect>().is_none());
    }
}
