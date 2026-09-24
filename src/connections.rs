use crate::runtime::App;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf};

fn key_path(app: &App) -> PathBuf {
    PathBuf::from(format!("{}.credentials", app.config.database))
}
pub fn openrouter_key(app: &App) -> Option<String> {
    credential(app, "openrouter", &app.config.openrouter.api_key_env)
}
pub fn credential(app: &App, name: &str, environment: &str) -> Option<String> {
    if let Ok(bytes) = std::fs::read(key_path(app)) {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(key) = value[name].as_str().filter(|k| !k.is_empty()) {
                return Some(key.into());
            }
        }
    }
    // A process-wide credential belongs to the legacy workspace, never a tenant.
    if app.config.profiles.enabled && !app.config.vm.managed_id.is_empty() {
        return None;
    }
    std::env::var(environment).ok().filter(|k| !k.is_empty())
}
pub fn save_openrouter(app: &App, key: &str) -> Result<()> {
    save_credential(app, "openrouter", &app.config.openrouter.api_key_env, key)
}
pub fn save_credential(app: &App, name: &str, environment: &str, key: &str) -> Result<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("Credential store unavailable"))?;
    ensure!(
        app.config.database != ":memory:",
        "Provider keys require a persistent server database"
    );
    ensure!(
        key.len() <= 512 && key.bytes().all(|b| b.is_ascii_graphic()),
        "Invalid provider key"
    );
    ensure!(
        key.is_empty() || key.len() >= 8,
        "Provider key is too short"
    );
    if key.is_empty() && app.config.vm.managed_id.is_empty() && std::env::var(environment).is_ok() {
        anyhow::bail!(
            "This server also supplies this key through its service environment; remove that key there to disconnect completely."
        );
    }
    let path = key_path(app);
    let mut keys: serde_json::Map<String, Value> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(e.into()),
    };
    keys.insert(name.into(), json!(key));
    let temporary = path.with_extension(format!("credentials-{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(serde_json::to_vec(&keys)?.as_slice())?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}
pub fn default_apps() -> Value {
    json!([])
}
