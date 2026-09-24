//! Bounded OpenAI-compatible catalogue discovery; metadata never contains API keys.
use crate::{
    db,
    provider_accounts::{self, CustomModel, CustomProvider},
    runtime::{App, Shared},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct CatalogInfo {
    pub updated_at: Option<i64>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    #[serde(default)]
    pub checked_revision: String,
}
pub struct CatalogState {
    gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    permits: tokio::sync::Semaphore,
    started: AtomicBool,
}
impl Default for CatalogState {
    fn default() -> Self {
        Self {
            gates: Default::default(),
            permits: tokio::sync::Semaphore::new(4),
            started: AtomicBool::new(false),
        }
    }
}
impl CatalogState {
    fn gate(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    pub fn busy(&self, id: &str) -> bool {
        self.gates
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|gate| gate.try_lock().is_err())
    }
}
fn provider(app: &App, id: &str) -> Result<CustomProvider> {
    provider_accounts::custom(app)?
        .into_iter()
        .find(|p| p.id == id)
        .context("Provider not found")
}
pub fn response(p: &CustomProvider) -> Value {
    json!({"data":p.models.iter().map(|m|json!({"model":m.model,"displayName":if m.display_name.is_empty(){&m.model}else{&m.display_name},"reasoning":m.reasoning,"vision":m.vision,"context_window":m.context_window})).collect::<Vec<_>>(),"models":p.models,"catalog":p.catalog,"revision":p.revision})
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn contains(value: &Value, name: &str) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == name))
}
// Verified vision model whose local /models catalogue may contain only its ID.
// Do not infer vision from broad family names: text-only variants exist.
fn model_vision(entry: &Value, id: &str) -> bool {
    let flags = [&entry["vision"], &entry["supports_vision"], &entry["capabilities"]["vision"]];
    if flags.iter().any(|flag| flag.as_bool() == Some(false)) {
        return false;
    }
    if flags.iter().any(|flag| flag.as_bool() == Some(true)) {
        return true;
    }
    for modalities in [&entry["architecture"]["input_modalities"], &entry["input_modalities"]] {
        if let Some(values) = modalities.as_array() {
            return values.iter().any(|value| value == "image");
        }
    }
    id.eq_ignore_ascii_case("incoai/Qwen3.6-35B-A3B-Splash")
        || id.eq_ignore_ascii_case("Qwen3.6-35B-A3B-Splash")
}

