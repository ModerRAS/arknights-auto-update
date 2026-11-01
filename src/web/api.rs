use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::adb::{discover_adb_auto, probe_adb as check_adb_binary};
use crate::config::EmulatorConfig;
use crate::state::SharedState;
use crate::task::{self, DeviceSelector, UpdateRequest};
use crate::util::{nonempty, valid_endpoint};

type ApiError = (StatusCode, Json<Value>);

fn err(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({"ok": false, "error": message.into()})))
}

pub async fn state(State(state): State<SharedState>) -> Json<Value> {
    let config = state.config.read().await.clone();
    let devices = state.devices.read().await.clone();
    let active = devices
        .iter()
        .filter(|device| device.task.phase.is_active())
        .count();
    Json(json!({
        "ok": true,
        "listen": state.bound_listen,
        "configured_listen": config.listen,
        "restart_required": state.bound_listen != config.listen,
        "config_path": state.paths.config_path.display().to_string(),
        "cache_dir": state.cache_dir(&config).display().to_string(),
        "concurrency": config.concurrency,
        "official": state.official.read().await.clone(),
        "download": state.download.read().await.clone(),
        "adb": state.adb.read().await.clone(),
        "device_count": devices.len(),
        "active_tasks": active,
        "auto": state.auto.read().await.clone(),
    }))
}

pub async fn devices(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "devices": state.devices.read().await.clone(),
    }))
}

pub async fn logs(State(state): State<SharedState>) -> Json<Value> {
    let recent = state
        .recent_logs
        .lock()
        .map(|logs| logs.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    Json(json!({"ok": true, "logs": recent}))
}

pub async fn refresh(State(state): State<SharedState>) -> Result<Json<Value>, ApiError> {
    task::refresh_devices(&state)
        .await
        .map_err(|message| err(StatusCode::BAD_REQUEST, message))?;
    Ok(Json(json!({
        "ok": true,
        "devices": state.devices.read().await.clone(),
    })))
}

pub async fn check(State(state): State<SharedState>) -> Result<Json<Value>, ApiError> {
    task::check_official(&state, true)
        .await
        .map_err(|message| err(StatusCode::BAD_GATEWAY, message))?;
    Ok(Json(json!({
        "ok": true,
        "official": state.official.read().await.clone(),
    })))
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateBody {
    #[serde(default)]
    devices: DeviceSelector,
    #[serde(default)]
    force: bool,
}

pub async fn update(
    State(state): State<SharedState>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<Value>, ApiError> {
    task::start_update(
        &state,
        UpdateRequest {
            devices: body.devices,
            force: body.force,
        },
    )
    .await
    .map_err(|message| err(StatusCode::BAD_REQUEST, message))?;
    Ok(Json(json!({"ok": true})))
}

pub async fn cancel(State(state): State<SharedState>) -> Json<Value> {
    task::cancel_all(&state).await;
    Json(json!({"ok": true}))
}

pub async fn get_config(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "config": state.config.read().await.clone(),
        "config_path": state.paths.config_path.display().to_string(),
    }))
}

pub async fn put_config(
    State(state): State<SharedState>,
    Json(mut config): Json<crate::config::AppConfig>,
) -> Result<Json<Value>, ApiError> {
    config.normalize();
    config
        .validate()
        .map_err(|message| err(StatusCode::BAD_REQUEST, message))?;
    task::persist_config(&state, &config)
        .await
        .map_err(|message| err(StatusCode::INTERNAL_SERVER_ERROR, message))?;
    *state.config.write().await = config.clone();
    state.reload_concurrency().await;
    let restart_required = state.bound_listen != config.listen;
    let adb_error = task::refresh_adb_status(&state).await.err();
    Ok(Json(json!({
        "ok": true,
        "config": config,
        "restart_required": restart_required,
        "adb_error": adb_error,
    })))
}

pub async fn list_emulators(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "emulators": state.config.read().await.emulators.clone(),
    }))
}

pub async fn create_emulator(
    State(state): State<SharedState>,
    Json(emulator): Json<EmulatorConfig>,
) -> Result<Json<Value>, ApiError> {
    let mut config = state.config.read().await.clone();
    config.emulators.push(emulator);
    saved_config(&state, config).await
}

pub async fn update_emulator(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(mut emulator): Json<EmulatorConfig>,
) -> Result<Json<Value>, ApiError> {
    emulator.id = id.clone();
    let mut config = state.config.read().await.clone();
    let Some(slot) = config.emulators.iter_mut().find(|item| item.id == id) else {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!("没有 id 为 {id} 的模拟器"),
        ));
    };
    *slot = emulator;
    saved_config(&state, config).await
}

