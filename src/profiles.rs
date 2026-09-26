//! Account and profile boundary. The existing application runs with one private
//! database and computer per profile. Server chat routes explicitly bridge only
//! membership-scoped conversation content above those private workspaces.
use crate::{
    config::Config,
    db,
    runtime::{App, Shared},
    web,
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ring::{digest, pbkdf2};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

#[path = "server_chats.rs"]
mod server_chats;

type Portal = Arc<Profiles>;
type ApiResult = std::result::Result<Json<Value>, web::Error>;
const SESSION_SECONDS: i64 = 90 * 86400;
#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
macro_rules! ensure {
    ($condition:expr, $($message:tt)*) => {
        if !$condition { return Err(anyhow::anyhow!($($message)*).into()); }
    };
}

pub struct Profiles {
    self_ref: std::sync::OnceLock<std::sync::Weak<Profiles>>,
    config: Config,
    registry: Mutex<Connection>,
    apps: Mutex<HashMap<String, Shared>>,
    legacy: Option<Shared>,
    password_slots: Arc<tokio::sync::Semaphore>,
    attempts: Mutex<HashMap<String, (i64, usize)>>,
    schedulers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    run_schedulers: bool,
    shared_lock: Mutex<()>,
}

#[derive(Clone)]
struct Identity {
    account: String,
    profile: String,
    admin: bool,
    legacy: bool,
}

fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn hash(value: &str) -> Vec<u8> {
    digest::digest(&digest::SHA256, value.as_bytes())
        .as_ref()
        .to_vec()
}
fn password_hash(password: &str, salt: &str) -> Vec<u8> {
    let mut output = vec![0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(600_000).unwrap(),
        salt.as_bytes(),
        password.as_bytes(),
        &mut output,
    );
    output
}
fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
}
fn field<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    let s = v[key].as_str().unwrap_or("").trim();
    ensure!(
        !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control),
        "Invalid {key}"
    );
    Ok(s)
}
fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl Profiles {
    pub fn open(config: Config, legacy: Option<Shared>, run_schedulers: bool) -> Result<Portal> {
        private_dir(Path::new(&config.profiles.directory))?;
        let registry = Connection::open(Path::new(&config.profiles.directory).join("accounts.db"))?;
        registry.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY, login TEXT NOT NULL UNIQUE, salt TEXT NOT NULL, password BLOB NOT NULL, admin INTEGER NOT NULL, disabled INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY, account_id TEXT NOT NULL REFERENCES accounts(id), name TEXT NOT NULL, legacy INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS profile_requests(account_id TEXT NOT NULL REFERENCES accounts(id),request_id TEXT NOT NULL,profile_id TEXT NOT NULL REFERENCES profiles(id),PRIMARY KEY(account_id,request_id));
            CREATE TABLE IF NOT EXISTS sessions(digest BLOB PRIMARY KEY, account_id TEXT NOT NULL REFERENCES accounts(id), profile_id TEXT NOT NULL REFERENCES profiles(id), expires INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS device_links(digest BLOB PRIMARY KEY, account_id TEXT NOT NULL REFERENCES accounts(id), profile_id TEXT NOT NULL REFERENCES profiles(id), expires INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS directory(account_id TEXT NOT NULL REFERENCES accounts(id), server TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(account_id,server));
            CREATE TABLE IF NOT EXISTS invites(digest BLOB PRIMARY KEY, expires INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS controls(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS sessions_expiry ON sessions(expires);")?;
        server_chats::migrate(&registry)?;
        let portal = Arc::new(Self {
            self_ref: Default::default(),
            config,
            registry: Mutex::new(registry),
            apps: Default::default(),
            legacy: legacy.clone(),
            password_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            attempts: Default::default(),
            schedulers: Default::default(),
            run_schedulers,
            shared_lock: Mutex::new(()),
        });
        let _ = portal.self_ref.set(Arc::downgrade(&portal));
        if let Some(app) = legacy {
            portal.apps.lock().unwrap().insert("legacy".into(), app);
        }
        let ids: Vec<String> = portal
            .registry
            .lock()
            .unwrap()
            .prepare("SELECT id FROM profiles")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            portal.app(&id)?;
        }
        if run_schedulers {
            portal
                .schedulers
                .lock()
                .unwrap()
                .push(tokio::spawn(server_chats::worker(Arc::downgrade(&portal))));
        }
        Ok(portal)
    }

    fn app(&self, id: &str) -> Result<Shared> {
        let mut apps = self.apps.lock().unwrap();
        if let Some(app) = apps.get(id) {
            return Ok(app.clone());
        }
        ensure!(uuid::Uuid::parse_str(id).is_ok(), "Unknown profile");
        let (name,disabled): (String,bool) = self.registry.lock().unwrap().query_row("SELECT p.name,a.disabled FROM profiles p JOIN accounts a ON a.id=p.account_id WHERE p.id=?", [id], |r|Ok((r.get(0)?,r.get(1)?)))?;
        let root = PathBuf::from(&self.config.profiles.directory).join(id);
        private_dir(&root)?;
        let mut config = self.config.clone();
        config.database = root.join("kindred.db").to_string_lossy().into_owned();
        config.vm.managed_id = id.into();
        config.vm.manager = config.profiles.vm_manager.clone();
        config.vm.domain = format!("kindred-{id}");
        config.vm.ssh_alias = "kindred-guest".into();
        config.vm.ssh_config = root
            .join("computer/ssh_config")
            .to_string_lossy()
            .into_owned();
        config.vm.control_helper.clear();
        let app = App::open(config, secret())?;
        let _ = app.profile_portal.set((self.self_ref.get().cloned().unwrap_or_default(),id.into()));
        app.db.save_setting("_account_disabled", &json!(disabled))?;
        if app.db.setting("general")?.is_none() {
            app.db
                .save_setting("general", &db::general_settings(Some(json!({"name":name}))))?;
        }
        if self.run_schedulers {
            self.schedulers
                .lock()
                .unwrap()
                .push(tokio::spawn(crate::runtime::scheduler(app.clone())));
            tokio::spawn(crate::provider_catalog::startup(app.clone()));
        }
        apps.insert(id.into(), app.clone());
        Ok(app)
    }

    fn identity(&self, token: &str) -> Result<Identity> {
        if let Some(app) = &self.legacy {
            if web::valid_token(&app.token, token) {
                // Legacy devices retain only their original profile. Upgrading to an
                // account requires the owner's password, including after initial claim.
                return Ok(Identity {
                    account: String::new(),
                    profile: "legacy".into(),
                    admin: false,
                    legacy: true,
                });
            }
        }
        ensure!(
            (32..=256).contains(&token.len()),
            "Sign in to your Kindred account"
        );
        let c = self.registry.lock().unwrap();
        let row = c.query_row("SELECT s.account_id,s.profile_id,a.admin FROM sessions s JOIN accounts a ON a.id=s.account_id JOIN profiles p ON p.id=s.profile_id AND p.account_id=a.id WHERE s.digest=? AND s.expires>? AND a.disabled=0", params![hash(token),db::now()], |r| Ok(Identity { account:r.get(0)?, profile:r.get(1)?, admin:r.get(2)?, legacy:false })).optional()?;
        row.ok_or_else(|| anyhow::anyhow!("Your session expired. Sign in again."))
    }
    fn session(c: &Connection, account: &str, profile: &str) -> Result<String> {
        c.execute("DELETE FROM sessions WHERE expires<=?", [db::now()])?;
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE account_id=?",
            [account],
            |r| r.get(0),
        )?;
        ensure!(
            count < 128,
            "Too many signed-in devices. Sign out of an old device first."
        );
        let token = secret();
        c.execute(
            "INSERT INTO sessions VALUES(?,?,?,?)",
            params![hash(&token), account, profile, db::now() + SESSION_SECONDS],
        )?;
        Ok(token)
    }
    fn registration_open(&self, c: &Connection) -> Result<bool> {
        Ok(c.query_row(
            "SELECT value FROM controls WHERE key='registration'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(|v| v == "open")
        .unwrap_or(self.config.profiles.open_registration))
    }
    fn origin(&self, headers: &HeaderMap) -> Result<()> {
        if let Some(origin) = headers.get(header::ORIGIN) {
            ensure!(
                origin
                    .to_str()
                    .ok()
                    .is_some_and(|o| self.config.allows_origin(o)),
                "Open this page on your Kindred server"
            );
        }
        Ok(())
    }
    fn rate_limit(&self, key: &str) -> Result<()> {
        let now = db::now();
        let mut rates = self.attempts.lock().unwrap();
        rates.retain(|_, (start, _)| *start + 300 > now);
        ensure!(
            rates.len() < 4096 || rates.contains_key(key),
            "Sign-in is busy. Try again in a few minutes."
        );
        let entry = rates.entry(key.to_owned()).or_insert((now, 0));
        entry.1 += 1;
        ensure!(
            entry.1 <= 20,
            "Too many sign-in attempts. Try again in five minutes."
        );
        Ok(())
    }
    fn projection(&self, id: &Identity) -> Result<Value> {
        if id.legacy {
            let app = self.app("legacy")?;
            let name = app.db.setting("general")?.unwrap_or(json!({}))["name"]
                .as_str()
                .unwrap_or("Personal")
                .to_owned();
            let claimed: bool = self.registry.lock().unwrap().query_row(
                "SELECT EXISTS(SELECT 1 FROM profiles WHERE id='legacy')",
                [],
                |r| r.get(0),
            )?;
            return Ok(
                json!({"active":"legacy","legacy":true,"claim_available":!claimed,"profiles":[{"id":"legacy","name":name,"active":true,"unread":self.unread(&app)?}],"admin":false}),
            );
        }
        let rows: Vec<(String, String)> = self
            .registry
            .lock()
            .unwrap()
            .prepare("SELECT id,name FROM profiles WHERE account_id=? ORDER BY created,rowid")?
            .query_map([&id.account], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let shared_unread = server_chats::unread(self, &id.account)?;
        let mut profiles = Vec::new();
        for (profile, name) in rows {
            let app = self.app(&profile)?;
            let name = app
                .db
                .setting("general")?
                .and_then(|v| v["name"].as_str().map(str::to_owned))
                .unwrap_or(name);
            profiles.push(json!({"id":profile,"name":name,"active":profile==id.profile,"unread":self.unread(&app)?+shared_unread}));
        }
        let directory: Vec<Value> = self
            .registry
            .lock()
            .unwrap()
            .prepare("SELECT server,name FROM directory WHERE account_id=? ORDER BY name")?
            .query_map([&id.account], |r| {
                Ok(json!({"server":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let username: String = self.registry.lock().unwrap().query_row(
            "SELECT login FROM accounts WHERE id=?",
            [&id.account],
            |r| r.get(0),
        )?;
        Ok(
            json!({"active":id.profile,"account_id":id.account,"username":username,"admin":id.admin,"profiles":profiles,"directory":directory,"legacy":false}),
        )
    }
    fn unread(&self, app: &Shared) -> Result<i64> {
        // Count unread assistant messages, including input-needed cards, with the
        // same persisted cursors used by chat. Opening the switcher never marks read.
        let c = app.db.0.lock().unwrap();
        Ok(c.query_row("SELECT COUNT(*) FROM chat_messages m JOIN chats c ON c.id=m.chat_id LEFT JOIN chat_reads r ON r.chat_id=c.id WHERE c.archived=0 AND c.id NOT LIKE 'server-%' AND m.suppressed=0 AND m.seq>COALESCE(r.message_seq,0) AND m.sender NOT IN ('user','system') AND trim(m.body)!=''",[],|r|r.get(0))?)
    }
}
impl Drop for Profiles {
    fn drop(&mut self) {
        for handle in self.schedulers.lock().unwrap().drain(..) {
            handle.abort();
        }
    }
}

pub fn router(portal: Portal) -> Router {
    Router::new()
        .merge(server_chats::routes())
        .route("/identity/meta", get(meta))
        .route("/identity/register", post(register))
        .route("/identity/login", post(login))
        .route("/identity/logout", post(logout))
        .route("/identity/profiles", get(list).post(create))
        .route("/identity/switch", post(switch))
        .route("/identity/profile", post(rename))
        .route(
            "/identity/transfer",
            get(transfer_status).post(transfer_prepare),
        )
        .route("/identity/transfer/cancel", post(transfer_cancel))
        .route("/identity/transfer/finish", post(transfer_finish))
        .route(
            "/identity/transfer/import",
            post(transfer_import).layer(DefaultBodyLimit::max(crate::workspace_transfer::LIMIT)),
        )
        .route("/identity/directory", post(directory))
        .route("/identity/password", post(change_password))
        .route("/identity/admin", get(admin).post(admin_update))
        .route("/identity/server-update", get(server_update_status).post(server_update_start))
        .route("/identity/computer-settings", get(computer_settings).post(save_computer_settings))
        .route("/device/claim", post(claim_device))
        .route("/api/devices/link", post(link_device))
        .route(
            "/hooks/gmail/{id}",
            post(gmail_push).layer(DefaultBodyLimit::max(65536)),
        )
        .fallback(dispatch)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::from_fn(web::headers))
        .with_state(portal)
}
async fn gmail_push(
    State(p): State<Portal>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if uuid::Uuid::parse_str(&id).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let app = p
        .apps
        .lock()
        .unwrap()
        .values()
        .find(|app| crate::mail_watch::find_watch(app, &id))
        .cloned();
    let Some(app) = app else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match crate::mail_watch::receive(&app, &id, &headers, &body).await {
        Ok(status) => status.into_response(),
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}
async fn meta(State(p): State<Portal>) -> ApiResult {
    let c = p.registry.lock().unwrap();
    let first: bool = c.query_row("SELECT NOT EXISTS(SELECT 1 FROM accounts)", [], |r| {
        r.get(0)
    })?;
    Ok(Json(
        json!({"profiles":true,"server_chats":true,"first_user":first,"legacy_claim":first&&p.legacy.is_some(),"registration":p.registration_open(&c)?,"version":env!("CARGO_PKG_VERSION")}),
    ))
}
async fn register(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let login = field(&v, "login", 80)?.to_lowercase();
    ensure!(
        login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)),
        "Use letters, numbers, dots, hyphens or an email address for your username"
    );
    let password = field(&v, "password", 1024)?.to_owned();
    ensure!(
        password.chars().count() >= 4,
        "Use a password of at least 4 characters"
    );
    let name = field(&v, "name", 80)?.to_owned();
    p.rate_limit(&format!("register:{login}"))?;
    let permit = p
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("Sign-in is busy. Try again shortly."))?;
    let salt = secret();
    let copy = salt.clone();
    let digest = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        password_hash(&password, &copy)
    })
    .await?;
    let (token, profile) = {
        let mut c = p.registry.lock().unwrap();
        let tx = c.transaction()?;
        let count: usize = tx.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
        ensure!(
            count < p.config.profiles.max_users,
            "This server has reached its user limit"
        );
        let first = count == 0;
        let invite = v["invite"].as_str().unwrap_or("");
        if !first && !p.registration_open(&tx)? {
            ensure!(
                tx.execute(
                    "DELETE FROM invites WHERE digest=? AND expires>?",
                    params![hash(invite), db::now()]
                )? == 1,
                "Ask your server administrator for an invitation"
            );
        }
        let import = first && p.legacy.is_some();
        if import {
            ensure!(
                v["claim_legacy"] == true,
                "Confirm that this account will own the existing workspace, including its bots and conversations"
            );
            ensure!(
                p.legacy
                    .as_ref()
                    .is_some_and(|app| web::valid_token(&app.token, bearer(&headers))),
                "Connect to your existing workspace before creating its owner account"
            );
        }
        let account = db::id();
        let profile = if import {
            "legacy".to_owned()
        } else {
            db::id()
        };
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE login=?)",
                [&login],
                |r| r.get::<_, bool>(0)
            )?,
            "That username is already registered"
        );
        tx.execute(
            "INSERT INTO accounts(id,login,salt,password,admin,created) VALUES(?,?,?,?,?,?)",
            params![account, login, salt, digest, first, db::now()],
        )?;
        tx.execute(
            "INSERT INTO profiles VALUES(?,?,?,?,?)",
            params![profile, account, name, import, db::now()],
        )?;
        if let Some(request) = v["request_id"].as_str() {
            ensure!(
                uuid::Uuid::parse_str(request).is_ok(),
                "Invalid profile creation request"
            );
            tx.execute(
                "INSERT INTO profile_requests VALUES(?,?,?)",
                params![account, request, profile],
            )?;
        }
        let token = Profiles::session(&tx, &account, &profile)?;
        tx.commit()?;
        (token, profile)
    };
    let app = p.app(&profile)?;
    if profile == "legacy" {
        let mut general = app.db.setting("general")?.unwrap_or(json!({}));
        general["name"] = v["name"].clone();
        app.db.save_setting("general", &general)?;
    }
    Ok(Json(json!({"token":token,"profile_id":profile})))
}
async fn login(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let login = field(&v, "login", 80)?.to_lowercase();
    let password = field(&v, "password", 1024)?.to_owned();
    p.rate_limit(&format!("login:{login}"))?;
    let row: Option<(String, String, Vec<u8>, bool)> = p
        .registry
        .lock()
        .unwrap()
        .query_row(
            "SELECT id,salt,password,disabled FROM accounts WHERE login=?",
            [login],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (account, salt, digest, disabled) = row.unwrap_or((
        String::new(),
        "invalid-account-salt".into(),
        vec![0; 32],
        true,
    ));
    let permit = p
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("Sign-in is busy. Try again shortly."))?;
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        pbkdf2::verify(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(600_000).unwrap(),
            salt.as_bytes(),
            password.as_bytes(),
            &digest,
        )
        .is_ok()
    })
    .await?;
    ensure!(valid && !disabled, "Username or password was not accepted");
    let (token, profile) = {
        let mut c = p.registry.lock().unwrap();
        let tx = c.transaction()?;
        let profile = if v["new_profile_name"].is_string() {
            insert_profile(
                &p,
                &tx,
                &account,
                field(&v, "new_profile_name", 80)?,
                v["request_id"].as_str(),
            )?
        } else {
            let requested = v["profile_id"].as_str().unwrap_or("");
            tx.query_row("SELECT id FROM profiles WHERE account_id=? ORDER BY (id=?) DESC,created,rowid LIMIT 1",params![&account,requested],|r|r.get::<_,String>(0))?
        };
        let token = Profiles::session(&tx, &account, &profile)?;
        tx.commit()?;
        (token, profile)
    };
    p.app(&profile)?;
    Ok(Json(json!({"token":token,"profile_id":profile})))
}
async fn list(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    Ok(Json(p.projection(&id)?))
}
fn insert_profile(
    p: &Profiles,
    c: &Connection,
    account: &str,
    name: &str,
    request: Option<&str>,
) -> Result<String> {
    if let Some(request) = request {
        ensure!(
            uuid::Uuid::parse_str(request).is_ok(),
            "Invalid profile creation request"
        );
        if let Some(id) = c
            .query_row(
                "SELECT profile_id FROM profile_requests WHERE account_id=? AND request_id=?",
                params![account, request],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
    }
    let count: usize = c.query_row(
        "SELECT COUNT(*) FROM profiles WHERE account_id=?",
        [account],
        |r| r.get(0),
    )?;
    ensure!(
        count < p.config.profiles.max_profiles_per_user,
        "Your account has reached this server's profile limit"
    );
    let profile = db::id();
    c.execute(
        "INSERT INTO profiles VALUES(?,?,?,0,?)",
        params![profile, account, name, db::now()],
    )?;
    if let Some(request) = request {
        c.execute(
            "INSERT INTO profile_requests VALUES(?,?,?)",
            params![account, request, profile],
        )?;
    }
    Ok(profile)
}
async fn create(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Create or sign in to your owner account first");
    let name = field(&v, "name", 80)?;
    let profile = {
        let mut c = p.registry.lock().unwrap();
        let tx = c.transaction()?;
        let profile = insert_profile(&p, &tx, &id.account, name, v["request_id"].as_str())?;
        tx.commit()?;
        profile
    };
    p.app(&profile)?;
    Ok(Json(json!({"id":profile,"name":name})))
}
async fn rename(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(
        !id.legacy,
        "Sign in to your account before editing a profile"
    );
    let name = field(&v, "name", 80)?;
    let app = p.app(&id.profile)?;
    ensure!(
        app.db.transfer_status()?.is_null(),
        "Finish or cancel the workspace transfer before renaming this profile"
    );
    let mut general = app.db.setting("general")?.unwrap_or(json!({}));
    general["name"] = json!(name);
    let mut c = p.registry.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        tx.execute(
            "UPDATE profiles SET name=? WHERE id=? AND account_id=?",
            params![name, id.profile, id.account]
        )? == 1,
        "Profile unavailable"
    );
    app.db.save_setting("general", &general)?;
    tx.commit()?;
    Ok(Json(json!({"id":id.profile,"name":name})))
}
async fn switch(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Sign in to switch profiles");
    let profile = field(&v, "profile_id", 64)?;
    let c = p.registry.lock().unwrap();
    ensure!(
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE id=? AND account_id=?)",
            params![profile, id.account],
            |r| r.get::<_, bool>(0)
        )?,
        "Profile unavailable"
    );
    // Switching rotates the device's session instead of accumulating tokens.
    let token = Profiles::session(&c, &id.account, profile)?;
    c.execute(
        "DELETE FROM sessions WHERE digest=?",
        [hash(bearer(&headers))],
    )?;
    Ok(Json(json!({"token":token,"profile_id":profile})))
}
async fn logout(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(
        !id.legacy,
        "Legacy access tokens are managed by the server owner"
    );
    let c = p.registry.lock().unwrap();
    if v["all_devices"] == true {
        c.execute("DELETE FROM sessions WHERE account_id=?", [&id.account])?;
    } else {
        c.execute(
            "DELETE FROM sessions WHERE digest=?",
            [hash(bearer(&headers))],
        )?;
    }
    Ok(Json(json!({"signed_out":true})))
}
async fn directory(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Sign in first");
    let name = field(&v, "name", 80)?;
    let server = field(&v, "server", 512)?;
    let mut check = p.config.clone();
    check.public_url = server.into();
    check.allowed_origins.clear();
    check.validate()?;
    let c = p.registry.lock().unwrap();
    if v["remove"] == true {
        c.execute(
            "DELETE FROM directory WHERE account_id=? AND server=?",
            params![id.account, server.trim_end_matches('/')],
        )?;
    } else {
        let count: usize = c.query_row(
            "SELECT COUNT(*) FROM directory WHERE account_id=?",
            [&id.account],
            |r| r.get(0),
        )?;
        ensure!(count < 32, "At most 32 server bookmarks can be synced");
        c.execute("INSERT INTO directory VALUES(?,?,?) ON CONFLICT(account_id,server) DO UPDATE SET name=excluded.name",params![id.account,server.trim_end_matches('/'),name])?;
    }
    Ok(Json(json!({"saved":true})))
}
async fn change_password(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Sign in first");
    p.rate_limit(&format!("password:{}", id.account))?;
    let old = field(&v, "current_password", 1024)?.to_owned();
    let new = field(&v, "password", 1024)?.to_owned();
    ensure!(
        new.chars().count() >= 4,
        "Use a password of at least 4 characters"
    );
    let (old_salt, old_hash): (String, Vec<u8>) = p.registry.lock().unwrap().query_row(
        "SELECT salt,password FROM accounts WHERE id=?",
        [&id.account],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let permit = p
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("Sign-in is busy"))?;
    let salt = secret();
    let copy = salt.clone();
    let expected = old_hash.clone();
    let digest = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let _permit = permit;
        ensure!(
            pbkdf2::verify(
                pbkdf2::PBKDF2_HMAC_SHA256,
                NonZeroU32::new(600_000).unwrap(),
                old_salt.as_bytes(),
                old.as_bytes(),
                &expected
            )
            .is_ok(),
            "Current password was not accepted"
        );
        Ok(password_hash(&new, &copy))
    })
    .await??;
    let mut c = p.registry.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        tx.execute(
            "UPDATE accounts SET salt=?,password=? WHERE id=? AND password=?",
            params![salt, digest, id.account, old_hash]
        )? == 1,
        "Password changed on another device. Sign in again."
    );
    tx.execute("DELETE FROM sessions WHERE account_id=?", [&id.account])?;
    tx.execute("DELETE FROM device_links WHERE account_id=?", [&id.account])?;
    let token = Profiles::session(&tx, &id.account, &id.profile)?;
    tx.commit()?;
    Ok(Json(json!({"token":token,"profile_id":id.profile})))
}
async fn link_device(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    if id.legacy {
        return crate::pairing::issue(State(p.app("legacy")?)).await;
    }
    let c = p.registry.lock().unwrap();
    c.execute("DELETE FROM device_links WHERE expires<=?", [db::now()])?;
    let count: usize = c.query_row(
        "SELECT COUNT(*) FROM device_links WHERE account_id=?",
        [&id.account],
        |r| r.get(0),
    )?;
    ensure!(count < 8, "Eight device links are already pending");
    let code = secret();
    let expires = db::now() + 600;
    c.execute(
        "INSERT INTO device_links VALUES(?,?,?,?)",
        params![hash(&code), id.account, id.profile, expires],
    )?;
    Ok(Json(
        json!({"url":format!("{}/#pair={code}",p.config.public_url.trim_end_matches('/')),"expires_at":expires}),
    ))
}
async fn claim_device(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    ensure!(
        headers.contains_key(header::ORIGIN),
        "Open the device link in your browser"
    );
    let code = field(&v, "code", 64)?;
    let token = {
        let mut c = p.registry.lock().unwrap();
        let tx = c.transaction()?;
        let row:Option<(String,String)>=tx.query_row("SELECT d.account_id,d.profile_id FROM device_links d JOIN accounts a ON a.id=d.account_id WHERE d.digest=? AND d.expires>? AND a.disabled=0",params![hash(code),db::now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((account, profile)) = row {
            tx.execute("DELETE FROM device_links WHERE digest=?", [hash(code)])?;
            let token = Profiles::session(&tx, &account, &profile)?;
            tx.commit()?;
            Some(token)
        } else {
            None
        }
    };
    if let Some(token) = token {
        return Ok(Json(json!({"token":token})));
    }
    if let Some(app) = &p.legacy {
        return crate::pairing::claim(State(app.clone()), headers, Json(v)).await;
    }
    Err(
        anyhow::anyhow!("This link expired or was already used. Create a fresh device link.")
            .into(),
    )
}
async fn computer_settings(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(id.admin, "Administrator access required");
    let app = p.app(&id.profile)?;
    if app.config.vm.managed_id.is_empty() {
        return Ok(Json(json!({"supported":false})));
    }
    Ok(Json(
        crate::vm::managed(&app.config.vm, "resource-settings", 20).await?,
    ))
}
async fn save_computer_settings(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(id.admin, "Administrator access required");
    let app = p.app(&id.profile)?;
    web::computer_ready(&app)?;
    let mut leases = Vec::new();
    for slot in 1..=32 {
        leases.push(
            app.screen_lock(slot)
                .try_lock_owned()
                .map_err(|_| anyhow::anyhow!("A screen is in use. Stop active tasks first."))?,
        );
    }
    Ok(Json(crate::vm::resize_resources(&app.config.vm, v).await?))
}

async fn server_update_status(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    ensure!(p.identity(bearer(&headers))?.admin, "Administrator access required");
    Ok(Json(crate::server_update::request("status", None).await?))
}
async fn server_update_start(State(p): State<Portal>, headers: HeaderMap, Json(body): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    ensure!(p.identity(bearer(&headers))?.admin, "Administrator access required");
    let version=body["version"].as_str().unwrap_or("");
    ensure!(!version.is_empty() && version.len()<32 && version.split('.').count()==3 && version.split('.').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())), "Invalid release version");
    Ok(Json(crate::server_update::request("start", Some(version)).await?))
}

