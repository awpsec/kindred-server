//! Public release transport. Never forwards workspace credentials or arbitrary URLs.
use axum::{body::Body, http::header, response::Response};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
const REPO: &str = "https://github.com/awpsec/kindred/releases";

pub fn asset_url(file: &str) -> Option<String> {
    if matches!(
        file,
        "stable.json" | "client-stable.json" | "client-linux.json"
    ) {
        // Full releases share one manifest across Mac and Linux; partial releases
        // must never become GitHub's latest stable release.
        let name = if file == "client-linux.json" {
            "client-stable.json"
        } else {
            file
        };
        return Some(format!("{REPO}/latest/download/{name}"));
    }
    for (prefix, suffix, label) in [
        ("kindred-windows-", ".zip", "Windows-Update.zip"),
        ("kindred-linux-x86_64-", ".AppImage", "Linux-x64.AppImage"),
        ("kindred-macos-aarch64-", ".dmg", "macOS-Apple-Silicon.dmg"),
    ] {
        if let Some(version) = file
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(suffix))
        {
            let parts: Vec<_> = version.split('.').collect();
            if parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|c| c.is_ascii_digit()))
            {
                return Some(format!(
                    "{REPO}/download/v{version}/Kindred-{version}-{label}"
                ));
            }
        }
    }
    None
}
fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

pub async fn fetch(file: &str) -> Option<Response> {
    // Unit fixtures must not depend on the public latest release or make live
    // downloads. Transport URLs/policy are checked separately below.
    if cfg!(test) {
        return None;
    }
    let url = asset_url(file)?;
    let manifest = file.ends_with(".json");
    let maximum = if manifest {
        65536
    } else if file.starts_with("kindred-windows-") {
        67108864
    } else {
        536870912
    };
    let client = reqwest::Client::builder()
        .user_agent("Kindred-Updater")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(if manifest { 5 } else { 600 }))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .ok()?;
    let mut upstream = client.get(url).send().await.ok()?;
    if !upstream.status().is_success() || upstream.content_length().is_some_and(|n| n > maximum) {
        return None;
    }
    let mime = if manifest {
        "application/json"
    } else {
        "application/octet-stream"
    };
    if manifest {
        let mut data = Vec::new();
        while let Some(chunk) = upstream.chunk().await.ok()? {
            if data.len() + chunk.len() > maximum as usize {
                return None;
            }
            data.extend_from_slice(&chunk);
        }
        // Clients verify the pinned RSA signature before trusting this envelope.
        return Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(data))
            .ok();
    }
    let mut builder = Response::builder().header(header::CONTENT_TYPE, mime);
    if let Some(size) = upstream.content_length() {
        builder = builder.header(header::CONTENT_LENGTH, size);
    }
    let (mut writer, reader) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let mut total = 0u64;
        while let Ok(Some(chunk)) = upstream.chunk().await {
            total += chunk.len() as u64;
            if total > maximum || writer.write_all(&chunk).await.is_err() {
                break;
            }
        }
        // Truncation is rejected by the installer's signed size/hash checks.
    });
    builder
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(reader)))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_release_assets_and_https_github_redirects() {
        assert!(
            asset_url("stable.json")
                .unwrap()
                .ends_with("/latest/download/stable.json")
        );
        for (file, name) in [
            ("kindred-windows-1.2.3.zip", "Windows-Update.zip"),
            ("kindred-linux-x86_64-1.2.3.AppImage", "Linux-x64.AppImage"),
            ("kindred-macos-aarch64-1.2.3.dmg", "macOS-Apple-Silicon.dmg"),
        ] {
            assert!(
                asset_url(file)
                    .unwrap()
                    .ends_with(&format!("/v1.2.3/Kindred-1.2.3-{name}"))
            );
        }
        for file in [
            "../stable.json",
            "https://evil.test/x",
            "kindred-windows-1.2.3/other.zip",
            "kindred-windows-999999.2.3.zip",
        ] {
            assert!(asset_url(file).is_none());
        }
        for url in [
            "http://github.com/x",
            "https://evil.test/x",
            "https://github.com.evil.test/x",
            "https://user@github.com/x",
        ] {
            assert!(!allowed(&url.parse().unwrap()));
        }
        assert!(allowed(
            &"https://release-assets.githubusercontent.com/x"
                .parse()
                .unwrap()
        ));
    }
}