pub async fn delete_emulator(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut config = state.config.read().await.clone();
    let before = config.emulators.len();
    config.emulators.retain(|item| item.id != id);
    if config.emulators.len() == before {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!("没有 id 为 {id} 的模拟器"),
        ));
    }
    saved_config(&state, config).await
}

async fn saved_config(
    state: &SharedState,
    config: crate::config::AppConfig,
) -> Result<Json<Value>, ApiError> {
    store_config(state, config).await?;
    Ok(Json(json!({
        "ok": true,
        "config": state.config.read().await.clone(),
    })))
}

async fn store_config(
    state: &SharedState,
    mut config: crate::config::AppConfig,
) -> Result<(), ApiError> {
    config.normalize();
    config
        .validate()
        .map_err(|message| err(StatusCode::BAD_REQUEST, message))?;
    task::persist_config(state, &config)
        .await
        .map_err(|message| err(StatusCode::INTERNAL_SERVER_ERROR, message))?;
    *state.config.write().await = config;
    state.reload_concurrency().await;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub(crate) struct EndpointBody {
    endpoint: String,
}

pub async fn add_device(
    State(state): State<SharedState>,
    Json(body): Json<EndpointBody>,
) -> Result<Json<Value>, ApiError> {
    let endpoint = body.endpoint.trim().to_string();
    if !valid_endpoint(&endpoint) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "地址无效。示例：127.0.0.1:16384 或 emulator-5554",
        ));
    }
    let mut config = state.config.read().await.clone();
    if !config.extra_devices.iter().any(|item| item == &endpoint) {
        config.extra_devices.push(endpoint);
    }
    store_config(&state, config).await?;
    let _ = task::refresh_devices(&state).await;
    Ok(Json(json!({
        "ok": true,
        "devices": state.devices.read().await.clone(),
    })))
}

pub async fn remove_device(
    State(state): State<SharedState>,
    Json(body): Json<EndpointBody>,
) -> Result<Json<Value>, ApiError> {
    let endpoint = body.endpoint.trim().to_string();
    let mut config = state.config.read().await.clone();
    config.extra_devices.retain(|item| item != &endpoint);
    store_config(&state, config).await?;
    {
        let mut devices = state.devices.write().await;
        devices.retain(|device| device.serial != endpoint || device.task.phase.is_active());
    }
    Ok(Json(json!({
        "ok": true,
        "devices": state.devices.read().await.clone(),
    })))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProbeBody {
    path: Option<String>,
    #[serde(default)]
    apply_fallback: bool,
}

pub async fn probe_adb(
    State(state): State<SharedState>,
    Json(body): Json<ProbeBody>,
) -> Result<Json<Value>, ApiError> {
    if let Some(path) = nonempty(body.path.as_deref()) {
        match check_adb_binary(std::path::Path::new(&path), "手动检测").await {
            Ok(info) => {
                return Ok(Json(json!({
                    "ok": true,
                    "path": info.path.display().to_string(),
                    "version": info.version,
                    "source": info.source,
                    "fallback_applied": false,
                })));
            }
            Err(message) => {
                let fallback = discover_adb_auto().await.ok();
                let mut applied = false;
                if body.apply_fallback {
                    if let Some(info) = &fallback {
                        let mut config = state.config.read().await.clone();
                        config.adb_path = None;
                        if let Err(save_error) = task::persist_config(&state, &config).await {
                            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, save_error));
                        }
                        *state.config.write().await = config;
                        *state.adb.write().await = crate::state::AdbStatus {
                            path: Some(info.path.display().to_string()),
                            version: Some(info.version.clone()),
                            source: info.source.clone(),
                            error: None,
                        };
                        applied = true;
                    }
                }
                return Ok(Json(json!({
                    "ok": false,
                    "error": message,
                    "path": fallback.as_ref().map(|info| info.path.display().to_string()),
                    "version": fallback.as_ref().map(|info| info.version.clone()),
                    "source": fallback.as_ref().map(|info| info.source.clone()),
                    "fallback_applied": applied,
                })));
            }
        }
    }
    match discover_adb_auto().await {
        Ok(info) => Ok(Json(json!({
            "ok": true,
            "path": info.path.display().to_string(),
            "version": info.version,
            "source": info.source,
            "fallback_applied": false,
        }))),
        Err(message) => Ok(Json(json!({
            "ok": false,
            "error": message,
            "fallback_applied": false,
        }))),
    }
}
