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
use crate::infrastructure::monitoring::event_logger::EventLogger;
use crate::infrastructure::websocket::candle_aggregator::{CandleAggregator, CompletedCandle};
use crate::strategy::live_runner::{
    ExecutionState, LiveRunner, LiveRunnerConfig, PositionLockState,
};

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
    /// Yahoo OR 교체 실패 단계 (빈 배열이면 정상)
    pub or_refresh_warnings: Vec<String>,
    /// 전략 파라미터 요약
    pub params: StrategyParams,
    /// degraded — WS 체결통보 비정상 등 시스템 신뢰성 미달 시 true. 진입 차단.
    pub degraded: bool,
    /// 사람 개입 필요 — 재시작 복구 시 메타 부족으로 자동 재구성 불가. 자동매매 중단 상태.
    pub manual_intervention_required: bool,
    /// degraded/manual 사유 (UI 표시용)
    pub degraded_reason: Option<String>,
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

/// 2026-04-19 PR #2.e: `GET /api/v1/monitoring/armed-stats` 응답 항목.
///
/// `ArmedWatchStats` 를 그대로 싣고, 판단에 필요한 현재 state 플래그 및 편의
/// avg 필드를 포함한다. 프론트엔드가 종목별 카운터/지연 대시보드를 그릴 수 있도록
/// 모든 값을 serializable 로 노출.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArmedStatsEntry {
    pub stock_code: String,
    pub stats: crate::strategy::live_runner::ArmedWatchStats,
    pub current_armed_signal_id: Option<String>,
    pub manual_intervention_required: bool,
    pub degraded: bool,
    pub execution_state: String,
    pub ready_avg_duration_ms: Option<f64>,
    pub aborted_avg_duration_ms: Option<f64>,
}

/// WS 체결통보 startup health 상태.
///
/// 2026-04-15 Codex review #2 대응. 기존 `system_degraded: Option<String>` 는
/// `None` = "아직 판정 전 or 정상" 으로 두 상태가 섞여 있어, 기동 직후 race 로
/// UI 가 수동 start 를 누르면 health gate 판정 전인데도 통과해 버리는 버그가 있었다.
/// `Pending` 초기 상태를 명시하여 "모르면 거절" 원칙을 강제한다.
///
/// 중요: 이 enum 은 **startup gate 전용**이다. 기동 후 runtime 에서 AES key 가
/// 소실되는 사후 장애는 같은 상태로 다루지 않는다 (이미 보유한 live position 을
/// 시장가 청산으로 몰아넣는 회귀를 피하기 위함). runtime degradation 은 future work.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum NotificationHealth {
    /// 초기 상태 — startup health gate 판정 이전. `start_runner` 거부.
    #[default]
    Pending,
    /// AES key/iv 수립 확인 — 거래 허용.
    Ready,
    /// startup gate 실패 — 이후 신규 start 영구 거부.
    Failed(String),
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
    active_position_lock: Arc<RwLock<PositionLockState>>,
    /// 운영 이벤트 로거 (fire-and-forget 비동기 DB 저장)
    pub event_logger: Option<Arc<EventLogger>>,
    /// WS 체결통보 startup health 상태.
    ///
    /// 2026-04-15 Codex review #2 대응. `Pending` 초기 상태를 명시하여 기동 직후
    /// race 로 UI 가 start 를 눌러도 health gate 판정 전이면 거부되게 한다.
    /// startup 판정은 `mark_notification_ready` / `mark_notification_startup_failed`
    /// 로만 전이한다. runtime degradation 은 본 enum 으로 다루지 않음.
    notification_health: Arc<RwLock<NotificationHealth>>,
    /// 자동매매 허용 종목 화이트리스트. 비어있으면 모든 등록 종목 허용.
    /// `KIS_ALLOWED_CODES` 환경변수에서 파싱. 실전 초기 122630 단일 운용 시 사용.
    /// 2026-04-16 Task 4 — 허용 목록 밖 종목은 start_runner 단계에서 거부.
    allowed_codes: Arc<RwLock<Vec<String>>>,
    /// LiveRunner 생성 시 주입할 라이브 전용 설정.
    /// 실전 모드에서는 `LiveRunnerConfig::for_real_mode` 로 구성된 보수 기본값.
    live_runner_config: Arc<RwLock<LiveRunnerConfig>>,
    /// 실전 주문 경로 opt-in 플래그 — `false` 이면 start_runner 에서 거부.
    /// `AppConfig.is_real_mode() && !AppConfig.enable_real_trading` 조합에서 start 를
    /// 봉쇄하기 위한 마지막 하드 가드. 2026-04-16 Task 5.
    block_starts_reason: Arc<RwLock<Option<String>>>,
    /// reconcile 불일치 감지 후 재확인 시도 횟수 (default 3).
    /// 2026-04-17 P1 — `confirm_reconcile_qty` 가 사용. 첫 일치 시 즉시 통과.
    reconcile_confirm_attempts: u32,
    /// reconcile 재확인 시도 간 대기 (초, default 5).
    /// 2026-04-17 P1 — KIS 잔고 API 일시 지연을 흡수하면서도 진짜 hidden position
    /// 감지 지연을 최소화하기 위한 보수 기본값.
    reconcile_confirm_interval_secs: u64,
    /// 2026-04-17 v3 F1.1+: 종목별 reconcile 재확인 연속 inconclusive 횟수.
    /// 모든 attempt fetch 가 실패하면 +1, 한 번이라도 성공하면 0 으로 reset.
    /// 3회 연속 inconclusive 면 "상태 불명" manual_intervention 으로 승격.
    /// 무기한 유예가 hidden position 을 못 보는 위험을 차단한다.
    reconcile_inconclusive_streaks: Arc<RwLock<HashMap<String, u32>>>,
}

const RECONCILE_POST_FLAT_SETTLE_WINDOW_SECS: u64 = 10;

