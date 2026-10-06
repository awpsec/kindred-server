use super::{ActionReceipt, Browser, Candidate, Observation, Task, Work};
use crate::{config::Vm, vm};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

pub struct GuestBrowser {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl GuestBrowser {
    pub async fn connect(config: &Vm, screen: i64) -> Result<Self> {
        ensure!((1..=32).contains(&screen), "Invalid browser screen");
        let source = include_str!("../../deploy/browser-driver.py");
        let observer = include_str!("../../deploy/browser-observer.js");
        let profile = if screen == 1 {
            "/home/bot/.local/share/kindred/browser".into()
        } else {
            format!("/home/bot/.local/share/kindred/browser-{screen}")
        };
        let mut command = vm::ssh(config);
        command.arg(format!(
            "python3 -u -c '{}' '{}' ':{}'",
            source.replace('\'', "'\"'\"'"),
            profile,
            screen
        ));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("Start browser observer in the bot VM")?;
        let mut driver = Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            _child: child,
        };
        let hello = driver
            .request(json!({"op":"hello","protocol":1,"observer":observer}))
            .await?;
        ensure!(
            hello["protocol"] == 1,
            "Browser observer protocol unavailable"
        );
        Ok(driver)
    }
    #[cfg(test)]
    pub async fn local_fixture(profile: &str, display: Option<&str>) -> Result<Self> {
        let mut command = tokio::process::Command::new("python3");
        command
            .arg("-u")
            .arg("-c")
            .arg(include_str!("../../deploy/browser-driver.py"))
            .arg(profile);
        if let Some(display) = display {
            command.arg(display);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let mut driver = Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            _child: child,
        };
        ensure!(driver.request(json!({"op":"hello","protocol":1,"observer":include_str!("../../deploy/browser-observer.js")})).await?["protocol"]==1,"Fixture browser handshake failed");
        Ok(driver)
    }
    async fn request(&mut self, request: Value) -> Result<Value> {
        let mut bytes = serde_json::to_vec(&request)?;
        ensure!(bytes.len() < 65536, "Browser request too large");
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(30), async {
            self.input.write_all(&bytes).await?;
            self.input.flush().await?;
            let mut line = Vec::new();
            loop {
                let buffer = self.output.fill_buf().await?;
                ensure!(!buffer.is_empty(), "Browser observer disconnected");
                let count = buffer
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(buffer.len(), |i| i + 1);
                ensure!(
                    line.len() + count <= 8 * 1024 * 1024,
                    "Browser observation too large"
                );
                let finished = buffer[count - 1] == b'\n';
                line.extend_from_slice(&buffer[..count]);
                self.output.consume(count);
                if finished {
                    break;
                }
            }
            let response: Value = serde_json::from_slice(&line)?;
            ensure!(
                response.get("error").is_none(),
                "Browser observer unavailable: {}",
                response["error"].as_str().unwrap_or("invalid response")
            );
            Ok(response)
        })
        .await
        .context("Browser observer timed out")?
    }
}
impl Browser for GuestBrowser {
    fn observe<'a>(&'a mut self, task: &'a Task) -> Work<'a, Observation> {
        Box::pin(async move {
            Ok(serde_json::from_value(
                self.request(json!({"op":"observe","origin":task.origin,"values":task.values}))
                    .await?,
            )?)
        })
    }
    fn act<'a>(
        &'a mut self,
        observation: &'a Observation,
        candidate: &'a Candidate,
    ) -> Work<'a, ActionReceipt> {
        Box::pin(async move {
            Ok(serde_json::from_value(self.request(json!({"op":"act","snapshot_id":observation.snapshot_id,"choice_id":candidate.id})).await?)?)
        })
    }
}
