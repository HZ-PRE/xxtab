//! Startup and on-demand GitHub Releases updates. No resident polling or GitHub credentials.
use anyhow::{Context, Result, bail, ensure};
#[cfg(not(windows))]
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(not(windows))]
use std::sync::Arc;
use std::{io::Write, path::PathBuf, time::Duration};
#[cfg(not(windows))]
use tokio::net::TcpStream;
use tokio::time::timeout;
use url::Url;

pub const REPOSITORY: &str = "HZ-PRE/xxtab";
pub const RELEASES: &str = "https://github.com/HZ-PRE/xxtab/releases";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// One background check for CLI run/relay. Diagnostics stay off JSON stdout.
pub async fn startup_notice() {
    match check().await {
        Ok(check) if check.available => {
            if let Some(release) = check.release {
                let message = format!(
                    "发现 xxtab 新版本 {}（当前 {}）\n下载并校验：xxtab update download {}\n停止连接后解压替换程序。\n{}",
                    release.version, check.current, release.version, release.page
                );
                crate::log!("{message}");
                #[cfg(target_os = "linux")]
                if let Err(error) = linux::notify(&message).await {
                    crate::log!("无法显示更新弹窗，请查看终端提示：{error:#}");
                }
            }
        }
        Ok(_) => {}
        Err(error) => crate::log!("自动检查更新失败：{error:#}"),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub filename: String,
    pub url: String,
    pub checksum_url: String,
    pub size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Check {
    pub current: String,
    pub available: bool,
    pub release: Option<Release>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Download {
    pub version: String,
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn version(value: &str) -> Result<[u64; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    ensure!(parts.len() == 3, "版本号必须为 major.minor.patch");
    let mut result = [0; 3];
    for (to, from) in result.iter_mut().zip(parts) {
        ensure!(
            !from.is_empty()
                && from.bytes().all(|b| b.is_ascii_digit())
                && (from == "0" || !from.starts_with('0')),
            "无效版本号"
        );
        *to = from.parse().context("版本号超出范围")?;
    }
    Ok(result)
}
fn platform() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("windows-x64-setup.exe"),
        ("macos", "aarch64") => Ok("macos-arm64.dmg"),
        ("macos", "x86_64") => Ok("macos-x86_64.dmg"),
        ("linux", "x86_64") => Ok("linux-x64.tar.gz"),
        _ => bail!("此系统架构暂无预编译更新包，请前往 GitHub Releases"),
    }
}
fn asset_url(version: &str, filename: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/v{version}/{filename}")
}
fn parse_release(bytes: &[u8], current: &str, platform: &str) -> Result<Check> {
    let api: ApiRelease = serde_json::from_slice(bytes).context("GitHub Release 数据无效")?;
    ensure!(!api.draft && !api.prerelease, "更新源不是正式版本");
    let latest = api
        .tag_name
        .strip_prefix('v')
        .context("Release 标签必须以 v 开头")?;
    let newer = version(latest)? > version(current)?;
    let filename = format!("xxtab-{latest}-{platform}");
    let find = |name: &str| -> Result<&Asset> {
        let mut matches = api.assets.iter().filter(|a| a.name == name);
        let asset = matches
            .next()
            .context("此 Release 缺少当前平台安装包或 SHA256 文件，请等待发布完成")?;
        ensure!(matches.next().is_none(), "Release 包含重复文件");
        ensure!(
            asset.browser_download_url == asset_url(latest, name),
            "Release 下载地址不属于指定仓库和版本"
        );
        Ok(asset)
    };
    let asset = find(&filename)?;
    let checksum = find(&format!("{filename}.sha256"))?;
    ensure!(
        asset.size > 0 && asset.size <= MAX_PACKAGE,
        "安装包大小超出限制"
    );
    ensure!(
        checksum.size > 0 && checksum.size <= 4096,
        "SHA256 文件大小无效"
    );
    Ok(Check {
        current: current.into(),
        available: newer,
        release: Some(Release {
            version: latest.into(),
            page: format!("{RELEASES}/tag/v{latest}"),
            filename,
            url: asset.browser_download_url.clone(),
            checksum_url: checksum.browser_download_url.clone(),
            size: asset.size,
        }),
    })
}

fn trusted_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("无效的更新地址")?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url.fragment().is_none()
            && matches!(
                url.host_str(),
                Some(
                    "api.github.com"
                        | "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            ),
        "更新请求只能使用 GitHub 的 HTTPS 下载地址"
    );
    Ok(url)
}

#[cfg(not(windows))]
struct Connection(tokio::task::JoinHandle<()>);
#[cfg(not(windows))]
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// The body is streamed into the writer: even a large installer uses bounded memory.
#[cfg(windows)]
async fn fetch(value: &str, limit: u64, writer: &mut impl Write) -> Result<u16> {
    windows::fetch(value, limit, writer).await
}

#[cfg(not(windows))]
async fn fetch(value: &str, limit: u64, writer: &mut impl Write) -> Result<u16> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let mut url = trusted_url(value)?;
    for _ in 0..6 {
        let host = url.host_str().context("更新地址缺少主机")?;
        let tcp = TcpStream::connect((host, 443))
            .await
            .context("无法连接 GitHub，请检查网络")?;
        let server = rustls::pki_types::ServerName::try_from(host.to_owned())?;
        let stream = tokio_rustls::TlsConnector::from(tls.clone())
            .connect(server, tcp)
            .await
            .context("GitHub TLS 验证失败")?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await?;
        let _connection = Connection(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let request = hyper::Request::builder()
            .uri(url.as_str())
            .header("Host", host)
            .header("User-Agent", concat!("xxtab/", env!("CARGO_PKG_VERSION")))
            .header(
                "Accept",
                if host == "api.github.com" {
                    "application/vnd.github+json"
                } else {
                    "application/octet-stream"
                },
            )
            .body(http_body_util::Empty::<hyper::body::Bytes>::new())?;
        let mut response = sender
            .send_request(request)
            .await
            .context("GitHub 请求失败")?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .context("下载重定向缺少地址")?
                .to_str()?;
            url = trusted_url(url.join(location)?.as_str())?;
            continue;
        }
        if status == 404 {
            return Ok(status);
        }
        ensure!(
            status != 403 && status != 429,
            "GitHub 请求受限，请稍后重试（HTTP {status}）"
        );
        ensure!(status == 200, "GitHub 返回 HTTP {status}");
        if let Some(length) = response.headers().get("content-length") {
            ensure!(
                length.to_str()?.parse::<u64>()? <= limit,
                "更新文件超出大小限制"
            );
        }
        let mut received = 0u64;
        while let Some(frame) = response.body_mut().frame().await {
            if let Ok(data) = frame?.into_data() {
                received = received
                    .checked_add(data.len() as u64)
                    .context("更新文件过大")?;
                ensure!(received <= limit, "更新文件超出大小限制");
                writer.write_all(&data)?;
            }
        }
        writer.flush()?;
        return Ok(status);
    }
    bail!("GitHub 下载重定向次数过多")
}

pub async fn check() -> Result<Check> {
    timeout(Duration::from_secs(30), async {
        let platform = platform()?;
        let mut bytes = Vec::new();
        let status = fetch(
            &format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
            2 * 1024 * 1024,
            &mut bytes,
        )
        .await?;
        if status == 404 {
            return Ok(Check {
                current: CURRENT_VERSION.into(),
                available: false,
                release: None,
            });
        }
        parse_release(&bytes, CURRENT_VERSION, platform)
    })
    .await
    .context("检查更新超时，请检查 GitHub 连通性")?
}

fn checksum(bytes: &[u8], filename: &str) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("SHA256 文件编码无效")?;
    let fields: Vec<_> = text.split_whitespace().collect();
    ensure!(
        fields.len() == 2
            && fields[1].trim_start_matches('*') == filename
            && fields[0].len() == 64
            && fields[0].bytes().all(|b| b.is_ascii_hexdigit()),
        "SHA256 文件内容或文件名无效"
    );
    Ok(fields[0].to_ascii_lowercase())
}

