mod api;
mod sse;

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_embed::RustEmbed;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::state::SharedState;

#[derive(RustEmbed)]
#[folder = "web/"]
struct WebAssets;

pub fn router(state: SharedState) -> Router {
    let compressed = Router::new()
        .route("/api/state", get(api::state))
        .route("/api/devices", get(api::devices).post(api::add_device))
        .route("/api/devices/refresh", post(api::refresh))
        .route("/api/devices/remove", post(api::remove_device))
        .route("/api/check", post(api::check))
        .route("/api/update", post(api::update))
        .route("/api/update/cancel", post(api::cancel))
        .route("/api/config", get(api::get_config).put(api::put_config))
        .route(
            "/api/emulators",
            get(api::list_emulators).post(api::create_emulator),
        )
        .route(
            "/api/emulators/:id",
            axum::routing::put(api::update_emulator).delete(api::delete_emulator),
        )
        .route("/api/adb/probe", post(api::probe_adb))
        .route("/api/logs", get(api::logs))
        .fallback(static_handler)
        .layer(CompressionLayer::new());

    Router::new()
        .route("/api/events", get(sse::events))
        .merge(compressed)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match WebAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, mime.to_string())
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from(file.data.into_owned()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => (StatusCode::NOT_FOUND, "未找到页面").into_response(),
    }
}
