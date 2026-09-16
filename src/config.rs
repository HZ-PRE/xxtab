use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};
use url::Url;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: String,
    pub server_ip: Option<Ipv4Addr>,
    #[serde(default = "prefix")]
    pub path_prefix: String,
    #[serde(default = "remote")]
    pub remote: String,
    #[serde(default = "listen")]
    pub listen: SocketAddr,
    pub ca_file: Option<PathBuf>,
    pub wireguard: Option<WireGuard>,
    #[serde(default)]
    pub transport: Transport,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LatencyMode {
    #[default]
    Balanced,
    Interactive,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Transport {
    pub latency_mode: LatencyMode,
    pub diagnostics_interval_secs: u64,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            latency_mode: LatencyMode::Balanced,
            diagnostics_interval_secs: 15,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGuard {
    pub config: PathBuf,
    #[serde(default = "name")]
    pub name: String,
    pub executable: Option<PathBuf>,
    #[serde(default = "mtu")]
    pub mtu: u16,
}
fn prefix() -> String {
    "v1".into()
}
fn remote() -> String {
    "127.0.0.1:7007".into()
}
fn listen() -> SocketAddr {
    "127.0.0.1:51820".parse().unwrap()
}
fn name() -> String {
    "xxtab0".into()
}
fn mtu() -> u16 {
    1280
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).context("cannot read xxtab config")?;
        Self::parse(&content, path)
    }
    pub fn parse(content: &str, path: &Path) -> Result<Self> {
        // TOML parse errors may contain source lines: never print secrets/path tokens.
        let mut cfg: Self = toml::from_str(content).map_err(|_| {
            anyhow::anyhow!("invalid xxtab TOML; check field names and value types")
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        if let Some(ca) = &mut cfg.ca_file {
            *ca = base.join(&*ca);
        }
        if let Some(wg) = &mut cfg.wireguard {
            wg.config = base.join(&wg.config);
        }
        cfg.validate()?;
        Ok(cfg)
    }
    pub fn url(&self) -> Result<Url> {
        Url::parse(&self.server).map_err(|_| anyhow::anyhow!("invalid server URL"))
    }
    pub fn destination(&self) -> Result<(String, u16)> {
        let u =
            Url::parse(&format!("udp://{}", self.remote)).context("invalid remote host:port")?;
        ensure!(
            u.username().is_empty()
                && u.password().is_none()
                && u.path().is_empty()
                && u.query().is_none()
                && u.fragment().is_none(),
            "remote must be host:port"
        );
        let host = u.host_str().context("remote host missing")?.to_owned();
        let port = u.port().context("remote port missing")?;
        ensure!(port > 0, "remote port must be nonzero");
        Ok((host, port))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.transport.diagnostics_interval_secs == 0
                || (5..=3600).contains(&self.transport.diagnostics_interval_secs),
            "diagnostics_interval_secs must be 0 (disabled) or 5..3600"
        );
        let u = self.url()?;
        ensure!(
            matches!(u.scheme(), "ws" | "wss") && u.host_str().is_some(),
            "server must be ws://host:port or wss://host:port"
        );
        ensure!(
            u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
                && matches!(u.path(), "" | "/"),
            "use path_prefix for paths; URL credentials/query are unsupported"
        );
        ensure!(
            u.port_or_known_default().unwrap_or(0) > 0,
            "invalid server port"
        );
        ensure!(
            self.listen.ip() == std::net::IpAddr::V4(Ipv4Addr::LOCALHOST) && self.listen.port() > 0,
            "listen must be 127.0.0.1 with nonzero port"
        );
        ensure!(
            !self.path_prefix.is_empty()
                && self.path_prefix.len() <= 256
                && self
                    .path_prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "path_prefix must contain only ASCII letters, digits, -, _"
        );
        self.destination()?;
        if let Some(wg) = &self.wireguard {
            ensure!(
                !wg.name.is_empty()
                    && wg.name.len() <= 15
                    && wg
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "WireGuard name must be 1..15 ASCII letters/digits/underscores"
            );
            ensure!((1280..=1420).contains(&wg.mtu), "mtu must be 1280..1420");
        }
        if matches!(u.host(), Some(url::Host::Ipv6(_))) {
            bail!("this version requires an IPv4 outer server; WireGuard inner IPv6 is supported");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_defaults_and_validation() {
        let parse = |extra: &str| {
            Config::parse(
                &format!("server='wss://example.com'\n{extra}"),
                Path::new("x.toml"),
            )
        };
        assert!(parse("").unwrap().transport.latency_mode == LatencyMode::Balanced);
        assert!(
            parse("[transport]\nlatency_mode='interactive'\ndiagnostics_interval_secs=5")
                .unwrap()
                .transport
                .latency_mode
                == LatencyMode::Interactive
        );
        for invalid in [
            "latency_mode='invalid'",
            "diagnostics_interval_secs=1",
            "diagnostics_interval_secs=3601",
            "unknown=true",
        ] {
            assert!(parse(&format!("[transport]\n{invalid}")).is_err());
        }
        assert!(parse("[transport]\ndiagnostics_interval_secs=0").is_ok());
    }
}
