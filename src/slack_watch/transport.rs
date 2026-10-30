use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Mutex, OnceLock};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

// Slack-owned codes only. HTTP bodies, socket URLs and transport errors can
// contain credentials and are never exposed in status, logs or tool results.
fn api_error(code: &str) -> String {
    match code {
        "invalid_auth"|"token_revoked"|"account_inactive"|"not_authed"=>"Slack rejected this token. Reconnect the account in Settings → Routines.".into(),
        "missing_scope"=>"The Slack app is missing a required permission. Update its scopes and reinstall it.".into(),
        "not_allowed_token_type"=>"Use the Slack app's app-level token for events and its user or bot OAuth token for the account.".into(),
        "channel_not_found"|"not_in_channel"=>"This Slack account cannot access that channel. Join it or select an accessible channel.".into(),
        "ratelimited"=>"Slack is rate limiting this connection. Delivery will reconnect after a short delay.".into(),
        _=>"Slack could not complete this connection check. Check the app's Socket Mode and event subscriptions.".into(),
    }
}
pub(super) async fn api(
    token: &str,
    method: &str,
    fields: &[(&str, &str)],
) -> Result<(Value, Vec<String>)> {
    api_at(token, method, fields, "https://slack.com/api").await
}
async fn api_at(
    token: &str,
    method: &str,
    fields: &[(&str, &str)],
    base: &str,
) -> Result<(Value, Vec<String>)> {
    ensure!(
        matches!(
            method,
            "auth.test" | "apps.connections.open" | "conversations.list" | "conversations.info"
        ),
        "Unsupported Slack method"
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let encoded = reqwest::Url::parse_with_params("https://slack.com/", fields)?
        .query()
        .unwrap_or("")
        .to_owned();
    let mut response = http
        .post(format!("{base}/{method}"))
        .bearer_auth(token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded)
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Slack connection unavailable. Kindred will retry without starting a bot task."
            )
        })?;
    if response.status().as_u16() == 429 {
        anyhow::bail!("{}", api_error("ratelimited"));
    }
    ensure!(
        response.status().is_success(),
        "Slack returned an unavailable API response"
    );
    let scopes = response
        .headers()
        .get("x-oauth-scopes")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("Slack response interrupted"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= 2 * 1024 * 1024,
            "Slack response exceeded its size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Slack returned an invalid response"))?;
    ensure!(
        value["ok"] == true,
        "{}",
        api_error(value["error"].as_str().unwrap_or(""))
    );
    Ok((value, scopes))
}
fn validate_socket_url(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| anyhow::anyhow!("Slack returned an invalid event connection"))?;
    ensure!(
        url.scheme() == "wss"
            && url.host_str().is_some_and(|h| h.ends_with(".slack.com"))
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Slack returned an unsupported event connection"
    );
    Ok(url.to_string())
}
pub(super) async fn socket_url(token: &str) -> Result<String> {
    let (value, _) = api(token, "apps.connections.open", &[]).await?;
    validate_socket_url(
        value["url"]
            .as_str()
            .context("Slack did not return an event connection")?,
    )
}
fn current(app: &App, a: &Account) -> bool {
    !app.account_disabled()
        && app.db.transfer_status().is_ok_and(|v| v.is_null())
        && account(&app.db, &a.id)
            .is_ok_and(|v| v.revision == a.revision && v.status != "disconnected")
}
static SOCKET_OWNERS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
struct Lease(String);
impl Lease {
    fn take(key: String) -> Result<Self> {
        ensure!(
            SOCKET_OWNERS
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .insert(key.clone()),
            "This Slack app is already listening in another Kindred workspace. Use a separate Slack app for this workspace."
        );
        Ok(Self(key))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        SOCKET_OWNERS.get().unwrap().lock().unwrap().remove(&self.0);
    }
}
pub(super) async fn run(app: Shared, a: &Account) {
    let mut delay = 2;
    while current(&app, a) {
        let result = listen(&app, a).await;
        if !current(&app, a) {
            break;
        }
        match result {
            Ok(()) => {
                delay = 2;
                let _ = health(&app.db, &a.id, &a.revision, "connecting", "");
            }
            Err(error) => {
                let _ = health(&app.db, &a.id, &a.revision, "error", &error.to_string());
                delay = (delay * 2).min(60);
            }
        }
        for _ in 0..delay {
            if !current(&app, a) {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
async fn listen(app: &App, a: &Account) -> Result<()> {
    let keys = credentials(app, &a.id)?;
    let hash = ring::digest::digest(&ring::digest::SHA256, keys.app_token.as_bytes());
    let _token = Lease::take(format!("token:{:?}", hash.as_ref()))?;
    let (identity, scopes) = api(&keys.access_token, "auth.test", &[]).await?;
    ensure!(
        identity["team_id"] == a.team_id
            && identity["user_id"] == a.user_id
            && identity["bot_id"].as_str().unwrap_or("") == a.bot_id,
        "Slack account identity changed. Reconnect it before monitoring."
    );
    app.db.0.lock().unwrap().execute("UPDATE slack_accounts SET body=json_set(body,'$.scopes',json(?)) WHERE id=? AND json_extract(body,'$.revision')=?",params![json!(scopes).to_string(),a.id,a.revision])?;
    let url = socket_url(&keys.app_token).await?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(12),
        tokio_tungstenite::connect_async_with_config(url, Some(config), false),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Slack event connection timed out"))?
    .map_err(|_| anyhow::anyhow!("Slack event connection unavailable"))?;
    receive(app, a, socket).await
}
async fn receive<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    app: &App,
    a: &Account,
    mut socket: tokio_tungstenite::WebSocketStream<S>,
) -> Result<()> {
    let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .map_err(|_| anyhow::anyhow!("Slack did not confirm the event connection"))?
        .context("Slack closed its event connection")?
        .map_err(|_| anyhow::anyhow!("Slack event connection interrupted"))?;
    let hello: Value = serde_json::from_str(
        frame
            .to_text()
            .map_err(|_| anyhow::anyhow!("Slack did not confirm the event connection"))?,
    )
    .map_err(|_| anyhow::anyhow!("Slack returned an invalid event greeting"))?;
    let app_id = hello["connection_info"]["app_id"]
        .as_str()
        .filter(|s| valid_id(s, b"A"))
        .context("Slack did not identify the event app")?;
    ensure!(
        hello["type"] == "hello",
        "Slack did not confirm the event connection"
    );
    let _application = Lease::take(format!("app:{app_id}"))?;
    let mut connected = a.clone();
    connected.app_id = app_id.into();
    app.db.0.lock().unwrap().execute("UPDATE slack_accounts SET body=json_set(body,'$.app_id',?) WHERE id=? AND json_extract(body,'$.revision')=?",params![app_id,a.id,a.revision])?;
    health(&app.db, &a.id, &a.revision, "connected", "")?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    let mut check = tokio::time::interval(Duration::from_secs(1));
    let mut received = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _=check.tick()=>{if !current(app,a){return Ok(());}ensure!(received.elapsed()<Duration::from_secs(90),"Slack stopped responding; reconnecting the event connection");},
            _=heartbeat.tick()=>{tokio::time::timeout(Duration::from_secs(5),socket.send(Message::Ping(Vec::new().into()))).await.map_err(|_|anyhow::anyhow!("Slack heartbeat timed out"))?.map_err(|_|anyhow::anyhow!("Slack heartbeat interrupted"))?;},
            frame=socket.next()=>{
                received=tokio::time::Instant::now();
                let Some(frame)=frame else{return Ok(());};
                match frame.map_err(|_|anyhow::anyhow!("Slack event connection interrupted"))? {
                    Message::Text(text)=>{
                        let envelope:Value=serde_json::from_str(&text).map_err(|_|anyhow::anyhow!("Slack returned an invalid event"))?;
                        if envelope["type"]=="disconnect" {return Ok(());}
                        if !current(app,a){return Ok(());}
                        if let Some(id)=envelope["envelope_id"].as_str().filter(|s|!s.is_empty() && s.len()<=200) {
                            if envelope["type"]=="events_api" {ingest(&app.db,&connected,&envelope["payload"])?;}
                            // No model, lookup, or external write before acknowledgement.
                            tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Text(json!({"envelope_id":id}).to_string().into()))).await.map_err(|_|anyhow::anyhow!("Slack event acknowledgement timed out"))?.map_err(|_|anyhow::anyhow!("Slack event acknowledgement interrupted"))?;
                        }
                    },
                    Message::Ping(bytes)=>{socket.send(Message::Pong(bytes)).await.map_err(|_|anyhow::anyhow!("Slack heartbeat interrupted"))?;},
                    Message::Close(_)=>return Ok(()),
                    _=>{},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn socket_ack_follows_durable_receipt_and_reconnect_does_not_duplicate() {
        use crate::slack_watch::tests::{count, event, setup};
        use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};
        let (app, a, _) = setup();
        for attempt in 0..2 {
            let (client, server) = tokio::io::duplex(65536);
            let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
            let cloned = app.clone();
            let account = a.clone();
            let task = tokio::spawn(async move { receive(&cloned, &account, socket).await });
            server
                .send(Message::Text(
                    json!({"type":"hello","connection_info":{"app_id":"A123"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            for delivery in 0..3 {
                let id = format!("envelope-{attempt}-{delivery}");
                let envelope = json!({"type":"events_api","envelope_id":id,"payload":event(&a,"900.1","C123","channel","A real event frame")});
                server
                    .send(Message::Text(envelope.to_string().into()))
                    .await
                    .unwrap();
                let ack = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        match server.next().await.unwrap().unwrap() {
                            Message::Text(t) => break serde_json::from_str::<Value>(&t).unwrap(),
                            Message::Ping(v) => server.send(Message::Pong(v)).await.unwrap(),
                            _ => {}
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(ack["envelope_id"], id);
                assert_eq!(
                    count(&app.db, "slack_receipts"),
                    1,
                    "Receipt must exist when Slack sees its ACK"
                );
                assert_eq!(
                    count(&app.db, "runs"),
                    0,
                    "Receiving messages never calls a model"
                );
            }
            server
                .send(Message::Text(
                    json!({"type":"disconnect","reason":"refresh_requested"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
    #[test]
    fn socket_destinations_and_errors_never_expose_credentials() {
        assert!(validate_socket_url("wss://wss-primary.slack.com/link/?ticket=fixture").is_ok());
        for value in [
            "ws://wss-primary.slack.com/",
            "wss://slack.com.evil.test/",
            "wss://127.0.0.1/",
            "wss://secret@wss-primary.slack.com/",
            "wss://wss-primary.slack.com:8443/",
        ] {
            assert!(validate_socket_url(value).is_err());
        }
        assert!(!api_error("xapp-secret private body").contains("secret"));
    }
    #[test]
    fn duplicate_socket_owner_is_refused_and_released() {
        let key = db::id();
        let first = Lease::take(key.clone()).unwrap();
        assert!(Lease::take(key.clone()).is_err());
        drop(first);
        assert!(Lease::take(key).is_ok());
    }
    #[tokio::test]
    async fn local_transport_checks_scope_header_and_redacts_provider_errors() {
        use axum::{Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/auth.test",
                post(|headers: axum::http::HeaderMap, body: String| async move {
                    assert_eq!(headers["authorization"], "Bearer xoxp-fixture");
                    assert!(!body.contains("xoxp"));
                    (
                        [("x-oauth-scopes", "channels:history, im:history")],
                        Json(json!({"ok":true,"user_id":"U123","team_id":"T123"})),
                    )
                }),
            )
            .route(
                "/conversations.info",
                post(|| async { Json(json!({"ok":false,"error":"private xapp-secret"})) }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (v, scopes) = api_at("xoxp-fixture", "auth.test", &[], &format!("http://{addr}"))
            .await
            .unwrap();
        assert_eq!(v["user_id"], "U123");
        assert_eq!(scopes, vec!["channels:history", "im:history"]);
        let error = api_at(
            "xoxp-fixture",
            "conversations.info",
            &[],
            &format!("http://{addr}"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(!error.contains("secret"));
        server.abort();
    }
}
