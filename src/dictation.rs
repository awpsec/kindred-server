//! Explicit, account-scoped cloud dictation. Audio is never persisted here.
use crate::{runtime::Shared, web::Error};
use anyhow::{Context, Result, ensure};
use axum::{Json, extract::State, http::StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};

const MODELS: &str = "https://openrouter.ai/api/v1/models?output_modalities=transcription";
const TRANSCRIBE: &str = "https://openrouter.ai/api/v1/audio/transcriptions";
const AUDIO_BYTES: usize = 44 + 16000 * 2 * 60;
pub const REQUEST_BYTES: usize = 3 * 1024 * 1024;

fn client(seconds: u64) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(seconds))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

async fn read_json(mut response: reqwest::Response, limit: usize) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("OpenRouter response was interrupted. Try dictating again.")?
    {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "OpenRouter returned too much data."
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("OpenRouter returned an unreadable response. Try again.")
}

fn transcription_models(catalog: &Value) -> Vec<Value> {
    let mut models: Vec<Value> = catalog["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| {
            m["architecture"]["input_modalities"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "audio"))
                && m["architecture"]["output_modalities"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "transcription"))
        })
        .filter_map(|m| {
            let id = m["id"].as_str().filter(|id| valid_model_id(id))?;
            let name = m["name"]
                .as_str()
                .filter(|name| name.len() <= 256)
                .unwrap_or(id);
            Some(json!({"id":id,"name":name}))
        })
        .collect();
    models.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    models.dedup_by(|a, b| a["id"] == b["id"]);
    models
}

fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._:-".contains(&b))
}

async fn catalog_at(url: &str) -> Result<Vec<Value>> {
    let response = client(20)?
        .get(url)
        .send()
        .await
        .context("OpenRouter's dictation models could not be loaded. Try again.")?;
    ensure!(
        response.status().is_success(),
        "OpenRouter's dictation models are unavailable. Try again."
    );
    let catalog = read_json(response, 8 * 1024 * 1024).await?;
    ensure!(
        catalog["data"].is_array(),
        "OpenRouter returned an unreadable model catalog."
    );
    Ok(transcription_models(&catalog))
}

