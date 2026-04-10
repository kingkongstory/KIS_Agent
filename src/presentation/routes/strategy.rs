use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};
use tracing::{error, info, warn};

use super::super::app_state::AppState;
use crate::domain::models::price::InquirePrice;
use crate::domain::ports::realtime::RealtimeData;
use crate::domain::types::{StockCode, TransactionId};
use crate::infrastructure::cache::postgres_store::PostgresStore;
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};
use crate::infrastructure::websocket::candle_aggregator::{CandleAggregator, CompletedCandle};
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
    pub or_high: Option<i64>,
    pub or_low: Option<i64>,
    /// Multi-Stage OR 범위 [(단계, high, low)]
    pub or_stages: Vec<(String, i64, i64)>,
    /// OR 백필 데이터 출처: "ws" / "yahoo" / "naver" / None (미수집)
    pub or_source: Option<String>,
    /// 전략 파라미터 요약
    pub params: StrategyParams,
}

/// 전략 파라미터 (웹 표시용)
#[derive(Debug, Clone, Serialize)]
pub struct StrategyParams {
    pub rr_ratio: f64,
    pub trailing_r: f64,
    pub breakeven_r: f64,
    pub max_daily_trades: usize,
    pub long_only: bool,
}

/// 종목별 러너 핸들
struct RunnerHandle {
    stop_flag: Arc<AtomicBool>,
    stop_notify: Arc<tokio::sync::Notify>,
    runner_state: Arc<RwLock<crate::strategy::live_runner::RunnerState>>,
}

/// 전략 관리자
#[derive(Clone)]
pub struct StrategyManager {
    pub statuses: Arc<RwLock<HashMap<String, StrategyStatus>>>,
    runners: Arc<RwLock<HashMap<String, RunnerHandle>>>,
    client: Arc<RwLock<Option<Arc<KisHttpClient>>>>,
    realtime_tx: Option<broadcast::Sender<RealtimeData>>,
    db_store: Option<Arc<PostgresStore>>,
    ws_candles: Option<Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>>,
    /// OR 백필 출처 조회 + 수동 리프레시용
    candle_aggregator: Option<Arc<CandleAggregator>>,
    /// 공유 포지션 잠금: 한 종목이 포지션 보유 중이면 다른 종목 진입 차단
    active_position_lock: Arc<RwLock<Option<String>>>,
}