pub fn parse(value: &Value) -> Result<Vec<CustomModel>> {
    let entries = value
        .get("data")
        .or_else(|| value.get("models"))
        .unwrap_or(value)
        .as_array()
        .context("The /models response is not a model catalogue")?;
    ensure!(
        entries.len() <= 4096,
        "Provider catalogue exceeds the model limit"
    );
    ensure!(
        value["has_more"] != true && value["next_cursor"].as_str().is_none_or(str::is_empty),
        "Provider returned a paginated catalogue; a complete /models list is required"
    );
    let mut ids = std::collections::HashSet::new();
    let mut models = Vec::new();
    for entry in entries {
        let id = entry["id"]
            .as_str()
            .or_else(|| entry["model"].as_str())
            .or_else(|| entry.as_str());
        let Some(id) = id.filter(|id| {
            !id.trim().is_empty() && id.len() <= 200 && !id.chars().any(char::is_control)
        }) else {
            continue;
        };
        if !ids.insert(id.to_owned()) {
            continue;
        }
        // Only explicit capability exclusions are used; opaque IDs are not guessed.
        if matches!(
            entry["type"].as_str(),
            Some("embedding" | "embeddings" | "audio" | "image")
        ) || entry["supports_tool_calls"] == false
            || entry["capabilities"]["tools"] == false
        {
            continue;
        }
        let output = &entry["architecture"]["output_modalities"];
        if output.is_array() && !contains(output, "text") {
            continue;
        }
        let context = [
            &entry["context_window"],
            &entry["context_length"],
            &entry["max_model_len"],
            &entry["max_context_length"],
            &entry["top_provider"]["context_length"],
        ]
        .into_iter()
        .find_map(|v| number(v).filter(|n| *n >= 1024.0 && *n <= 10_000_000.0 && n.fract() == 0.0))
        .map(|n| n as u64)
        .unwrap_or(32768);
        let price = |field: &str| {
            number(&entry["pricing"][field])
                .map(|n| n * 1_000_000.0)
                .filter(|n| n.is_finite() && *n <= 10000.0)
        };
        let display_name = entry["name"]
            .as_str()
            .or_else(|| entry["display_name"].as_str())
            .filter(|name| name.len() <= 200 && !name.chars().any(char::is_control))
            .unwrap_or(id)
            .to_owned();
        models.push(CustomModel {
            model: id.into(),
            display_name,
            context_window: context,
            max_output_tokens: [&entry["max_output_tokens"], &entry["max_completion_tokens"], &entry["top_provider"]["max_completion_tokens"]]
                .into_iter().find_map(|v| number(v).filter(|n| *n >= 1.0 && *n <= context as f64 && n.fract() == 0.0)).map(|n| n as u64),
            reasoning: entry["reasoning"] == true
                || entry["supports_reasoning"] == true
                || entry["capabilities"]["reasoning"] == true
                || contains(&entry["supported_parameters"], "reasoning"),
            vision: model_vision(entry, id),
            input_cost: price("prompt"),
            output_cost: price("completion"),
        });
    }
    // An empty valid catalogue is authoritative; malformed nonempty responses are not.
    ensure!(
        entries.is_empty() || !ids.is_empty(),
        "The /models response contains no valid model IDs"
    );
    models.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then(a.model.cmp(&b.model))
    });
    Ok(models)
}
async fn fetch(p: &CustomProvider, key: Option<&str>) -> Result<Vec<CustomModel>> {
    provider_accounts::validate(p)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut request = client
        .get(format!("{}/models", p.base_url))
        .header(reqwest::header::ACCEPT, "application/json");
    if !p.no_auth {
        request = request.bearer_auth(
            key.context("Add an API key or enable No API key required, then refresh models")?,
        );
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| {
            let local = reqwest::Url::parse(&p.base_url).ok().is_some_and(|url| {
                url.host_str().is_some_and(|host| host == "localhost" || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
            });
            anyhow::anyhow!(if local {
                "Could not reach the provider's /models endpoint. Localhost points to the Kindred server, not this desktop. For a model on your computer, use an address reachable from your Kindred server."
            } else {
                "Could not reach the provider's /models endpoint. Check that the base URL is reachable from your Kindred server."
            })
        })?;
    ensure!(
        response.status().is_success(),
        "Model catalogue returned HTTP {}. Check the base URL and API key, then refresh models",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("The model catalogue download was interrupted"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Provider catalogue exceeds the download size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("The /models endpoint did not return valid JSON"))?;
    parse(&value)
}
pub async fn refresh(app: &App, id: &str) -> Result<CustomProvider> {
    let _ = provider(app, id)?; // Never allocate catalogue gates for arbitrary IDs.
    let gate = app.catalogues.gate(id);
    let (guard, waited) = match gate.clone().try_lock_owned() {
        Ok(guard) => (guard, false),
        Err(_) => (gate.lock_owned().await, true),
    };
    let _permit = app.catalogues.permits.acquire().await?;
    let (snapshot, key) = {
        let _config = provider_accounts::CONFIG_LOCK.lock().await;
        (
            provider(app, id)?,
            crate::connections::credential(app, id, ""),
        )
    };
    if waited
        && snapshot.catalog.checked_at.is_some()
        && snapshot.catalog.checked_revision == snapshot.revision
    {
        return Ok(snapshot);
    }
    let result = fetch(&snapshot, key.as_deref()).await;
    let _config = provider_accounts::CONFIG_LOCK.lock().await;
    let mut all = provider_accounts::custom(app)?;
    let current = all
        .iter_mut()
        .find(|p| p.id == id)
        .context("Provider not found")?;
    if current.revision != snapshot.revision
        || current.base_url != snapshot.base_url
        || current.no_auth != snapshot.no_auth
        || crate::connections::credential(app, id, "") != key
    {
        return Ok(current.clone()); // A newer configuration owns the next refresh.
    }
    current.catalog.checked_at = Some(db::now());
    current.catalog.checked_revision = current.revision.clone();
    match result {
        Ok(models) => {
            current.models = models;
            current.catalog.updated_at = Some(db::now());
            current.catalog.error = None;
        }
        Err(error) => current.catalog.error = Some(error.to_string()),
    }
    let answer = current.clone();
    app.db
        .save_setting("custom_providers", &serde_json::to_value(all)?)?;
    drop(guard);
    Ok(answer)
}
pub async fn startup(app: Shared) {
    if app.catalogues.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(providers) = provider_accounts::custom(&app) else {
        return;
    };
    let mut tasks = tokio::task::JoinSet::new();
    for p in providers {
        if !p.no_auth && crate::connections::credential(&app, &p.id, "").is_none() {
            continue;
        }
        let app = app.clone();
        tasks.spawn(async move {
            let _ = refresh(&app, &p.id).await;
        });
    }
    while tasks.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
    };
    use std::sync::atomic::AtomicUsize;
    struct Fixture {
        url: String,
        mode: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        headers: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn fixture() -> Fixture {
        let mode = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let headers = Arc::new(Mutex::new(Vec::new()));
        let (m, c, h) = (mode.clone(), calls.clone(), headers.clone());
        let router=Router::new().route("/v1/models",get(move|headers:HeaderMap|{let (m,c,h)=(m.clone(),c.clone(),h.clone());async move{
            c.fetch_add(1,Ordering::SeqCst);h.lock().unwrap().push(headers.get("authorization").and_then(|v|v.to_str().ok()).unwrap_or("").to_owned());
            tokio::time::sleep(Duration::from_millis(30)).await;
            match m.load(Ordering::SeqCst){
                1=>(StatusCode::SERVICE_UNAVAILABLE,"sensitive-provider-error-body").into_response(),
                2=>(StatusCode::OK,"invalid-json-sensitive-error").into_response(),
                3=>(StatusCode::FOUND,[("location","/sink")],"").into_response(),
                4=>Json(json!({"data":[]})).into_response(),
                _=>Json(json!({"data":[{"id":"test/model","name":"A discovered model","context_length":64000,"max_output_tokens":32000,"pricing":{"prompt":"0.000002","completion":"0.000005"},"supported_parameters":["tools","reasoning"],"architecture":{"input_modalities":["text","image"]}},{"id":"local-model"}]})).into_response()
            }
        }})).route("/sink",get(||async{panic!("Catalogue redirects must never be followed");#[allow(unreachable_code)]""}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Fixture {
            url,
            mode,
            calls,
            headers,
            task,
        }
    }
    fn with_provider(app: &App, url: &str) -> String {
        let id = format!("custom-{}", db::id());
        app.db
            .save_setting(
                "custom_providers",
                &json!([{"id":id,"name":"Fixture","base_url":url,"no_auth":true}]),
            )
            .unwrap();
        id
    }
    fn persistent_app() -> (Shared, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("kindred-catalogue-{}.db", db::id()));
        let mut app = crate::tests::app();
        let a = Arc::get_mut(&mut app).unwrap();
        a.config.database = path.to_string_lossy().into();
        a.db = crate::db::Db::open(&a.config.database).unwrap();
        (app, path)
    }
    #[test]
    fn splash_vision_fallback_respects_explicit_capabilities() {
        let id = "incoai/Qwen3.6-35B-A3B-Splash";
        assert!(model_vision(&json!({"id":id}), id));
        assert!(model_vision(&json!({}), "Qwen3.6-35B-A3B-Splash"));
        assert!(!model_vision(&json!({"vision":false}), id));
        assert!(!model_vision(&json!({"input_modalities":["text"]}), id));
        assert!(!model_vision(&json!({}), "unknown-qwen"));
        assert!(model_vision(&json!({"supports_vision":true}), "opaque"));
    }

    #[test]
    fn catalogue_metadata_is_bounded_and_missing_prices_remain_unknown() {
        let parsed=parse(&json!({"data":[{"id":"opaque"},{"id":"opaque"},{"id":"vision","name":"Friendly model","context_length":64000,"max_output_tokens":32000,"pricing":{"prompt":"0.000002","completion":"0.000005"},"supports_reasoning":true,"input_modalities":["image","text"]},{"id":"embed","type":"embedding"}]})).unwrap();
        assert_eq!(parsed.len(), 2);
        let p = parsed.iter().find(|m| m.model == "opaque").unwrap();
        assert_eq!(p.context_window, 32768);
        assert_eq!(p.max_output_tokens, None);
        assert_eq!(p.input_cost, None);
        assert!(!p.reasoning && !p.vision);
        let p = parsed.iter().find(|m| m.model == "vision").unwrap();
        assert!(p.reasoning && p.vision);
        assert_eq!(p.input_cost, Some(2.0));
        assert_eq!(p.output_cost, Some(5.0));
        assert_eq!(p.context_window, 64000);
        assert_eq!(p.max_output_tokens, Some(32000));
        assert!(parse(&json!({"data":[{}]})).is_err());
        assert!(parse(&json!({"data":[],"has_more":true})).is_err());
        assert!(parse(&json!({"data":vec![json!({"id":"x"});4097]})).is_err());
        assert!(parse(&json!({"error":"key-secret"})).is_err());
        assert!(parse(&json!({"data":[]})).unwrap().is_empty());
    }
    #[tokio::test]
    async fn startup_refreshes_once_and_cached_reads_do_not_refetch() {
        let f = fixture().await;
        let app = crate::tests::app();
        let id = with_provider(&app, &f.url);
        startup(app.clone()).await;
        startup(app.clone()).await;
        let Json(data) =
            provider_accounts::models(State(app.clone()), axum::extract::Path(id.clone()))
                .await
                .ok()
                .unwrap();
        assert_eq!(data["data"].as_array().unwrap().len(), 2);
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        assert_eq!(&*f.headers.lock().unwrap(), &[""]);
        let a = refresh(&app, &id);
        let b = refresh(&app, &id);
        let (a, b) = tokio::join!(a, b);
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
        let mut reload = crate::tests::app();
        Arc::get_mut(&mut reload)
            .unwrap()
            .db
            .save_setting(
                "custom_providers",
                &app.db.setting("custom_providers").unwrap().unwrap(),
            )
            .unwrap();
        let Json(data) = provider_accounts::models(State(reload), axum::extract::Path(id))
            .await
            .ok()
            .unwrap();
        assert_eq!(data["models"].as_array().unwrap().len(), 2);
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn failed_refresh_and_redirect_retain_cache_without_exposing_error_bodies() {
        let f = fixture().await;
        let app = crate::tests::app();
        let id = with_provider(&app, &f.url);
        let first = refresh(&app, &id).await.unwrap();
        for mode in [1, 2, 3] {
            f.mode.store(mode, Ordering::SeqCst);
            let p = refresh(&app, &id).await.unwrap();
            assert_eq!(p.models.len(), 2);
            assert_eq!(p.catalog.updated_at, first.catalog.updated_at);
            assert!(p.catalog.error.is_some());
            assert!(!p.catalog.error.unwrap().contains("sensitive"));
        }
        f.mode.store(4, Ordering::SeqCst);
        let p = refresh(&app, &id).await.unwrap();
        assert!(p.models.is_empty());
        assert!(p.catalog.error.is_none());
    }
    #[tokio::test]
    async fn save_discovers_named_providers_without_models_and_preserves_card_order() {
        let f = fixture().await;
        let (app, path) = persistent_app();
        let Json(first)=provider_accounts::save(State(app.clone()),Json(json!({"provider":{"name":"First provider","base_url":f.url,"no_auth":false},"key":"fixture-api-key"}))).await.ok().unwrap();
        let first = first["id"].as_str().unwrap().to_owned();
        let _ = provider_accounts::models(State(app.clone()), axum::extract::Path(first.clone()))
            .await
            .ok()
            .unwrap();
        let Json(second) = provider_accounts::save(
            State(app.clone()),
            Json(json!({"provider":{"name":"Local models","base_url":f.url,"no_auth":true}})),
        )
        .await
        .ok()
        .unwrap();
        let second = second["id"].as_str().unwrap().to_owned();
        let _ = provider_accounts::models(State(app.clone()), axum::extract::Path(second.clone()))
            .await
            .ok()
            .unwrap();
        let _=provider_accounts::save(State(app.clone()),Json(json!({"provider":{"id":first,"name":"Renamed provider","base_url":f.url,"no_auth":false}}))).await.ok().unwrap();
        let saved = provider_accounts::custom(&app).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].id, first);
        assert_eq!(saved[0].name, "Renamed provider");
        assert_eq!(saved[1].id, second);
        assert_eq!(saved[0].models.len(), 2);
        assert_eq!(saved[1].models.len(), 2);
        assert!(
            f.headers
                .lock()
                .unwrap()
                .contains(&"Bearer fixture-api-key".to_owned())
        );
        assert!(f.headers.lock().unwrap().contains(&String::new()));
        assert!(
            !serde_json::to_string(&saved)
                .unwrap()
                .contains("fixture-api-key")
        );
        drop(app);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.credentials", path.display()));
    }
    #[tokio::test]
    async fn discovered_model_runs_the_existing_pi_tool_loop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let router=Router::new().route("/v1/models",get(||async{Json(json!({"data":[{"id":"discovered/model","context_length":64000}]}))}))
            .route("/v1/chat/completions",axum::routing::post(move|headers:HeaderMap,Json(body):Json<Value>|{let counter=counter.clone();async move{
                assert!(headers.get("authorization").is_none());assert_eq!(body["model"],"discovered/model");
                crate::pi::sse_response(if counter.fetch_add(1,Ordering::SeqCst)==0 {json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"catalogue-memory","type":"function","function":{"name":"remember","arguments":"{\"text\":\"Discovered model memory\"}"}}]},"finish_reason":"tool_calls"}]})} else {json!({"choices":[{"message":{"role":"assistant","content":"Discovered model completed."},"finish_reason":"stop"}]})})
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut app, path) = persistent_app();
        let config = &mut Arc::get_mut(&mut app).unwrap().config.pi;
        config.node_binary =
            std::env::var("KINDRED_PI_TEST_NODE").unwrap_or_else(|_| "node".into());
        config.worker_script = std::env::var("KINDRED_PI_TEST_WORKER")
            .unwrap_or_else(|_| format!("{}/harness/pi/worker.mjs", env!("CARGO_MANIFEST_DIR")));
        let Json(saved) = provider_accounts::save(
            State(app.clone()),
            Json(json!({"provider":{"name":"Discovered fixture","base_url":url,"no_auth":true}})),
        )
        .await
        .ok()
        .unwrap();
        let id = saved["id"].as_str().unwrap();
        let Json(models) =
            provider_accounts::models(State(app.clone()), axum::extract::Path(id.into()))
                .await
                .ok()
                .unwrap();
        assert_eq!(models["data"][0]["model"], "discovered/model");
        let mut bot = crate::tests::bot(&app.db, id);
        bot.model = "discovered/model".into();
        bot.reasoning_effort.clear();
        app.db.save_bot(&bot).unwrap();
        let task = app
            .db
            .run(
                &app.db
                    .queue(&bot.id, "Remember a catalogue fact", 0)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            provider_accounts::run(&app, &bot, &task).await.unwrap(),
            "Discovered model completed."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            app.db
                .bot(&bot.id)
                .unwrap()
                .memory
                .contains("Discovered model memory")
        );
        let Json(usage) =
            provider_accounts::usage(State(app.clone()), axum::extract::Path(id.into()))
                .await
                .ok()
                .unwrap();
        assert_eq!(usage["bots"][0]["requests"], 2);
        assert_eq!(usage["bots"][0]["unpriced_requests"], 2);
        server.abort();
        drop(app);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.credentials", path.display()));
    }
    #[tokio::test]
    async fn edited_endpoint_clears_old_key_and_rejects_old_inflight_catalogue() {
        let old = fixture().await;
        let new = fixture().await;
        let (app, path) = persistent_app();
        let id = with_provider(&app, &old.url);
        {
            let mut p = provider(&app, &id).unwrap();
            p.no_auth = false;
            app.db
                .save_setting("custom_providers", &json!([p]))
                .unwrap();
            crate::connections::save_credential(&app, &id, "", "old-fixture-key").unwrap();
        }
        let copy = app.clone();
        let copy_id = id.clone();
        let pending = tokio::spawn(async move { refresh(&copy, &copy_id).await.unwrap() });
        while old.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let _=provider_accounts::save(State(app.clone()),Json(json!({"provider":{"id":id,"name":"Moved provider","base_url":new.url,"no_auth":false}}))).await.ok().unwrap();
        let _ = pending.await.unwrap();
        let current = provider(&app, &id).unwrap();
        assert_eq!(current.base_url, new.url);
        assert!(current.models.is_empty());
        assert!(crate::connections::credential(&app, &id, "").is_none());
        assert_eq!(new.calls.load(Ordering::SeqCst), 0);
        let _=provider_accounts::save(State(app.clone()),Json(json!({"provider":{"id":id,"name":"Moved provider","base_url":new.url,"no_auth":false},"key":"new-fixture-key"}))).await.ok().unwrap();
        let _ = provider_accounts::models(State(app.clone()), axum::extract::Path(id))
            .await
            .ok()
            .unwrap();
        assert_eq!(&*new.headers.lock().unwrap(), &["Bearer new-fixture-key"]);
        drop(app);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.credentials", path.display()));
    }
}
