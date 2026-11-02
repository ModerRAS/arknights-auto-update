use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::config::{parse_interval, AppConfig};
use crate::self_update::{
    self, current_commit, current_restart_args, current_version, running_from_cargo_target,
    stage_planned, StageEvent, StageRequest, UpdateDecision, DEFAULT_RELEASE_API,
};
use crate::state::{AppUpdatePhase, DeviceSnapshot, SharedState, WatchStatus};
use crate::task::{self, DeviceSelector, UpdateRequest};
use crate::version::version_is_newer;

const APP_UPDATE_GRACE: Duration = Duration::from_secs(3);
const APP_UPDATE_MIN: Duration = Duration::from_secs(5 * 60);
const WATCH_GRACE: Duration = Duration::from_secs(5);
const WATCH_MIN: Duration = Duration::from_secs(15);
const BOOT_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const BUSY_RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    Auto,
    Manual,
    CheckOnly,
}

pub async fn app_update_loop(state: SharedState) {
    let mut armed: Option<Duration> = None;
    let mut next_run: Option<Instant> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let config = state.config.read().await.clone();
        let interval = match parse_interval(&config.app_update_interval) {
            Ok(interval) => interval,
            Err(err) => {
                note_app(
                    &state,
                    AppUpdatePhase::Failed,
                    format!("程序更新间隔无效：{err}"),
                    Some(err),
                    None,
                )
                .await;
                armed = None;
                next_run = None;
                continue;
            }
        };
        let Some(mut interval) = interval else {
            armed = None;
            next_run = None;
            note_app(
                &state,
                AppUpdatePhase::Idle,
                "程序自动更新已关闭。",
                None,
                None,
            )
            .await;
            continue;
        };
        if interval < APP_UPDATE_MIN {
            interval = APP_UPDATE_MIN;
        }
        if armed != Some(interval) {
            let first = armed.is_none();
            armed = Some(interval);
            next_run = Some(Instant::now() + if first { APP_UPDATE_GRACE } else { interval });
            if first {
                set_app_message(&state, "程序会检查自己的发布版本。有新构建时会下载并重启。").await;
            }
        }
        set_app_next(&state, next_run).await;
        if Instant::now() < next_run.unwrap_or_else(Instant::now) {
            continue;
        }
        if task::game_update_busy(&state).await || state.app_job_running.load(Ordering::SeqCst) {
            next_run = Some(Instant::now() + BUSY_RETRY);
            set_app_message(&state, "官服更新还在进行，程序更新等一分钟。").await;
            continue;
        }
        next_run = Some(Instant::now() + interval);
        if let Err(err) = begin_app_update(&state, ApplyMode::Auto) {
            note_app(
                &state,
                AppUpdatePhase::Failed,
                err.clone(),
                Some(err),
                next_run,
            )
            .await;
        }
    }
}

pub fn begin_app_update(state: &SharedState, mode: ApplyMode) -> Result<(), String> {
    if state
        .app_job_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("程序更新已经在进行".into());
    }
    let state = std::sync::Arc::clone(state);
    tokio::spawn(async move {
        let _guard = AppJobGuard {
            state: std::sync::Arc::clone(&state),
        };
        if let Err(err) = run_app_update(&state, mode).await {
            note_app(&state, AppUpdatePhase::Failed, err.clone(), Some(err), None).await;
        }
    });
    Ok(())
}