pub async fn models(State(app): State<Shared>) -> Result<Json<Value>, Error> {
    if crate::connections::openrouter_key(&app).is_none() {
        return Ok(Json(json!({"configured":false,"data":[]})));
    }
    Ok(Json(
        json!({"configured":true,"data":catalog_at(MODELS).await?}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    model: String,
    audio: String,
}

// Accept only the bounded PCM WAV format produced by our recorder, not arbitrary
// media/URLs or caller-controlled provider parameters. Validate before billing.
fn validate(recording: &Recording) -> Result<()> {
    ensure!(
        valid_model_id(&recording.model),
        "Choose an OpenRouter dictation model."
    );
    ensure!(
        recording.audio.len() <= AUDIO_BYTES.div_ceil(3) * 4,
        "Dictation recordings can be at most one minute."
    );
    let bytes = STANDARD
        .decode(&recording.audio)
        .context("Invalid dictation audio.")?;
    ensure!(
        (6444..=AUDIO_BYTES).contains(&bytes.len()),
        "Dictation recordings must be between 0.2 seconds and one minute."
    );
    let u16_at = |i| u16::from_le_bytes(bytes[i..i + 2].try_into().unwrap());
    let u32_at = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
    ensure!(
        &bytes[0..4] == b"RIFF"
            && &bytes[8..12] == b"WAVE"
            && &bytes[12..16] == b"fmt "
            && u32_at(4) == bytes.len() - 8
            && u32_at(16) == 16
            && u16_at(20) == 1
            && u16_at(22) == 1
            && u32_at(24) == 16000
            && u32_at(28) == 32000
            && u16_at(32) == 2
            && u16_at(34) == 16
            && &bytes[36..40] == b"data"
            && u32_at(40) == bytes.len() - 44
            && (bytes.len() - 44) % 2 == 0,
        "Invalid dictation audio. Record again using Kindred's microphone."
    );
    Ok(())
}

async fn transcribe_at(
    recording: &Recording,
    key: &str,
    zdr: bool,
    catalog: &str,
    endpoint: &str,
) -> Result<Value> {
    validate(recording)?;
    ensure!(
        catalog_at(catalog)
            .await?
            .iter()
            .any(|m| m["id"] == recording.model),
        "This dictation model is no longer available. Choose another in Settings."
    );
    let response = client(65)?.post(endpoint).bearer_auth(key)
        .header("X-OpenRouter-Cache", "false")
        .json(&json!({"model":recording.model,"input_audio":{"data":recording.audio,"format":"wav"},
            "response_format":"json","provider":{"data_collection":"deny","zdr":zdr}}))
        .send().await.context("OpenRouter transcription did not complete. Your existing draft was kept. Try dictating again.")?;
    // Never echo the provider response: it can contain transcripts or credentials.
    let message = match response.status().as_u16() {
        200..=299 => None,
        401 | 403 => Some("OpenRouter rejected the API key or model access. Check Connections."),
        402 => Some("OpenRouter needs more credits before it can transcribe. Your draft was kept."),
        429 => Some("OpenRouter is busy or rate limited. Try dictating again in a moment."),
        _ => Some(
            "OpenRouter could not transcribe this recording with your data policy. Try another dictation model.",
        ),
    };
    if let Some(message) = message {
        anyhow::bail!(message);
    }
    let value = read_json(response, 128 * 1024).await?;
    let text = value["text"]
        .as_str()
        .context("OpenRouter returned no transcript. Try again.")?;
    ensure!(
        text.len() <= 16384,
        "The transcript exceeded the supported length."
    );
    Ok(json!({"text":text.trim()}))
}

pub async fn transcribe(
    State(app): State<Shared>,
    Json(recording): Json<Recording>,
) -> Result<Json<Value>, Error> {
    let key = crate::connections::openrouter_key(&app)
        .context("Connect an OpenRouter API key in Connections to use cloud dictation.")?;
    static REQUESTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    let _permit = REQUESTS
        .get_or_init(|| tokio::sync::Semaphore::new(4))
        .try_acquire()
        .map_err(|_| {
            Error::with_status(
                StatusCode::TOO_MANY_REQUESTS,
                "Dictation is busy. Try again in a moment.",
            )
        })?;
    Ok(Json(
        transcribe_at(
            &recording,
            &key,
            app.config.openrouter.require_zdr,
            MODELS,
            TRANSCRIBE,
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        http::HeaderMap,
        routing::{get, post},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn recording() -> Recording {
        let mut bytes = vec![0; 6444];
        for (at, value) in [(0, b"RIFF"), (8, b"WAVE"), (12, b"fmt "), (36, b"data")] {
            bytes[at..at + 4].copy_from_slice(value);
        }
        for (at, value) in [(4, 6436u32), (16, 16), (24, 16000), (28, 32000), (40, 6400)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (at, value) in [(20, 1u16), (22, 1), (32, 2), (34, 16)] {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        Recording {
            model: "test/transcribe".into(),
            audio: STANDARD.encode(bytes),
        }
    }
    fn model(id: &str, output: &str) -> Value {
        json!({"id":id,"name":id,"architecture":{"input_modalities":["audio"],"output_modalities":[output]}})
    }

    #[test]
    fn only_transcription_models_are_offered() {
        let data = transcription_models(
            &json!({"data":[model("test/transcribe","transcription"),model("chat/audio","text"),model("invalid id","transcription"),{"id":"no-capabilities"}]}),
        );
        assert_eq!(
            data,
            vec![json!({"id":"test/transcribe","name":"test/transcribe"})]
        );
    }
    #[test]
    fn rejects_malformed_oversized_and_non_pcm_audio() {
        let mut r = recording();
        validate(&r).unwrap();
        r.audio = "not audio".into();
        assert!(validate(&r).is_err());
        r.audio = "A".repeat(AUDIO_BYTES.div_ceil(3) * 4 + 1);
        assert!(validate(&r).is_err());
        let mut bytes = STANDARD.decode(recording().audio).unwrap();
        bytes[24] = 0;
        r.audio = STANDARD.encode(bytes);
        assert!(validate(&r).is_err());
        r = recording();
        r.model = "https://evil.test?a=b".into();
        assert!(validate(&r).is_err());
    }
    #[tokio::test]
    async fn sends_scoped_key_and_privacy_policy_without_retries() {
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let router = Router::new().route("/models", get(|| async { Json(json!({"data":[model("test/transcribe","transcription")]})) }))
            .route("/transcribe", post(move |headers: HeaderMap, Json(body): Json<Value>| { let requests = requests.clone(); async move {
                requests.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["authorization"], "Bearer scoped-key");
                assert_eq!(headers["x-openrouter-cache"], "false");
                assert_eq!(body["provider"], json!({"data_collection":"deny","zdr":true}));
                assert_eq!(body["input_audio"]["format"], "wav");
                Json(json!({"text":"  Words after a pause.  ","private_metadata":"not returned"}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let result = transcribe_at(
            &recording(),
            "scoped-key",
            true,
            &format!("{base}/models"),
            &format!("{base}/transcribe"),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"text":"Words after a pause."}));
        let mut bad = recording();
        bad.model = "chat/audio".into();
        assert!(
            transcribe_at(
                &bad,
                "scoped-key",
                true,
                &format!("{base}/models"),
                &format!("{base}/transcribe")
            )
            .await
            .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn provider_errors_do_not_echo_private_responses() {
        let router = Router::new()
            .route(
                "/models",
                get(|| async { Json(json!({"data":[model("test/transcribe","transcription")]})) }),
            )
            .route(
                "/transcribe",
                post(|| async { (StatusCode::PAYMENT_REQUIRED, "secret transcript and key") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let error = transcribe_at(
            &recording(),
            "scoped-key",
            false,
            &format!("{base}/models"),
            &format!("{base}/transcribe"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("credits"));
        assert!(!error.contains("secret"));
        task.abort();
    }
}
