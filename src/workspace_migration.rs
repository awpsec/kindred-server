//! Full profile migration worker boundary. Registry identities and sessions stay
//! on their respective hosts; only an authenticated profile owner can drive it.
use crate::runtime::Shared;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{path::{Path, PathBuf}, time::Duration};

async fn worker(app: &Shared, account_path: &Path, request: Value) -> Result<Value> {
    ensure!(uuid::Uuid::parse_str(&app.config.vm.managed_id).is_ok(),
        "This workspace needs a managed profile computer before it can be migrated");
    let helper = Path::new(&app.config.vm.manager).with_file_name("migration-worker.py");
    ensure!(helper.is_file(), "Update this server to a version that supports complete workspace migration");
    let action = request["action"].as_str().unwrap_or("");
    let seconds = match action { "prepare" => 2400, "verify" => 900, "abort" | "cancel" => 180, _ => 120 };
    let mut command = tokio::process::Command::new("python3");
    command.arg(helper).arg(&app.config.vm.managed_id).arg(env!("CARGO_PKG_VERSION")).arg(account_path)
        .env("KINDRED_PROFILES_DIR", &app.config.profiles.directory);
    let output = crate::vm::capture_output(command, Some(serde_json::to_vec(&request)?), seconds, 8 * 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow::anyhow!("Migration worker did not return a valid receipt; the original workspace is retained"))?;
    ensure!(output.status.success(), "{}", value["error"].as_str().unwrap_or("Migration stopped; the original workspace is retained"));
    Ok(value)
}

