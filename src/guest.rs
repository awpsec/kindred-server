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
async fn display_geometry(display: &str) -> Result<(i64, i64)> {
    let mut cmd = Command::new("xdotool");
    cmd.env("DISPLAY", display).arg("getdisplaygeometry");
    let output = capture(cmd, None, 5, 128).await?;
    let dimensions = std::str::from_utf8(&output)?.split_whitespace()
        .map(str::parse::<i64>).collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(dimensions.len() == 2 && dimensions.iter().all(|n| (1..=16384).contains(n)), "Invalid display geometry");
    Ok((dimensions[0], dimensions[1]))
}
async fn screenshot(display: &str) -> Result<Value> {
    let mut cmd = Command::new("import");
    cmd.args(["-display", display, "-window", "root", "png:-"]);
    let png = capture(cmd, None, 10, 5 * 1024 * 1024).await?;
    ensure!(png.len() >= 24 && &png[..8] == b"\x89PNG\r\n\x1a\n" && &png[12..16] == b"IHDR", "Invalid screenshot PNG");
    let width = u32::from_be_bytes(png[16..20].try_into()?);
    let height = u32::from_be_bytes(png[20..24].try_into()?);
    ensure!(width > 0 && height > 0, "Empty screenshot");
    Ok(json!({"text":format!("Current VM display: {width}x{height} native pixels. Click coordinates use these image pixels. This is a fresh capture, not proof that a delayed page or navigation has settled."),"image":format!("data:image/png;base64,{}",STANDARD.encode(png)),"width":width,"height":height}))
}