impl StrategyManager {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        for (code, name) in [("122630", "KODEX 레버리지"), ("114800", "KODEX 인버스")] {
            let cfg = crate::strategy::orb_fvg::OrbFvgConfig::default();
            map.insert(code.to_string(), StrategyStatus {
                code: code.to_string(),
                name: name.to_string(),
                active: false,
                state: "대기".to_string(),
                today_trades: 0,
                today_pnl: 0.0,
                message: "자동매매 비활성".to_string(),
                or_high: None,
                or_low: None,
                or_stages: Vec::new(),
                or_source: None,
                params: StrategyParams {
                    rr_ratio: cfg.rr_ratio,
                    trailing_r: cfg.trailing_r,
                    breakeven_r: cfg.breakeven_r,
                    max_daily_trades: cfg.max_daily_trades,
                    long_only: cfg.long_only,
                },
            });
        }
        Self {
            statuses: Arc::new(RwLock::new(map)),
            runners: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(RwLock::new(None)),
            realtime_tx: None,
            db_store: None,
            ws_candles: None,
            candle_aggregator: None,
            active_position_lock: Arc::new(RwLock::new(None)),
        }
    }

    /// 활성 자동매매 러너가 있는지 확인
    pub async fn has_active_runners(&self) -> bool {
        let statuses = self.statuses.read().await;
        statuses.values().any(|s| s.active)
    }

    pub fn set_client(&self, client: Arc<KisHttpClient>) {
        let c = self.client.clone();
        tokio::spawn(async move {
            *c.write().await = Some(client);
        });
    }

    pub fn set_realtime_tx(&mut self, tx: broadcast::Sender<RealtimeData>) {
        self.realtime_tx = Some(tx);
    }

    pub fn set_db_store(&mut self, store: Arc<PostgresStore>) {
        self.db_store = Some(store);
    }

    pub fn set_ws_candles(&mut self, candles: Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>) {
        self.ws_candles = Some(candles);
    }

    pub fn set_candle_aggregator(&mut self, agg: Arc<CandleAggregator>) {
        self.candle_aggregator = Some(agg);
    }

    /// 서버 시작 시 모든 종목 자동매매 활성화 + 잔존 포지션 복구
    pub async fn auto_start_all(&self) {
        // 잔고 조회하여 보유 종목 확인
        let _held_positions: std::collections::HashMap<String, (i64, u64)> = {
            let client_guard = self.client.read().await;
            if let Some(client) = client_guard.as_ref() {
                match self.fetch_balance(client).await {
                    Ok(positions) => positions,
                    Err(e) => {
                        warn!("잔고 조회 실패 (포지션 복구 불가): {e}");
                        std::collections::HashMap::new()
                    }
                }
            } else {
                std::collections::HashMap::new()
            }
        };

        let codes: Vec<(String, String)> = {
            let s = self.statuses.read().await;
            s.values().map(|v| (v.code.clone(), v.name.clone())).collect()
        };

        for (code, name) in &codes {
            match self.start_runner(code, name).await {
                Ok(()) => {
                    let mut s = self.statuses.write().await;
                    if let Some(status) = s.get_mut(code) {
                        status.active = true;
                        status.state = "시작됨".to_string();
                        status.message = "자동매매 시작됨".to_string();
                    }
                    info!("{}: 자동매매 자동 시작", name);

                    // 보유 포지션이 있으면 LiveRunner에 전달하여 복구
                    // (LiveRunner가 run() 시작 시 자동 감지)
                }
                Err(e) => {
                    error!("{}: 자동매매 자동 시작 실패: {e}", name);
                }
            }
        }
    }

    /// 가용 현금 조회 — D+2 가수도정산금액(prvs_rcdl_excc_amt) 사용
    /// KIS HTS "D+2 예수금"과 동일하며 매도 정산 대금까지 포함된 실제 주문가능 cash.
    async fn get_available_cash(&self, client: &KisHttpClient) -> i64 {
        let query = [
            ("CANO", client.account_no()),
            ("ACNT_PRDT_CD", client.account_product_code()),
            ("AFHR_FLPR_YN", "N"), ("OFL_YN", ""), ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"), ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"), ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""), ("CTX_AREA_NK100", ""),
        ];
        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = client
            .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance, Some(&query), None)
            .await;
        if let Ok(r) = resp {
            if let Some(output2) = r.output2 {
                if let Some(first) = output2.first() {
                    let d2_cash: i64 = first.get("prvs_rcdl_excc_amt")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    info!("가용 현금: D+2 예수금 {}", d2_cash);
                    return d2_cash;
                }
            }
        }
        warn!("가용 현금 조회 실패 — 500만원 기본값 사용");
        5_000_000 // 최소 안전값
    }

    /// 잔고 조회 → 보유 종목별 (평균가, 수량) 반환
    async fn fetch_balance(&self, client: &KisHttpClient) -> Result<std::collections::HashMap<String, (i64, u64)>, String> {
        let query = [
            ("CANO", client.account_no()),
            ("ACNT_PRDT_CD", client.account_product_code()),
            ("AFHR_FLPR_YN", "N"),
            ("OFL_YN", ""),
            ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"),
            ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"),
            ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];

        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = client
            .execute(HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &crate::domain::types::TransactionId::InquireBalance,
                Some(&query), None)
            .await;

        match resp {
            Ok(r) if r.rt_cd == "0" => {
                let mut map = std::collections::HashMap::new();
                if let Some(items) = r.output {
                    for item in &items {
                        let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
                        let qty_str = item.get("hldg_qty").and_then(|v| v.as_str()).unwrap_or("0");
                        let avg_str = item.get("pchs_avg_pric").and_then(|v| v.as_str()).unwrap_or("0");
                        let qty: u64 = qty_str.parse().unwrap_or(0);
                        let avg: f64 = avg_str.parse().unwrap_or(0.0);
                        if qty > 0 && !code.is_empty() {
                            map.insert(code.to_string(), (avg as i64, qty));
                        }
                    }
                }
                Ok(map)
            }
            Ok(r) => Err(format!("잔고 조회 실패: {}", r.msg1)),
            Err(e) => Err(format!("잔고 조회 에러: {e}")),
        }
    }

    async fn start_runner(&self, code: &str, name: &str) -> Result<(), String> {
        // 이미 실행 중인 러너가 있으면 거부 (좀비 태스크 방지)
        if self.runners.read().await.contains_key(code) {
            return Err("이미 실행 중입니다".to_string());
        }

        let client_guard = self.client.read().await;
        let client = client_guard.as_ref()
            .ok_or("KIS 클라이언트가 초기화되지 않았습니다")?
            .clone();

        let stock_code = StockCode::new(code).map_err(|e| e.to_string())?;
        let stop_flag = Arc::new(AtomicBool::new(false));

        // 전액 투입: KIS 매수가능조회 API로 실제 주문가능수량 조회
        let quantity = {
            // 1) 현재가 조회
            let query = [
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", code),
            ];
            let resp: KisResponse<InquirePrice> = client
                .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/quotations/inquire-price",
                    &TransactionId::InquirePrice, Some(&query), None)
                .await
                .map_err(|e| format!("현재가 조회 실패: {e}"))?;
            let price_data = resp.into_result().map_err(|e| format!("현재가 파싱 실패: {e}"))?;
            let price = price_data.stck_prpr;
            if price <= 0 {
                return Err("현재가가 0 이하입니다".to_string());
            }

            // 2) 매수가능수량 조회 (KIS API가 가용금액 기준으로 계산)
            let price_str = price.to_string();
            let buyable_query = [
                ("CANO", client.account_no()),
                ("ACNT_PRDT_CD", client.account_product_code()),
                ("PDNO", code),
                ("ORD_UNPR", price_str.as_str()),
                ("ORD_DVSN", "01"),
                ("CMA_EVLU_AMT_ICLD_YN", "N"),
                ("OVRS_ICLD_YN", "N"),
            ];
            use crate::domain::models::account::BuyableInfo;
            let buyable_resp: Result<KisResponse<BuyableInfo>, _> = client
                .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/trading/inquire-psbl-order",
                    &TransactionId::InquireBuyable, Some(&buyable_query), None)
                .await;

            let qty = match buyable_resp {
                Ok(r) => {
                    match r.into_result() {
                        Ok(info) => {
                            let api_qty = info.orderable_qty() as u64;
                            info!("{}: 현재가 {}원, 주문가능금액 {}원, 주문가능수량 {}주 (증거금율 반영)",
                                name, price, info.orderable_cash(), api_qty);
                            api_qty
                        }
                        Err(e) => {
                            warn!("{}: 매수가능조회 파싱 실패 — 잔고 기반 fallback (80%): {e}", name);
                            ((self.get_available_cash(&client).await as f64 * 0.80) as i64 / price) as u64
                        }
                    }
                }
                Err(e) => {
                    warn!("{}: 매수가능조회 실패 — 잔고 기반 fallback (80%): {e}", name);
                    ((self.get_available_cash(&client).await as f64 * 0.80) as i64 / price) as u64
                }
            };

            if qty == 0 {
                return Err(format!("현재가 {}원 — 주문가능수량 0주", price));
            }
            qty
        };

        let mut runner = LiveRunner::new(
            client,
            stock_code,
            name.to_string(),
            quantity,
            stop_flag.clone(),
        );
        if let Some(ref tx) = self.realtime_tx {
            runner = runner.with_trade_tx(tx.clone());
        }
        if let Some(ref store) = self.db_store {
            runner = runner.with_db_store(Arc::clone(store));
        }
        if let Some(ref candles) = self.ws_candles {
            runner = runner.with_ws_candles(Arc::clone(candles));
        }
        runner = runner.with_position_lock(Arc::clone(&self.active_position_lock));

        let stop_notify = runner.stop_notify();
        let runner_state = runner.state.clone();
        let code_str = code.to_string();
        let statuses = self.statuses.clone();
        let runners_cleanup = self.runners.clone();

        // 러너 핸들 저장
        self.runners.write().await.insert(code.to_string(), RunnerHandle {
            stop_flag: stop_flag.clone(),
            stop_notify,
            runner_state: runner_state.clone(),
        });

        // 백그라운드 태스크로 러너 실행
        let code_cleanup = code.to_string();
        tokio::spawn(async move {
            let mut runner = runner;
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
            // 러너 종료 후 핸들 제거 (재시작 허용)
            runners_cleanup.write().await.remove(&code_cleanup);
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
                    status.or_high = rs.or_high;
                    status.or_low = rs.or_low;
                    status.or_stages = rs.or_stages.clone();
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
            handle.stop_notify.notify_waiters(); // sleep 즉시 깨움
        }
    }
}

