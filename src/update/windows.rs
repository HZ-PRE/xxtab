//! Use Windows proxy/PAC settings and the OS TLS stack without a bundled HTTP client.
use super::trusted_url;
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::c_void,
    io::Write,
    ptr::{null, null_mut},
};
use tokio::sync::mpsc;
use windows_sys::Win32::Networking::WinHttp::*;

const CHUNK: usize = 64 * 1024;
enum Event {
    Data(Vec<u8>),
    Done(u16),
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct Handle(*mut c_void);
impl Handle {
    fn new(raw: *mut c_void) -> Result<Self> {
        if raw.is_null() {
            Err(std::io::Error::last_os_error().into())
        } else {
            Ok(Self(raw))
        }
    }
    fn option(&self, name: u32, value: u32) -> Result<()> {
        checked(unsafe { WinHttpSetOption(self.0, name, (&value as *const u32).cast(), 4) })
    }
    fn header(&self, name: u32) -> Result<Option<String>> {
        // Bound metadata allocation as well as body buffering.
        let mut buffer = [0u16; 8192];
        let mut bytes = std::mem::size_of_val(&buffer) as u32;
        let result = unsafe {
            WinHttpQueryHeaders(
                self.0,
                name,
                null(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
                null_mut(),
            )
        };
        if result == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_WINHTTP_HEADER_NOT_FOUND as i32) {
                return Ok(None);
            }
            return Err(error).context("无法读取 GitHub 响应头或响应头过大");
        }
        Ok(Some(
            String::from_utf16(&buffer[..bytes as usize / 2])?
                .trim_end_matches('\0')
                .to_owned(),
        ))
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
fn checked(result: i32) -> Result<()> {
    if result == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

// A bounded channel keeps the blocking OS API off the UI and tunnel executor.
// Closing its receiver cancels further reads; a pending OS call has finite timeouts.
pub(super) async fn fetch(value: &str, limit: u64, writer: &mut impl Write) -> Result<u16> {
    let url = trusted_url(value)?;
    let (tx, mut rx) = mpsc::channel(1);
    std::thread::Builder::new()
        .name("xxtab-http".into())
        .stack_size(1024 * 1024)
        .spawn(move || {
            let result = download(url, limit, &tx, None).map(Event::Done);
            let _ = tx.blocking_send(result);
        })
        .context("无法启动更新网络请求")?;
    while let Some(event) = rx.recv().await {
        match event? {
            Event::Data(bytes) => writer.write_all(&bytes)?,
            Event::Done(status) => {
                writer.flush()?;
                return Ok(status);
            }
        }
    }
    bail!("更新网络请求意外结束")
}

fn download(
    mut url: url::Url,
    limit: u64,
    tx: &mpsc::Sender<Result<Event>>,
    proxy: Option<&str>,
) -> Result<u16> {
    let proxy = proxy.map(wide);
    let agent = wide(concat!("xxtab/", env!("CARGO_PKG_VERSION")));
    let session = Handle::new(unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            if proxy.is_some() {
                WINHTTP_ACCESS_TYPE_NAMED_PROXY
            } else {
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
            },
            proxy.as_ref().map_or(null(), |p| p.as_ptr()),
            null(),
            0,
        )
    })
    .context("无法初始化 Windows 系统代理")?;
    checked(unsafe { WinHttpSetTimeouts(session.0, 5000, 8000, 10000, 15000) })?;
    if session
        .option(
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3,
        )
        .is_err()
    {
        session.option(
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2,
        )?;
    }
    for _ in 0..6 {
        ensure!(!tx.is_closed(), "更新请求已取消");
        let host = url.host_str().context("更新地址缺少主机")?;
        let connection =
            Handle::new(unsafe { WinHttpConnect(session.0, wide(host).as_ptr(), 443, 0) })?;
        let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                wide("GET").as_ptr(),
                wide(path).as_ptr(),
                null(),
                null(),
                null(),
                WINHTTP_FLAG_SECURE,
            )
        })?;
        // Validate every redirect ourselves; never weaken TLS verification or send
        // Windows login credentials to the update host automatically.
        request.option(
            WINHTTP_OPTION_REDIRECT_POLICY,
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
        )?;
        request.option(
            WINHTTP_OPTION_AUTOLOGON_POLICY,
            WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH,
        )?;
        request.option(WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_DISABLE_COOKIES)?;
        let accept = wide(if host == "api.github.com" {
            "Accept: application/vnd.github+json\r\n"
        } else {
            "Accept: application/octet-stream\r\n"
        });
        checked(unsafe {
            WinHttpSendRequest(
                request.0,
                accept.as_ptr(),
                (accept.len() - 1) as u32,
                null(),
                0,
                0,
                0,
            )
        })
        .context("无法通过 Windows 网络设置连接 GitHub，请检查系统代理是否可用及网络连通性")?;
        checked(unsafe { WinHttpReceiveResponse(request.0, null_mut()) })
            .context("等待 GitHub 响应失败，请检查系统代理或稍后重试")?;
        let status: u16 = request
            .header(WINHTTP_QUERY_STATUS_CODE)?
            .context("缺少 HTTP 状态码")?
            .parse()?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = request
                .header(WINHTTP_QUERY_LOCATION)?
                .context("下载重定向缺少地址")?;
            url = trusted_url(url.join(&location)?.as_str())?;
            continue;
        }
        if status == 404 {
            return Ok(status);
        }
        ensure!(
            status != 407,
            "系统代理要求身份验证，请配置无需交互认证的代理后重试"
        );
        ensure!(
            status != 403 && status != 429,
            "GitHub 请求受限，请稍后重试（HTTP {status}）"
        );
        ensure!(status == 200, "GitHub 返回 HTTP {status}");
        let length = request
            .header(WINHTTP_QUERY_CONTENT_LENGTH)?
            .map(|value| value.parse::<u64>())
            .transpose()?;
        ensure!(
            length.is_none_or(|size| size <= limit),
            "更新文件超出大小限制"
        );
        let mut received = 0u64;
        loop {
            ensure!(!tx.is_closed(), "更新请求已取消");
            let mut buffer = vec![0u8; CHUNK];
            let mut count = 0u32;
            checked(unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut count,
                )
            })
            .context("下载 GitHub 更新数据失败")?;
            if count == 0 {
                break;
            }
            received = received
                .checked_add(u64::from(count))
                .context("更新文件过大")?;
            ensure!(received <= limit, "更新文件超出大小限制");
            buffer.truncate(count as usize);
            tx.blocking_send(Ok(Event::Data(buffer)))
                .map_err(|_| anyhow::anyhow!("更新请求已取消"))?;
        }
        ensure!(
            length.is_none_or(|size| size == received),
            "更新文件下载不完整"
        );
        return Ok(status);
    }
    bail!("GitHub 下载重定向次数过多")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        net::TcpListener,
        time::{Duration, Instant},
    };

    #[test]
    fn https_request_uses_connect_proxy_without_direct_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "updater did not use the proxy");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8(request).unwrap()
        });
        let (tx, _rx) = mpsc::channel(1);
        let result = download(
            trusted_url("https://api.github.com/test").unwrap(),
            1024,
            &tx,
            Some(&proxy),
        );
        let request = server.join().unwrap();
        assert!(
            request.starts_with("CONNECT api.github.com:443 HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(result.is_err(), "must not bypass an unavailable proxy");
    }

    #[test]
    fn cancelled_request_does_not_connect() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let error = download(
            trusted_url("https://api.github.com/test").unwrap(),
            1024,
            &tx,
            Some("127.0.0.1:1"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("已取消"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "uses real GitHub HTTPS through the current Windows proxy settings"]
    async fn live_system_proxy_and_download_size_limit() {
        let mut body = Vec::new();
        let status = fetch(
            "https://api.github.com/repos/rust-lang/rust",
            128 * 1024,
            &mut body,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        let repo: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(repo["full_name"], "rust-lang/rust");
        body.clear();
        let error = fetch("https://api.github.com/repos/rust-lang/rust", 32, &mut body)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("大小限制"), "{error:#}");
        assert!(body.len() <= 32);
        body.clear();
        // Exercise GitHub -> release-assets redirects and their signed query string.
        let status = fetch(
            "https://github.com/erebe/wstunnel/releases/download/v10.7.1/checksums.txt",
            16 * 1024,
            &mut body,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("wstunnel_10.7.1_windows_amd64.tar.gz")
        );
    }
}
