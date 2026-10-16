use crate::{db, runtime::Shared};
use axum::{
    Json,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{OwnedMutexGuard, broadcast},
};

pub struct VncState {
    tickets: Mutex<HashMap<String, (i64, bool, crate::screen_capacity::Identity)>>,
    pub stop: broadcast::Sender<i64>,
    sessions: AtomicUsize,
}
impl Default for VncState {
    fn default() -> Self {
        Self {
            tickets: Mutex::new(HashMap::new()),
            stop: broadcast::channel(64).0,
            sessions: AtomicUsize::new(0),
        }
    }
}
impl VncState {
    pub fn has_pending(&self,slot:i64)->bool {
        self.tickets.lock().unwrap().values().any(|(expiry,_,actual)|*expiry>db::now() && actual.slot==slot)
    }
    pub fn close_all(&self) {
        self.tickets.lock().unwrap().clear();
        let _ = self.stop.send(-1);
    }
    fn ticket_slot(&self,ticket:&str)->Option<i64> {
        self.tickets.lock().unwrap().get(ticket).filter(|(expiry,_,_)|*expiry>db::now()).map(|(_,_,identity)|identity.slot)
    }
    pub fn has_ticket(&self, ticket: &str) -> bool {
        ticket.len() == 64
            && self
                .tickets
                .lock()
                .unwrap()
                .get(ticket)
                .is_some_and(|(expiry, _, _)| *expiry > db::now())
    }
    fn issue(&self, control: bool, identity: crate::screen_capacity::Identity) -> anyhow::Result<String> {
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, (expiry, _, _)| *expiry > db::now());
        anyhow::ensure!(tickets.len() < 16, "Too many pending desktop connections");
        let ticket = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        tickets.insert(ticket.clone(), (db::now() + 30, control, identity));
        Ok(ticket)
    }
    fn consume(&self, ticket: &str) -> Option<(bool, crate::screen_capacity::Identity)> {
        self.tickets
            .lock()
            .unwrap()
            .remove(ticket)
            .filter(|(expiry, _, _)| *expiry > db::now())
            .map(|(_, control, slot)| (control, slot))
    }
    pub fn close_controls_for(&self, slot: i64) {
        let _ = self.stop.send(slot);
    }
}
// This observer is shipped by the server so an older guest executable can still
// report its lifecycle. It never allocates, starts, or restarts a screen.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScreenPhase { Asleep, Waking, Sleeping, Ready, Failed, Unavailable, Off, Paused }

fn power_phase(power: &str) -> Option<ScreenPhase> {
    match power.trim() {
        "shut off" | "stopped" | "not started" => Some(ScreenPhase::Off),
        "paused" => Some(ScreenPhase::Paused),
        _ => None,
    }
}
async fn guest_phase(config: &crate::config::Vm, slot: i64) -> anyhow::Result<ScreenPhase> {
    anyhow::ensure!((1..=32).contains(&slot), "Invalid screen");
    let source = include_str!("../deploy/screen-state.py").replace('\'', "'\"'\"'");
    let mut command = crate::vm::ssh(config);
    command.arg(format!("python3 -c '{source}' {slot} {} {}", config.vnc_port, config.vnc_view_port));
    let output = crate::vm::capture(command, None, 15, 4096).await?;
    let value: Value = serde_json::from_slice(&output)?;
    Ok(serde_json::from_value(value["state"].clone())?)
}
pub async fn screen_state(State(app): State<Shared>, Query(query): Query<crate::web::ScreenQuery>) -> Response {
    let result: anyhow::Result<ScreenPhase> = async {
        let owner = if query.bot_id.is_empty() {
            app.db.bots()?.into_iter().find(|b| !b.profile.archived)
                .ok_or_else(|| anyhow::anyhow!("Choose a bot before opening a computer"))?.id
        } else { app.db.bot(&query.bot_id)?; query.bot_id };
        let power = crate::vm::control(&app.config.vm, "domstate").await;
        if let Ok(ref value) = power {
            if let Some(phase) = power_phase(value) { return Ok(phase); }
        }
        let Some(slot) = app.db.existing_screen(&owner)? else {
            return Ok(if power.as_deref().is_ok_and(|s| s.trim()=="running") { ScreenPhase::Asleep } else { ScreenPhase::Unavailable });
        };
        let _ownership = app.screen_ownership_lock(slot).read_owned().await;
        app.db.screen_identity(&owner, slot)?;
        guest_phase(&app.config.vm, slot).await
    }.await;
    match result {
        Ok(state) => Json(json!({"state":state})).into_response(),
        Err(_) => Json(json!({"state":"unavailable"})).into_response(),
    }
}
fn screen_failure() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"screen_state":"failed","error":"The computer screen could not start. Try Wake screen again, or check Bot Computer settings."}))).into_response()
}

