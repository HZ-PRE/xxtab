use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::HashSet, net::SocketAddr};

pub struct Prepared {
    pub text: String,
    pub source: SocketAddr,
}

/// Parse only portable WireGuard settings. Hooks are deliberately not executed.
pub fn prepare(input: &str, endpoint: SocketAddr, mtu: u16) -> Result<Prepared> {
    let mut section = "";
    let mut peers = 0;
    let mut interface = Vec::new();
    let mut peer = Vec::new();
    let mut seen = HashSet::new();
    let mut source_port = 0u16;
    for raw in input.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            match line {
                "[Interface]" if section.is_empty() => section = "Interface",
                "[Peer]" if section == "Interface" => {
                    section = "Peer";
                    peers += 1;
                }
                _ => bail!("expected one [Interface] followed by exactly one [Peer]"),
            }
            continue;
        }
        let (key, value) = line.split_once('=').context("invalid WireGuard setting")?;
        let (key, value) = (key.trim(), value.trim());
        ensure!(
            !value.is_empty() && seen.insert(format!("{section}.{key}")),
            "empty or repeated WireGuard field: {key}"
        );
        match (section, key) {
            ("Interface", "PrivateKey") | ("Peer", "PublicKey" | "PresharedKey") => {
                ensure!(
                    STANDARD.decode(value).is_ok_and(|k| k.len() == 32),
                    "invalid 32-byte key in {key}"
                );
            }
            ("Interface", "Address") | ("Peer", "AllowedIPs") => {
                for net in value.split(',') {
                    net.trim()
                        .parse::<ipnet::IpNet>()
                        .with_context(|| format!("invalid network in {key}"))?;
                }
            }
            ("Interface", "ListenPort") => {
                source_port = value.parse().context("invalid ListenPort")?;
            }
            ("Interface", "DNS") => {
                for ip in value.split(',') {
                    ip.trim()
                        .parse::<std::net::IpAddr>()
                        .context("DNS must contain IP addresses")?;
                }
            }
            ("Interface", "MTU") | ("Peer", "Endpoint" | "PersistentKeepalive") => {}
            _ => bail!(
                "unsupported WireGuard field {key}; hooks, SaveConfig and custom tables are unsupported"
            ),
        }
        if matches!(
            key,
            "MTU" | "Endpoint" | "PersistentKeepalive" | "ListenPort"
        ) {
            continue;
        }
        // Windows /0 enables WireGuard's restrictive firewall, blocking the outer TCP socket.
        // Explicit /1 routes retain full routing without enabling that firewall mode.
        let value = if key == "AllowedIPs" {
            value
                .split(',')
                .flat_map(|n| {
                    match n
                        .trim()
                        .parse::<ipnet::IpNet>()
                        .unwrap()
                        .trunc()
                        .to_string()
                        .as_str()
                    {
                        "0.0.0.0/0" => vec!["0.0.0.0/1".to_string(), "128.0.0.0/1".to_string()],
                        "::/0" => vec!["::/1".to_string(), "8000::/1".to_string()],
                        n => vec![n.to_string()],
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            value.into()
        };
        if section == "Interface" {
            interface.push(format!("{key} = {value}"));
        } else {
            peer.push(format!("{key} = {value}"));
        }
    }
    ensure!(peers == 1, "exactly one WireGuard peer is required");
    for key in [
        "Interface.PrivateKey",
        "Interface.Address",
        "Peer.PublicKey",
        "Peer.AllowedIPs",
    ] {
        ensure!(seen.contains(key), "missing WireGuard field {key}");
    }
    if source_port == 0 {
        // Reserve an unused source port during selection. The OS WireGuard opens it after setup.
        loop {
            source_port = std::net::UdpSocket::bind("127.0.0.1:0")?
                .local_addr()?
                .port();
            if source_port != endpoint.port() {
                break;
            }
        }
    }
    ensure!(
        source_port != endpoint.port(),
        "WireGuard ListenPort must differ from relay listen port"
    );
    let text = format!(
        "[Interface]\n{}\nListenPort = {source_port}\nMTU = {mtu}\n\n[Peer]\n{}\nEndpoint = {endpoint}\nPersistentKeepalive = 25\n",
        interface.join("\n"),
        peer.join("\n")
    );
    Ok(Prepared {
        text,
        source: SocketAddr::from(([127, 0, 0, 1], source_port)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> String {
        format!(
            "[Interface]\nPrivateKey = {}\nAddress = 10.0.0.2/32\nListenPort = 51822\n[Peer]\nPublicKey = {}\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = old:1\n",
            STANDARD.encode([1; 32]),
            STANDARD.encode([2; 32])
        )
    }
    #[test]
    fn rewrites_endpoint_and_default_routes() {
        let p = prepare(&sample(), "127.0.0.1:51821".parse().unwrap(), 1280).unwrap();
        assert!(
            p.text
                .contains("AllowedIPs = 0.0.0.0/1, 128.0.0.0/1, ::/1, 8000::/1")
        );
        assert!(p.text.contains("Endpoint = 127.0.0.1:51821"));
        assert!(!p.text.contains("old:1"));
        assert_eq!(p.source.port(), 51822);
    }
    #[test]
    fn rejects_hooks_and_extra_peers() {
        let e = "127.0.0.1:51821".parse().unwrap();
        assert!(
            prepare(
                &sample().replace("Address =", "PostUp = bad\nAddress ="),
                e,
                1280
            )
            .is_err()
        );
        assert!(prepare(&(sample() + "[Peer]\n"), e, 1280).is_err());
        assert!(prepare(&sample().replace("51822", "51821"), e, 1280).is_err());
    }
}
