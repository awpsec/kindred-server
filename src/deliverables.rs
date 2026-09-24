//! Immutable, authenticated downloads of files a bot explicitly shares in chat.
use crate::{
    db::{self, Db, Run},
    runtime::App,
    vm,
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const LIMIT: usize = 8 * 1024 * 1024;
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS deliverables(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),name TEXT NOT NULL,path TEXT NOT NULL,sha256 TEXT NOT NULL,bytes BLOB NOT NULL,created INTEGER NOT NULL,UNIQUE(run_id,path,sha256)); CREATE INDEX IF NOT EXISTS deliverables_run ON deliverables(run_id);")?;
    let has_source: bool = c
        .prepare("PRAGMA table_info(deliverables)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|n| n == "source_url");
    if !has_source {
        c.execute_batch(
            "ALTER TABLE deliverables ADD COLUMN source_url TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}
fn valid_path(path: &str) -> bool {
    path.starts_with("/workspace/")
        && path.len() <= 2000
        && !path.chars().any(char::is_control)
        && path[11..]
            .split('/')
            .all(|p| !p.is_empty() && !matches!(p, "." | ".."))
}
impl Db {
    pub fn attach_file(&self, run: &Run, path: &str, bytes: &[u8]) -> Result<Value> {
        self.attach_file_with_source(run, path, bytes, "")
    }
    pub fn attach_file_with_source(
        &self,
        run: &Run,
        path: &str,
        bytes: &[u8],
        source: &str,
    ) -> Result<Value> {
        validate_source(source)?;
        ensure!(
            valid_path(path),
            "Choose a regular file under /workspace, without traversal or symlinks"
        );
        ensure!(
            bytes.len() <= LIMIT,
            "File exceeds 8 MB; split or compress it first"
        );
        let name = path.rsplit('/').next().unwrap();
        ensure!(name.len() <= 240, "File name is too long");
        let hash: String = ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(
            !crate::workspace_transfer::frozen(&tx)?,
            "This workspace is paused"
        );
        let actual: (String, String) = tx.query_row(
            "SELECT bot_id,chat_id FROM runs WHERE id=?",
            [&run.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            actual == (run.bot_id.clone(), run.chat_id.clone()),
            "Task ownership changed"
        );
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM deliverables WHERE run_id=? AND path=? AND sha256=?",
                params![run.id, path, hash],
                |r| r.get(0),
            )
            .optional()?;
        let id = if let Some(id) = existing {
            if !source.is_empty() {
                let prior: String = tx.query_row(
                    "SELECT source_url FROM deliverables WHERE id=?",
                    [&id],
                    |r| r.get(0),
                )?;
                ensure!(
                    prior.is_empty() || prior == source,
                    "This snapshot already links to a different Drive file; share a separately named snapshot for the other file"
                );
                tx.execute(
                    "UPDATE deliverables SET source_url=? WHERE id=? AND source_url=''",
                    params![source, id],
                )?;
            }
            id
        } else {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM deliverables WHERE run_id=?",
                [&run.id],
                |r| r.get(0),
            )?;
            ensure!(count < 20, "This task already has 20 shared files");
            let id = db::id();
            tx.execute("INSERT INTO deliverables(id,run_id,name,path,sha256,bytes,created,source_url) VALUES(?,?,?,?,?,?,?,?)",params![id,run.id,name,path,hash,bytes,db::now(),source])?;
            tx.execute(
                "INSERT INTO events(run_id,kind,body,created) VALUES(?,'attachment',?,?)",
                params![
                    run.id,
                    json!({"id":id,"kind":"file","name":name,"size":bytes.len(),"sha256":hash})
                        .to_string(),
                    db::now()
                ],
            )?;
            id
        };
        tx.commit()?;
        Ok(
            json!({"id":id,"name":name,"size":bytes.len(),"sha256":hash,"shared_in_chat":true,"message":"File saved and attached to this conversation as a downloadable snapshot. No Markdown path link is needed."}),
        )
    }
    pub fn file_attachments(&self, run: &str) -> Result<Vec<Value>> {
        Ok(self.0.lock().unwrap().prepare("SELECT id,name,created,length(bytes),sha256,source_url FROM deliverables WHERE run_id=? ORDER BY created,rowid")?.query_map([run],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"name":r.get::<_,String>(1)?,"created":r.get::<_,i64>(2)?,"size":r.get::<_,i64>(3)?,"sha256":r.get::<_,String>(4)?,"source_url":r.get::<_,String>(5)?,"kind":"file"})))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn file_download(&self, id: &str) -> Result<(String, Vec<u8>)> {
        Ok(self.0.lock().unwrap().query_row(
            "SELECT name,bytes FROM deliverables WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
}

// Traverse with directory handles, never following a symlink, including the root.
// A nonblocking regular-file check prevents FIFO/device reads from hanging a task.
const EXPORT: &str = r#"import os,sys,json,stat,base64
v=json.load(sys.stdin)
path=v['path']
assert path.startswith('/workspace/') and len(path)<=2000
parts=path[len('/workspace/'):].split('/')
assert all(p and p not in ('.','..') and not any(ord(c)<32 for c in p) for p in parts)
fd=os.open('/workspace',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
try:
 for part in parts[:-1]:
  child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd)
  os.close(fd);fd=child
 file=os.open(parts[-1],os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=fd)
 with os.fdopen(file,'rb') as stream:
  before=os.fstat(stream.fileno())
  assert stat.S_ISREG(before.st_mode) and before.st_size<=8*1024*1024
  data=stream.read(8*1024*1024+1)
  after=os.fstat(stream.fileno())
  assert len(data)<=8*1024*1024 and (before.st_size,before.st_mtime_ns,before.st_ctime_ns)==(after.st_size,after.st_mtime_ns,after.st_ctime_ns), 'File changed while reading; finish writing it first'
 print(json.dumps({'data':base64.b64encode(data).decode()}))
finally: os.close(fd)
"#;
fn validate_source(source: &str) -> Result<()> {
    if source.is_empty() {
        return Ok(());
    }
    let url = reqwest::Url::parse(source)?;
    ensure!(
        source.len() <= 2048
            && url.scheme() == "https"
            && matches!(url.host_str(), Some("drive.google.com" | "docs.google.com"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none(),
        "Use the exact HTTPS Google Drive or Google Docs link returned for this file"
    );
    Ok(())
}
pub async fn share(app: &App, run: &Run, path: &str, source: &str) -> Result<Value> {
    validate_source(source)?;
    ensure!(!app.account_disabled(), "This workspace is paused");
    ensure!(
        valid_path(path),
        "Choose a regular file under /workspace, without traversal or symlinks"
    );
    let mut cmd = vm::ssh(&app.config.vm);
    cmd.arg(format!("python3 -c '{}'", EXPORT.replace('\'', "'\\''")));
    let bytes = vm::capture(
        cmd,
        Some(serde_json::to_vec(&json!({"path":path}))?),
        30,
        12 * 1024 * 1024,
    )
    .await?;
    let data: Value = serde_json::from_slice(&bytes)?;
    let bytes = STANDARD.decode(
        data["data"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("File export returned no data"))?,
    )?;
    ensure!(!app.db.cancelled(&run.id), "Task stopped before sharing");
    Ok(
        json!({"text":serde_json::to_string(&app.db.attach_file_with_source(run,path,&bytes,source)?)?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{app, bot};
    #[test]
    fn drive_links_are_validated_and_can_be_added_once_without_duplicating_bytes() {
        let app = app();
        let b = bot(&app.db, "codex");
        let id = app.db.queue(&b.id, "Report", 0).unwrap();
        let run = app.db.run(&id).unwrap();
        let file = app
            .db
            .attach_file(&run, "/workspace/report.docx", b"fixture")
            .unwrap();
        let url = "https://docs.google.com/document/d/fixture/edit";
        let linked = app
            .db
            .attach_file_with_source(&run, "/workspace/report.docx", b"fixture", url)
            .unwrap();
        assert_eq!(file["id"], linked["id"]);
        assert_eq!(app.db.file_attachments(&id).unwrap()[0]["source_url"], url);
        for url in [
            "javascript:alert(1)",
            "https://docs.google.com.evil.test/file",
            "https://user:pass@drive.google.com/file",
            "https://drive.google.com:8443/file",
            "http://docs.google.com/file",
        ] {
            assert!(
                app.db
                    .attach_file_with_source(&run, "/workspace/other.docx", b"fixture", url)
                    .is_err()
            );
        }
        assert!(
            app.db
                .attach_file_with_source(
                    &run,
                    "/workspace/report.docx",
                    b"fixture",
                    "https://drive.google.com/file/d/other/view"
                )
                .is_err()
        );
        assert_eq!(app.db.file_attachments(&id).unwrap().len(), 1);
        assert_eq!(
            app.db
                .file_download(file["id"].as_str().unwrap())
                .unwrap()
                .1,
            b"fixture"
        );
    }
    #[tokio::test]
    async fn files_are_immutable_deduplicated_persistent_and_authenticated() {
        let root = std::env::temp_dir().join(format!("kindred-files-{}", db::id()));
        let path = root.join("kindred.db");
        let mut app = app();
        std::sync::Arc::get_mut(&mut app).unwrap().db = Db::open(path.to_str().unwrap()).unwrap();
        let b = bot(&app.db, "codex");
        let id = app.db.queue(&b.id, "Create report", 0).unwrap();
        let run = app.db.run(&id).unwrap();
        let file = app
            .db
            .attach_file(&run, "/workspace/review/report.md", b"# Verified report\n")
            .unwrap();
        assert_eq!(
            file,
            app.db
                .attach_file(&run, "/workspace/review/report.md", b"# Verified report\n")
                .unwrap()
        );
        let revision = app
            .db
            .attach_file(&run, "/workspace/review/report.md", b"Revised bytes")
            .unwrap();
        assert_ne!(file["id"], revision["id"]);
        for bad in [
            "/etc/passwd",
            "/workspace/../etc/passwd",
            "/workspace/a/../../x",
            "/workspace/",
            "/workspace/a//b",
        ] {
            assert!(app.db.attach_file(&run, bad, b"bad").is_err());
        }
        assert!(
            app.db
                .attach_file(&run, "/workspace/large", &vec![0; LIMIT + 1])
                .is_err()
        );
        app.db.finish(&id, "completed", "", "").unwrap();
        app.db.chat_complete(&app.db.run(&id).unwrap()).unwrap();
        assert_eq!(
            app.db.chat_messages(&run.chat_id).unwrap().last().unwrap()["attachments"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        drop(app);
        let mut app = crate::tests::app();
        std::sync::Arc::get_mut(&mut app).unwrap().db = Db::open(path.to_str().unwrap()).unwrap();
        let file_id = file["id"].as_str().unwrap();
        assert_eq!(
            app.db.file_download(file_id).unwrap().1,
            b"# Verified report\n"
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/deliverables/{file_id}",
            listener.local_addr().unwrap()
        );
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        let response = client
            .get(&url)
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(
            response.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            b"# Verified report\n"
        );
        server.abort();
        let _ = server.await;
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn export_rejects_symlinks_devices_traversal_and_directories() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("kindred-export-{}", db::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/report.txt"), b"Fixture").unwrap();
        std::os::unix::fs::symlink("nested/report.txt", root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("nested", root.join("linkdir")).unwrap();
        let script = EXPORT.replace(
            "os.open('/workspace',",
            &format!("os.open('{}',", root.display()),
        );
        for (path, success) in [
            ("/workspace/nested/report.txt", true),
            ("/workspace/link.txt", false),
            ("/workspace/linkdir/report.txt", false),
            ("/workspace/nested", false),
            ("/workspace/../etc/passwd", false),
            ("/etc/passwd", false),
        ] {
            let mut child = std::process::Command::new("python3")
                .args(["-c", &script])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(json!({"path":path}).to_string().as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(output.status.success(), success, "{path}");
            if success {
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(
                    STANDARD.decode(value["data"].as_str().unwrap()).unwrap(),
                    b"Fixture"
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
