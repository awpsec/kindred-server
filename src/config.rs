use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::Path};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub public_url: String,
    pub allowed_origins: Vec<String>,
    /// Exact HTTP origins approved by the managed Connection access Save flow.
    pub confirmed_http_origins: Vec<String>,
    pub database: String,
    pub token_env: String,
    pub profiles: Profiles,
    pub vm: Vm,
    pub openrouter: OpenRouter,
    pub pi: Pi,
    pub decisions: Decisions,
    pub max_steps: usize,
    pub max_parallel_runs: usize,
    /// Legacy blanket run timer. Only a customized value still caps tasks;
    /// see `task_timeout_seconds()`.
    pub run_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub task_timeout_seconds: Option<u64>,
    pub task_action_limit: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Vm {
    pub managed_id: String,
    pub manager: String,
    pub ssh_config: String,
    pub control_helper: String,
    pub uri: String,
    pub domain: String,
    pub ssh_alias: String,
    pub guest_binary: String,
    pub codex_binary: String,
    pub vnc_port: u16,
    pub vnc_view_port: u16,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profiles {
    pub enabled: bool,
    pub directory: String,
    pub import_legacy: bool,
    pub open_registration: bool,
    pub max_users: usize,
    pub max_profiles_per_user: usize,
    pub vm_manager: String,
}
impl Default for Profiles {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: "data/profiles".into(),
            import_legacy: false,
            open_registration: true,
            max_users: 128,
            max_profiles_per_user: 8,
            vm_manager: "/usr/local/lib/kindred/vm-manager.py".into(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenRouter {
    pub api_key_env: String,
    pub require_zdr: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Decisions {
    /// Explicit opt-in: screenshots and supplied values are sent to OpenAI.
    pub enabled: bool,
    pub api_key_env: String,
}
impl Default for Decisions {
    fn default() -> Self {
        Self { enabled: false, api_key_env: "KINDRED_DECISIONS_API_KEY".into() }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pi {
    pub node_binary: String,
    pub worker_script: String,
}

impl Default for Pi {
    fn default() -> Self {
        Self {
            node_binary: "/opt/kindred/pi/current/node/bin/node".into(),
            worker_script: "/opt/kindred/pi/current/worker.mjs".into(),
        }
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self {
            managed_id: String::new(),
            manager: String::new(),
            ssh_config: String::new(),
            control_helper: String::new(),
            uri: "qemu:///system".into(),
            domain: "kindred-bots".into(),
            ssh_alias: "kindred-guest".into(),
            guest_binary: "/usr/local/bin/kindred".into(),
            codex_binary: "/usr/local/bin/codex".into(),
            vnc_port: 5900,
            vnc_view_port: 5901,
        }
    }
}
impl Default for OpenRouter {
    fn default() -> Self {
        Self {
            api_key_env: "OPENROUTER_API_KEY".into(),
            require_zdr: true,
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:7340".parse().unwrap(),
            public_url: "http://127.0.0.1:7340".into(),
            allowed_origins: Vec::new(),
            confirmed_http_origins: Vec::new(),
            database: "data/kindred.db".into(),
            token_env: "KINDRED_TOKEN".into(),
            profiles: Profiles::default(),
            vm: Vm::default(),
            openrouter: OpenRouter::default(),
            pi: Pi::default(),
            decisions: Decisions::default(),
            max_steps: 0,
            max_parallel_runs: 4,
            run_timeout_seconds: 0,
            request_timeout_seconds: 600,
            idle_timeout_seconds: 1800,
            task_timeout_seconds: None,
            task_action_limit: 10_000,
        }
    }
}

pub fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn safe_executable(value: &str) -> bool {
    value.starts_with('/')
        && value.len() < 512
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
}
fn private_network_host(host: &str) -> bool {
    let host=host.trim_matches(['[',']']);
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local() || (ip.octets()[0]==100 && (64..=127).contains(&ip.octets()[1])),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0]&0xfe00)==0xfc00 || (ip.segments()[0]&0xffc0)==0xfe80,
        Err(_) => host=="localhost" || host.ends_with(".ts.net") || host.ends_with(".local"),
    }
}
impl Config {
    pub fn allows_origin(&self, origin: &str) -> bool {
        origin == self.public_url.trim_end_matches('/')
            || self
                .allowed_origins
                .iter()
                .any(|v| origin == v.trim_end_matches('/') && reqwest::Url::parse(origin).is_ok_and(|u| u.scheme()!="http" || private_network_host(u.host_str().unwrap_or("")) || self.confirmed_http_origins.iter().any(|s|s==origin)))
    }
    pub fn connection_policy_digest(&self) -> String {
        let bytes=serde_json::to_vec(&(&self.allowed_origins,&self.confirmed_http_origins)).unwrap();
        ring::digest::digest(&ring::digest::SHA256,&bytes).as_ref().iter().map(|b|format!("{b:02x}")).collect()
    }
    /// Active-time cap for one task in seconds; 0 means no total cap.
    /// An explicit `task_timeout_seconds` wins. The old shipped default
    /// `run_timeout_seconds = 1800` no longer cuts off productive work, but a
    /// customized legacy value is still honored.
    pub fn task_timeout_seconds(&self) -> u64 {
        match self.task_timeout_seconds {
            Some(seconds) => seconds,
            None if matches!(self.run_timeout_seconds, 0 | 1800) => 0,
            None => self.run_timeout_seconds,
        }
    }
    /// Tool actions allowed per task. A nonzero legacy `max_steps` is a lower
    /// ceiling and keeps its meaning.
    pub fn task_action_limit(&self) -> usize {
        match self.max_steps {
            0 => self.task_action_limit,
            steps => steps.min(self.task_action_limit),
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = toml::from_str(
            &std::fs::read_to_string(path).context("read configuration; run kindred init first")?,
        )?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.decisions.api_key_env.len() <= 128 && !self.decisions.api_key_env.is_empty()
            && self.decisions.api_key_env.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "Decisions api_key_env must name a server environment variable");
        ensure!(
            (1..=1024).contains(&self.profiles.max_users)
                && (1..=32).contains(&self.profiles.max_profiles_per_user),
            "Profile limits are out of range"
        );
        ensure!(
            !self.profiles.enabled
                || (!self.profiles.directory.is_empty()
                    && safe_executable(&self.profiles.vm_manager)),
            "Profiles require a data directory and absolute VM manager path"
        );
        ensure!(
            self.vm.managed_id.is_empty()
                || (uuid::Uuid::parse_str(&self.vm.managed_id).is_ok()
                    && safe_executable(&self.vm.manager)
                    && self.vm.ssh_config.starts_with('/')),
            "Invalid managed computer configuration"
        );
        ensure!(
            safe_executable(&self.pi.node_binary) && safe_executable(&self.pi.worker_script),
            "Pi runtime and worker must be absolute paths without spaces or shell characters"
        );
        ensure!(
            (1..=8).contains(&self.max_parallel_runs),
            "max_parallel_runs must be 1..8"
        );
        ensure!(
            safe_name(&self.vm.domain) && safe_name(&self.vm.ssh_alias),
            "VM domain and SSH alias must be simple names"
        );
        ensure!(
            matches!(self.vm.uri.as_str(), "qemu:///system" | "qemu:///session"),
            "only local libvirt URIs are supported"
        );
        ensure!(
            safe_executable(&self.vm.guest_binary) && safe_executable(&self.vm.codex_binary),
            "guest executables must be absolute paths without spaces or shell characters"
        );
        ensure!(
            self.vm.control_helper.is_empty() || safe_executable(&self.vm.control_helper),
            "control_helper must be an absolute path without shell characters"
        );
        ensure!(
            self.max_steps <= 100,
            "max_steps must be 0 (use task_action_limit) or between 1 and 100"
        );
        ensure!(
            self.run_timeout_seconds == 0 || (30..=7200).contains(&self.run_timeout_seconds),
            "run_timeout_seconds must be 0 (unset) or 30..7200 seconds"
        );
        ensure!(
            (30..=7200).contains(&self.request_timeout_seconds),
            "request_timeout_seconds must be 30..7200 seconds"
        );
        ensure!(
            (30..=86400).contains(&self.idle_timeout_seconds),
            "idle_timeout_seconds must be 30..86400 seconds"
        );
        ensure!(
            matches!(self.task_timeout_seconds, None | Some(0) | Some(30..=604800)),
            "task_timeout_seconds must be 0 (unlimited) or 30..604800 seconds"
        );
        ensure!(
            (1..=1_000_000).contains(&self.task_action_limit),
            "task_action_limit must be 1..1000000"
        );
        ensure!(
            self.vm.vnc_port > 0
                && self.vm.vnc_port <= 65472
                && self.vm.vnc_view_port == self.vm.vnc_port + 1,
            "VNC view port must follow the control port, leaving space for 32 screens nonzero ports"
        );
        ensure!(
            self.allowed_origins.len() <= 8,
            "at most eight extra origins are allowed"
        );
        ensure!(self.confirmed_http_origins.len()<=8,"At most eight unencrypted connection addresses can be confirmed");
        for origin in &self.confirmed_http_origins {
            let address=reqwest::Url::parse(origin)?;
            ensure!(address.scheme()=="http" && address.username().is_empty() && address.password().is_none() && address.query().is_none() && address.fragment().is_none() && address.path()=="/" && address.origin().ascii_serialization()==*origin,"Confirmed addresses must be complete HTTP addresses without a path or credentials");
            ensure!(self.allowed_origins.iter().any(|s|s.trim_end_matches('/')==origin),"Confirmed HTTP addresses must also be selected connection addresses");
        }
        for origin in &self.allowed_origins {
            let mut single = self.clone();
            single.allowed_origins.clear();
            single.confirmed_http_origins.clear();
            let mut address=reqwest::Url::parse(origin)?;
            // Preserve legacy private HTTP. Other HTTP is accepted only by an
            // explicit exact consent list; authentication remains unchanged.
            if address.scheme()=="http" && (private_network_host(address.host_str().unwrap_or("")) || self.confirmed_http_origins.iter().any(|s|s==origin.trim_end_matches('/'))) {
                address.set_scheme("https").map_err(|_|anyhow::anyhow!("Invalid origin scheme"))?;
            }
            single.public_url = address.to_string();
            single.validate()?;
        }
        let url = reqwest::Url::parse(&self.public_url)?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "public_url must be an origin, without credentials or a path"
        );
        let local = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        ensure!(
            url.scheme() == "https" || (local && url.scheme() == "http"),
            "remote public_url requires HTTPS"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_private_origins_allow_phone_access_without_public_http() {
        for origin in ["http://100.64.1.2:9444", "http://192.168.1.5:9444", "http://laptop.example.ts.net:9444"] {
            let mut c=Config::default();c.allowed_origins=vec![origin.into()];c.validate().unwrap();
            assert!(c.allows_origin(origin));assert!(!c.allows_origin("http://attacker.example"));
        }
        for origin in ["http://example.com:9444","http://8.8.8.8:9444","http://user:pass@100.64.1.2:9444","http://100.64.1.2:9444/path"] {
            let mut c=Config::default();c.allowed_origins=vec![origin.into()];assert!(c.validate().is_err());
        }
    }
    #[test]
    fn rejects_command_and_option_injection() {
        for bad in [
            "-oProxyCommand=sh",
            "bot;id",
            "bot\nstart",
            "user@host",
            "$(id)",
        ] {
            assert!(!safe_name(bad));
        }
        let mut c = Config::default();
        c.vm.codex_binary = "/bin/codex; id".into();
        assert!(c.validate().is_err());
    }
    #[test]
    fn task_limits_roundtrip_and_validate() {
        let mut c = Config::default();
        c.validate().unwrap();
        assert_eq!((c.request_timeout_seconds, c.idle_timeout_seconds), (600, 1800));
        assert_eq!((c.task_timeout_seconds(), c.task_action_limit()), (0, 10_000));
        c.task_timeout_seconds = Some(86_400);
        c.idle_timeout_seconds = 3600;
        c.task_action_limit = 500;
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        back.validate().unwrap();
        assert_eq!(back.task_timeout_seconds, Some(86_400));
        assert_eq!((back.idle_timeout_seconds, back.task_action_limit), (3600, 500));
        let check = |edit: &dyn Fn(&mut Config)| { let mut c = Config::default(); edit(&mut c); c.validate().is_ok() };
        assert!(check(&|c| c.task_timeout_seconds = Some(0)));
        assert!(check(&|c| c.task_timeout_seconds = Some(604_800)));
        for bad in [1, 29, 604_801] { assert!(!check(&|c| c.task_timeout_seconds = Some(bad))); }
        for bad in [0, 29, 7201] { assert!(!check(&|c| c.request_timeout_seconds = bad)); }
        for bad in [0, 29, 86_401] { assert!(!check(&|c| c.idle_timeout_seconds = bad)); }
        for bad in [0, 1_000_001] { assert!(!check(&|c| c.task_action_limit = bad)); }
        for bad in [1, 29, 7201] { assert!(!check(&|c| c.run_timeout_seconds = bad)); }
        assert!(!check(&|c| c.max_steps = 101));
    }
    #[test]
    fn legacy_limits_migrate_without_reviving_the_old_cutoff() {
        let parse = |text: &str| { let c: Config = toml::from_str(text).unwrap(); c.validate().unwrap(); c };
        // Existing installs wrote the old default explicitly.
        let old = parse("max_steps = 0\nrun_timeout_seconds = 1800\n");
        assert_eq!((old.task_timeout_seconds(), old.task_action_limit()), (0, 10_000));
        assert_eq!(old.idle_timeout_seconds, 1800);
        let custom = parse("max_steps = 40\nrun_timeout_seconds = 3600\n");
        assert_eq!((custom.task_timeout_seconds(), custom.task_action_limit()), (3600, 40));
        let explicit = parse("run_timeout_seconds = 3600\ntask_timeout_seconds = 0\n");
        assert_eq!(explicit.task_timeout_seconds(), 0);
        let explicit = parse("run_timeout_seconds = 3600\ntask_timeout_seconds = 7200\n");
        assert_eq!(explicit.task_timeout_seconds(), 7200);
        assert_eq!(parse("max_steps = 100\ntask_action_limit = 20\n").task_action_limit(), 20);
        assert_eq!(parse("").task_timeout_seconds(), 0);
    }
    #[test]
    fn remote_requires_tls() {
        let mut c = Config::default();
        c.public_url = "http://server.example".into();
        assert!(c.validate().is_err());
        c.public_url = "https://server.example".into();
        assert!(c.validate().is_ok());
    }
}

#[cfg(test)]
mod connection_access_tests {
    #[test]
    fn connection_access_confirmed_http_is_exact_bounded_and_revocable() {
        use super::Config;
        let origin="http://203.0.113.7:9444";
        let mut value=serde_json::to_value(Config::default()).unwrap();
        value["allowed_origins"]=serde_json::json!([origin]);
        assert!(serde_json::from_value::<Config>(value.clone()).unwrap().validate().is_err());
        value["confirmed_http_origins"]=serde_json::json!([origin]);
        let c:Config=serde_json::from_value(value.clone()).unwrap();c.validate().unwrap();
        assert!(c.allows_origin(origin));assert!(!c.allows_origin("http://203.0.113.8:9444"));assert!(!c.allows_origin("https://203.0.113.7:9444"));
        value["confirmed_http_origins"]=serde_json::json!(["http://203.0.113.8:9444"]);
        assert!(serde_json::from_value::<Config>(value.clone()).unwrap().validate().is_err());
        value["allowed_origins"]=serde_json::json!([]);value["confirmed_http_origins"]=serde_json::json!([]);
        let revoked:Config=serde_json::from_value(value).unwrap();revoked.validate().unwrap();assert!(!revoked.allows_origin(origin));
    }
}
