use crate::{config, logging, system, transport, wgconfig};
use anyhow::{Context, Result, bail};
use std::{
    future::Future,
    net::IpAddr,
    path::Path,
    sync::{Arc, atomic::Ordering::Relaxed},
};

pub async fn run(path: &Path, mode: &str, stop: impl Future<Output = Result<()>>) -> Result<()> {
    let cfg = config::Config::load(path)?;
    let prepared = if mode != "relay" {
        if let Some(wg) = &cfg.wireguard {
            Some(wgconfig::prepare(
                &std::fs::read_to_string(&wg.config).context("cannot read WireGuard config")?,
                cfg.listen,
                wg.mtu,
            )?)
        } else {
            None
        }
    } else {
        None
    };
    let tls = transport::tls_connector(&cfg)?;
    if mode == "check" {
        println!("configuration valid; no connection or system changes made");
        return Ok(());
    }
    if mode == "run" && prepared.is_none() {
        bail!("run requires a [wireguard] section");
    }
    tokio::pin!(stop);
    logging::state(logging::Status::Connecting);
    let stats = Arc::new(transport::Stats::default());
    let udp = Arc::new(
        tokio::net::UdpSocket::bind(cfg.listen)
            .await
            .context("cannot bind relay UDP port")?,
    );
    let startup = async {
        let address = transport::resolve(&cfg).await?;
        let ws = transport::connect(&cfg, address, tls.clone()).await?;
        Ok::<_, anyhow::Error>((address, ws))
    };
    let (address, ws) = tokio::select! {
        biased;
        r = &mut stop => { r?; return Ok(()); }
        r = startup => r?,
    };
    let source = prepared.as_ref().map(|p| p.source);
    let mut managed = if let Some(p) = &prepared {
        let mut m = system::Managed::prepare(cfg.wireguard.as_ref().unwrap(), p)?;
        let IpAddr::V4(ip) = address.ip() else {
            unreachable!()
        };
        m.start(ip)?;
        Some(m)
    } else {
        None
    };
    drop(prepared);
    crate::log!(
        "tunnel ready: {} -> {address}; UDP remote {}; transport {}",
        cfg.listen,
        cfg.remote,
        cfg.url()?.scheme()
    );
    logging::state(logging::Status::Connected);
    let result = tokio::select! {
        biased;
        r = &mut stop => { r?; Ok(()) }
        r = transport::run(&cfg, address, tls, ws, udp, source, stats.clone()) => r,
    };
    if let Some(m) = &mut managed {
        m.stop()?;
    }
    crate::log!(
        "stopped: tx={} bytes rx={} bytes dropped={} packets reconnects={}",
        stats.tx.load(Relaxed),
        stats.rx.load(Relaxed),
        stats.dropped.load(Relaxed),
        stats.reconnects.load(Relaxed)
    );
    result
}