pub async fn emulator_watch_loop(state: SharedState) {
    let mut armed: Option<Duration> = None;
    let mut next_run: Option<Instant> = None;
    let mut message = "正在安排模拟器开机检测".to_string();
    let mut cooldown: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let config = state.config.read().await.clone();
        let interval = match parse_interval(&config.emulator_watch_interval) {
            Ok(interval) => interval,
            Err(err) => {
                message = format!("开机检测间隔无效：{err}");
                publish_watch(&state, &config, false, None, &message, &[]).await;
                armed = None;
                next_run = None;
                continue;
            }
        };
        let Some(mut interval) = interval else {
            armed = None;
            next_run = None;
            cooldown.clear();
            message = "模拟器开机检测已关闭。".into();
            publish_watch(&state, &config, false, None, &message, &[]).await;
            continue;
        };
        let mut clamped = false;
        if interval < WATCH_MIN {
            interval = WATCH_MIN;
            clamped = true;
        }
        if armed != Some(interval) {
            let first = armed.is_none();
            armed = Some(interval);
            next_run = Some(Instant::now() + if first { WATCH_GRACE } else { interval });
            if first {
                message = if clamped {
                    "开机检测已开启。间隔短于 15 秒，按 15 秒查看，不会为了检测去拉起模拟器。"
                        .into()
                } else {
                    "开机检测已开启。模拟器打开后会自动安装落后的官服，这一步不会拉起模拟器。"
                        .into()
                };
            }
        }
        let due = next_run.unwrap_or_else(Instant::now);
        let devices = state.devices.read().await.clone();
        let offline = offline_emulator_names(&devices);
        publish_watch(&state, &config, true, Some(due), &message, &offline).await;
        if Instant::now() < due {
            continue;
        }
        next_run = Some(Instant::now() + interval);
        if task::update_busy(&state).await {
            message = "有更新在进行，这一轮先不扫描模拟器。".into();
            continue;
        }
        if let Err(err) = task::refresh_devices(&state).await {
            let devices = state.devices.read().await.clone();
            let offline = offline_emulator_names(&devices);
            message = scan_failure_message(&err, &offline);
            continue;
        }
        let devices = state.devices.read().await.clone();
        message = tick_watch(&state, &config, &devices, &mut cooldown).await;
    }
}

pub fn select_online_updates(
    devices: &[DeviceSnapshot],
    official: &str,
    cooling_down: &HashSet<String>,
) -> Vec<String> {
    let mut serials = Vec::new();
    for device in devices.iter().filter(|device| device.online) {
        if cooling_down.contains(&device.serial) {
            continue;
        }
        if !needs_install(official, device.installed_version.as_deref()) {
            continue;
        }
        serials.push(device.serial.clone());
    }
    serials.sort();
    serials
}

async fn run_app_update(state: &SharedState, mode: ApplyMode) -> Result<(), String> {
    let config = state.config.read().await.clone();
    let cancel = CancellationToken::new();
    *state.app_cancel.lock().await = Some(cancel.clone());
    note_app(
        state,
        AppUpdatePhase::Checking,
        "正在检查程序更新。",
        None,
        None,
    )
    .await;
    let client = state.version_client.read().await.clone();
    let feed = feed_of(&config);
    let remote = match self_update::inspect_release(&client, &feed, &cancel).await {
        Ok(remote) => remote,
        Err(err) => return Err(err),
    };
    let target = self_update::current_release_target()?;
    let decision =
        self_update::decide_update(current_version(), current_commit(), target, &remote)?;
    let plan = match decision {
        UpdateDecision::Keep { message } => {
            note_app(state, AppUpdatePhase::UpToDate, message, None, None).await;
            let mut status = state.app_update.write().await;
            status.latest_version = Some(remote.version);
            status.latest_commit = remote.commit;
            status.update_available = false;
            status.percent = 100.0;
            return Ok(());
        }
        UpdateDecision::Update(plan) => plan,
    };
    {
        let mut status = state.app_update.write().await;
        status.latest_version = Some(plan.version.clone());
        status.latest_commit = plan.commit.clone();
        status.update_available = true;
        status.phase = AppUpdatePhase::UpToDate;
        status.error = None;
    }
    if mode == ApplyMode::CheckOnly {
        set_app_message(state, format!("发现程序 {}，还没有安装。", plan.version)).await;
        return Ok(());
    }
    if mode == ApplyMode::Auto && !config.auto_apply_app_update {
        set_app_message(
            state,
            format!("发现程序 {}。自动替换是关的，所以没有安装。", plan.version),
        )
        .await;
        return Ok(());
    }
    if mode == ApplyMode::Auto && running_from_cargo_target() {
        set_app_message(
            state,
            format!(
                "发现程序 {}。当前是开发构建，不会自动替换。安装版会自己更新。",
                plan.version
            ),
        )
        .await;
        return Ok(());
    }
    if task::game_update_busy(state).await {
        set_app_message(state, "发现新的程序版本。官服更新还在进行，完成后再替换。").await;
        return Ok(());
    }
    note_app(
        state,
        AppUpdatePhase::Downloading,
        format!("正在下载程序 {}。", plan.version),
        None,
        None,
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let progress_state = std::sync::Arc::clone(state);
    let progress = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            apply_stage_event(&progress_state, event).await;
        }
    });
    let manual = self_update_client(&config)?;
    let install_exe = std::env::current_exe().map_err(|err| format!("找不到当前程序：{err}"))?;
    let work_dir = state.paths.data_dir.join("updates");
    let staged = stage_planned(
        StageRequest {
            version_client: &client,
            manual_client: &manual,
            feed: &feed,
            install_exe: &install_exe,
            work_dir: &work_dir,
            current_version: current_version(),
            current_commit: current_commit(),
            target,
            cancel: &cancel,
            events: Some(&tx),
        },
        &plan,
    )
    .await;
    drop(tx);
    let _ = progress.await;
    let staged = staged?;
    if task::game_update_busy(state).await {
        set_app_message(state, "程序包已经下好。官服更新还在进行，完成后再重启。").await;
        return Ok(());
    }
    note_app(
        state,
        AppUpdatePhase::Applying,
        format!("正在换成程序 {} 并重启。", plan.version),
        None,
        None,
    )
    .await;
    self_update::relaunch_and_exit(&staged, &current_restart_args())
}

