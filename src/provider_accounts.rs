//! Owner-configured endpoints and usage receipts. Credentials never enter settings.
use crate::{
    db::{self, Bot, Run},
    runtime::{App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProvider {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<CustomModel>,
    #[serde(default)]
    pub no_auth: bool,
    #[serde(default)]
    pub catalog: crate::provider_catalog::CatalogInfo,
    #[serde(default)]
    pub revision: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomModel {
    pub model: String,
    #[serde(default)]
    pub display_name: String,
    pub context_window: u64,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub input_cost: Option<f64>,
    #[serde(default)]
    pub output_cost: Option<f64>,
}
pub fn valid_id(id: &str) -> bool {
    matches!(id, "codex" | "openrouter" | "claude-code" | "kimi-code" | "opencode" | "opencode-go")
        || id
            .strip_prefix("custom-")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}
pub fn validate(p: &CustomProvider) -> Result<()> {
    ensure!(
        p.id.starts_with("custom-") && valid_id(&p.id),
        "Invalid provider ID"
    );
    ensure!(
        !p.name.trim().is_empty() && p.name.len() <= 80,
        "Provider name must have 1–80 characters"
    );
    let url = reqwest::Url::parse(&p.base_url).context("Invalid provider URL")?;
    ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && p.base_url.len() <= 2000,
        "Use a provider base URL without credentials, query or fragment"
    );
    let host = url.host_str().unwrap();
    ensure!(
        host != "169.254.169.254"
            && host != "metadata.google.internal"
            && !host.starts_with("169.254."),
        "Metadata endpoints cannot be model providers"
    );
    ensure!(
        p.models.len() <= 4096,
        "Provider catalogue exceeds the model limit"
    );
    let mut ids = std::collections::HashSet::new();
    for m in &p.models {
        ensure!(m.max_output_tokens.is_none_or(|n| n > 0 && n <= m.context_window), "Invalid model output limit");
        ensure!(
            !m.model.trim().is_empty()
                && m.model.len() <= 200
                && !m.model.chars().any(char::is_control)
                && ids.insert(&m.model),
            "Model IDs must be unique and valid"
        );
        ensure!(
            (1024..=10_000_000).contains(&m.context_window),
            "Invalid model context window"
        );
        ensure!(
            [m.input_cost, m.output_cost]
                .into_iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=10000.0).contains(&v)),
            "Costs must be dollars per million tokens"
        );
    }
    Ok(())
}
pub fn custom(app: &App) -> Result<Vec<CustomProvider>> {
    Ok(serde_json::from_value(
        app.db.setting("custom_providers")?.unwrap_or(json!([])),
    )?)
}
pub async fn list(State(app): State<Shared>) -> Result<Json<Value>, crate::web::Error> {
    let mut rows = vec![
        json!({"id":"codex","name":"Codex","kind":"subscription"}),
        json!({"id":"openrouter","name":"OpenRouter","kind":"api","connected":crate::connections::openrouter_key(&app).is_some()}),
        json!({"id":"claude-code","name":"Claude Code","kind":"subscription"}),
        json!({"id":"kimi-code","name":"Kimi Code","kind":"subscription"}),
    ];
    for (id, name) in [("opencode-go", "OpenCode Go"), ("opencode", "OpenCode Zen")] {
        rows.push(json!({"id":id,"name":name,"kind":"api","auth":"opencode-key","connected":crate::connections::credential(&app,id,"").is_some()}));
    }
    for p in custom(&app)? {
        let mut row = serde_json::to_value(&p)?;
        row["connected"] =
            json!(p.no_auth || crate::connections::credential(&app, &p.id, "").is_some());
        row["kind"] = json!("api");
        row["catalog_refreshing"] = json!(app.catalogues.busy(&p.id));
        rows.push(row);
    }
    let history = app
        .db
        .0
        .lock()
        .unwrap()
        .prepare("SELECT DISTINCT provider FROM provider_usage")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
    for row in &mut rows {
        row["has_usage"] = json!(history.contains(row["id"].as_str().unwrap_or_default()));
    }
    Ok(Json(json!({"providers":rows})))
}
pub(crate) static CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn normalize_base_url(input: &str) -> String {
    let mut value = input.trim().trim_end_matches('/').to_owned();
    if !value.contains("://") {
        let parsed = reqwest::Url::parse(&format!("http://{value}"));
        let local = parsed.as_ref().is_ok_and(|url| {
            url.port().is_some() || url.host_str().is_some_and(|host| {
                !host.contains('.') || host.ends_with(".local") || host.ends_with(".internal")
                    || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_private() || ip.is_loopback())
            })
        });
        value = format!("{}://{value}", if local { "http" } else { "https" });
    }
    if reqwest::Url::parse(&value).is_ok_and(|url| url.path() == "/") {
        value.push_str("/v1");
    }
    value
}

