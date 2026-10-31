//! Account-scoped live calls. API keys and transcript collection stay on the server.
use crate::{
    db::{self, Db},
    runtime::Shared,
    web::Error,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{Message as UpstreamMessage, client::IntoClientRequest};
mod work;
pub(crate) mod session;
mod options;
use options::{PaceUpdate, SpeakingPace};

macro_rules! require {
    ($condition:expr,$message:literal) => {
        if !$condition {
            return Err(anyhow::anyhow!($message).into());
        }
    };
}
pub mod preview;

const ENDPOINT: &str = "wss://api.openai.com/v1/live/sessions";
const MAX_TEXT: usize = 54_000;
// OpenAI currently documents these voices but does not offer a public list API.
// Keep provenance in the response. Never claim a local catalog is API discovery.
const VOICES: &[&str] = &[
    "marin", "cedar", "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse",
    "quartz", "ripple", "vesper", "willow", "stone", "gleam", "meridian", "bossa", "tempo",
    "beacon", "delta", "cinder", "nuri", "juni", "flitz", "brise", "sillage", "nira", "harema",
    "shida", "noeul",
];
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub enabled: bool,
    pub voice: String,
    #[serde(default)]
    pub pace: SpeakingPace,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: "marin".into(),
            pace: SpeakingPace::Normal,
        }
    }
}
fn preferences(db: &Db) -> Preferences {
    db.setting("voice")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}
