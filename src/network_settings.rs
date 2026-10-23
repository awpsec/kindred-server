//! Container-owned networking. The supervisor restarts only the server child;
//! the API never controls Docker or trusts forwarded IP headers.
use super::*;
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, net::{IpAddr, SocketAddr}};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    access: String,
    tailnet_address: String,
    public_url: String,
    allowed_origins: Vec<String>,
    confirmed_http_origins: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    revision: String,
    baseline: Settings,
    settings: Settings,
}

#[derive(Deserialize)]
pub struct Controller {
    mode: String,
    path: PathBuf,
    #[serde(flatten)]
    applied: Saved,
}

impl Controller {
    pub fn from_env() -> Result<Option<Self>> {
        std::env::var_os("KINDRED_WEB_NETWORK_SETTINGS").map(|path| Self::read(Path::new(&path))).transpose()
    }
    pub fn read(path: &Path) -> Result<Self> {
        let bytes=fs::read(path)?;
        anyhow::ensure!(bytes.len()<=32768,"Invalid network control file");
        let control:Self=serde_json::from_slice(&bytes)?;
        anyhow::ensure!(matches!(control.mode.as_str(),"host"|"bridge"),"Invalid container network mode");
        Ok(control)
    }
    fn pending(&self) -> Result<Saved> {
        let bytes=fs::read(&self.path)?;
        anyhow::ensure!(bytes.len()<=32768,"Invalid network settings file");
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn view(&self) -> Result<Value> {
        let pending=self.pending()?;
        Ok(json!({"mode":self.mode,"revision":self.applied.revision,"settings":self.applied.settings,
            "defaults":self.applied.baseline,"pending":(pending.revision!=self.applied.revision).then_some(pending)}))
    }
}

fn listeners(settings:&Settings, mode:&str) -> Result<Vec<SocketAddr>> {
    if mode=="bridge" {
        anyhow::ensure!(settings.access=="bridge" && settings.tailnet_address.is_empty(),"Docker port exposure is managed in Compose for this installation");
        return Ok(vec!["0.0.0.0:9444".parse()?]);
    }
    match settings.access.as_str() {
        "local"=>Ok(vec!["127.0.0.1:9444".parse()?]),
        "all"=>Ok(vec!["0.0.0.0:9444".parse()?]),
        "tailnet"=>{
            let ip:IpAddr=settings.tailnet_address.parse().map_err(|_|anyhow::anyhow!("Enter this Linux host's Tailscale IP address"))?;
            let tail=match ip {
                IpAddr::V4(ip)=>u32::from(ip)&0xffc00000==0x64400000,
                IpAddr::V6(ip)=>ip.segments()[..3]==[0xfd7a,0x115c,0xa1e0],
            };
            anyhow::ensure!(tail,"Enter this Linux host's Tailscale IP address");
            Ok(vec!["127.0.0.1:9444".parse()?,SocketAddr::new(ip,9444)])
        },
        _=>anyhow::bail!("Choose localhost, localhost and Tailscale, or all IPv4 interfaces"),
    }
}

fn normalize(settings:&mut Settings) -> Result<()> {
    fn origin(value:&str)->Result<String> {
        anyhow::ensure!(value.len()<=2048 && !value.chars().any(char::is_control),"Invalid connection address");
        let url=reqwest::Url::parse(value.trim())?;
        anyhow::ensure!(matches!(url.scheme(),"http"|"https") && url.username().is_empty() && url.password().is_none()
            && url.query().is_none() && url.fragment().is_none() && url.path()=="/","Use a complete HTTP or HTTPS address without a path or credentials");
        Ok(url.origin().ascii_serialization())
    }
    settings.public_url=origin(&settings.public_url)?;
    for list in [&mut settings.allowed_origins,&mut settings.confirmed_http_origins] {
        anyhow::ensure!(list.len()<=8,"At most eight additional connection addresses are allowed");
        *list=list.iter().map(|s|origin(s)).collect::<Result<Vec<_>>>()?;
        list.sort();list.dedup();
    }
    settings.tailnet_address=settings.tailnet_address.trim().to_owned();
    if settings.access!="tailnet" {settings.tailnet_address.clear();}
    Ok(())
}

fn validate(settings:&Settings, mode:&str, current:&Config, probe:bool) -> Result<()> {
    let addresses=listeners(settings,mode)?;
    let mut config=current.clone();
    config.public_url=settings.public_url.clone();
    config.allowed_origins=settings.allowed_origins.clone();
    config.confirmed_http_origins=settings.confirmed_http_origins.clone();
    config.listen=addresses[0];config.additional_listen=addresses[1..].to_vec();
    config.validate()?;
    if probe && mode=="host" {
        let current:Vec<_>=std::iter::once(current.listen).chain(current.additional_listen.iter().copied()).collect();
        for address in addresses {
            // Probe assignment even when a current wildcard listener covers the
            // port. A Tailscale address belonging to another device is invalid.
            std::net::UdpSocket::bind(SocketAddr::new(address.ip(),0))
                .map_err(|_|anyhow::anyhow!("That address is not available on this Linux host. Check that Tailscale is connected."))?;
            let covered=current.iter().any(|old|old.port()==address.port() && (old.ip()==address.ip()
                || old.ip().is_unspecified() && old.is_ipv4()==address.is_ipv4()));
            if !covered && !address.ip().is_unspecified() {
                std::net::TcpListener::bind(address).map_err(|_|anyhow::anyhow!("The selected listening address is already in use"))?;
            }
        }
    }
    Ok(())
}

fn write(path:&Path, value:&Saved)->Result<()> {
    let temporary=path.with_extension(format!("{}.tmp",uuid::Uuid::new_v4()));
    let result=(||->Result<()> {
        let mut options=fs::OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt; options.mode(0o600);}
        let mut file=options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(value)?)?;file.sync_all()?;
        fs::rename(&temporary,path)?;
        fs::File::open(path.parent().ok_or_else(||anyhow::anyhow!("Invalid settings directory"))?)?.sync_all()?;
        Ok(())
    })();
    let _=fs::remove_file(temporary);result
}

