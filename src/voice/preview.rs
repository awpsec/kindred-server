//! A fixed, bounded sample of the same voice used in calls. No chat or microphone.
use super::*;
use axum::response::IntoResponse;

const SAMPLE: &str = "Hi there, I'm ready when you are.";
const MAX_CLIP_BYTES: usize = 24_000 * 2 * 10;
const CACHE_LIMIT: usize = 8;
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub(super) struct State {
    current: Mutex<Option<(String, tokio::sync::watch::Sender<bool>)>>,
    samples: Mutex<std::collections::VecDeque<CachedSample>>,
}
struct CachedSample {
    credential: Vec<u8>,
    voice: String,
    created: Instant,
    wav: Vec<u8>,
}
struct PreviewLease<'a> {
    state: &'a State,
    id: String,
    cancel: tokio::sync::watch::Receiver<bool>,
}
impl Drop for PreviewLease<'_> {
    fn drop(&mut self) {
        let mut current = self.state.current.lock().unwrap();
        if current.as_ref().is_some_and(|(id, _)| id == &self.id) {
            *current = None;
        }
    }
}
impl State {
    fn cached(&self, key: &str, voice: &str, now: Instant) -> Option<Vec<u8>> {
        let credential = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
        let mut samples = self.samples.lock().unwrap();
        samples.retain(|s| {
            s.credential == credential.as_ref()
                && now.saturating_duration_since(s.created) < CACHE_TTL
        });
        let index = samples.iter().position(|s| s.voice == voice)?;
        let sample = samples.remove(index)?;
        let bytes = sample.wav.clone();
        samples.push_back(sample);
        Some(bytes)
    }
    fn remember(&self, key: &str, voice: &str, wav: Vec<u8>, now: Instant) {
        if wav.len() > MAX_CLIP_BYTES + 44 {
            return;
        }
        let credential = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
        let mut samples = self.samples.lock().unwrap();
        samples.retain(|s| {
            s.credential == credential.as_ref()
                && s.voice != voice
                && now.saturating_duration_since(s.created) < CACHE_TTL
        });
        while samples.len() >= CACHE_LIMIT {
            samples.pop_front();
        }
        samples.push_back(CachedSample {
            credential: credential.as_ref().to_vec(),
            voice: voice.into(),
            created: now,
            wav,
        });
    }
    fn acquire(&self) -> PreviewLease<'_> {
        let mut current = self.current.lock().unwrap();
        if let Some((_, old)) = current.take() {
            let _ = old.send(true);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (send, cancel) = tokio::sync::watch::channel(false);
        *current = Some((id.clone(), send));
        PreviewLease {
            state: self,
            id,
            cancel,
        }
    }
    pub(super) fn cancel(&self) {
        if let Some((_, send)) = self.current.lock().unwrap().take() {
            let _ = send.send(true);
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    voice: String,
}
pub async fn render(
    axum::extract::State(app): axum::extract::State<Shared>,
    Json(input): Json<Preview>,
) -> Result<Response, Error> {
    require!(!app.account_disabled(), "This account is unavailable.");
    require!(
        VOICES.contains(&input.voice.as_str()),
        "Choose an available voice."
    );
    let key =
        crate::connections::decisions_key(&app).context("Connect your OpenAI API key first.")?;
    let mut lease = {
        // Serialize against call upgrade so a real call always wins this race.
        let active = app.voice.active.lock().unwrap();
        require!(active.is_none(), "End your call before previewing a voice.");
        app.voice.preview.acquire()
    };
    // Only fixed, provider-generated samples are retained, in account-local RAM.
    // Replaying a voice does not open another paid session. Credentials, account
    // access and active-call checks still run before every cache lookup.
    if let Some(wav) = app.voice.preview.cached(&key, &input.voice, Instant::now()) {
        if *lease.cancel.borrow() {
            return Err(cancelled());
        }
        return Ok(response(wav));
    }
    let mut request = ENDPOINT.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {key}").parse()?);
    let transport = tokio::select! {
        _ = lease.cancel.wait_for(|cancel| *cancel) => return Err(cancelled()),
        result = tokio::time::timeout(Duration::from_secs(10),tokio_tungstenite::connect_async(request)) => result,
    };
    let (mut upstream, _) = transport
        .ok()
        .and_then(Result::ok)
        .ok_or_else(unavailable)?;
    // Dropping this future also drops the socket and lease. No retries or background calls.
    let clip = tokio::select! {
        _ = lease.cancel.wait_for(|cancel| *cancel) => return Err(cancelled()),
        result = tokio::time::timeout(Duration::from_secs(18),sample(&mut upstream,&input.voice)) => result,
    };
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        let _ = send_upstream(&mut upstream, json!({"type":"session.close"})).await;
        let _ = upstream.close(None).await;
    })
    .await;
    let pcm = clip.ok().and_then(Result::ok).ok_or_else(unavailable)?;
    if *lease.cancel.borrow() {
        return Err(cancelled());
    }
    require!(
        !app.account_disabled() && crate::connections::decisions_key(&app).as_deref() == Some(&key),
        "This account is unavailable."
    );
    let wav = wav(&pcm);
    app.voice
        .preview
        .remember(&key, &input.voice, wav.clone(), Instant::now());
    Ok(response(wav))
}
fn response(wav: Vec<u8>) -> Response {
    (
        [
            ("Content-Type", "audio/wav"),
            ("Cache-Control", "no-store"),
            ("X-Content-Type-Options", "nosniff"),
        ],
        wav,
    )
        .into_response()
}
fn cancelled() -> Error {
    Error::with_status(StatusCode::CONFLICT, "Voice preview cancelled.")
}
fn unavailable() -> Error {
    Error::with_status(
        StatusCode::BAD_GATEWAY,
        "Voice preview is unavailable. Check your connection and OpenAI Voice settings, then try again.",
    )
}
async fn sample<S>(
    upstream: &mut tokio_tungstenite::WebSocketStream<S>,
    voice: &str,
) -> Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let instruction = format!(
        "This is an AI voice preview, not a conversation. Immediately say only this short sample in English: {SAMPLE} Then remain silent. Do not use tools or request backend work."
    );
    send_upstream(
        upstream,
        json!({"type":"session.start","session":{
            "model":"gpt-live-1","store":false,"instructions":instruction,
            "audio":{"format":{"type":"audio/pcm","rate":24000},"output":{"voice":voice}},
            "delegation":{"type":"client","acknowledgement_delay_ms":800}
        }}),
    )
    .await?;
    let mut ready = false;
    let mut clip = Clip::default();
    let mut tick = tokio::time::interval(Duration::from_millis(40));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Live needs a running audio timeline even for a requested spoken preview.
    // These synthetic zero samples never touch a user's microphone.
    let silence = json!({"type":"session.input_audio.append","audio":STANDARD.encode([0u8;1920])});
    loop {
        tokio::select! {
            incoming = upstream.next() => {
                let event = match incoming {
                    Some(Ok(UpstreamMessage::Text(text))) => serde_json::from_str::<Value>(&text)?,
                    Some(Ok(UpstreamMessage::Ping(bytes))) => {upstream.send(UpstreamMessage::Pong(bytes)).await?;continue;},
                    Some(Ok(UpstreamMessage::Close(_))) | None => anyhow::bail!("Preview ended before audio."),
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => anyhow::bail!("Preview connection ended."),
                };
                match event["type"].as_str().unwrap_or("") {
                    "session.started" => {
                        ensure!(!ready, "Duplicate preview start.");
                        ready = true;
                        send_upstream(upstream,json!({"type":"session.commentary.append","delegation_id":null,"event_id":uuid::Uuid::new_v4().to_string(),"content":format!("Say the preview now: {SAMPLE}")})).await?;
                    },
                    "session.output_audio.delta" => {
                        ensure!(ready, "Preview has not started.");
                        let bytes = audio_bytes(&event["delta"],96_000)?;
                        if clip.append(&bytes)? {return Ok(clip.pcm);}
                    },
                    "error" | "session.closed" | "session.delegation.created" => anyhow::bail!("Preview could not complete."),
                    _ => {}
                }
            },
            _ = tick.tick(), if ready => send_upstream(upstream,silence.clone()).await?,
        }
    }
}

