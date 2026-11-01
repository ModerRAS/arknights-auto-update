use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::sync::broadcast;

use crate::config::{ensure_loopback, parse_interval, resolve_config_path, save, AppConfig};
use crate::logsetup;
use crate::platform::{default_data_dir, open_browser};
use crate::state::{AppPaths, AppState, LogLine, LOG_CAPACITY};
use crate::task;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36 ark-updater";

#[derive(Debug, Parser)]
#[command(name = "ark-updater", version, about = "明日方舟官服自动更新器")]
struct Cli {
    /// 配置文件路径。默认优先使用程序目录下的 config.toml，否则使用系统配置目录。
    #[arg(long)]
    config: Option<PathBuf>,
    /// 覆盖监听地址。只接受本机回环地址，例如 127.0.0.1:8787。
    #[arg(long)]
    bind: Option<String>,
    /// 启动后用系统浏览器打开 WebUI。
    #[arg(long)]
    open: bool,
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data_dir = default_data_dir();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("创建数据目录 {} 失败", data_dir.display()))?;
    let config_path = resolve_config_path(cli.config.as_deref());
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录 {} 失败", parent.display()))?;
    }
    if !config_path.exists() {
        save(&config_path, &AppConfig::default())
            .map_err(|err| anyhow::anyhow!(err))
            .with_context(|| format!("写入默认配置 {} 失败", config_path.display()))?;
        eprintln!("已写入默认配置 {}", config_path.display());
    }
    let config = AppConfig::load(&config_path).map_err(|err| anyhow::anyhow!(err))?;
    let listen = cli.bind.clone().unwrap_or_else(|| config.listen.clone());
    let bound = ensure_loopback(&listen).map_err(|err| anyhow::anyhow!(err))?;

    let (events, _) = broadcast::channel(512);
    let recent = Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::<LogLine>::with_capacity(LOG_CAPACITY),
    ));
    let log_dir = data_dir.join("logs");
    logsetup::init(&log_dir, events.clone(), recent.clone())?;

    let version_client = version_client().map_err(|err| anyhow::anyhow!(err))?;
    let download_client = download_client().map_err(|err| anyhow::anyhow!(err))?;
    let paths = AppPaths {
        config_path: config_path.clone(),
        data_dir,
        log_dir,
    };
    let auto_interval = config.auto_check_interval.clone();
    let auto_install = config.auto_update_on_check;
    let state = AppState::new(
        config,
        paths,
        bound.to_string(),
        events,
        recent,
        version_client,
        download_client,
    );
    task::note_config_mtime(&state).await;

    let app = crate::web::router(state.clone());
    let listener = tokio::net::TcpListener::bind(bound)
        .await
        .with_context(|| format!("监听 {bound} 失败"))?;
    let local = listener.local_addr().context("读取监听地址失败")?;
    tracing::info!(%local, config = %config_path.display(), "WebUI 已启动");
    println!("明日方舟更新器已启动：http://{local}");
    println!("配置文件：{}", config_path.display());
    match parse_interval(&auto_interval) {
        Ok(Some(_)) if auto_install => {
            println!(
                "无人值守已开启：启动后约 8 秒自动检查并安装，之后每 {} 重复。已是最新的模拟器不会被反复拉起。",
                auto_interval
            );
        }
        Ok(Some(_)) => {
            println!(
                "会按 {} 自动检查版本，但不会自己安装。把 auto_update_on_check 设为 true 后才会无人值守安装。",
                auto_interval
            );
        }
        _ => {
            println!(
                "无人值守未开启。要自动更新，把 auto_check_interval 设为 6h，并把 auto_update_on_check 设为 true。"
            );
        }
    }

    if cli.open {
        open_browser(&format!("http://{local}"));
    }

    {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = task::check_official(&state, false).await {
                tracing::warn!(error = %err, "启动时检查官方版本失败");
            }
        });
    }
    {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = task::refresh_devices(&state).await {
                tracing::warn!(error = %err, "启动时扫描设备失败");
            }
        });
    }
    {
        let state = state.clone();
        tokio::spawn(async move {
            task::watch_config(state).await;
        });
    }
    {
        let state = state.clone();
        tokio::spawn(async move {
            task::auto_check_loop(state).await;
        });
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            if tokio::signal::ctrl_c().await.is_err() {
                tracing::warn!("无法监听 Ctrl+C");
            }
            tracing::info!("正在退出");
        })
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

fn version_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|err| format!("创建版本查询客户端失败：{err}"))
}

fn download_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|err| format!("创建下载客户端失败：{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_present() {
        assert!(USER_AGENT.contains("Chrome"));
    }
}