// This administrator diagnostic probes from the server, not from the browser.
fn check_origin(value: &str) -> Result<reqwest::Url> {
    ensure!(value.len() <= 2048 && !value.chars().any(char::is_control), "Invalid connection address");
    let value=value.trim();
    let authority=value.split_once("://").map(|(_,v)|v).unwrap_or("");
    let suffix=authority.find('/').map(|i|&authority[i..]).unwrap_or("");
    ensure!(!authority.is_empty() && !authority.contains('@') && (suffix.is_empty() || suffix=="/") && !value.contains('\\'), "Use an HTTP or HTTPS root address");
    let url=reqwest::Url::parse(value).map_err(|_|anyhow::anyhow!("Invalid connection address"))?;
    ensure!(matches!(url.scheme(),"http"|"https") && url.host_str().is_some() && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && url.path()=="/", "Use an HTTP or HTTPS root address without credentials, a query, or a fragment");
    Ok(url)
}
fn check_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip)=>!ip.is_link_local() && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast() && ip.octets()[0]!=0 && ip.octets()[0]<240 && ip.octets()!=[100,100,100,200] && ip.octets()!=[192,0,0,192],
        IpAddr::V6(ip)=>if let Some(v4)=ip.to_ipv4_mapped(){check_ip(IpAddr::V4(v4))}else{!ip.is_unspecified() && !ip.is_multicast() && ip.segments()[0]&0xffc0!=0xfe80 && ip != "fd00:ec2::254".parse::<std::net::Ipv6Addr>().unwrap()},
    }
}
fn check_error(error: &reqwest::Error) -> &'static str {
    use std::error::Error;
    let mut text=String::new();let mut source=error.source();
    while let Some(e)=source {text.push_str(&e.to_string());source=e.source();}
    let text=text.to_ascii_lowercase();
    if ["invalid peer certificate","invalid certificate","certificate verify","unknownissuer","unknown issuer","certificate has expired"].iter().any(|s|text.contains(s)) {"certificate"}
    else if ["wrong version number","invalidcontenttype","invalid content type","received corrupt message","invalid tls record"].iter().any(|s|text.contains(s)) {"protocol_mismatch"}
    else {"unavailable"}
}
async fn check_probe(url: &reqwest::Url) -> &'static str {
    let host=url.host_str().unwrap();let host=host.trim_start_matches('[').trim_end_matches(']');let port=url.port_or_known_default().unwrap();
    let addresses=match tokio::net::lookup_host((host,port)).await {Ok(a)=>a.collect::<Vec<_>>(),Err(_)=>return "unavailable"};
    if addresses.is_empty(){return "unavailable";}
    if addresses.iter().any(|a|!check_ip(a.ip())) {return "blocked";}
    // Pin the validated DNS results, bypass ambient proxies, and never redirect.
    let client=match reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(6)).resolve_to_addrs(host,&addresses).build(){Ok(c)=>c,Err(_)=>return "unavailable"};
    let target=url.join("identity/meta").unwrap();
    let mut response=match client.get(target).send().await{Ok(r)=>r,Err(e)=>return check_error(&e)};
    if !response.status().is_success(){return "not_kindred";}
    if response.content_length().is_some_and(|n|n>32768){return "not_kindred";}
    let mut bytes=Vec::new();
    loop {match response.chunk().await {Ok(Some(chunk))=>{if bytes.len()+chunk.len()>32768{return "not_kindred";}bytes.extend_from_slice(&chunk);},Ok(None)=>break,Err(e)=>return check_error(&e)}}
    let value:Value=match serde_json::from_slice(&bytes){Ok(v)=>v,Err(_)=>return "not_kindred"};
    if value["profiles"]==true && value["version"].as_str().is_some_and(|s|!s.is_empty()&&s.len()<80) && value["first_user"].is_boolean() && value["registration"].is_boolean(){"reachable"}else{"not_kindred"}
}
pub(super) async fn check(State(p):State<Portal>,headers:HeaderMap,Json(body):Json<Value>)->ApiResult {
    p.origin(&headers)?;
    ensure!(p.identity(bearer(&headers))?.admin,"Administrator access required");
    let url=check_origin(field(&body,"url",2048)?)?;
    let state=tokio::time::timeout(std::time::Duration::from_secs(8),check_probe(&url)).await.unwrap_or("unavailable");
    let message=match state {
        "reachable"=>"The server reached a Kindred sign-in endpoint. This does not confirm access from other devices or that it is this workspace.",
        "protocol_mismatch"=>"The server received a response that does not match the selected HTTPS protocol. Check the scheme and port; no HTTP fallback was attempted.",
        "certificate"=>"The server could not verify the HTTPS certificate. Check the certificate and hostname.",
        "blocked"=>"The address resolves to a reserved or link-local destination that cannot be checked.",
        "not_kindred"=>"The server did not receive the expected Kindred sign-in metadata. Redirects are not followed.",
        _=>"The server could not reach this address within the check limit.",
    };
    Ok(Json(json!({"url":url.origin().ascii_serialization(),"state":state,"message":message,"checked_from":"server","encrypted":(state=="reachable").then_some(url.scheme()=="https")})))
}

