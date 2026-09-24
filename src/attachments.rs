use crate::db::{self, Db};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

impl Db {
    pub fn attach_screenshot(&self, run_id: &str, image: &str, title: &str) -> Result<String> {
        ensure!(
            image.len() <= 7 * 1024 * 1024,
            "Screenshot exceeds the size limit"
        );
        let encoded = image
            .strip_prefix("data:image/png;base64,")
            .ok_or_else(|| anyhow::anyhow!("Expected a PNG screenshot"))?;
        let png = STANDARD.decode(encoded)?;
        ensure!(
            png.starts_with(b"\x89PNG\r\n\x1a\n") && png.len() <= 5 * 1024 * 1024,
            "Invalid or oversized screenshot"
        );
        let title = title.trim();
        ensure!(
            !title.is_empty() && title.len() <= 200 && !title.chars().any(char::is_control),
            "Invalid screenshot title"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM attachments WHERE run_id=?",
            [run_id],
            |r| r.get(0),
        )?;
        ensure!(
            count < 20,
            "This task already has 20 screenshot attachments"
        );
        let id = db::id();
        tx.execute(
            "INSERT INTO attachments(id,run_id,title,png,created) VALUES(?,?,?,?,?)",
            params![id, run_id, title, png, db::now()],
        )?;
        tx.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'attachment',?,?)",
            params![
                run_id,
                json!({"id":id,"title":title}).to_string(),
                db::now()
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }
    pub fn attachments(&self, run_id: &str) -> Result<Vec<Value>> {
        let mut rows=self.0.lock().unwrap().prepare("SELECT id,title,created,length(png) FROM attachments WHERE run_id=? ORDER BY created,rowid")?
            .query_map([run_id], |r| Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"created":r.get::<_,i64>(2)?,"size":r.get::<_,i64>(3)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.extend(self.file_attachments(run_id)?);
        Ok(rows)
    }

    pub fn attachment_png(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT png FROM attachments WHERE id=?", [id], |r| r.get(0))
            .optional()?)
    }
}
