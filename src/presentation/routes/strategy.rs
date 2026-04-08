use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info};

use super::super::app_state::AppState;
use crate::domain::types::StockCode;
use crate::infrastructure::kis_client::http_client::KisHttpClient;
use crate::strategy::live_runner::LiveRunner;

/// 종목별 전략 실행 상태
#[derive(Debug, Clone, Serialize)]
pub struct StrategyStatus {
    pub code: String,
    pub name: String,
    pub active: bool,
    pub state: String,
    pub today_trades: i32,
    pub today_pnl: f64,
    pub message: String,
}

/// 종목별 러너 핸들
struct RunnerHandle {
    stop_flag: Arc<AtomicBool>,
    runner_state: Arc<RwLock<crate::strategy::live_runner::RunnerState>>,
}

/// 전략 관리자
#[derive(Clone)]
pub struct StrategyManager {
    pub statuses: Arc<RwLock<HashMap<String, StrategyStatus>>>,
    runners: Arc<RwLock<HashMap<String, RunnerHandle>>>,
    client: Arc<RwLock<Option<Arc<KisHttpClient>>>>,
}

impl StrategyManager {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        for (code, name) in [("122630", "KODEX 레버리지"), ("114800", "KODEX 인버스")] {
            map.insert(code.to_string(), StrategyStatus {
                code: code.to_string(),
                name: name.to_string(),
                active: false,
                state: "대기".to_string(),
                today_trades: 0,
                today_pnl: 0.0,
                message: "자동매매 비활성".to_string(),
            });
        }
        Self {
            statuses: Arc::new(RwLock::new(map)),
            runners: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_client(&self, client: Arc<KisHttpClient>) {
        let c = self.client.clone();
        tokio::spawn(async move {
            *c.write().await = Some(client);
        });
    }

    async fn start_runner(&self, code: &str, name: &str) -> Result<(), String> {
        let client_guard = self.client.read().await;
        let client = client_guard.as_ref()
            .ok_or("KIS 클라이언트가 초기화되지 않았습니다")?
            .clone();

        let stock_code = StockCode::new(code).map_err(|e| e.to_string())?;
        let stop_flag = Arc::new(AtomicBool::new(false));

        // 매수 수량 계산 (현재가 기준, 잔고 조회 후 5:5 배분 — 추후 구현)
        // 일단 1주로 시작
        let quantity = 1u64;

        let runner = LiveRunner::new(
            client,
            stock_code,
            name.to_string(),
            quantity,
            stop_flag.clone(),
        );

        let runner_state = runner.state.clone();
        let code_str = code.to_string();
        let statuses = self.statuses.clone();

        // 러너 핸들 저장
        self.runners.write().await.insert(code.to_string(), RunnerHandle {
            stop_flag: stop_flag.clone(),
            runner_state: runner_state.clone(),
        });

        // 백그라운드 태스크로 러너 실행
        tokio::spawn(async move {
            match runner.run().await {
                Ok(trades) => {
                    let pnl: f64 = trades.iter().map(|t| t.pnl_pct()).sum();
                    info!("{}: 자동매매 종료 — {}건 거래, 손익 {:.2}%", code_str, trades.len(), pnl);
                    let mut s = statuses.write().await;
                    if let Some(status) = s.get_mut(&code_str) {
                        status.active = false;
                        status.state = "종료".to_string();
                        status.today_trades = trades.len() as i32;
                        status.today_pnl = pnl;
                        status.message = format!("{}건 거래, {:.2}%", trades.len(), pnl);
                    }
                }
                Err(e) => {
                    error!("{}: 자동매매 에러 — {e}", code_str);
                    let mut s = statuses.write().await;
                    if let Some(status) = s.get_mut(&code_str) {
                        status.active = false;
                        status.state = "에러".to_string();
                        status.message = format!("에러: {e}");
                    }
                }
            }
        });

        // 상태 업데이트 태스크 (3초마다 러너 상태 → 웹 상태 동기화)
        let statuses2 = self.statuses.clone();
        let code2 = code.to_string();
        let rs2 = runner_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let rs = rs2.read().await;
                let mut s = statuses2.write().await;
                if let Some(status) = s.get_mut(&code2) {
                    if !status.active { break; }
                    status.state = rs.phase.clone();
                    status.today_trades = rs.today_trades.len() as i32;
                    status.today_pnl = rs.today_pnl;
                    if let Some(ref pos) = rs.current_position {
                        status.message = format!("{:?} {}주 @ {}", pos.side, pos.quantity, pos.entry_price);
                    } else {
                        status.message = rs.phase.clone();
                    }
                }
            }
        });

        Ok(())
    }

    async fn stop_runner(&self, code: &str) {
        let runners = self.runners.read().await;
        if let Some(handle) = runners.get(code) {
            handle.stop_flag.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Deserialize)]
pub struct ToggleRequest {
    pub code: String,
}

/// GET /api/v1/strategy/status
async fn get_status(State(state): State<AppState>) -> Json<Vec<StrategyStatus>> {
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
    let name = {
        let s = state.strategy_manager.statuses.read().await;
        s.get(&req.code).map(|s| s.name.clone()).unwrap_or_default()
    };

    match state.strategy_manager.start_runner(&req.code, &name).await {
        Ok(()) => {
            let mut s = state.strategy_manager.statuses.write().await;
            if let Some(status) = s.get_mut(&req.code) {
                status.active = true;
                status.state = "시작됨".to_string();
                status.message = "자동매매 시작됨".to_string();
                return Json(status.clone());
            }
        }
        Err(e) => {
            let mut s = state.strategy_manager.statuses.write().await;
            if let Some(status) = s.get_mut(&req.code) {
                status.message = format!("시작 실패: {e}");
                return Json(status.clone());
            }
        }
    }

    Json(StrategyStatus {
        code: req.code, name: String::new(), active: false,
        state: "오류".to_string(), today_trades: 0, today_pnl: 0.0,
        message: "종목 없음".to_string(),
    })
}

/// POST /api/v1/strategy/stop
async fn stop_strategy(
    State(state): State<AppState>,
    Json(req): Json<ToggleRequest>,
) -> Json<StrategyStatus> {
    state.strategy_manager.stop_runner(&req.code).await;

    let mut s = state.strategy_manager.statuses.write().await;
    if let Some(status) = s.get_mut(&req.code) {
        status.active = false;
        status.state = "중지 중".to_string();
        status.message = "중지 요청됨".to_string();
        Json(status.clone())
    } else {
        Json(StrategyStatus {
            code: req.code, name: String::new(), active: false,
            state: "오류".to_string(), today_trades: 0, today_pnl: 0.0,
            message: "종목 없음".to_string(),
        })
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/strategy/status", get(get_status))
        .route("/api/v1/strategy/start", post(start_strategy))
        .route("/api/v1/strategy/stop", post(stop_strategy))
}