pub(super) async fn get(State(p):State<Portal>,headers:HeaderMap)->ApiResult {
    p.origin(&headers)?;
    ensure!(p.identity(bearer(&headers))?.admin,"Administrator access required");
    let control=p.network.lock().unwrap();
    let control=control.as_ref().ok_or_else(||anyhow::anyhow!("Web networking settings are unavailable for this installation"))?;
    Ok(Json(control.view()?))
}

pub(super) async fn save(State(p):State<Portal>,headers:HeaderMap,Json(body):Json<Value>)->ApiResult {
    p.origin(&headers)?;
    ensure!(p.identity(bearer(&headers))?.admin,"Administrator access required");
    let control=p.network.lock().unwrap();
    let control=control.as_ref().ok_or_else(||anyhow::anyhow!("Web networking settings are unavailable for this installation"))?;
    let revision=field(&body,"revision",80)?;
    let request=field(&body,"request_id",80)?;
    ensure!(uuid::Uuid::parse_str(request).is_ok(),"Invalid settings request");
    let mut settings:Settings=if body["reset"]==true {control.applied.baseline.clone()}
        else {serde_json::from_value(body["settings"].clone())?};
    normalize(&mut settings)?;
    let pending=control.pending()?;
    // A lost response can be retried using the same request ID, never replayed
    // against a different generation or with different settings.
    if pending.revision==request {
        ensure!(pending.settings==settings,"This request ID was already used with different settings");
        return Ok(Json(json!({"revision":request,"settings":settings,"restarting":pending.revision!=control.applied.revision})));
    }
    ensure!(revision==control.applied.revision && pending.revision==control.applied.revision,"Network settings changed. Reload them before saving again.");
    validate(&settings,&control.mode,&p.config,true)?;
    let c=p.registry.lock().unwrap();
    ensure!(account_management::identity_in(&c,bearer(&headers))?.admin,"Administrator access required");
    if settings==control.applied.settings {
        return Ok(Json(json!({"revision":control.applied.revision,"settings":settings,"restarting":false})));
    }
    let saved=Saved{revision:request.to_owned(),baseline:control.applied.baseline.clone(),settings};
    write(&control.path.with_file_name("network-last-good.json"),&control.applied)?;
    write(&control.path,&saved)?;
    Ok(Json(json!({"revision":request,"settings":saved.settings,"restarting":true})))
}

