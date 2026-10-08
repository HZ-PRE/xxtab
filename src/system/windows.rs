//! Recovery is opt-in by provenance: an unlocked session journal AND the
//! service's exact executable/config arguments must identify our last session.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::windows::ffi::OsStringExt,
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SUCCESS, LocalFree,
    },
    NetworkManagement::IpHelper::{
        CreateIpForwardEntry2, DeleteIpForwardEntry2, GetBestRoute2, InitializeIpForwardEntry,
        MIB_IPFORWARD_ROW2,
    },
    Networking::WinSock::{AF_INET, IN_ADDR, IN_ADDR_0, SOCKADDR_IN, SOCKADDR_INET},
    Security::{
        Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::{
        Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
            QueryServiceConfigW, QueryServiceStatusEx, SC_HANDLE, SC_MANAGER_CONNECT,
            SC_STATUS_PROCESS_INFO, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
            SERVICE_STATUS_PROCESS,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::Shell::CommandLineToArgvW,
};

fn ipv4_socket(address: Ipv4Addr) -> SOCKADDR_INET {
    SOCKADDR_INET {
        Ipv4: SOCKADDR_IN {
            sin_family: AF_INET,
            sin_addr: IN_ADDR {
                S_un: IN_ADDR_0 {
                    S_addr: u32::from_ne_bytes(address.octets()),
                },
            },
            ..Default::default()
        },
    }
}

fn route_error(operation: &str, error: u32) -> anyhow::Error {
    let detail = std::io::Error::from_raw_os_error(error as i32);
    anyhow::anyhow!("Windows {operation}失败（错误 {error}: {detail}）")
}

fn is_exact_host_route(row: &MIB_IPFORWARD_ROW2, destination: Ipv4Addr) -> bool {
    row.DestinationPrefix.PrefixLength == 32
        && unsafe { row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr }
            == u32::from_ne_bytes(destination.octets())
}

pub(super) struct BypassRoute {
    row: MIB_IPFORWARD_ROW2,
}

impl BypassRoute {
    pub(super) fn remove(&self) -> Result<()> {
        let result = unsafe { DeleteIpForwardEntry2(&self.row) };
        ensure!(
            result == ERROR_SUCCESS,
            "{}",
            route_error("清理服务器绕行路由", result)
        );
        Ok(())
    }
}

pub(super) fn install_bypass_route(destination: Ipv4Addr) -> Result<Option<BypassRoute>> {
    // Query and update the kernel routing table directly. NetTCPIP PowerShell
    // cmdlets can block while their provider is loading or refreshing adapters.
    let destination_address = ipv4_socket(destination);
    let mut best = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();
    let result = unsafe {
        GetBestRoute2(
            std::ptr::null(),
            0,
            std::ptr::null(),
            &destination_address,
            0,
            &mut best,
            &mut source,
        )
    };
    ensure!(
        result == ERROR_SUCCESS,
        "{}",
        route_error("查找服务器出口路由", result)
    );
    if is_exact_host_route(&best, destination) {
        return Ok(None);
    }

    let mut row = MIB_IPFORWARD_ROW2::default();
    unsafe { InitializeIpForwardEntry(&mut row) };
    row.InterfaceLuid = best.InterfaceLuid;
    row.DestinationPrefix.Prefix = destination_address;
    row.DestinationPrefix.PrefixLength = 32;
    row.NextHop = best.NextHop;
    row.Metric = 1;
    let result = unsafe { CreateIpForwardEntry2(&row) };
    ensure!(
        result == ERROR_SUCCESS,
        "{}",
        route_error("添加服务器绕行路由", result)
    );
    Ok(Some(BypassRoute { row }))
}

pub(super) fn current_user_sid() -> Result<String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    let query = || -> Result<String> {
        // Read our process token directly; PowerShell's WindowsIdentity API is
        // unavailable under ConstrainedLanguage. OwnedHandle closes on errors.
        unsafe {
            let mut raw_token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let token = OwnedHandle::from_raw_handle(raw_token);
            let mut size = 0;
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut size,
            );
            ensure!(
                size as usize >= std::mem::size_of::<TOKEN_USER>(),
                "invalid token information size"
            );
            // TOKEN_USER contains pointers, so the backing buffer must be aligned.
            let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
            if GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut text = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let sid = String::from_utf16(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            Ok(sid?)
        }
    };
    query().context("Windows 查询配置文件权限所需的用户身份失败")
}

