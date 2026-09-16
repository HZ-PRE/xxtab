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
    #[cfg(windows)]
    if args.len() == 5 && args[0] == "update-install" {
        let result = xxtab::update::finish_install(
            args[1].parse()?,
            &args[2],
            Path::new(&args[3]),
            Path::new(&args[4]),
        );
        if let Err(error) = &result {
            let message: Vec<u16> = format!("更新未完成：{error:#}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let title: Vec<u16> = "xxtab 更新".encode_utf16().chain(Some(0)).collect();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                        | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
        }
        return result;
    }
    if args.first().is_some_and(|arg| arg == "update") {
        anyhow::ensure!(
            matches!(args.len(), 2 | 3)
                && matches!(args[1].as_str(), "check" | "download")
                && (args.len() == 2 || args[1] == "download"),
            "usage: xxtab update check | xxtab update download [version]"
        );
        let check = xxtab::update::check().await?;
        if args[1] == "check" {
            println!("{}", serde_json::to_string(&check)?);
        } else {
            anyhow::ensure!(check.available, "没有可用的新版本");
            let release = check.release.as_ref().unwrap();
            if let Some(expected) = args.get(2) {
                anyhow::ensure!(
                    expected == &release.version,
                    "最新版本已变化，请重新检查更新"
                );
            }
            println!(
                "{}",
                serde_json::to_string(&xxtab::update::download(release).await?)?
            );
        }
        return Ok(());
    }
    if args.len() == 1 && args[0] == "desktop" {
        return xxtab::desktop::serve();
    }
    #[cfg(target_os = "macos")]
    if args.len() == 3 && args[0] == "macos-session" {
        return xxtab::macos::session(Path::new(&args[1]), args[2].parse()?).await;
    }
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        println!(
            "xxtab {}\n\nUsage: xxtab <check|run|relay> <config.toml>\n       xxtab update check\n       xxtab update download [version]\n\n  check  Validate config offline; no network/system changes\n  run    Manage system WireGuard and embedded wstunnel (admin/root)\n  relay  Only run embedded wstunnel; first local UDP sender is pinned\n  update Check GitHub Releases or download a SHA256-verified update (JSON output)\n\nCtrl+C (Linux/macOS also SIGTERM) stops and cleans up managed WireGuard.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if args.len() != 2 || !matches!(args[0].as_str(), "check" | "run" | "relay") {
        bail!("usage: xxtab <check|run|relay> <config.toml>");
    }
    xxtab::app::run(Path::new(&args[1]), &args[0], shutdown()).await
}