impl Default for StrategyManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StrategyManager {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        for (code, name) in [("122630", "KODEX 레버리지"), ("114800", "KODEX 인버스")] {
            let cfg = crate::strategy::orb_fvg::OrbFvgConfig::default();
            map.insert(
                code.to_string(),
                StrategyStatus {
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
                    or_refresh_warnings: Vec::new(),
                    params: StrategyParams {
                        rr_ratio: cfg.rr_ratio,
                        trailing_r: cfg.trailing_r,
                        breakeven_r: cfg.breakeven_r,
                        max_daily_trades: cfg.max_daily_trades,
                        long_only: cfg.long_only,
                    },
                    degraded: false,
                    manual_intervention_required: false,
                    degraded_reason: None,
                },
            );
        }
        Self {
            statuses: Arc::new(RwLock::new(map)),
            runners: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(RwLock::new(None)),
            realtime_tx: None,
            db_store: None,
            ws_candles: None,
            candle_aggregator: None,
            active_position_lock: Arc::new(RwLock::new(PositionLockState::Free)),
            event_logger: None,
            notification_health: Arc::new(RwLock::new(NotificationHealth::Pending)),
            allowed_codes: Arc::new(RwLock::new(Vec::new())),
            live_runner_config: Arc::new(RwLock::new(LiveRunnerConfig::default())),
            block_starts_reason: Arc::new(RwLock::new(None)),
            reconcile_confirm_attempts: 3,
            reconcile_confirm_interval_secs: 5,
            reconcile_inconclusive_streaks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// reconcile 재확인 파라미터 주입 (2026-04-17 P1).
    /// 인자 하한: attempts ≥ 1, interval_secs ≥ 2.
    pub fn set_reconcile_confirm_params(&mut self, attempts: u32, interval_secs: u64) {
        self.reconcile_confirm_attempts = attempts.max(1);
        self.reconcile_confirm_interval_secs = interval_secs.max(2);
    }

    /// 2026-04-17 v3 F1.1+: 종목별 inconclusive streak 를 1 증가시키고 새 값 반환.
    async fn bump_reconcile_inconclusive_streak(&self, code: &str) -> u32 {
        let mut map = self.reconcile_inconclusive_streaks.write().await;
        let entry = map.entry(code.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// 2026-04-17 v3 F1.1+: 재확인 fetch 가 1회라도 성공한 종목의 streak 를 0 으로 reset.
    async fn reset_reconcile_inconclusive_streak(&self, code: &str) {
        let mut map = self.reconcile_inconclusive_streaks.write().await;
        map.remove(code);
    }

    /// 자동매매 허용 종목 주입. 비어있으면 모든 등록 종목 허용.
    pub async fn set_allowed_codes(&self, codes: Vec<String>) {
        *self.allowed_codes.write().await = codes;
    }

    /// LiveRunner 에 주입할 설정 저장. start_runner 에서 clone 해서 사용한다.
    pub async fn set_live_runner_config(&self, cfg: LiveRunnerConfig) {
        *self.live_runner_config.write().await = cfg;
    }

    /// 실전 주문 경로 영구 차단 사유 설정.
    /// `KIS_ENABLE_REAL_TRADING=false` 상태에서 실전 환경이면 호출된다.
    /// start_runner 는 이 값이 `Some` 이면 모든 시작 요청을 거부한다.
    pub async fn set_block_starts(&self, reason: Option<String>) {
        *self.block_starts_reason.write().await = reason;
    }

    /// 현재 전역 거래 카운터 스냅샷. UI/모니터링용.
    pub async fn global_trade_snapshot(&self) -> Option<(usize, usize)> {
        let cfg = self.live_runner_config.read().await;
        if let Some(ref gate) = cfg.global_trade_gate {
            let today = chrono::Local::now().date_naive();
            let (count, max) = gate.write().await.snapshot(today);
            Some((count, max))
        } else {
            None
        }
    }

    /// WS 체결통보 startup health 가 정상 수립됨을 표시.
    ///
    /// main.rs 의 health gate 폴링이 AES key/iv 수립 확인 후 호출한다.
    /// 이전까지 `Pending` 이라 거부되던 `start_runner` 호출이 이 시점부터 허용된다.
    pub async fn mark_notification_ready(&self) {
        *self.notification_health.write().await = NotificationHealth::Ready;
        // 각 StrategyStatus 의 degraded 플래그도 해제 (기동 초기 Pending 동안 UI 에
        // 표시됐을 수 있는 "startup 진행 중" 메시지를 정리).
        let mut s = self.statuses.write().await;
        for status in s.values_mut() {
            // 러너 고유 degraded(예: manual_intervention) 상태는 건드리지 않는다 —
            // `manual_intervention_required` 플래그가 true 면 message 만 덮어쓰지 않고
            // 정상 경로일 때만 "자동매매 비활성" 으로 되돌린다.
            if !status.manual_intervention_required {
                status.degraded = false;
                status.degraded_reason = None;
                if status.message.starts_with("degraded:") {
                    status.message = "자동매매 비활성".to_string();
                }
            }
        }
    }

    /// WS 체결통보 startup health gate 실패.
    ///
    /// - 상태를 `Failed` 로 고정하여 이후 모든 `start_runner` 를 영구 거부
    /// - 기동 직후 race (`Pending` 상태에서 수동 start 성공) 로 이미 시작된 러너가
    ///   있으면 `stop_flag` 를 세워 즉시 중단
    /// - 각 StrategyStatus 에 degraded 전파 + event_log critical 기록
    ///
    /// 호출 제약: `Pending` 상태에서만 호출한다. `Ready` 이후 runtime 장애는
    /// 본 메서드로 다루지 않는다 (이미 보유 포지션을 강제 청산하는 회귀 차단).
    pub async fn mark_notification_startup_failed(&self, reason: String) {
        *self.notification_health.write().await = NotificationHealth::Failed(reason.clone());

        // Pending race 로 이미 시작된 러너를 강제 중단.
        // stop_flag + stop_notify 만 세우고, 러너 자체의 graceful shutdown 경로
        // (체결 대기 루프 탈출 → cancel → run 종료)를 신뢰한다.
        {
            let runners = self.runners.read().await;
            for (code, handle) in runners.iter() {
                warn!(
                    "{}: notification_startup_failed — Pending race 러너 강제 중단",
                    code
                );
                handle.stop_flag.store(true, Ordering::Relaxed);
                handle.stop_notify.notify_waiters();
            }
        }

        // 각 StrategyStatus 에 degraded 전파.
        {
            let mut s = self.statuses.write().await;
            for status in s.values_mut() {
                status.degraded = true;
                status.degraded_reason = Some(reason.clone());
                status.message = format!("degraded: {}", reason);
            }
        }

        if let Some(ref el) = self.event_logger {
            el.log_event(
                "",
                "system",
                "notification_startup_failed",
                "critical",
                &reason,
                serde_json::json!({}),
            );
        }
    }

    pub fn set_event_logger(&mut self, logger: Arc<EventLogger>) {
        self.event_logger = Some(logger);
    }

    /// 활성 자동매매 러너가 있는지 확인
    pub async fn has_active_runners(&self) -> bool {
        let statuses = self.statuses.read().await;
        statuses.values().any(|s| s.active)
    }

    /// 2026-04-24 P0-1 보강: 장외 WS backoff 확대가 안전한 상태인지 계산한다.
    ///
    /// WebSocket 계층은 `ManualHeld` / pending / open 상태를 직접 알 수 없으므로
    /// 운영 레이어가 주기적으로 이 값을 계산해 `KisWebSocketClient` 의 안전 플래그를
    /// 갱신한다. 하나라도 상태가 불명확하거나 포지션 관련 위험이 있으면 fail-closed 한다.
    pub async fn off_hours_ws_backoff_safe(&self) -> bool {
        if !matches!(
            *self.active_position_lock.read().await,
            PositionLockState::Free
        ) {
            return false;
        }

        let (active_codes, known_codes) = {
            let statuses = self.statuses.read().await;
            let mut active_codes = Vec::new();
            let mut known_codes = Vec::new();
            for status in statuses.values() {
                known_codes.push(status.code.clone());
                if status.manual_intervention_required || status.degraded {
                    return false;
                }
                if status.active {
                    active_codes.push(status.code.clone());
                }
            }
            (active_codes, known_codes)
        };

        if let Some(store) = &self.db_store {
            for code in &known_codes {
                match store.get_active_position(code).await {
                    Ok(Some(_)) => return false,
                    Ok(None) => {}
                    Err(_) => {
                        // DB 상태를 확인하지 못하면 hidden position 가능성을 배제할 수 없으므로 차단한다.
                        return false;
                    }
                }
            }
        }

        let runners = self.runners.read().await;
        for code in &active_codes {
            if !runners.contains_key(code) {
                // status 는 active 인데 runner handle 이 아직 없으면 시작/종료 race 로 보고 보수적으로 차단.
                return false;
            }
        }

        for handle in runners.values() {
            let state = handle.runner_state.read().await;
            if state.manual_intervention_required
                || state.degraded
                || state.exit_pending
                || state.current_position.is_some()
                || state.execution_state != ExecutionState::Flat
            {
                return false;
            }
        }

        true
    }

    /// 2026-04-19 PR #2.e: 전체 러너의 armed watch 지표 스냅샷.
    ///
    /// monitoring API 가 read-only 로 호출. lock 을 잠깐만 잡는다 (이유별 카운터 +
    /// duration + 현재 armed 중인 signal_id + state flag).
    pub async fn snapshot_armed_stats(&self) -> Vec<ArmedStatsEntry> {
        let runners = self.runners.read().await;
        let mut out = Vec::with_capacity(runners.len());
        for (code, handle) in runners.iter() {
            let state = handle.runner_state.read().await;
            out.push(ArmedStatsEntry {
                stock_code: code.clone(),
                stats: state.armed_stats.clone(),
                current_armed_signal_id: state.current_armed_signal_id.clone(),
                manual_intervention_required: state.manual_intervention_required,
                degraded: state.degraded,
                execution_state: state.execution_state.label().to_string(),
                ready_avg_duration_ms: state.armed_stats.ready_avg_duration_ms(),
                aborted_avg_duration_ms: state.armed_stats.aborted_avg_duration_ms(),
            });
        }
        out.sort_by(|a, b| a.stock_code.cmp(&b.stock_code));
        out
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

    pub fn set_ws_candles(
        &mut self,
        candles: Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>,
    ) {
        self.ws_candles = Some(candles);
    }

    pub fn set_candle_aggregator(&mut self, agg: Arc<CandleAggregator>) {
        self.candle_aggregator = Some(agg);
    }

    /// 서버 시작 시 모든 종목 자동매매 활성화 + 잔존 포지션 복구
    pub async fn auto_start_all(&self) {
        // 호출자(main.rs) 가 `mark_notification_ready` 후 호출하므로 항상 Ready 여야 하지만,
        // 극히 짧은 race 를 방어하기 위해 재확인.
        if !matches!(
            &*self.notification_health.read().await,
            NotificationHealth::Ready
        ) {
            warn!("auto_start_all: notification_health 가 Ready 가 아님 — 자동 시작 스킵");
            return;
        }

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
            s.values()
                .map(|v| (v.code.clone(), v.name.clone()))
                .collect()
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
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await;
        if let Ok(r) = resp
            && let Some(output2) = r.output2
            && let Some(first) = output2.first()
        {
            let d2_cash: i64 = first
                .get("prvs_rcdl_excc_amt")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            info!("가용 현금: D+2 예수금 {}", d2_cash);
            return d2_cash;
        }
        warn!("가용 현금 조회 실패 — 500만원 기본값 사용");
        5_000_000 // 최소 안전값
    }

    /// 잔고 조회 → 보유 종목별 (평균가, 수량) 반환
    async fn fetch_balance(
        &self,
        client: &KisHttpClient,
    ) -> Result<HashMap<String, (i64, u64)>, String> {
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
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &crate::domain::types::TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await;

        match resp {
            Ok(r) => extract_balance_holdings(r),
            Err(e) => Err(format!("잔고 조회 에러: {e}")),
        }
    }

    /// 잔고 불일치 감지 후 재확인.
    ///
    /// 2026-04-17 P1 — 기존 "2초 sleep 후 1회 재조회"를 "interval 간격으로 attempts
    /// 회까지 재조회하되 첫 일치 시 즉시 종료" 로 강화. KIS `inquire-balance` 의
    /// 일시 지연(체결 41초 후에도 0주 응답된 사고 사례)을 흡수하면서도, 회복이
    /// 빠를 때는 mismatch 판정 지연을 만들지 않는다.
    ///
    /// 인자:
    /// - `expected_qty`: 러너 측 보유 수량. KIS 응답이 이 값과 같아지면 즉시 return.
    ///
    /// 반환: 마지막 attempt 의 KIS 응답 보유 수량. 모든 attempt 가 실패해도 마지막
    /// 값을 반환하므로 호출자(`reconcile_once`)는 이 값으로 mismatch 판정을 한다.
    async fn confirm_reconcile_qty(
        &self,
        client: &KisHttpClient,
        code: &str,
        expected_qty: u64,
    ) -> Result<u64, String> {
        let attempts = self.reconcile_confirm_attempts;
        let interval = std::time::Duration::from_secs(self.reconcile_confirm_interval_secs.max(2));
        confirm_reconcile_qty_with(expected_qty, attempts, interval, || async {
            let holdings = self.fetch_balance(client).await?;
            Ok(holdings.get(code).map(|(_, q)| *q).unwrap_or(0))
        })
        .await
    }

    async fn start_runner(&self, code: &str, name: &str) -> Result<(), String> {
        // 2026-04-16 Task 5 — 실전 opt-in 가드.
        // `KIS_ENABLE_REAL_TRADING` 이 꺼져 있으면서 실전 환경이면 main.rs 에서
        // block_starts_reason 을 `Some` 으로 설정. 이 경로에서 start 는 영구 거부.
        if let Some(reason) = self.block_starts_reason.read().await.clone() {
            return Err(format!("실전 주문 opt-in 미활성 — start 거부: {reason}"));
        }

        // 2026-04-16 Task 4 — 허용 종목 화이트리스트.
        // 비어있으면 생략 (기본 2종목 허용). 지정되어 있으면 목록 외 거부.
        {
            let allowed = self.allowed_codes.read().await;
            if !allowed.is_empty() && !allowed.iter().any(|c| c == code) {
                return Err(format!(
                    "허용 종목 목록(KIS_ALLOWED_CODES) 밖 — {code} 시작 거부. 현재 허용: {:?}",
                    *allowed
                ));
            }
        }

        // 2026-04-16 Go 조건 #2 — 전역 일일 거래 한도 이미 도달?
        // start 단계에서 선차단하여 불필요한 러너 생성/OR 조회를 막는다.
        {
            let cfg = self.live_runner_config.read().await;
            if let Some(ref gate) = cfg.global_trade_gate {
                let today = chrono::Local::now().date_naive();
                let (count, max) = gate.write().await.snapshot(today);
                if count >= max {
                    return Err(format!(
                        "전역 일일 거래 한도 이미 도달 ({}/{}) — 추가 시작 거부",
                        count, max
                    ));
                }
            }
        }

        // WS 체결통보 startup health gate 체크 (2026-04-15 Codex review #2).
        // Pending 동안은 기동 직후 race 를 막기 위해 거부한다.
        match &*self.notification_health.read().await {
            NotificationHealth::Pending => {
                return Err("체결통보 health check 진행 중 — 잠시 후 재시도".to_string());
            }
            NotificationHealth::Failed(reason) => {
                return Err(format!("체결통보 불가 — 거래 차단: {reason}"));
            }
            NotificationHealth::Ready => { /* 통과 */ }
        }

        // 수동 개입 대기 중인 종목은 재시작 거부 (2026-04-15 Codex review #4).
        // 숨은 live position 이 남아있을 수 있어 자동 운영을 재개하면 안 된다.
        if let Some(status) = self
            .statuses
            .read()
            .await
            .get(code)
            .filter(|s| s.manual_intervention_required)
        {
            return Err(format!(
                "수동 개입 필요 — HTS/MTS 에서 포지션 확인 후 서버 재시작 필요: {}",
                status
                    .degraded_reason
                    .clone()
                    .unwrap_or_else(|| "메타 부족".to_string())
            ));
        }

        // 이미 실행 중인 러너가 있으면 거부 (좀비 태스크 방지)
        if self.runners.read().await.contains_key(code) {
            return Err("이미 실행 중입니다".to_string());
        }

        let client_guard = self.client.read().await;
        let client = client_guard
            .as_ref()
            .ok_or("KIS 클라이언트가 초기화되지 않았습니다")?
            .clone();

        let stock_code = StockCode::new(code).map_err(|e| e.to_string())?;
        let stop_flag = Arc::new(AtomicBool::new(false));

        // 전액 투입: KIS 매수가능조회 API로 실제 주문가능수량 조회
        let quantity = {
            // 1) 현재가 조회
            let query = [("FID_COND_MRKT_DIV_CODE", "J"), ("FID_INPUT_ISCD", code)];
            let resp: KisResponse<InquirePrice> = client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-price",
                    &TransactionId::InquirePrice,
                    Some(&query),
                    None,
                )
                .await
                .map_err(|e| format!("현재가 조회 실패: {e}"))?;
            let price_data = resp
                .into_result()
                .map_err(|e| format!("현재가 파싱 실패: {e}"))?;
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
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/trading/inquire-psbl-order",
                    &TransactionId::InquireBuyable,
                    Some(&buyable_query),
                    None,
                )
                .await;

            let qty = match buyable_resp {
                Ok(r) => match r.into_result() {
                    Ok(info) => {
                        let api_qty = info.orderable_qty() as u64;
                        info!(
                            "{}: 매수가능조회 상세 — ord_psbl_cash={}, nrcvb_buy_amt={}, nrcvb_buy_qty={}, max_buy_amt={}, max_buy_qty={}",
                            name,
                            info.ord_psbl_cash,
                            info.nrcvb_buy_amt,
                            info.nrcvb_buy_qty,
                            info.max_buy_amt,
                            info.max_buy_qty
                        );
                        info!("{}: 현재가 {}원, 주문가능수량 {}주", name, price, api_qty);
                        api_qty
                    }
                    Err(e) => {
                        warn!(
                            "{}: 매수가능조회 파싱 실패 — 잔고 기반 fallback (80%): {e}",
                            name
                        );
                        ((self.get_available_cash(&client).await as f64 * 0.80) as i64 / price)
                            as u64
                    }
                },
                Err(e) => {
                    warn!(
                        "{}: 매수가능조회 실패 — 잔고 기반 fallback (80%): {e}",
                        name
                    );
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
        if let Some(ref el) = self.event_logger {
            runner = runner.with_event_logger(Arc::clone(el));
        }
        // 2026-04-16 Task 4/5: LiveRunnerConfig 주입 (실전 single_15m, max_daily_trades=1 등).
        let live_cfg_snapshot = self.live_runner_config.read().await.clone();
        runner = runner.with_live_config(live_cfg_snapshot);

        let stop_notify = runner.stop_notify();
        let runner_state = runner.state.clone();
        let code_str = code.to_string();
        let statuses = self.statuses.clone();
        let runners_cleanup = self.runners.clone();

        // 러너 핸들 저장
        self.runners.write().await.insert(
            code.to_string(),
            RunnerHandle {
                stop_flag: stop_flag.clone(),
                stop_notify,
                runner_state: runner_state.clone(),
            },
        );

        // 백그라운드 태스크로 러너 실행
        let code_cleanup = code.to_string();
        let db_for_report = self.db_store.clone();
        let runner_state_for_exit = runner_state.clone();
        tokio::spawn(async move {
            let mut runner = runner;
            let run_result = runner.run().await;

            // 러너 종료 직전의 최종 상태 스냅샷 — manual_intervention_required 여부 판단
            let (manual_intervention, degraded_reason) = {
                let rs = runner_state_for_exit.read().await;
                (rs.manual_intervention_required, rs.degraded_reason.clone())
            };

            match run_result {
                Ok(trades) => {
                    let pnl: f64 = trades.iter().map(|t| t.pnl_pct()).sum();
                    let mut s = statuses.write().await;
                    if let Some(status) = s.get_mut(&code_str) {
                        status.active = false;
                        status.today_trades = trades.len() as i32;
                        status.today_pnl = pnl;
                        if manual_intervention {
                            // 자동 재개 금지 — 운영자가 HTS 에서 수동 청산 후 명시적 재시작 필요.
                            status.state = "수동 개입 필요".to_string();
                            status.manual_intervention_required = true;
                            status.degraded = true;
                            status.degraded_reason = degraded_reason.clone();
                            status.message = format!(
                                "⚠ HTS 수동 확인 필요: {}",
                                degraded_reason.as_deref().unwrap_or("메타 부족")
                            );
                            error!("{}: 자동매매 중단 — 수동 개입 필요", code_str);
                        } else {
                            status.state = "종료".to_string();
                            status.message = format!("{}건 거래, {:.2}%", trades.len(), pnl);
                            info!(
                                "{}: 자동매매 종료 — {}건 거래, 손익 {:.2}%",
                                code_str,
                                trades.len(),
                                pnl
                            );
                        }
                    }
                }
                Err(e) => {
                    error!("{}: 자동매매 에러 — {e}", code_str);
                    let mut s = statuses.write().await;
                    if let Some(status) = s.get_mut(&code_str) {
                        status.active = false;
                        if manual_intervention {
                            status.state = "수동 개입 필요".to_string();
                            status.manual_intervention_required = true;
                            status.degraded = true;
                            status.degraded_reason = degraded_reason;
                            status.message = format!("⚠ HTS 수동 확인 필요 (에러: {e})");
                        } else {
                            status.state = "에러".to_string();
                            status.message = format!("에러: {e}");
                        }
                    }
                }
            }
            // 러너 종료 후 핸들 제거 (재시작 허용)
            runners_cleanup.write().await.remove(&code_cleanup);

            // 모든 러너 종료 시 일일 결산 리포트 자동 생성 (비동기, fire-and-forget)
            if runners_cleanup.read().await.is_empty()
                && let Some(store) = db_for_report
            {
                let today = chrono::Local::now().date_naive();
                let reporter =
                    crate::infrastructure::monitoring::daily_report::DailyReportGenerator::new(
                        store.pool().clone(),
                    );
                tokio::spawn(async move { reporter.generate(today).await });
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
                    // 루프 탈출: 러너가 멈췄고 수동 개입 상태도 아닐 때만.
                    // manual_intervention 시에는 운영자가 해제할 때까지 상태를 계속 노출.
                    if !status.active && !rs.manual_intervention_required {
                        break;
                    }
                    // 2026-04-15 Codex re-review: manual 상태라면 러너 쪽 phase("종료" 등)가
                    // state 를 덮어써 운영자에게 가장 보여줘야 할 "수동 개입 필요" 가 약화되는
                    // 회귀를 차단. manual 시엔 state 를 고정 문구로 노출한다.
                    if rs.manual_intervention_required {
                        status.state = "수동 개입 필요".to_string();
                    } else {
                        status.state = rs.phase.clone();
                    }
                    status.today_trades = rs.today_trades.len() as i32;
                    status.today_pnl = rs.today_pnl;
                    status.or_high = rs.or_high;
                    status.or_low = rs.or_low;
                    status.or_stages = rs.or_stages.clone();
                    status.degraded = rs.degraded;
                    status.manual_intervention_required = rs.manual_intervention_required;
                    status.degraded_reason = rs.degraded_reason.clone();
                    if rs.manual_intervention_required {
                        status.message = format!(
                            "⚠ 수동 개입 필요: {}",
                            rs.degraded_reason.as_deref().unwrap_or("메타 부족")
                        );
                    } else if rs.degraded {
                        status.message = format!(
                            "degraded: {}",
                            rs.degraded_reason
                                .as_deref()
                                .unwrap_or("시스템 신뢰성 미달")
                        );
                    } else if let Some(ref pos) = rs.current_position {
                        status.message =
                            format!("{:?} {}주 @ {}", pos.side, pos.quantity, pos.entry_price);
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

    /// 모든 러너에 중지 신호 전송 — 체결 대기 루프/신호 탐색 sleep 즉시 탈출.
    /// Graceful shutdown 경로에서 호출된다.
    pub async fn stop_all(&self) {
        let runners = self.runners.read().await;
        for (code, handle) in runners.iter() {
            info!("{}: graceful shutdown — stop 신호 전송", code);
            handle.stop_flag.store(true, Ordering::Relaxed);
            handle.stop_notify.notify_waiters();
        }
    }

    /// 주기적 잔고 reconciliation 태스크 기동.
    ///
    /// 2026-04-16 Task 3 — KIS 잔고와 러너 state 의 current_position.quantity 를
    /// 주기적으로 대조한다. 불일치(숨은 매수/매도)가 감지되면:
    /// 1. RunnerState 에 `manual_intervention_required=true` 설정
    /// 2. `balance_reconcile_mismatch` 이벤트를 critical 로 기록
    /// 3. `stop_flag` + `stop_notify` 로 러너 강제 중단
    ///
    /// 사고 재현: 2026-04-15 cancel/fill race 로 122630/114800 조합 총 186주 숨은
    /// 매수가 체결됐으나 DB/UI 에는 0건으로 표시돼 SL/TP 어느 것도 작동하지 못함.
    /// 본 루프는 이 괴리가 1분(`interval_secs` 기본값) 내에 감지되도록 한다.
    pub fn spawn_reconciliation(&self, interval_secs: u64) {
        let mgr = self.clone();
        tokio::spawn(async move {
            // 초기 유예 — 기동 직후 토큰/첫 잔고 조회가 혼재되는 구간 회피.
            tokio::time::sleep(std::time::Duration::from_secs(45)).await;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs.max(15))).await;
                mgr.reconcile_once().await;
            }
        });
    }

    /// 1회 잔고 reconciliation. 장중에만 수행.
    async fn reconcile_once(&self) {
        let now = chrono::Local::now().time();
        let market_start = chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let market_end = chrono::NaiveTime::from_hms_opt(15, 35, 0).unwrap();
        if now < market_start || now > market_end {
            return;
        }

        let active_count = self.runners.read().await.len();
        if active_count == 0 {
            return;
        }

        let client_opt = self.client.read().await.as_ref().cloned();
        let Some(client) = client_opt else {
            return;
        };

        let holdings = match self.fetch_balance(&client).await {
            Ok(h) => h,
            Err(e) => {
                warn!("[reconcile] 잔고 조회 실패 — skip: {e}");
                return;
            }
        };

        // 스냅샷: (code, runner_state, stop_flag, stop_notify)
        let snapshots: Vec<(
            String,
            Arc<RwLock<crate::strategy::live_runner::RunnerState>>,
            Arc<AtomicBool>,
            Arc<tokio::sync::Notify>,
        )> = {
            let runners = self.runners.read().await;
            runners
                .iter()
                .map(|(c, h)| {
                    (
                        c.clone(),
                        h.runner_state.clone(),
                        Arc::clone(&h.stop_flag),
                        Arc::clone(&h.stop_notify),
                    )
                })
                .collect()
        };

        for (code, rs, stop_flag, stop_notify) in snapshots {
            // manual 이미 전환된 러너는 반복 기록 피하기
            let (runner_qty, already_manual, post_flat_remaining_ms) = {
                let guard = rs.read().await;
                (
                    guard
                        .current_position
                        .as_ref()
                        .map(|p| p.quantity)
                        .unwrap_or(0),
                    guard.manual_intervention_required,
                    reconcile_post_flat_remaining_ms(&guard),
                )
            };
            if already_manual {
                continue;
            }
            if let Some(remaining_ms) = post_flat_remaining_ms {
                self.reset_reconcile_inconclusive_streak(&code).await;
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        &code,
                        "position",
                        "reconcile_skipped_post_flat_window",
                        "info",
                        "flat 직후 settle window 동안 reconcile 승격 유예",
                        serde_json::json!({
                            "runner_qty": runner_qty,
                            "remaining_ms": remaining_ms,
                            "window_secs": RECONCILE_POST_FLAT_SETTLE_WINDOW_SECS,
                        }),
                    );
                }
                continue;
            }
            // 2026-04-17 v3 불변식 #3: Pending(self) / ManualHeld(self) 이면 skip.
            // 지정가 발주 ~ 체결 확인 구간(~30 초)의 과도 상태에서 reconcile 이
            // runner_qty=0, KIS_qty>0 을 hidden position 으로 오판하는 것을 차단.
            // 2026-04-17 11:00 사고 원인.
            if should_skip_reconcile_for_lock(&*self.active_position_lock.read().await, &code) {
                tracing::debug!("[reconcile] {} lock 상태로 skip (Pending/ManualHeld)", code);
                continue;
            }
            let initial_kis_qty: u64 = holdings.get(&code).map(|(_, q)| *q).unwrap_or(0);
            if matches!(
                classify_reconcile_outcome(runner_qty, initial_kis_qty, Err("skipped"), 0),
                ReconcileDecision::AlreadyMatched
            ) {
                // 2026-04-17 v3 fixups D: 1차 fetch 가 이미 일치 — 정상 cycle.
                // streak 가 진짜 "연속 inconclusive" 의미가 되도록 reset.
                self.reset_reconcile_inconclusive_streak(&code).await;
                continue;
            }

            let confirm_result = self.confirm_reconcile_qty(&client, &code, runner_qty).await;
            let streak_after_bump = match &confirm_result {
                Ok(_) => {
                    // 2026-04-17 v3 F1.1+: 재확인 fetch 가 한 번이라도 성공하면
                    // 누적 inconclusive streak 를 reset.
                    self.reset_reconcile_inconclusive_streak(&code).await;
                    0
                }
                Err(_) => {
                    // 2026-04-17 v3 F1.1: 재확인 모든 attempt 가 fetch error 면 inconclusive.
                    // 기존 코드는 1차 stale 샘플로 fallback → false manual stop 위험.
                    // v3 는 manual 전환을 보류하고 다음 cycle (1분 후) 재시도. P0 가
                    // 손실 경로는 차단 중이므로 잠시 mismatch 미발견 비용 < false stop 비용.
                    self.bump_reconcile_inconclusive_streak(&code).await
                }
            };
            match classify_reconcile_outcome(
                runner_qty,
                initial_kis_qty,
                confirm_result.clone(),
                streak_after_bump,
            ) {
                ReconcileDecision::AlreadyMatched => {
                    self.reset_reconcile_inconclusive_streak(&code).await;
                    continue;
                }
                ReconcileDecision::Recovered { confirmed_qty } => {
                    warn!(
                        "[reconcile] {} 불일치 자동 복구 — runner={}주, KIS {}→{}주",
                        code, runner_qty, initial_kis_qty, confirmed_qty
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            &code,
                            "position",
                            "balance_reconcile_recovered",
                            "warn",
                            &format!(
                                "reconcile 재확인에서 일치 회복 — runner={}주, KIS {}→{}주",
                                runner_qty, initial_kis_qty, confirmed_qty
                            ),
                            serde_json::json!({
                                "runner_qty": runner_qty,
                                "kis_qty_first": initial_kis_qty,
                                "kis_qty_confirmed": confirmed_qty,
                            }),
                        );
                    }
                    continue;
                }
                ReconcileDecision::Mismatch { confirmed_qty } => {
                    let reason = format!(
                        "reconcile 불일치: runner={}주, KIS={}주 (1차 {}주) — 숨은 포지션 감지",
                        runner_qty, confirmed_qty, initial_kis_qty
                    );
                    error!("{}: {}", code, reason);

                    {
                        let mut s = self.statuses.write().await;
                        if let Some(status) = s.get_mut(&code) {
                            status.manual_intervention_required = true;
                            status.degraded = true;
                            status.degraded_reason = Some(reason.clone());
                            status.state = "수동 개입 필요".to_string();
                            status.message = format!("⚠ {}", reason);
                        }
                    }

                    crate::strategy::live_runner::enter_manual_intervention_state(
                        &rs,
                        Some(&self.active_position_lock),
                        &stop_flag,
                        &stop_notify,
                        self.event_logger.as_ref(),
                        &code,
                        "balance_reconcile_mismatch",
                        reason,
                        serde_json::json!({
                            "runner_qty": runner_qty,
                            "kis_qty": confirmed_qty,
                            "kis_qty_first": initial_kis_qty,
                        }),
                    )
                    .await;
                    self.persist_manual_flag_or_log(&code).await;
                }
                ReconcileDecision::Inconclusive { streak } => {
                    let err = confirm_result
                        .as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    warn!(
                        "[reconcile] {} 재확인 모든 attempt 실패 — inconclusive({}회), 다음 cycle 재시도 (runner={}주, KIS 1차={}주): {}",
                        code, streak, runner_qty, initial_kis_qty, err
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            &code,
                            "position",
                            "balance_reconcile_inconclusive",
                            "warn",
                            "재확인 모든 attempt 실패 — manual 전환 보류, 다음 cycle 재시도",
                            serde_json::json!({
                                "runner_qty": runner_qty,
                                "kis_qty_first": initial_kis_qty,
                                "streak": streak,
                                "error": err,
                            }),
                        );
                    }
                    continue;
                }
                ReconcileDecision::InconclusiveEscalated { streak } => {
                    let err = confirm_result
                        .as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    let reason = format!(
                        "reconcile 재확인 {}회 연속 전부 실패 — 상태 불명, 수동 개입 필요",
                        streak
                    );
                    error!("{}: {}", code, reason);
                    {
                        let mut s = self.statuses.write().await;
                        if let Some(status) = s.get_mut(&code) {
                            status.manual_intervention_required = true;
                            status.degraded = true;
                            status.degraded_reason = Some(reason.clone());
                            status.state = "수동 개입 필요".to_string();
                            status.message = format!("⚠ {}", reason);
                        }
                    }
                    self.reset_reconcile_inconclusive_streak(&code).await;
                    crate::strategy::live_runner::enter_manual_intervention_state(
                        &rs,
                        Some(&self.active_position_lock),
                        &stop_flag,
                        &stop_notify,
                        self.event_logger.as_ref(),
                        &code,
                        "balance_reconcile_inconclusive_escalated",
                        reason,
                        serde_json::json!({
                            "runner_qty": runner_qty,
                            "kis_qty_first": initial_kis_qty,
                            "streak": streak,
                            "error": err,
                        }),
                    )
                    .await;
                    self.persist_manual_flag_or_log(&code).await;
                    continue;
                }
            }
        }
    }

    /// 2026-04-17 긴급 follow-up: active_positions.manual_intervention_required
    /// 플래그를 DB 에 저장. 실패 시 critical 이벤트만 기록 (저장 실패로 manual
    /// 차단 자체를 풀면 안 됨 — 재시작 전까지는 메모리 state 로 보호).
    async fn persist_manual_flag_or_log(&self, code: &str) {
        let Some(ref store) = self.db_store else {
            return;
        };
        match store.set_active_position_manual(code, true).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!(
                    "{}: manual 플래그 DB 저장 실패 — 재시작 시 보호 우회 위험: {e}",
                    code
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        code,
                        "storage",
                        "manual_flag_persist_failed",
                        "critical",
                        &format!("manual_intervention_required DB 저장 실패: {e}"),
                        serde_json::json!({"error": e.to_string()}),
                    );
                }
            }
        }
    }

