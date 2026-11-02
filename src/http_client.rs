use std::time::Duration;

use serde::Serialize;

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36 ark-updater";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyChoice {
    Direct,
    Url(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProxyStatus {
    pub source: String,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProxy {
    pub choice: ProxyChoice,
    pub status: ProxyStatus,
}

pub struct BuiltClients {
    pub version: reqwest::Client,
    pub download: reqwest::Client,
    pub proxy: ResolvedProxy,
}

pub fn build_clients(config_proxy: Option<&str>) -> Result<BuiltClients, String> {
    let proxy = resolve_from_environment(config_proxy)?;
    let version = version_client(&proxy.choice)?;
    let download = download_client(&proxy.choice)?;
    Ok(BuiltClients {
        version,
        download,
        proxy,
    })
}

pub fn manual_redirect_client(config_proxy: Option<&str>) -> Result<reqwest::Client, String> {
    let proxy = resolve_from_environment(config_proxy)?;
    finish(
        base_builder(
            &proxy.choice,
            Duration::from_secs(60),
            Some(Duration::from_secs(600)),
        )?
        .redirect(reqwest::redirect::Policy::none()),
        "创建程序更新下载客户端失败",
    )
}

pub fn resolve_from_environment(config_proxy: Option<&str>) -> Result<ResolvedProxy, String> {
    resolve_proxy(config_proxy, env_proxy().as_deref())
}

pub fn resolve_proxy(
    config_proxy: Option<&str>,
    env_proxy: Option<&str>,
) -> Result<ResolvedProxy, String> {
    let configured = config_proxy
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match configured {
        Some(value) if is_direct_word(value) => Ok(direct_status()),
        Some(value) => {
            validate_proxy_url(value)?;
            Ok(ResolvedProxy {
                choice: ProxyChoice::Url(value.to_string()),
                status: ProxyStatus {
                    source: "config".into(),
                    display: redact_proxy(value),
                },
            })
        }
        None => match env_proxy.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) if is_direct_word(value) => Ok(direct_status()),
            Some(value) => match validate_proxy_url(value) {
                Ok(()) => Ok(ResolvedProxy {
                    choice: ProxyChoice::Url(value.to_string()),
                    status: ProxyStatus {
                        source: "environment".into(),
                        display: redact_proxy(value),
                    },
                }),
                Err(err) => Ok(ResolvedProxy {
                    choice: ProxyChoice::Direct,
                    status: ProxyStatus {
                        source: "direct".into(),
                        display: format!("环境变量里的代理无效，改为直连：{err}"),
                    },
                }),
            },
            None => Ok(direct_status()),
        },
    }
}

pub fn validate_proxy_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|err| format!("代理地址无效：{err}"))?;
    match url.scheme() {
        "http" | "https" | "socks5" | "socks5h" => {}
        other => {
            return Err(format!(
                "不支持的代理协议 {other}，可用 http、https、socks5、socks5h"
            ));
        }
    }
    if url.host_str().is_none() {
        return Err("代理地址缺少主机".into());
    }
    Ok(())
}

pub fn redact_proxy(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return "已配置代理".into();
    };
    let has_secret = !url.username().is_empty() || url.password().is_some();
    if has_secret {
        let _ = url.set_username("***");
        let _ = url.set_password(None);
    }
    url.to_string()
}

pub fn validate_feed_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|err| format!("程序更新源无效：{err}"))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if host_is_loopback(url.host_str()) => Ok(()),
        "http" => Err("程序更新源只允许 https，或本机 http".into()),
        other => Err(format!("程序更新源协议无效：{other}")),
    }
}

fn env_proxy() -> Option<String> {
    for name in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Some(value) = std::env::var(name)
            .ok()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
        {
            return Some(value);
        }
    }
    None
}

fn is_direct_word(value: &str) -> bool {
    value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("direct")
}

fn direct_status() -> ResolvedProxy {
    ResolvedProxy {
        choice: ProxyChoice::Direct,
        status: ProxyStatus {
            source: "direct".into(),
            display: "直连".into(),
        },
    }
}

