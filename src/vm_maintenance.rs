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
    pub runtime_delivery: Option<RuntimeDelivery>,
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
#[derive(Serialize, Deserialize, Clone)]
pub struct RuntimeDelivery {
    pub job_id: String,
    pub target: Value,
    pub phase: String,
    pub error: String,
    previous: Option<Value>,
}
fn runtime_record(c: &Connection) -> Result<Option<RuntimeDelivery>> {
    let raw:String=c.query_row("SELECT runtime_delivery FROM vm_maintenance WHERE id=1",[],|r|r.get(0))?;
    Ok(if raw.is_empty() {None} else {Some(serde_json::from_str(&raw)?)})
}
fn save_runtime(c:&Connection, value:&RuntimeDelivery)->Result<()> {
    c.execute("UPDATE vm_maintenance SET runtime_delivery=? WHERE id=1",[serde_json::to_string(value)?])?;
    Ok(())
}
fn runtime_pending(value:&RuntimeDelivery)->Result<bool> {
    ensure!(matches!(value.phase.as_str(),"pending"|"starting"|"checking"|"completed"|"failed"),"Unknown runtime delivery state");
    Ok(matches!(value.phase.as_str(),"pending"|"starting"|"checking"))
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
    if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('vm_maintenance') WHERE name='runtime_delivery')", [], |r| r.get::<_,bool>(0))? {
        c.execute("ALTER TABLE vm_maintenance ADD COLUMN runtime_delivery TEXT NOT NULL DEFAULT ''", [])?;
    }
    Ok(())
}
pub fn busy(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT phase IN ('starting','updating','checking','rebooting') OR CASE WHEN runtime_delivery='' THEN 0 WHEN NOT json_valid(runtime_delivery) THEN 1 ELSE COALESCE(json_extract(runtime_delivery,'$.phase') NOT IN ('pending','completed','failed'),1) END FROM vm_maintenance WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
pub fn state(db: &Db) -> Result<State> {
    let c=db.0.lock().unwrap();
    let runtime_delivery=runtime_record(&c)?;
    Ok(c.query_row("SELECT enabled,phase,job_id,attempted,finished,last_success,next_check,reboot_recommended,error,requested,detail FROM vm_maintenance WHERE id=1",[],|r|Ok(State {runtime_delivery:runtime_delivery.clone(),detail:r.get(10)?,requested:r.get(9)?,enabled:r.get(0)?,phase:r.get(1)?,job_id:r.get(2)?,attempted:r.get(3)?,finished:r.get(4)?,last_success:r.get(5)?,next_check:r.get(6)?,reboot_recommended:r.get(7)?,error:r.get(8)?}))?)
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
    if runtime_record(&tx)?.as_ref().map(runtime_pending).transpose()?.unwrap_or(false) {return Ok(false);}
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
    {
        let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
        ensure!(!runtime_record(&tx)?.as_ref().map(runtime_pending).transpose()?.unwrap_or(false),"A runtime delivery request is already pending");
        tx.execute("UPDATE vm_maintenance SET requested=1,error='',detail='' WHERE id=1 AND phase NOT IN ('starting','updating','checking','rebooting')", [])?;
        tx.commit()?;
    }
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
    let (bytes,hash)=runtime_bytes().await?;
    // Only known managed launcher bytes are replaced. Custom launchers fail
    // before the runtime switches; existing browsers/profiles are untouched.
    let launchers=managed_launchers();
    let declaration=format!("LAUNCHERS = json.loads({})",serde_json::to_string(&serde_json::to_string(&launchers)?)?);
    let helper=include_str!("../deploy/update-guest-runtime.py").replace("LAUNCHERS = None", &declaration).replace('\'', "'\"'\"'");
    let mut command=vm::ssh(&app.config.vm);
    command.arg(format!("sudo -n python3 -c '{helper}' {hash} {}",env!("CARGO_PKG_VERSION")));
    let output=vm::capture(command,Some(bytes),90,4096).await?;
    let receipt:Value=serde_json::from_slice(&output)?;
    verified_runtime_update(&receipt, update_optional_driver(app)).await
}

async fn runtime_bytes()->Result<(Vec<u8>,String)> {
    let source=std::env::current_exe()?;
    let size=tokio::fs::metadata(&source).await?.len();
    ensure!(size>0 && size<=256*1024*1024,"Guest runtime size is outside the transfer limit");
    let bytes=tokio::fs::read(source).await?;
    let digest=ring::digest::digest(&ring::digest::SHA256,&bytes);
    let hash=digest.as_ref().iter().map(|b|format!("{b:02x}")).collect::<String>();
    Ok((bytes,hash))
}
fn managed_launchers() -> Value {
    serde_json::json!({
        "wait-screen-ready":{"source":include_str!("../deploy/wait-screen-ready"),"previous_sha256":["69e2c4b24709917c9a0b038e17dd9b1e2f6f028859d66b1078dd026f22c35eff",null]},
        "browser-launch.py":{"source":include_str!("../deploy/browser-launch.py"),"previous_sha256":["f0d21bee517c16ce234a4ac233dfbf8b1009ddcc70ec0063bbaadd3bd4ded9a8","1166f7ad32848fa71b4084960825bc40c9bbe03300d8c23c0d6710533efd8ee2",null]},
        "desktop-launch":{"source":include_str!("../deploy/desktop-launch"),"previous_sha256":["b8b477780f13889b14176510d6d2c805b4eb9c6e6d73ade8c3793c41cddf9645", "ee430558ef4fd501d1dd55a453e4de9c9d9fc58018464a1091dee7adfccaa1df", "8cae9194cb3d1dc53606cb52aa254a531364bce3c006c0444d93088def9c2ba3"]},
        "start-desktop.sh":{"source":include_str!("../deploy/start-desktop.sh"),"previous_sha256":["df6b0b492f39da89f743df24793606ad35244fb8616e3ad88ad3dae3c4cf467c", "45eb0cdb9e2ea03b86f8c6d21d96f769ebaa07d86d04e2f442da6732b6aaf3c6", "3d10321fd71359ceac432daa657b299d346aeb6142d07fe33a42d372733803d4"]},
        "ensure-screen":{"source":include_str!("../deploy/ensure-screen"),"previous_sha256":["de417ce99b63a1effd3f4c37efab7fb2e285ed7a7bfb8888ded84cbfc4fc27d9"]},
        "screen-ownership.py":{"source":include_str!("../deploy/screen-ownership.py"),"previous_sha256":["1c309e1bbbb04b7d597328e09c060060a6499daeaa4c266ddedf1e59049d2fca","e3e81b892467ae509ecd873ce786407ef5bc9d0a5781ea37edd119019e26f004",null]}
    })
}
pub async fn request_runtime(app:&App)->Result<State> {
    ensure!(app.config.vm.guest_binary=="/usr/local/bin/kindred","Custom guest executable must be updated by its administrator");
    let (_,hash)=runtime_bytes().await?;
    request_runtime_target(&app.db,serde_json::json!({"sha256":hash,"version":env!("CARGO_PKG_VERSION"),"launcher_hashes":managed_launcher_hashes()}))
}
fn request_runtime_target(db:&Db,target:Value)->Result<State> {
    validate_runtime_target(&target)?;
    {
        let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
        if let Some(saved)=runtime_record(&tx)? {
            if runtime_pending(&saved)? {
                ensure!(saved.target==target,"Another runtime target is already pending");
                tx.commit()?;drop(c);return state(db);
            }
        }
        ensure!(!busy(&tx)?,"Wait for current computer maintenance to finish");
        save_runtime(&tx,&RuntimeDelivery {job_id:db::id(),target,phase:"pending".into(),error:String::new(),previous:None})?;
        tx.commit()?;
    }
    state(db)
}
fn validate_runtime_target(target:&Value)->Result<()> {
    let hash=target["sha256"].as_str().unwrap_or("");
    ensure!(hash.len()==64 && hash.bytes().all(|v|v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),"Invalid runtime target hash");
    ensure!(target["version"]==env!("CARGO_PKG_VERSION") && target["launcher_hashes"]==managed_launcher_hashes(),"Runtime target does not match this server's helpers");
    Ok(())
}
fn reserve_runtime(db:&Db)->Result<Option<RuntimeDelivery>> {
    let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
    let Some(mut value)=runtime_record(&tx)? else {return Ok(None)};
    if value.phase!="pending" {return Ok(None);}
    validate_runtime_target(&value.target)?;
    if busy(&tx)? || !idle(&tx,db::now(),true)? {return Ok(None);}
    value.previous=Some(tx.query_row("SELECT phase,job_id,finished,error,detail FROM vm_maintenance WHERE id=1",[],|r|Ok(serde_json::json!({"phase":r.get::<_,String>(0)?,"job_id":r.get::<_,String>(1)?,"finished":r.get::<_,i64>(2)?,"error":r.get::<_,String>(3)?,"detail":r.get::<_,String>(4)?})))?);
    value.phase="starting".into();save_runtime(&tx,&value)?;
    tx.execute("UPDATE vm_maintenance SET phase='starting',job_id=?,error='',detail='Delivering matching computer runtime…' WHERE id=1",[&value.job_id])?;
    tx.commit()?;Ok(Some(value))
}
fn settle_runtime(db:&Db,receipt:&Value)->Result<bool> {
    let mut c=db.0.lock().unwrap();let tx=c.transaction()?;
    let mut value=runtime_record(&tx)?.ok_or_else(||anyhow::anyhow!("Missing runtime operation"))?;
    ensure!(matches!(value.phase.as_str(),"starting"|"checking"),"Runtime operation is not active");
    ensure!(receipt["job_id"]==value.job_id && receipt["sha256"]==value.target["sha256"] && receipt["version"]==value.target["version"] && receipt["launcher_hashes"]==value.target["launcher_hashes"],"Runtime receipt belongs to another target or operation");
    let phase=receipt["phase"].as_str().unwrap_or("");ensure!(matches!(phase,"completed"|"failed"|"checking"),"Unknown runtime receipt state");
    if phase=="completed" {ensure!(receipt["updated"].is_boolean() && receipt["launchers_updated"].is_boolean(),"Incomplete runtime receipt");}
    value.phase=phase.into();value.error=if phase=="completed" {String::new()} else {"Matching runtime delivery needs verification or a new explicitly selected attempt.".into()};
    if phase=="checking" {
        tx.execute("UPDATE vm_maintenance SET phase='checking',error='Matching runtime delivery needs recovery. Tasks remain paused.' WHERE id=1",[])?;
    } else {
        let old=value.previous.as_ref().ok_or_else(||anyhow::anyhow!("Missing predecessor maintenance state"))?;
        tx.execute("UPDATE vm_maintenance SET phase=?,job_id=?,finished=?,error=?,detail=? WHERE id=1",params![old["phase"].as_str(),old["job_id"].as_str(),old["finished"].as_i64(),old["error"].as_str(),old["detail"].as_str()])?;
    }
    save_runtime(&tx,&value)?;tx.commit()?;Ok(phase=="checking")
}
async fn runtime_delivery(app:&App,value:&RuntimeDelivery,operation:&str)->Result<Value> {
    ensure!(matches!(operation,"install"|"status"),"Unknown runtime operation");
    ensure!(app.config.vm.guest_binary=="/usr/local/bin/kindred","Custom guest executable must be updated by its administrator");
    validate_runtime_target(&value.target)?;uuid::Uuid::parse_str(&value.job_id)?;
    let (bytes,hash)=if operation=="install" {
        let (bytes,hash)=runtime_bytes().await?;
        ensure!(value.target["sha256"]==hash,"Selected runtime binary changed; reservation retained");
        (bytes,hash)
    } else {(Vec::new(),value.target["sha256"].as_str().unwrap_or("").to_owned())};
    let (command,input)=runtime_delivery_command(app,value,operation,bytes,&hash)?;
    let out=vm::capture(command,input,90,4096).await?;
    Ok(serde_json::from_slice(&out)?)
}

fn runtime_delivery_command(app:&App,value:&RuntimeDelivery,operation:&str,bytes:Vec<u8>,hash:&str)->Result<(tokio::process::Command,Option<Vec<u8>>)> {
    ensure!(matches!(operation,"install"|"status"),"Unknown runtime operation");
    validate_runtime_target(&value.target)?;uuid::Uuid::parse_str(&value.job_id)?;
    ensure!(value.target["sha256"]==hash,"Runtime bytes do not match the selected target");
    let declaration=format!("LAUNCHERS = json.loads({})",serde_json::to_string(&serde_json::to_string(&managed_launchers())?)?);
    let helper=include_str!("../deploy/update-guest-runtime.py").replace("LAUNCHERS = None",&declaration).replace('\'',"'\"'\"'");
    let mut command=vm::ssh(&app.config.vm);
    command.arg(format!("sudo -n python3 -c '{helper}' {hash} {} {operation} {}",env!("CARGO_PKG_VERSION"),value.job_id));
    Ok((command,if operation=="install" {Some(bytes)} else {None}))
}

pub(crate) fn managed_launcher_hashes() -> Value {
    let mut values=serde_json::Map::new();
    for (name, source) in [
        ("wait-screen-ready",include_str!("../deploy/wait-screen-ready")),
        ("browser-launch.py",include_str!("../deploy/browser-launch.py")),
        ("desktop-launch",include_str!("../deploy/desktop-launch")),
        ("start-desktop.sh",include_str!("../deploy/start-desktop.sh")),
        ("ensure-screen",include_str!("../deploy/ensure-screen")),
        ("screen-ownership.py",include_str!("../deploy/screen-ownership.py"))
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
        let mut runtime_cycle=false;
        let result:Result<()>=async {
            let runtime={let c=app.db.0.lock().unwrap();runtime_record(&c)?};
            if let Some(value)=runtime {
                if runtime_pending(&value)? {
                    runtime_cycle=true;
                    if value.phase=="pending" {
                        // No start/boot request, and no optional maintenance path.
                        let idle_now={let c=app.db.0.lock().unwrap();idle(&c,db::now(),true)?};
                        if !idle_now || vm::control(&app.config.vm,"domstate").await?.trim()!="running" {return Ok(());}
                        let acquired=match leases(&app) {Ok(v)=>v,Err(_)=>return Ok(())};
                        if let Some(selected)=reserve_runtime(&app.db)? {
                            held=Some(acquired);
                            // Durable reservation precedes exactly one dispatch. Errors
                            // keep it active; every later tick is status-only.
                            if let Ok(receipt)=runtime_delivery(&app,&selected,"install").await {
                                if !settle_runtime(&app.db,&receipt)? {held=None;}
                            }
                        }
                    } else {
                        if held.is_none() {held=Some(leases(&app)?);}
                        if let Ok(receipt)=runtime_delivery(&app,&value,"status").await {
                            if !settle_runtime(&app.db,&receipt)? {held=None;}
                        }
                    }
                    return Ok(());
                }
            }
            if busy(&app.db.0.lock().unwrap())? {
                if held.is_none() {held=Some(leases(&app)?);}
                let id=state(&app.db)?.job_id;
                match guest(&app,"reconcile",Some(&id)).await {
                    Ok(value)=>if !reconcile(&app.db,&value,db::now())? {held=None;},
                    Err(_)=>{app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET phase='checking',error='Waiting to verify computer updates. Tasks will resume once the update service can be checked.' WHERE id=1",[])?;}
                }
            } else if due(&app.db,db::now(),false)? {
                // Do not boot an offline computer for maintenance.
                if vm::control(&app.config.vm,"domstate").await?.trim()!="running" {
                    app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET detail='Computer is off. Start the computer to install the queued update.' WHERE id=1 AND requested=1",[])?;
                    return Ok(());
                }
                let acquired=match leases(&app) {Ok(value)=>value,Err(_)=>{touch(&app.db.0.lock().unwrap())?;return Ok(());}};
                // Verify guest identity and noninteractive sudo before reserving
                // a package attempt. A missing prerequisite must not stall tasks.
                let status=match guest(&app,"status",None).await {
                    Ok(value)=>value,
                    Err(_)=>{
                        app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET detail='Computer is running, but its update service is not ready. Waiting for the guest connection and update permissions.',idle_since=0 WHERE id=1",[])?;
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
            if !runtime_cycle && saved.reboot_recommended
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
    async fn computer_power_reads_do_not_start_offline_guest_or_fail_queued_update() {
        let root=std::env::temp_dir().join(format!("kindred-power-{}",db::id()));
        std::fs::create_dir(&root).unwrap();
        let manager=root.join("manager.py");
        std::fs::write(&manager,"import sys,json\nassert sys.argv[1]=='domstate'\nprint(json.dumps({'state':'shut off'}))\n").unwrap();
        let mut app=crate::tests::app();
        let config=&mut std::sync::Arc::get_mut(&mut app).unwrap().config.vm;
        config.manager=manager.to_str().unwrap().into();config.managed_id=db::id();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("http://{}/api/computer/state",listener.local_addr().unwrap());
        let router=crate::web::router(app.clone());
        let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap()});
        let client=reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(),401);
        let value:Value=client.get(&url).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
        assert_eq!(value["state"],"shut off");
        request(&app.db).unwrap();
        let task=tokio::spawn(worker(app.clone()));
        tokio::time::timeout(Duration::from_secs(5),async {
            while state(&app.db).unwrap().detail.is_empty(){tokio::time::sleep(Duration::from_millis(10)).await;}
        }).await.unwrap();
        let saved=state(&app.db).unwrap();
        assert!(saved.requested);assert!(saved.error.is_empty());assert_eq!(saved.attempted,0);
        assert!(saved.detail.contains("Computer is off"));
        task.abort();server.abort();std::fs::remove_dir_all(root).unwrap();
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


#[cfg(test)]
mod runtime_delivery_tests {
    use super::*;
    fn target()->Value {serde_json::json!({"sha256":"a".repeat(64),"version":env!("CARGO_PKG_VERSION"),"launcher_hashes":managed_launcher_hashes()})}
    fn receipt(value:&RuntimeDelivery,phase:&str)->Value {serde_json::json!({"job_id":value.job_id,"sha256":value.target["sha256"],"version":value.target["version"],"launcher_hashes":value.target["launcher_hashes"],"phase":phase,"updated":true,"launchers_updated":false})}
    fn clocks(db:&Db)->Value {
        db.0.lock().unwrap().query_row("SELECT enabled,requested,attempted,next_check,last_success,previous_attempt,reboot_recommended FROM vm_maintenance WHERE id=1",[],|r|Ok(serde_json::json!([r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?]))).unwrap()
    }
    #[test]
    fn runtime_bypasses_package_cooldown_and_preserves_pending_package_state() {
        let db=Db::open(":memory:").unwrap();
        db.0.lock().unwrap().execute("UPDATE vm_maintenance SET enabled=0,requested=1,attempted=100,next_check=9999999999,last_success=80,previous_attempt=50,reboot_recommended=1,phase='failed',job_id='old-package',error='old-package-error',detail='old-detail',finished=101 WHERE id=1",[]).unwrap();
        let before=clocks(&db);request_runtime_target(&db,target()).unwrap();let selected=reserve_runtime(&db).unwrap().unwrap();
        assert_eq!(clocks(&db),before);assert!(busy(&db.0.lock().unwrap()).unwrap());
        assert!(!settle_runtime(&db,&receipt(&selected,"completed")).unwrap());assert_eq!(clocks(&db),before);
        let state=state(&db).unwrap();assert_eq!(state.phase,"failed");assert_eq!(state.job_id,"old-package");assert_eq!(state.error,"old-package-error");assert_eq!(state.finished,101);assert_eq!(state.runtime_delivery.unwrap().phase,"completed");
    }
    #[test]
    fn duplicate_and_foreign_requests_cannot_overwrite_pending_target() {
        let db=Db::open(":memory:").unwrap();let first=request_runtime_target(&db,target()).unwrap().runtime_delivery.unwrap();
        assert_eq!(request_runtime_target(&db,target()).unwrap().runtime_delivery.unwrap().job_id,first.job_id);
        let mut foreign=target();foreign["sha256"]=serde_json::json!("b".repeat(64));assert!(request_runtime_target(&db,foreign).is_err());assert!(request(&db).is_err());
        let held=reserve_runtime(&db).unwrap().unwrap();assert_eq!(held.job_id,first.job_id);assert!(!due(&db,db::now(),true).unwrap());
        assert_eq!(request_runtime_target(&db,target()).unwrap().runtime_delivery.unwrap().job_id,first.job_id);
    }
    #[test]
    fn running_queued_commands_takeover_and_transfer_block_runtime_reservation() {
        for kind in ["run","command","takeover","transfer","disabled"] {
            let db=Db::open(":memory:").unwrap();let bot=crate::tests::bot(&db,"codex");let slot=db.screen(&bot.id).unwrap();
            request_runtime_target(&db,target()).unwrap();
            match kind {
                "run"=>{db.queue(&bot.id,"do not interrupt",0).unwrap();},
                "command"=>{ // The real existing command-job predicate is tested without dispatching a command.
                    let run=db.queue(&bot.id,"owned command",0).unwrap();db.finish(&run,"completed","done","").unwrap();
                    let c=db.0.lock().unwrap();c.execute("INSERT INTO command_jobs(id,bot_id,run_id,device_id,screen,title,status,created) VALUES('owned',?1,?2,'',?3,'owned','running',1)",params![bot.id,run,slot]).unwrap();
                },
                "takeover"=>{crate::screen_control::set(&db,slot,true,"manual").unwrap();},
                "transfer"=>{db.0.lock().unwrap().execute("INSERT INTO workspace_transfers(id,state,package) VALUES('owned','prepared','{}')",[]).unwrap();},
                _=>{db.save_setting("_account_disabled",&serde_json::json!(true)).unwrap();}
            }
            assert!(reserve_runtime(&db).unwrap().is_none(),"{kind}");assert!(!busy(&db.0.lock().unwrap()).unwrap());
        }
    }
    #[test]
    fn uncertain_partial_foreign_receipts_keep_reservation_and_work_fenced() {
        let db=Db::open(":memory:").unwrap();let bot=crate::tests::bot(&db,"codex");request_runtime_target(&db,target()).unwrap();let selected=reserve_runtime(&db).unwrap().unwrap();
        let run=db.queue(&bot.id,"wait",0).unwrap();assert!(db.claim_bot(&bot.id).unwrap().is_none());
        let mut wrong=receipt(&selected,"completed");wrong["job_id"]=serde_json::json!(db::id());assert!(settle_runtime(&db,&wrong).is_err());
        assert!(settle_runtime(&db,&receipt(&selected,"checking")).unwrap());assert!(db.claim_bot(&bot.id).unwrap().is_none());
        assert!(reserve_runtime(&db).unwrap().is_none()); // Never redispatch a selected operation.
        assert!(!settle_runtime(&db,&receipt(&selected,"failed")).unwrap());assert_eq!(db.claim_bot(&bot.id).unwrap().unwrap().id,run);
    }
    #[test]
    fn unknown_kind_and_invalid_target_refuse_conservatively() {
        let db=Db::open(":memory:").unwrap();let mut bad=target();bad["launcher_hashes"]=serde_json::json!({});assert!(request_runtime_target(&db,bad).is_err());
        request_runtime_target(&db,target()).unwrap();let mut record=runtime_record(&db.0.lock().unwrap()).unwrap().unwrap();record.phase="unknown".into();save_runtime(&db.0.lock().unwrap(),&record).unwrap();
        assert!(busy(&db.0.lock().unwrap()).unwrap());assert!(due(&db,db::now(),true).is_err());assert!(request(&db).is_err());assert!(request_runtime_target(&db,target()).is_err());
    }
    #[test]
    fn active_runtime_restart_reacquires_existing_all_screen_and_integration_leases() {
        let root=std::env::temp_dir().join(format!("kindred-runtime-restart-{}",db::id()));std::fs::create_dir(&root).unwrap();let path=root.join("db");
        {let db=Db::open(path.to_str().unwrap()).unwrap();request_runtime_target(&db,target()).unwrap();reserve_runtime(&db).unwrap().unwrap();}
        let mut config=crate::config::Config::default();config.database=path.to_str().unwrap().into();let app=App::open(config,"owned".into()).unwrap();
        for slot in 1..=32 {assert!(app.screen_lock(slot).try_lock().is_err());}assert!(app.integrations.try_lock().is_err());
        assert_eq!(state(&app.db).unwrap().runtime_delivery.unwrap().phase,"starting");drop(app);std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn all_active_run_states_and_admission_races_defer_without_touching_package_clocks() {
        for status in ["queued","running","awaiting_user","awaiting_approval","cancelling"] {
            let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");request_runtime_target(&app.db,target()).unwrap();let before=clocks(&app.db);
            let held=app.screen_lock(17).try_lock_owned().unwrap();assert!(leases(&app).is_err());drop(held);
            let acquired=leases(&app).unwrap();let id=app.db.queue(&bot.id,"arrived after initial check",0).unwrap();
            app.db.0.lock().unwrap().execute("UPDATE runs SET status=? WHERE id=?",params![status,id]).unwrap();
            assert!(reserve_runtime(&app.db).unwrap().is_none(),"{status}");assert_eq!(clocks(&app.db),before);drop(acquired);
        }
    }
    #[tokio::test]
    async fn runtime_only_offline_worker_never_boots_or_dispatches_guest_updates() {
        let root=std::env::temp_dir().join(format!("kindred-runtime-offline-{}",db::id()));std::fs::create_dir(&root).unwrap();let marker=root.join("calls");let manager=root.join("manager.py");
        std::fs::write(&manager,format!("import sys,json\nfrom pathlib import Path\nassert sys.argv[1]=='domstate'\nPath({:?}).write_text('domstate only')\nprint(json.dumps({{'state':'shut off'}}))\n",marker.to_str().unwrap())).unwrap();
        let mut app=crate::tests::app();let vm=&mut std::sync::Arc::get_mut(&mut app).unwrap().config.vm;vm.manager=manager.to_str().unwrap().into();vm.managed_id=db::id();
        // An existing package reboot reminder must not cause even a package-status SSH call in this runtime-only cycle.
        let ssh_marker=root.join("unexpected-ssh");let proxy=root.join("proxy.py");std::fs::write(&proxy,format!("from pathlib import Path\nPath({:?}).write_text('unexpected package probe')\nraise SystemExit(1)\n",ssh_marker.to_str().unwrap())).unwrap();
        let ssh_config=root.join("ssh_config");std::fs::write(&ssh_config,format!("Host *\n HostName 127.0.0.1\n User nobody\n UserKnownHostsFile /dev/null\n GlobalKnownHostsFile /dev/null\n IdentityFile /dev/null\n IdentitiesOnly yes\n ProxyCommand /usr/bin/python3 {}\n",proxy.to_str().unwrap())).unwrap();
        std::sync::Arc::get_mut(&mut app).unwrap().config.vm.ssh_config=ssh_config.to_str().unwrap().into();
        app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET reboot_recommended=1 WHERE id=1",[]).unwrap();
        request_runtime_target(&app.db,target()).unwrap();let before=clocks(&app.db);let task=tokio::spawn(worker(app.clone()));
        tokio::time::timeout(Duration::from_secs(5),async {while !marker.exists() {tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        task.abort();let _=task.await;assert!(!ssh_marker.exists(),"runtime-only worker entered a package status path");assert_eq!(std::fs::read_to_string(&marker).unwrap(),"domstate only");assert_eq!(clocks(&app.db),before);assert_eq!(state(&app.db).unwrap().runtime_delivery.unwrap().phase,"pending");assert!(!busy(&app.db.0.lock().unwrap()).unwrap());std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn backwards_compatible_http_request_scope_is_explicit_and_authenticated() {
        let app=crate::tests::app();let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/api/computer/maintenance",listener.local_addr().unwrap());let router=crate::web::router(app.clone());let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap()});let client=reqwest::Client::new();
        assert_eq!(client.post(&url).send().await.unwrap().status(),401);
        for body in ["","{}",r#"{"scope":"full"}"#] {assert_eq!(client.post(&url).bearer_auth(&app.token).body(body).send().await.unwrap().status(),200);assert!(state(&app.db).unwrap().requested);}
        for body in [r#"{"scope":"unknown"}"#,r#"{"scope":null}"#,r#"{"scope":"runtime","sha256":"foreign"}"#,"[]"] {assert_eq!(client.post(&url).bearer_auth(&app.token).body(body).send().await.unwrap().status(),400);}
        request_runtime_target(&app.db,target()).unwrap();let id=state(&app.db).unwrap().runtime_delivery.unwrap().job_id;
        assert_eq!(client.post(&url).bearer_auth(&app.token).send().await.unwrap().status(),400);assert_eq!(state(&app.db).unwrap().runtime_delivery.unwrap().job_id,id);server.abort();
    }
    #[tokio::test]
    async fn exact_runtime_transport_contains_only_the_mandatory_installer() {
        let app=crate::tests::app();let bytes=b"OWNED_SYNTHETIC_RUNTIME".to_vec();let hash=ring::digest::digest(&ring::digest::SHA256,&bytes).as_ref().iter().map(|b|format!("{b:02x}")).collect::<String>();let mut selected=target();selected["sha256"]=serde_json::json!(hash);request_runtime_target(&app.db,selected).unwrap();let value=reserve_runtime(&app.db).unwrap().unwrap();
        let fixture=r#"import sys,shlex,hashlib,ast,json
args=shlex.split(sys.argv[-1]);assert args[:4]==['sudo','-n','python3','-c']
source=args[4];digest,version,operation,job=args[5:];assert operation in ('install','status')
processes=[node for node in ast.walk(ast.parse(source)) if isinstance(node,ast.Call) and isinstance(node.func,ast.Attribute) and isinstance(node.func.value,ast.Name) and node.func.value.id=='subprocess']
assert len(processes)==2 and all(node.func.attr=='run' for node in processes)
assert {ast.literal_eval(node.args[0].elts[1]) for node in processes}=={'--version','guest-rpc'}
module=ast.parse(source);assignment=next(n for n in module.body if isinstance(n,ast.Assign) and any(isinstance(t,ast.Name) and t.id=='LAUNCHERS' for t in n.targets));launchers=json.loads(ast.literal_eval(assignment.value.args[0]))
assert len(launchers)==6
body=sys.stdin.buffer.read();assert (hashlib.sha256(body).hexdigest()==digest) if operation=='install' else body==b''
print(json.dumps({'job_id':job,'sha256':digest,'version':version,'launcher_hashes':{n:hashlib.sha256(v['source'].encode()).hexdigest() for n,v in launchers.items()},'phase':'completed','updated':False,'launchers_updated':False}))
"#;
        for operation in ["install","status"] {
            let (ssh,input)=runtime_delivery_command(&app,&value,operation,bytes.clone(),&hash).unwrap();assert_eq!(ssh.as_std().get_program(),"ssh");
            // Only the transport process is replaced; its exact generated argv
            // and stdin are consumed by a local synthetic peer, never real SSH.
            let mut peer=tokio::process::Command::new("python3");peer.args(["-c",fixture]);peer.args(ssh.as_std().get_args());
            let raw=vm::capture(peer,input,5,4096).await.unwrap();let observed:Value=serde_json::from_slice(&raw).unwrap();assert_eq!(observed["launcher_hashes"],managed_launcher_hashes());assert_eq!(observed["job_id"],value.job_id);
        }
    }

    #[test]
    fn legacy_rows_migrate_with_no_runtime_request_and_keep_package_history() {
        let c=Connection::open_in_memory().unwrap();
        // The original single-row schema before requested/detail/operation columns.
        c.execute_batch("CREATE TABLE vm_maintenance(id INTEGER PRIMARY KEY,enabled INTEGER NOT NULL,phase TEXT NOT NULL,job_id TEXT NOT NULL,attempted INTEGER NOT NULL,finished INTEGER NOT NULL,last_success INTEGER NOT NULL,next_check INTEGER NOT NULL,reboot_recommended INTEGER NOT NULL,error TEXT NOT NULL,idle_since INTEGER NOT NULL); INSERT INTO vm_maintenance VALUES(1,0,'failed','old',11,12,10,999,1,'old error',0);").unwrap();
        migrate(&c).unwrap();assert!(runtime_record(&c).unwrap().is_none());assert!(!busy(&c).unwrap());
        let row:(i64,i64,String)=c.query_row("SELECT attempted,next_check,error FROM vm_maintenance WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();assert_eq!(row,(11,999,"old error".into()));
    }

}
