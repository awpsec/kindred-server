use crate::vm::capture;
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string {key}"))
}
fn number(v: &Value, key: &str, max: i64) -> Result<String> {
    let n = v
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("missing integer {key}"))?;
    ensure!((0..=max).contains(&n), "{key} outside supported range");
    Ok(n.to_string())
}
fn keyboard_shortcut(key: &str) -> Result<String> {
    ensure!(
        !key.is_empty() && key.len() < 100,
        "Invalid key combination"
    );
    key.split('+')
        .map(|part| {
            ensure!(
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "Invalid key combination"
            );
            let lower = part.to_ascii_lowercase();
            Ok(match lower.as_str() {
                "ctrl" | "control" | "control_l" => "Control_L".into(),
                "control_r" => "Control_R".into(),
                "alt" | "alt_l" => "Alt_L".into(),
                "alt_r" => "Alt_R".into(),
                "shift" | "shift_l" => "Shift_L".into(),
                "shift_r" => "Shift_R".into(),
                "super" | "super_l" | "win" | "windows" => "Super_L".into(),
                "super_r" => "Super_R".into(),
                "enter" | "return" => "Return".into(),
                "esc" | "escape" => "Escape".into(),
                "tab" => "Tab".into(),
                "space" | "spacebar" => "space".into(),
                "backspace" => "BackSpace".into(),
                "delete" | "del" => "Delete".into(),
                "insert" | "ins" => "Insert".into(),
                "home" => "Home".into(),
                "end" => "End".into(),
                "pageup" | "page_up" | "pgup" | "prior" => "Prior".into(),
                "pagedown" | "page_down" | "pgdn" | "next" => "Next".into(),
                "up" | "arrowup" => "Up".into(),
                "down" | "arrowdown" => "Down".into(),
                "left" | "arrowleft" => "Left".into(),
                "right" | "arrowright" => "Right".into(),
                _ if lower.len() == 1 => lower,
                _ if lower
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u8>().ok())
                    .is_some_and(|n| (1..=35).contains(&n)) =>
                {
                    lower.to_ascii_uppercase()
                }
                _ => part.into(),
            })
        })
        .collect::<Result<Vec<String>>>()
        .map(|parts| parts.join("+"))
}
// Chromium forwards to an existing profile and exits, or owns a new persistent
// browser process. Waiting for that process to exit is not a navigation check.
async fn launch_browser(mut command: Command) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    if let Some(status) = child.try_wait()? {
        ensure!(
            status.success(),
            "Browser launch exited {status}; inspect the computer before retrying"
        );
    }
    Ok(())
}
async fn command_result(cmd: Command, seconds: u64) -> Value {
    let start = std::time::Instant::now();
    let result = crate::vm::capture_output(cmd, None, seconds, 65536).await;
    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(output) => {
            let code = output.status.code();
            let timed_out = code == Some(124);
            let status = if timed_out {
                "Command timed out"
            } else if output.status.success() {
                "Command completed successfully"
            } else {
                "Command failed"
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            json!({"text":format!("{status} (exit code {}, elapsed {elapsed:.2}s).\n{stdout}{}",code.map(|c|c.to_string()).unwrap_or_else(||"signal".into()),if stderr.is_empty(){String::new()}else{format!("\nStandard error:\n{stderr}")}),"failed":!output.status.success(),"exit_code":code,"elapsed_seconds":elapsed,"timed_out":timed_out})
        }
        Err(error) => {
            json!({"text":error.to_string(),"failed":true,"elapsed_seconds":elapsed,"timed_out":error.to_string().contains("timed out")})
        }
    }
}
pub async fn rpc() -> Result<()> {
    let mut input = Vec::new();
    tokio::io::stdin()
        .take(65537)
        .read_to_end(&mut input)
        .await?;
    ensure!(input.len() <= 65536, "request too large");
    let result = match serde_json::from_slice::<Value>(&input) {
        Ok(request) => {
            match execute_screen(
                request["tool"].as_str().unwrap_or(""),
                &request["args"],
                request["screen"].as_i64().unwrap_or(1),
            )
            .await
            {
                Ok(v) => v,
                Err(e) => json!({"error":e.to_string()}),
            }
        }
        Err(_) => json!({"error":"invalid JSON request"}),
    };
    let mut output = tokio::io::stdout();
    output.write_all(&serde_json::to_vec(&result)?).await?;
    output.flush().await?;
    Ok(())
}
pub async fn execute_screen(tool: &str, args: &Value, screen: i64) -> Result<Value> {
    ensure!((1..=32).contains(&screen), "Invalid screen");
    ensure!(
        std::env::var("KINDRED_GUEST").as_deref() == Ok("1"),
        "guest tools require KINDRED_GUEST=1 inside the dedicated VM"
    );
    let display = format!(":{screen}");
    if tool.starts_with("computer_") && tool != "computer_resources" {
        let mut cmd = Command::new("/usr/local/lib/kindred/ensure-screen");
        cmd.arg(screen.to_string());
        capture(cmd, None, 30, 4096).await?;
    }
    let browser = if screen == 1 {
        "/home/bot/.local/share/kindred/browser".to_string()
    } else {
        format!("/home/bot/.local/share/kindred/browser-{screen}")
    };
    match tool {
        "screen_ensure" => {
            let mut cmd = Command::new("/usr/local/lib/kindred/ensure-screen");
            cmd.arg(screen.to_string());
            capture(cmd, None, 30, 4096).await?;
            Ok(json!({"ready":true}))
        }
        "computer_resources" => crate::resources::sample().await,
        "computer_open_url" => {
            let url = reqwest::Url::parse(text(args, "url")?)?;
            ensure!(
                url.scheme() == "https"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.as_str().len() <= 2000,
                "Expected HTTPS URL without embedded credentials"
            );
            let mut cmd = Command::new("chromium");
            cmd.env("DISPLAY", display).args([
                "--no-first-run",
                &format!("--user-data-dir={browser}"),
                url.as_str(),
            ]);
            launch_browser(cmd).await?;
            Ok(
                json!({"text":"Navigation requested in the persistent browser. Inspect a fresh screenshot and verify the page loaded before continuing; this response alone does not prove it loaded."}),
            )
        }
        "command_start" | "command_poll" => {
            let root = std::path::PathBuf::from(std::env::var("HOME")?)
                .join(".local/share/kindred/commands");
            if tool == "command_start" {
                return crate::managed_process::start(&root, text(args, "process_id")?, args)
                    .map_err(|e| anyhow::anyhow!(e.to_string()));
            }
            let jobs = args["commands"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing commands"))?;
            ensure!(jobs.len() <= 16, "Too many commands");
            let receipts = jobs
                .iter()
                .map(|job| {
                    let id = text(job, "id")?;
                    let result = if job["stop"] == true {
                        crate::managed_process::stop(&root, id)
                    } else {
                        crate::managed_process::status(&root, id)
                    };
                    result.map_err(|e| anyhow::anyhow!(e.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(json!({"commands":receipts}))
        }
        "guest_exec" => {
            let command = text(args, "command")?;
            ensure!(command.len() <= 32000, "command too long");
            let mut cmd = Command::new("timeout");
            cmd.args([
                "--signal=TERM",
                "--kill-after=2s",
                "60s",
                "sh",
                "-lc",
                command,
            ])
            .current_dir("/workspace");
            if args["use_desktop"] == true {
                cmd.env("DISPLAY", &display).env("KINDRED_BROWSER_PROFILE", &browser);
            } else {
                cmd.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").env_remove("KINDRED_BROWSER_PROFILE");
            }
            if let Some(zone) = args["timezone"].as_str() {
                cmd.env("TZ", crate::timezone::parse(zone)?.name());
            }
            Ok(command_result(cmd, 65).await)
        }
        "computer_screenshot" => {
            let mut cmd = Command::new("import");
            cmd.args([
                "-display",
                &display,
                "-window",
                "root",
                "-resize",
                "1280x800>",
                "png:-",
            ]);
            let png = capture(cmd, None, 10, 5 * 1024 * 1024).await?;
            Ok(
                json!({"text":"Current shared VM display. Coordinates refer to the 1280x800 screen.","image":format!("data:image/png;base64,{}",STANDARD.encode(png))}),
            )
        }
        "computer_click" | "computer_type" | "computer_key" | "computer_scroll" => {
            let mut cmd = Command::new("xdotool");
            cmd.env("DISPLAY", display);
            match tool {
                "computer_click" => {
                    let x = number(args, "x", 1279)?;
                    let y = number(args, "y", 799)?;
                    let b = args["button"].as_i64().unwrap_or(1);
                    ensure!((1..=3).contains(&b), "button must be 1..3");
                    cmd.args(["mousemove", "--sync", &x, &y, "click", &b.to_string()]);
                }
                "computer_type" => {
                    let value = text(args, "text")?;
                    ensure!(value.len() <= 16000, "typed text too long");
                    cmd.args(["type", "--clearmodifiers", "--delay", "1", "--", value]);
                }
                "computer_key" => {
                    let key = keyboard_shortcut(text(args, "key")?)?;
                    cmd.args(["key", "--clearmodifiers", &key]);
                }
                _ => {
                    let direction = text(args, "direction")?;
                    ensure!(
                        matches!(direction, "up" | "down"),
                        "direction must be up/down"
                    );
                    let n = args["clicks"].as_i64().unwrap_or(3);
                    ensure!((1..=20).contains(&n), "clicks must be 1..20");
                    cmd.args([
                        "click",
                        "--repeat",
                        &n.to_string(),
                        "--delay",
                        "50",
                        if direction == "up" { "4" } else { "5" },
                    ]);
                }
            }
            let output = crate::vm::capture_output(cmd, None, 30, 4096).await?;
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            ensure!(
                output.status.success() && !diagnostic.contains("No such key name"),
                "Computer input failed: {diagnostic}"
            );
            Ok(
                json!({"text":"Action completed. Inspect a fresh screenshot before the next action."}),
            )
        }
        _ => bail!("unknown guest tool"),
    }
}

#[cfg(all(test, unix))]
mod browser_tests {
    use super::*;
    #[test]
    fn keyboard_aliases_match_linux_keysyms() {
        for (input, expected) in [
            ("CTRL+L", "Control_L+l"),
            ("ENTER", "Return"),
            ("ctrl+SHIFT+TAB", "Control_L+Shift_L+Tab"),
            ("Alt+F4", "Alt_L+F4"),
            ("ESC", "Escape"),
            ("PGDN", "Next"),
            ("Control_L+period", "Control_L+period"),
        ] {
            assert_eq!(keyboard_shortcut(input).unwrap(), expected);
        }
        for input in ["", "Ctrl++L", "Return;shutdown", "Ctrl+ L"] {
            assert!(keyboard_shortcut(input).is_err());
        }
    }
    #[tokio::test]
    async fn command_receipts_distinguish_empty_success_failure_output_and_timeout() {
        for (script, code, expected) in [
            ("pass", 0, "completed successfully"),
            (
                "print('partial output'); raise SystemExit(23)",
                23,
                "partial output",
            ),
        ] {
            let mut cmd = Command::new("python3");
            cmd.args(["-c", script]);
            let result = command_result(cmd, 5).await;
            assert_eq!(result["exit_code"], code);
            assert_eq!(result["failed"], code != 0);
            assert!(result["text"].as_str().unwrap().contains(expected));
            assert!(result["elapsed_seconds"].is_number());
        }
        let mut cmd = Command::new("timeout");
        cmd.args(["--kill-after=1s", ".05s", "sleep", "5"]);
        let result = command_result(cmd, 3).await;
        assert_eq!(result["exit_code"], 124);
        assert_eq!(result["timed_out"], true);
        assert_eq!(result["failed"], true);
    }
    #[tokio::test]
    async fn fresh_browser_process_is_not_waited_on_or_killed_after_launch() {
        let marker = std::env::temp_dir().join(format!("kindred-browser-{}", crate::db::id()));
        let mut command = Command::new("python3");
        command.args(["-c","import pathlib,sys,time; time.sleep(.6); pathlib.Path(sys.argv[1]).write_text('still running')",marker.to_str().unwrap()]);
        let start = std::time::Instant::now();
        launch_browser(command).await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(550));
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "still running");
        std::fs::remove_file(marker).unwrap();
        let mut failure = Command::new("python3");
        failure.args(["-c", "raise SystemExit(23)"]);
        assert!(
            launch_browser(failure)
                .await
                .unwrap_err()
                .to_string()
                .contains("23")
        );
    }
}
