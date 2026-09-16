//! Recovery is opt-in by provenance: an unlocked session journal AND the
//! service's exact executable/config arguments must identify our last session.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::windows::ffi::OsStringExt,
};
use windows_sys::Win32::{Foundation::LocalFree, UI::Shell::CommandLineToArgvW};

#[derive(Deserialize, Serialize)]
struct Session {
    config: PathBuf,
    executable: PathBuf,
}

fn query_result(code: Option<i32>, name: &str) -> Result<bool> {
    match code {
        Some(0) => Ok(true),
        Some(1060) => Ok(false),
        Some(5) => bail!("无法查询 WireGuard 服务 {name}：访问被拒绝，请以管理员身份运行"),
        _ => bail!(
            "查询 WireGuard 服务 {name} 失败（退出码 {code:?}），请检查系统服务管理器和管理员权限"
        ),
    }
}

pub(super) fn service_exists(name: &str) -> Result<bool> {
    let result = output(
        "sc.exe",
        &args(&["query", &format!("WireGuardTunnel${name}")]),
    )?;
    query_result(result.status.code(), name)
}

pub(super) fn uninstall(executable: &str, name: &str) -> Result<()> {
    run(executable, &args(&["/uninstalltunnelservice", name]))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while service_exists(name)? {
        ensure!(
            Instant::now() < deadline,
            "WireGuard 服务 {name} 卸载尚未完成，请稍后重试"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn command_args(command: &str) -> Result<Vec<PathBuf>> {
    let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
    let mut count = 0;
    // WireGuard quotes paths using the Windows command-line convention.
    unsafe {
        let argv = CommandLineToArgvW(wide.as_ptr(), &mut count);
        ensure!(!argv.is_null(), "cannot parse WireGuard service command");
        let result = std::slice::from_raw_parts(argv, count as usize)
            .iter()
            .map(|&arg| {
                let mut len = 0;
                while *arg.add(len) != 0 {
                    len += 1;
                }
                PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
                    arg, len,
                )))
            })
            .collect();
        LocalFree(argv.cast());
        Ok(result)
    }
}

impl Session {
    fn matches(&self, command: &str, wg: &WireGuard, root: &Path) -> bool {
        let Some(directory) = self.config.parent() else {
            return false;
        };
        let expected_exe = wg.executable.clone().unwrap_or_else(default_executable);
        // Refuse edited/stale journals, non-runtime paths and foreign services.
        directory.parent() == Some(root)
            && directory
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("xxtab-") && s.len() > 6)
            && self.config.file_name() == Some(std::ffi::OsStr::new(&format!("{}.conf", wg.name)))
            && self.executable == expected_exe
            && command_args(command).is_ok_and(|args| {
                args == [
                    self.executable.clone(),
                    PathBuf::from("/tunnelservice"),
                    self.config.clone(),
                ]
            })
    }
}

pub(super) fn record(mut lock: &File, config: &Path, executable: &str) -> Result<()> {
    let session = Session {
        config: config.into(),
        executable: executable.into(),
    };
    lock.seek(SeekFrom::Start(0))?;
    lock.set_len(0)?;
    serde_json::to_writer(&mut lock, &session)?;
    lock.flush()?;
    lock.sync_data()?;
    Ok(())
}

pub(super) fn recover(mut lock: &File, wg: &WireGuard) -> Result<()> {
    if !service_exists(&wg.name)? {
        return Ok(());
    }
    lock.seek(SeekFrom::Start(0))?;
    let session: Option<Session> = serde_json::from_reader(lock.take(16 * 1024)).ok();
    let conflict = || {
        anyhow::anyhow!(
            "WireGuard 同名服务 WireGuardTunnel${} 已存在，无法确认属于本程序的上次连接；请先在原客户端断开，或修改 wireguard.name",
            wg.name
        )
    };
    let session = session.ok_or_else(conflict)?;
    // Names are validated as ASCII alphanumerics, '_' and '-' by Config.
    let command = ps(&format!(
        "(Get-CimInstance Win32_Service -Filter 'Name = \"WireGuardTunnel${}\"').PathName | ConvertTo-Json -Compress", wg.name
    )).context("无法读取已有 WireGuard 服务的启动信息，未进行自动清理")?;
    let command: String =
        serde_json::from_str(&command).context("已有 WireGuard 服务信息不可用，请稍后重试")?;
    ensure!(
        session.matches(&command, wg, &std::env::temp_dir()),
        "{}",
        conflict()
    );
    crate::log!(
        "检测到上次异常退出残留的 WireGuard 服务 {}，正在恢复",
        wg.name
    );
    // Keep the same-name lock held throughout uninstall and the next startup.
    uninstall(&session.executable.to_string_lossy(), &wg.name)?;
    // Delete only the recorded config and an empty directory; never recurse.
    if let Err(error) = std::fs::remove_file(&session.config)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::log!("无法清理旧运行时配置：{error}");
    }
    if let Some(directory) = session.config.parent() {
        let _ = std::fs::remove_dir(directory);
    }
    lock.set_len(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_query_failure_from_conflict() {
        assert!(query_result(Some(0), "test").unwrap());
        assert!(!query_result(Some(1060), "test").unwrap());
        assert!(
            query_result(Some(5), "test")
                .unwrap_err()
                .to_string()
                .contains("访问被拒绝")
        );
        assert!(query_result(Some(1722), "test").is_err());
        assert!(query_result(None, "test").is_err());
    }

    #[test]
    fn recovery_requires_exact_recorded_service() {
        let root = Path::new(r"C:\Users\Test User\Temp");
        let session = Session {
            config: root.join("xxtab-AbC123/xxtab0.conf"),
            executable: PathBuf::from(r"C:\Program Files\WireGuard\wireguard.exe"),
        };
        let wg = WireGuard {
            config: PathBuf::new(),
            name: "xxtab0".into(),
            executable: Some(session.executable.clone()),
            mtu: 1280,
        };
        let command = format!(
            "\"{}\" /tunnelservice \"{}\"",
            session.executable.display(),
            session.config.display()
        );
        assert!(session.matches(&command, &wg, root));
        assert!(!session.matches(&command.replace("xxtab-AbC123", "other"), &wg, root));
        assert!(!session.matches(
            &command.replace("/tunnelservice", "/installtunnelservice"),
            &wg,
            root
        ));
        assert!(!session.matches(&(command.clone() + " extra"), &wg, root));
        assert!(!session.matches(&command, &wg, Path::new(r"C:\OtherUser\Temp")));
        let foreign = Session {
            config: root.join("foreign/xxtab0.conf"),
            executable: session.executable.clone(),
        };
        let foreign_command = format!(
            "\"{}\" /tunnelservice \"{}\"",
            foreign.executable.display(),
            foreign.config.display()
        );
        assert!(!foreign.matches(&foreign_command, &wg, root));
    }
}