async fn admin(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(id.admin, "Administrator access required");
    let c = p.registry.lock().unwrap();
    let users:Vec<Value>=c.prepare("SELECT id,login,admin,disabled,(SELECT COUNT(*) FROM profiles p WHERE p.account_id=a.id) FROM accounts a ORDER BY created,rowid")?.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"username":r.get::<_,String>(1)?,"admin":r.get::<_,bool>(2)?,"disabled":r.get::<_,bool>(3)?,"profile_count":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<_>>()?;
    Ok(Json(
        json!({"users":users,"registration":p.registration_open(&c)?,"max_users":p.config.profiles.max_users,"max_profiles_per_user":p.config.profiles.max_profiles_per_user}),
    ))
}
async fn admin_update(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(id.admin, "Administrator access required");
    let mut c = p.registry.lock().unwrap();
    let tx = c.transaction()?;
    match v["action"].as_str().unwrap_or("") {
        "registration" => {
            let value = if v["open"] == true { "open" } else { "closed" };
            tx.execute("INSERT INTO controls VALUES('registration',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[value])?;
        }
        "invite" => {
            let code = secret();
            let expiry = db::now() + 86400;
            tx.execute("DELETE FROM invites WHERE expires<=?", [db::now()])?;
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM invites", [], |r| r.get(0))?;
            ensure!(count < 128, "Too many pending invitations");
            tx.execute(
                "INSERT INTO invites VALUES(?,?)",
                params![hash(&code), expiry],
            )?;
            tx.commit()?;
            return Ok(Json(json!({"invite":code,"expires_at":expiry})));
        }
        "disable" => {
            let user = field(&v, "user_id", 64)?;
            ensure!(user != id.account, "You cannot disable your own account");
            ensure!(
                tx.execute(
                    "UPDATE accounts SET disabled=? WHERE id=?",
                    params![v["disabled"] == true, user]
                )? == 1,
                "User unavailable"
            );
            tx.execute("DELETE FROM sessions WHERE account_id=?", [user])?;
            tx.execute("DELETE FROM device_links WHERE account_id=?", [user])?;
        }
        _ => return Err(anyhow::anyhow!("Unknown administrator action").into()),
    }
    let affected: Vec<String> = if v["action"] == "disable" {
        tx.prepare("SELECT id FROM profiles WHERE account_id=?")?
            .query_map([v["user_id"].as_str().unwrap_or("")], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    tx.commit()?;
    drop(c);
    for profile in affected {
        let app = p.app(&profile)?;
        app.db.save_setting("_account_disabled", &v["disabled"])?;
        if v["disabled"] == true {
            app.vnc.close_all();
            let runs:Vec<String>=app.db.0.lock().unwrap().prepare("SELECT id FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval')")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for run in runs {
                app.db.cancel(&run)?;
            }
            if p.run_schedulers && !app.config.vm.managed_id.is_empty() {
                let app = app.clone();
                tokio::spawn(async move {
                    let _ = crate::vm::control(&app.config.vm, "shutdown").await;
                });
            }
        }
    }
    Ok(Json(json!({"saved":true})))
}

async fn transfer_status(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(
        !id.legacy,
        "Sign in to the account that owns this workspace first"
    );
    let app = p.app(&id.profile)?;
    Ok(Json(app.db.transfer_status()?))
}
async fn transfer_prepare(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(
        !id.legacy,
        "Sign in to the account that owns this workspace first"
    );
    let app = p.app(&id.profile)?;
    let name = app
        .db
        .setting("general")?
        .and_then(|g| g["name"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "Kindred".into());
    Ok(Json(app.db.prepare_transfer(field(&v, "id", 64)?, &name)?))
}
async fn transfer_cancel(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Sign in first");
    p.app(&id.profile)?
        .db
        .cancel_transfer(field(&v, "id", 64)?)?;
    Ok(Json(json!({"cancelled":true})))
}
async fn transfer_finish(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Sign in first");
    let destination = field(&v, "destination", 2048)?;
    let url = reqwest::Url::parse(destination)?;
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
        "Invalid destination server"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "Use only the destination server address"
    );
    p.app(&id.profile)?
        .db
        .finish_transfer(field(&v, "id", 64)?, destination)?;
    Ok(Json(json!({"moved":true})))
}
async fn transfer_import(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let id = p.identity(bearer(&headers))?;
    ensure!(!id.legacy, "Sign in to a new, empty profile on this server");
    let app = p.app(&id.profile)?;
    let receipt = app.db.import_transfer(&v)?;
    let name = app
        .db
        .setting("general")?
        .and_then(|g| g["name"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "Kindred".into());
    p.registry.lock().unwrap().execute(
        "UPDATE profiles SET name=? WHERE id=? AND account_id=?",
        params![name, id.profile, id.account],
    )?;
    Ok(Json(
        json!({"receipt":receipt,"profile_id":id.profile,"name":name}),
    ))
}

async fn dispatch(State(p): State<Portal>, mut request: Request<Body>) -> Response {
    let path = request.uri().path();
    if let Some(filename) = path.strip_prefix("/updates/") {
        return web::release_asset(&p.config.database, filename.to_owned()).await;
    }
    if path.starts_with("/api/chats/server-") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let protected = path.starts_with("/api/");
    let target = if protected {
        if p.origin(request.headers()).is_err() {
            return StatusCode::FORBIDDEN.into_response();
        }
        match p
            .identity(bearer(request.headers()))
            .and_then(|id| p.app(&id.profile))
        {
            Ok(app) => app,
            Err(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error":"Sign in to your Kindred account."})),
                )
                    .into_response();
            }
        }
    } else if path == "/vnc" {
        let ticket = reqwest::Url::parse(&format!("http://localhost{}", request.uri()))
            .ok()
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "ticket")
                    .map(|(_, v)| v.into_owned())
            })
            .unwrap_or_default();
        let app = p
            .apps
            .lock()
            .unwrap()
            .values()
            .find(|app| app.vnc.has_ticket(&ticket))
            .cloned();
        match app {
            Some(app) => app,
            None => return StatusCode::UNAUTHORIZED.into_response(),
        }
    } else {
        // Assets do not need an account, database, or running VM. A separate static
        // router below prevents accidental future public routes gaining tenant state.
        return match web::assets().oneshot(request).await {
            Ok(response) => response,
            Err(e) => match e {},
        };
    };
    if protected
        && !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        )
        && target.db.transfer_status().is_ok_and(|s| !s.is_null())
    {
        return (StatusCode::CONFLICT,Json(json!({"error":"This workspace is paused for a server transfer. Open Profile settings to resume or cancel it."}))).into_response();
    }
    if protected {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", target.token).parse().unwrap(),
        );
    }
    match web::router(target).oneshot(request).await {
        Ok(response) => response,
        Err(e) => match e {},
    }
}

impl Profiles {
    pub(crate) fn bot_chat_edit(&self, profile: &str, app: &App, actor: &db::Bot, run: Option<&db::Run>, args: &Value, approved: Option<&Value>) -> Result<Value> {
        server_chats::bot_edit(self,profile,app,actor,run,args,approved)
    }
}
