//! Short-lived elevated tunnel worker. The AppKit UI remains unprivileged.
use crate::{
    logging::{self, Feed, Status},
    profiles::Draft,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::CString,
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn complete_dependencies(root: &Path) -> bool {
    let quick = quick_path(root);
    [
        root.join("bash"),
        root.join("wg"),
        root.join("wireguard-go"),
        quick.clone(),
    ]
    .iter()
    .all(|path| {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.mode() & 0o111 != 0)
    }) && (quick.parent() == Some(root) || quick.with_file_name("wg-quick.bash").is_file())
}
fn quick_path(root: &Path) -> PathBuf {
    if root.ends_with("Contents/Helpers") {
        root.parent().unwrap().join("Resources/wireguard/wg-quick")
    } else {
        root.join("wg-quick")
    }
}
pub(crate) fn wireguard_executable() -> Option<PathBuf> {
    dependency_root().map(|root| quick_path(&root))
}
fn bundled_root(executable: &Path) -> Option<PathBuf> {
    let bin = executable.parent()?;
    let contents = bin.parent()?;
    (bin.file_name()? == "MacOS" && contents.file_name()? == "Contents")
        .then(|| contents.join("Helpers"))
}
pub(crate) fn dependency_root() -> Option<PathBuf> {
    if let Some(root) = std::env::current_exe()
        .ok()
        .and_then(|exe| bundled_root(&exe))
    {
        // A damaged app must not silently pass CI or run a different Homebrew binary.
        return complete_dependencies(&root).then_some(root);
    }
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .into_iter()
        .map(PathBuf::from)
        .find(|root| complete_dependencies(root))
}
pub(crate) fn command_path() -> Result<std::ffi::OsString> {
    let paths = dependency_root().into_iter().chain(
        ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .into_iter()
            .map(PathBuf::from),
    );
    std::env::join_paths(paths).context("application path cannot contain a colon")
}
pub fn dependencies_ready() -> bool {
    dependency_root().is_some()
}

struct SessionDir {
    directory: File,
    uid: u32,
}
impl SessionDir {
    fn open(path: &Path, uid: u32) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "cannot open session directory");
        let directory = unsafe { File::from_raw_fd(fd) };
        let meta = directory.metadata()?;
        ensure!(
            meta.uid() == uid && meta.mode() & 0o077 == 0,
            "session directory must be private and owned by requesting user"
        );
        Ok(Self { directory, uid })
    }
    fn open_input(&self, name: &str) -> Result<File> {
        let name = CString::new(name)?;
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        ensure!(fd >= 0, "session input unavailable");
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == self.uid
                && meta.nlink() == 1
                && meta.mode() & 0o077 == 0
                && meta.len() <= 512 * 1024,
            "invalid session input"
        );
        Ok(file)
    }
    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let file = self.open_input(name)?;
        let mut data = Vec::new();
        file.take(512 * 1024 + 1).read_to_end(&mut data)?;
        ensure!(data.len() <= 512 * 1024, "session input too large");
        Ok(data)
    }
    fn stopped(&self) -> bool {
        self.read("stop").is_ok()
    }
    fn snapshot_file(&self) -> Result<File> {
        let name = c"snapshot.json";
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(fd >= 0, "cannot create session snapshot");
        let file = unsafe { File::from_raw_fd(fd) };
        ensure!(
            unsafe { libc::fchown(fd, self.uid, !0) } == 0,
            "cannot assign snapshot owner"
        );
        Ok(file)
    }
}

