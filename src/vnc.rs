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
    // Finish waking before issuing the short-lived WebSocket ticket. Startup
    // failures must reach the UI as an HTTP result, not an unexplained socket close.
    if let Err(error) = crate::vm::guest_screen_identity(&app.config.vm, slot, "screen_ensure", json!({}), Some(&identity)).await {
        eprintln!("Screen {slot} startup: {error}");
        return screen_failure();
    }
    if !matches!(guest_phase(&app.config.vm, slot).await, Ok(ScreenPhase::Ready)) {
        return screen_failure();
    }
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
