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
    save_credential_at(key_path(app), name, key)
}
pub(crate) fn save_credential_at(path: PathBuf, name: &str, key: &str) -> Result<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("Credential store unavailable"))?;
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

fn decisions_path(app: &App) -> Result<PathBuf> {
    if let Some((portal, profile)) = app
        .profile_portal
        .get()
        .or_else(|| app.decisions_account.get())
    {
        let portal = portal
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Account unavailable"))?;
        match portal.account_credential_path(profile) {
            Ok(path) => return Ok(path),
            Err(_) if profile == "legacy" && !app.account_disabled() => return Ok(key_path(app)),
            Err(error) => return Err(error),
        }
    }
    ensure!(app.config.vm.managed_id.is_empty(), "Account unavailable");
    ensure!(
        app.config.database != ":memory:",
        "Decisions keys require a persistent server database"
    );
    Ok(key_path(app))
}
pub fn decisions_key(app: &App) -> Option<String> {
    decisions_credential(app).map(|(key, _)| key)
}
fn decisions_credential(app: &App) -> Option<(String, &'static str)> {
    let path = decisions_path(app).ok()?;
    match std::fs::read(path) {
        Ok(bytes) => {
            let v: Value = serde_json::from_slice(&bytes).ok()?;
            if let Some(key) = v["decisions"].as_str() {
                return crate::browser_use::transport::OpenAiDecisions::valid_key(key)
                    .then(|| (key.into(), "saved"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    // A claimed legacy workspace's saved key belongs only to its owning account.
    // A primary empty tombstone already returned None above, so removal wins.
    if let Some((portal, profile)) = app
        .profile_portal
        .get()
        .or_else(|| app.decisions_account.get())
    {
        if let Some(path) = portal
            .upgrade()
            .and_then(|p| p.legacy_credential_path(profile).ok().flatten())
        {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                    if let Some(key) = v["decisions"].as_str().filter(|key| {
                        crate::browser_use::transport::OpenAiDecisions::valid_key(key)
                    }) {
                        return Some((key.into(), "saved"));
                    }
                }
            }
        }
    }
    // Environment configuration is only for the legacy workspace, never tenants.
    if !app.config.vm.managed_id.is_empty()
        || app.profile_portal.get().is_some()
        || !app.config.decisions.enabled
    {
        return None;
    }
    let key = std::env::var(&app.config.decisions.api_key_env).ok()?;
    crate::browser_use::transport::OpenAiDecisions::valid_key(&key).then_some((key, "environment"))
}
pub fn decisions_status(app: &App) -> Value {
    let credential = decisions_credential(app);
    let fingerprint = credential.as_ref().map(|(key, _)| {
        json!(
            ring::digest::digest(&ring::digest::SHA256, key.as_bytes())
                .as_ref()
                .to_vec()
        )
    });
    let last_error = app
        .db
        .setting("decisions_last_use")
        .ok()
        .flatten()
        .filter(|v| fingerprint.as_ref() == Some(&v["fingerprint"]))
        .and_then(|v| v["error"].as_str().map(str::to_string));
    let account = app
        .profile_portal
        .get()
        .or_else(|| app.decisions_account.get())
        .is_some_and(|(p, id)| {
            p.upgrade()
                .is_some_and(|p| p.account_credential_path(id).is_ok())
        });
    json!({"configured":credential.is_some(),"source":credential.as_ref().map(|(_,source)|*source).unwrap_or("none"),"scope":if account {"account"}else{"workspace"},"access_verified":false,"last_error":last_error})
}
pub fn save_decisions(app: &App, key: Option<&str>) -> Result<()> {
    if let Some(key) = key {
        ensure!(
            crate::browser_use::transport::OpenAiDecisions::valid_key(key) && key.len() <= 512,
            "Enter a valid OpenAI API key. Blank does not remove the saved key."
        );
    }
    // An explicit empty tombstone prevents environment fallback after removal.
    save_credential_at(decisions_path(app)?, "decisions", key.unwrap_or("")).map_err(|_| {
        anyhow::anyhow!("Could not save the Decisions key. The previous key was retained.")
    })
}