// The GUI holds an exclusive flock for its lifetime, with close-on-exec set.
// A frozen/sleeping GUI keeps the lock; process exit releases it in the kernel.
// Keeping this descriptor open also avoids PID reuse and path replacement races.
fn owner_alive(file: &File) -> Result<bool> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0,
            "cannot release session owner probe"
        );
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error).context("cannot check session owner lock")
    }
}
struct Snapshots {
    file: File,
    lines: VecDeque<String>,
    sequence: u64,
    previous: Option<(Status, bool)>,
}
impl Snapshots {
    fn publish(&mut self, feed: &Arc<Mutex<Feed>>, finished: bool) -> Result<()> {
        let mut feed = feed.lock().unwrap_or_else(|e| e.into_inner());
        if feed.lines.is_empty() && self.previous == Some((feed.status, finished)) {
            return Ok(());
        }
        for line in feed.lines.drain(..) {
            self.lines.push_back(line.chars().take(1024).collect());
            self.sequence += 1;
        }
        while self.lines.len() > 128 {
            self.lines.pop_front();
        }
        let value = serde_json::to_vec(
            &json!({"status": format!("{:?}", feed.status), "lines": self.lines, "sequence": self.sequence, "finished": finished}),
        )?;
        self.file.rewind()?;
        self.file.write_all(&value)?;
        self.file.set_len(value.len() as u64)?;
        self.file.flush()?;
        self.previous = Some((feed.status, finished));
        Ok(())
    }
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

pub async fn session(path: &Path, uid: u32) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0 && uid > 0,
        "session requires administrator authorization and a non-root owner"
    );
    let directory = SessionDir::open(path, uid)?;
    let mut output = Snapshots {
        file: directory.snapshot_file()?,
        lines: VecDeque::new(),
        sequence: 0,
        previous: None,
    };
    let feed = Arc::new(Mutex::new(Feed::default()));
    logging::attach(feed.clone());
    let result = run_session(&directory, uid, &feed, &mut output).await;
    match &result {
        Ok(()) => logging::state(Status::Idle),
        Err(e) => {
            logging::state(Status::Failed);
            crate::log!("connection failed: {e:#}");
        }
    }
    output.publish(&feed, true)?;
    result
}
async fn run_session(
    directory: &SessionDir,
    uid: u32,
    feed: &Arc<Mutex<Feed>>,
    output: &mut Snapshots,
) -> Result<()> {
    let bin = dependency_root()
        .context("WireGuard dependencies unavailable; reinstall the complete xxtab.app (source CLI: brew install bash wireguard-tools wireguard-go)")?;
    let draft: Draft = serde_json::from_slice(&directory.read("request.json")?)
        .map_err(|_| anyhow::anyhow!("invalid session request"))?;
    if directory.stopped() {
        crate::log!("会话停止：已收到界面断开请求");
        return Ok(());
    }
    let owner = directory
        .open_input("owner.lock")
        .context("无法打开界面进程存活锁")?;
    ensure!(owner_alive(&owner)?, "界面进程已退出，未启动隧道");
    // Root writes configurations only in its own fresh private directory. The
    // unprivileged request may supply settings, never an executable to elevate.
    let private = tempfile::Builder::new()
        .prefix("xxtab-macos-")
        .tempdir_in("/private/var/tmp")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700))?;
    let mut config: toml::Value =
        toml::from_str(&draft.tunnel).map_err(|_| anyhow::anyhow!("invalid tunnel TOML"))?;
    let wg = config
        .get_mut("wireguard")
        .and_then(toml::Value::as_table_mut)
        .context("missing wireguard section")?;
    wg.insert("config".into(), toml::Value::String("wg.conf".into()));
    wg.insert("name".into(), toml::Value::String(format!("xxm{uid}")));
    wg.insert(
        "executable".into(),
        toml::Value::String(quick_path(&bin).to_string_lossy().into()),
    );
    let config_path = private.path().join("xxtab.toml");
    write_private(&config_path, toml::to_string(&config)?.as_bytes())?;
    write_private(&private.path().join("wg.conf"), draft.wireguard.as_bytes())?;
    let stop_failure = Cell::new(None::<&'static str>);
    let stop = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                _ = term.recv() => { crate::log!("会话停止：收到 SIGTERM 信号"); break; },
                signal = tokio::signal::ctrl_c() => { signal?; crate::log!("会话停止：收到 SIGINT 信号"); break; },
            }
            if directory.stopped() {
                crate::log!("会话停止：已收到界面断开请求");
                break;
            }
            match owner_alive(&owner) {
                Ok(true) => {}
                Ok(false) => {
                    let reason = "界面进程已退出，正在断开并清理隧道";
                    crate::log!("会话停止：{reason}");
                    stop_failure.set(Some(reason));
                    break;
                }
                Err(_) => {
                    let reason = "无法检查界面进程存活锁，正在断开并清理隧道";
                    crate::log!("会话停止：{reason}");
                    stop_failure.set(Some(reason));
                    break;
                }
            }
        }
        Ok(())
    };
    let run = crate::app::run(&config_path, "run", stop);
    tokio::pin!(run);
    let mut timer = tokio::time::interval(Duration::from_millis(500));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut run => {
                // Let app::run clean up normally before reporting lost ownership.
                result?;
                if let Some(reason) = stop_failure.get() {
                    anyhow::bail!("{reason}");
                }
                return Ok(());
            },
            _ = timer.tick() => { output.publish(feed, false)?; },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_lock_needs_no_updates_and_releases_on_close() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("owner.lock");
        let gui = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(gui.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let dir = SessionDir::open(temp.path(), unsafe { libc::getuid() }).unwrap();
        let worker = dir.open_input("owner.lock").unwrap();
        assert!(owner_alive(&worker).unwrap());
        assert_eq!(worker.metadata().unwrap().len(), 0);
        // Even path replacement cannot redirect the worker to a different owner.
        std::fs::remove_file(&path).unwrap();
        write_private(&path, b"").unwrap();
        assert!(owner_alive(&worker).unwrap());
        drop(gui);
        assert!(!owner_alive(&worker).unwrap());
    }
    #[test]
    fn resolves_relocated_app_dependencies_and_checks_execute_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let contents = temp.path().join("Moved App With Spaces.app/Contents");
        let root = contents.join("Helpers");
        assert_eq!(
            bundled_root(&contents.join("MacOS/xxtab")),
            Some(root.clone())
        );
        assert!(bundled_root(&temp.path().join("xxtab")).is_none());
        std::fs::create_dir_all(&root).unwrap();
        for path in [
            root.join("bash"),
            root.join("wg"),
            root.join("wireguard-go"),
            quick_path(&root),
            quick_path(&root).with_file_name("wg-quick.bash"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"test").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(complete_dependencies(&root));
        std::fs::set_permissions(root.join("bash"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(!complete_dependencies(&root));
        std::fs::remove_file(root.join("wg")).unwrap();
        assert!(!complete_dependencies(&root));
    }
    #[test]
    fn rejects_symlinks_and_unprotected_input() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = SessionDir::open(temp.path(), unsafe { libc::getuid() }).unwrap();
        write_private(&temp.path().join("heartbeat"), b"1").unwrap();
        assert_eq!(dir.read("heartbeat").unwrap(), b"1");
        symlink(temp.path().join("heartbeat"), temp.path().join("stop")).unwrap();
        assert!(!dir.stopped());
        symlink(
            temp.path().join("heartbeat"),
            temp.path().join("snapshot.json"),
        )
        .unwrap();
        assert!(dir.snapshot_file().is_err());
        std::fs::set_permissions(
            temp.path().join("heartbeat"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(dir.read("heartbeat").is_err());
    }
}