pub(super) fn meta(p:&Profiles)->Value {
    let control=p.network.lock().unwrap();
    control.as_ref().map(|c|json!({"mode":c.mode,"revision":c.applied.revision})).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings()->Settings {Settings{access:"local".into(),tailnet_address:String::new(),public_url:"http://127.0.0.1:9444".into(),allowed_origins:vec![],confirmed_http_origins:vec![]}}
    #[test]
    fn check_roots_and_reserved_addresses() {
        for url in ["http://127.0.0.1:9444", "https://host.example/", "http://[::1]:9444"] {assert!(check_origin(url).is_ok(),"{url}");}
        for url in ["ftp://host", "https://user:pass@host", "https://@host", "https://host/path", "https://host/a/..", "https://host/?x=1", "https://host/#x", "https://host/\\path"] {assert!(check_origin(url).is_err(),"{url}");}
        for ip in ["169.254.169.254","100.100.100.200","fe80::1","fd00:ec2::254","192.0.0.192","::ffff:169.254.169.254","0.0.0.0","224.0.0.1","::"] {assert!(!check_ip(ip.parse().unwrap()),"{ip}");}
        for ip in ["127.0.0.1","::1","192.168.1.1","100.64.0.1","fd7a:115c:a1e0::1"] {assert!(check_ip(ip.parse().unwrap()),"{ip}");}
    }
    async fn check_fixture(reply:String, tls:bool)->(&'static str,String) {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let task=tokio::spawn(async move {let(mut stream,_)=listener.accept().await.unwrap();let mut bytes=[0;8192];let n=stream.read(&mut bytes).await.unwrap();stream.write_all(reply.as_bytes()).await.unwrap();String::from_utf8_lossy(&bytes[..n]).to_string()});
        let url=check_origin(&format!("{}://{address}",if tls{"https"}else{"http"})).unwrap();
        let state=check_probe(&url).await;(state,task.await.unwrap())
    }
    #[tokio::test]
    async fn check_metadata_redirect_limits_and_protocol() {
        let body=r#"{"profiles":true,"version":"0.87.2","first_user":false,"registration":true}"#;
        let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        let(state,request)=check_fixture(response.clone(),false).await;assert_eq!(state,"reachable");assert!(request.starts_with("GET /identity/meta HTTP/1.1"));assert!(!request.to_lowercase().contains("authorization:"));assert!(!request.to_lowercase().contains("cookie:"));
        let(state,_)=check_fixture("HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n".into(),false).await;assert_eq!(state,"not_kindred");
        let(state,_)=check_fixture("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".into(),false).await;assert_eq!(state,"not_kindred");
        let(state,_)=check_fixture("HTTP/1.1 200 OK\r\nContent-Length: 32769\r\n\r\n".into(),false).await;assert_eq!(state,"not_kindred");
        let(state,_)=check_fixture(response,true).await;assert_eq!(state,"protocol_mismatch");
    }
    #[test]
    fn descriptor_roundtrip_and_generation_are_independent_of_pending_save() {
        let folder=std::env::temp_dir().join(format!("network-test-{}",uuid::Uuid::new_v4()));fs::create_dir(&folder).unwrap();
        let descriptor=folder.join("startup.json");let pending=folder.join("settings.json");
        let state=Saved{revision:"initial".into(),settings:settings(),baseline:settings()};
        write(&pending,&state).unwrap();
        fs::write(&descriptor,serde_json::to_vec(&json!({"revision":"initial","mode":"host","path":pending,"settings":settings(),"baseline":settings()})).unwrap()).unwrap();
        let controller=Controller::read(&descriptor).unwrap();
        let mut next=state.clone();next.revision="pending".into();next.settings.access="all".into();write(&pending,&next).unwrap();
        let view=controller.view().unwrap();assert_eq!(view["revision"],"initial");assert_eq!(view["pending"]["revision"],"pending");
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(fs::metadata(&pending).unwrap().permissions().mode()&0o777,0o600);}
        fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn addresses_normalize_and_public_http_is_rejected() {
        let mut value=settings();value.public_url="https://EXAMPLE.com:443/".into();value.allowed_origins=vec!["http://127.0.0.1:9444/".into(),"http://127.0.0.1:9444".into()];
        normalize(&mut value).unwrap();assert_eq!(value.public_url,"https://example.com");assert_eq!(value.allowed_origins.len(),1);
        validate(&value,"host",&Config::default(),false).unwrap();
        value.public_url="http://example.com".into();assert!(validate(&value,"host",&Config::default(),false).is_err());
        for url in ["https://user:pass@example.com","https://example.com/path","javascript:alert(1)"] {value.public_url=url.into();assert!(normalize(&mut value).is_err());}
    }
    #[test]
    fn tailnet_and_bridge_boundaries() {
        let mut value=settings();value.access="tailnet".into();
        for ip in ["100.64.0.1","100.127.255.254","fd7a:115c:a1e0::1"] {value.tailnet_address=ip.into();let bound=listeners(&value,"host").unwrap();assert_eq!(bound.len(),2);assert!(bound[0].ip().is_loopback());}
        for ip in ["100.128.0.1","192.168.1.1","fd7a:115c:a1e1::1","127.0.0.1"] {value.tailnet_address=ip.into();assert!(listeners(&value,"host").is_err());}
        assert!(listeners(&settings(),"bridge").is_err());value=settings();value.access="bridge".into();assert_eq!(listeners(&value,"bridge").unwrap()[0].to_string(),"0.0.0.0:9444");
    }
}