#[derive(Deserialize, Serialize)]
struct Session {
    config: PathBuf,
    executable: PathBuf,
}

struct ServiceHandle(SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn service_error(operation: &str, name: &str) -> anyhow::Error {
    let error = std::io::Error::last_os_error();
    anyhow::anyhow!("Windows {operation} WireGuard 服务 {name}失败：{error}")
}

fn open_service(name: &str, access: u32) -> Result<Option<ServiceHandle>> {
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    ensure!(
        !manager.is_null(),
        "{}",
        service_error("打开服务管理器", name)
    );
    let manager = ServiceHandle(manager);
    let service_name = wide_nul(&format!("WireGuardTunnel${name}"));
    let service = unsafe { OpenServiceW(manager.0, service_name.as_ptr(), access) };
    if service.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
            return Ok(None);
        }
        return Err(anyhow::anyhow!(
            "Windows 打开 WireGuard 服务 {name}失败：{error}"
        ));
    }
    Ok(Some(ServiceHandle(service)))
}

pub(super) fn service_exists(name: &str) -> Result<bool> {
    Ok(open_service(name, SERVICE_QUERY_STATUS)?.is_some())
}

pub(super) fn service_running(name: &str) -> Result<bool> {
    let Some(service) = open_service(name, SERVICE_QUERY_STATUS)? else {
        bail!("WireGuard 服务 {name} 尚未创建")
    };
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut size = 0;
    let ok = unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut size,
        )
    };
    ensure!(ok != 0, "{}", service_error("查询状态", name));
    Ok(status.dwCurrentState == SERVICE_RUNNING)
}

fn service_command(name: &str) -> Result<String> {
    let Some(service) = open_service(name, SERVICE_QUERY_CONFIG)? else {
        bail!("WireGuard 服务 {name} 不存在")
    };
    let mut needed = 0;
    let ok = unsafe { QueryServiceConfigW(service.0, std::ptr::null_mut(), 0, &mut needed) };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        ensure!(
            error.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32) && needed > 0,
            "Windows 查询 WireGuard 服务 {name}启动信息失败：{error}"
        );
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    let ok = unsafe {
        QueryServiceConfigW(
            service.0,
            buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>(),
            needed,
            &mut needed,
        )
    };
    ensure!(ok != 0, "{}", service_error("读取启动信息", name));
    let config = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    ensure!(
        !config.lpBinaryPathName.is_null(),
        "WireGuard 服务 {name} 未提供启动路径"
    );
    let mut length = 0;
    unsafe {
        while *config.lpBinaryPathName.add(length) != 0 {
            length += 1;
        }
        Ok(String::from_utf16(std::slice::from_raw_parts(
            config.lpBinaryPathName,
            length,
        ))?)
    }
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
    let command = service_command(&wg.name)
        .context("无法读取已有 WireGuard 服务的启动信息，未进行自动清理")?;
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
    fn native_user_sid_matches_whoami() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"));
        let result = output("whoami.exe", &args(&["/user", "/fo", "csv", "/nh"])).unwrap();
        assert!(result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stdout)
                .split('"')
                .any(|field| field == sid)
        );
    }

    #[test]
    fn native_service_queries_do_not_need_powershell() {
        let name = "xxtab-service-that-does-not-exist";
        assert!(!service_exists(name).unwrap());
        assert!(service_running(name).is_err());
        assert!(service_command(name).is_err());
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