// Provider output includes continuous silence. Bound and trim by audio energy,
// not message arrival, so the sample starts promptly and never plays a long blank.
#[derive(Default)]
struct Clip {
    pcm: Vec<u8>,
    pending: Vec<u8>,
    leading: Vec<u8>,
    voice_frames: usize,
    quiet_frames: usize,
}
impl Clip {
    fn append(&mut self, bytes: &[u8]) -> Result<bool> {
        const FRAME: usize = 960; // 20 ms, mono PCM16 at 24 kHz.
        self.pending.extend_from_slice(bytes);
        while self.pending.len() >= FRAME {
            let frame: Vec<u8> = self.pending.drain(..FRAME).collect();
            let energy: u64 = frame
                .chunks_exact(2)
                .map(|v| {
                    let n = i16::from_le_bytes([v[0], v[1]]) as i64;
                    (n * n) as u64
                })
                .sum();
            let audible = energy > 64 * 64 * (FRAME as u64 / 2);
            if audible {
                if self.voice_frames == 0 {
                    self.pcm.append(&mut self.leading);
                }
                self.voice_frames += 1;
                self.quiet_frames = 0;
            } else if self.voice_frames == 0 {
                self.leading.extend_from_slice(&frame);
                if self.leading.len() > FRAME * 5 {
                    self.leading.drain(..FRAME);
                }
                continue;
            } else {
                self.quiet_frames += 1;
            }
            self.pcm.extend_from_slice(&frame);
            ensure!(
                self.pcm.len() <= MAX_CLIP_BYTES,
                "Voice preview exceeded its duration limit."
            );
            if self.quiet_frames >= 45 && self.voice_frames >= 5 {
                // Retain 120 ms of natural decay, not the full silence detector tail.
                self.pcm
                    .truncate(self.pcm.len() - (self.quiet_frames - 6) * FRAME);
                return Ok(true);
            }
        }
        Ok(false)
    }
}
fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24000u32.to_le_bytes());
    out.extend_from_slice(&48000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_previews_expire_evict_and_never_cross_credentials() {
        let state = State::default();
        let now = Instant::now();
        for n in 0..CACHE_LIMIT {
            state.remember("key-a", &format!("voice-{n}"), vec![n as u8], now);
        }
        assert_eq!(state.cached("key-a", "voice-0", now), Some(vec![0]));
        state.remember("key-a", "new", vec![99], now);
        assert!(state.cached("key-a", "voice-1", now).is_none());
        assert_eq!(state.cached("key-a", "voice-0", now), Some(vec![0]));
        assert_eq!(state.samples.lock().unwrap().len(), CACHE_LIMIT);
        assert!(state.cached("key-a", "voice-0", now + CACHE_TTL).is_none());
        state.remember("key-a", "marin", vec![1], now);
        assert!(state.cached("key-b", "marin", now).is_none());
        assert!(state.cached("key-a", "marin", now).is_none());
        state.remember("key-a", "oversized", vec![0; MAX_CLIP_BYTES + 45], now);
        assert!(state.samples.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn cached_preview_route_reuses_audio_but_preserves_call_and_credential_guards() {
        use tower::ServiceExt;
        let root = std::env::temp_dir().join(format!("kindred-preview-cache-{}", db::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = crate::tests::app();
        std::sync::Arc::get_mut(&mut app).unwrap().config.database =
            root.join("state.db").to_string_lossy().into_owned();
        let key = "sk-proj-fixture-preview-cache-not-real";
        crate::connections::save_decisions(&app, Some(key)).unwrap();
        let bytes = wav(&[0, 1, 2, 3]);
        app.voice
            .preview
            .remember(key, "marin", bytes.clone(), Instant::now());
        for active in [false, false, true] {
            *app.voice.active.lock().unwrap() = active.then(|| "fixture-call".into());
            let request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/voice/preview")
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", app.token))
                .body(axum::body::Body::from(r#"{"voice":"marin"}"#))
                .unwrap();
            let response = tokio::time::timeout(
                Duration::from_secs(1),
                crate::web::router(app.clone()).oneshot(request),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(response.status().as_u16(), if active { 400 } else { 200 });
            if !active {
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(
                    axum::body::to_bytes(response.into_body(), 1000)
                        .await
                        .unwrap()
                        .as_ref(),
                    bytes
                );
            }
        }
        *app.voice.active.lock().unwrap() = None;
        crate::connections::save_decisions(&app, None).unwrap();
        assert!(
            render(
                axum::extract::State(app.clone()),
                Json(Preview {
                    voice: "marin".into()
                })
            )
            .await
            .is_err()
        );
        let calls: i64 = app
            .db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM voice_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn preview_trims_continuous_silence_and_preserves_pcm() {
        let mut clip = Clip::default();
        assert!(!clip.append(&vec![0; 960 * 100]).unwrap());
        let sound: Vec<u8> = (0..480 * 20)
            .flat_map(|i| if i % 2 == 0 { 1200i16 } else { -1200i16 }.to_le_bytes())
            .collect();
        for chunk in sound.chunks(382) {
            assert!(!clip.append(chunk).unwrap());
        }
        assert!(clip.append(&vec![0; 960 * 50]).unwrap());
        assert_eq!(clip.pcm.len(), 960 * (5 + 20 + 6));
        assert_eq!(&clip.pcm[960 * 5..960 * 25], sound);
        let wave = wav(&clip.pcm);
        assert_eq!(&wave[..4], b"RIFF");
        assert_eq!(&wave[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wave[24..28].try_into().unwrap()), 24000);
        assert_eq!(
            u32::from_le_bytes(wave[40..44].try_into().unwrap()) as usize,
            clip.pcm.len()
        );
        assert_eq!(&wave[44..], clip.pcm);
    }
    #[test]
    fn preview_limits_length_and_new_preview_cancels_only_its_predecessor() {
        let mut clip = Clip::default();
        let sound: Vec<u8> = (0..24_000 * 11)
            .flat_map(|_| 2000i16.to_le_bytes())
            .collect();
        assert!(clip.append(&sound).is_err());
        let state = State::default();
        let first = state.acquire();
        let second = state.acquire();
        assert!(*first.cancel.borrow());
        assert!(!*second.cancel.borrow());
        drop(first);
        assert_eq!(state.current.lock().unwrap().as_ref().unwrap().0, second.id);
        state.cancel();
        assert!(*second.cancel.borrow());
        drop(second);
        assert!(state.current.lock().unwrap().is_none());
        let last = state.acquire();
        assert!(!*last.cancel.borrow());
        drop(last);
        assert!(state.current.lock().unwrap().is_none());
    }
    #[tokio::test]
    async fn preview_authentication_and_input_validation_do_not_create_chat_work() {
        use tower::ServiceExt;
        let app = crate::tests::app();
        for (auth, body, status) in [
            (false, r#"{"voice":"marin"}"#, 401),
            (true, r#"{"voice":"invented"}"#, 400),
            (true, r#"{"voice":"marin","text":"run a task"}"#, 422),
        ] {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/voice/preview")
                .header("Content-Type", "application/json");
            if auth {
                request = request.header("Authorization", format!("Bearer {}", app.token));
            }
            let response = crate::web::router(app.clone())
                .oneshot(request.body(axum::body::Body::from(body)).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
        }
        let c = app.db.0.lock().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM voice_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn preview_stream_uses_selected_live_voice_and_synthetic_silence() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let event: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(event["session"]["audio"]["output"]["voice"], "quartz");
            assert_eq!(event["session"]["store"], false);
            assert!(event["session"]["input"].is_null());
            send_upstream(&mut ws, json!({"type":"session.started"}))
                .await
                .unwrap();
            let commentary: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(commentary["type"], "session.commentary.append");
            let silence: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(silence["type"], "session.input_audio.append");
            assert!(
                audio_bytes(&silence["audio"], 48_000)
                    .unwrap()
                    .iter()
                    .all(|v| *v == 0)
            );
            let sound: Vec<u8> = (0..480 * 8).flat_map(|_| 3000i16.to_le_bytes()).collect();
            for bytes in [vec![0; 960 * 10], sound, vec![0; 960 * 50]] {
                send_upstream(
                    &mut ws,
                    json!({"type":"session.output_audio.delta","delta":STANDARD.encode(bytes)}),
                )
                .await
                .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let clip = tokio::time::timeout(Duration::from_secs(2), sample(&mut ws, "quartz"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(clip.len(), 960 * (5 + 8 + 6));
        server.await.unwrap();
    }
}
