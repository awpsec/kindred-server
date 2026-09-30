//! Admin-approved resets are bound to a secret held by the requesting browser.
use super::*;
#[path = "password_reset_location.rs"]
mod location;
macro_rules! ensure { ($condition:expr, $($message:tt)*) => { if !$condition { return Err(anyhow::anyhow!($($message)*).into()); } }; }

pub(super) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS password_resets(id TEXT PRIMARY KEY,digest BLOB NOT NULL UNIQUE,account TEXT REFERENCES accounts(id),created INTEGER NOT NULL,expires INTEGER NOT NULL,ip TEXT NOT NULL,state TEXT NOT NULL,decided INTEGER,decided_by TEXT);
      CREATE INDEX IF NOT EXISTS password_resets_expiry ON password_resets(expires);")?;
    if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('password_resets') WHERE name='location')",[],|r|r.get::<_,bool>(0))?{c.execute("ALTER TABLE password_resets ADD COLUMN location TEXT",[])?;}
    Ok(())
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
            [login],
            |r| r.get(0),
        )
        .optional()?;
    tx.execute("INSERT INTO password_resets(id,digest,account,created,expires,ip,state) VALUES(?,?,?,?,?,?,'pending')",params![id,hash(&code),account,now,now+86400,ip])?;
    tx.commit()?;
    drop(c);
    if std::env::var("KINDRED_RESET_LOCATION_LOOKUPS").as_deref()!=Ok("false") && !headers.contains_key("forwarded") && !headers.contains_key("x-forwarded-for") && !headers.contains_key("x-real-ip") {
        if let Ok(address)=ip.parse::<std::net::IpAddr>() {if location::public_ip(address){
            let portal=Arc::downgrade(&p);let request=id.clone();
            tokio::spawn(async move{if let Some(value)=location::lookup(address).await{if let Some(p)=portal.upgrade(){let _=p.registry.lock().unwrap().execute("UPDATE password_resets SET location=? WHERE id=?",params![value,request]);}}});
        }}
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
    let state = if action == "approve" {
        "approved"
    } else {
        "denied"
    };
    let now = db::now();
    let c = p.registry.lock().unwrap();
    ensure!(c.execute("UPDATE password_resets SET state=?,decided=?,decided_by=?,expires=? WHERE id=? AND state='pending' AND expires>? AND account<>? AND account IN (SELECT id FROM accounts WHERE disabled=0)",params![state,now,actor.account,now+900,id,now,actor.account])?==1,"This request expired, was already handled, or belongs to your own account");
    Ok(Json(json!({"state":state})))
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
