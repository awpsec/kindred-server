use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::Path};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub public_url: String,
    pub allowed_origins: Vec<String>,
    pub database: String,
    pub token_env: String,
    pub profiles: Profiles,
    pub vm: Vm,
    pub openrouter: OpenRouter,
    pub pi: Pi,
    pub max_steps: usize,
    pub max_parallel_runs: usize,
    pub run_timeout_seconds: u64,
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
            database: "data/kindred.db".into(),
            token_env: "KINDRED_TOKEN".into(),
            profiles: Profiles::default(),
            vm: Vm::default(),
            openrouter: OpenRouter::default(),
            pi: Pi::default(),
            max_steps: 0,
            max_parallel_runs: 4,
            run_timeout_seconds: 1800,
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
impl Config {
    pub fn allows_origin(&self, origin: &str) -> bool {
        origin == self.public_url.trim_end_matches('/')
            || self
                .allowed_origins
                .iter()
                .any(|v| origin == v.trim_end_matches('/'))
    }
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = toml::from_str(
            &std::fs::read_to_string(path).context("read configuration; run kindred init first")?,
        )?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
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
            "max_steps must be 0 (unlimited) or between 1 and 100"
        );
        ensure!(
            (30..=7200).contains(&self.run_timeout_seconds),
            "run timeout must be 30..7200 seconds"
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
        for origin in &self.allowed_origins {
            let mut single = self.clone();
            single.allowed_origins.clear();
            single.public_url = origin.clone();
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
    fn remote_requires_tls() {
        let mut c = Config::default();
        c.public_url = "http://server.example".into();
        assert!(c.validate().is_err());
        c.public_url = "https://server.example".into();
        assert!(c.validate().is_ok());
    }
}
