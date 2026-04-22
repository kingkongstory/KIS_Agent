use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::presentation::app_state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/health", get(health_check))
}

async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "kis_agent",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