#[derive(Deserialize)]
pub struct ToggleRequest {
    pub code: String,
}

/// GET /api/v1/strategy/status
async fn get_status(State(state): State<AppState>) -> Json<Vec<StrategyStatus>> {
    let mut list: Vec<StrategyStatus> = {
        let mgr = state.strategy_manager.statuses.read().await;
        mgr.values().cloned().collect()
    };
    list.sort_by(|a, b| b.code.cmp(&a.code));

    // aggregator로부터 실효 출처 덮어쓰기
    if let Some(ref agg) = state.strategy_manager.candle_aggregator {
        for status in list.iter_mut() {
            status.or_source = agg.effective_source(&status.code).await;
        }
    }

    Json(list)
}

/// POST /api/v1/strategy/refresh-or-backfill
/// 수동 OR 백필 재수행 — Yahoo 1순위, 실패 시 네이버 fallback.
async fn refresh_or_backfill(State(state): State<AppState>) -> Json<RefreshOrResponse> {
    let Some(ref agg) = state.strategy_manager.candle_aggregator else {
        return Json(RefreshOrResponse {
            ok: false,
            message: "CandleAggregator 미설정".to_string(),
            sources: HashMap::new(),
        });
    };

    let codes = ["122630", "114800"];
    agg.backfill_or(&codes, true).await;

    let mut sources = HashMap::new();
    for code in &codes {
        if let Some(src) = agg.effective_source(code).await {
            sources.insert((*code).to_string(), src);
        }
    }

    Json(RefreshOrResponse {
        ok: true,
        message: "백필 재수행 완료".to_string(),
        sources,
    })
}