pub async fn settings(State(app): State<Shared>) -> Result<Json<Value>, Error> {
    let prefs = preferences(&app.db);
    Ok(Json(
        json!({"configured":crate::connections::decisions_key(&app).is_some(),"enabled":prefs.enabled,"voice":prefs.voice,"pace":prefs.pace,"voice_changes":"next_call","model":"gpt-live-1","catalog_source":"openai_documentation","catalog_url":"https://developers.openai.com/api/reference/resources/live/methods/create","voices":VOICES.iter().map(|id| json!({"id":id,"name":format!("{}{}",id[..1].to_uppercase(),&id[1..])})).collect::<Vec<_>>()}),
    ))
}
pub async fn save_settings(
    State(app): State<Shared>,
    Json(prefs): Json<Preferences>,
) -> Result<Json<Value>, Error> {
    require!(
        VOICES.contains(&prefs.voice.as_str()),
        "Choose an available voice."
    );
    require!(
        !prefs.enabled || crate::connections::decisions_key(&app).is_some(),
        "Connect your OpenAI API key first."
    );
    require!(
        app.voice.active.lock().unwrap().is_none() || prefs.enabled == preferences(&app.db).enabled,
        "End your call before turning voice calls on or off."
    );
    app.db
        .save_setting("voice", &serde_json::to_value(prefs)?)?;
    app.voice.preview.cancel();
    settings(State(app)).await
}
#[derive(Clone)]
struct Ticket {
    id: String,
    chat: String,
    voice: String,
    expires: i64,
}
#[derive(Default)]
pub struct Calls {
    tickets: Mutex<HashMap<String, Ticket>>,
    active: Mutex<Option<String>>,
    preview: preview::State,
}
impl Calls {
    pub fn has_ticket(&self, key: &str) -> bool {
        self.tickets
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|t| t.expires > db::now())
    }
}
struct Lease {
    app: Shared,
    id: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut active = self.app.voice.active.lock().unwrap();
        if active.as_ref() == Some(&self.id) {
            *active = None;
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    chat_id: String,
}
pub async fn start(
    State(app): State<Shared>,
    Json(input): Json<Start>,
) -> Result<Json<Value>, Error> {
    require!(!app.account_disabled(), "This account is unavailable.");
    let prefs = preferences(&app.db);
    require!(
        prefs.enabled && crate::connections::decisions_key(&app).is_some(),
        "Enable Voice with an OpenAI API key in Settings."
    );
    require!(
        VOICES.contains(&prefs.voice.as_str()),
        "Choose an available voice."
    );
    require!(
        app.db.transfer_status()?.is_null(),
        "Finish moving this workspace before calling."
    );
    let chat = app.db.chat(&input.chat_id)?;
    require!(
        chat.id.starts_with("dm-") && chat.members.len() == 1 && !chat.archived,
        "Open a bot conversation to call."
    );
    let bot = app.db.bot(&chat.members[0])?;
    require!(!bot.profile.archived, "This bot is archived.");
    require!(
        app.voice.active.lock().unwrap().is_none(),
        "Finish the current call first."
    );
    let mut tickets = app.voice.tickets.lock().unwrap();
    tickets.retain(|_, t| t.expires > db::now());
    require!(
        tickets.len() < 4,
        "A call is already connecting. Try again shortly."
    );
    let id = uuid::Uuid::new_v4().to_string();
    let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    tickets.insert(
        key.clone(),
        Ticket {
            id: id.clone(),
            chat: chat.id,
            voice: prefs.voice,
            expires: db::now() + 60,
        },
    );
    Ok(Json(json!({"id":id,"ticket":key,"bot":bot})))
}
#[derive(Deserialize)]
pub struct Join {
    ticket: String,
}
pub async fn upgrade(
    State(app): State<Shared>,
    Query(join): Query<Join>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Error> {
    if app.account_disabled()
        || headers
            .get("origin")
            .is_some_and(|o| !o.to_str().ok().is_some_and(|o| app.config.allows_origin(o)))
    {
        return Err(Error::with_status(
            StatusCode::FORBIDDEN,
            "Call unavailable.",
        ));
    }
    let ticket = app
        .voice
        .tickets
        .lock()
        .unwrap()
        .remove(&join.ticket)
        .filter(|t| t.expires > db::now())
        .context("Call expired. Start again.")?;
    {
        let mut active = app.voice.active.lock().unwrap();
        require!(active.is_none(), "Finish the current call first.");
        *active = Some(ticket.id.clone());
        app.voice.preview.cancel();
    }
    let lease = Lease {
        app: app.clone(),
        id: ticket.id.clone(),
    };
    Ok(ws
        .max_message_size(128 * 1024)
        .on_upgrade(move |socket| call(app, ticket, socket, lease)))
}
pub fn migrate(c: &rusqlite::Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS voice_calls(id TEXT PRIMARY KEY, chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE, started INTEGER NOT NULL, seconds INTEGER NOT NULL DEFAULT 0, transcript TEXT NOT NULL DEFAULT '[]', finished INTEGER NOT NULL DEFAULT 0, confirmed INTEGER NOT NULL DEFAULT 0, message_seq INTEGER, handoff_run TEXT); CREATE INDEX IF NOT EXISTS voice_pending ON voice_calls(id) WHERE finished=0;")?;
    // Transport counters only: never retain microphone samples or provider errors.
    c.execute_batch("CREATE TABLE IF NOT EXISTS voice_call_health(call_id TEXT PRIMARY KEY REFERENCES voice_calls(id) ON DELETE CASCADE, counters TEXT NOT NULL, error_code TEXT);")?;
    work::migrate(c)?;
    session::migrate(c)
}

#[derive(Default, Serialize)]
struct AudioHealth {
    input_frames: u64,
    input_bytes: u64,
    input_active_frames: u64,
    output_frames: u64,
    output_bytes: u64,
    input_transcript_events: u64,
    output_transcript_events: u64,
}
impl AudioHealth {
    fn save(&self, db: &Db, id: &str, error: Option<&str>) -> Result<()> {
        db.0.lock().unwrap().execute(
            "INSERT INTO voice_call_health(call_id,counters,error_code) SELECT id,?,? FROM voice_calls WHERE id=? ON CONFLICT(call_id) DO UPDATE SET counters=excluded.counters,error_code=excluded.error_code",
            params![serde_json::to_string(self)?, error, id],
        )?;
        Ok(())
    }
}
#[derive(Debug)]
struct MissingAudio;
impl std::fmt::Display for MissingAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Microphone audio stopped reaching the call.")
    }
}
impl std::error::Error for MissingAudio {}
fn audio_overdue(last: Instant, muted: bool) -> bool {
    !muted && last.elapsed() >= Duration::from_secs(8)
}
fn audio_bytes(value: &Value, limit: usize) -> Result<Vec<u8>> {
    let data = value.as_str().context("Invalid audio.")?;
    ensure!(data.len() <= limit.div_ceil(3) * 4, "Invalid audio frame.");
    let bytes = STANDARD.decode(data)?;
    ensure!(!bytes.is_empty() && bytes.len() <= limit && bytes.len() % 2 == 0, "Invalid audio frame.");
    Ok(bytes)
}
#[derive(Default, Serialize, Deserialize)]
struct Transcript {
    fragments: Vec<Fragment>,
    #[serde(skip)]
    bytes: usize,
    #[serde(skip)]
    seen: HashSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Fragment {
    speaker: String,
    text: String,
    start_ms: u64,
    end_ms: u64,
}
impl Transcript {
    fn append(&mut self, event: &Value) -> Result<()> {
        let speaker = match event["type"].as_str() {
            Some("session.input_transcript.delta") => "user",
            Some("session.output_transcript.delta") => "voice",
            _ => return Ok(()),
        };
        let text = event["delta"]
            .as_str()
            .context("Invalid voice transcript.")?;
        let start = event["start_ms"]
            .as_u64()
            .context("Invalid transcript timing.")?;
        let end = event["end_ms"]
            .as_u64()
            .context("Invalid transcript timing.")?;
        ensure!(
            start <= end && end <= 3_600_000,
            "Invalid transcript timing."
        );
        let id = event["event_id"]
            .as_str()
            .context("Missing transcript event ID.")?;
        if self.seen.contains(id) {
            return Ok(());
        }
        ensure!(
            self.bytes + text.len() + 8 * (self.fragments.len() + 1) <= MAX_TEXT
                && self.fragments.len() < 10000
                && id.len() < 256,
            "Call reached its transcript limit."
        );
        self.seen.insert(id.into());
        self.bytes += text.len();
        self.fragments.push(Fragment {
            speaker: speaker.into(),
            text: text.into(),
            start_ms: start,
            end_ms: end,
        });
        Ok(())
    }
    fn text(&self) -> String {
        let mut fragments: Vec<_> = self.fragments.iter().enumerate().collect();
        fragments.sort_by_key(|(i, f)| (f.start_ms, *i));
        let mut text = String::new();
        let mut last = "";
        for (_, f) in fragments {
            if last != f.speaker {
                text.push_str(&format!("\n{}: ", f.speaker));
                last = &f.speaker;
            }
            text.push_str(&f.text);
        }
        text
    }
}
fn checkpoint(db: &Db, id: &str, transcript: &Transcript, seconds: u64) -> Result<()> {
    db.0.lock().unwrap().execute(
        "UPDATE voice_calls SET transcript=?,seconds=? WHERE id=? AND finished=0",
        params![serde_json::to_string(transcript)?, seconds, id],
    )?;
    Ok(())
}
// Durable receipt + handoff share one transaction. Disconnect/recovery cannot double-send.
fn finish(db: &Db, id: &str, confirmed: bool) -> Result<()> {
    let (chat_id, seconds, raw, done): (String, u64, String, bool) =
        db.0.lock().unwrap().query_row(
            "SELECT chat_id,seconds,transcript,finished FROM voice_calls WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    if done {
        return Ok(());
    }
    let transcript: Transcript = serde_json::from_str(&raw)?;
    let chat = db.chat(&chat_id)?;
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    if tx.query_row("SELECT finished FROM voice_calls WHERE id=?", [id], |r| {
        r.get::<_, bool>(0)
    })? {
        return Ok(());
    }
    let label = format!("Call ended · {}:{:02}", seconds / 60, seconds % 60);
    let seq = work::receipt(&tx, id, &chat_id)?;
    tx.execute("UPDATE chat_messages SET body=? WHERE seq=?", params![label,seq])?;
    let mut run = None;
    let archived = tx
        .query_row(
            "SELECT json_extract(profile,'$.archived')=1 FROM bots WHERE id=?",
            [&chat.members[0]],
            |r| r.get::<_, Option<bool>>(0),
        )?
        .unwrap_or(false);
    let call_owned = session::finish(&tx, id, confirmed)?;
    if !call_owned && transcript
        .fragments
        .iter()
        .any(|f| f.speaker == "user" && !f.text.trim().is_empty())
        && !chat.archived
        && !archived
    {
        run = work::finish_tail(&tx, id, &chat, &transcript)?;
    }
    tx.execute(
        "UPDATE voice_calls SET finished=1,confirmed=?,message_seq=?,handoff_run=? WHERE id=?",
        params![confirmed, seq, run, id],
    )?;
    tx.commit()?;
    Ok(())
}
pub fn restore_context(db: &Db, messages: &mut [Value]) -> Result<()> {
    let c = db.0.lock().unwrap();
    for message in messages.iter_mut().filter(|m| m["kind"] == "voice_call") {
        let raw: Option<String> = c
            .query_row(
                "SELECT transcript FROM voice_calls WHERE message_seq=?",
                [message["seq"].as_i64()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(raw) = raw {
            if let Ok(t) = serde_json::from_str::<Transcript>(&raw) {
                message["text"] = json!(format!(
                    "Historical voice call transcript (user and AI voice; requests may already be completed or in progress, so verify task state before acting):{}",
                    t.text()
                ));
            }
        }
    }
    Ok(())
}
pub fn recover(db: &Db) -> Result<()> {
    // A recap has no tools or side effects. Interrupted generation may resume;
    // call actions themselves are never requeued.
    db.0.lock().unwrap().execute("UPDATE voice_sessions SET recap_state='pending' WHERE recap_state='generating'",[])?;
    recover_except(db, None)
}
fn recover_except(db: &Db, active: Option<&str>) -> Result<()> {
    let ids =
        db.0.lock()
            .unwrap()
            .prepare("SELECT id FROM voice_calls WHERE finished=0")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut failed = false;
    for id in ids {
        if Some(id.as_str()) != active && finish(db, &id, false).is_err() {
            failed = true;
        }
    }
    ensure!(!failed, "Some voice handoffs are waiting for recovery.");
    Ok(())
}
pub async fn worker(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tick.tick().await;
        if app.account_disabled() {
            continue;
        }
        {
            // Hold the lease lock through recovery so a newly starting call
            // cannot appear between reading the lease and querying the DB.
            let active = app.voice.active.lock().unwrap();
            // A busy task queue must not permanently strand a saved transcript.
            let _ = recover_except(&app.db, active.as_deref());
        }
        let _ = session::recap_pending(&app).await;
    }
}
async fn send(client: &mut WebSocket, value: Value) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5),
        client.send(Message::Text(value.to_string().into())),
    )
    .await
    .context("Call connection stalled.")??;
    Ok(())
}
async fn send_upstream<S>(
    upstream: &mut tokio_tungstenite::WebSocketStream<S>,
    value: Value,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(
        Duration::from_secs(5),
        upstream.send(UpstreamMessage::Text(value.to_string().into())),
    )
    .await
    .context("Voice connection stalled.")??;
    Ok(())
}
fn elapsed(start: Instant, closing: Option<Instant>) -> u64 {
    closing
        .unwrap_or_else(Instant::now)
        .saturating_duration_since(start)
        .as_secs()
}
async fn call(app: Shared, ticket: Ticket, mut client: WebSocket, _lease: Lease) {
    let mut health = AudioHealth::default();
    let result = relay(&app, &ticket, &mut client, &mut health).await;
    finish_call(&app, &ticket, &mut client, result, &health).await;
}
async fn finish_call(app: &Shared, ticket: &Ticket, client: &mut WebSocket, result: Result<bool>, health: &AudioHealth) {
    let error_code = result.as_ref().err().map(|e| if e.is::<MissingAudio>() { "microphone_audio_missing" } else { "voice_connection_failed" });
    let _ = health.save(&app.db, &ticket.id, error_code);
    if let Some(code) = error_code {
        let message = if code == "microphone_audio_missing" {
            "Your microphone audio stopped reaching the call. Check your microphone and try again. Your conversation so far is being saved."
        } else {
            "The voice connection ended. Check your connection and OpenAI Voice settings, then try again. Your conversation so far is being saved."
        };
        let _=send(client,json!({"type":"error","code":code,"message":message})).await;
    }
    // Failures are intentionally not logged with provider bodies, keys, or transcripts.
    let exists = app
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM voice_calls WHERE id=?)",
            [&ticket.id],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false);
    let saved = !exists || finish(&app.db, &ticket.id, result.unwrap_or(false)).is_ok();
    let _ = send(
        client,
        json!({"type":"ended","id":ticket.id,"saved":saved}),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
}
async fn relay(app: &Shared, ticket: &Ticket, client: &mut WebSocket, health: &mut AudioHealth) -> Result<bool> {
    let key = crate::connections::decisions_key(app).context("Connect OpenAI in Settings.")?;
    ensure!(preferences(&app.db).enabled, "Voice is disabled.");
    let chat = app.db.chat(&ticket.chat)?;
    let bot = app.db.bot(&chat.members[0])?;
    ensure!(!chat.archived && !bot.profile.archived, "Bot unavailable.");
    let mut request = ENDPOINT.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {key}").parse()?);
    let (upstream, _) = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .context("Voice connection timed out.")??;
    relay_connected(app, ticket, client, upstream, &key, health).await
}
// The same relay is exercised against a local provider fixture; no production
// endpoint override or test-only authentication bypass exists.
async fn relay_connected<S>(
    app: &Shared,
    ticket: &Ticket,
    client: &mut WebSocket,
    mut upstream: tokio_tungstenite::WebSocketStream<S>,
    key: &str,
    health: &mut AudioHealth,
) -> Result<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let chat = app.db.chat(&ticket.chat)?;
    let bot = app.db.bot(&chat.members[0])?;
    let input = work::context(&app.db, &chat.id)?;
    let run = session::begin(&app.db, ticket)?;
    let delegation = session::configuration(app,&bot,&run)?;
    let initial_pace = preferences(&app.db).pace;
    let mut pace_update = PaceUpdate::new(initial_pace);
    let instructions = format!("You are the AI voice of {}, a teammate in Kindred. Speak naturally and briefly, listen to interruptions, and keep the conversation flowing while tools work. Do not claim to be human. Bot role: {}. You have a recent chat excerpt; the call's private backend has the bot's context and tools. Generate <|SpawnThinking|> for a clear request needing current facts, tools, memory beyond your excerpt, or substantive reasoning. Clarify ambiguity aloud before acting. Do not delegate greetings or small talk. Never invent results. Give verified findings in the call; a concise recap with useful links is saved to chat after hangup. Do not say results were posted to chat during the call. Required action approvals remain explicit controls in chat; spoken agreement does not bypass them. Do not narrate every handoff or fill pauses with repeated checking acknowledgments.", bot.name,bot.profile.label);
    let instructions = format!("{instructions} {}",initial_pace.instruction());
    send_upstream(&mut upstream,json!({"type":"session.start","session":{"model":"gpt-live-1","store":false,"instructions":instructions,"input":input,"audio":{"format":{"type":"audio/pcm","rate":24000},"output":{"voice":ticket.voice}},"delegation":delegation}})).await?;
    let mut pending = std::collections::VecDeque::<session::ToolCall>::new();
    let mut running = futures_util::stream::FuturesUnordered::new();
    let mut responses = HashMap::<String,session::ResponseWork>::new();
    let mut delegation_responses = HashMap::<String,String>::new();
    let mut action_notice = false;
    let mut started = None;
    let mut closing = None;
    let mut transcript = Transcript::default();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    let mut last_client = Instant::now();
    let mut connected = Instant::now();
    let mut confirmed = false;
    let mut last_audio = Instant::now();
    let mut muted = false;
    loop {
        if closing.is_none() && running.is_empty() {
            if let Some(tool)=pending.pop_front() {
                running.push(session::execute(app.clone(),bot.clone(),run.clone(),ticket.id.clone(),tool));
            }
        }
        if closing.is_none() && session::ready_to_resume(&responses) {
            // One continuation after every known pending call has a result,
            // including overlapping delegations from spoken interruptions.
            send_upstream(&mut upstream,json!({"type":"response.create","event_id":db::id()})).await?;
            for state in responses.values_mut().filter(|s|s.ready()) {state.continued=true;}
        }
        tokio::select! {
            result=running.next(), if !running.is_empty()=>{
                if let Some((tool,result))=result {
                    if responses.get(&tool.response).is_some_and(|s|!s.continued) {
                        send_upstream(&mut upstream,json!({"type":"response.item.create","event_id":db::id(),"item":{"type":"function_call_output","call_id":tool.id,"output":session::tool_output(&result)}})).await?;
                    }
                    if let Some(state)=responses.get_mut(&tool.response) {state.pending=state.pending.saturating_sub(1);}
                }
            },
            incoming=upstream.next()=>{
                let event=match incoming {Some(Ok(UpstreamMessage::Text(text)))=>serde_json::from_str::<Value>(&text)?,Some(Ok(UpstreamMessage::Ping(bytes)))=>{upstream.send(UpstreamMessage::Pong(bytes)).await?;continue;},Some(Ok(_))=>continue,_=>break};
                if let Some(result)=pace_update.handle(&event) {send(client,result).await?;continue;}
                if PaceUpdate::is_options_error(&event) {continue;}
                match event["type"].as_str().unwrap_or("") {
                    "session.started"=>{ensure!(started.is_none(),"Duplicate start.");started=Some(Instant::now());connected=Instant::now();last_audio=Instant::now();
                        {let c=app.db.0.lock().unwrap();ensure!(!crate::workspace_transfer::frozen(&c)?,"This workspace is moving.");c.execute("UPDATE voice_calls SET started=? WHERE id=?",params![db::now(),ticket.id])?;}
                        send(client,json!({"type":"started","id":ticket.id,"started_ms":db::now()*1000})).await?;},
                    "session.input_transcript.delta"|"session.output_transcript.delta"=>{transcript.append(&event)?;if event["type"]=="session.input_transcript.delta" {health.input_transcript_events+=1;}else{health.output_transcript_events+=1;}if let Some(start)=started {checkpoint(&app.db,&ticket.id,&transcript,elapsed(start,closing))?;}},
                    "session.output_audio.delta"=>{if closing.is_none(){let bytes=audio_bytes(&event["delta"],96000)?;health.output_frames+=1;health.output_bytes+=bytes.len() as u64;send(client,json!({"type":"audio","data":event["delta"]})).await?;}},
                    "session.delegation.created"=>{
                        ensure!(started.is_some(),"Call has not started.");
                        ensure!(event["delegation"]["target"]=="responses","Unexpected voice work mode.");
                        let id=event["delegation"]["id"].as_str().context("Missing voice delegation ID.")?;
                        let response=event["delegation"]["response_id"].as_str().context("Missing delegated response ID.")?;
                        ensure!(id.len()<=256 && response.len()<=256 && delegation_responses.len()<256,"This call reached its request limit.");
                        delegation_responses.insert(id.into(),response.into());
                        // A session-wide silent update cancels pending automatic
                        // acknowledgments; it does not suppress useful speech.
                        send_upstream(&mut upstream,json!({"type":"session.thinking.append","event_id":db::id(),"delegation_id":null,"content":""})).await?;
                    },
                    "response.event"=>{
                        ensure!(started.is_some(),"Call has not started.");
                        let e=&event["event"];
                        match e["type"].as_str().unwrap_or("") {
                            "response.created"=>{
                                let delegation=event["delegation_id"].as_str().context("Missing voice delegation ID.")?;
                                let response=e["response"]["id"].as_str().context("Missing voice response ID.")?;
                                ensure!(response.len()<=256 && responses.len()<256,"This call reached its request limit.");
                                ensure!(delegation_responses.contains_key(delegation),"Unknown voice delegation.");
                                delegation_responses.insert(delegation.into(),response.into());
                                responses.entry(response.into()).or_default();
                            },
                            "response.output_item.done"=>{
                                // Responses item events have no response_id. Associate
                                // them through the envelope and response.created.
                                let delegation=event["delegation_id"].as_str().context("Missing voice delegation ID.")?;
                                let response=delegation_responses.get(delegation).context("Unknown voice delegation.")?.as_str();
                                ensure!(responses.len()<256 || responses.contains_key(response),"This call reached its request limit.");
                                let item=&e["item"];
                                session::record_item(&app.db,&ticket.id,response,item)?;
                                if item["type"]=="function_call" && closing.is_none() {
                                    if let Some(tool)=session::accept_tool(&app.db,&ticket.id,response,item)? {
                                        let state=responses.entry(response.into()).or_default();state.pending+=1;state.has_tools=true;
                                        pending.push_back(tool);
                                    }
                                }
                            },
                            "response.completed"=>{
                                let id=e["response"]["id"].as_str().context("Missing completed voice response ID.")?;
                                if let Some(state)=responses.get_mut(id) {state.completed=true;}
                            },
                            "response.failed"|"response.incomplete"=>{
                                app.db.event(&run.id,"voice_backend_failed",json!({}))?;
                                if let Some(id)=e["response"]["id"].as_str() {
                                    if let Some(state)=responses.get_mut(id) {state.continued=true;}
                                    pending.retain(|tool|tool.response!=id);
                                }
                                if closing.is_none() {
                                    send_upstream(&mut upstream,json!({"type":"session.commentary.append","event_id":db::id(),"delegation_id":null,"content":"The latest backend request did not complete. Do not claim success or repeat any external action automatically. Explain the limitation briefly if relevant to the caller's current question."})).await?;
                                }
                            },
                            _=>{}
                        }
                    },
                    "session.closed"=>{confirmed=true;break;},
                    "error"=>anyhow::bail!("Voice provider reported an error."),
                    _=>{}
                }
            },
            incoming=client.recv(), if closing.is_none()=>{
                let event=match incoming {Some(Ok(Message::Text(text)))=>serde_json::from_str::<Value>(&text)?,Some(Ok(Message::Pong(_)))=>{last_client=Instant::now();continue;},Some(Ok(Message::Ping(bytes)))=>{client.send(Message::Pong(bytes)).await?;last_client=Instant::now();continue;},_=>json!({"type":"end"})};
                last_client=Instant::now();
                let command=match event["type"].as_str().unwrap_or("") {
                    "audio"=>{ensure!(started.is_some(),"Call has not started.");let bytes=audio_bytes(&event["data"],48000)?;last_audio=Instant::now();health.input_frames+=1;health.input_bytes+=bytes.len() as u64;if bytes.chunks_exact(2).any(|s|i16::from_le_bytes([s[0],s[1]]).unsigned_abs()>128){health.input_active_frames+=1;}json!({"type":"session.input_audio.append","audio":event["data"]})},
                    "mute"=>{muted=event["muted"]==true;last_audio=Instant::now();json!({"type":if muted{"session.input_audio.mute"}else{"session.input_audio.unmute"},"event_id":uuid::Uuid::new_v4().to_string()})},
                    "end"=>{closing=Some(Instant::now());json!({"type":"session.close"})},
                    "ping"=>{send(client,json!({"type":"pong"})).await?;continue;},
                    _=>anyhow::bail!("Unsupported voice command.")
                };
                send_upstream(&mut upstream,command).await?;
            },
            _=heartbeat.tick()=>{
                if let Some(start)=started {checkpoint(&app.db,&ticket.id,&transcript,elapsed(start,closing))?;health.save(&app.db,&ticket.id,None)?;}
                if started.is_some() && closing.is_none() && audio_overdue(last_audio,muted) {
                    let _ = send_upstream(&mut upstream,json!({"type":"session.close"})).await;
                    return Err(MissingAudio.into());
                }
                if started.is_some() && closing.is_none() {
                    if let Some(result)=pace_update.timeout() {send(client,result).await?;}
                    if let Some(command)=pace_update.command(preferences(&app.db).pace) {send_upstream(&mut upstream,command).await?;}
                    let waiting=app.db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE run_id=?1 AND status='pending') OR EXISTS(SELECT 1 FROM user_tasks WHERE run_id=?1 AND status IN ('pending','ready')) OR EXISTS(SELECT 1 FROM questions WHERE run_id=?1 AND status='pending')",[&run.id],|r|r.get::<_,bool>(0))?;
                    if waiting && !action_notice {
                        send_upstream(&mut upstream,json!({"type":"session.commentary.append","event_id":db::id(),"delegation_id":null,"content":"The current action needs your explicit review or participation using the pending card in chat. It has not completed. Spoken agreement does not submit that card."})).await?;
                    }
                    action_notice=waiting;
                }
                if closing.is_some_and(|t|t.elapsed()>Duration::from_secs(15)){break;}
                if started.is_none()&&connected.elapsed()>Duration::from_secs(20){anyhow::bail!("Voice start timed out.");}
                if closing.is_none()&&(last_client.elapsed()>Duration::from_secs(30)||app.account_disabled()||app.db.bot(&bot.id).map_or(true,|b|b.profile.archived)||crate::connections::decisions_key(app).as_deref()!=Some(key)||started.is_some_and(|s|s.elapsed()>Duration::from_secs(3600))) {
                    closing=Some(Instant::now());send_upstream(&mut upstream,json!({"type":"session.close"})).await?;
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), upstream.close(None)).await;
    if let Some(start) = started {
        checkpoint(&app.db, &ticket.id, &transcript, elapsed(start, closing))?;
    }
    Ok(confirmed)
}