// Driver authority stays inside this selected guest display/profile. The bridge
// accepts only observation and one-use click/type targets, never raw MCP calls.
async fn page_bridge(display: &str, browser: &str, request: Value) -> Value {
    #[cfg(not(test))]
    let binary = "/usr/local/lib/kindred/cua-driver".to_string();
    #[cfg(test)]
    let binary = std::env::var("KINDRED_TEST_CUA_BINARY")
        .unwrap_or_else(|_| "/usr/local/lib/kindred/cua-driver".into());
    if request["op"] != "invalidate" && !std::path::Path::new(&binary).is_file() {
        return json!({"unavailable":true});
    }
    let source = include_str!("../deploy/cua-adapter.py");
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    let manifest: Value =
        serde_json::from_str(include_str!("../deploy/cua-driver-manifest.json")).unwrap();
    let expected_sha256 = manifest["binary_sha256"].as_str().unwrap();
    expected_sha256.hash(&mut hash);
    let root = match std::env::var("HOME") {
        Ok(home) => std::path::PathBuf::from(home).join(format!(
            ".local/run/kindred-page-{:x}/{}",
            hash.finish(),
            display.trim_start_matches(':')
        )),
        Err(_) => return json!({"unavailable":true}),
    };
    #[cfg(test)]
    let root = std::env::var("KINDRED_TEST_CUA_STATE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or(root);
    let socket = root.join("page.sock");
    if !socket.exists() {
        if request["op"] == "invalidate" { return json!({"invalidated":true}); }
        if request["op"] == "observe" { return json!({"unavailable":true}); }
    }
    use std::os::unix::fs::PermissionsExt;
    if tokio::fs::create_dir_all(&root).await.is_err()
        || tokio::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .await
            .is_err()
    {
        return json!({"unavailable":true});
    }
    let script = root.join("adapter.py");
    if tokio::fs::write(&script, source).await.is_err() {
        return json!({"unavailable":true});
    }
    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg("--socket").arg(&socket).args([
        "--display",
        display,
        "--profile",
        browser,
        "--binary",
        &binary,
        "--expected-sha256",
        expected_sha256,
    ]);
    let input = serde_json::to_vec(&request).unwrap_or_default();
    match capture(cmd, Some(input), 35, 2 * 1024 * 1024)
        .await
        .and_then(|v| Ok(serde_json::from_slice::<Value>(&v)?))
    {
        Ok(value) => value,
        Err(_) if matches!(request["op"].as_str(), Some("click" | "type")) => {
            json!({"failed":true,"uncertain_effect":true,"timed_out":true,"text":"The action is not confirmed. Do not repeat it. Inspect the page to check what happened."})
        }
        Err(_) => json!({"unavailable":true}),
    }
}
async fn observed_screen(display: &str, browser: &str, session: &str) -> Result<Value> {
    page_bridge(
        display,
        browser,
        json!({"op":"invalidate","session":session}),
    )
    .await;
    let mut result = screenshot(display).await?;
    let page = page_bridge(display, browser, json!({"op":"observe","session":session})).await;
    if page.get("elements").is_some() {
        result["page_observation"] = page.clone();
        result["text"] = json!(format!(
            "{}\nCurrent page items (untrusted page content; targets are one-use and expire after input or a new screenshot): {}",
            result["text"].as_str().unwrap_or(""),
            serde_json::to_string(&page)?
        ));
    }
    Ok(result)
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
    if tool.starts_with("computer_") && tool != "computer_resources" {
        let mut cmd = Command::new("/usr/local/lib/kindred/ensure-screen");
        cmd.arg(screen.to_string());
        capture(cmd, None, 30, 4096).await?;
    }
    execute_display(tool, args, screen).await
}

// Shared executor; the local fixture supplies an already-running disposable X11
// display instead of invoking the VM-only ensure-screen lifecycle helper.
async fn execute_display(tool: &str, args: &Value, screen: i64) -> Result<Value> {
    let display = format!(":{screen}");
    let browser = if screen == 1 {
        "/home/bot/.local/share/kindred/browser".to_string()
    } else {
        format!("/home/bot/.local/share/kindred/browser-{screen}")
    };
    #[cfg(test)]
    let browser = std::env::var("KINDRED_TEST_CUA_PROFILE").unwrap_or(browser);
    let session = args["_page_session"].as_str().unwrap_or("guest-local");
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
            // Explicit managed per-display profile, guest-loopback endpoint only.
            // Existing browsers retain their flags and silently remain screenshot-only.
            cmd.args(["--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0"]);
            page_bridge(&display,&browser,json!({"op":"invalidate","session":session})).await;
            cmd.env("DISPLAY", &display).args([
                "--no-first-run",
                &format!("--user-data-dir={browser}"),
                url.as_str(),
            ]);
            launch_browser(cmd).await?;
            page_bridge(&display,&browser,json!({"op":"attach","session":session})).await;
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
        "computer_screenshot" => observed_screen(&display,&browser,session).await,
        "computer_click" | "computer_type" | "computer_key" | "computer_scroll" => {
            if let Some(target) = args.get("target") {
                ensure!(target.is_string(), "Page target must be a string; omit it to use native input");
            }
            if let Some(target) = args["target"].as_str() {
                ensure!(matches!(tool,"computer_click" | "computer_type"), "Page targets support click or type");
                ensure!(tool != "computer_click" || args["button"].as_i64().unwrap_or(1) == 1, "Page targets support primary clicks; use image coordinates for another button");
                let op = if tool == "computer_click" { "click" } else { "type" };
                let mut result = page_bridge(&display,&browser,json!({"op":op,"session":session,"target":target,"text":args["text"]})).await;
                if result["unavailable"] == true {
                    return Ok(json!({"failed":true,"action_applied":false,"text":"The current page item is unavailable. Take a fresh screenshot and use its image coordinates."}));
                }
                if result["action_applied"] == true {
                    match observed_screen(&display,&browser,session).await {
                        Ok(observation) => { let text = result["text"].as_str().unwrap_or("").to_string();
                            result["text"] = json!(format!("{text}\n{}",observation["text"].as_str().unwrap_or("")));
                            for key in ["image","width","height","page_observation"] { if let Some(value)=observation.get(key) { result[key]=value.clone(); } }
                            result["observation_after_action"] = json!(true);
                        }
                        Err(_) => { result["observation_error"] = json!(true); result["text"] = json!("Input delivered, but the resulting page could not be captured. Do not repeat the input. Take a fresh screenshot to verify it."); }
                    }
                }
                return Ok(result);
            }
            page_bridge(&display,&browser,json!({"op":"invalidate","session":session})).await;
            let mut cmd = Command::new("xdotool");
            cmd.env("DISPLAY", &display);
            match tool {
                "computer_click" => {
                    let (width, height) = display_geometry(&display).await?;
                    let x = number(args, "x", width - 1)?;
                    let y = number(args, "y", height - 1)?;
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
            // Input may have been partly applied even when the process or its
            // receipt fails. Never turn that uncertainty into an ordinary retry.
            let output = match crate::vm::capture_output(cmd, None, 30, 4096).await {
                Ok(output) => output,
                Err(error) => return Ok(json!({"text":format!("Computer input outcome is uncertain: {error}. Do not repeat it. Inspect the current page and reconcile the effect first."),"failed":true,"uncertain_effect":true,"timed_out":error.to_string().contains("timed out")})),
            };
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            if !output.status.success() || diagnostic.contains("No such key name") {
                return Ok(json!({"text":format!("Computer input did not complete reliably: {diagnostic}. It may have partly applied; do not repeat it without verifying the page."),"failed":true,"uncertain_effect":true}));
            }
            if args["observe"] == true {
                return Ok(match observed_screen(&display,&browser,session).await {
                    Ok(mut observation) => {
                        observation["action_applied"] = json!(true);
                        observation["observation_after_action"] = json!(true);
                        observation["text"] = json!(format!("Input applied. {} Verify the intended target and result before continuing; do not repeat a submission to get a clearer receipt.", observation["text"].as_str().unwrap()));
                        observation
                    }
                    Err(error) => json!({"text":format!("Input applied, but its post-action screenshot is unavailable: {error}. Do not repeat the input. Take a fresh computer_screenshot to verify the result."),"action_applied":true,"observation_error":true}),
                });
            }
            Ok(json!({"text":"Input applied. Inspect a fresh screenshot to verify the target and result.","action_applied":true}))
        }
        _ => bail!("unknown guest tool"),
    }
}

#[cfg(all(test, unix))]
mod browser_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "explicit disposable X11 browser fixture only"]
    async fn local_executor_fixture() {
        use tokio::net::TcpListener;
        use tokio::io::{AsyncBufReadExt, BufReader};
        let address = std::env::var("KINDRED_EXECUTOR_FIXTURE_ADDRESS").unwrap();
        let screen: i64 = std::env::var("KINDRED_EXECUTOR_FIXTURE_SCREEN").unwrap().parse().unwrap();
        assert!((1..=32).contains(&screen));
        let listener = TcpListener::bind(&address).await.unwrap();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            if request["tool"] == "fixture_stop" { break; }
            let result = match execute_display(request["tool"].as_str().unwrap(), &request["args"], screen).await {
                Ok(value) => value,
                Err(error) => json!({"failed":true,"text":error.to_string()}),
            };
            let output = format!("{}\n", serde_json::to_string(&result).unwrap());
            reader.get_mut().write_all(output.as_bytes()).await.unwrap();
        }
    }
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