#[derive(Serialize)]
struct RefreshOrResponse {
    ok: bool,
    message: String,
    sources: HashMap<String, String>,
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

    let cfg = crate::strategy::orb_fvg::OrbFvgConfig::default();
    Json(StrategyStatus {
        code: req.code, name: String::new(), active: false,
        state: "오류".to_string(), today_trades: 0, today_pnl: 0.0,
        message: "종목 없음".to_string(),
        or_high: None, or_low: None, or_stages: Vec::new(), or_source: None,
        params: StrategyParams {
            rr_ratio: cfg.rr_ratio, trailing_r: cfg.trailing_r,
            breakeven_r: cfg.breakeven_r, max_daily_trades: cfg.max_daily_trades,
            long_only: cfg.long_only,
        },
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
        let cfg = crate::strategy::orb_fvg::OrbFvgConfig::default();
        Json(StrategyStatus {
            code: req.code, name: String::new(), active: false,
            state: "오류".to_string(), today_trades: 0, today_pnl: 0.0,
            message: "종목 없음".to_string(),
            or_high: None, or_low: None, or_stages: Vec::new(), or_source: None,
            params: StrategyParams {
                rr_ratio: cfg.rr_ratio, trailing_r: cfg.trailing_r,
                breakeven_r: cfg.breakeven_r, max_daily_trades: cfg.max_daily_trades,
                long_only: cfg.long_only,
            },
        })
    }
}

/// 거래 기록 응답
#[derive(Debug, Serialize, sqlx::FromRow)]
struct TradeRow {
    id: i64,
    stock_code: String,
    stock_name: String,
    side: String,
    quantity: i64,
    entry_price: i64,
    exit_price: i64,
    exit_reason: String,
    pnl_pct: f64,
    entry_time: chrono::NaiveDateTime,
    exit_time: chrono::NaiveDateTime,
}

/// GET /api/v1/strategy/trades — 당일 거래 내역
async fn get_trades(State(state): State<AppState>) -> Json<Vec<TradeRow>> {
    let Some(ref pool) = state.db_pool else {
        return Json(Vec::new());
    };

    let rows: Vec<TradeRow> = sqlx::query_as(
        "SELECT id, stock_code, stock_name, side, quantity, entry_price, exit_price,
         exit_reason, pnl_pct, entry_time, exit_time
         FROM trades WHERE entry_time::date = CURRENT_DATE
         ORDER BY id DESC LIMIT 50"
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Json(rows)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/strategy/status", get(get_status))
        .route("/api/v1/strategy/start", post(start_strategy))
        .route("/api/v1/strategy/stop", post(stop_strategy))
        .route("/api/v1/strategy/trades", get(get_trades))
        .route(
            "/api/v1/strategy/refresh-or-backfill",
            post(refresh_or_backfill),
        )
}
