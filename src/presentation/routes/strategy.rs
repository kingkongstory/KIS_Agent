use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::super::app_state::AppState;

/// 종목별 전략 실행 상태
#[derive(Debug, Clone, Serialize)]
pub struct StrategyStatus {
    pub code: String,
    pub name: String,
    pub active: bool,
    pub state: String, // "대기", "OR 수집", "신호 탐색", "포지션 보유", "거래 완료"
    pub today_trades: i32,
    pub today_pnl: f64,
    pub message: String,
}

/// 전역 전략 상태 저장소
#[derive(Clone)]
pub struct StrategyManager {
    pub statuses: Arc<RwLock<HashMap<String, StrategyStatus>>>,
}

impl StrategyManager {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        map.insert(
            "122630".to_string(),
            StrategyStatus {
                code: "122630".to_string(),
                name: "KODEX 레버리지".to_string(),
                active: false,
                state: "대기".to_string(),
                today_trades: 0,
                today_pnl: 0.0,
                message: "자동매매 비활성".to_string(),
            },
        );
        map.insert(
            "114800".to_string(),
            StrategyStatus {
                code: "114800".to_string(),
                name: "KODEX 인버스".to_string(),
                active: false,
                state: "대기".to_string(),
                today_trades: 0,
                today_pnl: 0.0,
                message: "자동매매 비활성".to_string(),
            },
        );
        Self {
            statuses: Arc::new(RwLock::new(map)),
        }
    }
}

#[derive(Deserialize)]
pub struct ToggleRequest {
    pub code: String,
}

/// GET /api/v1/strategy/status
async fn get_status(
    State(state): State<AppState>,
) -> Json<Vec<StrategyStatus>> {
    let mgr = state.strategy_manager.statuses.read().await;
    let mut list: Vec<StrategyStatus> = mgr.values().cloned().collect();
    list.sort_by(|a, b| a.code.cmp(&b.code));
    Json(list)
}

/// POST /api/v1/strategy/start
async fn start_strategy(
    State(state): State<AppState>,
    Json(req): Json<ToggleRequest>,
) -> Json<StrategyStatus> {
    let mut mgr = state.strategy_manager.statuses.write().await;
    if let Some(status) = mgr.get_mut(&req.code) {
        status.active = true;
        status.state = "OR 수집 대기".to_string();
        status.message = "자동매매 시작됨".to_string();
        Json(status.clone())
    } else {
        Json(StrategyStatus {
            code: req.code,
            name: "알 수 없음".to_string(),
            active: false,
            state: "오류".to_string(),
            today_trades: 0,
            today_pnl: 0.0,
            message: "종목을 찾을 수 없습니다".to_string(),
        })
    }
}

/// POST /api/v1/strategy/stop
async fn stop_strategy(
    State(state): State<AppState>,
    Json(req): Json<ToggleRequest>,
) -> Json<StrategyStatus> {
    let mut mgr = state.strategy_manager.statuses.write().await;
    if let Some(status) = mgr.get_mut(&req.code) {
        status.active = false;
        status.state = "대기".to_string();
        status.message = "자동매매 중지됨".to_string();
        Json(status.clone())
    } else {
        Json(StrategyStatus {
            code: req.code,
            name: "알 수 없음".to_string(),
            active: false,
            state: "오류".to_string(),
            today_trades: 0,
            today_pnl: 0.0,
            message: "종목을 찾을 수 없습니다".to_string(),
        })
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/strategy/status", get(get_status))
        .route("/api/v1/strategy/start", post(start_strategy))
        .route("/api/v1/strategy/stop", post(stop_strategy))
}
