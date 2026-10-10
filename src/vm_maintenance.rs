//! Durable computer and provider maintenance. Manual updates restart the guest.
use crate::{
    db::{self, Db},
    runtime::{App, Shared},
    vm,
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::OwnedMutexGuard;

const INTERVAL: i64 = 3 * 86400;
const IDLE: i64 = 900;
const HELPER: &str = include_str!("../deploy/guest-maintenance.py");
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct State {
    pub enabled: bool,
    pub requested: bool,
    pub phase: String,
    pub job_id: String,
    pub attempted: i64,
    pub finished: i64,
    pub last_success: i64,
    pub next_check: i64,
    pub reboot_recommended: bool,
    pub error: String,
    #[serde(default)]
    pub detail: String,
}
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS vm_maintenance(id INTEGER PRIMARY KEY CHECK(id=1),enabled INTEGER NOT NULL DEFAULT 1,phase TEXT NOT NULL DEFAULT 'idle',job_id TEXT NOT NULL DEFAULT '',attempted INTEGER NOT NULL DEFAULT 0,finished INTEGER NOT NULL DEFAULT 0,last_success INTEGER NOT NULL DEFAULT 0,next_check INTEGER NOT NULL DEFAULT 0,reboot_recommended INTEGER NOT NULL DEFAULT 0,error TEXT NOT NULL DEFAULT '',idle_since INTEGER NOT NULL DEFAULT 0); INSERT OR IGNORE INTO vm_maintenance(id) VALUES(1);")?;
    let has_previous: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('vm_maintenance') WHERE name='previous_attempt')", [], |r|r.get(0))?;
    if !has_previous {
        c.execute(
            "ALTER TABLE vm_maintenance ADD COLUMN previous_attempt INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !c.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('vm_maintenance') WHERE name='requested')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        c.execute(
            "ALTER TABLE vm_maintenance ADD COLUMN requested INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('vm_maintenance') WHERE name='detail')", [], |r| r.get::<_, bool>(0))? {
        c.execute("ALTER TABLE vm_maintenance ADD COLUMN detail TEXT NOT NULL DEFAULT ''", [])?;
    }
    Ok(())
}
pub fn busy(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT phase IN ('starting','updating','checking','rebooting') FROM vm_maintenance WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
pub fn state(db: &Db) -> Result<State> {
    Ok(db.0.lock().unwrap().query_row("SELECT enabled,phase,job_id,attempted,finished,last_success,next_check,reboot_recommended,error,requested,detail FROM vm_maintenance WHERE id=1",[],|r|Ok(State {detail:r.get(10)?,requested:r.get(9)?,enabled:r.get(0)?,phase:r.get(1)?,job_id:r.get(2)?,attempted:r.get(3)?,finished:r.get(4)?,last_success:r.get(5)?,next_check:r.get(6)?,reboot_recommended:r.get(7)?,error:r.get(8)?}))?)
}
pub fn available(db: &Db) -> Result<()> {
    ensure!(
        !busy(&db.0.lock().unwrap())?,
        "The bot computer is updating. New tasks will start when updates finish."
    );
    Ok(())
}
pub fn set_enabled(db: &Db, enabled: bool) -> Result<State> {
    // Disabling stops future jobs. Never interrupt a package installation.
    db.0.lock().unwrap().execute(
        "UPDATE vm_maintenance SET enabled=?,idle_since=0 WHERE id=1",
        [enabled],
    )?;
    state(db)
}
pub fn touch(c: &Connection) -> Result<()> {
    c.execute("UPDATE vm_maintenance SET idle_since=0 WHERE id=1", [])?;
    Ok(())
}
fn idle(c: &Connection, now: i64, manual: bool) -> Result<bool> {
    Ok(!crate::workspace_transfer::frozen(c)? && !c.query_row("SELECT EXISTS(SELECT 1 FROM command_jobs WHERE status IN ('starting','running')) OR EXISTS(SELECT 1 FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')) OR EXISTS(SELECT 1 FROM screens WHERE takeover=1 OR quiet_until>?1) OR EXISTS(SELECT 1 FROM settings WHERE (key IN ('takeover','_account_disabled') AND value='true') OR (key='quiet_until' AND CAST(value AS INTEGER)>?1)) OR EXISTS(SELECT 1 FROM events WHERE created>?2) OR EXISTS(SELECT 1 FROM runs WHERE created>?2)",params![now,if manual { i64::MAX } else { now-IDLE }],|r|r.get::<_,bool>(0))?)
}
fn due(db: &Db, now: i64, reserve: bool) -> Result<bool> {
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let (enabled, attempted, next, since, manual): (bool, i64, i64, i64, bool) = tx.query_row(
        "SELECT enabled,attempted,next_check,idle_since,requested FROM vm_maintenance WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if busy(&tx)? || (!enabled && !manual) || !idle(&tx, now, manual)? {
        touch(&tx)?;
        tx.commit()?;
        return Ok(false);
    }
    if since == 0 && !manual {
        tx.execute("UPDATE vm_maintenance SET idle_since=? WHERE id=1", [now])?;
        tx.commit()?;
        return Ok(false);
    }
    let ready = manual
        || (now - since >= IDLE && (attempted == 0 || now - attempted >= INTERVAL) && now >= next);
    if ready && reserve {
        tx.execute("UPDATE vm_maintenance SET phase='starting',job_id=?,previous_attempt=attempted,attempted=?,next_check=?,error='',detail='Updating server providers…',idle_since=0 WHERE id=1",params![db::id(),now,now+INTERVAL])?;
    }
    tx.commit()?;
    Ok(ready)
}
pub fn request(db: &Db) -> Result<State> {
    db.0.lock().unwrap().execute("UPDATE vm_maintenance SET requested=1,error='' WHERE id=1 AND phase NOT IN ('starting','updating','checking','rebooting')", [])?;
    state(db)
}
pub struct Leases {
    _screens: Vec<OwnedMutexGuard<()>>,
    _integrations: OwnedMutexGuard<()>,
}
pub fn leases(app: &App) -> Result<Leases> {
    let integrations = app.integrations.clone().try_lock_owned()?;
    let mut screens = Vec::new();
    for slot in 1..=32 {
        screens.push(app.screen_lock(slot).try_lock_owned()?);
    }
    Ok(Leases {
        _screens: screens,
        _integrations: integrations,
    })
}
async fn update_guest_runtime(app: &App) -> Result<()> {
    ensure!(app.config.vm.guest_binary == "/usr/local/bin/kindred", "Custom guest executable must be updated by its administrator");
    let source=std::env::current_exe()?;
    let size=tokio::fs::metadata(&source).await?.len();
    ensure!(size>0 && size<=256*1024*1024,"Guest runtime size is outside the transfer limit");
    let bytes=tokio::fs::read(source).await?;
    let digest=ring::digest::digest(&ring::digest::SHA256,&bytes);
    let hash=digest.as_ref().iter().map(|b|format!("{b:02x}")).collect::<String>();
    // Only known managed launcher bytes are replaced. Custom launchers fail
    // before the runtime switches; existing browsers/profiles are untouched.
    let launchers=serde_json::json!({
        "browser-launch.py":{"source":include_str!("../deploy/browser-launch.py"),"previous_sha256":null},
        "desktop-launch":{"source":include_str!("../deploy/desktop-launch"),"previous_sha256":"8cae9194cb3d1dc53606cb52aa254a531364bce3c006c0444d93088def9c2ba3"},
        "start-desktop.sh":{"source":include_str!("../deploy/start-desktop.sh"),"previous_sha256":"3d10321fd71359ceac432daa657b299d346aeb6142d07fe33a42d372733803d4"}
    });
    let declaration=format!("LAUNCHERS = json.loads({})",serde_json::to_string(&serde_json::to_string(&launchers)?)?);
    let helper=include_str!("../deploy/update-guest-runtime.py").replace("LAUNCHERS = None", &declaration).replace('\'', "'\"'\"'");
    let mut command=vm::ssh(&app.config.vm);
    command.arg(format!("sudo -n python3 -c '{helper}' {hash} {}",env!("CARGO_PKG_VERSION")));
    let output=vm::capture(command,Some(bytes),90,4096).await?;
    let receipt:Value=serde_json::from_slice(&output)?;
    verified_runtime_update(&receipt, update_optional_driver(app)).await
}

fn managed_launcher_hashes() -> Value {
    let mut values=serde_json::Map::new();
    for (name, source) in [
        ("browser-launch.py",include_str!("../deploy/browser-launch.py")),
        ("desktop-launch",include_str!("../deploy/desktop-launch")),
        ("start-desktop.sh",include_str!("../deploy/start-desktop.sh"))
    ] {
        let digest=ring::digest::digest(&ring::digest::SHA256,source.as_bytes());
        values.insert(name.into(),Value::String(digest.as_ref().iter().map(|b|format!("{b:02x}")).collect()));
    }
    Value::Object(values)
}

async fn verified_runtime_update(
    receipt: &Value,
    optional_driver: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    ensure!(receipt["version"] == env!("CARGO_PKG_VERSION") && receipt["updated"].is_boolean(), "Guest runtime receipt did not verify");
    ensure!(receipt["launchers_updated"].is_boolean() && receipt["launcher_hashes"] == managed_launcher_hashes(), "Guest browser launchers did not verify");
    if let Err(error) = optional_driver.await {
        // The mandatory runtime is already verified. An optional component
        // must not report that it was retained or prevent normal maintenance.
        eprintln!("Optional computer page component unavailable: {error}");
    }
    Ok(())
}

async fn update_optional_driver(app: &App) -> Result<()> {
    // The release carries this payload offline. Existing logged-in browsers are
    // neither restarted nor changed; without an attachable endpoint, pixel tools remain.
    let manifest:Value=serde_json::from_str(include_str!("../deploy/cua-driver-manifest.json"))?;
    let software=std::env::var("KINDRED_GUEST_SOFTWARE").unwrap_or_else(|_|"/opt/kindred/guest".into());
    let payload=std::path::PathBuf::from(software).join("vendor/cua-driver");
    if payload.is_file() {
        let bytes=tokio::fs::read(payload).await?;
        ensure!(bytes.len() as u64 == manifest["binary_bytes"].as_u64().unwrap_or(0),"Bundled driver size mismatch");
        let digest=ring::digest::digest(&ring::digest::SHA256,&bytes);
        let hash=digest.as_ref().iter().map(|b|format!("{b:02x}")).collect::<String>();
        ensure!(Some(hash.as_str())==manifest["binary_sha256"].as_str(),"Bundled driver hash mismatch");
        let helper=include_str!("../deploy/update-cua-driver.py").replace('\'', "'\"'\"'");
        let mut command=vm::ssh(&app.config.vm);
        command.arg(format!("sudo -n python3 -c '{helper}' {hash} {}",bytes.len()));
        let output=vm::capture(command,Some(bytes),90,4096).await?;
        let receipt:Value=serde_json::from_slice(&output)?;
        ensure!(receipt["sha256"]==hash && receipt["updated"].is_boolean(),"Guest driver receipt did not verify");
    }
    Ok(())
}

async fn guest(app: &App, operation: &str, job: Option<&str>) -> Result<Value> {
    let mut cmd = vm::ssh(&app.config.vm);
    let action = if let Some(id) = job {
        ensure!(
            matches!(operation, "start" | "manual" | "reconcile"),
            "Unknown maintenance operation"
        );
        uuid::Uuid::parse_str(id)?;
        format!("{operation} {id}")
    } else {
        "status".into()
    };
    // The loader supplies its own exact source to the durable guest worker.
    cmd.arg(format!("sudo -n python3 -c 'import sys; SOURCE=sys.stdin.read(); exec(compile(SOURCE,\"kindred-maintenance\",\"exec\"))' {action}"));
    let source = format!(
        "CODEX_DOWNLOADER={}\nCODEX_CHECKER={}\nHARNESS_UPDATER={}\nPROVIDER_BRIDGE={}\n{}",
        serde_json::to_string(include_str!("../deploy/download-codex.py"))?,
        serde_json::to_string(include_str!("../deploy/check-codex.py"))?,
        serde_json::to_string(include_str!("../deploy/update-harnesses.py"))?,
        serde_json::to_string(include_str!("../deploy/provider-cli.py"))?,
        HELPER
    );
    let out = vm::capture(cmd, Some(source.into_bytes()), 50, 16384).await?;
    parse_guest(&out)
}
fn parse_guest(out: &[u8]) -> Result<Value> {
    let v: Value = serde_json::from_slice(out)?;
    ensure!(
        v.get("error")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || v["phase"] == "failed",
        "Could not verify the computer update service."
    );
    ensure!(
        matches!(
            v["phase"].as_str(),
            Some(
                "idle"
                    | "starting"
                    | "updating"
                    | "rebooting"
                    | "completed"
                    | "failed"
                    | "deferred"
            )
        ),
        "Unknown computer update state"
    );
    for name in ["attempted", "finished", "retry_at"] {
        if let Some(value) = v.get(name) {
            ensure!(
                value
                    .as_i64()
                    .is_some_and(|n| (0..=253402300799).contains(&n)),
                "Invalid maintenance timestamp"
            );
        }
    }
    if let Some(value) = v.get("reboot_recommended") {
        ensure!(value.is_boolean(), "Invalid reboot state");
    }
    if let Some(value) = v.get("error") {
        ensure!(value.is_string(), "Invalid maintenance error");
    }
    if !matches!(v["phase"].as_str(), Some("idle" | "deferred")) {
        uuid::Uuid::parse_str(v["job_id"].as_str().unwrap_or(""))?;
        ensure!(
            v["attempted"].as_i64().is_some_and(|n| n > 0),
            "Missing maintenance attempt time"
        );
    }
    if matches!(v["phase"].as_str(), Some("completed" | "failed")) {
        ensure!(
            v["finished"]
                .as_i64()
                .is_some_and(|n| n >= v["attempted"].as_i64().unwrap_or(i64::MAX)),
            "Missing maintenance finish time"
        );
    }
    Ok(v)
}
fn reconcile(db: &Db, value: &Value, now: i64) -> Result<bool> {
    let phase = value["phase"].as_str().unwrap_or("checking");
    let active = matches!(phase, "starting" | "updating" | "checking" | "rebooting");
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    if matches!(phase, "completed" | "failed" | "idle") {
        let expected: String =
            tx.query_row("SELECT job_id FROM vm_maintenance WHERE id=1", [], |r| {
                r.get(0)
            })?;
        ensure!(
            !expected.is_empty() && value["job_id"].as_str() == Some(expected.as_str()),
            "The update result belongs to another attempt"
        );
    }
    tx.execute("UPDATE vm_maintenance SET detail=? WHERE id=1", [value["detail"].as_str().unwrap_or("")])?;
    // Preserve the server attempt clock, including uncertain dispatches and guest deferrals.
    let attempted: i64 =
        tx.query_row("SELECT attempted FROM vm_maintenance WHERE id=1", [], |r| {
            r.get(0)
        })?;
    let attempted = if phase == "deferred" {
        value["attempted"].as_i64().unwrap_or(tx.query_row(
            "SELECT previous_attempt FROM vm_maintenance WHERE id=1",
            [],
            |r| r.get(0),
        )?)
    } else {
        attempted
    };
    if phase == "deferred" {
        tx.execute(
            "UPDATE vm_maintenance SET attempted=? WHERE id=1",
            [attempted],
        )?;
    }
    let next = (if attempted > 0 {
        attempted + INTERVAL
    } else {
        0
    })
    .max(value["retry_at"].as_i64().unwrap_or(0));
    tx.execute("UPDATE vm_maintenance SET requested=0,phase=?,job_id=CASE WHEN ?='' THEN job_id ELSE ? END,finished=?,last_success=CASE WHEN ?='completed' THEN ? ELSE last_success END,next_check=?,reboot_recommended=COALESCE(?,reboot_recommended),error=?,idle_since=0 WHERE id=1",params![phase,value["job_id"].as_str().unwrap_or(""),value["job_id"].as_str().unwrap_or(""),value["finished"].as_i64().unwrap_or(0),phase,value["finished"].as_i64().unwrap_or(now),next,value["reboot_recommended"].as_bool(),value["error"].as_str().unwrap_or("")])?;
    tx.commit()?;
    Ok(active)
}
pub async fn worker(app: Shared) {
    let mut held = app.maintenance_leases.lock().unwrap().take();
    loop {
        let result:Result<()>=async {
            if busy(&app.db.0.lock().unwrap())? {
                if held.is_none() {held=Some(leases(&app)?);}
                let id=state(&app.db)?.job_id;
                match guest(&app,"reconcile",Some(&id)).await {
                    Ok(value)=>if !reconcile(&app.db,&value,db::now())? {held=None;},
                    Err(_)=>{app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET phase='checking',error='Waiting to verify computer updates. Tasks will resume once the update service can be checked.' WHERE id=1",[])?;}
                }
            } else if due(&app.db,db::now(),false)? {
                // Do not boot an offline computer for maintenance.
                if vm::control(&app.config.vm,"domstate").await?.trim()!="running" {return Ok(());}
                let acquired=match leases(&app) {Ok(value)=>value,Err(_)=>{touch(&app.db.0.lock().unwrap())?;return Ok(());}};
                // Verify guest identity and noninteractive sudo before reserving
                // a package attempt. A missing prerequisite must not stall tasks.
                let status=match guest(&app,"status",None).await {
                    Ok(value)=>value,
                    Err(_)=>{
                        app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET requested=0,phase='failed',attempted=?,next_check=?,idle_since=0,error='Could not verify the bot computer and its update permissions. Check the computer connection and try again.' WHERE id=1",params![db::now(),db::now()+INTERVAL])?;
                        return Ok(());
                    }
                };
                if matches!(status["phase"].as_str(),Some("starting"|"updating"|"rebooting")) {
                    held=Some(acquired);
                    app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET attempted=? WHERE id=1",[status["attempted"].as_i64().unwrap_or(db::now())])?;
                    reconcile(&app.db,&status,db::now())?;
                    return Ok(());
                }
                if !due(&app.db,db::now(),true)? {return Ok(());}
                held=Some(acquired);
                let id=state(&app.db)?.job_id;
                let mut command=tokio::process::Command::new("python3");
                let pi_root=std::path::Path::new(&app.config.pi.worker_script).parent().ok_or_else(||anyhow::anyhow!("Pi worker has no installation directory"))?;
                command.args(["-", "pi"]).arg(pi_root);
                if let Err(error)=vm::capture(command,Some(include_bytes!("../deploy/update-harnesses.py").to_vec()),1200,16384).await {
                    eprintln!("Pi harness update: {error}");
                    reconcile(&app.db,&serde_json::json!({"phase":"failed","job_id":id,"error":"The server's Pi harness could not be updated. Previous runtime retained; check server logs."}),db::now())?;
                    held=None;
                    return Ok(());
                }
                // Only after idle reservation and screen leases: never replace a
                // guest executable underneath an active managed command.
                if let Err(error)=update_guest_runtime(&app).await {
                    eprintln!("Guest runtime update: {error}");
                    reconcile(&app.db,&serde_json::json!({"phase":"failed","job_id":id,"error":"The Kindred guest runtime could not be updated. The previous runtime is retained; check server logs."}),db::now())?;
                    held=None;
                    return Ok(());
                }
                // Dispatch once. An uncertain response is reconciled by ID next tick; the guest fences a request that never started.
                let operation=if state(&app.db)?.requested {"manual"} else {"start"};
                if let Ok(value)=guest(&app,operation,Some(&id)).await {if !reconcile(&app.db,&value,db::now())? {held=None;}}
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            eprintln!("computer maintenance: {error}");
        }
        // A manual reboot may occur during the three-day cooldown. Clear its
        // reminder only after the guest confirms a different boot, without
        // starting the VM or another update attempt.
        if let Ok(saved) = state(&app.db) {
            if saved.reboot_recommended
                && !matches!(
                    saved.phase.as_str(),
                    "starting" | "updating" | "checking" | "rebooting"
                )
            {
                if let Ok(value) = guest(&app, "status", None).await {
                    if value["reboot_recommended"] == false {
                        let _ = app.db.0.lock().unwrap().execute(
                            "UPDATE vm_maintenance SET reboot_recommended=0 WHERE id=1",
                            [],
                        );
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn apply(db: &Db, value: &Value, now: i64) -> Result<bool> {
        let mut value = value.clone();
        if value.get("job_id").is_none() {
            value["job_id"] = serde_json::json!(state(db)?.job_id);
        }
        reconcile(db, &value, now)
    }
    fn aged(db: &Db, at: i64) {
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE vm_maintenance SET idle_since=? WHERE id=1",
                [at - IDLE],
            )
            .unwrap();
    }
    #[tokio::test]
    async fn authenticated_preference_and_public_audio_asset() {
        let app = crate::tests::app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = crate::web::router(app.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        let denied = client
            .put(format!("{base}/api/computer/maintenance"))
            .json(&serde_json::json!({"enabled":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 401);
        assert!(state(&app.db).unwrap().enabled);
        let denied_manual = client
            .post(format!("{base}/api/computer/maintenance"))
            .send()
            .await
            .unwrap();
        assert_eq!(denied_manual.status(), 401);
        assert!(!state(&app.db).unwrap().requested);
        let manual = client
            .post(format!("{base}/api/computer/maintenance"))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap();
        assert_eq!(manual.status(), 200);
        assert!(state(&app.db).unwrap().requested);
        let saved = client
            .put(format!("{base}/api/computer/maintenance"))
            .bearer_auth(&app.token)
            .json(&serde_json::json!({"enabled":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(saved.status(), 200);
        assert!(!state(&app.db).unwrap().enabled);
        let invalid = client
            .put(format!("{base}/api/computer/maintenance"))
            .bearer_auth(&app.token)
            .json(&serde_json::json!({"enabled":"yes"}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), 400);
        assert!(!state(&app.db).unwrap().enabled);
        let sound = client
            .get(format!("{base}/audio/kindred-pop.wav"))
            .send()
            .await
            .unwrap();
        assert_eq!(sound.status(), 200);
        assert_eq!(sound.headers()["content-type"], "audio/wav");
        let bytes = sound.bytes().await.unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 20204);
        server.abort();
    }
    #[tokio::test]
    async fn optional_driver_failure_does_not_fail_verified_runtime_update() {
        let app = crate::tests::app();
        let db = &app.db;
        request(&db).unwrap();
        assert!(due(&db, db::now(), true).unwrap());
        let root = std::env::temp_dir().join(format!("kindred-optional-driver-{}", db::id()));
        std::fs::create_dir(&root).unwrap();
        let target = root.join("driver");
        std::fs::write(&target, b"previous optional driver").unwrap();
        let source = serde_json::to_string(include_str!("../deploy/update-cua-driver.py")).unwrap();
        let destination = serde_json::to_string(target.to_str().unwrap()).unwrap();
        let mut helper = tokio::process::Command::new("python3");
        helper.arg("-c").arg(format!("import io; scope={{'__name__':'fixture'}}; exec({source}, scope); scope['install'](io.BytesIO(b'bad'), {destination}, '0'*64, 1)"));
        verified_runtime_update(&serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"updated":true,"launchers_updated":true,"launcher_hashes":managed_launcher_hashes()}), async {
            vm::capture(helper, None, 5, 4096).await?;
            anyhow::bail!("Expected actual helper transfer failure")
        }).await.expect("Real failed driver helper must not fail the verified runtime update");
        assert_eq!(std::fs::read(&target).unwrap(), b"previous optional driver");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "Failed staged transfer must clean up");
        std::fs::remove_dir_all(root).unwrap();
        for reason in ["Bundled driver size mismatch", "Bundled driver hash mismatch", "SSH/helper failed", "Guest driver receipt did not verify"] {
            verified_runtime_update(&serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"updated":true,"launchers_updated":true,"launcher_hashes":managed_launcher_hashes()}), async { Err(anyhow::anyhow!(reason)) })
                .await.expect("Optional driver failure must not report that the verified runtime was retained");
            assert_eq!(state(&db).unwrap().phase, "starting");
            assert!(state(&db).unwrap().error.is_empty());
        }
        // The following real maintenance projection can complete normally.
        apply(&db, &serde_json::json!({"phase":"completed","finished":db::now(),"error":""}), db::now()).unwrap();
        assert_eq!(state(&db).unwrap().phase, "completed");
        assert!(state(&db).unwrap().error.is_empty());
        assert!(verified_runtime_update(&serde_json::json!({"version":"wrong","updated":true}), async { panic!("Invalid runtime receipt must not start optional installation") }).await.is_err());
    }
    #[tokio::test]
    async fn verified_runtime_requires_exact_launcher_receipt_before_optional_driver() {
        for hashes in [serde_json::json!({}),serde_json::json!({"browser-launch.py":"wrong"})] {
            let receipt=serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"updated":true,"launchers_updated":false,"launcher_hashes":hashes});
            assert!(verified_runtime_update(&receipt, async { panic!("Unverified launchers must not start optional driver installation") }).await.is_err());
        }
        assert!(verified_runtime_update(&serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"updated":true}), async { panic!("Binary-only receipt must not imply launcher installation") }).await.is_err());
    }

    #[test]
    fn manual_updates_bypass_schedule_but_wait_for_work_and_preserve_preference() {
        let db = Db::open(":memory:").unwrap();
        set_enabled(&db, false).unwrap();
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE vm_maintenance SET attempted=?,next_check=? WHERE id=1",
                params![db::now(), db::now() + INTERVAL],
            )
            .unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let run = db.queue(&bot.id, "finish first", 0).unwrap();
        request(&db).unwrap();
        assert!(!due(&db, db::now(), true).unwrap());
        db.finish(&run, "completed", "done", "").unwrap();
        assert!(due(&db, db::now(), true).unwrap());
        let id = state(&db).unwrap().job_id;
        request(&db).unwrap();
        assert_eq!(state(&db).unwrap().job_id, id);
        apply(&db, &serde_json::json!({"phase":"rebooting"}), db::now()).unwrap();
        assert!(available(&db).is_err());
        assert!(!state(&db).unwrap().requested);
        apply(&db, &serde_json::json!({"phase":"completed"}), db::now()).unwrap();
        assert!(available(&db).is_ok());
        assert!(!state(&db).unwrap().enabled);
    }
    #[test]
    fn only_idle_workspaces_reserve_and_every_attempt_waits_three_days() {
        let db = Db::open(":memory:").unwrap();
        let at = db::now() + IDLE + 1;
        assert!(!due(&db, at, false).unwrap());
        assert!(!due(&db, at + IDLE - 1, true).unwrap());
        assert!(due(&db, at + IDLE, true).unwrap());
        assert!(busy(&db.0.lock().unwrap()).unwrap());
        let id = state(&db).unwrap().job_id;
        assert!(!due(&db, at + 2 * IDLE, true).unwrap());
        assert_eq!(id, state(&db).unwrap().job_id);
        apply(&db,&serde_json::json!({"phase":"failed","error":"Package installation failed (exit 100)."}),at+IDLE+60).unwrap();
        aged(&db, at + INTERVAL);
        assert!(!due(&db, at + INTERVAL, true).unwrap());
        assert!(due(&db, at + IDLE + INTERVAL, true).unwrap());
        assert_ne!(id, state(&db).unwrap().job_id);
    }
    #[test]
    fn completed_guest_response_with_empty_error_releases_reservation() {
        let db = Db::open(":memory:").unwrap();
        let at = db::now();
        aged(&db, at);
        assert!(due(&db, at, true).unwrap());
        let response=parse_guest(br#"{"phase":"completed","job_id":"cae84a0a-8eb4-41bc-bca4-d9f0954e9bfa","attempted":100,"error":"","finished":123,"changed":false,"reboot_recommended":false}"#).unwrap();
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE vm_maintenance SET job_id=? WHERE id=1",
                [response["job_id"].as_str().unwrap()],
            )
            .unwrap();
        assert!(!apply(&db, &response, at + 60).unwrap());
        assert!(!busy(&db.0.lock().unwrap()).unwrap());
        assert!(parse_guest(br#"{"phase":"updating","job_id":"cae84a0a-8eb4-41bc-bca4-d9f0954e9bfa","attempted":100,"error":""}"#).is_ok());
        assert!(
            parse_guest(br#"{"phase":"failed","job_id":"cae84a0a-8eb4-41bc-bca4-d9f0954e9bfa","attempted":100,"finished":101,"error":"Package index update failed (exit 100)."}"#)
                .is_ok()
        );
        assert!(parse_guest(br#"{"error":"Could not verify automatic updates."}"#).is_err());
        assert!(parse_guest(br#"{"phase":"completed","error":"Unverified result"}"#).is_err());
    }
    #[test]
    fn stale_or_incomplete_results_cannot_release_an_active_reservation() {
        let db = Db::open(":memory:").unwrap();
        let at = db::now();
        aged(&db, at);
        assert!(due(&db, at, true).unwrap());
        let expected = state(&db).unwrap().job_id;
        for value in [
            serde_json::json!({"phase":"completed","job_id":db::id()}),
            serde_json::json!({"phase":"idle"}),
        ] {
            assert!(reconcile(&db, &value, at + 60).is_err());
            assert!(busy(&db.0.lock().unwrap()).unwrap());
            assert_eq!(state(&db).unwrap().job_id, expected);
        }
        for value in [
            serde_json::json!({"phase":"completed"}),
            serde_json::json!({"phase":"updating","job_id":"invalid","attempted":1}),
            serde_json::json!({"phase":"deferred","retry_at":i64::MAX}),
            serde_json::json!({"phase":"idle","reboot_recommended":"false"}),
            serde_json::json!({"phase":"idle","error":false}),
        ] {
            assert!(parse_guest(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    #[test]
    fn a_startup_deferral_preserves_an_existing_reboot_reminder() {
        let db = Db::open(":memory:").unwrap();
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE vm_maintenance SET reboot_recommended=1 WHERE id=1",
                [],
            )
            .unwrap();
        apply(
            &db,
            &serde_json::json!({"phase":"deferred","retry_at":db::now()+300}),
            db::now(),
        )
        .unwrap();
        assert!(state(&db).unwrap().reboot_recommended);
    }
    #[test]
    fn restarting_a_server_requires_fresh_observed_idle_time() {
        let root = std::env::temp_dir().join(format!("kindred-idle-{}", db::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("test.db");
        {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            aged(&db, db::now());
        }
        let mut config = crate::config::Config::default();
        config.database = path.to_str().unwrap().into();
        let app = App::open(config, "test".into()).unwrap();
        assert!(!due(&app.db, db::now(), true).unwrap());
        assert!(!busy(&app.db.0.lock().unwrap()).unwrap());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn guest_startup_deferral_is_not_a_package_attempt() {
        let db = Db::open(":memory:").unwrap();
        let at = db::now();
        aged(&db, at);
        assert!(due(&db, at, true).unwrap());
        apply(
            &db,
            &serde_json::json!({"phase":"deferred","retry_at":at+IDLE}),
            at,
        )
        .unwrap();
        assert_eq!(state(&db).unwrap().attempted, 0);
        assert_eq!(state(&db).unwrap().next_check, at + IDLE);
        aged(&db, at + IDLE);
        assert!(due(&db, at + IDLE, true).unwrap());
    }
    #[test]
    fn queued_work_manual_control_disable_and_transfer_block_start() {
        let db = Db::open(":memory:").unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let slot = db.screen(&bot.id).unwrap();
        let at = db::now() + IDLE + 1;
        aged(&db, at);
        crate::screen_control::set(&db, slot, true, "manual").unwrap();
        assert!(!due(&db, at, true).unwrap());
        crate::screen_control::set(&db, slot, false, "manual").unwrap();
        aged(&db, at);
        let id = db.queue(&bot.id, "User work first", 0).unwrap();
        assert!(!due(&db, at, true).unwrap());
        db.finish(&id, "completed", "ok", "").unwrap();
        set_enabled(&db, false).unwrap();
        aged(&db, at);
        assert!(!due(&db, at, true).unwrap());
        set_enabled(&db, true).unwrap();
        db.0.lock()
            .unwrap()
            .execute(
                "INSERT INTO workspace_transfers(id,state,package) VALUES('test','prepared','{}')",
                [],
            )
            .unwrap();
        aged(&db, at);
        assert!(!due(&db, at, true).unwrap());
    }
    #[test]
    fn new_tasks_wait_and_disabling_does_not_interrupt_packages() {
        let db = Db::open(":memory:").unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let slot = db.screen(&bot.id).unwrap();
        let at = db::now() + IDLE + 1;
        aged(&db, at);
        assert!(due(&db, at, true).unwrap());
        let run = db.queue(&bot.id, "Wait through maintenance", 0).unwrap();
        assert!(db.claim_bot(&bot.id).unwrap().is_none());
        assert!(crate::screen_control::set(&db, slot, true, "manual").is_err());
        assert!(available(&db).is_err());
        set_enabled(&db, false).unwrap();
        assert!(busy(&db.0.lock().unwrap()).unwrap());
        apply(&db, &serde_json::json!({"phase":"updating"}), at + 60).unwrap();
        assert!(db.claim_bot(&bot.id).unwrap().is_none());
        apply(
            &db,
            &serde_json::json!({"phase":"completed","finished":at+120,"reboot_recommended":true}),
            at + 120,
        )
        .unwrap();
        assert_eq!(db.claim_bot(&bot.id).unwrap().unwrap().id, run);
        assert!(!state(&db).unwrap().enabled);
        assert!(state(&db).unwrap().reboot_recommended);
    }
    #[test]
    fn restart_takes_all_leases_before_clients_can_use_the_vm() {
        let root = std::env::temp_dir().join(format!("kindred-maintenance-{}", db::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("test.db");
        {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            aged(&db, db::now());
            assert!(due(&db, db::now(), true).unwrap());
        }
        let mut config = crate::config::Config::default();
        config.database = path.to_str().unwrap().into();
        let app = App::open(config, "test".into()).unwrap();
        for slot in 1..=32 {
            assert!(app.screen_lock(slot).try_lock().is_err());
        }
        assert!(app.integrations.try_lock().is_err());
        assert!(busy(&app.db.0.lock().unwrap()).unwrap());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_busy_other_screen_or_integration_prevents_allocation() {
        let app = crate::tests::app();
        let held = app.screen_lock(17).try_lock_owned().unwrap();
        assert!(leases(&app).is_err());
        assert!(app.screen_lock(1).try_lock().is_ok());
        drop(held);
        let held = app.integrations.try_lock().unwrap();
        assert!(leases(&app).is_err());
        drop(held);
        assert!(leases(&app).is_ok());
    }
}
