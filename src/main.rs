use anyhow::{Result, bail};
use std::path::Path;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(err) = execute().await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
async fn shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! { r = tokio::signal::ctrl_c() => r?, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }
    Ok(())
}
async fn execute() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        println!(
            "xxtab {}\n\nUsage: xxtab <check|run|relay> <config.toml>\n\n  check  Validate config offline; no network/system changes\n  run    Manage system WireGuard and embedded wstunnel (admin/root)\n  relay  Only run embedded wstunnel; first local UDP sender is pinned\n\nCtrl+C (Linux also SIGTERM) stops and cleans up managed WireGuard.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if args.len() != 2 || !matches!(args[0].as_str(), "check" | "run" | "relay") {
        bail!("usage: xxtab <check|run|relay> <config.toml>");
    }
    xxtab::app::run(Path::new(&args[1]), &args[0], shutdown()).await
}
