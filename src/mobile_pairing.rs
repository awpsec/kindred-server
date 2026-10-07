//! Short-lived account pairing. QR secrets are hashed at rest, bound to the
//! issuing session, and consumed transactionally into an independent session.
use super::*;
macro_rules! ensure { ($condition:expr, $($message:tt)*) => { if !$condition { return Err(anyhow::anyhow!($($message)*).into()); } }; }
use axum::extract::Path as RoutePath;

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS mobile_pairings(id TEXT PRIMARY KEY,digest BLOB NOT NULL UNIQUE,issuer BLOB NOT NULL,account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,expires INTEGER NOT NULL,claimed INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS mobile_pairing_expiry ON mobile_pairings(expires);")?;
    Ok(())
}
pub fn routes() -> Router<Portal> {
    Router::new()
        .route("/identity/mobile-pairing", post(issue))
        .route("/identity/mobile-pairing/claim", post(claim))
        .route("/identity/mobile-pairing/{id}", get(status).delete(cancel))
}
fn phone_origin(value: &str) -> Result<String> {
    ensure!(value.len()<=512 && !value.chars().any(char::is_control), "Enter the complete server address reachable from your phone");
    let url = reqwest::Url::parse(value).map_err(|_|anyhow::anyhow!("Enter a complete HTTPS address or a private IP address with http://"))?;
    let private_literal=url.host_str().unwrap_or("").trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|match ip {
        std::net::IpAddr::V4(v)=>v.is_private() || (v.octets()[0]==100 && (64..=127).contains(&v.octets()[1])),
        std::net::IpAddr::V6(v)=>(v.segments()[0]&0xff00)==0xfd00 && v.to_ipv4().is_none(),
    });
    ensure!((url.scheme()=="https" || (url.scheme()=="http" && private_literal)) && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && matches!(url.path(),""|"/"), "Use HTTPS, or HTTP to a private IP address, without a path or credentials");
    ensure!(!url.host_str().unwrap_or("").ends_with('.'), "Use the server address without a trailing dot");
    let host=url.host_str().unwrap_or("").trim_matches(['[',']']).trim_end_matches('.');
    ensure!(!host.is_empty() && host!="localhost" && !host.ends_with(".localhost") && !host.parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback()||ip.is_unspecified()||matches!(ip,std::net::IpAddr::V6(v) if v.to_ipv4().is_some_and(|v|v.is_loopback()||v.is_unspecified()))), "A phone cannot connect to this computer's localhost address. Enter its reachable HTTPS address.");
    Ok(url.origin().ascii_serialization())
}
async fn issue(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?;
    let identity=p.identity(bearer(&headers))?;
    ensure!(!identity.legacy, "Set up your Kindred account before connecting the mobile app");
    let server=phone_origin(v["server"].as_str().unwrap_or(&p.config.public_url))?;
    ensure!(p.config.allows_origin(&server), "This address is not enabled for this server. In Standalone, select its network under Server admin → Networking, then choose Save to apply it. For a hosted server, ask its administrator to configure this connection address.");
    let code=secret();
    let mut link=reqwest::Url::parse("kindred://pair")?;
    link.query_pairs_mut().append_pair("server",&server);
    link.set_fragment(Some(&format!("code={code}")));
    let qr=qrcode::QrCode::new(link.as_str().as_bytes())?;
    let modules:Vec<bool>=qr.to_colors().into_iter().map(|c|c==qrcode::Color::Dark).collect();
    let id=db::id(); let expires=db::now()+300;
    let mut c=p.registry.lock().unwrap(); let tx=c.transaction()?;
    tx.execute("DELETE FROM mobile_pairings WHERE expires<=?",[db::now()])?;
    // A new code replaces only this desktop session's older code.
    tx.execute("DELETE FROM mobile_pairings WHERE issuer=?",[hash(bearer(&headers))])?;
    let count:i64=tx.query_row("SELECT count(*) FROM mobile_pairings WHERE account_id=?",[&identity.account],|r|r.get(0))?;
    ensure!(count<8,"Too many pending pairing codes. Close an older pairing window first.");
    // Revalidate under the write lock: sign-out/password reset revokes issuance too.
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions s JOIN accounts a ON a.id=s.account_id WHERE s.digest=? AND s.expires>? AND a.disabled=0)",params![hash(bearer(&headers)),db::now()],|r|r.get(0))?;
    ensure!(valid,"Your session expired. Sign in again.");
    let login:String=tx.query_row("SELECT login FROM accounts WHERE id=?",[&identity.account],|r|r.get(0))?;
    tx.execute("INSERT INTO mobile_pairings(id,digest,issuer,account_id,profile_id,expires) VALUES(?,?,?,?,?,?)",params![id,hash(&code),hash(bearer(&headers)),identity.account,identity.profile,expires])?;
    tx.commit()?;
    Ok(Json(json!({"id":id,"url":link.as_str(),"server":server,"login":login,"profile_id":identity.profile,"expires_at":expires,"qr":{"width":qr.width(),"modules":modules}})))
}
async fn claim(State(p): State<Portal>, headers: HeaderMap, Json(v): Json<Value>) -> ApiResult {
    p.origin(&headers)?; // Native requests have no Origin; browser origins remain checked.
    let code=field(&v,"code",64)?;
    ensure!(code.len()==64 && code.bytes().all(|c|c.is_ascii_hexdigit()),"This pairing code is invalid, expired, or already used. Create a new code.");
    let mut c=p.registry.lock().unwrap(); let tx=c.transaction()?;
    let row:Option<(String,String,String)>=tx.query_row("SELECT m.account_id,m.profile_id,a.login FROM mobile_pairings m JOIN accounts a ON a.id=m.account_id JOIN profiles pr ON pr.id=m.profile_id AND pr.account_id=a.id JOIN sessions s ON s.digest=m.issuer AND s.account_id=a.id WHERE m.digest=? AND m.expires>? AND m.claimed=0 AND s.expires>? AND a.disabled=0",params![hash(code),db::now(),db::now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let (account,profile,login)=row.ok_or_else(||anyhow::anyhow!("This pairing code is invalid, expired, or already used. Create a new code."))?;
    let token=Profiles::session(&tx,&account,&profile)?;
    tx.execute("UPDATE mobile_pairings SET claimed=1 WHERE digest=? AND claimed=0",[hash(code)])?;
    tx.commit()?;
    Ok(Json(json!({"token":token,"account_id":account,"profile_id":profile,"login":login})))
}
async fn status(State(p): State<Portal>, headers: HeaderMap, RoutePath(id): RoutePath<String>) -> ApiResult {
    p.origin(&headers)?; let user=p.identity(bearer(&headers))?;
    let c=p.registry.lock().unwrap();
    let row:Option<(i64,bool)>=c.query_row("SELECT expires,claimed FROM mobile_pairings WHERE id=? AND account_id=? AND issuer=?",params![id,user.account,hash(bearer(&headers))],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    Ok(Json(json!({"status":match row {Some((_,true))=>"claimed",Some((expires,false)) if expires>db::now()=>"pending",_=>"expired"}})))
}
async fn cancel(State(p): State<Portal>, headers: HeaderMap, RoutePath(id): RoutePath<String>) -> ApiResult {
    p.origin(&headers)?;let user=p.identity(bearer(&headers))?;
    p.registry.lock().unwrap().execute("DELETE FROM mobile_pairings WHERE id=? AND account_id=? AND issuer=?",params![id,user.account,hash(bearer(&headers))])?;
    Ok(Json(json!({"ok":true})))
}

#[cfg(test)]
mod origin_tests {
    use super::*;
    #[test]
    fn private_http_pairing_requires_canonical_literal_origin() {
        for address in ["http://10.1.2.3:9444","http://172.16.0.1:9444","http://192.168.1.20:9444","http://100.64.1.2:9444","http://[fd7a:115c::5]:9444"] {
            assert_eq!(phone_origin(address).unwrap(),address);
        }
        assert_eq!(phone_origin("https://Computer.tailnet.ts.net:443/").unwrap(),"https://computer.tailnet.ts.net");
        for address in ["http://127.0.0.1:9444","http://[::1]:9444","http://0.0.0.0:9444","http://8.8.8.8:9444","http://100.128.1.2:9444","http://172.32.1.2:9444","http://[fc00::1]:9444","http://[2001:4860::1]:9444","http://169.254.1.2:9444","http://[fe80::5]:9444","http://computer.tailnet.ts.net:9444","http://computer.local:9444","http://[::ffff:192.168.1.2]:9444","http://user@192.168.1.2:9444","http://192.168.1.2:9444/path","http://192.168.1.2:9444?token=x","http://192.168.1.2:9444#x","192.168.1.2:9444","http://192.168.1.2:65536"] {
            assert!(phone_origin(address).is_err(),"{address}");
        }
    }
}