#[derive(Deserialize)]
pub struct DesktopRequest {
    #[serde(default)]
    control: bool,
    #[serde(default)]
    bot_id: String,
}
pub async fn ticket(State(app): State<Shared>, Json(request): Json<DesktopRequest>) -> Response {
    if let Ok(power) = crate::vm::control(&app.config.vm, "domstate").await {
        if let Some(phase) = power_phase(&power) {
            return (StatusCode::CONFLICT, Json(json!({"screen_state":phase,"error":if phase==ScreenPhase::Off {"Computer is off. Start it in Bot Computer settings."} else {"Computer is paused. Resume it before waking the screen."}}))).into_response();
        }
    }
    if crate::vm_maintenance::available(&app.db).is_err() {
        return (StatusCode::CONFLICT, Json(json!({"error":"The computer is updating. Its screen will be available when updates finish."}))).into_response();
    }
    let identity = match crate::web::screen_selection(&app, &request.bot_id).await {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":e.to_string()})),
            )
                .into_response();
        }
    };
    let slot=identity.slot;
    if request.control && !app.db.screen_takeover(slot).unwrap_or(false) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"Take control before connecting an interactive desktop."})),
        )
            .into_response();
    }
    let _ownership=app.screen_ownership_lock(slot).read_owned().await;
    if app.db.screen_identity(&identity.owner,slot).ok().as_ref()!=Some(&identity) {return StatusCode::CONFLICT.into_response()};
    // Maintenance takes these same screen locks before its durable reservation.
    // Recheck under the lock after ownership selection, for view and control alike.
    let start_lease = match screen_start_lease(&app, slot) {
        Ok(lease) => lease,
        Err(error) => return (StatusCode::CONFLICT, Json(json!({"error":error.to_string()}))).into_response(),
    };
    // Finish waking before issuing the short-lived WebSocket ticket. Startup
    // failures must reach the UI as an HTTP result, not an unexplained socket close.
    if let Err(error) = crate::vm::guest_screen_identity(&app.config.vm, slot, "screen_ensure", json!({}), Some(&identity)).await {
        eprintln!("Screen {slot} startup: {error}");
        return screen_failure();
    }
    if !matches!(guest_phase(&app.config.vm, slot).await, Ok(ScreenPhase::Ready)) {
        return screen_failure();
    }
    drop(start_lease); // Tickets and passive streaming do not retain the startup lock.
    match app.vnc.issue(request.control, identity) {
        Ok(ticket) => {
            Json(json!({"ticket":ticket,"control":request.control,"expires_in":30})).into_response()
        }
        Err(e) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}