async fn tick_watch(
    state: &SharedState,
    config: &AppConfig,
    devices: &[DeviceSnapshot],
    cooldown: &mut HashMap<String, Instant>,
) -> String {
    let online: HashSet<String> = devices
        .iter()
        .filter(|device| device.online)
        .map(|device| device.serial.clone())
        .collect();
    cooldown.retain(|serial, until| online.contains(serial) && *until > Instant::now());
    let offline = offline_emulator_names(devices);
    if online.is_empty() {
        return if offline.is_empty() {
            "还没有在线设备。模拟器开机后会自动更新。".into()
        } else {
            format!(
                "正在等这些模拟器开机：{}。开机后会自动安装，不会为了检测去拉起它们。",
                offline.join("、")
            )
        };
    }
    if !config.auto_update_on_check {
        return "有设备在线。自动安装是关的，所以开机检测只查看、不安装。".into();
    }
    let official = match task::check_official(state, false).await {
        Ok(version) => version.client_version,
        Err(err) => return format!("设备在线，但官方版本没查到：{err}"),
    };
    let cooling = cooldown
        .iter()
        .filter(|(_, until)| **until > Instant::now())
        .map(|(serial, _)| serial.clone())
        .collect();
    let due = select_online_updates(devices, &official, &cooling);
    if due.is_empty() {
        return if offline.is_empty() {
            format!("已开机的设备都跟上了官方 {official}。")
        } else {
            format!(
                "已开机的设备跟上了官方 {official}。还没开机：{}。",
                offline.join("、")
            )
        };
    }
    match task::start_update_if_idle(
        state,
        UpdateRequest {
            devices: DeviceSelector::Only(due.clone()),
            force: false,
        },
    )
    .await
    {
        Ok(()) => {
            let until = Instant::now() + BOOT_COOLDOWN;
            for serial in &due {
                cooldown.insert(serial.clone(), until);
            }
            format!("检测到 {} 台在线设备需要更新，已开始安装。", due.len())
        }
        Err(err) => format!("检测到需要更新的在线设备，但没能开始：{err}"),
    }
}

fn needs_install(official: &str, installed: Option<&str>) -> bool {
    match installed {
        Some(version) => version_is_newer(official, version),
        None => true,
    }
}

fn scan_failure_message(err: &str, offline: &[String]) -> String {
    if offline.is_empty() {
        format!("扫描设备失败，等下一轮再看开机没有：{err}")
    } else {
        format!(
            "正在等这些模拟器开机：{}。这一轮没扫成：{err}",
            offline.join("、")
        )
    }
}

fn offline_emulator_names(devices: &[DeviceSnapshot]) -> Vec<String> {
    devices
        .iter()
        .filter(|device| !device.online && device.emulator_id.is_some())
        .map(|device| {
            device
                .emulator_name
                .clone()
                .unwrap_or_else(|| device.serial.clone())
        })
        .collect()
}

fn feed_of(config: &AppConfig) -> String {
    config
        .app_update_feed
        .clone()
        .unwrap_or_else(|| DEFAULT_RELEASE_API.to_string())
}

fn self_update_client(config: &AppConfig) -> Result<reqwest::Client, String> {
    crate::http_client::manual_redirect_client(config.proxy.as_deref())
}