pub async fn save(
    State(app): State<Shared>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    let mut p: CustomProvider = serde_json::from_value(v["provider"].clone())?;
    if p.id.is_empty() {
        p.id = format!("custom-{}", db::id());
    }
    p.name = p.name.trim().to_owned();
    p.base_url = normalize_base_url(&p.base_url);
    // Catalogue and revision fields are server-owned; old clients may still send models.
    p.models.clear();
    p.catalog = Default::default();
    validate(&p)?;
    let guard = CONFIG_LOCK.lock().await;
    let mut all = custom(&app)?;
    if all.len() >= 20 && !all.iter().any(|old| old.id == p.id) {
        return Err(anyhow::anyhow!("At most 20 custom providers are allowed").into());
    }
    let old = all.iter().find(|old| old.id == p.id);
    let previous_key = crate::connections::credential(&app, &p.id, "").unwrap_or_default();
    let endpoint_changed = old.is_some_and(|old| old.base_url != p.base_url);
    let key = if p.no_auth || endpoint_changed && v["key"].as_str().is_none() {
        ""
    } else {
        v["key"].as_str().unwrap_or(&previous_key)
    };
    let changed = old.is_none_or(|old| old.base_url != p.base_url || old.no_auth != p.no_auth)
        || key != previous_key;
    p.revision = if changed {
        db::id()
    } else {
        old.unwrap().revision.clone()
    };
    if !changed {
        p.models = old.unwrap().models.clone();
        p.catalog = old.unwrap().catalog.clone();
    }
    crate::connections::save_credential(&app, &p.id, "", key)?;
    if let Some(index) = all.iter().position(|old| old.id == p.id) {
        all[index] = p.clone();
    } else {
        all.push(p.clone());
    }
    app.db
        .save_setting("custom_providers", &serde_json::to_value(all)?)?;
    drop(guard);
    if (changed || p.catalog.checked_at.is_none()) && (p.no_auth || !key.is_empty()) {
        let app = app.clone();
        let id = p.id.clone();
        tokio::spawn(async move {
            let _ = crate::provider_catalog::refresh(&app, &id).await;
        });
    }
    Ok(Json(json!({"id":p.id})))
}
/// Remove configuration and credentials; historical usage and bots remain intact.
pub async fn remove(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, crate::web::Error> {
    let _guard = CONFIG_LOCK.lock().await;
    let mut all = custom(&app)?;
    let index = all.iter().position(|p| p.id == id)
        .context("Custom provider is no longer configured")?;
    all.remove(index);
    crate::connections::save_credential(&app, &id, "", "")?;
    app.db.save_setting("custom_providers", &serde_json::to_value(all)?)?;
    Ok(Json(json!({"removed":id})))
}

pub async fn models(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, crate::web::Error> {
    let mut p = custom(&app)?
        .into_iter()
        .find(|p| p.id == id)
        .context("Provider not found")?;
    if p.catalog.checked_at.is_none() || app.catalogues.busy(&id) {
        p = crate::provider_catalog::refresh(&app, &id).await?;
    }
    Ok(Json(crate::provider_catalog::response(&p)))
}
pub async fn refresh_models(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(options): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    if options["startup"] == true {
        let p = custom(&app)?
            .into_iter()
            .find(|p| p.id == id)
            .context("Provider not found")?;
        if p.catalog
            .checked_at
            .is_some_and(|time| (0..60).contains(&(db::now() - time)))
            && !app.catalogues.busy(&id)
        {
            return Ok(Json(crate::provider_catalog::response(&p)));
        }
    }
    Ok(Json(crate::provider_catalog::response(
        &crate::provider_catalog::refresh(&app, &id).await?,
    )))
}
pub async fn run(app: &App, bot: &Bot, run: &Run) -> Result<String> {
    let p = custom(app)?
        .into_iter()
        .find(|p| p.id == bot.provider)
        .context("Custom provider is no longer configured")?;
    validate(&p)?;
    let key = if p.no_auth {
        "kindred-no-auth".to_owned()
    } else {
        crate::connections::credential(app, &p.id, "")
            .context("Connect this provider in Settings > Connections first")?
    };
    let m = p
        .models
        .iter()
        .find(|m| m.model == bot.model)
        .context("Choose a model from the provider catalogue in the bot settings; refresh models in Connections if needed")?;
    let info = json!({"no_auth":p.no_auth,"id":m.model,"context_window":m.context_window,"max_tokens":m.max_output_tokens,"reasoning":m.reasoning,"vision":m.vision,"input_cost":m.input_cost,"output_cost":m.output_cost});
    crate::pi::custom(
        app,
        bot,
        run,
        &key,
        info,
        &format!("{}/chat/completions", p.base_url),
    )
    .await
}

pub fn migrate(c: &Connection) -> Result<()> {
    let transaction = c.unchecked_transaction()?;
    let c = &*transaction;
    c.execute_batch("CREATE TABLE IF NOT EXISTS provider_usage(run_id TEXT NOT NULL,request_id INTEGER NOT NULL,bot_id TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL,cached_tokens INTEGER NOT NULL,cost REAL,cost_source TEXT NOT NULL,created INTEGER NOT NULL,tokens_reported INTEGER NOT NULL DEFAULT 1,bot_name TEXT NOT NULL,bot_profile TEXT NOT NULL,PRIMARY KEY(run_id,request_id));")?;
    let columns = c
        .prepare("PRAGMA table_info(provider_usage)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "bot_name") {
        // Receipts outlive operational runs and bots. Copy only existing receipt
        // data; identity snapshots begin with the current identity at migration.
        c.execute_batch("CREATE TABLE provider_usage_history_migration(run_id TEXT NOT NULL,request_id INTEGER NOT NULL,bot_id TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL,cached_tokens INTEGER NOT NULL,cost REAL,cost_source TEXT NOT NULL,created INTEGER NOT NULL,tokens_reported INTEGER NOT NULL DEFAULT 1,bot_name TEXT NOT NULL,bot_profile TEXT NOT NULL,PRIMARY KEY(run_id,request_id));
            INSERT INTO provider_usage_history_migration SELECT u.run_id,u.request_id,u.bot_id,u.provider,u.model,u.input_tokens,u.output_tokens,u.cached_tokens,u.cost,u.cost_source,u.created,u.tokens_reported,COALESCE(b.name,'Deleted bot'),json_object('shape',COALESCE(json_extract(b.profile,'$.shape'),'round'),'color',COALESCE(json_extract(b.profile,'$.color'),'#d8d8d8'),'eyes',COALESCE(json_extract(b.profile,'$.eyes'),'curious'),'animated',json(CASE WHEN COALESCE(json_extract(b.profile,'$.animated'),1) THEN 'true' ELSE 'false' END)) FROM provider_usage u LEFT JOIN bots b ON b.id=u.bot_id;
            DROP TABLE provider_usage;
            ALTER TABLE provider_usage_history_migration RENAME TO provider_usage;")?;
    }
    c.execute_batch("CREATE INDEX IF NOT EXISTS usage_provider ON provider_usage(provider,bot_id);
        CREATE TRIGGER IF NOT EXISTS preserve_bot_usage_identity BEFORE DELETE ON bots BEGIN
            UPDATE provider_usage SET bot_name=OLD.name,bot_profile=json_object('shape',COALESCE(json_extract(OLD.profile,'$.shape'),'round'),'color',COALESCE(json_extract(OLD.profile,'$.color'),'#d8d8d8'),'eyes',COALESCE(json_extract(OLD.profile,'$.eyes'),'curious'),'animated',json(CASE WHEN COALESCE(json_extract(OLD.profile,'$.animated'),1) THEN 'true' ELSE 'false' END)) WHERE bot_id=OLD.id;
        END;")?;
    transaction.commit()?;
    Ok(())
}
fn avatar_profile(profile: &Value) -> Value {
    json!({
        "shape":profile["shape"].as_str().unwrap_or("round"),
        "color":profile["color"].as_str().unwrap_or("#d8d8d8"),
        "eyes":profile["eyes"].as_str().unwrap_or("curious"),
        "animated":profile["animated"].as_bool().unwrap_or(true),
    })
}
pub fn record(app: &App, bot: &Bot, run: &Run, v: &Value) -> Result<()> {
    let request = v["request_id"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 1000)
        .context("Invalid usage receipt ID")?;
    // Receipt counters restart with each provider process; preserve usage from
    // earlier connection attempts instead of overwriting its first request.
    let attempt =
        crate::task_recovery::metadata(&app.db.0.lock().unwrap(), &run.id, "provider_retry")?
            .and_then(|v| v["attempt"].as_u64())
            .unwrap_or(0)
            .min(5);
    let request = request + attempt * 1000;
    let count = |key: &str| {
        v[key]
            .as_u64()
            .filter(|n| *n <= 1_000_000_000)
            .context("Invalid token receipt")
    };
    let (input, output, cached) = (
        count("input_tokens")?,
        count("output_tokens")?,
        count("cached_tokens")?,
    );
    let cost = v["cost"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 1_000_000.0);
    let source = v["cost_source"].as_str().unwrap_or("unknown");
    ensure!(
        matches!(
            source,
            "reported" | "estimated" | "unknown" | "subscription"
        ) && (cost.is_some() == matches!(source, "reported" | "estimated")),
        "Invalid cost receipt"
    );
    let profile = avatar_profile(&serde_json::to_value(&bot.profile)?);
    let c = app.db.0.lock().unwrap();
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs r JOIN bots b ON b.id=r.bot_id WHERE r.id=? AND b.id=?)",
        params![run.id, bot.id],
        |r| r.get(0),
    )?;
    ensure!(
        exists,
        "Usage receipt does not belong to an existing bot run"
    );
    c.execute("INSERT INTO provider_usage(run_id,request_id,bot_id,provider,model,input_tokens,output_tokens,cached_tokens,cost,cost_source,created,tokens_reported,bot_name,bot_profile) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(run_id,request_id) DO UPDATE SET input_tokens=excluded.input_tokens,output_tokens=excluded.output_tokens,cached_tokens=excluded.cached_tokens,cost=excluded.cost,cost_source=excluded.cost_source,tokens_reported=excluded.tokens_reported",params![run.id,request,bot.id,bot.provider,bot.model,input,output,cached,cost,source,db::now(),v["tokens_reported"]!=false,bot.name,profile.to_string()])?;
    Ok(())
}
pub async fn usage(
    State(app): State<Shared>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, crate::web::Error> {
    if !valid_id(&provider) {
        return Err(anyhow::anyhow!("Invalid provider").into());
    }
    let c = app.db.0.lock().unwrap();
    let mut q = c.prepare("WITH latest AS (SELECT bot_id,bot_name,bot_profile,ROW_NUMBER() OVER (PARTITION BY bot_id ORDER BY created DESC,rowid DESC) AS position FROM provider_usage WHERE provider=?1)
        SELECT u.bot_id,COALESCE(b.name,l.bot_name),SUM(u.input_tokens),SUM(u.output_tokens),SUM(u.cached_tokens),SUM(CASE WHEN u.cost_source='reported' THEN u.cost ELSE 0 END),SUM(CASE WHEN u.cost_source='estimated' THEN u.cost ELSE 0 END),SUM(CASE WHEN u.cost_source='unknown' THEN 1 ELSE 0 END),COUNT(*),MIN(u.created),SUM(CASE WHEN u.tokens_reported=0 THEN 1 ELSE 0 END),SUM(CASE WHEN u.cost_source='reported' THEN 1 ELSE 0 END),SUM(CASE WHEN u.cost_source='estimated' THEN 1 ELSE 0 END),MAX(u.created),COALESCE(b.profile,l.bot_profile),CASE WHEN b.id IS NULL OR COALESCE(json_extract(b.profile,'$.deleted'),0)=1 THEN 'deleted' WHEN COALESCE(json_extract(b.profile,'$.archived'),0)=1 THEN 'archived' ELSE 'active' END
        FROM provider_usage u LEFT JOIN bots b ON b.id=u.bot_id JOIN latest l ON l.bot_id=u.bot_id AND l.position=1 WHERE u.provider=?1 GROUP BY u.bot_id ORDER BY COALESCE(b.name,l.bot_name) COLLATE NOCASE,u.bot_id")?;
    let rows = q.query_map([provider],|r| {
        let first_used_at = r.get::<_,i64>(9)?;
        let profile: Value = serde_json::from_str(&r.get::<_,String>(14)?).unwrap_or(json!({}));
        let status = r.get::<_,String>(15)?;
        Ok(json!({"bot_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"input_tokens":r.get::<_,i64>(2)?,"output_tokens":r.get::<_,i64>(3)?,"cached_tokens":r.get::<_,i64>(4)?,"reported_cost":r.get::<_,f64>(5)?,"estimated_cost":r.get::<_,f64>(6)?,"unpriced_requests":r.get::<_,i64>(7)?,"requests":r.get::<_,i64>(8)?,"since":first_used_at,"first_used_at":first_used_at,"last_used_at":r.get::<_,i64>(13)?,"unreported_tokens":r.get::<_,i64>(10)?,"reported_requests":r.get::<_,i64>(11)?,"estimated_requests":r.get::<_,i64>(12)?,"profile":avatar_profile(&profile),"deleted":status=="deleted","status":status}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(
        json!({"bots":rows,"scope":"Recorded Kindred requests since usage tracking was enabled. Provider billing is authoritative."}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_usage_migration_preserves_receipts_and_survives_parent_removal() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE bots(id TEXT PRIMARY KEY,name TEXT NOT NULL,profile TEXT NOT NULL);
            CREATE TABLE runs(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL REFERENCES bots(id));
            CREATE TABLE provider_usage(run_id TEXT NOT NULL REFERENCES runs(id),request_id INTEGER NOT NULL,bot_id TEXT NOT NULL REFERENCES bots(id),provider TEXT NOT NULL,model TEXT NOT NULL,input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL,cached_tokens INTEGER NOT NULL,cost REAL,cost_source TEXT NOT NULL,created INTEGER NOT NULL,tokens_reported INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(run_id,request_id));
            CREATE INDEX usage_provider ON provider_usage(provider,bot_id);
            INSERT INTO bots VALUES('bot','Before migration','{\"shape\":\"triangle\",\"color\":\"#123456\",\"eyes\":\"happy\",\"animated\":false,\"local_device_id\":\"private-device\"}');
            INSERT INTO runs VALUES('run','bot');
            INSERT INTO provider_usage VALUES('run',1,'bot','openrouter','legacy-model',120,30,20,0.004,'reported',100,1);
            INSERT INTO provider_usage VALUES('run',2,'bot','openrouter','legacy-model',0,0,0,NULL,'unknown',200,0);")
            .unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let receipt: (i64,i64,i64,Option<f64>,String,i64,i64,String,String) = c.query_row(
            "SELECT input_tokens,output_tokens,cached_tokens,cost,cost_source,created,tokens_reported,bot_name,bot_profile FROM provider_usage WHERE request_id=1",[],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))
        ).unwrap();
        assert_eq!(
            (
                receipt.0,
                receipt.1,
                receipt.2,
                receipt.3,
                receipt.4.as_str(),
                receipt.5,
                receipt.6
            ),
            (120, 30, 20, Some(0.004), "reported", 100, 1)
        );
        assert_eq!(receipt.7, "Before migration");
        let profile: Value = serde_json::from_str(&receipt.8).unwrap();
        assert_eq!(
            profile,
            json!({"shape":"triangle","color":"#123456","eyes":"happy","animated":false})
        );
        c.execute_batch("UPDATE bots SET name='Last identity'; DELETE FROM runs WHERE id='run'; DELETE FROM bots WHERE id='bot';").unwrap();
        let totals: (i64,i64,i64,i64,String) = c.query_row("SELECT COUNT(*),SUM(input_tokens),MIN(created),MAX(created),MIN(bot_name) FROM provider_usage",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
        assert_eq!(totals, (2, 120, 100, 200, "Last identity".into()));
        assert_eq!(
            c.query_row(
                "SELECT cost IS NULL AND tokens_reported=0 FROM provider_usage WHERE request_id=2",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
            true
        );
        assert!(
            !c.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }

    #[tokio::test]
    async fn removal_erases_key_and_configuration_but_preserves_bot() {
        let mut app = crate::tests::app();
        let dir = std::env::temp_dir().join(format!("kindred-provider-remove-{}", db::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::sync::Arc::get_mut(&mut app).unwrap().config.database = dir.join("test.db").to_string_lossy().into_owned();
        let id = format!("custom-{}", db::id());
        app.db.save_setting("custom_providers", &json!([{"id":id,"name":"Local","base_url":"http://localhost:8000/v1","no_auth":false}])).unwrap();
        crate::connections::save_credential(&app, &id, "", "test-provider-key").unwrap();
        let bot = crate::tests::bot(&app.db, &id);
        assert!(remove(State(app.clone()), Path(id.clone())).await.is_ok());
        assert!(custom(&app).unwrap().is_empty());
        assert!(crate::connections::credential(&app, &id, "").is_none());
        assert!(app.db.bot(&bot.id).is_ok());
        assert!(remove(State(app.clone()), Path("codex".into())).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn usage_keeps_lifecycle_identity_dates_and_disconnected_provider_history() {
        let app = crate::tests::app();
        let provider = format!("custom-{}", db::id());
        app.db.save_setting("custom_providers",&json!([{"id":provider,"name":"Disconnected fixture","base_url":"https://models.example/v1","models":[{"model":"test/model","context_window":32000}]}])).unwrap();
        let active = crate::tests::bot(&app.db, &provider);
        let mut archived = crate::tests::bot(&app.db, &provider);
        let mut deleted = crate::tests::bot(&app.db, &provider);
        let _unused = crate::tests::bot(&app.db, &provider);
        let receipt = json!({"request_id":1,"input_tokens":120,"output_tokens":30,"cached_tokens":20,"cost":0.004,"cost_source":"reported","tokens_reported":true});
        let active_run = app
            .db
            .run(&app.db.queue(&active.id, "usage fixture", 0).unwrap())
            .unwrap();
        let archived_run = app
            .db
            .run(&app.db.queue(&archived.id, "usage fixture", 0).unwrap())
            .unwrap();
        let deleted_run = app
            .db
            .run(&app.db.queue(&deleted.id, "usage fixture", 0).unwrap())
            .unwrap();
        for (bot, run) in [
            (&active, &active_run),
            (&archived, &archived_run),
            (&deleted, &deleted_run),
        ] {
            record(&app, bot, run, &receipt).unwrap();
        }
        let mut second = receipt.clone();
        second["request_id"] = json!(2);
        second["cost_source"] = json!("estimated");
        record(&app, &active, &active_run, &second).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE provider_usage SET created=CASE request_id WHEN 1 THEN 100 ELSE 200 END",
                [],
            )
            .unwrap();
        // Repeated cumulative receipts neither add requests nor move the date range.
        record(&app, &active, &active_run, &second).unwrap();
        app.db
            .finish(&archived_run.id, "completed", "Usage fixture finished", "")
            .unwrap();
        archived.profile.archived = true;
        archived.provider = "codex".into();
        app.db.save_bot(&archived).unwrap();
        deleted.name = "Remembered bot".into();
        deleted.profile.shape = "triangle".into();
        deleted.profile.color = "#123456".into();
        deleted.profile.eyes = "happy".into();
        deleted.profile.animated = false;
        let private_device = db::id();
        deleted.profile.local_device_id = private_device.clone();
        app.db.save_bot(&deleted).unwrap();
        {
            let c = app.db.0.lock().unwrap();
            c.execute(
                "DELETE FROM run_message_sources WHERE run_id=?",
                [&deleted_run.id],
            )
            .unwrap();
            c.execute("DELETE FROM runs WHERE id=?", [&deleted_run.id])
                .unwrap();
            c.execute("DELETE FROM bots WHERE id=?", [&deleted.id])
                .unwrap();
            assert!(
                !c.prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .exists([])
                    .unwrap()
            );
        }
        assert!(record(&app, &deleted, &deleted_run, &receipt).is_err());
        assert!(record(&app, &active, &archived_run, &receipt).is_err());
        let Json(result) = usage(State(app.clone()), Path(provider.clone()))
            .await
            .ok()
            .unwrap();
        let rows = result["bots"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        let row = |id: &str| rows.iter().find(|r| r["bot_id"] == id).unwrap();
        assert_eq!(row(&active.id)["status"], "active");
        assert_eq!(row(&active.id)["requests"], 2);
        assert_eq!(row(&active.id)["input_tokens"], 240);
        assert_eq!(row(&active.id)["reported_requests"], 1);
        assert_eq!(row(&active.id)["estimated_requests"], 1);
        assert_eq!(row(&active.id)["first_used_at"], 100);
        assert_eq!(row(&active.id)["since"], 100);
        assert_eq!(row(&active.id)["last_used_at"], 200);
        assert_eq!(row(&archived.id)["status"], "archived");
        assert_eq!(row(&deleted.id)["status"], "deleted");
        assert_eq!(row(&deleted.id)["deleted"], true);
        assert_eq!(row(&deleted.id)["name"], "Remembered bot");
        assert_eq!(row(&deleted.id)["requests"], 1);
        assert_eq!(
            row(&deleted.id)["profile"],
            json!({"shape":"triangle","color":"#123456","eyes":"happy","animated":false})
        );
        assert!(!result.to_string().contains(&private_device));
        let Json(providers) = list(State(app)).await.ok().unwrap();
        let provider_row = providers["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == provider)
            .unwrap();
        assert_eq!(provider_row["connected"], false);
        assert_eq!(provider_row["has_usage"], true);
        assert_eq!(providers["providers"][0]["has_usage"], false);
    }

    #[test]
    fn provider_url_normalization_preserves_explicit_transport() {
        for (input, expected) in [
            ("arcadian:8000/v1", "http://arcadian:8000/v1"),
            (" arcadian:8000/ ", "http://arcadian:8000/v1"),
            ("localhost:8000/v1", "http://localhost:8000/v1"),
            ("http://models.example/v1", "http://models.example/v1"),
            ("https://arcadian:8000/v1", "https://arcadian:8000/v1"),
            ("models.example", "https://models.example/v1"),
        ] { assert_eq!(normalize_base_url(input), expected); }
    }

    #[test]
    fn endpoint_and_model_validation_rejects_credential_urls_and_invalid_prices() {
        let mut p = CustomProvider {
            id: format!("custom-{}", db::id()),
            name: "Private inference".into(),
            no_auth: false,
            base_url: "http://192.168.1.12:8000/v1".into(),
            models: vec![CustomModel {
                model: "local-model".into(),
                display_name: String::new(),
                context_window: 32768,
                max_output_tokens: None,
                reasoning: false,
                vision: false,
                input_cost: None,
                output_cost: None,
            }],
            catalog: Default::default(),
            revision: String::new(),
        };
        validate(&p).unwrap();
        for url in ["http://arcadian:8000/v1", "http://public.example/v1", "http://arcadian.local:8000/v1", "http://[fd00::1]:8000/v1"] {
            p.base_url = url.into();
            validate(&p).unwrap();
        }
        for url in [
            "https://key@public.example/v1",
            "https://public.example/v1?key=secret",
            "http://169.254.169.254/latest",
        ] {
            p.base_url = url.into();
            assert!(validate(&p).is_err());
        }
        p.base_url = "https://models.example/v1".into();
        p.models[0].input_cost = Some(-1.0);
        assert!(validate(&p).is_err());
        p.models[0].input_cost = None;
        p.models.push(p.models[0].clone());
        assert!(validate(&p).is_err());
    }
    #[tokio::test]
    async fn usage_is_per_bot_and_cumulative_receipts_replace_without_double_billing() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db.queue(&bot.id, "usage fixture", 0).unwrap();
        let run = app.db.claim().unwrap().unwrap();
        let unknown = json!({"request_id":1,"input_tokens":0,"output_tokens":0,"cached_tokens":0,"cost":null,"cost_source":"unknown","tokens_reported":false});
        record(&app, &bot, &run, &unknown).unwrap();
        let receipt = json!({"request_id":1,"input_tokens":120,"output_tokens":30,"cached_tokens":20,"cost":0.004,"cost_source":"reported","tokens_reported":true});
        record(&app, &bot, &run, &receipt).unwrap();
        record(&app, &bot, &run, &receipt).unwrap();
        let mut second = unknown;
        second["request_id"] = json!(2);
        record(&app, &bot, &run, &second).unwrap();
        let Json(result) = usage(State(app.clone()), Path("openrouter".into()))
            .await
            .ok()
            .unwrap();
        let row = &result["bots"][0];
        assert_eq!(row["requests"], 2);
        assert_eq!(row["input_tokens"], 120);
        assert_eq!(row["reported_cost"], 0.004);
        assert_eq!(row["unpriced_requests"], 1);
        assert_eq!(row["unreported_tokens"], 1);
        let Json(other) = usage(State(app), Path("claude-code".into()))
            .await
            .ok()
            .unwrap();
        assert!(other["bots"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn configured_keyless_provider_runs_through_pi_and_records_model_usage() {
        use axum::{Router, http::HeaderMap, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let router=Router::new().route("/v1/chat/completions",post(move|headers:HeaderMap,Json(body):Json<Value>|{let counter=counter.clone();async move{
            assert!(headers.get("authorization").is_none());assert_eq!(body["model"],"test/model");assert!(body["provider"].is_null());
            let index=counter.fetch_add(1,Ordering::SeqCst);
            crate::pi::sse_response(if index==0 {json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"custom-memory","type":"function","function":{"name":"remember","arguments":"{\"text\":\"Custom provider memory\"}"}}]},"finish_reason":"tool_calls"}]})}else{json!({"choices":[{"message":{"role":"assistant","content":"Custom provider completed."},"finish_reason":"stop"}]})})
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let mut app = crate::tests::app();
        {
            let config = &mut Arc::get_mut(&mut app).unwrap().config.pi;
            config.node_binary =
                std::env::var("KINDRED_PI_TEST_NODE").unwrap_or_else(|_| "node".into());
            config.worker_script = std::env::var("KINDRED_PI_TEST_WORKER").unwrap_or_else(|_| {
                format!("{}/harness/pi/worker.mjs", env!("CARGO_MANIFEST_DIR"))
            });
        }
        let id = format!("custom-{}", db::id());
        let mut bot = crate::tests::bot(&app.db, "openrouter");
        bot.provider = id.clone();
        bot.model = "test/model".into();
        bot.reasoning_effort.clear();
        app.db.save_bot(&bot).unwrap();
        app.db.save_setting("custom_providers",&json!([{"id":id,"name":"Keyless fixture","base_url":url,"no_auth":true,"models":[{"model":"test/model","context_window":32000}]}])).unwrap();
        app.db.queue(&bot.id, "Remember a fact", 0).unwrap();
        let task = app.db.claim().unwrap().unwrap();
        assert_eq!(
            run(&app, &bot, &task).await.unwrap(),
            "Custom provider completed."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            app.db
                .bot(&bot.id)
                .unwrap()
                .memory
                .contains("Custom provider memory")
        );
        let Json(usage) = usage(State(app), Path(id)).await.ok().unwrap();
        assert_eq!(usage["bots"][0]["requests"], 2);
        assert_eq!(usage["bots"][0]["unreported_tokens"], 2);
        assert_eq!(usage["bots"][0]["unpriced_requests"], 2);
        server.abort();
    }
}