#[derive(Deserialize)]
pub struct TicketQuery {
    ticket: String,
}
pub async fn upgrade(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if !headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|origin| app.config.allows_origin(origin))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(selected_slot)=app.vnc.ticket_slot(&query.ticket) else {return StatusCode::UNAUTHORIZED.into_response()};
    let ownership=app.screen_ownership_lock(selected_slot).read_owned().await;
    let Some((control, identity)) = app.vnc.consume(&query.ticket) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let slot=identity.slot;
    if app.db.screen_identity(&identity.owner,slot).ok().as_ref()!=Some(&identity) {return StatusCode::CONFLICT.into_response()};
    if slot!=selected_slot {return StatusCode::UNAUTHORIZED.into_response()};
    if app.vnc.sessions.fetch_add(1, Ordering::SeqCst) >= 16 {
        app.vnc.sessions.fetch_sub(1, Ordering::SeqCst);
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let receiver = app.vnc.stop.subscribe();
    let lease = if control {
        if !app.db.screen_takeover(slot).unwrap_or(false)
            || app.db.screen_quiet(slot).unwrap_or(i64::MAX) > db::now()
        {
            app.vnc.sessions.fetch_sub(1, Ordering::SeqCst);
            return StatusCode::CONFLICT.into_response();
        }
        match app.screen_lock(slot).try_lock_owned() {
            Ok(lease) => Some(lease),
            Err(_) => {
                app.vnc.sessions.fetch_sub(1, Ordering::SeqCst);
                return StatusCode::CONFLICT.into_response();
            }
        }
    } else {
        None
    };
    let guard = SessionGuard(app.clone(),ownership);
    if control && !app.db.screen_takeover(slot).unwrap_or(false) {
        return StatusCode::CONFLICT.into_response();
    }
    ws.max_message_size(1024 * 1024)
        .max_frame_size(1024 * 1024)
        .on_upgrade(move |socket| bridge(socket, app, control, identity, lease, guard, receiver))
}
struct SessionGuard(Shared, tokio::sync::OwnedRwLockReadGuard<()>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.vnc.sessions.fetch_sub(1, Ordering::SeqCst);
    }
}
fn screen_start_lease(app:&Shared,slot:i64)->anyhow::Result<OwnedMutexGuard<()>> {
    let lease=app.screen_lock(slot).try_lock_owned().map_err(|_|anyhow::anyhow!("Computer startup is busy"))?;
    crate::vm_maintenance::available(&app.db)?;
    Ok(lease)
}
async fn bridge(
    mut socket: WebSocket,
    app: Shared,
    control: bool,
    identity: crate::screen_capacity::Identity,
    _lease: Option<OwnedMutexGuard<()>>,
    _guard: SessionGuard,
    mut stop: broadcast::Receiver<i64>,
) {
    let slot=identity.slot;
    if app.db.screen_identity(&identity.owner,slot).ok().as_ref()!=Some(&identity) {return}
    let port = (if control {
        app.config.vm.vnc_port
    } else {
        app.config.vm.vnc_view_port
    }) + ((slot - 1) * 2) as u16;
    // The view-only upstream is a separate x11vnc listener with -viewonly enforced
    // by the server. Browser-side viewOnly is only an additional UI convenience.
    let mut command = crate::vm::ssh_with_forward(&app.config.vm, Some(port));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut buffer = vec![0u8; 65536];
    let mut keepalive = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            biased;
            stopped=stop.recv()=>{if stopped.is_err() || stopped.as_ref().ok()==Some(&-1) || (control && stopped.ok()==Some(slot)) {break;}},
            _=keepalive.tick()=>{if socket.send(Message::Ping(vec![].into())).await.is_err(){break;}},
            incoming=socket.recv()=>match incoming {
                Some(Ok(Message::Binary(bytes)))=>{
                    if control && !app.db.screen_takeover(slot).unwrap_or(false) {break;}
                    if !matches!(tokio::time::timeout(Duration::from_secs(10),input.write_all(&bytes)).await,Ok(Ok(()))){break;}
                },
                Some(Ok(Message::Ping(bytes)))=>{if socket.send(Message::Pong(bytes)).await.is_err(){break;}},
                Some(Ok(Message::Pong(_)))=>{},
                _=>break,
            },
            read=output.read(&mut buffer)=>match read {
                Ok(n) if n>0=>{if !matches!(tokio::time::timeout(Duration::from_secs(10),socket.send(Message::Binary(buffer[..n].to_vec().into()))).await,Ok(Ok(()))){break;}},
                _=>break,
            }
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn screen_observation_does_not_reserve_or_wake_and_power_off_is_not_sleep() {
        use std::{future::IntoFuture, sync::Arc};
        let root = std::env::temp_dir().join(format!("kindred-screen-state-{}", db::id()));
        std::fs::create_dir(&root).unwrap();
        let manager = root.join("manager.py");
        std::fs::write(&manager, "import pathlib,sys,json\np=pathlib.Path(__file__).parent\nassert sys.argv[1]=='domstate', 'Observer attempted to change power'\nprint(json.dumps({'state':(p/'power').read_text()}))\n").unwrap();
        std::fs::write(root.join("power"), "running").unwrap();
        let mut app = crate::tests::app();
        let config = &mut Arc::get_mut(&mut app).unwrap().config.vm;
        config.managed_id = db::id();config.manager = manager.to_string_lossy().into();
        let bot = crate::tests::bot(&app.db, "codex");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        let url = format!("{base}/api/computer/screen-state?bot_id={}", bot.id);
        assert_eq!(client.get(&url).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        let value: Value = client.get(&url).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
        assert_eq!(value["state"], "asleep");
        assert_eq!(app.db.existing_screen(&bot.id).unwrap(), None);
        for (power, expected) in [("shut off", "off"), ("paused", "paused"), ("unknown", "unavailable")] {
            std::fs::write(root.join("power"), power).unwrap();
            let value: Value = client.get(&url).bearer_auth(&app.token).send().await.unwrap().json().await.unwrap();
            assert_eq!(value["state"], expected);
        }
        std::fs::write(root.join("power"), "shut off").unwrap();
        let response = client.post(format!("{base}/api/computer/session")).bearer_auth(&app.token)
            .json(&json!({"bot_id":bot.id,"control":false})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.json::<Value>().await.unwrap()["screen_state"], "off");
        assert_eq!(app.db.existing_screen(&bot.id).unwrap(), None);
        assert!(app.vnc.tickets.lock().unwrap().is_empty());
        server.abort();std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn desktop_tickets_are_single_use_bounded_and_expire() {
        let state = VncState::default();
        let identity=crate::screen_capacity::Identity{slot:1,owner:db::id(),generation:db::id(),legacy:true};
        let ticket = state.issue(true, identity.clone()).unwrap();
        assert_eq!(state.consume(&ticket), Some((true, identity.clone())));
        assert_eq!(state.consume(&ticket), None);
        let expired = state.issue(false, identity.clone()).unwrap();
        state.tickets.lock().unwrap().get_mut(&expired).unwrap().0 = db::now() - 1;
        assert_eq!(state.consume(&expired), None);
        for _ in 0..16 {
            state.issue(false, identity.clone()).unwrap();
        }
        assert!(state.issue(false, identity.clone()).is_err());
    }
}

#[cfg(test)]
mod runtime_start_tests {
    use super::*;
    #[tokio::test]
    async fn passive_screen_ensure_cannot_bypass_maintenance_mutex() {
        let app=crate::tests::app();let held=app.screen_lock(5).try_lock_owned().unwrap();
        assert!(screen_start_lease(&app,5).is_err(),"view-only screen_ensure bypassed the maintenance lease");
        drop(held);let start=screen_start_lease(&app,5).unwrap();assert!(app.screen_lock(5).try_lock().is_err());
        drop(start);assert!(app.screen_lock(5).try_lock().is_ok(),"passive stream must release the startup mutex");
    }
    #[test]
    fn passive_start_admission_rechecks_durable_maintenance_state() {
        let app=crate::tests::app();app.db.0.lock().unwrap().execute("UPDATE vm_maintenance SET phase='checking' WHERE id=1",[]).unwrap();
        assert!(screen_start_lease(&app,5).is_err());assert!(app.screen_lock(5).try_lock().is_ok());
    }

}

#[cfg(test)]
mod composition_tests {
    use super::*;
    use std::{ffi::{CStr, c_char, c_int, c_void}, path::{Path, PathBuf}, sync::{Arc, atomic::AtomicBool}};

    struct Fixture { app: Shared, bot: String, root: PathBuf }
    impl Fixture {
        fn new(root: &Path, name: &str, control: bool) -> Self {
            let root=root.join(name);std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("ensure.release"), "").unwrap();
            std::fs::write(root.join("phase.release"), "").unwrap();
            let manager=root.join("manager.py");
            std::fs::write(&manager,"import sys,json\nassert sys.argv[1]=='domstate'\nprint(json.dumps({'state':'running'}))\n").unwrap();
            let config=root.join("ssh_config");std::fs::write(&config,"owned transport fixture").unwrap();
            let mut app=crate::tests::app();let vm=&mut Arc::get_mut(&mut app).unwrap().config.vm;
            vm.managed_id=db::id();vm.manager=manager.to_string_lossy().into();vm.ssh_config=config.to_string_lossy().into();vm.ssh_alias="owned-vnc-fixture".into();
            let bot=crate::tests::bot(&app.db,"codex").id;
            {
                let c=app.db.0.lock().unwrap();
                c.execute("INSERT INTO screens(bot_id,slot,takeover) VALUES(?1,17,?2)",rusqlite::params![bot,control]).unwrap();
                c.execute("INSERT INTO screen_ownership VALUES(17,?1,?2,1)",rusqlite::params![bot,db::id()]).unwrap();
            }
            Self {app,bot,root}
        }
        async fn call(&self,control:bool)->Response {
            ticket(State(self.app.clone()),Json(DesktopRequest{control,bot_id:self.bot.clone()})).await
        }
        fn permit(&self,stage:&str) {std::fs::write(self.root.join(format!("{stage}.release")),"").unwrap();}
        async fn reached(&self,stage:&str) {
            tokio::time::timeout(Duration::from_secs(5),async {
                while !self.root.join(format!("{stage}.entered")).exists() {tokio::time::sleep(Duration::from_millis(5)).await;}
            }).await.expect("owned transport did not reach expected stage");
        }
        fn no_ticket_or_transport(&self) {
            assert!(self.app.vnc.tickets.lock().unwrap().is_empty());
            assert!(!self.root.join("ensure.entered").exists());
        }
    }

    unsafe extern "C" fn observe_maintenance_read(data:*mut c_void,action:c_int,table:*const c_char,_column:*const c_char,_database:*const c_char,_trigger:*const c_char)->c_int {
        if action==rusqlite::ffi::SQLITE_READ && !table.is_null() && unsafe {CStr::from_ptr(table)}.to_bytes()==b"vm_maintenance" {
            unsafe {&*(data as *const AtomicBool)}.store(true,Ordering::SeqCst);
        }
        rusqlite::ffi::SQLITE_OK
    }

    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn ticket_startup_and_readiness_share_runtime_admission_for_both_modes() {
        // Only this isolated test child has the owned SSH transport in PATH.
        // No process-global environment mutation or actual SSH peer is involved.
        let Some(root)=std::env::var_os("KINDRED_VNC_COMPOSITION_FIXTURE") else {
            use std::os::unix::fs::PermissionsExt;
            let root=std::env::temp_dir().join(format!("kindred-vnc-composition-{}",db::id()));
            std::fs::create_dir(&root).unwrap();let bin=root.join("bin");std::fs::create_dir(&bin).unwrap();
            let ssh=bin.join("ssh");std::fs::write(&ssh,r#"#!/usr/bin/python3
import json,pathlib,sys,time
assert sys.argv[1]=='-F'
p=pathlib.Path(sys.argv[2]).parent
assert (p/'ssh_config').read_text()=='owned transport fixture'
assert sys.argv[-2]=='owned-vnc-fixture'
command=sys.argv[-1]
if command.endswith('guest-rpc'):
    request=json.load(sys.stdin)
    assert request['screen']==17 and request['tool']=='screen_ensure'
    stage='ensure'
else:
    assert command.startswith('python3 -c ') and 'Observe a managed screen' in command
    stage='phase'
(p/(stage+'.entered')).write_text('entered')
deadline=time.monotonic()+5
while not (p/(stage+'.release')).exists():
    assert time.monotonic()<deadline,'owned fixture gate timeout'
    time.sleep(.005)
failure=(p/'failure').read_text() if (p/'failure').exists() else ''
print(json.dumps(({'error':'owned ensure failure'} if failure=='ensure' else {}) if stage=='ensure' else {'state':'failed' if failure=='phase' else 'ready'}))
"#).unwrap();std::fs::set_permissions(&ssh,std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut failures=Vec::new();
            for mode in ["view","control"] {
                let owned=root.join(mode);std::fs::create_dir(&owned).unwrap();
                let output=std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact","vnc::composition_tests::ticket_startup_and_readiness_share_runtime_admission_for_both_modes","--nocapture","--test-threads=1"])
                    .env_clear().env("PATH",format!("{}:/usr/bin:/bin",bin.display())).env("HOME",&owned)
                    .env("KINDRED_VNC_COMPOSITION_FIXTURE",&owned).env("KINDRED_VNC_COMPOSITION_MODE",mode).output().unwrap();
                if !output.status.success() {failures.push(format!("{mode}: stdout={} stderr={}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)));}
            }
            std::fs::remove_dir_all(&root).unwrap();
            assert!(failures.is_empty(),"{}",failures.join("\n"));
            return;
        };
        let root=PathBuf::from(root);
        for control in [false,true] {
            let mode=if control {"control"} else {"view"};
            if std::env::var("KINDRED_VNC_COMPOSITION_MODE").unwrap()!=mode {continue;}
            // Real App slot17 and all-screen/integration lease admission.
            let f=Fixture::new(&root,&format!("{mode}-held"),control);
            let held=f.app.screen_lock(17).try_lock_owned().unwrap();
            assert_eq!(f.call(control).await.status(),StatusCode::CONFLICT);
            f.no_ticket_or_transport();drop(held);
            let all=crate::vm_maintenance::leases(&f.app).unwrap();
            assert_eq!(f.call(control).await.status(),StatusCode::CONFLICT);
            f.no_ticket_or_transport();drop(all);

            // Observe the first real maintenance SQL read, then change durable
            // state while ownership selection is blocked. No sleep-based race.
            let f=Fixture::new(&root,&format!("{mode}-reservation-race"),control);
            let ownership=f.app.screen_ownership_lock(17).write_owned().await;
            let watched=Box::new(AtomicBool::new(false));
            {
                let c=f.app.db.0.lock().unwrap();
                assert_eq!(unsafe {rusqlite::ffi::sqlite3_set_authorizer(c.handle(),Some(observe_maintenance_read),watched.as_ref() as *const AtomicBool as *mut c_void)},rusqlite::ffi::SQLITE_OK);
            }
            let app=f.app.clone();let bot=f.bot.clone();
            let task=tokio::spawn(async move {ticket(State(app),Json(DesktopRequest{control,bot_id:bot})).await});
            tokio::time::timeout(Duration::from_secs(5),async {while !watched.load(Ordering::SeqCst) {tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
            {
                let c=f.app.db.0.lock().unwrap();
                assert_eq!(unsafe {rusqlite::ffi::sqlite3_set_authorizer(c.handle(),None,std::ptr::null_mut())},rusqlite::ffi::SQLITE_OK);
                c.execute("UPDATE vm_maintenance SET runtime_delivery=?1 WHERE id=1",[json!({"job_id":db::id(),"phase":"starting","target":{"version":"owned-runtime-race"},"error":"","previous":null}).to_string()]).unwrap();
            }
            drop(watched);drop(ownership);
            assert_eq!(task.await.unwrap().status(),StatusCode::CONFLICT);
            f.no_ticket_or_transport();assert!(f.app.screen_lock(17).try_lock().is_ok());

            // Both guest startup and subsequent readiness hold the same mutex.
            let f=Fixture::new(&root,&format!("{mode}-startup"),control);
            for stage in ["ensure","phase"] {std::fs::remove_file(f.root.join(format!("{stage}.release"))).unwrap();}
            let app=f.app.clone();let bot=f.bot.clone();
            let task=tokio::spawn(async move {ticket(State(app),Json(DesktopRequest{control,bot_id:bot})).await});
            for stage in ["ensure","phase"] {
                if stage=="phase" {f.permit("ensure");}
                f.reached(stage).await;
                assert!(f.app.screen_lock(17).try_lock().is_err(),"{mode} {stage} lacks startup mutex");
                assert!(crate::vm_maintenance::leases(&f.app).is_err(),"maintenance admitted during {mode} {stage}");
                assert!(f.app.integrations.try_lock().is_ok(),"failed all-screen acquisition leaked integration lease");
                assert!(f.app.vnc.tickets.lock().unwrap().is_empty(),"ticket issued before ready");
            }
            // Block issue itself and prove the startup mutex is already released.
            let (locked_tx,locked_rx)=tokio::sync::oneshot::channel();
            let (released_tx,released_rx)=tokio::sync::oneshot::channel();let app=f.app.clone();
            // The observer thread has its own wall-clock deadline, so a blocked
            // synchronous issue cannot starve the fixture's release mechanism.
            let blocker=std::thread::spawn(move || {
                let map=app.vnc.tickets.lock().unwrap();let _=locked_tx.send(());
                let deadline=std::time::Instant::now()+Duration::from_secs(3);
                let released=loop {
                    if app.screen_lock(17).try_lock().is_ok() {break true;}
                    if std::time::Instant::now()>=deadline {break false;}
                    std::thread::sleep(Duration::from_millis(5));
                };
                drop(map);let _=released_tx.send(released);
            });
            locked_rx.await.unwrap();f.permit("phase");
            let released=released_rx.await.unwrap();blocker.join().unwrap();
            assert!(released,"startup mutex remained held during ticket issue");
            assert_eq!(task.await.unwrap().status(),StatusCode::OK);
            assert_eq!(f.app.vnc.tickets.lock().unwrap().len(),1);
            let all=crate::vm_maintenance::leases(&f.app).unwrap();drop(all);

            for failure in ["ensure","phase"] {
                let f=Fixture::new(&root,&format!("{mode}-error-{failure}"),control);
                std::fs::write(f.root.join("failure"),failure).unwrap();
                assert_eq!(f.call(control).await.status(),StatusCode::SERVICE_UNAVAILABLE);
                assert!(f.app.vnc.tickets.lock().unwrap().is_empty());
                assert!(crate::vm_maintenance::leases(&f.app).is_ok(),"error leaked startup lock");
            }
        }
    }

    #[tokio::test]
    async fn passive_session_lifetime_does_not_hold_the_startup_mutex() {
        let app=crate::tests::app();
        let ownership=app.screen_ownership_lock(17).read_owned().await;
        app.vnc.sessions.fetch_add(1,Ordering::SeqCst);
        let session=SessionGuard(app.clone(),ownership);
        let startup=screen_start_lease(&app,17).unwrap();drop(startup);
        let all=crate::vm_maintenance::leases(&app).unwrap();drop(all);
        assert_eq!(app.vnc.sessions.load(Ordering::SeqCst),1);
        drop(session);assert_eq!(app.vnc.sessions.load(Ordering::SeqCst),0);
    }
}
