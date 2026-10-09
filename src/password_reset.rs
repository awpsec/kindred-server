//! Admin-approved resets are bound to a secret held by the requesting browser.
use super::*;
#[path = "password_reset_location.rs"]
mod location;
macro_rules! ensure { ($condition:expr, $($message:tt)*) => { if !$condition { return Err(anyhow::anyhow!($($message)*).into()); } }; }

pub(super) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS password_resets(id TEXT PRIMARY KEY,digest BLOB NOT NULL UNIQUE,account TEXT REFERENCES accounts(id),created INTEGER NOT NULL,expires INTEGER NOT NULL,ip TEXT NOT NULL,state TEXT NOT NULL,decided INTEGER,decided_by TEXT);
      CREATE INDEX IF NOT EXISTS password_resets_expiry ON password_resets(expires);")?;
    if !c.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('password_resets') WHERE name='location')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        c.execute("ALTER TABLE password_resets ADD COLUMN location TEXT", [])?;
    }
    c.execute_batch("CREATE TABLE IF NOT EXISTS password_reset_audit(id INTEGER PRIMARY KEY,created INTEGER NOT NULL,source TEXT NOT NULL,actor TEXT NOT NULL,request_id TEXT,action TEXT NOT NULL,outcome TEXT NOT NULL);")?;
    Ok(())
}
// Only these metadata fields cross the logging/console boundary. JSON encoding
// prevents line injection; also remove terminal controls and bidi overrides.
fn metadata(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("://") || lower.contains("token=") || lower.contains("password=") {
        return "[redacted metadata]".into();
    }
    value.chars().filter(|c| !c.is_control() && !matches!(*c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')).take(200).collect()
}
fn incoming_record(id: &str, login: &str, now: i64, ip: &str) -> Value {
    json!({"event":"password_reset_requested","request_id":id,"username":metadata(login),"created":now,"observed_ip":metadata(ip),"state":"pending"})
}
#[derive(Clone, Copy)]
enum DecisionActor<'a> {
    LocalHost,
    WebAdmin(&'a str),
}
impl DecisionActor<'_> {
    fn source(self) -> &'static str {
        match self {
            Self::LocalHost => "local-host",
            Self::WebAdmin(_) => "web-admin",
        }
    }
    fn name(self) -> String {
        match self {
            Self::LocalHost => "local-host".into(),
            Self::WebAdmin(id) => metadata(id),
        }
    }
}
fn decide_record(
    c: &mut Connection,
    raw_id: &str,
    action: &str,
    actor: DecisionActor<'_>,
) -> Result<Value> {
    anyhow::ensure!(
        matches!(action, "approve" | "deny"),
        "Choose approve or deny"
    );
    // Do not echo an invalid CLI argument: it could accidentally be a token.
    let id = uuid::Uuid::parse_str(raw_id).ok().map(|id| id.to_string());
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Lock acquisition can wait: expiry and the decision window use the time
    // at which this transaction can actually decide the request.
    let now = db::now();
    let row: Option<(String, i64, Option<String>, Option<bool>)> = if let Some(id) = &id {
        tx.query_row("SELECT r.state,r.expires,r.account,a.disabled FROM password_resets r LEFT JOIN accounts a ON a.id=r.account WHERE r.id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?
    } else {
        None
    };
    let outcome = match row {
        _ if id.is_none() => "invalid-id",
        None => "unknown-request",
        Some((_, expires, _, _)) if expires <= now => "expired",
        Some((state, _, _, _)) if state != "pending" => "already-handled",
        Some((_, _, None, _)) => "unavailable-account",
        Some((_, _, _, disabled)) if disabled != Some(false) => "unavailable-account",
        Some((_, _, Some(account), _)) if matches!(actor,DecisionActor::WebAdmin(own) if account==own) => {
            "self-approval-denied"
        }
        Some(_) => {
            if action == "approve" {
                "approved"
            } else {
                "denied"
            }
        }
    };
    if matches!(outcome, "approved" | "denied") {
        anyhow::ensure!(tx.execute("UPDATE password_resets SET state=?,decided=?,decided_by=?,expires=? WHERE id=? AND state='pending' AND expires>?",params![outcome,now,actor.name(),now+900,id,now])?==1,"This request is no longer pending");
    }
    tx.execute("INSERT INTO password_reset_audit(created,source,actor,request_id,action,outcome) VALUES(?,?,?,?,?,?)",params![now,actor.source(),actor.name(),id,action,outcome])?;
    tx.commit()?;
    eprintln!(
        "{}",
        json!({"event":"password_reset_decision","source":actor.source(),"actor":actor.name(),"request_id":id,"action":action,"outcome":outcome,"created":now})
    );
    anyhow::ensure!(
        matches!(outcome, "approved" | "denied"),
        "This request is invalid, expired, already handled, unavailable, or belongs to your own account"
    );
    Ok(json!({"id":id,"state":outcome,"expires_at":now+900,"decided_by":actor.source()}))
}
fn host_registry(config: &Config) -> Result<Connection> {
    anyhow::ensure!(
        config.profiles.enabled,
        "Account profiles are not enabled in the selected configuration"
    );
    let path = Path::new(&config.profiles.directory).join("accounts.db");
    // No CREATE flag or directory provisioning: a typo must not create a second
    // registry or bootstrap a workspace. Relative paths follow server cwd rules.
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| {
            anyhow::anyhow!(
                "Could not open the existing account registry selected by this configuration"
            )
        })?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch("PRAGMA foreign_keys=ON;")?;
    anyhow::ensure!(
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='accounts')",
            [],
            |r| r.get::<_, bool>(0)
        )?,
        "Selected registry has no accounts table"
    );
    c.execute_batch("BEGIN IMMEDIATE")?;
    if let Err(error) = migrate(&c) {
        let _ = c.execute_batch("ROLLBACK");
        return Err(error);
    }
    c.execute_batch("COMMIT")?;
    Ok(c)
}
pub(super) fn host_list(config: &Config) -> Result<Value> {
    let c = host_registry(config)?;
    let rows=c.prepare("SELECT r.id,a.login,r.created,r.expires,r.ip,CASE WHEN r.expires<=? THEN 'expired' ELSE r.state END,r.decided,r.location,a.disabled,r.decided_by FROM password_resets r LEFT JOIN accounts a ON a.id=r.account ORDER BY r.created DESC,r.rowid DESC")?.query_map([db::now()],|r|{
        let login:Option<String>=r.get(1)?;let location:Option<String>=r.get(7)?;let by:Option<String>=r.get(9)?;
        Ok(json!({"id":metadata(&r.get::<_,String>(0)?),"username":login.map(|s|metadata(&s)),"created":r.get::<_,i64>(2)?,"expires_at":r.get::<_,i64>(3)?,"ip":metadata(&r.get::<_,String>(4)?),"state":metadata(&r.get::<_,String>(5)?),"decided":r.get::<_,Option<i64>>(6)?,"location":location.map(|s|metadata(&s)),"account_disabled":r.get::<_,Option<bool>>(8)?,"decided_by":by.map(|s|metadata(&s))}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        json!({"requests":rows,"metadata_notice":"IP and location describe the observed connection, not verified identity."}),
    )
}
pub(super) fn host_decide(config: &Config, id: &str, action: &str) -> Result<Value> {
    let mut c = host_registry(config)?;
    decide_record(&mut c, id, action, DecisionActor::LocalHost)
}

pub(super) async fn request(
    State(p): State<Portal>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let login = field(&v, "login", 80)?.to_lowercase();
    // Forwarded headers are untrusted. This is the observed connection address,
    // which may be a reverse proxy, never a claimed identity proof.
    let ip = peer
        .map(|axum::Extension(axum::extract::ConnectInfo(addr))| addr.ip().to_string())
        .unwrap_or_else(|| "Unavailable".into());
    p.rate_limit(&format!("reset-ip:{ip}"))?;
    p.rate_limit(&format!("reset-login:{login}"))?;
    let code = secret();
    let id = db::id();
    let now = db::now();
    let mut c = p.registry.lock().unwrap();
    let tx = c.transaction()?;
    tx.execute("DELETE FROM password_resets WHERE expires<=?", [now])?;
    ensure!(
        tx.query_row("SELECT COUNT(*) FROM password_resets", [], |r| r
            .get::<_, i64>(0))?
            < 512,
        "Password recovery is busy. Try again later."
    );
    let account: Option<String> = tx
        .query_row(
            "SELECT id FROM accounts WHERE login=? AND disabled=0",
            [&login],
            |r| r.get(0),
        )
        .optional()?;
    tx.execute("INSERT INTO password_resets(id,digest,account,created,expires,ip,state) VALUES(?,?,?,?,?,?,'pending')",params![id,hash(&code),account,now,now+86400,ip])?;
    tx.commit()?;
    drop(c);
    eprintln!("{}", incoming_record(&id, &login, now, &ip));
    if std::env::var("KINDRED_RESET_LOCATION_LOOKUPS").as_deref() != Ok("false")
        && !headers.contains_key("forwarded")
        && !headers.contains_key("x-forwarded-for")
        && !headers.contains_key("x-real-ip")
    {
        if let Ok(address) = ip.parse::<std::net::IpAddr>() {
            if location::public_ip(address) {
                let portal = Arc::downgrade(&p);
                let request = id.clone();
                tokio::spawn(async move {
                    if let Some(value) = location::lookup(address).await {
                        if let Some(p) = portal.upgrade() {
                            let _ = p.registry.lock().unwrap().execute(
                                "UPDATE password_resets SET location=? WHERE id=?",
                                params![value, request],
                            );
                        }
                    }
                });
            }
        }
    }
    Ok(Json(
        json!({"token":code,"id":id,"state":"pending","expires_at":now+86400}),
    ))
}
pub(super) async fn status(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let token = field(&v, "token", 256)?;
    let c = p.registry.lock().unwrap();
    let state:Option<String>=c.query_row("SELECT CASE WHEN expires<=? THEN 'expired' ELSE state END FROM password_resets WHERE digest=?",params![db::now(),hash(token)],|r|r.get(0)).optional()?;
    Ok(Json(
        json!({"state":state.unwrap_or_else(||"expired".into())}),
    ))
}
pub(super) async fn list(State(p): State<Portal>, headers: HeaderMap) -> ApiResult {
    p.origin(&headers)?;
    ensure!(
        p.identity(bearer(&headers))?.admin,
        "Administrator access required"
    );
    let c = p.registry.lock().unwrap();
    let rows=c.prepare("SELECT r.id,a.login,r.created,r.ip,r.state,r.decided,r.location FROM password_resets r JOIN accounts a ON a.id=r.account WHERE r.expires>? AND a.disabled=0 ORDER BY r.created DESC,r.rowid DESC")?.query_map([db::now()],|r|Ok(json!({"id":r.get::<_,String>(0)?,"username":r.get::<_,String>(1)?,"created":r.get::<_,i64>(2)?,"ip":r.get::<_,String>(3)?,"state":r.get::<_,String>(4)?,"decided":r.get::<_,Option<i64>>(5)?,"location":r.get::<_,Option<String>>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"requests":rows})))
}
pub(super) async fn decide(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let actor = p.identity(bearer(&headers))?;
    ensure!(actor.admin, "Administrator access required");
    let id = field(&v, "id", 64)?;
    let action = field(&v, "action", 16)?;
    ensure!(
        matches!(action, "approve" | "deny"),
        "Choose approve or deny"
    );
    let mut c = p.registry.lock().unwrap();
    Ok(Json(decide_record(
        &mut c,
        id,
        action,
        DecisionActor::WebAdmin(&actor.account),
    )?))
}
pub(super) async fn finish(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let token = field(&v, "token", 256)?.to_owned();
    let password = field(&v, "password", 1024)?.to_owned();
    ensure!(
        password.chars().count() >= 4,
        "Use a password of at least 4 characters"
    );
    ensure!(
        v["confirm_password"].as_str() == Some(password.as_str()),
        "Passwords do not match"
    );
    let digest = hash(&token);
    let account:String=p.registry.lock().unwrap().query_row("SELECT r.account FROM password_resets r JOIN accounts a ON a.id=r.account WHERE r.digest=? AND r.state='approved' AND r.expires>? AND a.disabled=0",params![digest,db::now()],|r|r.get(0)).optional()?.ok_or_else(||anyhow::anyhow!("This reset is not approved or has expired"))?;
    p.rate_limit(&format!("reset-save:{account}"))?;
    let permit = p
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("Password recovery is busy. Try again shortly."))?;
    let salt = secret();
    let copy = salt.clone();
    let password = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        password_hash(&password, &copy)
    })
    .await?;
    let mut c = p.registry.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(tx.execute("UPDATE accounts SET salt=?,password=? WHERE id=? AND disabled=0 AND EXISTS(SELECT 1 FROM password_resets WHERE digest=? AND account=? AND state='approved' AND expires>?)",params![salt,password,account,digest,account,db::now()])?==1,"This reset is no longer available");
    tx.execute("DELETE FROM sessions WHERE account_id=?", [&account])?;
    tx.execute("DELETE FROM device_links WHERE account_id=?", [&account])?;
    tx.execute("DELETE FROM password_resets WHERE account=?", [&account])?;
    tx.commit()?;
    Ok(Json(json!({"saved":true})))
}
#[cfg(test)]
mod host_tests {
    use super::*;
    #[test]
    fn password_reset_expiry_is_rechecked_after_waiting_for_sqlite_writer() {
        let path = std::env::temp_dir().join(format!("kindred-reset-clock-{}.db", db::id()));
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE accounts(id TEXT PRIMARY KEY,disabled INTEGER); INSERT INTO accounts VALUES('owner',0);").unwrap();
        migrate(&writer).unwrap();
        let id = db::id();
        let now = db::now();
        writer.execute("INSERT INTO password_resets(id,digest,account,created,expires,ip,state) VALUES(?,X'01','owner',?,?,'127.0.0.1','pending')",params![id,now,now+1]).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let copy = path.clone();
        let request = id.clone();
        let worker = std::thread::spawn(move || {
            let mut c = Connection::open(copy).unwrap();
            c.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
            started.send(db::now()).unwrap();
            decide_record(&mut c, &request, "approve", DecisionActor::LocalHost)
        });
        assert!(
            ready.recv().unwrap() < now + 1,
            "Fixture request must still be pending before the lock wait"
        );
        let elapsed = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_secs(2));
        writer.execute_batch("COMMIT").unwrap();
        let result = worker.join().unwrap();
        let state: String = writer
            .query_row("SELECT state FROM password_resets WHERE id=?", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        let outcome: String = writer
            .query_row(
                "SELECT outcome FROM password_reset_audit WHERE request_id=?",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        drop(writer);
        let _ = std::fs::remove_file(path);
        assert!(elapsed.elapsed() >= std::time::Duration::from_secs(2));
        assert!(
            result.is_err(),
            "Expired request was approved after waiting for another SQLite writer"
        );
        assert_eq!(state, "pending");
        assert_eq!(outcome, "expired");
    }
    #[test]
    fn password_reset_log_metadata_excludes_controls_bidi_links_and_secrets() {
        let v = incoming_record(
            "safe-id",
            "user\n\u{1b}[31mFAKE\u{202e}",
            123,
            "127.0.0.1\nInjected",
        );
        let line = v.to_string();
        assert!(!line.contains('\n'));
        assert!(!line.contains('\u{1b}'));
        assert!(!line.contains('\u{202e}'));
        assert!(line.contains("password_reset_requested"));
        assert!(!line.contains("digest"));
        assert!(!line.contains("token"));
        assert!(!line.contains("password\""));
        assert_eq!(
            metadata("https://example.test/reset?token=DO_NOT_LOG"),
            "[redacted metadata]"
        );
        assert_eq!(metadata("x".repeat(300).as_str()).len(), 200);
    }
}