struct HashedFile {
    file: std::fs::File,
    hash: Sha256,
    size: u64,
}
impl Write for HashedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(bytes)?;
        self.hash.update(&bytes[..n]);
        self.size += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

trait DownloadSource {
    async fn fetch(&self, url: &str, limit: u64, writer: &mut impl Write) -> Result<u16>;
}
struct GitHub;
impl DownloadSource for GitHub {
    async fn fetch(&self, url: &str, limit: u64, writer: &mut impl Write) -> Result<u16> {
        fetch(url, limit, writer).await
    }
}
pub async fn download(release: &Release) -> Result<Download> {
    download_using(release, &GitHub).await
}
async fn download_using(release: &Release, source: &impl DownloadSource) -> Result<Download> {
    ensure!(
        version(&release.version)? > version(CURRENT_VERSION)?,
        "此版本无需更新"
    );
    let filename = format!("xxtab-{}-{}", release.version, platform()?);
    ensure!(
        release.filename == filename
            && release.url == asset_url(&release.version, &filename)
            && release.checksum_url == asset_url(&release.version, &format!("{filename}.sha256"))
            && release.size > 0
            && release.size <= MAX_PACKAGE,
        "更新信息无效，请重新检查更新"
    );
    timeout(Duration::from_secs(300), async {
        let mut bytes = Vec::new();
        ensure!(
            source
                .fetch(&release.checksum_url, 4096, &mut bytes)
                .await?
                == 200,
            "找不到 SHA256 文件"
        );
        let expected = checksum(&bytes, &filename)?;
        let directory = tempfile::Builder::new().prefix("xxtab-update-").tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let path = directory.path().join(&filename);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut sink = HashedFile {
            file,
            hash: Sha256::new(),
            size: 0,
        };
        ensure!(
            source.fetch(&release.url, release.size, &mut sink).await? == 200,
            "找不到更新安装包"
        );
        ensure!(sink.size == release.size, "安装包下载不完整");
        let actual = format!("{:x}", sink.hash.finalize());
        ensure!(actual == expected, "安装包 SHA256 校验失败，已拒绝更新");
        sink.file.sync_all()?;
        drop(sink.file);
        let _ = directory.keep();
        Ok(Download {
            version: release.version.clone(),
            path,
            sha256: expected,
        })
    })
    .await
    .context("下载更新超时，请稍后重试")?
}

