use crate::{config::WireGuard, wgconfig::Prepared};
use anyhow::{Context, Result, bail, ensure};
use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(windows)]
mod windows;

fn output(program: &str, args: &[String]) -> Result<std::process::Output> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(target_os = "macos")]
    cmd.env_clear()
        .env(
            "PATH",
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        )
        .env("HOME", "/var/root")
        .env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot execute {program}"))?;
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn run(program: &str, args: &[String]) -> Result<String> {
    let o = output(program, args)?;
    // wg-quick may echo commands containing secrets; never forward its stderr.
    ensure!(
        o.status.success(),
        "{program} failed with status {}; check administrator/root rights, installed tools and configuration",
        o.status
    );
    Ok(String::from_utf8_lossy(&o.stdout).trim().into())
}
fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).into()).collect()
}
#[cfg(windows)]
fn ps(script: &str) -> Result<String> {
    run(
        "powershell.exe",
        &args(&[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("$ErrorActionPreference='Stop'; {script}"),
        ]),
    )
}

pub struct Route {
    delete: Vec<String>,
}
impl Route {
    pub fn install(ip: Ipv4Addr) -> Result<Option<Self>> {
        if ip.is_loopback() {
            return Ok(None);
        }
        #[cfg(windows)]
        {
            let found = ps(&format!(
                "$r = Get-NetRoute -AddressFamily IPv4 -PolicyStore ActiveStore | Where-Object {{ $_.DestinationPrefix -eq '{ip}/32' }}; if ($r) {{ 'exists' }}"
            ))?;
            if found == "exists" {
                return Ok(None);
            }
            let data = ps(&format!(
                "Find-NetRoute -RemoteIPAddress '{ip}' | Where-Object {{ $null -ne $_.NextHop }} | Select-Object -First 1 InterfaceIndex,NextHop | ConvertTo-Json -Compress"
            ))?;
            let v: serde_json::Value =
                serde_json::from_str(&data).context("cannot find physical route to server")?;
            let index = v["InterfaceIndex"]
                .as_u64()
                .context("missing route interface")?;
            let hop: Ipv4Addr = v["NextHop"].as_str().context("missing gateway")?.parse()?;
            ps(&format!(
                "New-NetRoute -DestinationPrefix '{ip}/32' -InterfaceIndex {index} -NextHop '{hop}' -RouteMetric 1 -PolicyStore ActiveStore | Out-Null"
            ))?;
            Ok(Some(Self {
                delete: vec![format!(
                    "Get-NetRoute -DestinationPrefix '{ip}/32' -InterfaceIndex {index} -NextHop '{hop}' -PolicyStore ActiveStore | Remove-NetRoute -Confirm:$false"
                )],
            }))
        }
        #[cfg(target_os = "linux")]
        {
            let prefix = format!("{ip}/32");
            let existing = run(
                "ip",
                &args(&["-j", "-4", "route", "show", "exact", &prefix]),
            )?;
            if !serde_json::from_str::<Vec<serde_json::Value>>(&existing)?.is_empty() {
                return Ok(None);
            }
            let data = run("ip", &args(&["-j", "-4", "route", "get", &ip.to_string()]))?;
            let v: Vec<serde_json::Value> = serde_json::from_str(&data)?;
            let route = v.first().context("no route to server")?;
            let dev = route["dev"].as_str().context("route has no device")?;
            let mut add = args(&["-4", "route", "add", &prefix]);
            if let Some(gateway) = route["gateway"].as_str() {
                add.extend(args(&["via", gateway]));
            }
            add.extend(args(&["dev", dev]));
            run("ip", &add)?;
            add[2] = "del".into();
            Ok(Some(Self { delete: add }))
        }
        #[cfg(target_os = "macos")]
        {
            let ip = ip.to_string();
            let data = run("/sbin/route", &args(&["-n", "get", "-inet", &ip]))?;
            let field = |key: &str| {
                data.lines().find_map(|line| {
                    let (name, value) = line.trim().split_once(':')?;
                    (name == key).then(|| value.trim())
                })
            };
            if field("destination") == Some(ip.as_str())
                && field("flags").is_some_and(|v| v.split([',', '<', '>']).any(|f| f == "HOST"))
            {
                return Ok(None);
            }
            let mut add = args(&["-n", "add", "-host", &ip]);
            if let Some(gateway) = field("gateway").and_then(|g| g.parse::<Ipv4Addr>().ok()) {
                add.extend(args(&["-gateway", &gateway.to_string()]));
            } else {
                let interface = field("interface").context("missing outer route interface")?;
                add.extend(args(&["-interface", interface]));
            }
            run("/sbin/route", &add)?;
            add[1] = "delete".into();
            Ok(Some(Self { delete: add }))
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        bail!("managed mode supports Windows, Linux and macOS")
    }
    fn remove(&self) -> Result<()> {
        #[cfg(windows)]
        {
            ps(&self.delete[0])?;
        }
        #[cfg(target_os = "linux")]
        {
            run("ip", &self.delete)?;
        }
        #[cfg(target_os = "macos")]
        run("/sbin/route", &self.delete)?;
        Ok(())
    }
}

pub struct Managed {
    _lock: std::fs::File,
    directory: Option<tempfile::TempDir>,
    config: PathBuf,
    name: String,
    executable: String,
    route: Option<Route>,
    started: bool,
}
impl Managed {
    pub fn prepare(wg: &WireGuard, prepared: &Prepared) -> Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(target_os = "macos")]
        let lock_root = {
            use std::os::unix::fs::OpenOptionsExt;
            ensure!(
                unsafe { libc::geteuid() } == 0,
                "WireGuard management requires administrator authorization"
            );
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            PathBuf::from("/var/run")
        };
        #[cfg(not(target_os = "macos"))]
        let lock_root = std::env::temp_dir();
        let lock = options.open(lock_root.join(format!("xxtab-{}.lock", wg.name)))?;
        lock.try_lock()
            .context("another xxtab process manages this WireGuard name")?;
        #[cfg(windows)]
        windows::recover(&lock, wg)?;
        #[cfg(target_os = "linux")]
        {
            ensure!(
                !output("ip", &args(&["link", "show", "dev", &wg.name]))?
                    .status
                    .success(),
                "WireGuard interface already exists; choose another wireguard.name"
            );
        }
        #[cfg(target_os = "macos")]
        ensure!(
            !Path::new("/var/run/wireguard")
                .join(format!("{}.name", wg.name))
                .exists(),
            "WireGuard interface mapping already exists; choose another wireguard.name"
        );
        let directory = tempfile::Builder::new().prefix("xxtab-").tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(windows)]
        {
            let sid = ps("[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value")?;
            ensure!(
                sid.starts_with("S-1-")
                    && sid
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-'),
                "invalid user SID"
            );
            run(
                "icacls.exe",
                &[
                    directory.path().to_string_lossy().into(),
                    "/inheritance:r".into(),
                    "/grant:r".into(),
                    format!("*{sid}:(OI)(CI)F"),
                    "*S-1-5-18:(OI)(CI)F".into(),
                    "*S-1-5-32-544:(OI)(CI)F".into(),
                ],
            )?;
        }
        let config = directory.path().join(format!("{}.conf", wg.name));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options.open(&config)?.write_all(prepared.text.as_bytes())?;
        let executable = wg
            .executable
            .clone()
            .unwrap_or_else(default_executable)
            .to_string_lossy()
            .into();
        Ok(Self {
            _lock: lock,
            directory: Some(directory),
            config,
            name: wg.name.clone(),
            executable,
            route: None,
            started: false,
        })
    }
    pub fn start(&mut self, server: Ipv4Addr) -> Result<()> {
        #[cfg(windows)]
        windows::record(&self._lock, &self.config, &self.executable)?;
        self.route = Route::install(server).context("cannot install server bypass route")?;
        self.started = true; // partial startup must also be rolled back
        #[cfg(windows)]
        {
            run(
                &self.executable,
                &args(&["/installtunnelservice", &self.config.to_string_lossy()]),
            )?;
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let state = ps(&format!(
                    "(Get-Service -Name 'WireGuardTunnel${}').Status.ToString()",
                    self.name
                ))?;
                if state == "Running" {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "WireGuard service failed to start"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            run(
                &self.executable,
                &args(&["up", &self.config.to_string_lossy()]),
            )?;
        }
        crate::log!(
            "WireGuard {} started; runtime config: {}",
            self.name,
            self.config.display()
        );
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.started {
            #[cfg(windows)]
            {
                if windows::service_exists(&self.name)? {
                    windows::uninstall(&self.executable, &self.name)?;
                }
            }
            #[cfg(target_os = "linux")]
            {
                if output("ip", &args(&["link", "show", "dev", &self.name]))?
                    .status
                    .success()
                {
                    run(
                        &self.executable,
                        &args(&["down", &self.config.to_string_lossy()]),
                    )?;
                }
            }
            #[cfg(target_os = "macos")]
            {
                if Path::new("/var/run/wireguard")
                    .join(format!("{}.name", self.name))
                    .exists()
                {
                    run(
                        &self.executable,
                        &args(&["down", &self.config.to_string_lossy()]),
                    )?;
                }
            }
            self.started = false;
        }
        if let Some(route) = &self.route {
            route
                .remove()
                .context("cannot remove server bypass route")?;
        }
        self.route = None;
        #[cfg(windows)]
        self._lock.set_len(0)?;
        Ok(())
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        if let Err(err) = self.stop() {
            crate::log!(
                "cleanup failed: {err:#}; preserve runtime config {} for manual recovery",
                self.config.display()
            );
            if let Some(dir) = self.directory.take() {
                let _ = dir.keep();
            }
        }
    }
}
fn default_executable() -> PathBuf {
    if cfg!(windows) {
        Path::new(&std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into()))
            .join("WireGuard/wireguard.exe")
    } else if cfg!(target_os = "macos") {
        ["/opt/homebrew/bin/wg-quick", "/usr/local/bin/wg-quick"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/wg-quick"))
    } else {
        PathBuf::from("wg-quick")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    #[cfg(windows)]
    #[ignore = "requires admin and installed WireGuard; simulates process exit with an isolated tunnel"]
    fn windows_recovers_abandoned_session() {
        let input = format!(
            "[Interface]\nPrivateKey = {}\nAddress = 10.254.252.1/32\n[Peer]\nPublicKey = {}\nAllowedIPs = 10.254.252.2/32\n",
            STANDARD.encode([1; 32]),
            STANDARD.encode([2; 32])
        );
        let prepared =
            crate::wgconfig::prepare(&input, "127.0.0.1:51893".parse().unwrap(), 1280).unwrap();
        let wg = WireGuard {
            config: PathBuf::new(),
            name: "xxtabrecover".into(),
            executable: None,
            mtu: 1280,
        };
        if std::env::var_os("XXTAB_RECOVERY_TEST_CHILD").is_some() {
            let mut managed = Managed::prepare(&wg, &prepared).unwrap();
            managed.start(Ipv4Addr::LOCALHOST).unwrap();
            // Model forced termination: no destructors, OS releases the lock.
            std::process::exit(0);
        }
        assert!(
            !windows::service_exists(&wg.name).unwrap(),
            "test service already exists"
        );
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if windows::service_exists("xxtabrecover").unwrap_or(false) {
                    let _ =
                        windows::uninstall(&default_executable().to_string_lossy(), "xxtabrecover");
                }
            }
        }
        let _cleanup = Cleanup;
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "system::tests::windows_recovers_abandoned_session",
                "--ignored",
                "--nocapture",
            ])
            .env("XXTAB_RECOVERY_TEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(windows::service_exists(&wg.name).unwrap());
        let journal = std::env::temp_dir().join("xxtab-xxtabrecover.lock");
        let record = std::fs::read(&journal).unwrap();
        let info: serde_json::Value = serde_json::from_slice(&record).unwrap();
        let old_config = PathBuf::from(info["config"].as_str().unwrap());
        // No provenance: never remove a service merely because its name matches.
        std::fs::write(&journal, b"").unwrap();
        assert!(Managed::prepare(&wg, &prepared).is_err());
        assert!(windows::service_exists(&wg.name).unwrap());
        std::fs::write(&journal, record).unwrap();
        let mut recovered = Managed::prepare(&wg, &prepared).unwrap();
        assert!(!windows::service_exists(&wg.name).unwrap());
        assert!(!old_config.exists());
        recovered.start(Ipv4Addr::LOCALHOST).unwrap();
        recovered.stop().unwrap();
        drop(recovered);
        assert!(!windows::service_exists(&wg.name).unwrap());
        assert!(std::fs::read(journal).unwrap().is_empty());
    }

    #[test]
    #[ignore = "requires admin/root and installed WireGuard; creates and removes an isolated test interface"]
    fn managed_wireguard_lifecycle() {
        let input = format!(
            "[Interface]\nPrivateKey = {}\nAddress = 10.254.253.1/32\n[Peer]\nPublicKey = {}\nAllowedIPs = 10.254.253.2/32\n",
            STANDARD.encode([1; 32]),
            STANDARD.encode([2; 32])
        );
        let prepared =
            crate::wgconfig::prepare(&input, "127.0.0.1:51891".parse().unwrap(), 1280).unwrap();
        let wg = WireGuard {
            config: PathBuf::new(),
            name: "xxtabtest".into(),
            executable: None,
            mtu: 1280,
        };
        let mut managed = Managed::prepare(&wg, &prepared).unwrap();
        let path = managed.config.clone();
        assert!(
            Managed::prepare(&wg, &prepared).is_err(),
            "second owner accepted"
        );
        managed.start(Ipv4Addr::LOCALHOST).unwrap();
        managed.stop().unwrap();
        managed.stop().unwrap();
        drop(managed);
        assert!(!path.exists(), "private runtime config was not removed");
    }
    #[test]
    #[ignore = "requires admin/root; adds and removes one documentation-address host route"]
    fn bypass_route_lifecycle() {
        if let Some(route) = Route::install("198.51.100.42".parse().unwrap()).unwrap() {
            assert!(
                Route::install("198.51.100.42".parse().unwrap())
                    .unwrap()
                    .is_none()
            );
            route.remove().unwrap();
        }
    }
}
