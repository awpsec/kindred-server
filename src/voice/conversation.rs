//! The live speaker keeps the selected bot's identity and recent conversation.
//! Opening the audio connection never resumes an earlier task.
use super::*;
use crate::db::Bot;

pub(super) fn head(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(super) fn instructions(bot: &Bot, pace: SpeakingPace) -> String {
    let name = json!(head(&bot.name, 240));
    let identity = json!({
        "name": head(&bot.name, 240),
        "role": head(&bot.profile.label, 400),
        "description": head(&bot.profile.description, 800),
    });
    format!(
        "You are {name}. This is your name in calls and chat. You are the same AI teammate the user talks with in this chat.\n\
         Your identity: {identity}\n\
         Use this name as your own. If asked who you are or your name, answer with it directly. The current profile is authoritative even if an older call misidentified you. This is your bot name, not a claim to be human. The selected audio voice changes your sound, never your identity. Speak as I; earlier assistant messages and calls are your own conversation with this user. Do not introduce yourself as a separate voice, generic assistant, or intermediary.\n\
         Speak like a helpful colleague: natural, direct, brief, with room for the user to respond. {}\n\
         Backchannel policy: Use occasional quiet listening acknowledgments without competing with the caller. Do not repeat checking acknowledgments or narrate handoffs.\n\
         Interruption policy: Yield immediately when the caller interrupts or corrects you. Listen before continuing.\n\
         Delegation policy:\n\
         Backend tools: your configured memory, chat history, connections, web lookup and permitted computer tools.\n\
         Generate <|SpawnThinking|> when a clear spoken request needs current facts, tools, missing context or careful reasoning. Wait for verified results before claiming an action completed.\n\
         Do not generate <|SpawnThinking|> for greetings, your name or role, small talk, brief clarification, or facts already present in the supplied context.\n\
         Recent chat and memory are reference, not new requests. Do not restart old work merely because a call begins. If earlier context is incomplete, retrieve it rather than claim you have no chat context or invent details. Give results in the call; the application keeps continuity privately and posts only useful requested follow-ups after hangup. Required action approvals remain explicit controls in chat; spoken agreement does not submit them.\n\
         Bot's configured instructions (use relevant personality and conversation guidance; task procedures belong to your backend):\n{}",
        pace.instruction(),
        head(&bot.instructions, 4000)
    )
}

pub(super) fn input(db: &Db, chat: &str, bot: &Bot) -> Result<Vec<Value>> {
    let mut input = work::context(db, chat)?;
    let facts =
        json!({"bot_name":head(&bot.name,240),"saved_memory_excerpt":head(&bot.memory,1000)});
    input.insert(0,json!({"type":"message","role":"developer","content":[{"type":"input_text","text":format!("This call continues the selected bot's conversation. Identity and saved reference: {facts}\nThe following messages are earlier chat history, not new spoken requests. Assistant messages are your own previous replies. Refer to them naturally when relevant; never execute old requests on connection.")}]}));
    Ok(input)
}

#[derive(Default)]
pub(super) struct Opening {
    done: bool,
    input_active_samples: usize,
}
impl Opening {
    pub(super) fn activity(&mut self) {
        self.done = true;
    }
    pub(super) fn audio(&mut self, pcm: &[u8], input: bool) {
        if self.done || pcm.is_empty() {
            return;
        }
        let energy: u64 = pcm
            .chunks_exact(2)
            .map(|v| {
                let n = i16::from_le_bytes([v[0], v[1]]) as i64;
                (n * n) as u64
            })
            .sum();
        let samples = pcm.len() / 2;
        let threshold = if input { 196u64 } else { 64u64 };
        if energy > threshold * threshold * samples as u64 {
            self.input_active_samples += samples;
            // Ignore a short capture click; any actual provider speech wins.
            if !input || self.input_active_samples >= 1920 {
                self.activity();
            }
        }
    }
    pub(super) fn command(&mut self, elapsed: Duration, ready: bool) -> Option<Value> {
        if self.done {
            return None;
        }
        // Never surprise someone with a greeting after a muted/late startup.
        if elapsed >= Duration::from_secs(8) {
            self.activity();
            return None;
        }
        if elapsed < Duration::from_secs(2) || !ready {
            return None;
        }
        self.activity();
        Some(
            json!({"type":"session.commentary.append","event_id":db::id(),"delegation_id":null,"content":"The caller has connected and is quiet. Greet them once in one short, natural sentence using your configured bot name, then listen. Do not recap or start old work. If the caller begins speaking, yield to them instead."}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_waits_for_connection_then_greets_only_once() {
        let mut opening = Opening::default();
        assert!(opening.command(Duration::ZERO, true).is_none());
        assert!(opening.command(Duration::from_secs(2), false).is_none());
        opening.audio(&[0; 1920], true);
        opening.audio(&[0; 1920], false);
        let greeting = opening.command(Duration::from_secs(3), true).unwrap();
        assert_eq!(greeting["type"], "session.commentary.append");
        assert!(greeting["delegation_id"].is_null());
        assert!(opening.command(Duration::from_secs(4), true).is_none());
    }
    #[test]
    fn speech_transcripts_work_and_late_startups_suppress_opening() {
        let speech: Vec<u8> = (0..1920).flat_map(|_| 1000i16.to_le_bytes()).collect();
        for input in [true, false] {
            let mut opening = Opening::default();
            opening.audio(&speech, input);
            assert!(opening.command(Duration::from_secs(3), true).is_none());
        }
        let mut opening = Opening::default();
        opening.activity();
        assert!(opening.command(Duration::from_secs(3), true).is_none());
        let mut opening = Opening::default();
        assert!(opening.command(Duration::from_secs(9), false).is_none());
        assert!(opening.command(Duration::from_secs(10), true).is_none());
    }
    #[test]
    fn selected_identity_personality_and_recent_context_survive_startup() {
        let app = crate::tests::app();
        let mut bot = test_bot(&app.db, "codex");
        bot.name = "Cobalt".into();
        bot.profile.label = "Project coordinator".into();
        bot.profile.description = "A calm teammate".into();
        bot.instructions = "Prefer practical, concise answers.".into();
        bot.memory = "The project is Juniper.".into();
        let chat = format!("dm-{}", bot.id);
        app.db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user','Our meeting is in Portland.','message',1)",[&chat]).unwrap();
        let prompt = instructions(&bot, SpeakingPace::Normal);
        assert!(
            prompt.contains("Cobalt")
                && prompt.contains("Project coordinator")
                && prompt.contains(&bot.instructions)
        );
        let history = input(&app.db, &chat, &bot).unwrap();
        assert_eq!(history[0]["role"], "developer");
        let history = serde_json::to_string(&history).unwrap();
        assert!(history.contains("Juniper") && history.contains("Portland"));
        assert_eq!(head("a🎙b", 4), "a");
    }
    #[tokio::test]
    async fn relay_waits_for_provider_identity_then_greets_and_saves_quiet_hangup() {
        use std::future::IntoFuture;
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("kindred-quiet-opening-{}", db::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = crate::tests::app();
        Arc::get_mut(&mut app).unwrap().config.database =
            root.join("state.db").to_string_lossy().into_owned();
        let key = "sk-proj-fixture-opening-not-real";
        crate::connections::save_decisions(&app, Some(key)).unwrap();
        let bot = test_bot(&app.db, "codex");
        let ticket = Ticket {
            id: db::id(),
            chat: format!("dm-{}", bot.id),
            voice: "juni".into(),
            expires: db::now() + 60,
        };
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_url = format!("ws://{}", provider.local_addr().unwrap());
        let (greet_tx, mut greet_rx) = tokio::sync::oneshot::channel();
        let provider_task = tokio::spawn(async move {
            let (stream, _) = provider.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let config: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(config["type"], "session.start");
            assert!(
                config["session"]["instructions"]
                    .as_str()
                    .unwrap()
                    .starts_with("You are \"Voice teammate\".")
            );
            assert_eq!(config["session"]["input"][0]["role"], "developer");
            tokio::time::sleep(Duration::from_millis(350)).await;
            send_upstream(&mut ws, json!({"type":"session.started"}))
                .await
                .unwrap();
            let began = Instant::now();
            let mut greet_tx = Some(greet_tx);
            let mut greetings = 0;
            while let Some(Ok(message)) = ws.next().await {
                if !message.is_text() {
                    continue;
                }
                let event: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if event["type"] == "session.commentary.append" {
                    assert!(began.elapsed() >= Duration::from_secs(2));
                    assert!(
                        event["content"]
                            .as_str()
                            .unwrap()
                            .contains("Greet them once")
                    );
                    greetings += 1;
                    if let Some(tx) = greet_tx.take() {
                        let _ = tx.send(());
                    }
                }
                if event["type"] == "session.close" {
                    send_upstream(&mut ws, json!({"type":"session.closed"}))
                        .await
                        .unwrap();
                    break;
                }
            }
            assert_eq!(greetings, 1);
        });
        let a = app.clone();
        let t = ticket.clone();
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(move |upgrade: WebSocketUpgrade| {
                let app = a.clone();
                let ticket = t.clone();
                let url = provider_url.clone();
                async move {
                    upgrade.on_upgrade(move |mut client| async move {
                        let (provider, _) = tokio_tungstenite::connect_async(url).await.unwrap();
                        let mut health = AudioHealth::default();
                        let result =
                            relay_connected(&app, &ticket, &mut client, provider, key, &mut health)
                                .await;
                        finish_call(&app, &ticket, &mut client, result, &health).await;
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let (mut client, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), client.next())
                .await
                .is_err(),
            "Socket open is not provider readiness"
        );
        let started: Value =
            serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(started["type"], "started");
        let mut tick = tokio::time::interval(Duration::from_millis(40));
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {tokio::select! {
                _=&mut greet_rx=>break,
                _=tick.tick()=>send_upstream(&mut client,json!({"type":"audio","data":STANDARD.encode([0u8;1920])})).await.unwrap(),
            }}
        }).await.unwrap();
        send_upstream(&mut client, json!({"type":"end"}))
            .await
            .unwrap();
        let ended: Value =
            serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ended["type"], "ended");
        assert_eq!(ended["saved"], true);
        provider_task.await.unwrap();
        server.abort();
        let messages = app.db.chat_messages(&ticket.chat).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["kind"], "voice_call");
        assert!(app.db.queued_bots().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