#[cfg(windows)]
pub fn install_after_exit(download: &Download) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let current = std::env::current_exe()?;
    let install_dir = current.parent().context("无法确定安装目录")?;
    let helper = download
        .path
        .parent()
        .context("无法确定更新目录")?
        .join("xxtab-updater.exe");
    // Run a copy: an updater in the install directory would prevent replacing the CLI.
    std::fs::copy(install_dir.join("xxtab.exe"), &helper)
        .context("无法启动更新器，请保留同目录的 xxtab.exe")?;
    std::process::Command::new(helper)
        .creation_flags(0x08000000)
        .args([
            "update-install",
            &std::process::id().to_string(),
            &download.sha256,
        ])
        .arg(&download.path)
        .arg(install_dir)
        .spawn()
        .context("无法启动更新器")?;
    Ok(())
}

#[cfg(windows)]
pub fn finish_install(
    pid: u32,
    expected: &str,
    path: &std::path::Path,
    directory: &std::path::Path,
) -> Result<()> {
    use std::{
        io::Read,
        os::windows::{fs::OpenOptionsExt, process::CommandExt},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "无效的更新校验值"
    );
    // Hold the process handle across hashing to avoid PID reuse races.
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if !parent.is_null() {
        let result = unsafe { WaitForSingleObject(parent, 60_000) };
        unsafe {
            CloseHandle(parent);
        }
        ensure!(result == WAIT_OBJECT_0, "等待旧程序退出超时，取消安装");
    } else {
        // Refuse access errors. ERROR_INVALID_PARAMETER means the parent has exited.
        ensure!(
            std::io::Error::last_os_error().raw_os_error() == Some(87),
            "无法确认旧程序已退出"
        );
    }
    // Disallow writes/deletes until Windows has opened the verified executable.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == expected.to_ascii_lowercase(),
        "安装包已被修改，取消安装"
    );
    std::process::Command::new(path)
        .creation_flags(0x08000000)
        .arg(format!("/DIR={}", directory.display()))
        .spawn()
        .context("无法打开更新安装包")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn downloads_only_complete_verified_packages() {
        let filename = format!("xxtab-999.0.0-{}", platform().unwrap());
        let release = Release {
            version: "999.0.0".into(),
            page: String::new(),
            filename: filename.clone(),
            url: asset_url("999.0.0", &filename),
            checksum_url: asset_url("999.0.0", &format!("{filename}.sha256")),
            size: 7,
        };
        struct Mock {
            checksum: String,
            payload: &'static [u8],
        }
        impl DownloadSource for Mock {
            async fn fetch(&self, url: &str, _limit: u64, writer: &mut impl Write) -> Result<u16> {
                if url.ends_with(".sha256") {
                    writer.write_all(self.checksum.as_bytes())?;
                } else {
                    writer.write_all(self.payload)?;
                }
                Ok(200)
            }
        }
        let hash = format!("{:x}", Sha256::digest(b"package"));
        let mut source = Mock {
            checksum: format!("{hash}  {filename}\n"),
            payload: b"package",
        };
        let downloaded = download_using(&release, &source).await.unwrap();
        assert_eq!(std::fs::read(&downloaded.path).unwrap(), b"package");
        assert_eq!(downloaded.sha256, hash);
        std::fs::remove_file(&downloaded.path).unwrap();
        std::fs::remove_dir(downloaded.path.parent().unwrap()).unwrap();
        source.payload = b"changed";
        assert!(
            download_using(&release, &source)
                .await
                .unwrap_err()
                .to_string()
                .contains("SHA256")
        );
        source.payload = b"short";
        assert!(
            download_using(&release, &source)
                .await
                .unwrap_err()
                .to_string()
                .contains("不完整")
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires rustc; launches only a synthetic installer in a private test directory"]
    fn windows_installer_handoff_waits_and_rechecks() {
        use std::{os::windows::process::CommandExt, process::Command, time::Instant};
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("installer.rs");
        let installer = directory.path().join("synthetic-installer.exe");
        let marker = installer.with_extension("started");
        std::fs::write(&source, r#"fn main() {
            let args: Vec<_> = std::env::args().skip(1).collect();
            if args == ["parent"] { std::thread::sleep(std::time::Duration::from_millis(900)); }
            else { std::fs::write(std::env::current_exe().unwrap().with_extension("started"), args.join("\n")).unwrap(); }
        }"#).unwrap();
        assert!(
            Command::new("rustc")
                .arg(&source)
                .arg("-o")
                .arg(&installer)
                .status()
                .unwrap()
                .success()
        );
        let hash = format!("{:x}", Sha256::digest(std::fs::read(&installer).unwrap()));
        let install_dir = directory.path().join("App With Spaces");
        let mut parent = Command::new(&installer)
            .arg("parent")
            .creation_flags(0x08000000)
            .spawn()
            .unwrap();
        let start = Instant::now();
        finish_install(parent.id(), &hash, &installer, &install_dir).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(500));
        parent.wait().unwrap();
        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("/DIR={}", install_dir.display())
        );
        // A checksum mismatch must fail before another executable is launched.
        assert!(
            finish_install(u32::MAX, &"0".repeat(64), &installer, &install_dir)
                .unwrap_err()
                .to_string()
                .contains("已被修改")
        );
    }
    fn fixture(tag: &str, suffix: &str) -> serde_json::Value {
        let filename = format!("xxtab-{tag}-{suffix}");
        serde_json::json!({"tag_name":format!("v{tag}"),"draft":false,"prerelease":false,"assets":[
            {"name":filename,"browser_download_url":asset_url(tag,&filename),"size":123},
            {"name":format!("{filename}.sha256"),"browser_download_url":asset_url(tag,&format!("{filename}.sha256")),"size":100}
        ]})
    }
    #[test]
    fn versions_and_platform_assets() {
        for suffix in [
            "windows-x64-setup.exe",
            "macos-arm64.dmg",
            "macos-x86_64.dmg",
            "linux-x64.tar.gz",
        ] {
            let bytes = serde_json::to_vec(&fixture("0.10.0", suffix)).unwrap();
            assert!(parse_release(&bytes, "0.9.9", suffix).unwrap().available);
            assert!(!parse_release(&bytes, "0.10.0", suffix).unwrap().available);
            assert!(!parse_release(&bytes, "1.0.0", suffix).unwrap().available);
            assert!(parse_release(&bytes, "0.1.1", "other.exe").is_err());
        }
        for bad in ["1.2", "1.2.3-beta", "01.2.3", "1.2.3/evil", "1.2.-3"] {
            assert!(version(bad).is_err());
        }
    }
    #[test]
    fn rejects_foreign_missing_and_unstable_assets() {
        let original = fixture("1.0.0", "macos-arm64.dmg");
        let mut foreign = original.clone();
        foreign["assets"][0]["browser_download_url"] = "https://github.com/other/repo/file".into();
        let mut missing = original.clone();
        missing["assets"].as_array_mut().unwrap().pop();
        let mut beta = original.clone();
        beta["prerelease"] = true.into();
        let mut huge = original;
        huge["assets"][0]["size"] = (MAX_PACKAGE + 1).into();
        for item in [foreign, missing, beta, huge] {
            assert!(
                parse_release(
                    &serde_json::to_vec(&item).unwrap(),
                    "0.1.1",
                    "macos-arm64.dmg"
                )
                .is_err()
            );
        }
    }
    #[test]
    fn validates_redirects_and_checksums() {
        assert!(trusted_url("https://release-assets.githubusercontent.com/file?token=abc").is_ok());
        for bad in [
            "http://github.com/file",
            "https://github.com.evil.test/file",
            "https://evil.test/file",
            "https://user@github.com/file",
            "https://github.com:444/file",
        ] {
            assert!(trusted_url(bad).is_err());
        }
        let hash = "a".repeat(64);
        assert_eq!(
            checksum(format!("{hash}  x.exe\n").as_bytes(), "x.exe").unwrap(),
            hash
        );
        assert!(checksum(format!("{hash}  y.exe").as_bytes(), "x.exe").is_err());
        assert!(checksum(b"bad x.exe", "x.exe").is_err());
    }
}
