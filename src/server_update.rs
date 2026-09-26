//! Narrow, administrator-only bridge to the host supervisor. No client supplied
//! paths, URLs or commands cross this boundary; the supervisor survives restart.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub async fn request(action: &str, version: Option<&str>) -> Result<Value> {
    ensure!(
        matches!(action, "status" | "start"),
        "Unknown update action"
    );
    #[cfg(unix)]
    {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let path = std::env::var("KINDRED_UPDATE_SOCKET")
            .unwrap_or_else(|_| "/run/kindred-updater/control.sock".into());
        if !std::path::Path::new(&path).exists() {
            return Ok(
                json!({"supported":false,"message":"Browser updates are not enabled on this host. Install the Kindred host updater, or update using your deployment manager."}),
            );
        }
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut stream = tokio::net::UnixStream::connect(path).await?;
            let mut body = serde_json::to_vec(&json!({"action":action,"version":version}))?;
            body.push(b'\n');
            stream.write_all(&body).await?;
            let mut response = String::new();
            BufReader::new(stream)
                .take(65537)
                .read_line(&mut response)
                .await?;
            ensure!(response.len() <= 65536, "Invalid updater response");
            Ok(serde_json::from_str(&response)?)
        })
        .await?
    }
    #[cfg(not(unix))]
    Ok(json!({"supported":false,"message":"Use the desktop app to update this standalone server."}))
}

#[cfg(test)]
mod tests {
    #[test]
    fn browser_version_is_stamped_from_the_build() {
        let source = include_str!("../ui/app.js");
        assert_eq!(source.matches("const UI_VERSION = \"0.63.0\";").count(), 1);
        let stamped = source.replace(
            "const UI_VERSION = \"0.63.0\";",
            &format!("const UI_VERSION = {:?};", env!("CARGO_PKG_VERSION")),
        );
        assert!(stamped.contains(&format!(
            "const UI_VERSION = {:?};",
            env!("CARGO_PKG_VERSION")
        )));
    }
}
