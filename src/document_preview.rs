//! Paginated DOCX previews. The original attachment is never modified.
use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{Arc, OnceLock},
};
use tokio::sync::Semaphore;

pub async fn render(name: String, bytes: Vec<u8>) -> Result<Vec<u8>> {
    ensure!(
        name.rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("docx")),
        "Page preview supports DOCX documents"
    );
    ensure!(bytes.len() <= 8 * 1024 * 1024, "Document exceeds 8 MB");
    // Across all accounts, at most one office engine runs at a time. Do not
    // accumulate a queue of documents and memory while bots are working.
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let slot = SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
        .context("Another document is being prepared. Try again shortly.")?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        let mut child = Command::new("python3")
            .args(["-c", include_str!("../deploy/render-document.py")])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().context("Page preview is unavailable on this server")?;
        if let Err(error) = child.stdin.take().unwrap().write_all(&bytes) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Could not prepare document preview");
        }
        let output = child.wait_with_output()?;
        ensure!(output.status.success() && output.stdout.starts_with(b"%PDF-") && output.stdout.len() <= 32 * 1024 * 1024,
            "Page preview is unavailable on this server. Use quick preview or download the original.");
        Ok(output.stdout)
    }).await?
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn rejects_non_word_files_before_starting_converter() {
        assert!(
            super::render("report.pdf".into(), b"%PDF-".to_vec())
                .await
                .unwrap_err()
                .to_string()
                .contains("DOCX")
        );
    }
    #[tokio::test]
    async fn rejects_oversized_files_before_starting_converter() {
        assert!(
            super::render("report.docx".into(), vec![0; 8 * 1024 * 1024 + 1])
                .await
                .unwrap_err()
                .to_string()
                .contains("8 MB")
        );
    }
}