    /// 모든 러너가 종료될 때까지 대기 (러너가 runners 맵에서 제거되는 것 기준).
    /// 타임아웃 도달 시 경고 후 반환.
    pub async fn wait_all_stopped(&self, timeout: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining_count = self.runners.read().await.len();
            if remaining_count == 0 {
                info!("graceful shutdown — 모든 러너 정상 종료");
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    "graceful shutdown timeout — {}개 러너 종료 대기 포기",
                    remaining_count
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

/// 2026-04-17 v3 불변식 #3: reconcile 이 skip 해야 하는 lock 상태 판정.
///
/// 지정가 발주 후 체결 확인 전 ~30 초 과도 상태 (`Pending(self)`) 동안
/// reconcile 이 runner_qty=0, KIS_qty>0 을 hidden position 으로 오판하는
/// 2026-04-17 11:00 사고 경로를 차단한다.
///
/// 이미 `ManualHeld(self)` 면 별도 already_manual 체크와 중복 방어.
///
/// reconcile_once 가 **실제로 호출하는 helper** — dead helper 확장 금지.
#[inline]
pub(crate) fn should_skip_reconcile_for_lock(lock_state: &PositionLockState, code: &str) -> bool {
    matches!(lock_state,
        PositionLockState::Pending { code: c, .. } if c == code
    ) || matches!(lock_state,
        PositionLockState::ManualHeld(c) if c == code
    )
}

/// 청산 직후 flat 전이와 KIS 잔고 반영 사이의 짧은 race window.
///
/// 오늘 사고처럼 `execution_state=flat` 으로 먼저 전이되고 `current_position` 정리와
/// 잔고 API 반영이 약간 뒤따르는 경우가 있어, 이 짧은 구간의 mismatch 는 즉시
/// manual 승격하지 않고 settle 시간을 준다.
#[inline]
pub(crate) fn reconcile_post_flat_remaining_ms(
    state: &crate::strategy::live_runner::RunnerState,
) -> Option<u64> {
    if state.execution_state != crate::strategy::live_runner::ExecutionState::Flat {
        return None;
    }
    let transitioned_at = state.last_flat_transition_at?;
    let window = std::time::Duration::from_secs(RECONCILE_POST_FLAT_SETTLE_WINDOW_SECS);
    let elapsed = transitioned_at.elapsed();
    if elapsed >= window {
        None
    } else {
        Some((window - elapsed).as_millis() as u64)
    }
}

/// 2026-04-17 v3 fixups K: reconcile cycle 결정 분류.
///
/// `reconcile_once` 의 흐름을 helper 로 추출해 단위테스트로 정책 회귀를 감시한다.
/// helper 는 순수 결정만 담당하고 사이드 이펙트(streak reset / event 기록 /
/// manual 전환) 는 호출자가 처리한다.
///
/// 분기:
/// - `runner_qty == initial_kis_qty` → `AlreadyMatched` (빠른 스킵)
/// - confirm `Ok(qty)` + `qty == runner_qty` → `Recovered` (회복)
/// - confirm `Ok(qty)` + `qty != runner_qty` → `Mismatch` (manual 승격 필요)
/// - confirm `Err(_)` + `streak < 3` → `Inconclusive`
/// - confirm `Err(_)` + `streak >= 3` → `InconclusiveEscalated` (manual 승격 필요)
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReconcileDecision {
    AlreadyMatched,
    Recovered { confirmed_qty: u64 },
    Mismatch { confirmed_qty: u64 },
    Inconclusive { streak: u32 },
    InconclusiveEscalated { streak: u32 },
}

#[inline]
pub(crate) fn classify_reconcile_outcome<E>(
    runner_qty: u64,
    initial_kis_qty: u64,
    confirm_result: Result<u64, E>,
    streak_after_bump: u32,
) -> ReconcileDecision {
    if runner_qty == initial_kis_qty {
        return ReconcileDecision::AlreadyMatched;
    }
    match confirm_result {
        Ok(qty) if qty == runner_qty => ReconcileDecision::Recovered { confirmed_qty: qty },
        Ok(qty) => ReconcileDecision::Mismatch { confirmed_qty: qty },
        Err(_) => {
            if streak_after_bump >= 3 {
                ReconcileDecision::InconclusiveEscalated {
                    streak: streak_after_bump,
                }
            } else {
                ReconcileDecision::Inconclusive {
                    streak: streak_after_bump,
                }
            }
        }
    }
}

/// 2026-04-17 P1 + v3 F1: reconcile 재확인 루프 (free function).
///
/// 동작:
/// - 매 attempt 마다 fetch 시도
/// - 성공 + `expected_qty` 일치 → 즉시 `Ok(qty)` (회복 즉시 종료)
/// - 성공 + mismatch → `last_successful_qty` 누적, 다음 attempt 진행
/// - 실패(Err) → 에러 누적, 다음 attempt 진행 (즉시 propagate 안 함)
/// - 모든 attempt 종료 후:
///   - `last_successful_qty = Some(q)` → `Ok(q)` (마지막 성공값으로 mismatch 판정)
///   - `last_successful_qty = None` (전부 fetch 실패) → `Err(...)`
///
/// 설계 의도 (v3 F1):
/// - "한 번이라도 성공했으면 그 값으로 판단, 전부 실패면 호출자가 inconclusive 처리"
/// - false manual stop 방지 — KIS API 일시 5xx/timeout 으로 단발 fetch 실패 시
///   기존 동작은 즉시 Err propagate → 호출자가 1차 stale 샘플로 mismatch 판정 →
///   manual 전환 발동했음. v3 는 같은 cycle 내 재시도로 흡수.
///
/// `attempts < 1` 이면 1로 보정. `interval` 은 attempt 사이 대기 (첫 시도 전 sleep 안 함).
pub(crate) async fn confirm_reconcile_qty_with<F, Fut>(
    expected_qty: u64,
    attempts: u32,
    interval: std::time::Duration,
    mut fetch: F,
) -> Result<u64, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<u64, String>>,
{
    let attempts = attempts.max(1);
    let mut last_successful_qty: Option<u64> = None;
    let mut errors: Vec<String> = Vec::new();
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        match fetch().await {
            Ok(qty) => {
                last_successful_qty = Some(qty);
                if qty == expected_qty {
                    return Ok(qty); // 회복 즉시 종료
                }
            }
            Err(e) => {
                errors.push(format!("attempt {}: {e}", attempt + 1));
            }
        }
    }
    match last_successful_qty {
        Some(q) => Ok(q), // 일부 성공 + 모두 mismatch → 마지막 성공값으로 mismatch 판정
        None => Err(format!(
            "재확인 모든 attempt 실패 ({}건): {}",
            errors.len(),
            errors.join("; ")
        )),
    }
}

fn extract_balance_holdings(
    resp: KisResponse<Vec<serde_json::Value>>,
) -> Result<HashMap<String, (i64, u64)>, String> {
    if resp.rt_cd != "0" {
        return Err(format!("잔고 조회 실패: {}", resp.msg1));
    }

    let items = resp.output.or(resp.output1).unwrap_or_default();
    let mut map = HashMap::new();
    for item in &items {
        let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
        let qty_str = item.get("hldg_qty").and_then(|v| v.as_str()).unwrap_or("0");
        let avg_str = item
            .get("pchs_avg_pric")
            .and_then(|v| v.as_str())
            .unwrap_or("0");
        let qty: u64 = qty_str.parse().unwrap_or(0);
        let avg: f64 = avg_str.parse().unwrap_or(0.0);
        if qty > 0 && !code.is_empty() {
            map.insert(code.to_string(), (avg as i64, qty));
        }
    }
    Ok(map)
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

    // aggregator로부터 실효 출처 + OR 교체 실패 상태 덮어쓰기
    if let Some(ref agg) = state.strategy_manager.candle_aggregator {
        let failures = agg.or_refresh_failures().await;
        for status in list.iter_mut() {
            status.or_source = agg.effective_source(&status.code).await;
            status.or_refresh_warnings = failures.clone();
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
        code: req.code,
        name: String::new(),
        active: false,
        state: "오류".to_string(),
        today_trades: 0,
        today_pnl: 0.0,
        message: "종목 없음".to_string(),
        or_high: None,
        or_low: None,
        or_stages: Vec::new(),
        or_source: None,
        or_refresh_warnings: Vec::new(),
        params: StrategyParams {
            rr_ratio: cfg.rr_ratio,
            trailing_r: cfg.trailing_r,
            breakeven_r: cfg.breakeven_r,
            max_daily_trades: cfg.max_daily_trades,
            long_only: cfg.long_only,
        },
        degraded: false,
        manual_intervention_required: false,
        degraded_reason: None,
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
            code: req.code,
            name: String::new(),
            active: false,
            state: "오류".to_string(),
            today_trades: 0,
            today_pnl: 0.0,
            message: "종목 없음".to_string(),
            or_high: None,
            or_low: None,
            or_stages: Vec::new(),
            or_source: None,
            or_refresh_warnings: Vec::new(),
            params: StrategyParams {
                rr_ratio: cfg.rr_ratio,
                trailing_r: cfg.trailing_r,
                breakeven_r: cfg.breakeven_r,
                max_daily_trades: cfg.max_daily_trades,
                long_only: cfg.long_only,
            },
            degraded: false,
            manual_intervention_required: false,
            degraded_reason: None,
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
         ORDER BY id DESC LIMIT 50",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Json(rows)
}

/// 당일 FVG 요약 (웹 패널용).
/// `event_log` 의 `entry_signal` / `abort_entry` / `drift_rejected` 이벤트를 집계하여
/// 각 FVG(= unique `b_time` 기준) 의 최종 상태와 FVG 품질 메타데이터를 반환한다.
#[derive(Debug, Clone, Serialize)]
pub struct FvgSummary {
    pub stock_code: String,
    pub stage: String,
    pub side: String,
    pub b_time: String,
    pub signal_time: String,
    pub gap_top: i64,
    pub gap_bottom: i64,
    pub gap_size_pct: f64,
    pub b_body_ratio: f64,
    pub or_breakout_pct: f64,
    pub b_volume: i64,
    /// 진입 가격 (지정가). Long=gap.top, Short=gap.bottom.
    pub entry_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    /// 최종 상태: pending / aborted / drift_rejected / filled
    pub state: String,
    /// abort_entry 의 reason (fill_timeout_or_cutoff / drift_exceeded / preempted / price_fetch_failed / vi_halted / api_error_retry_failed)
    pub reason: Option<String>,
    /// drift_rejected 이벤트의 drift %
    pub drift_pct: Option<f64>,
    /// 신호 시점 현재가 (drift 가드 활성 시 기록됨)
    pub current_price_at_signal: Option<i64>,
    /// 이벤트 수신 수 (동일 FVG 가 몇 번 abort 시도됐는지)
    pub event_count: i64,
    /// 가장 최근 이벤트 시각
    pub latest_event_at: chrono::DateTime<chrono::Local>,
}

/// GET /api/v1/strategy/fvgs?date=YYYY-MM-DD — 당일(또는 지정일) FVG 요약 목록
async fn get_fvgs(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<FvgSummary>> {
    let Some(ref pool) = state.db_pool else {
        return Json(Vec::new());
    };

    // date 파라미터 (기본: 오늘)
    let date_filter = params
        .get("date")
        .cloned()
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());

    // event_log 에서 FVG 관련 이벤트를 b_time 으로 그룹화.
    // 동일 FVG(b_time) 에 대해 여러 이벤트(entry_signal 반복, drift_rejected, abort_entry)가 있을 수 있음.
    // 최신 이벤트의 metadata 를 대표로 사용하되 상태는 우선순위로 결정.
    // 상태 우선순위: drift_rejected > aborted(abort_entry 의 fill_timeout 등) > pending(entry_signal 만)
    let rows: Vec<(
        String,                          // stock_code
        String,                          // event_type
        String,                          // severity
        serde_json::Value,               // metadata
        chrono::DateTime<chrono::Local>, // event_time
    )> = sqlx::query_as(
        "SELECT stock_code, event_type, severity, metadata, event_time
         FROM event_log
         WHERE event_time::date = $1::date
           AND event_type IN ('entry_signal', 'abort_entry', 'drift_rejected')
         ORDER BY event_time ASC",
    )
    .bind(&date_filter)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // (stock_code, b_time) 키로 그룹화
    let mut groups: std::collections::HashMap<
        (String, String),
        Vec<(
            String,
            String,
            serde_json::Value,
            chrono::DateTime<chrono::Local>,
        )>,
    > = std::collections::HashMap::new();

    for (stock_code, event_type, severity, metadata, event_time) in rows {
        let b_time = metadata
            .get("b_time")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if b_time.is_empty() {
            continue;
        }
        groups
            .entry((stock_code.clone(), b_time))
            .or_default()
            .push((event_type, severity, metadata, event_time));
    }

    // 그룹별로 FvgSummary 생성
    let mut summaries: Vec<FvgSummary> = Vec::new();
    for ((stock_code, _), events) in groups.iter() {
        // 마지막 이벤트의 metadata 를 기준으로 필드 채움 (signal_time 등은 동일)
        let Some((_, _, latest_meta, latest_time)) = events.last() else {
            continue;
        };

        // 상태 결정 (우선순위: drift_rejected > abort_entry > entry_signal)
        let has_drift = events.iter().any(|(et, _, _, _)| et == "drift_rejected");
        let has_abort = events.iter().any(|(et, _, _, _)| et == "abort_entry");

        let state = if has_drift {
            "drift_rejected".to_string()
        } else if has_abort {
            "aborted".to_string()
        } else {
            "pending".to_string()
        };

        // abort_entry 의 reason (마지막 abort 기준)
        let reason = events
            .iter()
            .rev()
            .find(|(et, _, _, _)| et == "abort_entry")
            .and_then(|(_, _, m, _)| m.get("reason").and_then(|r| r.as_str()).map(String::from));

        // drift_pct (drift_rejected 이벤트가 있으면 그 metadata 사용)
        let drift_pct = events
            .iter()
            .rev()
            .find(|(et, _, _, _)| et == "drift_rejected")
            .and_then(|(_, _, m, _)| m.get("drift").and_then(|v| v.as_f64()))
            .map(|v| v * 100.0);

        let as_i64 = |k: &str| latest_meta.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let as_f64 = |k: &str| latest_meta.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let as_str = |k: &str| {
            latest_meta
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let as_opt_i64 = |k: &str| latest_meta.get(k).and_then(|v| v.as_i64());

        summaries.push(FvgSummary {
            stock_code: stock_code.clone(),
            stage: as_str("stage"),
            side: as_str("side"),
            b_time: as_str("b_time"),
            signal_time: as_str("signal_time"),
            gap_top: as_i64("gap_top"),
            gap_bottom: as_i64("gap_bottom"),
            gap_size_pct: as_f64("gap_size_pct") * 100.0,
            b_body_ratio: as_f64("b_body_ratio"),
            or_breakout_pct: as_f64("or_breakout_pct") * 100.0,
            b_volume: as_i64("b_volume"),
            entry_price: as_i64("entry"),
            stop_loss: as_i64("sl"),
            take_profit: as_i64("tp"),
            state,
            reason,
            drift_pct,
            current_price_at_signal: as_opt_i64("current_price"),
            event_count: events.len() as i64,
            latest_event_at: *latest_time,
        });
    }

    // 최신 이벤트 기준 내림차순
    summaries.sort_by(|a, b| b.latest_event_at.cmp(&a.latest_event_at));
    Json(summaries)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/strategy/status", get(get_status))
        .route("/api/v1/strategy/start", post(start_strategy))
        .route("/api/v1/strategy/stop", post(stop_strategy))
        .route("/api/v1/strategy/trades", get(get_trades))
        .route("/api/v1/strategy/fvgs", get(get_fvgs))
        .route(
            "/api/v1/strategy/refresh-or-backfill",
            post(refresh_or_backfill),
        )
}

/// 2026-04-15 Codex re-review #2 대응 테스트.
///
/// `NotificationHealth` 의 상태 전이와 `start_runner` 거부 규칙이 회귀하지 않도록
/// 고정한다. 실제 KisHttpClient 주입 없이 Pending/Failed 거부 경로와 Pending race
/// 러너 강제 중단을 검증할 수 있다 (start_runner 는 client 검사보다 먼저
/// notification_health 를 체크하도록 구성돼 있음).
#[cfg(test)]
mod notification_health_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn pending_initial_state_rejects_start_runner() {
        let mgr = StrategyManager::new();
        // 생성 직후엔 Pending — 수동 start 거부.
        let result = mgr.start_runner("122630", "KODEX 레버리지").await;
        assert!(
            result.is_err(),
            "Pending 상태에서는 start_runner 가 거부돼야 함"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("health check 진행 중"),
            "에러 메시지에 Pending 안내가 포함돼야 함: {err}"
        );
    }

    #[tokio::test]
    async fn failed_state_rejects_start_runner_permanently() {
        let mgr = StrategyManager::new();
        mgr.mark_notification_startup_failed("test 실패".to_string())
            .await;
        let result = mgr.start_runner("122630", "KODEX 레버리지").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("체결통보 불가") && err.contains("test 실패"),
            "Failed 사유가 에러에 포함돼야 함: {err}"
        );
    }

    #[tokio::test]
    async fn ready_state_passes_health_gate() {
        let mgr = StrategyManager::new();
        mgr.mark_notification_ready().await;
        // Ready 이면 health gate 는 통과. 이후 client 미설정 → 다른 사유의 Err 나옴.
        let result = mgr.start_runner("122630", "KODEX 레버리지").await;
        assert!(result.is_err(), "client 미설정으로 Err 는 나와야 함");
        let err = result.unwrap_err();
        assert!(
            !err.contains("health check"),
            "Ready 이후엔 health 경로로 거부되면 안 됨: {err}"
        );
        assert!(
            !err.contains("체결통보 불가"),
            "Ready 이후엔 Failed 문구도 안 나와야 함: {err}"
        );
    }

    #[tokio::test]
    async fn mark_notification_startup_failed_stops_pending_race_runner() {
        let mgr = StrategyManager::new();
        // Pending race 시나리오: gate 판정 전에 start 가 통과해 runners 에 핸들이 삽입된 상태.
        // 테스트에서는 직접 runners 맵에 dummy 핸들을 넣는다 (러너 task 는 띄우지 않음).
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(tokio::sync::Notify::new());
        let runner_state = Arc::new(RwLock::new(crate::strategy::live_runner::RunnerState {
            phase: "시작됨".to_string(),
            today_trades: Vec::new(),
            today_pnl: 0.0,
            current_position: None,
            or_high: None,
            or_low: None,
            or_stages: Vec::new(),
            market_halted: false,
            degraded: false,
            manual_intervention_required: false,
            degraded_reason: None,
            exit_pending: false,
            execution_state: crate::strategy::live_runner::ExecutionState::Flat,
            preflight_metadata: crate::strategy::live_runner::PreflightMetadata::default(),
            last_external_gate_stale: false,
            last_flat_transition_at: None,
            last_quote_sanity_issue: None,
            armed_stats: crate::strategy::live_runner::ArmedWatchStats::default(),
            current_armed_signal_id: None,
        }));
        mgr.runners.write().await.insert(
            "122630".to_string(),
            RunnerHandle {
                stop_flag: Arc::clone(&stop_flag),
                stop_notify: Arc::clone(&stop_notify),
                runner_state,
            },
        );
        assert!(!stop_flag.load(Ordering::Relaxed));

        mgr.mark_notification_startup_failed("KIS_HTS_ID 미설정".to_string())
            .await;

        assert!(
            stop_flag.load(Ordering::Relaxed),
            "Pending race 러너의 stop_flag 가 세워져야 함"
        );
    }

    #[tokio::test]
    async fn manual_intervention_required_blocks_restart_even_when_ready() {
        let mgr = StrategyManager::new();
        mgr.mark_notification_ready().await;
        // 동일 종목의 status 에 manual_intervention_required=true 를 선세팅.
        {
            let mut s = mgr.statuses.write().await;
            let status = s.get_mut("122630").expect("default 종목이 있어야 함");
            status.manual_intervention_required = true;
            status.degraded = true;
            status.degraded_reason = Some("테스트: 잔고 감지됨".to_string());
        }
        let result = mgr.start_runner("122630", "KODEX 레버리지").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("수동 개입 필요"),
            "manual_intervention 거부 메시지가 나와야 함: {err}"
        );
    }

    #[test]
    fn extract_balance_holdings_uses_output1_when_output_is_empty() {
        let resp = KisResponse {
            rt_cd: "0".to_string(),
            msg_cd: String::new(),
            msg1: String::new(),
            output: None,
            output1: Some(vec![json!({
                "pdno": "122630",
                "hldg_qty": "95",
                "pchs_avg_pric": "101580"
            })]),
            output2: None,
            tr_cont: None,
        };

        let holdings = extract_balance_holdings(resp).expect("output1도 잔고로 해석돼야 함");
        assert_eq!(holdings.get("122630"), Some(&(101580, 95)));
    }

    #[test]
    fn extract_balance_holdings_skips_zero_qty_rows() {
        let resp = KisResponse {
            rt_cd: "0".to_string(),
            msg_cd: String::new(),
            msg1: String::new(),
            output: Some(vec![
                json!({
                    "pdno": "122630",
                    "hldg_qty": "0",
                    "pchs_avg_pric": "101580"
                }),
                json!({
                    "pdno": "114800",
                    "hldg_qty": "12",
                    "pchs_avg_pric": "1448"
                }),
            ]),
            output1: None,
            output2: None,
            tr_cont: None,
        };

        let holdings = extract_balance_holdings(resp).expect("정상 응답이어야 함");
        assert_eq!(holdings.len(), 1);
        assert_eq!(holdings.get("114800"), Some(&(1448, 12)));
        assert!(!holdings.contains_key("122630"));
    }
}

/// 2026-04-17 P1: `confirm_reconcile_qty_with` 의 첫 일치 조기 종료 / 모든 시도
/// 불일치 / attempts 하한 등 결정 로직 회귀 테스트.
#[cfg(test)]
mod confirm_reconcile_loop_tests {
    use super::confirm_reconcile_qty_with;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn responder(
        seq: Vec<u64>,
    ) -> (
        Arc<Mutex<u32>>,
        impl FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, String>> + Send>>,
    ) {
        let calls = Arc::new(Mutex::new(0u32));
        let queue = Arc::new(Mutex::new(seq));
        let calls_c = Arc::clone(&calls);
        let fetch = move || {
            let calls = Arc::clone(&calls_c);
            let queue = Arc::clone(&queue);
            Box::pin(async move {
                *calls.lock().unwrap() += 1;
                let mut q = queue.lock().unwrap();
                if q.is_empty() {
                    Ok(0u64)
                } else {
                    Ok(q.remove(0))
                }
            })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, String>> + Send>>
        };
        (calls, fetch)
    }

    #[tokio::test]
    async fn first_match_returns_immediately() {
        let (calls, fetch) = responder(vec![95, 0, 0]);
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert_eq!(result.unwrap(), 95);
        assert_eq!(*calls.lock().unwrap(), 1, "1차에서 일치하면 즉시 종료");
    }

    #[tokio::test]
    async fn second_match_returns_after_two_calls() {
        let (calls, fetch) = responder(vec![0, 95, 0]);
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert_eq!(result.unwrap(), 95);
        assert_eq!(*calls.lock().unwrap(), 2, "2차에서 일치하면 그 시점 종료");
    }

    #[tokio::test]
    async fn all_mismatch_returns_last_value() {
        let (calls, fetch) = responder(vec![0, 0, 42]);
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert_eq!(result.unwrap(), 42, "모든 시도 불일치면 마지막 값 반환");
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn attempts_zero_clamped_to_one() {
        let (calls, fetch) = responder(vec![0]);
        let result = confirm_reconcile_qty_with(95, 0, Duration::ZERO, fetch).await;
        assert_eq!(result.unwrap(), 0);
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "attempts<1 이어도 최소 1회는 시도"
        );
    }

    /// 2026-04-17 v3 F1 변경 후: fetch error 가 더 이상 즉시 propagate 하지 않는다.
    /// 모든 attempt 가 실패해야 Err. attempt 별 메시지가 누적되어 최종 Err 에 포함.
    #[tokio::test]
    async fn all_errors_returns_err_with_all_attempts_labeled() {
        let calls = Arc::new(Mutex::new(0u32));
        let calls_c = Arc::clone(&calls);
        let fetch = move || {
            let calls = Arc::clone(&calls_c);
            async move {
                *calls.lock().unwrap() += 1;
                Err::<u64, String>("KIS 응답 오류".to_string())
            }
        };
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("attempt 1"), "attempt 1 라벨 포함: {err}");
        assert!(
            err.contains("attempt 3"),
            "attempt 3 라벨 포함 (모든 시도 누적): {err}"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            3,
            "v3: error 시 즉시 종료 안 함 — 끝까지 시도"
        );
    }

    /// 2026-04-17 v3 F4-T2: error → success(일치) → 즉시 Ok, 호출 2회.
    /// false manual stop 방지 — 1차 fetch error 가 후속 회복을 막지 않아야 한다.
    #[tokio::test]
    async fn error_then_success_match_returns_immediately() {
        let calls = Arc::new(Mutex::new(0u32));
        let calls_c = Arc::clone(&calls);
        let attempt = Arc::new(Mutex::new(0u32));
        let attempt_c = Arc::clone(&attempt);
        let fetch = move || {
            let calls = Arc::clone(&calls_c);
            let attempt = Arc::clone(&attempt_c);
            async move {
                *calls.lock().unwrap() += 1;
                let n = {
                    let mut a = attempt.lock().unwrap();
                    *a += 1;
                    *a
                };
                if n == 1 {
                    Err::<u64, String>("일시 5xx".to_string())
                } else {
                    Ok(95)
                }
            }
        };
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert_eq!(result.unwrap(), 95);
        assert_eq!(
            *calls.lock().unwrap(),
            2,
            "error → success 일치 → 즉시 종료 (2회)"
        );
    }

    /// 2026-04-17 v3 F4-T2: error → mismatch → mismatch → Ok(last_qty), 호출 3회.
    /// 일부 fetch 만 성공해도 그 마지막 값으로 mismatch 판정 (호출자가 manual 발동).
    #[tokio::test]
    async fn error_then_mismatch_returns_last_successful_qty() {
        let calls = Arc::new(Mutex::new(0u32));
        let calls_c = Arc::clone(&calls);
        let attempt = Arc::new(Mutex::new(0u32));
        let attempt_c = Arc::clone(&attempt);
        let fetch = move || {
            let calls = Arc::clone(&calls_c);
            let attempt = Arc::clone(&attempt_c);
            async move {
                *calls.lock().unwrap() += 1;
                let n = {
                    let mut a = attempt.lock().unwrap();
                    *a += 1;
                    *a
                };
                match n {
                    1 => Err::<u64, String>("network".to_string()),
                    2 => Ok(0),
                    _ => Ok(42), // 마지막 성공 값
                }
            }
        };
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert_eq!(
            result.unwrap(),
            42,
            "마지막 성공 fetch 값 반환 (95 와 불일치 → 호출자가 mismatch 판정)"
        );
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    /// 2026-04-17 v3 F4-T2: success(불일치) → success(일치) → 즉시 Ok, 호출 2회.
    /// 기존 second_match 회귀 + early return 보장 검증.
    #[tokio::test]
    async fn success_mismatch_then_match_returns_immediately() {
        let calls = Arc::new(Mutex::new(0u32));
        let calls_c = Arc::clone(&calls);
        let attempt = Arc::new(Mutex::new(0u32));
        let attempt_c = Arc::clone(&attempt);
        let fetch = move || {
            let calls = Arc::clone(&calls_c);
            let attempt = Arc::clone(&attempt_c);
            async move {
                *calls.lock().unwrap() += 1;
                let n = {
                    let mut a = attempt.lock().unwrap();
                    *a += 1;
                    *a
                };
                if n == 1 { Ok::<u64, String>(0) } else { Ok(95) }
            }
        };
        let result = confirm_reconcile_qty_with(95, 3, Duration::ZERO, fetch).await;
        assert_eq!(result.unwrap(), 95);
        assert_eq!(*calls.lock().unwrap(), 2);
    }
}

/// 2026-04-17 v3 fixups K: `classify_reconcile_outcome` 결정 분류 회귀 테스트.
/// reconcile_once 의 분기 정책이 helper 와 일치하도록 코드 리뷰 가이드.
#[cfg(test)]
mod reconcile_decision_tests {
    use super::{ReconcileDecision, classify_reconcile_outcome};

    #[test]
    fn runner_matches_initial_returns_already_matched() {
        // confirm 결과 무관 — runner_qty == initial_kis_qty 면 빠른 스킵
        let d = classify_reconcile_outcome(95, 95, Err(()), 0);
        assert_eq!(d, ReconcileDecision::AlreadyMatched);
    }

    #[test]
    fn confirm_ok_match_returns_recovered() {
        // 1차 mismatch (95 vs 0) 후 confirm 일치 (95) → 회복
        let d = classify_reconcile_outcome(95, 0, Ok::<u64, ()>(95), 0);
        assert_eq!(d, ReconcileDecision::Recovered { confirmed_qty: 95 });
    }

    #[test]
    fn confirm_ok_mismatch_returns_mismatch() {
        // 1차 mismatch + confirm 도 mismatch → manual 승격 필요
        let d = classify_reconcile_outcome(95, 0, Ok::<u64, ()>(0), 0);
        assert_eq!(d, ReconcileDecision::Mismatch { confirmed_qty: 0 });
    }

    #[test]
    fn confirm_err_streak_below_threshold_returns_inconclusive() {
        // 1차 mismatch + confirm 모두 실패 + streak < 3 → 보류
        let d = classify_reconcile_outcome(95, 0, Err(()), 1);
        assert_eq!(d, ReconcileDecision::Inconclusive { streak: 1 });
        let d = classify_reconcile_outcome(95, 0, Err(()), 2);
        assert_eq!(d, ReconcileDecision::Inconclusive { streak: 2 });
    }

    #[test]
    fn confirm_err_streak_at_threshold_returns_escalated() {
        // streak == 3 부터는 escalated
        let d = classify_reconcile_outcome(95, 0, Err(()), 3);
        assert_eq!(d, ReconcileDecision::InconclusiveEscalated { streak: 3 });
        let d = classify_reconcile_outcome(95, 0, Err(()), 5);
        assert_eq!(d, ReconcileDecision::InconclusiveEscalated { streak: 5 });
    }
}

/// 2026-04-17 v3 불변식 #3: `should_skip_reconcile_for_lock` 매트릭스.
/// reconcile_once 가 실제 호출하는 helper 의 분기 정책 회귀 테스트.
#[cfg(test)]
mod reconcile_lock_skip_tests {
    use super::{PositionLockState, should_skip_reconcile_for_lock};

    #[test]
    fn pending_self_skips() {
        let state = PositionLockState::Pending {
            code: "122630".to_string(),
            preempted: false,
        };
        assert!(
            should_skip_reconcile_for_lock(&state, "122630"),
            "Pending(self) 는 reconcile skip"
        );
    }

    #[test]
    fn pending_other_does_not_skip() {
        let state = PositionLockState::Pending {
            code: "114800".to_string(),
            preempted: false,
        };
        assert!(
            !should_skip_reconcile_for_lock(&state, "122630"),
            "Pending(other) 상태에선 이 종목 reconcile 은 진행"
        );
    }

    #[test]
    fn held_self_does_not_skip() {
        let state = PositionLockState::Held("122630".to_string());
        assert!(
            !should_skip_reconcile_for_lock(&state, "122630"),
            "Held(self) 는 정상 보유 — reconcile 진행"
        );
    }

    #[test]
    fn held_other_does_not_skip() {
        let state = PositionLockState::Held("114800".to_string());
        assert!(!should_skip_reconcile_for_lock(&state, "122630"));
    }

    #[test]
    fn manual_held_self_skips() {
        let state = PositionLockState::ManualHeld("122630".to_string());
        assert!(
            should_skip_reconcile_for_lock(&state, "122630"),
            "ManualHeld(self) 는 skip (already_manual 과 중복 방어)"
        );
    }

    #[test]
    fn manual_held_other_does_not_skip() {
        let state = PositionLockState::ManualHeld("114800".to_string());
        assert!(!should_skip_reconcile_for_lock(&state, "122630"));
    }

    #[test]
    fn free_does_not_skip() {
        let state = PositionLockState::Free;
        assert!(!should_skip_reconcile_for_lock(&state, "122630"));
    }
}

/// 2026-04-24 P0-1 보강: 장외 WS backoff 안전 플래그 회귀 테스트.
#[cfg(test)]
mod off_hours_ws_backoff_safety_tests {
    use super::*;
    use crate::strategy::live_runner::{
        ArmedWatchStats, ExecutionState, PreflightMetadata, RunnerState,
    };

    fn runner_state(execution_state: ExecutionState) -> Arc<RwLock<RunnerState>> {
        Arc::new(RwLock::new(RunnerState {
            phase: "장 시작 대기".to_string(),
            today_trades: Vec::new(),
            today_pnl: 0.0,
            current_position: None,
            or_high: None,
            or_low: None,
            or_stages: Vec::new(),
            market_halted: false,
            degraded: false,
            manual_intervention_required: false,
            degraded_reason: None,
            exit_pending: false,
            execution_state,
            preflight_metadata: PreflightMetadata::default(),
            last_external_gate_stale: false,
            last_flat_transition_at: None,
            last_quote_sanity_issue: None,
            armed_stats: ArmedWatchStats::default(),
            current_armed_signal_id: None,
        }))
    }

    async fn insert_runner(mgr: &StrategyManager, code: &str, state: Arc<RwLock<RunnerState>>) {
        mgr.runners.write().await.insert(
            code.to_string(),
            RunnerHandle {
                stop_flag: Arc::new(AtomicBool::new(false)),
                stop_notify: Arc::new(tokio::sync::Notify::new()),
                runner_state: state,
            },
        );
        if let Some(status) = mgr.statuses.write().await.get_mut(code) {
            status.active = true;
        }
    }

    #[tokio::test]
    async fn safe_when_all_runners_are_flat_and_lock_free() {
        let mgr = StrategyManager::new();
        insert_runner(&mgr, "122630", runner_state(ExecutionState::Flat)).await;

        assert!(
            mgr.off_hours_ws_backoff_safe().await,
            "flat/free/no-position 상태에서는 장외 backoff 확대가 가능해야 한다"
        );
    }

    #[tokio::test]
    async fn unsafe_when_lock_is_manual_held() {
        let mgr = StrategyManager::new();
        *mgr.active_position_lock.write().await =
            PositionLockState::ManualHeld("122630".to_string());

        assert!(
            !mgr.off_hours_ws_backoff_safe().await,
            "ManualHeld 는 execution_state 와 무관하게 WS backoff 확대를 막아야 한다"
        );
    }

    #[tokio::test]
    async fn unsafe_when_runner_is_entry_pending() {
        let mgr = StrategyManager::new();
        insert_runner(&mgr, "122630", runner_state(ExecutionState::EntryPending)).await;

        assert!(
            !mgr.off_hours_ws_backoff_safe().await,
            "EntryPending 은 체결통보가 필요하므로 장외 backoff 확대 금지"
        );
    }

    #[tokio::test]
    async fn unsafe_when_status_is_manual_intervention() {
        let mgr = StrategyManager::new();
        if let Some(status) = mgr.statuses.write().await.get_mut("122630") {
            status.manual_intervention_required = true;
            status.degraded = true;
        }

        assert!(
            !mgr.off_hours_ws_backoff_safe().await,
            "status manual_intervention 이면 runner 가 없어도 fail-closed 해야 한다"
        );
    }
}

/// 2026-04-23 안정화: flat 직후 settle window 회귀 테스트.
#[cfg(test)]
mod reconcile_post_flat_window_tests {
    use super::reconcile_post_flat_remaining_ms;
    use crate::strategy::live_runner::{
        ArmedWatchStats, ExecutionState, PreflightMetadata, RunnerState,
    };

    fn make_state(
        execution_state: ExecutionState,
        last_flat_transition_at: Option<std::time::Instant>,
    ) -> RunnerState {
        RunnerState {
            phase: String::new(),
            today_trades: Vec::new(),
            today_pnl: 0.0,
            current_position: None,
            or_high: None,
            or_low: None,
            or_stages: Vec::new(),
            market_halted: false,
            degraded: false,
            manual_intervention_required: false,
            degraded_reason: None,
            exit_pending: false,
            execution_state,
            preflight_metadata: PreflightMetadata::default(),
            last_external_gate_stale: false,
            last_flat_transition_at,
            last_quote_sanity_issue: None,
            armed_stats: ArmedWatchStats::default(),
            current_armed_signal_id: None,
        }
    }

    #[test]
    fn flat_transition_inside_window_skips_reconcile() {
        let state = make_state(
            ExecutionState::Flat,
            Some(std::time::Instant::now() - std::time::Duration::from_secs(3)),
        );
        let remaining = reconcile_post_flat_remaining_ms(&state);
        assert!(
            remaining.is_some_and(|ms| ms > 0 && ms <= 10_000),
            "10초 settle window 안이면 남은 시간이 계산돼야 한다"
        );
    }

    #[test]
    fn flat_transition_outside_window_does_not_skip() {
        let state = make_state(
            ExecutionState::Flat,
            Some(std::time::Instant::now() - std::time::Duration::from_secs(11)),
        );
        assert_eq!(reconcile_post_flat_remaining_ms(&state), None);
    }

    #[test]
    fn non_flat_state_does_not_skip() {
        let state = make_state(
            ExecutionState::Open,
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        );
        assert_eq!(reconcile_post_flat_remaining_ms(&state), None);
    }
}

/// 2026-04-17 v3 fixups C: `bump_/reset_reconcile_inconclusive_streak` 회귀 테스트.
/// "연속" semantics 보장 — bump 단조증가, reset 후 다시 1, 종목별 격리.
#[cfg(test)]
mod streak_tests {
    use super::StrategyManager;

    #[tokio::test]
    async fn bump_starts_at_one_and_returns_new_value() {
        let mgr = StrategyManager::new();
        assert_eq!(mgr.bump_reconcile_inconclusive_streak("122630").await, 1);
        assert_eq!(mgr.bump_reconcile_inconclusive_streak("122630").await, 2);
        assert_eq!(mgr.bump_reconcile_inconclusive_streak("122630").await, 3);
    }

    #[tokio::test]
    async fn reset_clears_streak_and_next_bump_starts_at_one() {
        let mgr = StrategyManager::new();
        mgr.bump_reconcile_inconclusive_streak("122630").await;
        mgr.bump_reconcile_inconclusive_streak("122630").await;
        mgr.reset_reconcile_inconclusive_streak("122630").await;
        assert_eq!(
            mgr.bump_reconcile_inconclusive_streak("122630").await,
            1,
            "reset 후 다음 bump 는 1 부터 시작 ('연속' semantics)"
        );
    }

    #[tokio::test]
    async fn streaks_are_isolated_per_code() {
        let mgr = StrategyManager::new();
        mgr.bump_reconcile_inconclusive_streak("122630").await;
        mgr.bump_reconcile_inconclusive_streak("122630").await;
        // 다른 종목의 bump 는 122630 streak 에 영향 없음
        assert_eq!(mgr.bump_reconcile_inconclusive_streak("114800").await, 1);
        assert_eq!(mgr.bump_reconcile_inconclusive_streak("122630").await, 3);
    }

    #[tokio::test]
    async fn reset_on_unknown_code_is_safe_noop() {
        let mgr = StrategyManager::new();
        // panic 없이 통과해야 함
        mgr.reset_reconcile_inconclusive_streak("999999").await;
        // 그 후 정상 bump
        assert_eq!(mgr.bump_reconcile_inconclusive_streak("999999").await, 1);
    }
}

/// 2026-04-17 P1: `set_reconcile_confirm_params` 의 하한 보정 회귀 테스트.
#[cfg(test)]
mod reconcile_confirm_setter_tests {
    use super::StrategyManager;

    #[test]
    fn defaults_are_three_attempts_five_seconds() {
        let mgr = StrategyManager::new();
        assert_eq!(mgr.reconcile_confirm_attempts, 3);
        assert_eq!(mgr.reconcile_confirm_interval_secs, 5);
    }

    #[test]
    fn setter_applies_floor_one_for_attempts() {
        let mut mgr = StrategyManager::new();
        mgr.set_reconcile_confirm_params(0, 10);
        assert_eq!(mgr.reconcile_confirm_attempts, 1);
        assert_eq!(mgr.reconcile_confirm_interval_secs, 10);
    }

    #[test]
    fn setter_applies_floor_two_for_interval() {
        let mut mgr = StrategyManager::new();
        mgr.set_reconcile_confirm_params(5, 0);
        assert_eq!(mgr.reconcile_confirm_attempts, 5);
        assert_eq!(mgr.reconcile_confirm_interval_secs, 2);
    }

    #[test]
    fn setter_passes_normal_values() {
        let mut mgr = StrategyManager::new();
        mgr.set_reconcile_confirm_params(2, 8);
        assert_eq!(mgr.reconcile_confirm_attempts, 2);
        assert_eq!(mgr.reconcile_confirm_interval_secs, 8);
    }
}