pub async fn request(app: &Shared, account_path: PathBuf, mut request: Value) -> Result<Value> {
    let action = request["action"].as_str().unwrap_or("").to_owned();
    ensure!(["preflight", "status", "prepare", "reserve", "read", "write", "bind", "verify", "commit", "activate", "abort", "cancel", "cancel_signal"].contains(&action.as_str()),
        "Unknown workspace migration action");
    if action != "preflight" {
        ensure!(request["id"].as_str().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()), "Invalid migration ID");
    }
    if ["prepare", "reserve", "bind"].contains(&action.as_str()) {
        let peer = &request["peer"];
        ensure!(peer["profile"].as_str().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()), "Invalid destination profile");
        let url = reqwest::Url::parse(peer["server"].as_str().unwrap_or(""))?;
        ensure!((url.scheme() == "https" || (url.scheme() == "http" && matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]"))))
            && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && url.path() == "/",
            "Use a secure server origin for migration");
        ensure!(peer.as_object().is_some_and(|p| p.len() == 2), "Unexpected migration identity fields");
    }
    let _barrier = if matches!(action.as_str(), "prepare" | "activate") { Some(app.migration_barrier.write().await) } else { None };
    // Quiesce in-flight monitor refreshes before taking the snapshot or
    // replacing the database. New work is fenced by the worker transaction.
    let _mail = if matches!(action.as_str(), "prepare" | "activate") { Some(app.mail_lock.lock().await) } else { None };
    let _slack = if matches!(action.as_str(), "prepare" | "activate") { Some(app.slack_lock.lock().await) } else { None };
    if action != "activate" {
        return worker(app, &account_path, request).await;
    }
    request["action"] = json!("activate_prepare");
    let prepared = worker(app, &account_path, request.clone()).await?;
    if prepared["already_restored"] != true {
        let path = PathBuf::from(prepared["database"].as_str().ok_or_else(|| anyhow::anyhow!("Missing verified workspace snapshot"))?);
        let root = PathBuf::from(&app.config.profiles.directory).join(&app.config.vm.managed_id)
            .join(".migrations").join(request["id"].as_str().unwrap()).join("files");
        ensure!(path.canonicalize()?.starts_with(root.canonicalize()?) && !path.symlink_metadata()?.file_type().is_symlink(),
            "Workspace snapshot is outside its staging area");
        // Recheck and merge under the same lock as normal connection settings.
        if let Some(keys) = prepared["account_credentials"].as_object() {
            crate::connections::merge_migration_credentials(&account_path, keys)?;
        }
        let app = app.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let source = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut destination = app.db.0.lock().map_err(|_| anyhow::anyhow!("Destination database is busy"))?;
            use rusqlite::OptionalExtension;
            let imported: Option<String> = destination.query_row("SELECT value FROM settings WHERE key='_migration_import'", [], |r| r.get(0)).optional()?;
            let incoming: String = source.query_row("SELECT value FROM settings WHERE key='_migration_import'", [], |r| r.get(0))?;
            if imported.as_ref() == Some(&incoming) { return Ok(()); }
            let disabled: bool = destination.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key='_account_disabled' AND value='true')",[],|r|r.get(0))?;
            ensure!(!disabled,"The destination account was disabled during migration. Ask its administrator to restore access before continuing.");
            // Restore through the existing connection rather than replacing an
            // open SQLite file underneath the running application's WAL.
            {
                let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
                backup.run_to_completion(128, Duration::from_millis(1), None)?;
            }
            destination.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            Ok(())
        }).await??;
    }
    request["action"] = json!("activated");
    worker(app, &account_path, request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture { root: PathBuf, app: Shared, keys: PathBuf }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("kindred-migration-{}", uuid::Uuid::new_v4()));
            let id = uuid::Uuid::new_v4().to_string();
            let folder = root.join(&id);
            std::fs::create_dir_all(&folder).unwrap();
            let mut config = crate::config::Config::default();
            config.database = folder.join("kindred.db").to_string_lossy().into();
            config.profiles.directory = root.to_string_lossy().into();
            config.vm.managed_id = id;
            config.vm.manager = Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/vm-manager.py").to_string_lossy().into();
            let app = crate::runtime::App::open(config, "synthetic-migration-session".into()).unwrap();
            Self { keys: root.join(".account-fixture.credentials"), root, app }
        }
        async fn call(&self, id: &str, action: &str, mut body: Value) -> Result<Value> {
            body["id"] = json!(id); body["action"] = json!(action);
            request(&self.app, self.keys.clone(), body).await
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

    #[tokio::test]
    async fn full_schema_restore_through_live_connection_and_repeated_activation_keep_new_work() {
        let source = Fixture::new(); let target = Fixture::new(); let id = uuid::Uuid::new_v4().to_string();
        source.app.db.save_setting("migration_test_fact", &json!({"completion":"yesterday","memory":"preserve"})).unwrap();
        let peer = json!({"server":"https://destination.invalid","profile":target.app.config.vm.managed_id});
        let prepared = source.call(&id,"prepare",json!({"peer":peer})).await.unwrap();
        assert!(crate::workspace_transfer::frozen(&source.app.db.0.lock().unwrap()).unwrap());
        let manifest = &prepared["manifest"];
        let reservation = target.call(&id,"reserve",json!({"manifest":manifest,"peer":{"server":"https://source.invalid","profile":source.app.config.vm.managed_id}})).await.unwrap();
        source.call(&id,"bind",json!({"peer":peer,"receipt":reservation["receipt"]})).await.unwrap();
        for (index, file) in manifest["files"].as_array().unwrap().iter().enumerate() {
            let mut offset = 0;
            while offset < file["size"].as_u64().unwrap() {
                let chunk = source.call(&id,"read",json!({"index":index,"offset":offset})).await.unwrap();
                let copied = target.call(&id,"write",json!({"index":index,"offset":offset,"data":chunk["data"],"sha256":chunk["sha256"]})).await.unwrap();
                offset = copied["offset"].as_u64().unwrap();
            }
        }
        let checked = target.call(&id,"verify",json!({})).await.unwrap();
        let proof = source.call(&id,"commit",json!({"receipt":checked["receipt"]})).await.unwrap();
        let activated = target.call(&id,"activate",json!({"proof":proof})).await.unwrap();
        assert_eq!(activated["state"], "active");
        assert_eq!(target.app.db.setting("migration_test_fact").unwrap(),source.app.db.setting("migration_test_fact").unwrap());
        target.app.db.save_setting("work_after_migration", &json!("keep this newer work")).unwrap();
        target.call(&id,"activate",json!({"proof":proof})).await.unwrap();
        assert_eq!(target.app.db.setting("work_after_migration").unwrap(),Some(json!("keep this newer work")));
        assert!(!crate::workspace_transfer::frozen(&target.app.db.0.lock().unwrap()).unwrap());
        assert!(source.call(&id,"cancel",json!({})).await.is_err());
        assert!(target.call(&id,"abort",json!({})).await.is_err());
    }

    #[test]
    fn account_key_merge_is_atomic_and_preserves_concurrent_settings() {
        let f = Fixture::new();
        crate::connections::save_credential_at(f.keys.clone(), "existing", "synthetic-original").unwrap();
        let before = std::fs::read(&f.keys).unwrap();
        let incoming = json!({"added":"synthetic-addition","existing":"synthetic-conflict"});
        assert!(crate::connections::merge_migration_credentials(&f.keys,incoming.as_object().unwrap()).is_err());
        assert_eq!(std::fs::read(&f.keys).unwrap(), before);
        let incoming = json!({"added":"synthetic-addition","existing":"synthetic-original"});
        crate::connections::merge_migration_credentials(&f.keys,incoming.as_object().unwrap()).unwrap();
        let keys:Value=serde_json::from_slice(&std::fs::read(&f.keys).unwrap()).unwrap();
        assert_eq!(keys["added"],"synthetic-addition"); assert_eq!(keys["existing"],"synthetic-original");
    }

    #[tokio::test]
    async fn public_boundary_rejects_internal_actions_and_unsafe_peers() {
        let f=Fixture::new();let id=uuid::Uuid::new_v4().to_string();
        for action in ["activated","activate_prepare","delete"] { assert!(f.call(&id,action,json!({})).await.is_err()); }
        for server in ["http://public.example", "https://owner:secret@example.com", "https://example.com/path", "file:///tmp/"] {
            assert!(f.call(&id,"prepare",json!({"peer":{"server":server,"profile":id}})).await.is_err());
        }
        assert!(!crate::workspace_transfer::frozen(&f.app.db.0.lock().unwrap()).unwrap());
    }
}
