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
