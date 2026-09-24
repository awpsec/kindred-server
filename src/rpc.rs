use crate::{config::Vm, vm};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::VecDeque, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub struct Rpc {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    queue: VecDeque<Value>,
    next_id: u64,
}
fn error_message(message: &str) -> String {
    // RPC error messages can contain an entire upstream HTML challenge page.
    // Never forward its scripts, cookies, or challenge tokens to chat/settings.
    let lower = message.to_ascii_lowercase();
    let forbidden = lower.contains("403 forbidden") || lower.contains("status 403");
    if lower.contains("_cf_chl") || lower.contains("challenge-platform") {
        return format!(
            "OpenAI blocked this request with a browser security check{}.",
            if forbidden { " (HTTP 403)" } else { "" }
        );
    }
    if lower.contains("401 unauthorized") || lower.contains("status 401") {
        return "OpenAI rejected the Codex session (HTTP 401). Check connection in Settings > Connections.".into();
    }
    if forbidden {
        return "OpenAI denied this request (HTTP 403). Check the Codex account's access to this feature.".into();
    }
    if ["<!doctype html", "<html", "<body", "<script", "<style"]
        .iter()
        .any(|tag| lower.contains(tag))
    {
        return "OpenAI returned an error page instead of a valid response.".into();
    }
    let text: String = message
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .take(401)
        .collect();
    let mut bounded: String = text.chars().take(400).collect();
    if text.chars().count() > 400 {
        bounded.push('…');
    }
    bounded.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn guest_command(binary: &str) -> String {
    // The VM config validates binary as a single safe executable path.
    // A successful initialize/account read does not exercise code mode.
    // This check uses no credentials, model calls or real Kindred tools.
    let probe = include_str!("../deploy/check-codex.py").replace('\'', "'\"'\"'");
    format!(
        "python3 -c '{probe}' {binary} >/dev/null 2>&1 || exit 78; exec env -u OPENAI_API_KEY CODEX_HOME=/home/bot/.local/share/kindred/codex {binary} -c forced_login_method='\"chatgpt\"' -c features.shell_tool=false -c features.unified_exec=false -c web_search='\"disabled\"' app-server"
    )
}
impl Rpc {
    pub async fn connect(config: &Vm) -> Result<Self> {
        let mut cmd = vm::ssh(config);
        // Dedicated guest auth store; never touch the operator's regular Codex installation.
        cmd.arg(guest_command(&config.codex_binary));
        Self::spawn(cmd).await
    }
    pub async fn spawn(mut cmd: Command) -> Result<Self> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .context("start Codex app-server inside the VM")?;
        let mut rpc = Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            queue: VecDeque::new(),
            next_id: 0,
        };
        rpc.request("initialize",json!({"clientInfo":{"name":"kindred","title":"Kindred","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        rpc.send(json!({"method":"initialized","params":{}}))
            .await?;
        Ok(rpc)
    }
    pub async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        if let Err(error) = async {
            self.input.write_all(&bytes).await?;
            self.input.flush().await
        }
        .await
        {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return Err(self.disconnected().await);
            }
            return Err(error.into());
        }
        Ok(())
    }
    async fn read(&mut self) -> Result<Value> {
        // read_until by chunks with a cap; don't permit a runaway peer to allocate forever.
        let mut line = Vec::new();
        loop {
            let available = self.output.fill_buf().await?;
            if available.is_empty() {
                return Err(self.disconnected().await);
            }
            let n = available
                .iter()
                .position(|&x| x == b'\n')
                .map(|n| n + 1)
                .unwrap_or(available.len());
            ensure!(
                line.len() + n <= 8 * 1024 * 1024,
                "Codex frame exceeds limit"
            );
            let done = available[n - 1] == b'\n';
            line.extend_from_slice(&available[..n]);
            self.output.consume(n);
            if done {
                return serde_json::from_slice(&line).context("invalid Codex protocol frame");
            }
        }
    }
    async fn disconnected(&mut self) -> anyhow::Error {
        // SSH propagates the guest's exit code. Classify known startup failures
        // without displaying stderr, which can contain provider credentials.
        let code = tokio::time::timeout(Duration::from_millis(250), self.child.wait())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|status| status.code());
        anyhow::anyhow!(match code {
            Some(127) =>
                "Codex is not installed on the bot computer. If first setup is still running, wait for it to finish and retry sign-in. Otherwise ask the server administrator to repair the guest software installation.",
            Some(126) =>
                "The bot computer cannot run the installed Codex executable. Ask the server administrator to check its permissions and CPU architecture.",
            Some(78) =>
                "Codex's tool runtime is incomplete or failed its offline check. Memory and other tools cannot run. Ask the server administrator to repair the complete guest Codex package, then retry. Signing in again will not repair this.",
            Some(255) =>
                "Kindred could not keep an SSH connection to the bot computer. Check that the computer is running and its SSH configuration is valid, then retry sign-in.",
            _ =>
                "Codex stopped responding on the bot computer. Try Check connection or sign in again; if it repeats, ask the server administrator to check the guest's Codex installation.",
        })
    }
    pub async fn next(&mut self) -> Result<Value> {
        if let Some(v) = self.queue.pop_front() {
            Ok(v)
        } else {
            self.read().await
        }
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_inner(method, params, false).await
    }
    /// Auxiliary connector sessions never delegate provider prompts or approvals.
    /// A provider challenge stops this call; callers must not retry an uncertain write.
    pub async fn request_guarded(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_inner(method, params, true).await
    }
    async fn request_inner(&mut self, method: &str, params: Value, guarded: bool) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let v = self.read().await?;
                if guarded && v.get("id").is_some() && v.get("method").is_some() {
                    self.send(json!({"id":v["id"],"error":{"code":-32601,"message":"Kindred requires interactive provider sign-in outside connector calls; this request is not approved."}})).await?;
                    bail!("The provider requested an additional interactive step. Reconnect in Settings. This call stopped; do not retry an uncertain write.");
                }
                if v["id"] == id && v.get("method").is_none() {
                    if let Some(e) = v.get("error") {
                        bail!(
                            "Codex rejected {method}: {}",
                            error_message(e["message"].as_str().unwrap_or("protocol error"))
                        );
                    }
                    return Ok(v["result"].clone());
                }
                ensure!(
                    self.queue.len() < 1024,
                    "Codex pending frame limit exceeded"
                );
                self.queue.push_back(v);
            }
        })
        .await
        .context("Codex request timed out")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_error_pages_are_not_forwarded_and_plain_errors_are_bounded() {
        let challenge = "Request failed with status 403 Forbidden: <html><script>window._cf_chl_opt='private-challenge'</script></html>";
        assert_eq!(
            error_message(challenge),
            "OpenAI blocked this request with a browser security check (HTTP 403)."
        );
        assert!(
            !error_message("status 401 Unauthorized: <html>private-token</html>")
                .contains("private-token")
        );
        assert!(
            error_message("status 403 Forbidden: <html>private-token</html>").contains("HTTP 403")
        );
        assert_eq!(
            error_message("status 502: <HTML><STYLE>private-body</STYLE></HTML>"),
            "OpenAI returned an error page instead of a valid response."
        );
        assert_eq!(
            error_message("Unknown method app/installed"),
            "Unknown method app/installed"
        );
        let bounded = error_message(&"é".repeat(5000));
        assert_eq!(bounded.chars().count(), 401);
        assert!(bounded.ends_with('…'));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn incomplete_runtime_stops_before_starting_provider_or_authentication() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!("kindred-preflight-{}", crate::db::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let binary = directory.join("codex");
        let marker = directory.join("started");
        std::fs::write(
            &binary,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", &guest_command(binary.to_str().unwrap())]);
        let error = Rpc::spawn(command)
            .await
            .err()
            .expect("incomplete runtime must fail")
            .to_string();
        assert!(
            error.contains("Memory and other tools cannot run"),
            "{error}"
        );
        assert!(!marker.exists(), "must not launch an incomplete provider");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn codex_startup_errors_identify_missing_software_and_ssh_without_leaking_stderr() {
        for (code, expected) in [
            (127, "not installed"),
            (126, "CPU architecture"),
            (78, "Memory and other tools cannot run"),
            (255, "SSH connection"),
            (1, "stopped responding"),
        ] {
            let mut command = Command::new("python3");
            command.args(["-c", "import sys; sys.stdin.readline(); print('private-provider-secret', file=sys.stderr); sys.exit(int(sys.argv[1]))", &code.to_string()]);
            let error = Rpc::spawn(command)
                .await
                .err()
                .expect("startup must fail")
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("private-provider-secret"));
        }
    }
}
