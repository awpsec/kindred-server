//! Official OpenCode API-key accounts; Go and Zen never fall back to each other.
use crate::{
    db::{Bot, Run},
    runtime::{App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::{Value, json};
use std::time::Duration;

fn base(id: &str) -> Result<&'static str> {
    match id {
        "opencode" => Ok("https://opencode.ai/zen/v1"),
        "opencode-go" => Ok("https://opencode.ai/zen/go/v1"),
        _ => anyhow::bail!("Unknown OpenCode provider"),
    }
}
fn supported(id: &str) -> Result<Vec<Value>> {
    base(id)?;
    let catalog: Value = serde_json::from_str(include_str!("../harness/pi/opencode-models.json"))?;
    Ok(catalog[id]
        .as_array()
        .context("Missing OpenCode model metadata")?
        .clone())
}
async fn available(id: &str) -> Result<Vec<Value>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Kindred/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut response = client
        .get(format!("{}/models", base(id)?))
        .send()
        .await
        .context("Could not load OpenCode models; check the Kindred server connection")?;
    ensure!(
        response.status().is_success(),
        "OpenCode model catalogue returned HTTP {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 2 * 1024 * 1024,
            "OpenCode catalogue is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let live: Value = serde_json::from_slice(&bytes)?;
    let ids = live["data"]
        .as_array()
        .context("Invalid OpenCode catalogue")?;
    let models = supported(id)?
        .into_iter()
        .filter(|m| ids.iter().any(|v| v["id"] == m["id"]))
        .collect::<Vec<_>>();
    ensure!(
        !models.is_empty(),
        "No OpenCode models match the installed harness. Update Kindred to refresh model support."
    );
    Ok(models)
}
pub async fn save_key(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    base(&id)?;
    let key = body["key"].as_str().context("API key is required")?.trim();
    crate::connections::save_credential(&app, &id, "", key)?;
    Ok(Json(json!({"saved":true,"connected":!key.is_empty()})))
}
pub async fn models(Path(id): Path<String>) -> Result<Json<Value>, crate::web::Error> {
    let data=available(&id).await?.iter().map(|m|json!({"model":m["id"],"displayName":m["name"],"reasoning":m["reasoning"],"vision":m["input"].as_array().is_some_and(|v|v.iter().any(|v|v=="image")),"context_window":m["contextWindow"]})).collect::<Vec<_>>();
    Ok(Json(json!({"data":data})))
}
pub async fn run(app: &App, bot: &Bot, run: &Run) -> Result<String> {
    let key = crate::connections::credential(app, &bot.provider, "")
        .context("Connect OpenCode in Settings > Connections first")?;
    let m = available(&bot.provider)
        .await?
        .into_iter()
        .find(|m| m["id"] == bot.model)
        .context("Choose an available OpenCode model in bot settings")?;
    let suffix = match m["api"].as_str() {
        Some("anthropic-messages") => "messages",
        Some("openai-responses") => "responses",
        Some("openai-completions") => "chat/completions",
        _ => anyhow::bail!("Unsupported OpenCode model transport"),
    };
    let endpoint = format!("{}/{suffix}", base(&bot.provider)?);
    let info = json!({"id":bot.model,"context_window":m["contextWindow"],"reasoning":m["reasoning"],"vision":m["input"].as_array().is_some_and(|v|v.iter().any(|v|v=="image")),"max_tokens":m["maxTokens"]});
    crate::pi::custom(app, bot, run, &key, info, &endpoint).await
}
