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
use serde::Deserialize;
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
    tickets: Mutex<HashMap<String, (i64, bool, i64)>>,
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
    pub fn close_all(&self) {
        self.tickets.lock().unwrap().clear();
        let _ = self.stop.send(-1);
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
    fn issue(&self, control: bool, slot: i64) -> anyhow::Result<String> {
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, (expiry, _, _)| *expiry > db::now());
        anyhow::ensure!(tickets.len() < 16, "Too many pending desktop connections");
        let ticket = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        tickets.insert(ticket.clone(), (db::now() + 30, control, slot));
        Ok(ticket)
    }
    fn consume(&self, ticket: &str) -> Option<(bool, i64)> {
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
#[derive(Deserialize)]
pub struct DesktopRequest {
    #[serde(default)]
    control: bool,
    #[serde(default)]
    bot_id: String,
}
pub async fn ticket(State(app): State<Shared>, Json(request): Json<DesktopRequest>) -> Response {
    let slot = match crate::web::screen_slot(&app, &request.bot_id) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":e.to_string()})),
            )
                .into_response();
        }
    };
    if request.control && !app.db.screen_takeover(slot).unwrap_or(false) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"Take control before connecting an interactive desktop."})),
        )
            .into_response();
    }
    match app.vnc.issue(request.control, slot) {
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
    let Some((control, slot)) = app.vnc.consume(&query.ticket) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
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
    let guard = SessionGuard(app.clone());
    if control && !app.db.screen_takeover(slot).unwrap_or(false) {
        return StatusCode::CONFLICT.into_response();
    }
    ws.max_message_size(1024 * 1024)
        .max_frame_size(1024 * 1024)
        .on_upgrade(move |socket| bridge(socket, app, control, slot, lease, guard, receiver))
}
struct SessionGuard(Shared);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.vnc.sessions.fetch_sub(1, Ordering::SeqCst);
    }
}
async fn bridge(
    mut socket: WebSocket,
    app: Shared,
    control: bool,
    slot: i64,
    _lease: Option<OwnedMutexGuard<()>>,
    _guard: SessionGuard,
    mut stop: broadcast::Receiver<i64>,
) {
    if crate::vm::guest_screen(&app.config.vm, slot, "screen_ensure", json!({}))
        .await
        .is_err()
    {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
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
    #[test]
    fn desktop_tickets_are_single_use_bounded_and_expire() {
        let state = VncState::default();
        let ticket = state.issue(true, 1).unwrap();
        assert_eq!(state.consume(&ticket), Some((true, 1)));
        assert_eq!(state.consume(&ticket), None);
        let expired = state.issue(false, 1).unwrap();
        state.tickets.lock().unwrap().get_mut(&expired).unwrap().0 = db::now() - 1;
        assert_eq!(state.consume(&expired), None);
        for _ in 0..16 {
            state.issue(false, 1).unwrap();
        }
        assert!(state.issue(false, 1).is_err());
    }
}