#[cfg(test)]
fn test_bot(db: &Db, provider: &str) -> crate::db::Bot {
    let bot = crate::db::Bot {
        id: db::id(),
        name: "Voice teammate".into(),
        instructions: "Use the tools.".into(),
        provider: provider.into(),
        model: "test/model".into(),
        reasoning_effort: String::new(),
        memory: String::new(),
        auto_approve: false,
        approval_mode: "inherit".into(),
        profile: Default::default(),
    };
    // Match the production creation path, which atomically creates the bot's DM.
    // Saving preferences alone deliberately does not create a conversation.
    db.create_bot(&bot).unwrap();
    bot
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;
    fn fragment(id: &str, speaker: &str, text: &str, start: u64) -> Value {
        json!({"type":format!("session.{speaker}_transcript.delta"),"event_id":id,"delta":text,"start_ms":start,"end_ms":start+10})
    }
    fn record(db: &Db, chat: &str, t: &Transcript) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        db.0.lock()
            .unwrap()
            .execute(
                "INSERT INTO voice_calls(id,chat_id,started,seconds,transcript) VALUES(?,?,?,83,?)",
                params![id, chat, db::now(), serde_json::to_string(t).unwrap()],
            )
            .unwrap();
        id
    }
    #[test]
    fn missing_audio_watchdog_distinguishes_silence_mute_and_stalled_transport() {
        let old = Instant::now() - Duration::from_secs(9);
        assert!(audio_overdue(old, false));
        assert!(!audio_overdue(old, true));
        assert!(!audio_overdue(Instant::now(), false));
        assert_eq!(audio_bytes(&json!("AAAAAA=="),48000).unwrap(),vec![0;4]);
        for data in [json!(""),json!("AA=="),json!("not base64"),Value::Null] {
            assert!(audio_bytes(&data,48000).is_err());
        }
        assert!(audio_bytes(&json!(STANDARD.encode(vec![0;48002])),48000).is_err());
    }
    #[test]
    fn audio_diagnostics_keep_only_transport_counts_and_a_fixed_error_code() {
        let app=crate::tests::app();let bot=test_bot(&app.db,"codex");
        let id=record(&app.db,&format!("dm-{}",bot.id),&Transcript::default());
        let mut health=AudioHealth::default();health.input_frames=25;health.input_bytes=48000;
        health.save(&app.db,&id,None).unwrap();
        health.output_frames=2;health.save(&app.db,&id,Some("microphone_audio_missing")).unwrap();
        let c=app.db.0.lock().unwrap();
        let (raw,code):(String,String)=c.query_row("SELECT counters,error_code FROM voice_call_health WHERE call_id=?",[id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        let counters:Value=serde_json::from_str(&raw).unwrap();
        assert!(counters.as_object().unwrap().values().all(|v|v.is_u64()));
        assert_eq!(counters["input_frames"],25);assert_eq!(counters["output_frames"],2);
        assert_eq!(code,"microphone_audio_missing");
    }
    #[test]
    fn live_work_uses_bot_provider_and_closing_never_repeats_dispatched_work() {
        for provider in ["openrouter", "codex", "claude-code"] {
            let app = crate::tests::app();
            let bot = test_bot(&app.db, provider);
            let chat = format!("dm-{}",bot.id);
            let mut t = Transcript::default();
            t.append(&fragment("a","input","Find a local bicycle repair shop with a website and phone number.",1)).unwrap();
            let id = record(&app.db,&chat,&t);
            let first = work::dispatch(&app.db,&id,"delegate-a",&t).unwrap().unwrap();
            assert_eq!(work::dispatch(&app.db,&id,"delegate-a",&t).unwrap(),Some(first.clone()));
            assert_eq!(work::dispatch(&app.db,&id,"delegate-b",&t).unwrap(),Some(first.clone()));
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            assert_eq!(run.id,first);
            assert_eq!(app.db.bot(&run.bot_id).unwrap().provider,provider);
            assert!(!app.db.bot(&run.bot_id).unwrap().auto_approve);
            assert!(run.prompt.contains("normal model, chat context, memory, tools and permissions"));
            finish(&app.db,&id,true).unwrap();
            recover(&app.db).unwrap();
            assert_eq!(app.db.run(&first).unwrap().status,"running");
            let replies=app.db.chat_messages(&chat).unwrap();
            assert_eq!(replies.iter().filter(|m|m["kind"]=="voice_call").count(),1);
            assert_eq!(replies[0]["text"],"Call ended · 1:23");
            assert_eq!(app.db.0.lock().unwrap().query_row("SELECT count(*) FROM runs",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        }
    }
    #[test]
    fn corrections_steer_running_work_and_results_reach_voice_once_and_stay_in_chat() {
        let app=crate::tests::app();let bot=test_bot(&app.db,"openrouter");let chat=format!("dm-{}",bot.id);
        let mut t=Transcript::default();t.append(&fragment("a","input","Find a bike shop nearby.",1)).unwrap();
        let id=record(&app.db,&chat,&t);let first=work::dispatch(&app.db,&id,"a",&t).unwrap().unwrap();
        let run=app.db.claim_bot(&bot.id).unwrap().unwrap();assert_eq!(run.id,first);
        t.append(&fragment("b","input","Actually, only places open on Sunday.",100)).unwrap();
        checkpoint(&app.db,&id,&t,30).unwrap();let correction=work::dispatch(&app.db,&id,"b",&t).unwrap().unwrap();assert_ne!(correction,first);
        let offer=app.db.offer_followups(&run).unwrap().unwrap();assert_eq!(offer["messages"][0]["source_run_id"],correction);
        let offer_id=offer["id"].as_str().unwrap();app.db.dispatch_followup(&run,offer_id).unwrap();app.db.followup_transported(&run,offer_id).unwrap();app.db.complete_followup_turn(&run,offer_id).unwrap();
        let result="Cycle Example — Sunday 10–4. Phone: 555-0100. [Website](https://example.com/bikes).";
        app.db.finish(&run.id,"completed",result,"").unwrap();app.db.deliver_pending_completions().unwrap();
        let mut updates=work::Updates::default();let events=updates.poll(&app.db,&id).unwrap();
        assert_eq!(events.iter().filter(|e|e["type"]=="session.commentary.append").count(),1);
        assert!(events.iter().any(|e|e["content"].as_str().unwrap().contains("https://example.com/bikes")));
        assert!(updates.poll(&app.db,&id).unwrap().is_empty());
        finish(&app.db,&id,true).unwrap();
        assert!(app.db.chat_messages(&chat).unwrap().iter().any(|m|m["text"]==result));
        assert_eq!(app.db.0.lock().unwrap().query_row("SELECT count(*) FROM runs",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
    #[test]
    fn pending_choices_are_announced_without_answering_or_approving_them() {
        let app=crate::tests::app();let bot=test_bot(&app.db,"codex");let chat=format!("dm-{}",bot.id);
        let mut t=Transcript::default();t.append(&fragment("a","input","Help me choose a route.",1)).unwrap();let id=record(&app.db,&chat,&t);
        work::dispatch(&app.db,&id,"choice",&t).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        app.db.ask_question(&run,crate::questions::QuestionInput{topic_key:"voice-route".into(),question:"Which route?".into(),context:"The riverside route adds ten minutes.".into(),options:vec!["Riverside".into(),"Direct".into()]}).unwrap();
        let mut updates=work::Updates::default();let events=updates.poll(&app.db,&id).unwrap();
        assert!(events.iter().any(|e|e["content"].as_str().unwrap().contains("review the pending card")));
        assert_eq!(app.db.pending_questions().unwrap().len(),1);
        assert!(updates.poll(&app.db,&id).unwrap().is_empty());
    }
    #[test]
    fn late_transcript_tail_is_handed_off_once_after_restart() {
        let app=crate::tests::app();let bot=test_bot(&app.db,"openrouter");let chat=format!("dm-{}",bot.id);
        let mut t=Transcript::default();let id=record(&app.db,&chat,&t);
        assert!(work::dispatch(&app.db,&id,"early",&t).unwrap().is_none());
        t.append(&fragment("a","input","Find a bicycle shop.",1)).unwrap();checkpoint(&app.db,&id,&t,15).unwrap();work::dispatch(&app.db,&id,"early",&t).unwrap();
        t.append(&fragment("b","input","And check their Sunday hours.",100)).unwrap();checkpoint(&app.db,&id,&t,18).unwrap();
        recover(&app.db).unwrap();recover(&app.db).unwrap();
        let c=app.db.0.lock().unwrap();let (count,prompt):(i64,String)=c.query_row("SELECT count(*),prompt FROM runs WHERE id=(SELECT handoff_run FROM voice_calls WHERE id=?)",[id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(count,1);assert!(prompt.split("NEW spoken input since the last handoff:").nth(1).unwrap().contains("Sunday hours"));
        assert!(!prompt.split("NEW spoken input since the last handoff:").nth(1).unwrap().contains("Find a bicycle shop"));
        assert_eq!(c.query_row("SELECT count(*) FROM runs",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
    #[test]
    fn transcript_preserves_spaces_repeated_words_and_duplex_timing() {
        let mut t = Transcript::default();
        t.append(&fragment("a", "input", "I I", 100)).unwrap();
        t.append(&fragment("b", "output", "Okay", 300)).unwrap();
        t.append(&fragment("c", "input", " want that", 200))
            .unwrap();
        t.append(&fragment("a", "input", "I I", 100)).unwrap();
        assert_eq!(t.text(), "\nuser: I I want that\nvoice: Okay");
        assert_eq!(t.fragments.len(), 3);
        assert!(
            t.append(&fragment("d", "input", &"x".repeat(MAX_TEXT), 400))
                .is_err()
        );
        assert_eq!(t.fragments.len(), 3);
    }
    #[test]
    fn receipt_handoff_and_future_context_are_atomic_and_provider_independent() {
        for provider in ["openrouter", "codex", "claude-code"] {
            let app = crate::tests::app();
            let b = test_bot(&app.db, provider);
            let chat = format!("dm-{}", b.id);
            let mut t = Transcript::default();
            t.append(&fragment("a", "input", "Please review tomorrow's plan.", 1))
                .unwrap();
            let id = record(&app.db, &chat, &t);
            finish(&app.db, &id, true).unwrap();
            finish(&app.db, &id, true).unwrap();
            let mut messages = app.db.chat_messages(&chat).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["kind"], "voice_call");
            assert_eq!(messages[0]["text"], "Call ended · 1:23");
            let c = app.db.0.lock().unwrap();
            let (count, prompt): (i64, String) = c
                .query_row("SELECT count(*),prompt FROM runs", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            drop(c);
            assert_eq!(count, 1);
            assert!(prompt.contains("Please review tomorrow's plan."));
            restore_context(&app.db, &mut messages).unwrap();
            assert!(
                messages[0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("Please review tomorrow's plan.")
            );
        }
    }
    #[test]
    fn restart_recovers_once_and_empty_calls_do_not_create_tasks() {
        let app = crate::tests::app();
        let b = test_bot(&app.db, "openrouter");
        let chat = format!("dm-{}", b.id);
        let empty = record(&app.db, &chat, &Transcript::default());
        recover(&app.db).unwrap();
        recover(&app.db).unwrap();
        let c = app.db.0.lock().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row(
                "SELECT confirmed FROM voice_calls WHERE id=?",
                [empty],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(c);
        assert_eq!(app.db.chat_messages(&chat).unwrap().len(), 1);
    }
    #[test]
    fn tickets_expire_and_are_account_scoped() {
        let a = Calls::default();
        let b = Calls::default();
        a.tickets.lock().unwrap().insert(
            "test".into(),
            Ticket {
                id: "call".into(),
                chat: "dm-test".into(),
                voice: "marin".into(),
                expires: db::now() + 60,
            },
        );
        assert!(a.has_ticket("test"));
        assert!(!b.has_ticket("test"));
        a.tickets.lock().unwrap().get_mut("test").unwrap().expires = db::now();
        assert!(!a.has_ticket("test"));
    }
    #[tokio::test]
    async fn api_requires_auth_and_catalog_does_not_claim_provider_discovery() {
        let app = crate::tests::app();
        let router = crate::web::router(app.clone());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/voice/settings")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/voice/settings")
                    .header("Authorization", format!("Bearer {}", app.token))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let v: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 10000)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["catalog_source"], "openai_documentation");
        assert_eq!(v["enabled"], false);
        assert_eq!(v["voice"], "marin");
        assert_eq!(v["pace"], "normal");
        assert_eq!(v["voice_changes"], "next_call");
        assert_eq!(v["voices"].as_array().unwrap().len(), 31);
        assert!(
            v["voices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == "cedar")
        );
        assert!(
            v["voices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == "quartz")
        );
        assert!(
            save_settings(
                State(app.clone()),
                Json(Preferences {
                    enabled: false,
                    voice: "invented".into(),
                    pace: SpeakingPace::Normal,
                })
            )
            .await
            .is_err()
        );
        let denied = router
            .oneshot(
                Request::builder()
                    .uri("/api/voice/settings")
                    .header("Authorization", format!("Bearer {}", app.token))
                    .header("Origin", "https://untrusted.invalid")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), 403);
    }
    #[tokio::test]
    async fn voice_options_preserve_legacy_preferences_and_active_calls() {
        let app=crate::tests::app();
        app.db.save_setting("voice",&json!({"enabled":false,"voice":"willow"})).unwrap();
        assert_eq!(preferences(&app.db).pace,SpeakingPace::Normal);
        assert!(serde_json::from_value::<Preferences>(json!({"enabled":false,"voice":"marin","pace":"very-fast"})).is_err());
        *app.voice.active.lock().unwrap()=Some("existing-call".into());
        let result=save_settings(State(app.clone()),Json(Preferences{enabled:false,voice:"cedar".into(),pace:SpeakingPace::Faster})).await.ok().expect("active call options save");
        assert_eq!(result.0["voice"],"cedar");assert_eq!(result.0["pace"],"faster");
        assert_eq!(app.voice.active.lock().unwrap().as_deref(),Some("existing-call"));
        app.db.save_setting("voice",&json!({"enabled":true,"voice":"cedar","pace":"faster"})).unwrap();
        assert!(save_settings(State(app.clone()),Json(Preferences{enabled:false,voice:"cedar".into(),pace:SpeakingPace::Normal})).await.is_err());
        assert!(preferences(&app.db).enabled);
    }
    #[test]
    fn closing_freezes_elapsed_time_and_archived_bots_keep_the_receipt() {
        let start = Instant::now() - Duration::from_secs(90);
        assert_eq!(elapsed(start, Some(start + Duration::from_secs(83))), 83);
        let app = crate::tests::app();
        let mut bot = test_bot(&app.db, "openrouter");
        let chat = format!("dm-{}", bot.id);
        let mut transcript = Transcript::default();
        transcript
            .append(&fragment("a", "input", "Please check it.", 1))
            .unwrap();
        let id = record(&app.db, &chat, &transcript);
        bot.profile.archived = true;
        app.db.save_bot(&bot).unwrap();
        finish(&app.db, &id, true).unwrap();
        assert_eq!(app.db.chat_messages(&chat).unwrap().len(), 1);
        assert_eq!(
            app.db
                .0
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn recovery_skips_live_calls_and_retries_a_full_task_queue_without_duplicates() {
        let app = crate::tests::app();
        let bot = test_bot(&app.db, "openrouter");
        let chat = format!("dm-{}", bot.id);
        let mut t = Transcript::default();
        t.append(&fragment("a", "input", "Review the plan.", 1))
            .unwrap();
        let id = record(&app.db, &chat, &t);
        recover_except(&app.db, Some(&id)).unwrap();
        assert!(app.db.chat_messages(&chat).unwrap().is_empty());
        {
            let c = app.db.0.lock().unwrap();
            for _ in 0..100 {
                c.execute("INSERT INTO runs(id,bot_id,prompt,status,created) VALUES(?,?,'test','queued',?)",params![db::id(),bot.id,db::now()]).unwrap();
            }
        }
        assert!(finish(&app.db, &id, true).is_err());
        assert!(app.db.chat_messages(&chat).unwrap().is_empty());
        app.db
            .0
            .lock()
            .unwrap()
            .execute("UPDATE runs SET status='completed'", [])
            .unwrap();
        recover(&app.db).unwrap();
        recover(&app.db).unwrap();
        assert_eq!(app.db.chat_messages(&chat).unwrap().len(), 1);
        assert_eq!(
            app.db
                .0
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM runs WHERE status='queued'", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn real_websocket_relay_preserves_audio_mute_and_trailing_transcripts() {
        use std::future::IntoFuture;
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("kindred-voice-test-{}", db::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = crate::tests::app();
        Arc::get_mut(&mut app).unwrap().config.database =
            root.join("state.db").to_string_lossy().into_owned();
        let key = "sk-proj-voice-fixture-not-a-real-credential";
        crate::connections::save_decisions(&app, Some(key)).unwrap();
        let bot = test_bot(&app.db, "openrouter");
        let ticket = Ticket {
            id: db::id(),
            chat: format!("dm-{}", bot.id),
            voice: "willow".into(),
            expires: db::now() + 60,
        };
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_url = format!("ws://{}", provider.local_addr().unwrap());
        let provider_task = tokio::spawn(async move {
            let (stream, _) = provider.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let first = ws.next().await.unwrap().unwrap();
            let config: Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
            assert_eq!(config["type"], "session.start");
            assert_eq!(config["session"]["audio"]["output"]["voice"], "willow");
            assert_eq!(config["session"]["store"], false);
            assert_eq!(config["session"]["delegation"]["type"], "responses");
            send_upstream(&mut ws, json!({"type":"session.started"}))
                .await
                .unwrap();
            send_upstream(&mut ws, fragment("a", "input", "Please review", 0))
                .await
                .unwrap();
            send_upstream(
                &mut ws,
                json!({"type":"session.output_audio.delta","delta":"AAABAA=="}),
            )
            .await
            .unwrap();
            send_upstream(&mut ws,json!({"type":"session.delegation.created","delegation":{"id":"delegation-one","type":"delegation","target":"responses","response_id":"response-one"}})).await.unwrap();
            send_upstream(&mut ws,json!({"type":"response.event","delegation_id":"delegation-one","event":{"type":"response.created","response":{"id":"response-one","output":[]}}})).await.unwrap();
            // Real Responses item events do not contain a response_id. Repeated
            // delivery of the same call must execute once and return one result.
            let call=json!({"type":"response.event","delegation_id":"delegation-one","event":{"type":"response.output_item.done","item":{"id":"item-one","type":"function_call","call_id":"tool-one","name":"remember","arguments":json!({"mode":"append","text":"Prefer the north entrance."}).to_string()}}});
            send_upstream(&mut ws,call.clone()).await.unwrap();send_upstream(&mut ws,call).await.unwrap();
            send_upstream(&mut ws,json!({"type":"response.event","delegation_id":"delegation-one","event":{"type":"response.completed","response":{"id":"response-one","output":[]}}})).await.unwrap();
            let mut commands = Vec::new();
            while let Some(Ok(UpstreamMessage::Text(text))) = ws.next().await {
                let value: Value = serde_json::from_str(&text).unwrap();
                commands.push(value.clone());
                if value["type"]=="response.item.create" {
                    assert_eq!(value["item"]["call_id"],"tool-one");
                    let returned:Value=serde_json::from_str(value["item"]["output"][0]["text"].as_str().unwrap()).unwrap();
                    assert_ne!(returned["failed"],true);
                }
                if value["type"]=="response.create" {
                    send_upstream(&mut ws,json!({"type":"response.event","delegation_id":"delegation-one","event":{"type":"response.created","response":{"id":"response-two","output":[]}}})).await.unwrap();
                    send_upstream(&mut ws,json!({"type":"response.event","delegation_id":"delegation-one","event":{"type":"response.output_item.done","item":{"type":"message","id":"private-result","content":[{"type":"output_text","text":"The north entrance is saved. https://example.com/route","annotations":[]}]}}})).await.unwrap();
                    send_upstream(&mut ws,json!({"type":"response.event","delegation_id":"delegation-one","event":{"type":"response.completed","response":{"id":"response-two","output":[]}}})).await.unwrap();
                    send_upstream(&mut ws,json!({"type":"session.output_audio.delta","delta":"AgADAA=="})).await.unwrap();
                }
                if value["type"]=="session.instructions.append" {
                    assert!(value["content"].as_str().unwrap().contains("a little faster"));
                    send_upstream(&mut ws,json!({"type":"session.instructions.appended","client_event_id":value["event_id"]})).await.unwrap();
                }
                if value["type"] == "session.close" {
                    send_upstream(&mut ws, fragment("b", "input", " the plan.", 100))
                        .await
                        .unwrap();
                    send_upstream(&mut ws, fragment("b", "input", " the plan.", 100))
                        .await
                        .unwrap();
                    send_upstream(&mut ws, fragment("c", "output", "I'll pass it along.", 200))
                        .await
                        .unwrap();
                    send_upstream(&mut ws, json!({"type":"session.closed"}))
                        .await
                        .unwrap();
                    break;
                }
            }
            commands
        });
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let result_tx = Arc::new(Mutex::new(Some(result_tx)));
        let a = app.clone();
        let t = ticket.clone();
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(move |upgrade: WebSocketUpgrade| {
                let app = a.clone();
                let ticket = t.clone();
                let url = provider_url.clone();
                let tx = result_tx.clone();
                async move {
                    upgrade.on_upgrade(move |mut client| async move {
                        let (provider, _) = tokio_tungstenite::connect_async(url).await.unwrap();
                        let mut health=AudioHealth::default();
                        let result =
                            relay_connected(&app, &ticket, &mut client, provider, key, &mut health).await;
                        let confirmed = result.unwrap();
                        assert_eq!(health.input_frames,1);
                        assert_eq!(health.input_bytes,4);
                        assert_eq!(health.output_frames,2);
                        health.save(&app.db,&ticket.id,None).unwrap();
                        finish(&app.db, &ticket.id, confirmed).unwrap();
                        let _ = send(&mut client, json!({"type":"ended"})).await;
                        tx.lock().unwrap().take().unwrap().send(confirmed).unwrap();
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let (mut client, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let started: Value =
            serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(started["type"], "started");
        let audio: Value =
            serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(audio["data"], "AAABAA==");
        for command in [
            json!({"type":"audio","data":"AAABAA=="}),
            json!({"type":"mute","muted":true}),
            json!({"type":"mute","muted":false}),
        ] {
            send_upstream(&mut client, command).await.unwrap();
        }
        let reply=tokio::time::timeout(Duration::from_secs(10),client.next()).await.unwrap().unwrap().unwrap();
        let reply:Value=serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert_eq!(reply["data"],"AgADAA==");
        assert!(app.db.chat_messages(&ticket.chat).unwrap().is_empty());
        assert_eq!(app.db.bot(&bot.id).unwrap().memory.matches("north entrance").count(),1);
        assert!(save_settings(State(app.clone()),Json(Preferences{enabled:true,voice:"cedar".into(),pace:SpeakingPace::Faster})).await.is_ok());
        let options=tokio::time::timeout(Duration::from_secs(5),client.next()).await.unwrap().unwrap().unwrap();
        let options:Value=serde_json::from_str(options.to_text().unwrap()).unwrap();
        assert_eq!(options["type"],"options_applied");assert_eq!(options["pace"],"faster");
        send_upstream(&mut client,json!({"type":"end"})).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), result_rx)
                .await
                .unwrap()
                .unwrap()
        );
        let commands = provider_task.await.unwrap();
        assert_eq!(commands.iter().filter(|v|v["type"]=="session.instructions.append").count(),1);
        assert!(!commands.iter().any(|v|v["type"]=="session.start"||v["type"]=="session.update"),"voice selection never reconnects or sends unsupported audio updates");
        assert_eq!(commands.iter().filter(|v|v["type"]=="response.item.create").count(),1);
        assert_eq!(commands.iter().filter(|v|v["type"]=="response.create").count(),1);
        assert_eq!(
            commands
                .iter()
                .filter(|v|v["type"].as_str().unwrap().starts_with("session.input_audio.") || v["type"]=="session.close")
                .map(|v| v["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "session.input_audio.append",
                "session.input_audio.mute",
                "session.input_audio.unmute",
                "session.close"
            ]
        );
        let messages = app.db.chat_messages(&ticket.chat).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(!messages[0]["text"].as_str().unwrap().contains("plan"));
        let raw:String=app.db.0.lock().unwrap().query_row("SELECT transcript FROM voice_calls WHERE id=?",[&ticket.id],|r|r.get(0)).unwrap();
        let saved:Transcript=serde_json::from_str(&raw).unwrap();
        assert!(saved.text().contains("Please review the plan."));
        assert_eq!(saved.text().matches("the plan.").count(),1);
        assert!(app.db.queued_bots().unwrap().is_empty());
        assert!(app.db.runs(None).unwrap().is_empty());
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_live_socket_without_microphone_frames_ends_with_a_saved_receipt_and_releases_the_call() {
        use std::{future::IntoFuture,sync::Arc};
        let root=std::env::temp_dir().join(format!("kindred-voice-stall-{}",db::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app=crate::tests::app();
        Arc::get_mut(&mut app).unwrap().config.database=root.join("state.db").to_string_lossy().into_owned();
        let key="sk-proj-voice-fixture-not-a-real-credential";
        crate::connections::save_decisions(&app,Some(key)).unwrap();
        let bot=test_bot(&app.db,"openrouter");
        let ticket=Ticket{id:db::id(),chat:format!("dm-{}",bot.id),voice:"marin".into(),expires:db::now()+60};
        let provider=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_url=format!("ws://{}",provider.local_addr().unwrap());
        let provider_task=tokio::spawn(async move {
            let (stream,_)=provider.accept().await.unwrap();
            let mut ws=tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.next().await.unwrap().unwrap();
            send_upstream(&mut ws,json!({"type":"session.started"})).await.unwrap();
            while let Some(Ok(UpstreamMessage::Text(text)))=ws.next().await {
                let event:Value=serde_json::from_str(&text).unwrap();
                if event["type"]=="session.close" {return;}
            }
            panic!("Stalled call did not close the provider session");
        });
        let (done_tx,done_rx)=tokio::sync::oneshot::channel();
        let done_tx=Arc::new(Mutex::new(Some(done_tx)));
        let a=app.clone();let t=ticket.clone();
        let router=axum::Router::new().route("/",axum::routing::get(move |upgrade:WebSocketUpgrade| {
            let app=a.clone();let ticket=t.clone();let url=provider_url.clone();let tx=done_tx.clone();
            async move {upgrade.on_upgrade(move |mut client| async move {
                *app.voice.active.lock().unwrap()=Some(ticket.id.clone());
                let lease=Lease{app:app.clone(),id:ticket.id.clone()};
                let (provider,_)=tokio_tungstenite::connect_async(url).await.unwrap();
                let mut health=AudioHealth::default();
                let result=relay_connected(&app,&ticket,&mut client,provider,key,&mut health).await;
                assert!(result.as_ref().unwrap_err().is::<MissingAudio>());
                finish_call(&app,&ticket,&mut client,result,&health).await;
                drop(lease);tx.lock().unwrap().take().unwrap().send(()).unwrap();
            })}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("ws://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(axum::serve(listener,router).into_future());
        let (mut client,_)=tokio_tungstenite::connect_async(url).await.unwrap();
        let started:Value=serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(started["type"],"started");
        // A live connection and healthy silent input are not speech; both must
        // remain valid. Then keep the socket alive while capture stops entirely.
        send_upstream(&mut client,json!({"type":"mute","muted":true})).await.unwrap();
        send_upstream(&mut client,json!({"type":"audio","data":"AAAAAA=="})).await.unwrap();
        send_upstream(&mut client,json!({"type":"mute","muted":false})).await.unwrap();
        let mut ping=tokio::time::interval(Duration::from_secs(1));
        let mut error_seen=false;let mut ended=false;
        tokio::time::timeout(Duration::from_secs(12),async {
            while !ended {
                tokio::select! {
                    _=ping.tick()=>send_upstream(&mut client,json!({"type":"ping"})).await.unwrap(),
                    event=client.next()=>{
                        let event=event.unwrap().unwrap();
                        if !event.is_text(){continue;}
                        let value:Value=serde_json::from_str(event.to_text().unwrap()).unwrap();
                        if value["type"]=="error" {assert_eq!(value["code"],"microphone_audio_missing");error_seen=true;}
                        if value["type"]=="ended" {assert_eq!(value["saved"],true);ended=true;}
                    }
                }
            }
        }).await.unwrap();
        assert!(error_seen);done_rx.await.unwrap();provider_task.await.unwrap();
        assert!(app.voice.active.lock().unwrap().is_none());
        assert_eq!(app.db.chat_messages(&ticket.chat).unwrap().len(),1);
        let c=app.db.0.lock().unwrap();
        assert_eq!(c.query_row("SELECT count(*) FROM runs WHERE status='queued'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(c.query_row("SELECT count(*) FROM voice_sessions",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(c.query_row("SELECT error_code FROM voice_call_health WHERE call_id=?",[ticket.id],|r|r.get::<_,String>(0)).unwrap(),"microphone_audio_missing");
        drop(c);server.abort();std::fs::remove_dir_all(root).unwrap();
    }
}