async fn apply_stage_event(state: &SharedState, event: StageEvent) {
    let mut status = state.app_update.write().await;
    match event {
        StageEvent::Downloading { bytes, total } => {
            status.phase = AppUpdatePhase::Downloading;
            status.bytes = bytes;
            status.total = total;
            status.percent = match total {
                Some(total) if total > 0 => (bytes as f64 / total as f64 * 100.0).clamp(0.0, 100.0),
                _ => 0.0,
            };
            status.message = match total {
                Some(total) => format!(
                    "正在下载程序 {} / {}",
                    crate::util::format_bytes(bytes),
                    crate::util::format_bytes(total)
                ),
                None => format!("正在下载程序 {}", crate::util::format_bytes(bytes)),
            };
        }
        StageEvent::Verifying => {
            status.phase = AppUpdatePhase::Verifying;
            status.message = "正在核对程序包。".into();
        }
        StageEvent::Extracting => {
            status.phase = AppUpdatePhase::Verifying;
            status.message = "正在解出新程序。".into();
        }
    }
}

async fn note_app(
    state: &SharedState,
    phase: AppUpdatePhase,
    message: impl Into<String>,
    error: Option<String>,
    next_run: Option<Instant>,
) {
    let mut status = state.app_update.write().await;
    status.phase = phase;
    status.message = message.into();
    status.error = error;
    if let Some(next_run) = next_run {
        status.next_run_ms = Some(task::instant_to_unix_ms(next_run));
    }
}

async fn set_app_message(state: &SharedState, message: impl Into<String>) {
    let mut status = state.app_update.write().await;
    status.message = message.into();
    status.error = None;
}

async fn set_app_next(state: &SharedState, next_run: Option<Instant>) {
    state.app_update.write().await.next_run_ms = next_run.map(task::instant_to_unix_ms);
}

async fn publish_watch(
    state: &SharedState,
    config: &AppConfig,
    enabled: bool,
    next_run: Option<Instant>,
    message: &str,
    offline: &[String],
) {
    *state.watch.write().await = WatchStatus {
        enabled,
        interval: config.emulator_watch_interval.clone(),
        next_run_ms: next_run.map(task::instant_to_unix_ms),
        message: message.to_string(),
        offline_emulators: offline.to_vec(),
    };
}

struct AppJobGuard {
    state: SharedState,
}

impl Drop for AppJobGuard {
    fn drop(&mut self) {
        self.state.app_job_running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TaskView;

    fn device(serial: &str, online: bool, version: Option<&str>) -> DeviceSnapshot {
        let mut row = DeviceSnapshot::placeholder(serial, TaskView::idle());
        row.online = online;
        row.installed_version = version.map(str::to_string);
        if let Some(version) = version {
            row.installed_label = version.to_string();
        }
        row
    }

    #[test]
    fn online_outdated_device_is_selected_without_launching_offline() {
        let mut offline = device("127.0.0.1:16384", false, None);
        offline.emulator_id = Some("mumu-0".into());
        offline.emulator_name = Some("MuMu".into());
        let behind = device("127.0.0.1:5555", true, Some("2.7.80"));
        let current = device("127.0.0.1:5557", true, Some("2.7.91"));
        let cooling = HashSet::from(["127.0.0.1:5555".to_string()]);
        assert!(select_online_updates(
            &[offline.clone(), behind.clone(), current.clone()],
            "2.7.91",
            &cooling
        )
        .is_empty());
        let selected =
            select_online_updates(&[offline, behind, current], "2.7.91", &HashSet::new());
        assert_eq!(selected, vec!["127.0.0.1:5555".to_string()]);
    }

    #[test]
    fn unknown_online_version_is_selected() {
        let unknown = device("127.0.0.1:5555", true, None);
        let selected = select_online_updates(&[unknown], "2.7.91", &HashSet::new());
        assert_eq!(selected, vec!["127.0.0.1:5555".to_string()]);
    }

    #[test]
    fn scan_failure_still_names_offline_emulators() {
        let named = scan_failure_message("未找到 adb", &["演示 MuMu".into()]);
        assert!(named.contains("演示 MuMu"));
        assert!(named.contains("未找到 adb"));
        let plain = scan_failure_message("未找到 adb", &[]);
        assert!(plain.contains("扫描设备失败"));
        assert!(!plain.contains("正在等"));
    }
}