fn version_client(choice: &ProxyChoice) -> Result<reqwest::Client, String> {
    finish(
        base_builder(
            choice,
            Duration::from_secs(30),
            Some(Duration::from_secs(30)),
        )?
        .redirect(reqwest::redirect::Policy::limited(5)),
        "创建版本查询客户端失败",
    )
}

fn download_client(choice: &ProxyChoice) -> Result<reqwest::Client, String> {
    finish(
        base_builder(choice, Duration::from_secs(60), None)?
            .redirect(reqwest::redirect::Policy::limited(10)),
        "创建下载客户端失败",
    )
}

fn base_builder(
    choice: &ProxyChoice,
    connect_timeout: Duration,
    total_timeout: Option<Duration>,
) -> Result<reqwest::ClientBuilder, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .user_agent(USER_AGENT);
    if let Some(timeout) = total_timeout {
        builder = builder.timeout(timeout);
    }
    match choice {
        ProxyChoice::Direct => Ok(builder.no_proxy()),
        ProxyChoice::Url(url) => {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|err| format!("代理不可用：{err}"))?
                .no_proxy(reqwest::NoProxy::from_string("localhost,127.0.0.1,::1"));
            Ok(builder.proxy(proxy))
        }
    }
}

fn finish(builder: reqwest::ClientBuilder, context: &str) -> Result<reqwest::Client, String> {
    builder.build().map_err(|err| format!("{context}：{err}"))
}

fn host_is_loopback(host: Option<&str>) -> bool {
    matches!(host, Some("localhost" | "127.0.0.1" | "::1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_proxy_beats_environment() {
        let resolved =
            resolve_proxy(Some("http://127.0.0.1:7890"), Some("http://10.0.0.1:1")).expect("proxy");
        assert_eq!(
            resolved.choice,
            ProxyChoice::Url("http://127.0.0.1:7890".into())
        );
        assert_eq!(resolved.status.source, "config");
    }

    #[test]
    fn off_ignores_environment() {
        let resolved = resolve_proxy(Some("off"), Some("http://10.0.0.1:1")).expect("proxy");
        assert_eq!(resolved.choice, ProxyChoice::Direct);
        assert_eq!(resolved.status.display, "直连");
    }

    #[test]
    fn empty_config_uses_environment() {
        let resolved = resolve_proxy(None, Some("socks5://127.0.0.1:1080")).expect("proxy");
        assert_eq!(
            resolved.choice,
            ProxyChoice::Url("socks5://127.0.0.1:1080".into())
        );
        assert_eq!(resolved.status.source, "environment");
    }

    #[test]
    fn invalid_environment_proxy_falls_back_to_direct() {
        let resolved = resolve_proxy(None, Some("ftp://127.0.0.1:1")).expect("proxy");
        assert_eq!(resolved.choice, ProxyChoice::Direct);
        assert!(resolved.status.display.contains("直连"));
    }

    #[test]
    fn rejects_proxy_without_host() {
        assert!(validate_proxy_url("http://").is_err());
        assert!(validate_proxy_url("file:///tmp/proxy").is_err());
    }

    #[test]
    fn redacts_proxy_credentials() {
        let display = redact_proxy("http://user:secret@127.0.0.1:7890");
        assert!(!display.contains("secret"));
        assert!(!display.contains("user"));
        assert!(display.contains("127.0.0.1"));
    }

    #[test]
    fn feed_url_allows_https_and_loopback_only() {
        assert!(validate_feed_url("https://example.com/releases/latest").is_ok());
        assert!(validate_feed_url("http://127.0.0.1:9/latest").is_ok());
        assert!(validate_feed_url("http://example.com/latest").is_err());
    }

    #[tokio::test]
    async fn configured_proxy_receives_the_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 2048];
            let n = tokio::io::AsyncReadExt::read(&mut socket, &mut buf)
                .await
                .expect("read");
            let text = String::from_utf8_lossy(&buf[..n]);
            assert!(
                text.starts_with("GET http://example.test/ping"),
                "请求没有走代理：{text}"
            );
            let body = "proxied";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                .await
                .expect("write");
        });

        let client = manual_redirect_client(Some(&format!("http://{addr}"))).expect("client");
        let response = client
            .get("http://example.test/ping")
            .send()
            .await
            .expect("send");
        let body = response.text().await.expect("body");
        assert_eq!(body, "proxied");
    }
}
