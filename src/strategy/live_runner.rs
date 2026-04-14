use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Local, NaiveTime};
use serde::Deserialize;
use tokio::sync::{Mutex, Notify, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::domain::error::KisError;
use crate::domain::models::order::OrderSide;
use crate::domain::models::price::InquirePrice;
use crate::domain::ports::realtime::{RealtimeData, TradeNotification};
use crate::infrastructure::cache::postgres_store::{ActivePosition, PostgresStore, TradeRecord};
use crate::infrastructure::monitoring::event_logger::EventLogger;
use crate::infrastructure::websocket::candle_aggregator::CompletedCandle;
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::{StockCode, TransactionId};
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};

use super::candle::{self, MinuteCandle};
use super::fvg::{FairValueGap, FvgDirection};
use super::orb_fvg::{OrbFvgConfig, OrbFvgStrategy};
use super::types::*;

/// KIS 분봉 응답 항목
#[derive(Debug, Clone, Deserialize)]
struct MinutePriceItem {
    stck_bsop_date: String,
    stck_cntg_hour: String,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_oprc: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_hgpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_lwpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_prpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    cntg_vol: i64,
}

/// 실시간 러너 공유 상태 (웹에서 읽기 가능)
#[derive(Debug, Clone)]
pub struct RunnerState {
    pub phase: String,
    pub today_trades: Vec<TradeResult>,
    pub today_pnl: f64,
    pub current_position: Option<Position>,
    /// OR(Opening Range) 고가/저가 (기본 15분 — 호환용)
    pub or_high: Option<i64>,
    pub or_low: Option<i64>,
    /// Multi-Stage OR: (단계명, high, low) — "5m", "15m", "30m"
    pub or_stages: Vec<(String, i64, i64)>,
    /// 장 중단 여부 (VI 발동 또는 거래정지)
    pub market_halted: bool,
}

/// 실시간 트레이딩 러너
pub struct LiveRunner {
    client: Arc<KisHttpClient>,
    strategy: OrbFvgStrategy,
    stock_code: StockCode,
    stock_name: String,
    quantity: u64,
    /// 외부에서 중지 요청
    stop_flag: Arc<AtomicBool>,
    /// 공유 상태
    pub state: Arc<RwLock<RunnerState>>,
    /// 주문 체결 알림 전송
    trade_tx: Option<broadcast::Sender<RealtimeData>>,
    /// WebSocket 실시간 데이터 수신 (체결가)
    realtime_rx: Option<broadcast::Receiver<RealtimeData>>,
    /// 최신 실시간 가격 (WebSocket에서 갱신) + 갱신 시각
    latest_price: Arc<RwLock<Option<(i64, tokio::time::Instant)>>>,
    /// 외부 중지 알림 (sleep 즉시 깨우기)
    stop_notify: Arc<Notify>,
    /// WebSocket 실시간 분봉 (CandleAggregator 공유)
    ws_candles: Option<Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>>,
    /// DB 저장소 (거래 기록 영속화)
    db_store: Option<Arc<PostgresStore>>,
    /// 공유 포지션 잠금: 한 종목이 포지션 보유 시 다른 종목 진입 차단
    position_lock: Option<Arc<RwLock<PositionLockState>>>,
    /// 신호 탐색 진행 상태 (백테스트 BacktestEngine state.search_from 등가).
    /// 청산 시각 이후의 5분봉만 탐색하도록 하여 같은 FVG 재진입을 차단한다.
    /// 2026-04-10 사고(trade #54~58 같은 entry_price 4회 반복) 핫픽스.
    signal_state: Arc<RwLock<LiveSignalState>>,
    /// 운영 이벤트 로거 (fire-and-forget 비동기 DB 저장)
    event_logger: Option<Arc<EventLogger>>,
    /// 라이브 전용 설정 (drift 가드 등). 백테스트 미사용.
    live_cfg: LiveRunnerConfig,
    /// 최근 발주 이력 — 반복 발주 자동 안전장치용.
    /// 5분 창 내 동일가 ±0.1% 주문이 3회 이상이면 runner 자동 중단.
    recent_orders: Arc<Mutex<VecDeque<(NaiveTime, i64)>>>,
}

/// 공유 포지션 잠금 상태 — 두 종목(122630, 114800) 간 진입 조율.
///
/// Pending 단계에서 다른 종목이 선점(preempt) 요청을 할 수 있어,
/// 미체결 대기(최대 30초)로 인한 교착을 방지한다.
/// - `Free` → `Pending` → `Held` → `Free` (정상 플로우)
/// - `Pending(preempted=true)` → `Free` (선점 당해 취소)
/// - `Pending(preempted=true)` → `Held` (선점 요청 있으나 이미 체결 → 체결 우선)
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PositionLockState {
    /// 잠금 없음 — 모든 종목 진입 가능
    Free,
    /// 지정가 발주 후 체결 대기 중.
    /// `preempted`=true이면 다른 종목이 선점을 요청한 상태 → 즉시 취소해야 함.
    Pending { code: String, preempted: bool },
    /// 포지션 보유 확정 — 다른 종목 진입 차단
    Held(String),
}

impl Default for PositionLockState {
    fn default() -> Self { Self::Free }
}

/// 라이브 신호 탐색 상태 — 청산 시각 이후의 5분봉만 탐색하도록 한다.
/// 백테스트 `BacktestEngine::next_valid_candidate` 의 `state.search_from` 과
/// `simulate_day_locked` 의 `sync_search_to(target=last_unlock_time)` 에 대응.
#[derive(Debug, Default, Clone)]
pub(crate) struct LiveSignalState {
    /// 이 시각보다 큰 5분봉만 탐색 (`c.time > search_after`).
    /// None이면 제한 없이 모든 5분봉 탐색 (장 시작 직후).
    search_after: Option<NaiveTime>,
}

/// 라이브 러너 고유 설정 (백테스트와 분리).
///
/// `OrbFvgConfig` 는 백테스트/라이브 공용이라 백테스트 결과에 영향을 주지 않으려는
/// 라이브 전용 가드는 이곳에 둔다.
#[derive(Debug, Clone, Default)]
pub struct LiveRunnerConfig {
    /// 진입 지정가 대비 현재가 이탈 허용 한계 (Long: (cur - entry) / entry).
    /// `None` 이면 가드 비활성 (Day+0 기본). `Some(0.005)` 는 0.5%.
    pub max_entry_drift_pct: Option<f64>,
}

/// Multi-stage OR 탐색에서 선착순으로 확정된 진입 신호.
///
/// `signal_time` 은 FVG 리트레이스가 확인된 5분봉 시각(`c5.time`).
/// `abort_entry` 로 진입 포기 시 search_after 를 이 시각으로 전진하여 같은 FVG 재감지를 차단한다.
#[derive(Debug, Clone)]
struct BestSignal {
    side: PositionSide,
    entry_price: i64,
    stop_loss: i64,
    take_profit: i64,
    stage_name: &'static str,
    signal_time: NaiveTime,
}

impl LiveRunner {
    pub fn new(
        client: Arc<KisHttpClient>,
        stock_code: StockCode,
        stock_name: String,
        quantity: u64,
        stop_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            client,
            strategy: OrbFvgStrategy { config: OrbFvgConfig::default() },
            stock_code,
            stock_name,
            quantity,
            stop_flag,
            state: Arc::new(RwLock::new(RunnerState {
                phase: "초기화".to_string(),
                today_trades: Vec::new(),
                today_pnl: 0.0,
                current_position: None,
                or_high: None,
                or_low: None,
                or_stages: Vec::new(),
                market_halted: false,
            })),
            trade_tx: None,
            realtime_rx: None,
            latest_price: Arc::new(RwLock::new(None)),
            stop_notify: Arc::new(Notify::new()),
            ws_candles: None,
            db_store: None,
            position_lock: None,
            signal_state: Arc::new(RwLock::new(LiveSignalState::default())),
            event_logger: None,
            live_cfg: LiveRunnerConfig::default(),
            recent_orders: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub(crate) fn with_position_lock(mut self, lock: Arc<RwLock<PositionLockState>>) -> Self {
        self.position_lock = Some(lock);
        self
    }

    /// 라이브 전용 설정 주입 (drift 가드 활성화 등).
    pub fn with_live_config(mut self, cfg: LiveRunnerConfig) -> Self {
        self.live_cfg = cfg;
        self
    }

    pub fn with_event_logger(mut self, logger: Arc<EventLogger>) -> Self {
        self.event_logger = Some(logger);
        self
    }

    pub fn with_db_store(mut self, store: Arc<PostgresStore>) -> Self {
        self.db_store = Some(store);
        self
    }

    pub fn with_ws_candles(mut self, candles: Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>) -> Self {
        self.ws_candles = Some(candles);
        self
    }

    /// 미체결 주문 전체 취소 (서버 재시작 시 이전 TP 지정가 등 정리)
    async fn cancel_all_pending_orders(&self) {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
            ("INQR_DVSN_1", ""),
            ("INQR_DVSN_2", ""),
        ];

        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self.client
            .execute(HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-psbl-rvsecncl",
                &TransactionId::InquirePsblOrder,
                Some(&query), None)
            .await;

        if let Ok(r) = resp {
            let items = r.output.or(r.output1).unwrap_or_default();
            let mut cancelled = 0;
            for item in &items {
                let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
                if code != self.stock_code.as_str() { continue; }

                let order_no = item.get("odno").and_then(|v| v.as_str()).unwrap_or("");
                let _krx_orgno = item.get("orgn_odno").and_then(|v| v.as_str()).unwrap_or("");
                let qty_str = item.get("psbl_qty").and_then(|v| v.as_str()).unwrap_or("0");

                if order_no.is_empty() { continue; }

                // 취소 요청
                let cancel_body = serde_json::json!({
                    "CANO": self.client.account_no(),
                    "ACNT_PRDT_CD": self.client.account_product_code(),
                    "KRX_FWDG_ORD_ORGNO": "",
                    "ORGN_ODNO": order_no,
                    "ORD_DVSN": "00",
                    "RVSE_CNCL_DVSN_CD": "02",
                    "ORD_QTY": qty_str,
                    "ORD_UNPR": "0",
                    "QTY_ALL_ORD_YN": "Y",
                });

                let cancel_resp: Result<KisResponse<serde_json::Value>, _> = self.client
                    .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-rvsecncl",
                        &TransactionId::OrderCancel, None, Some(&cancel_body))
                    .await;

                match cancel_resp {
                    Ok(cr) if cr.rt_cd == "0" => {
                        cancelled += 1;
                        info!("{}: 미체결 주문 취소 — 주문번호={}", self.stock_name, order_no);
                    }
                    Ok(cr) => {
                        warn!("{}: 미체결 주문 취소 실패 — {}: {}", self.stock_name, order_no, cr.msg1);
                    }
                    Err(e) => {
                        warn!("{}: 미체결 주문 취소 에러 — {}: {e}", self.stock_name, order_no);
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }

            if cancelled > 0 {
                info!("{}: 미체결 주문 {}건 취소 완료", self.stock_name, cancelled);
                // 취소 후 잔고 반영 대기
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }

    /// DB에서 활성 포지션 복구 (TP 주문번호 포함)
    async fn check_and_restore_position(&self) {
        let Some(ref store) = self.db_store else {
            // DB 없으면 잔고 API로 fallback
            self.check_and_restore_from_balance().await;
            return;
        };

        match store.get_active_position(self.stock_code.as_str()).await {
            Ok(Some(saved)) => {
                info!("{}: DB에서 활성 포지션 복구 — {}주 @ {}원, TP주문={}",
                    self.stock_name, saved.quantity, saved.entry_price, saved.tp_order_no);

                // 이전 TP 지정가 취소 (주문번호를 알고 있으므로 확실히 취소 가능)
                let mut tp_cancel_failed = false;
                if !saved.tp_order_no.is_empty() {
                    match self.cancel_tp_order(&saved.tp_order_no, &saved.tp_krx_orgno).await {
                        Ok(()) => info!("{}: 이전 TP 주문 취소 성공", self.stock_name),
                        Err(e) => {
                            warn!("{}: 이전 TP 주문 취소 실패: {e}", self.stock_name);
                            tp_cancel_failed = true;
                        }
                    }
                    // 취소 후 잔고 반영 대기
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }

                // TP 취소 실패 시 잔고를 확인하여 유령 포지션 방지.
                // 재시작 동안 KRX가 TP를 체결했으면 보유 0주 → DB만 정리하고 깨끗하게 시작.
                if tp_cancel_failed && !self.check_balance_has_stock().await {
                    warn!("{}: TP 취소 실패 + 보유 0주 — 재시작 중 TP 체결된 것으로 판단, DB 정리",
                        self.stock_name);
                    let _ = store.delete_active_position(self.stock_code.as_str()).await;
                    // 포지션 잠금도 해제 (다른 종목 진입 허용)
                    if let Some(ref lock) = self.position_lock {
                        let mut guard = lock.write().await;
                        match &*guard {
                            PositionLockState::Held(c) | PositionLockState::Pending { code: c, .. }
                                if c == self.stock_code.as_str() => { *guard = PositionLockState::Free; }
                            _ => {}
                        }
                    }
                    info!("{}: 유령 포지션 정리 완료 — 깨끗한 상태에서 시작", self.stock_name);
                    return;
                }

                // 새 TP 지정가 발주
                let (tp_order_no, tp_krx_orgno, tp_limit_price) =
                    if let Some((no, orgno, placed_price)) = self.place_tp_limit_order(saved.take_profit, saved.quantity as u64).await {
                        (Some(no), Some(orgno), Some(placed_price))
                    } else {
                        warn!("{}: 복구 TP 지정가 발주 실패 — 시장가 fallback", self.stock_name);
                        (None, None, None)
                    };

                let mut state = self.state.write().await;
                state.current_position = Some(Position {
                    side: PositionSide::Long,
                    entry_price: saved.entry_price,
                    stop_loss: saved.stop_loss,
                    take_profit: saved.take_profit,
                    entry_time: saved.entry_time.time(),
                    quantity: saved.quantity as u64,
                    tp_order_no: tp_order_no.clone(),
                    tp_krx_orgno: tp_krx_orgno.clone(),
                    tp_limit_price,
                    reached_1r: saved.reached_1r,
                    best_price: saved.best_price,
                    original_sl: saved.original_sl,
                    sl_triggered_tick: false,
                    intended_entry_price: saved.entry_price,
                    order_to_fill_ms: 0,
                });
                if saved.reached_1r {
                    info!("{}: 트레일링 상태 복구 — reached_1r=true, best_price={}", self.stock_name, saved.best_price);
                }
                state.phase = "포지션 보유 (복구)".to_string();

                // DB에서 모든 OR 단계 복구
                let today = Local::now().date_naive();
                if let Ok(stages) = store.get_all_or_stages(self.stock_code.as_str(), today).await {
                    if !stages.is_empty() {
                        state.or_high = Some(stages[0].1);
                        state.or_low = Some(stages[0].2);
                        state.or_stages = stages.clone();
                        info!("{}: OR 범위 DB 복구 — {}단계", self.stock_name, stages.len());
                    }
                }

                // DB에 새 TP 주문번호 갱신
                if let (Some(no), Some(orgno)) = (&tp_order_no, &tp_krx_orgno) {
                    let mut updated = saved.clone();
                    updated.tp_order_no = no.clone();
                    updated.tp_krx_orgno = orgno.clone();
                    let _ = store.save_active_position(&updated).await;
                }

                info!("{}: 포지션 복구 완료 — SL={}, TP={}", self.stock_name, saved.stop_loss, saved.take_profit);
                return;
            }
            Ok(None) => {
                info!("{}: DB에 활성 포지션 없음 — orphan 주문 사전 정리", self.stock_name);
                // DB에 포지션이 없다는 것은 정상 TP 지정가도 없다는 뜻.
                // KIS에 남아있는 미체결 주문은 모두 orphan(프로세스 이상 종료 잔재) → 취소 안전.
                // 이 방어가 taskkill /F, SIGKILL, 패닉 등 모든 비정상 종료를 커버한다.
                self.cancel_all_pending_orders().await;
                return;
            }
            Err(e) => warn!("{}: DB 포지션 조회 실패: {e}", self.stock_name),
        }

        // DB 조회 실패 시에만 잔고 API로 fallback
        self.check_and_restore_from_balance().await;
    }

    /// 잔고 API로 보유 확인 → 포지션 복구 (DB에 없을 때 fallback)
    async fn check_and_restore_from_balance(&self) {
        // 미체결 주문 취소 시도
        self.cancel_all_pending_orders().await;

        // 잔고 API로 해당 종목 보유 여부 확인
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
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

        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self.client
            .execute(HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query), None)
            .await;

        if let Ok(r) = resp {
            let items = r.output.or(r.output1).unwrap_or_default();
            if !items.is_empty() {
                for item in &items {
                    let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
                    if code != self.stock_code.as_str() { continue; }

                    let qty: u64 = item.get("hldg_qty")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let avg: i64 = item.get("pchs_avg_pric")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(|f| f as i64)
                        .unwrap_or(0);

                    if qty > 0 && avg > 0 {
                        info!("{}: 잔존 포지션 감지 — {}주 @ {}원, 복구 중", self.stock_name, qty, avg);
                        self.restore_position(avg, qty).await;
                        return;
                    }
                }
            }
        }
    }

    /// 기존 보유 포지션 복구 (서버 재시작 시)
    async fn restore_position(&self, avg_price: i64, quantity: u64) {
        let cfg = &self.strategy.config;
        // OR 범위 기반 SL/TP 재계산은 불가 → 진입가 기준 고정 비율 사용
        let risk = (avg_price as f64 * 0.003) as i64; // 0.3% 리스크
        let stop_loss = avg_price - risk;
        let take_profit = avg_price + (risk as f64 * cfg.rr_ratio) as i64;

        // TP 지정가 매도 발주
        let (tp_order_no, tp_krx_orgno, tp_limit_price) =
            if let Some((no, orgno, placed_price)) = self.place_tp_limit_order(take_profit, quantity).await {
                (Some(no), Some(orgno), Some(placed_price))
            } else {
                (None, None, None)
            };

        let mut state = self.state.write().await;
        state.current_position = Some(Position {
            side: PositionSide::Long,
            entry_price: avg_price,
            stop_loss,
            take_profit,
            entry_time: Local::now().time(),
            quantity,
            tp_order_no,
            tp_krx_orgno,
            tp_limit_price,
            reached_1r: false,
            best_price: avg_price,
            original_sl: stop_loss,
            sl_triggered_tick: false,
            intended_entry_price: avg_price,
            order_to_fill_ms: 0,
        });
        state.phase = "포지션 보유 (복구)".to_string();

        // DB에서 OR 값 로드
        if let Some(ref store) = self.db_store {
            let today = Local::now().date_naive();
            if let Ok(Some((h, l))) = store.get_or_range(self.stock_code.as_str(), today).await {
                state.or_high = Some(h);
                state.or_low = Some(l);
            }
        }

        info!("포지션 복구: {}주 @ {}원, SL={}, TP={}", quantity, avg_price, stop_loss, take_profit);
    }

    /// 외부 중지 알림 핸들 (StrategyManager에서 사용)
    pub fn stop_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.stop_notify)
    }

    pub fn with_trade_tx(mut self, tx: broadcast::Sender<RealtimeData>) -> Self {
        // 실시간 수신용 receiver도 생성
        self.realtime_rx = Some(tx.subscribe());
        self.trade_tx = Some(tx);
        self
    }

    /// 주문 체결 알림 전송
    fn notify_trade(&self, action: &str) {
        if let Some(ref tx) = self.trade_tx {
            let _ = tx.send(RealtimeData::TradeNotification(TradeNotification {
                stock_code: self.stock_code.as_str().to_string(),
                stock_name: self.stock_name.clone(),
                action: action.to_string(),
            }));
        }
    }

    /// WebSocket 실시간 가격 + 장운영정보 수신 태스크 시작
    ///
    /// 틱마다 수행:
    /// 1. 최신 가격 캐시 갱신 (`latest_price`)
    /// 2. 포지션 보유 중이면 백테스트 `simulate_exit` Step 2+3 재현:
    ///    - best_price 갱신
    ///    - 1R 본전 스탑 활성화
    ///    - trailing SL 갱신
    ///    - 갱신된 SL 기준 돌파 감지 → `sl_triggered_tick` 플래그 + notify
    /// 3. 장중단(VI/거래정지) 상태 전환 감지
    ///
    /// SL 감지는 manage_position 3초 루프를 우회하여 틱 해상도로 수행.
    /// TP는 거래소(KRX)가 지정가 선발주로 직접 체결하므로 여기서 다루지 않음.
    fn spawn_price_listener(&mut self) {
        if let Some(mut rx) = self.realtime_rx.take() {
            let code = self.stock_code.as_str().to_string();
            let stock_name = self.stock_name.clone();
            let latest = Arc::clone(&self.latest_price);
            let state = Arc::clone(&self.state);
            let notify = Arc::clone(&self.stop_notify);
            let breakeven_r = self.strategy.config.breakeven_r;
            let trailing_r = self.strategy.config.trailing_r;

            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(RealtimeData::MarketOperation(op)) if op.stock_code == code => {
                            let halted = op.is_trading_halt || op.vi_applied != "0";
                            let mut s = state.write().await;
                            if halted != s.market_halted {
                                if halted {
                                    info!("{}: 장 중단 감지 (VI={}, 거래정지={})",
                                        code, op.vi_applied, op.is_trading_halt);
                                } else {
                                    info!("{}: 장 정상화", code);
                                }
                                s.market_halted = halted;
                            }
                        }
                        Ok(RealtimeData::Execution(exec)) if exec.stock_code == code => {
                            *latest.write().await = Some((exec.price, tokio::time::Instant::now()));

                            // 백테스트 simulate_exit Step 2+3 틱 재현
                            let wake = {
                                let mut s = state.write().await;
                                if let Some(ref mut pos) = s.current_position {
                                    // Step 2-a: best_price 갱신
                                    pos.best_price = match pos.side {
                                        PositionSide::Long => pos.best_price.max(exec.price),
                                        PositionSide::Short => pos.best_price.min(exec.price),
                                    };

                                    // Step 2-b: 1R 본전 스탑 활성화
                                    let risk = (pos.entry_price - pos.original_sl).abs() as f64;
                                    let breakeven_dist = (risk * breakeven_r) as i64;
                                    let trailing_dist = (risk * trailing_r) as i64;

                                    let profit = match pos.side {
                                        PositionSide::Long => pos.best_price - pos.entry_price,
                                        PositionSide::Short => pos.entry_price - pos.best_price,
                                    };
                                    if !pos.reached_1r && profit >= breakeven_dist {
                                        pos.reached_1r = true;
                                        pos.stop_loss = pos.entry_price;
                                        info!("{}: 1R 도달 (틱) — 본전 스탑 활성화 (SL → {})",
                                            stock_name, pos.stop_loss);
                                    }

                                    // Step 2-c: trailing SL 갱신 (best_price 기준)
                                    if pos.reached_1r {
                                        let new_sl = match pos.side {
                                            PositionSide::Long => pos.best_price - trailing_dist,
                                            PositionSide::Short => pos.best_price + trailing_dist,
                                        };
                                        let improved = match pos.side {
                                            PositionSide::Long => new_sl > pos.stop_loss,
                                            PositionSide::Short => new_sl < pos.stop_loss,
                                        };
                                        if improved {
                                            pos.stop_loss = new_sl;
                                        }
                                    }

                                    // Step 3: SL 돌파 감지 (갱신된 stop_loss 기준)
                                    let sl_hit = match pos.side {
                                        PositionSide::Long => exec.price <= pos.stop_loss,
                                        PositionSide::Short => exec.price >= pos.stop_loss,
                                    };
                                    if sl_hit && !pos.sl_triggered_tick {
                                        pos.sl_triggered_tick = true;
                                        info!("{}: SL 틱 감지 — price={}, SL={}",
                                            stock_name, exec.price, pos.stop_loss);
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            };

                            if wake {
                                // manage_position sleep 즉시 깨움
                                notify.notify_waiters();
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        _ => {} // 다른 종목이나 다른 타입은 무시
                    }
                }
            });
        }
    }

    /// 현재가 조회 — WebSocket 우선 (30초 이내), fallback으로 REST
    async fn get_current_price(&self) -> Result<i64, KisError> {
        if let Some((price, updated_at)) = *self.latest_price.read().await {
            if updated_at.elapsed() < std::time::Duration::from_secs(30) {
                return Ok(price);
            }
            let age_secs = updated_at.elapsed().as_secs();
            warn!("{}: 실시간 가격 {}초 미갱신 — REST 폴백", self.stock_name, age_secs);
            if let Some(ref el) = self.event_logger {
                el.log_event(self.stock_code.as_str(), "system", "ws_tick_gap", "warn",
                    &format!("틱 {}초 미수신 — REST 폴백", age_secs),
                    serde_json::json!({"last_tick_age_secs": age_secs}));
            }
        }
        // fallback: REST API (장외 시간, WS 끊김, 30초 이상 미갱신 시)
        let resp = self.fetch_current_price_rest().await?;
        Ok(resp.stck_prpr)
    }

    /// 장중 실행 루프 (토글 OFF 시 중단 가능)
    pub async fn run(&mut self) -> Result<Vec<TradeResult>, KisError> {
        // WebSocket 가격 수신 태스크 시작 (self 빌림 해제 전에 호출)
        self.spawn_price_listener();

        let cfg = &self.strategy.config;

        info!("=== {} ({}) 자동매매 시작 ===", self.stock_name, self.stock_code);
        info!("수량: {}주, RR: 1:{:.1}, 트레일링: {:.1}R, 본전: {:.1}R",
            self.quantity, cfg.rr_ratio, cfg.trailing_r, cfg.breakeven_r);

        // 잔존 포지션 확인
        self.check_and_restore_position().await;

        // 재시작 회귀 방지: DB에서 오늘 trades 상태를 읽어 signal_state.search_after 복구.
        // 핫픽스 commit 후 cargo build + restart 로 새 binary 가 시작될 때 (예: 2026-04-10 13:38:53),
        // 메인 루프 로컬 변수(trade_count / confirmed_side) 가 0/None 으로 reset 되어
        // 같은 5분봉의 같은 FVG 가 청산 후 다시 잡히는 것을 막기 위함.
        let today = Local::now().date_naive();
        if let Some(ref store) = self.db_store {
            if let Ok(Some(t)) = store.get_last_trade_exit_today(self.stock_code.as_str(), today).await {
                self.signal_state.write().await.search_after = Some(t);
                info!("{}: 재시작 복구 — signal_state.search_after = {} (DB)", self.stock_name, t);
            }
        }

        // 장 시작 대기
        self.update_phase("장 시작 대기").await;
        self.wait_until(cfg.or_start).await;
        if self.is_stopped() { return Ok(Vec::new()); }

        // 잔존 포지션이 있으면 장 시작 즉시 시장가 청산 (전일 잔여분 정리)
        // 단, "장 시작 직후"(or_start ~ or_start+5분)일 때만 동작.
        // 장중 재시작 시(예: 13:39 핫픽스 후 재시작) 오늘 들어간 정상 포지션을
        // 잘못 청산하지 않도록 시간 게이팅.
        let now_time = Local::now().time();
        let elapsed_secs_since_open = (now_time - cfg.or_start).num_seconds();
        let is_market_open_window = elapsed_secs_since_open >= 0 && elapsed_secs_since_open <= 300;
        let has_leftover = self.state.read().await.current_position.is_some();
        if has_leftover && !is_market_open_window {
            info!("{}: 잔존 포지션 발견했지만 장 시작 직후가 아님(elapsed={}s) — 정상 운용 진행", self.stock_name, elapsed_secs_since_open);
        }
        if has_leftover && is_market_open_window {
            info!("{}: 전일 잔여 포지션 — 장 시작 시장가 청산", self.stock_name);
            self.update_phase("잔여 포지션 청산").await;
            // 장 시작 직후 체결 안정화 대기 (5초)
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            match self.close_position_market(ExitReason::EndOfDay).await {
                Ok(result) => {
                    info!("{}: 잔여 포지션 청산 완료 — {:.2}%", self.stock_name, result.pnl_pct());
                    self.save_trade_to_db(&result).await;
                }
                Err(e) => {
                    warn!("{}: 잔여 포지션 청산 실패: {e} — 포지션 강제 폐기 (모의투자 잔고잠김 가능)", self.stock_name);
                    // 모의투자에서 잔고 있어도 매도 불가한 경우 발생
                    // 무한 재시도 방지를 위해 포지션 강제 폐기
                    let mut state = self.state.write().await;
                    state.current_position = None;
                    state.phase = "신호 탐색".to_string();
                    if let Some(ref store) = self.db_store {
                        let _ = store.delete_active_position(self.stock_code.as_str()).await;
                    }
                    drop(state);
                    if let Some(ref lock) = self.position_lock {
                        *lock.write().await = PositionLockState::Free;
                    }
                    warn!("{}: 포지션 강제 폐기 완료 — 깨끗한 상태에서 시작", self.stock_name);
                }
            }
        }

        // Multi-Stage OR: 5분 OR(09:05)부터 시작, 15분(09:15), 30분(09:30) 점진 추가
        let or_5m_end = NaiveTime::from_hms_opt(9, 5, 0).unwrap();
        let or_15m_end = NaiveTime::from_hms_opt(9, 15, 0).unwrap();
        let or_30m_end = NaiveTime::from_hms_opt(9, 30, 0).unwrap();

        self.update_phase("OR 수집 중 (5분)").await;
        self.wait_until(or_5m_end).await;
        if self.is_stopped() { return Ok(Vec::new()); }

        // 메인 루프: 분봉 폴링 → 전략 평가 → 주문 실행
        // 재시작 복구: trade_count / confirmed_side 를 DB의 오늘 거래에서 복원하여
        // 백테스트와 라이브의 일별 상태가 항상 일치하도록 한다.
        let mut all_trades: Vec<TradeResult> = Vec::new();
        let mut trade_count: usize = if let Some(ref store) = self.db_store {
            store.count_trades_today(self.stock_code.as_str(), today).await.unwrap_or(0) as usize
        } else { 0 };
        let mut confirmed_side: Option<PositionSide> = if let Some(ref store) = self.db_store {
            match store.get_last_trade_side_pnl_today(self.stock_code.as_str(), today).await {
                Ok(Some((side, pnl))) if pnl > 0.0 => match side.as_str() {
                    "Long" => Some(PositionSide::Long),
                    "Short" => Some(PositionSide::Short),
                    _ => None,
                },
                _ => None,
            }
        } else { None };
        if trade_count > 0 {
            info!("{}: 재시작 복구 — trade_count={}, confirmed_side={:?} (DB)",
                self.stock_name, trade_count, confirmed_side);
        }

        self.update_phase("신호 탐색").await;

        loop {
            if self.is_stopped() {
                info!("{}: 외부 중지 요청", self.stock_name);
                let state = self.state.read().await;
                if state.current_position.is_some() {
                    drop(state);
                    if let Ok(result) = self.close_position_market(ExitReason::EndOfDay).await {
                        self.save_trade_to_db(&result).await;
                        all_trades.push(result);
                    }
                }
                break;
            }

            let now = Local::now().time();

            // 강제 청산 시각
            if now >= cfg.force_exit {
                let state = self.state.read().await;
                if state.current_position.is_some() {
                    drop(state);
                    info!("{}: 장마감 강제 청산", self.stock_name);
                    if let Ok(result) = self.close_position_market(ExitReason::EndOfDay).await {
                        self.save_trade_to_db(&result).await;
                        all_trades.push(result);
                    }
                }
                break;
            }

            // 진입 마감
            if now >= cfg.entry_cutoff {
                let state = self.state.read().await;
                if state.current_position.is_none() {
                    info!("{}: 진입 마감 (15:20)", self.stock_name);
                    break;
                }
                drop(state);
            }

            // 포지션 보유 중: 실시간 청산 관리
            let has_position = self.state.read().await.current_position.is_some();
            if has_position {
                match self.manage_position().await {
                    Ok(Some(result)) => {
                        let pnl = result.pnl_pct();
                        let side = result.side;
                        let exit_time = result.exit_time;
                        self.save_trade_to_db(&result).await;
                        all_trades.push(result);
                        trade_count += 1;
                        self.update_pnl(&all_trades).await;

                        // 백테스트 sync_search_to(last_unlock_time) 등가:
                        // 다음 신호 탐색을 청산 시각 이후의 5분봉으로 한정.
                        // 같은 FVG 재진입 차단 (2026-04-10 trade #54~58 사고 핫픽스).
                        {
                            let mut ss = self.signal_state.write().await;
                            ss.search_after = Some(exit_time);
                        }

                        let cumulative_pnl: f64 = all_trades.iter().map(|t| t.pnl_pct()).sum();

                        // 일일 최대 거래 횟수
                        if trade_count >= cfg.max_daily_trades {
                            info!("{}: 최대 거래 횟수 도달 ({}회)", self.stock_name, cfg.max_daily_trades);
                            break;
                        }

                        // 일일 누적 손실 한도
                        if cumulative_pnl <= cfg.max_daily_loss_pct {
                            info!("{}: 일일 손실 한도 도달 ({:.2}%)", self.stock_name, cumulative_pnl);
                            break;
                        }

                        info!("{}: {}차 거래 {:.2}% (누적 {:.2}%) — 신호 탐색 계속",
                            self.stock_name, trade_count, pnl, cumulative_pnl);

                        confirmed_side = if pnl > 0.0 { Some(side) } else { None };
                        self.update_phase("신호 탐색").await;
                    }
                    Ok(None) => {} // 유지
                    Err(e) => {
                        warn!("{}: 포지션 관리 실패: {e}", self.stock_name);
                    }
                }
                // WebSocket으로 가격 수신하므로 짧은 간격, stop 시 즉시 깨움
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
                    _ = self.stop_notify.notified() => {}
                }
                continue;
            }

            // VI 발동/거래정지 중이면 신호 탐색 건너뛰기
            if self.state.read().await.market_halted {
                debug!("{}: 장 중단 중 — 신호 탐색 대기", self.stock_name);
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                    _ = self.stop_notify.notified() => {}
                }
                continue;
            }

            // 분봉 폴링 → 신호 탐색 → 진입
            match self.poll_and_enter(trade_count == 0, confirmed_side).await {
                Ok(true) => {
                    self.update_phase("포지션 보유").await;
                }
                Ok(false) => {} // 신호 없음
                Err(e) => {
                    warn!("{}: 폴링 실패: {e}", self.stock_name);
                }
            }

            // 폴링 5초: 두 종목 LiveRunner 가 거의 동시에 깨어나도록 하여
            // 한 종목이 청산 후 즉시 재진입하면서 다른 종목이 lock 잡을 윈도우 자체가
            // 사라지는 race 를 줄임. fetch_candles_split 은 local 메모리 조회라 부하 영향 없음.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                _ = self.stop_notify.notified() => {}
            }
        }

        self.update_phase("종료").await;
        self.update_pnl(&all_trades).await;

        for (i, t) in all_trades.iter().enumerate() {
            info!(
                "{}: {}차 거래 {:?} {:.2}% ({:?})",
                self.stock_name, i + 1, t.side, t.pnl_pct(), t.exit_reason
            );
        }

        Ok(all_trades)
    }

    // (track_order_and_guard_repeat 의 순수 로직은 파일 하단 free function 으로 분리해 테스트 가능하게 함)

    /// 진입 시도 중단 공통 경로.
    ///
    /// - `advance_search=true`: 해당 FVG 재탐색 차단 (search_after 전진).
    ///   미체결 timeout / drift 초과 / 진입 cutoff 등 "이 FVG는 포기" 사유.
    /// - `advance_search=false`: lock 만 해제. search_after 불변.
    ///   API 일시 장애 / 선점(preempt) / 현재가 조회 실패 등 "FVG 자체는 살려둠" 사유.
    ///
    /// lock 순서: `signal_state` write → drop → `position_lock` write.
    /// 두 락을 동시에 보유하지 않아 향후 다른 경로와 데드락 위험 없음.
    async fn abort_entry(&self, signal_time: NaiveTime, advance_search: bool, reason: &str) {
        if advance_search {
            let mut ss = self.signal_state.write().await;
            let new_after = ss.search_after.map_or(signal_time, |t| t.max(signal_time));
            ss.search_after = Some(new_after);
        }
        if let Some(ref lock) = self.position_lock {
            *lock.write().await = PositionLockState::Free;
        }
        info!(
            "{}: 진입 중단 ({}) — advance_search={}",
            self.stock_name, reason, advance_search
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "strategy",
                "abort_entry",
                "info",
                reason,
                serde_json::json!({
                    "signal_time": signal_time.to_string(),
                    "advance_search": advance_search,
                }),
            );
        }
    }

    /// 반복 발주 자동 안전장치.
    ///
    /// 최근 5분 창의 발주 이력을 유지하면서, 동일 가격(±0.1% tolerance)의 주문이
    /// 3회 이상 누적되면 `stop_flag` 를 세워 해당 종목 runner 를 자동 중단한다.
    /// 전체 봇을 중단하지 않아 dual-locked 다른 종목은 살린다.
    ///
    /// P0/P1 이 놓치는 미지 버그에 대한 최종 방어선.
    async fn track_order_and_guard_repeat(&self, limit_price: i64) {
        let now = Local::now().time();
        let mut queue = self.recent_orders.lock().await;

        let dup_count = update_order_queue_and_count_dup(&mut queue, now, limit_price);

        if dup_count >= 3 {
            error!(
                "{}: 동일가 {}원 5분 내 {}회 발주 감지 — runner 자동 중단",
                self.stock_name, limit_price, dup_count
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "strategy",
                    "repeated_order",
                    "critical",
                    &format!("동일가 {}원 {}회 반복 — runner stop", limit_price, dup_count),
                    serde_json::json!({
                        "price": limit_price,
                        "count": dup_count,
                    }),
                );
            }
            // 해당 러너만 중단. 전체 봇은 살아있어 다른 종목은 계속 운영.
            self.stop_flag.store(true, Ordering::SeqCst);
            self.stop_notify.notify_waiters();
        }
    }

    /// Multi-Stage ORB: 분봉 폴링 → 각 OR 단계별 FVG 탐색 → 선착순 진입
    async fn poll_and_enter(
        &self,
        require_or_breakout: bool,
        confirmed_side: Option<PositionSide>,
    ) -> Result<bool, KisError> {
        let (all_candles, realtime_candles) = self.fetch_candles_split().await?;
        if all_candles.is_empty() {
            warn!("{}: 분봉 데이터 비어 있음", self.stock_name);
            return Ok(false);
        }

        let cfg = &self.strategy.config;
        let now = Local::now().time();
        let today = Local::now().date_naive();

        // Multi-Stage OR 계산: 현재 시각에 따라 가용 단계 결정
        let or_stages_def = [
            ("5m",  cfg.or_start, NaiveTime::from_hms_opt(9, 5, 0).unwrap()),
            ("15m", cfg.or_start, NaiveTime::from_hms_opt(9, 15, 0).unwrap()),
            ("30m", cfg.or_start, NaiveTime::from_hms_opt(9, 30, 0).unwrap()),
        ];

        let mut available_stages: Vec<(&str, i64, i64, NaiveTime)> = Vec::new();

        for (stage_name, or_start, or_end) in &or_stages_def {
            if now < *or_end { continue; } // 아직 OR 수집 미완료

            // 캔들에서 OR 계산
            let or_candles: Vec<_> = all_candles.iter()
                .filter(|c| c.time >= *or_start && c.time < *or_end)
                .collect();

            let (h, l) = if !or_candles.is_empty() {
                let h = or_candles.iter().map(|c| c.high).max().unwrap();
                let l = or_candles.iter().map(|c| c.low).min().unwrap();
                // DB에 저장
                if let Some(ref store) = self.db_store {
                    if let Err(e) = store.save_or_range_stage(
                        self.stock_code.as_str(), today, h, l, "candle", stage_name
                    ).await {
                        warn!("{}: OR DB 저장 실패 (stage={}): {e}", self.stock_name, stage_name);
                    }
                }
                (h, l)
            } else {
                // DB에서 로드
                if let Some(ref store) = self.db_store {
                    if let Ok(Some((h, l))) = store.get_or_range_stage(
                        self.stock_code.as_str(), today, stage_name
                    ).await {
                        (h, l)
                    } else {
                        debug!("{}: OR {} 캔들 부족+DB 미저장 — 단계 스킵", self.stock_name, stage_name);
                        continue;
                    }
                } else { continue; }
            };

            available_stages.push((stage_name, h, l, *or_end));
        }

        if available_stages.is_empty() {
            warn!("{}: 가용 OR 단계 없음", self.stock_name);
            return Ok(false);
        }

        // 상태에 OR 정보 저장 (웹 표시용 — 첫 단계를 기본으로)
        {
            let mut state = self.state.write().await;
            state.or_high = Some(available_stages[0].1);
            state.or_low = Some(available_stages[0].2);
            state.or_stages = available_stages.iter()
                .map(|(name, h, l, _)| (name.to_string(), *h, *l))
                .collect();
        }

        // 각 OR 단계별 FVG 탐색 → 선착순 (가장 빠른 진입 시각)
        let mut best_signal: Option<BestSignal> = None;

        // 백테스트 sync_search_to 등가: 이전 청산 시각 이후의 캔들만 탐색.
        // 같은 5분봉의 같은 FVG 가 청산 후 재발견되는 것을 차단한다.
        let search_after = self.signal_state.read().await.search_after;

        for (stage_name, or_high, or_low, or_end) in &available_stages {
            let scan: Vec<_> = realtime_candles.iter()
                .filter(|c| c.time >= *or_end)
                .cloned()
                .collect();
            let candles_5m_full = candle::aggregate(&scan, 5);
            // 백테스트 BacktestEngine::next_valid_candidate 의 search_from 진행과 등가
            let candles_5m: Vec<_> = match search_after {
                Some(t) => candles_5m_full.into_iter().filter(|c| c.time > t).collect(),
                None => candles_5m_full,
            };
            if candles_5m.len() < 3 { continue; }

            // FVG 탐색 (백테스트 scan_and_trade 동일 로직)
            let mut pending_fvg: Option<FairValueGap> = None;
            let mut fvg_side: Option<PositionSide> = None;
            let mut fvg_formed_idx: usize = 0;

            for (idx, c5) in candles_5m.iter().enumerate() {
                if c5.time >= cfg.entry_cutoff { break; }

                if pending_fvg.is_some() && idx - fvg_formed_idx > cfg.fvg_expiry_candles {
                    pending_fvg = None;
                    fvg_side = None;
                }

                if pending_fvg.is_none() && idx >= 2 {
                    let a = &candles_5m[idx - 2];
                    let b = &candles_5m[idx - 1];
                    let c = c5;
                    let a_range = a.range().max(1);

                    if b.is_bullish() && a.high < c.low && b.body_size() * 100 >= a_range * 30 {
                        let or_ok = !require_or_breakout || b.close > *or_high;
                        let side_ok = confirmed_side.map_or(true, |s| s == PositionSide::Long);
                        if or_ok && side_ok {
                            pending_fvg = Some(FairValueGap {
                                direction: FvgDirection::Bullish,
                                top: c.low, bottom: a.high,
                                candle_b_idx: idx - 1, stop_loss: a.low,
                            });
                            fvg_side = Some(PositionSide::Long);
                            fvg_formed_idx = idx;
                        }
                    }
                }

                // 리트레이스 진입 확인
                if let Some(ref gap) = pending_fvg {
                    let side = fvg_side.unwrap();
                    let retrace = match side {
                        PositionSide::Long => c5.low,
                        PositionSide::Short => c5.high,
                    };

                    if gap.contains_price(retrace) {
                        // FVG zone 진입 즉시 체결되도록 가장 먼 경계로 지정가 설정.
                        // Bullish FVG (Long): 가격이 위→아래로 내려오므로 top에서 먼저 체결.
                        // Bearish FVG (Short): 가격이 아래→위로 올라오므로 bottom에서 먼저 체결.
                        // 백테스트는 zone 터치만으로 mid_price 체결을 가정하는데,
                        // 라이브 지정가를 top/bottom으로 하면 zone 진입 즉시 체결되어
                        // 백테스트의 "zone 터치 진입"과 구조적으로 일치한다.
                        let entry_price = match side {
                            PositionSide::Long => gap.top,
                            PositionSide::Short => gap.bottom,
                        };
                        let stop_loss = gap.stop_loss;
                        let risk = (entry_price - stop_loss).abs();
                        let take_profit = match side {
                            PositionSide::Long => entry_price + (risk as f64 * cfg.rr_ratio) as i64,
                            PositionSide::Short => entry_price - (risk as f64 * cfg.rr_ratio) as i64,
                        };

                        // 선착순: 이미 다른 단계가 신호를 냈으면 스킵
                        if best_signal.is_none() {
                            best_signal = Some(BestSignal {
                                side,
                                entry_price,
                                stop_loss,
                                take_profit,
                                stage_name,
                                signal_time: c5.time,
                            });
                        }
                        break; // 이 단계에서 첫 신호 찾으면 종료
                    }
                }
            }
        }

        // 선착순으로 선택된 신호로 진입
        if let Some(sig) = best_signal {
            let BestSignal { side, entry_price, stop_loss, take_profit, stage_name, signal_time } = sig;

            // 공유 포지션 잠금 (2단계: Pending → Held)
            if let Some(ref lock) = self.position_lock {
                let mut guard = lock.write().await;
                match &*guard {
                    PositionLockState::Held(code) => {
                        // 포지션 확정 보유 중 — 다른 종목 진입 차단
                        if code != self.stock_code.as_str() {
                            debug!("{}: 진입 차단 — {}가 포지션 확정 보유 중", self.stock_name, code);
                        }
                        return Ok(false);
                    }
                    PositionLockState::Pending { code, preempted } => {
                        if code == self.stock_code.as_str() {
                            // 같은 종목이 이미 대기 중 — 중복 발주 방지
                            return Ok(false);
                        }
                        // 다른 종목이 대기 중 → 선점 요청 (preempted=true)
                        if !preempted {
                            info!("{}: {} 미체결 대기 중 — 선점 요청", self.stock_name, code);
                            *guard = PositionLockState::Pending { code: code.clone(), preempted: true };
                        }
                        // 이번 사이클은 진입하지 않음 — 다음 사이클에서 Free 확인 후 진입
                        return Ok(false);
                    }
                    PositionLockState::Free => {
                        // Pending으로 잠금 선점 (execute_entry 전에 설정)
                        *guard = PositionLockState::Pending {
                            code: self.stock_code.as_str().to_string(),
                            preempted: false,
                        };
                    }
                }
                drop(guard);
            }

            // VI 발동 중이면 진입 차단 (잠금 해제 필요)
            if self.state.read().await.market_halted {
                warn!("{}: VI 발동 중 — 진입 보류", self.stock_name);
                // FVG 자체는 유효 — search_after 전진 금지
                self.abort_entry(signal_time, false, "vi_halted").await;
                return Ok(false);
            }

            // P1: FVG zone 이탈 가드 (Day+0 배포 시 max_entry_drift_pct=None 이면 스킵)
            if let Some(threshold) = self.live_cfg.max_entry_drift_pct {
                match self.get_current_price().await {
                    Ok(cur) => {
                        let drift = match side {
                            PositionSide::Long => (cur - entry_price) as f64 / entry_price as f64,
                            PositionSide::Short => (entry_price - cur) as f64 / entry_price as f64,
                        };
                        if drift > threshold {
                            warn!(
                                "{}: FVG zone 이탈 — current={}, entry={}, drift={:.3}% (한계 {:.3}%)",
                                self.stock_name, cur, entry_price, drift * 100.0, threshold * 100.0
                            );
                            if let Some(ref el) = self.event_logger {
                                el.log_event(
                                    self.stock_code.as_str(), "strategy", "drift_rejected", "warn",
                                    &format!("drift={:.3}% > {:.3}%", drift * 100.0, threshold * 100.0),
                                    serde_json::json!({
                                        "current": cur, "entry": entry_price,
                                        "drift": drift, "threshold": threshold,
                                        "stage": stage_name,
                                    }),
                                );
                            }
                            self.abort_entry(signal_time, true, "drift_exceeded").await;
                            return Ok(false);
                        }
                    }
                    Err(e) => {
                        // fail-close: 현재가 모르면 지정가 발주 자체가 위험.
                        // 단, 일시 장애이므로 search_after 전진은 안 함.
                        warn!("{}: 현재가 조회 실패 — 진입 포기: {e}", self.stock_name);
                        self.abort_entry(signal_time, false, "price_fetch_failed").await;
                        return Ok(false);
                    }
                }
            }

            info!("{}: [{}] {:?} 진입 신호 — entry={}, SL={}, TP={}",
                self.stock_name, stage_name, side, entry_price, stop_loss, take_profit);
            if let Some(ref el) = self.event_logger {
                el.log_event(self.stock_code.as_str(), "strategy", "entry_signal", "info",
                    &format!("[{}] {:?} entry={}, SL={}, TP={}", stage_name, side, entry_price, stop_loss, take_profit),
                    serde_json::json!({"stage": stage_name, "side": format!("{:?}", side), "entry": entry_price, "sl": stop_loss, "tp": take_profit}));
            }

            // 주문 실행 (API 에러만 재시도, 미체결은 재시도 불필요)
            match self.execute_entry(side, entry_price, stop_loss, take_profit).await {
                Ok(_) => return Ok(true),
                Err(KisError::Preempted) => {
                    // 다른 종목 선점 요청으로 양보. FVG 자체는 유효 → search_after 전진 금지.
                    self.abort_entry(signal_time, false, "preempted").await;
                    return Ok(false);
                }
                Err(e) if e.is_retryable() => {
                    // API 에러(네트워크, 토큰 만료 등) — 1회 재시도
                    let delay = e.retry_delay_ms().max(500);
                    warn!("{}: API 에러, {}ms 후 재시도: {e}", self.stock_name, delay);
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    match self.execute_entry(side, entry_price, stop_loss, take_profit).await {
                        Ok(_) => return Ok(true),
                        Err(KisError::Preempted) => {
                            self.abort_entry(signal_time, false, "preempted_on_retry").await;
                            return Ok(false);
                        }
                        Err(e2) => {
                            error!("{}: 재시도 실패 — 잠금 해제: {e2}", self.stock_name);
                            // API 일시 장애 — search_after 전진 금지 (다음 사이클 자연 재시도)
                            self.abort_entry(signal_time, false, "api_error_retry_failed").await;
                            return Ok(false);
                        }
                    }
                }
                Err(e) => {
                    // 미체결 취소, 진입 마감 임박 등 — 이 FVG 는 포기 (search_after 전진)
                    info!("{}: 진입 미성사 — 잠금 해제: {e}", self.stock_name);
                    self.abort_entry(signal_time, true, "fill_timeout_or_cutoff").await;
                    return Ok(false);
                }
            }
        }

        Ok(false)
    }

    /// 포지션 실시간 관리 — 백테스트 simulate_exit 동일 로직
    ///
    /// 청산 우선순위 (백테스트와 동일):
    /// 1. TP 지정가 체결 (KRX 거래소 우선, 레이턴시 0)
    /// 2. best_price 갱신 + 1R 본전스탑 + 트레일링 SL
    /// 3. SL 도달 → 서버 측 시장가 청산
    /// 4. 시간스탑 / 장마감
    async fn manage_position(&self) -> Result<Option<TradeResult>, KisError> {
        // stop 토글 시 즉시 청산
        if self.is_stopped() {
            let result = self.close_position_market(ExitReason::EndOfDay).await?;
            return Ok(Some(result));
        }

        let current = self.get_current_price().await?;
        let now = Local::now().time();
        let cfg = &self.strategy.config;

        let mut state = self.state.write().await;
        let pos = match state.current_position.as_mut() {
            Some(p) => p,
            None => return Ok(None),
        };

        let risk = (pos.entry_price - pos.original_sl).abs() as f64;
        let breakeven_dist = (risk * cfg.breakeven_r) as i64;
        let trailing_dist = (risk * cfg.trailing_r) as i64;

        // ── Step 1: TP 지정가 체결 체크 (거래소 우선, SL보다 먼저) ──
        let tp_hit = match pos.side {
            PositionSide::Long => current >= pos.take_profit,
            PositionSide::Short => current <= pos.take_profit,
        };
        if tp_hit {
            if let Some(ref tp_no) = pos.tp_order_no {
                // TP 지정가 체결 확인 (내부에서 폴링 재시도)
                let tp_no = tp_no.clone();
                let tp_price = pos.take_profit;
                let expected_qty = pos.quantity;
                drop(state);

                if let Some((fill_price, filled_qty)) = self.fetch_fill_detail(&tp_no).await {
                    info!("{}: TP 지정가 체결 확인 — {}원, {}주", self.stock_name, fill_price, filled_qty);
                    if let Some(ref el) = self.event_logger {
                        el.log_event(self.stock_code.as_str(), "position", "exit_tp", "info",
                            &format!("TP 체결 — {}원, {}주", fill_price, filled_qty),
                            serde_json::json!({"fill_price": fill_price, "tp_price": tp_price, "filled_qty": filled_qty, "expected_qty": expected_qty}));
                    }

                    // 부분 체결: 잔여분 시장가 청산
                    if filled_qty > 0 && filled_qty < expected_qty {
                        let remaining = expected_qty - filled_qty;
                        warn!("{}: TP 부분 체결 — {}주 중 {}주 체결, 잔여 {}주 시장가 청산",
                            self.stock_name, expected_qty, filled_qty, remaining);
                        // TP 주문 취소 (잔여분)
                        if let Some(ref orgno) = {
                            let s = self.state.read().await;
                            s.current_position.as_ref().and_then(|p| p.tp_krx_orgno.clone())
                        } {
                            let _ = self.cancel_tp_order(&tp_no, orgno).await;
                        }
                        // 잔여분 시장가 청산
                        if let Err(e) = self.place_market_sell(remaining).await {
                            error!("{}: 잔여 {}주 시장가 청산 실패 — 수동 확인 필요: {e}",
                                self.stock_name, remaining);
                        }
                    }

                    let result = {
                        let mut state = self.state.write().await;
                        let pos = state.current_position.take().unwrap();
                        let result = TradeResult {
                            side: pos.side,
                            entry_price: pos.entry_price,
                            exit_price: fill_price,
                            stop_loss: pos.stop_loss,
                            take_profit: tp_price,
                            entry_time: pos.entry_time,
                            exit_time: Local::now().time(),
                            exit_reason: ExitReason::TakeProfit,
                            quantity: pos.quantity,
                            intended_entry_price: pos.intended_entry_price,
                            order_to_fill_ms: pos.order_to_fill_ms,
                        };
                        state.current_position = None;
                        state.phase = "신호 탐색".to_string();
                        result
                    }; // state lock 해제
                    self.notify_trade("exit");
                    // DB 활성 포지션 삭제
                    if let Some(ref store) = self.db_store {
                        let _ = store.delete_active_position(self.stock_code.as_str()).await;
                    }
                    // 공유 포지션 잠금 해제 (state lock 해제 후)
                    if let Some(ref lock) = self.position_lock {
                        *lock.write().await = PositionLockState::Free;
                        info!("{}: 포지션 잠금 해제 (TP 체결)", self.stock_name);
                    }
                    return Ok(Some(result));
                }
                // 체결 미확인 — 다음 루프에서 재확인 (체결 지연 가능)
                debug!("{}: TP 체결 미확인 (지연 대기)", self.stock_name);
            } else {
                // TP 지정가 없음 — 시장가 fallback
                drop(state);
                let result = self.close_position_market(ExitReason::TakeProfit).await?;
                return Ok(Some(result));
            }
            return Ok(None);
        }

        // ── Step 2: best_price 갱신 + 1R 본전스탑 + 트레일링 (백테스트 동일) ──
        pos.best_price = match pos.side {
            PositionSide::Long => pos.best_price.max(current),
            PositionSide::Short => pos.best_price.min(current),
        };

        let profit_from_entry = match pos.side {
            PositionSide::Long => pos.best_price - pos.entry_price,
            PositionSide::Short => pos.entry_price - pos.best_price,
        };

        let mut trailing_changed = false;
        if !pos.reached_1r && profit_from_entry >= breakeven_dist {
            pos.reached_1r = true;
            pos.stop_loss = pos.entry_price; // 본전스탑 (백테스트 동일)
            info!("{}: 1R 도달 — 본전 스탑 활성화 (SL → {})", self.stock_name, pos.stop_loss);
            if let Some(ref el) = self.event_logger {
                el.log_event(self.stock_code.as_str(), "position", "breakeven_activated", "info",
                    &format!("1R 도달 — SL → {}", pos.stop_loss),
                    serde_json::json!({"price": current, "new_sl": pos.stop_loss, "best_price": pos.best_price}));
            }
            trailing_changed = true;
        }

        if pos.reached_1r {
            let new_sl = match pos.side {
                PositionSide::Long => pos.best_price - trailing_dist,
                PositionSide::Short => pos.best_price + trailing_dist,
            };
            let improved = match pos.side {
                PositionSide::Long => new_sl > pos.stop_loss,
                PositionSide::Short => new_sl < pos.stop_loss,
            };
            if improved {
                debug!("{}: 트레일링 SL 갱신 {} → {}", self.stock_name, pos.stop_loss, new_sl);
                pos.stop_loss = new_sl;
                trailing_changed = true;
            }
        }

        // 트레일링 상태 변경 시 DB에 저장 (재시작 복구용)
        if trailing_changed {
            if let Some(ref store) = self.db_store {
                let code = self.stock_code.as_str().to_string();
                let sl = pos.stop_loss;
                let r1r = pos.reached_1r;
                let bp = pos.best_price;
                let store = Arc::clone(store);
                // 비동기 저장 (메인 루프 블로킹 방지)
                tokio::spawn(async move {
                    let _ = store.update_position_trailing(&code, sl, r1r, bp).await;
                });
            }
        }

        // ── Step 3: SL 체크 ──
        // WS 틱 리스너가 이미 감지한 경우(`sl_triggered_tick=true`)를 우선 존중하고,
        // 그 외에는 REST 폴백/최신 캐시 `current` 기준으로 한 번 더 판정(안전망).
        // 백테스트 Step 3과 동일하게 갱신된 `pos.stop_loss` 기준으로 비교.
        let sl_hit = pos.sl_triggered_tick
            || match pos.side {
                PositionSide::Long => current <= pos.stop_loss,
                PositionSide::Short => current >= pos.stop_loss,
            };
        if sl_hit {
            let reason = if pos.reached_1r && pos.stop_loss > pos.original_sl {
                if pos.stop_loss == pos.entry_price {
                    ExitReason::BreakevenStop
                } else {
                    ExitReason::TrailingStop
                }
            } else {
                ExitReason::StopLoss
            };
            drop(state);
            let result = self.close_position_market(reason).await?;
            return Ok(Some(result));
        }

        // ── Step 4: 시간스탑 (백테스트 동일: !reached_1r && 경과시간 초과) ──
        let elapsed_min = (now - pos.entry_time).num_minutes();
        let time_limit = cfg.time_stop_candles as i64 * 5;
        if !pos.reached_1r && elapsed_min >= time_limit {
            drop(state);
            let result = self.close_position_market(ExitReason::TimeStop).await?;
            return Ok(Some(result));
        }

        Ok(None)
    }

    /// 지정가 진입 주문 (FVG mid_price 지정가 → 슬리피지 0)
    async fn execute_entry(
        &self,
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        _take_profit: i64,
    ) -> Result<(), KisError> {
        let order_side = match side {
            PositionSide::Long => OrderSide::Buy,
            PositionSide::Short => OrderSide::Sell,
        };

        // ── 시간 경계 사전 체크 ──
        // 지정가 대기(최대 30초) 고려하여 15:19:30 이후 진입 포기
        let now = Local::now().time();
        if now >= NaiveTime::from_hms_opt(15, 19, 30).unwrap() {
            warn!("{}: 진입 마감 30초 전 — 지정가 대기 불가, 진입 포기", self.stock_name);
            return Err(KisError::Internal("진입 마감 임박".into()));
        }

        // 지정가 = FVG mid_price를 호가단위로 정렬
        let limit_price = round_to_etf_tick(entry_price);

        // 주문 직전 매수가능수량 실시간 조회 (가용금액 변동 반영)
        let actual_qty = {
            let price_str = limit_price.to_string();
            let buyable_query = [
                ("CANO", self.client.account_no()),
                ("ACNT_PRDT_CD", self.client.account_product_code()),
                ("PDNO", self.stock_code.as_str()),
                ("ORD_UNPR", price_str.as_str()),
                ("ORD_DVSN", "00"),
                ("CMA_EVLU_AMT_ICLD_YN", "N"),
                ("OVRS_ICLD_YN", "N"),
            ];
            use crate::domain::models::account::BuyableInfo;
            let resp: Result<KisResponse<BuyableInfo>, _> = self.client
                .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/trading/inquire-psbl-order",
                    &TransactionId::InquireBuyable, Some(&buyable_query), None)
                .await;
            match resp {
                Ok(r) => match r.into_result() {
                    Ok(info) if info.orderable_qty() > 1 => {
                        let api_qty = info.orderable_qty();
                        let qty = (api_qty - 1) as u64; // 1주 여유 (슬리피지 대비)
                        info!("{}: 매수가능조회 상세 — ord_psbl_cash={}, nrcvb_buy_amt={}, nrcvb_buy_qty={}, max_buy_amt={}, max_buy_qty={}",
                            self.stock_name, info.ord_psbl_cash, info.nrcvb_buy_amt, info.nrcvb_buy_qty, info.max_buy_amt, info.max_buy_qty);
                        info!("{}: 주문 직전 매수가능 {}주 (API {}주 - 1)",
                            self.stock_name, qty, api_qty);
                        qty
                    }
                    Ok(info) => {
                        warn!("{}: 매수가능수량 부족 ({}주)", self.stock_name, info.orderable_qty());
                        0
                    }
                    Err(e) => {
                        warn!("{}: 매수가능조회 파싱 실패: {e} — 잔고 기반 fallback (증거금율 미반영, 80%)", self.stock_name);
                        let raw = self.get_available_cash().await;
                        let available = (raw as f64 * 0.80) as i64;
                        if available > 0 && entry_price > 0 {
                            let qty = (available / entry_price) as u64;
                            info!("{}: 잔고 기반 수량 {}주 (가용 {}원의 80%={}원)", self.stock_name, qty, raw, available);
                            qty
                        } else {
                            0
                        }
                    }
                },
                Err(e) => {
                    warn!("{}: 매수가능조회 실패: {e} — 잔고 기반 fallback (증거금율 미반영, 80%)", self.stock_name);
                    let raw = self.get_available_cash().await;
                    let available = (raw as f64 * 0.80) as i64;
                    if available > 0 && entry_price > 0 {
                        let qty = (available / entry_price) as u64;
                        info!("{}: 잔고 기반 수량 {}주 (가용 {}원의 80%={}원)", self.stock_name, qty, raw, available);
                        qty
                    } else {
                        0
                    }
                }
            }
        };

        if actual_qty == 0 {
            return Err(KisError::InsufficientBalance("매수가능수량 0주".into()));
        }

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "00",
            "ORD_QTY": actual_qty.to_string(),
            "ORD_UNPR": limit_price.to_string(),
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let order_start = tokio::time::Instant::now();
        let resp: KisResponse<serde_json::Value> = self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash", &tr_id, None, Some(&body))
            .await?;

        let side_str = format!("{:?}", order_side);
        if resp.rt_cd != "0" {
            // 주문 실패 로그
            if let Some(ref store) = self.db_store {
                store.save_order_log(
                    self.stock_code.as_str(), "진입", &side_str,
                    actual_qty as i64, limit_price, "", "실패", &resp.msg1,
                ).await;
            }
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        // 주문번호 + KRX 거래소번호 추출 (취소 시 필요)
        let order_no = resp.output.as_ref()
            .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();
        let krx_orgno = resp.output.as_ref()
            .and_then(|v| v.get("KRX_FWDG_ORD_ORGNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();

        info!("{}: 지정가 매수 발주 완료 — {}원 {}주, 주문번호={}",
            self.stock_name, limit_price, actual_qty, order_no);
        if let Some(ref store) = self.db_store {
            store.save_order_log(
                self.stock_code.as_str(), "진입", &side_str,
                actual_qty as i64, limit_price, &order_no, "발주", "",
            ).await;
        }

        // 반복 발주 자동 안전장치: 5분 창 안에서 동일가 ±0.1% 주문이 3회 이상 발생하면
        // 해당 종목 runner 자동 중단 (P0/P1 우회 버그 최종 방어선).
        // 2026-04-14 사고(동일 94465 지정가 282회 발주) 재발 방지.
        self.track_order_and_guard_repeat(limit_price).await;

        // ── 체결 대기 루프 (30초, WebSocket 우선 + REST 폴백) ──
        let fill_timeout = std::time::Duration::from_secs(30);
        let entry_deadline = NaiveTime::from_hms_opt(15, 20, 0).unwrap();
        let mut filled_qty: u64 = 0;
        let mut fill_price: i64 = 0;

        let mut was_preempted = false;
        loop {
            if self.is_stopped() { break; }
            if order_start.elapsed() >= fill_timeout { break; }
            if Local::now().time() >= entry_deadline {
                warn!("{}: 대기 중 진입 마감(15:20) 도달 — 미체결 취소", self.stock_name);
                break;
            }
            // 선점 체크 — 다른 종목이 진입 요청 시 즉시 양보
            if let Some(ref lock) = self.position_lock {
                let state = lock.read().await;
                if let PositionLockState::Pending { preempted: true, .. } = &*state {
                    warn!("{}: 다른 종목 선점 요청 감지 — 미체결 취소", self.stock_name);
                    was_preempted = true;
                    break;
                }
            }

            let remaining = fill_timeout.saturating_sub(order_start.elapsed());
            let wait_dur = remaining.min(std::time::Duration::from_secs(5));

            // WebSocket 체결 통보 대기 (최대 5초 단위)
            if let Some((price, qty)) = self.wait_execution_notice(&order_no, wait_dur).await {
                fill_price = price;
                filled_qty = qty;
                if filled_qty >= actual_qty { break; }
            }

            // REST 1회 조회 (WebSocket 미수신 보완)
            if filled_qty < actual_qty {
                if let Some((price, qty)) = self.query_execution(&order_no).await {
                    fill_price = price;
                    filled_qty = qty;
                    if filled_qty >= actual_qty { break; }
                }
            }
        }

        // ── 미체결/부분 체결 처리 ──
        if filled_qty < actual_qty {
            let was_partial = filled_qty > 0;
            if was_partial {
                info!("{}: 부분 체결 {}주/{}주 — 잔량 취소 시도", self.stock_name, filled_qty, actual_qty);
            } else {
                info!("{}: 미체결 — 주문 취소 시도 (주문번호={})", self.stock_name, order_no);
            }

            // 미체결 잔량 취소 (cancel_tp_order는 범용 취소 — 동일 API).
            // cancel 3회 재시도 모두 실패 시 cancel_all fallback으로 이 종목의 모든 미체결 정리.
            // 이 지점은 체결 확정 전 + TP 발주 전이라 이 종목의 정상 TP는 존재하지 않음 → 안전.
            if let Err(e) = self.cancel_tp_order(&order_no, &krx_orgno).await {
                warn!("{}: 주문 취소 3회 실패 — cancel_all fallback: {e}", self.stock_name);
                if let Some(ref el) = self.event_logger {
                    el.log_event(self.stock_code.as_str(), "order", "cancel_fallback", "warn",
                        &format!("주문 {} cancel 실패 → cancel_all_pending_orders 실행", order_no),
                        serde_json::json!({"order_no": &order_no}));
                }
                self.cancel_all_pending_orders().await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            // 취소 직후 체결 재조회 (레이스 컨디션 방지: 취소 직전 체결 가능)
            if let Some((price, qty)) = self.query_execution(&order_no).await {
                fill_price = price;
                filled_qty = qty;
            }

            if filled_qty == 0 {
                // 최종 미체결 확정
                let cancel_reason = if was_preempted { "선점으로 미체결 취소" } else { "지정가 매수 미체결 확정" };
                info!("{}: {} — 주문 {} 취소 완료", self.stock_name, cancel_reason, order_no);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "진입", &side_str,
                        actual_qty as i64, limit_price, &order_no, "미체결취소", "",
                    ).await;
                }
                if let Some(ref el) = self.event_logger {
                    el.log_event(self.stock_code.as_str(), "order", "entry_cancelled", "info",
                        &format!("{} — {}원 {}주", cancel_reason, limit_price, actual_qty),
                        serde_json::json!({
                            "order_no": &order_no, "limit_price": limit_price,
                            "quantity": actual_qty, "preempted": was_preempted,
                        }));
                }
                // preempt 는 FVG 자체 무효가 아니므로 전용 variant 로 반환
                // (poll_and_enter 의 `abort_entry(advance_search=false)` 경로로 수렴)
                if was_preempted {
                    return Err(KisError::Preempted);
                }
                return Err(KisError::Internal("지정가 매수 미체결".into()));
            }
        }

        // ── 체결 확정 (전량 또는 부분) ──
        let actual_qty = filled_qty; // 최종 체결 수량으로 갱신
        // 지정가 체결가 = 주문가 (슬리피지 0). fill_price가 0이면 주문가 fallback.
        let actual_entry = if fill_price > 0 { fill_price } else { limit_price };
        let order_to_fill_ms = order_start.elapsed().as_millis() as i64;

        // 주문 체결 로그
        if let Some(ref store) = self.db_store {
            store.save_order_log(
                self.stock_code.as_str(), "진입", &side_str,
                actual_qty as i64, actual_entry, &order_no, "체결", "",
            ).await;
        }

        info!("{}: {:?} {}주 지정가 진입 완료 (지정가={}, 체결가={})",
            self.stock_name, order_side, actual_qty, limit_price, actual_entry);
        self.notify_trade("entry");

        // Pending → Held 전환 (체결 확정 — 선점 요청이 있어도 체결이 우선)
        if let Some(ref lock) = self.position_lock {
            *lock.write().await = PositionLockState::Held(self.stock_code.as_str().to_string());
        }

        // 실제 체결가 기준으로 SL/TP 재계산.
        // R(=theoretical risk)은 (이론 entry - 이론 SL)로 유지하고, actual entry에 평행 이동.
        let theoretical_risk = (entry_price - stop_loss).abs();
        let rr = self.strategy.config.rr_ratio;
        let (actual_sl, actual_tp) = match side {
            PositionSide::Long => (
                actual_entry - theoretical_risk,
                actual_entry + (theoretical_risk as f64 * rr) as i64,
            ),
            PositionSide::Short => (
                actual_entry + theoretical_risk,
                actual_entry - (theoretical_risk as f64 * rr) as i64,
            ),
        };
        let slippage = actual_entry - entry_price;
        info!("{}: SL/TP 재계산 — actual entry {}원, R={}원, SL={}원, TP={}원, 슬리피지={}원",
            self.stock_name, actual_entry, theoretical_risk, actual_sl, actual_tp, slippage);
        if let Some(ref el) = self.event_logger {
            el.log_event(self.stock_code.as_str(), "order", "entry_executed", "info",
                &format!("{:?} {}주 지정가 진입 (지정가={}, 체결가={}, 슬리피지={})", side, actual_qty, limit_price, actual_entry, slippage),
                serde_json::json!({
                    "side": format!("{:?}", side),
                    "quantity": actual_qty,
                    "limit_price": limit_price,
                    "actual_price": actual_entry,
                    "slippage": slippage,
                    "stop_loss": actual_sl,
                    "take_profit": actual_tp,
                    "order_no": order_no,
                }));
        }

        // TP 지정가 매도 발주 (실패 시 시장가 fallback)
        let (tp_order_no, tp_krx_orgno, tp_limit_price) =
            if let Some((no, orgno, placed_price)) = self.place_tp_limit_order(actual_tp, actual_qty).await {
                (Some(no), Some(orgno), Some(placed_price))
            } else {
                warn!("{}: TP 지정가 발주 실패 — 시장가 fallback 모드", self.stock_name);
                (None, None, None)
            };

        // ActivePosition을 write lock 안에서 만들어 두고 lock 해제 후 DB 저장.
        // 같은 task에서 read+write를 동시에 잡으면 tokio RwLock에서 deadlock 발생.
        let ap_to_save = {
            let mut state = self.state.write().await;
            state.current_position = Some(Position {
                side,
                entry_price: actual_entry,
                stop_loss: actual_sl,
                take_profit: actual_tp,
                entry_time: Local::now().time(),
                quantity: actual_qty,
                tp_order_no: tp_order_no.clone(),
                tp_krx_orgno: tp_krx_orgno.clone(),
                tp_limit_price,
                reached_1r: false,
                best_price: actual_entry,
                original_sl: actual_sl,
                sl_triggered_tick: false,
                intended_entry_price: entry_price,
                order_to_fill_ms,
            });
            state.phase = "포지션 보유".to_string();

            self.db_store.as_ref().map(|_| ActivePosition {
                stock_code: self.stock_code.as_str().to_string(),
                side: format!("{:?}", side),
                entry_price: actual_entry,
                stop_loss: actual_sl,
                take_profit: actual_tp,
                quantity: actual_qty as i64,
                tp_order_no: tp_order_no.clone().unwrap_or_default(),
                tp_krx_orgno: tp_krx_orgno.clone().unwrap_or_default(),
                entry_time: Local::now().naive_local(),
                original_sl: actual_sl,
                reached_1r: false,
                best_price: actual_entry,
            })
        }; // write lock dropped here

        // DB save (lock 해제 후)
        if let (Some(store), Some(ap)) = (self.db_store.as_ref(), ap_to_save) {
            if let Err(e) = store.save_active_position(&ap).await {
                error!("{}: 활성 포지션 DB 저장 실패 — 재시작 시 복구 불가: {e}", self.stock_name);
            } else {
                info!("{}: 활성 포지션 DB 저장 완료", self.stock_name);
            }
        }

        // 포지션 잠금은 poll_and_enter에서 CAS로 이미 설정됨

        Ok(())
    }

    /// 시장가 청산 주문 (TP 지정가가 걸려있으면 먼저 취소)
    async fn close_position_market(&self, reason: ExitReason) -> Result<TradeResult, KisError> {
        let mut state = self.state.write().await;
        let pos = state.current_position.take()
            .ok_or_else(|| KisError::Internal("청산할 포지션 없음".into()))?;
        drop(state);

        // TP 지정가 취소 (실패해도 시장가 청산 진행)
        if let (Some(no), Some(orgno)) = (&pos.tp_order_no, &pos.tp_krx_orgno) {
            if let Err(e) = self.cancel_tp_order(no, orgno).await {
                warn!("{}: TP 취소 실패 (시장가 청산 진행): {e}", self.stock_name);
            }
        }

        let order_side = match pos.side {
            PositionSide::Long => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01",
            "ORD_QTY": pos.quantity.to_string(),
            "ORD_UNPR": "0",
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        // 청산 주문 실패 시 포지션을 반드시 복원 (이중 포지션 방지)
        let resp: KisResponse<serde_json::Value> = match self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash", &tr_id, None, Some(&body))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("{}: 청산 주문 네트워크 오류 — 포지션 복원: {e}", self.stock_name);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "청산", &format!("{:?}", order_side),
                        pos.quantity as i64, 0, "", "네트워크오류", &e.to_string(),
                    ).await;
                }
                self.state.write().await.current_position = Some(pos);
                return Err(e);
            }
        };

        if resp.rt_cd != "0" {
            error!("{}: 청산 주문 거부 — 포지션 복원: {}", self.stock_name, resp.msg1);
            if let Some(ref store) = self.db_store {
                store.save_order_log(
                    self.stock_code.as_str(), "청산", &format!("{:?}", order_side),
                    pos.quantity as i64, 0, "", "거부", &resp.msg1,
                ).await;
            }
            self.state.write().await.current_position = Some(pos);
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        // 주문번호 추출 → 실제 체결가 조회
        let order_no = resp.output.as_ref()
            .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();
        let ws_price = self.get_current_price().await.unwrap_or(pos.entry_price);
        let exit_price = match self.fetch_fill_price(&order_no).await {
            Some(price) => price,
            None => {
                error!("{}: 청산 체결가 미확인 — 현재가 {}원으로 fallback", self.stock_name, ws_price);
                ws_price
            }
        };
        let exit_time = Local::now().time();

        // 청산 성공 로그
        if let Some(ref store) = self.db_store {
            store.save_order_log(
                self.stock_code.as_str(), &format!("청산({:?})", reason), &format!("{:?}", order_side),
                pos.quantity as i64, exit_price, &order_no, "체결", "",
            ).await;
        }

        info!("{}: {:?} 청산 — {}주 @ {} ({:?}, WS={})", self.stock_name, pos.side, pos.quantity, exit_price, reason, ws_price);
        if let Some(ref el) = self.event_logger {
            let pnl_pct = match pos.side {
                PositionSide::Long => (exit_price - pos.entry_price) as f64 / pos.entry_price as f64 * 100.0,
                PositionSide::Short => (pos.entry_price - exit_price) as f64 / pos.entry_price as f64 * 100.0,
            };
            el.log_event(self.stock_code.as_str(), "position", "exit_market", "info",
                &format!("{:?} 청산 — {}주 @ {} ({:?})", pos.side, pos.quantity, exit_price, reason),
                serde_json::json!({
                    "reason": format!("{:?}", reason),
                    "entry_price": pos.entry_price,
                    "exit_price": exit_price,
                    "quantity": pos.quantity,
                    "pnl_pct": format!("{:.2}", pnl_pct),
                    "stop_loss": pos.stop_loss,
                    "take_profit": pos.take_profit,
                }));
        }
        self.notify_trade("exit");

        let result = TradeResult {
            side: pos.side,
            entry_price: pos.entry_price,
            exit_price,
            stop_loss: pos.stop_loss,
            take_profit: pos.take_profit,
            entry_time: pos.entry_time,
            exit_time,
            exit_reason: reason,
            quantity: pos.quantity,
            intended_entry_price: pos.intended_entry_price,
            order_to_fill_ms: pos.order_to_fill_ms,
        };

        {
            let mut state = self.state.write().await;
            state.current_position = None;
            state.phase = "신호 탐색".to_string();
        } // state lock 해제

        // DB 활성 포지션 삭제
        if let Some(ref store) = self.db_store {
            let _ = store.delete_active_position(self.stock_code.as_str()).await;
        }

        // 공유 포지션 잠금 해제 (state lock 해제 후)
        if let Some(ref lock) = self.position_lock {
            *lock.write().await = PositionLockState::Free;
            info!("{}: 포지션 잠금 해제 (다른 종목 진입 허용)", self.stock_name);
        }

        Ok(result)
    }

    // ── 헬퍼 메서드 ──

    async fn update_phase(&self, phase: &str) {
        let prev = self.state.read().await.phase.clone();
        self.state.write().await.phase = phase.to_string();
        if let Some(ref el) = self.event_logger {
            el.log_event(self.stock_code.as_str(), "strategy", "phase_change", "info",
                &format!("{} → {}", prev, phase),
                serde_json::json!({"from": prev, "to": phase}));
        }
    }

    async fn update_pnl(&self, trades: &[TradeResult]) {
        let mut state = self.state.write().await;
        state.today_trades = trades.to_vec();
        state.today_pnl = trades.iter().map(|t| t.pnl_pct()).sum();
    }

    fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::Relaxed)
    }

    /// 거래 결과를 DB에 저장
    async fn save_trade_to_db(&self, result: &TradeResult) {
        let Some(ref store) = self.db_store else { return };
        let today = Local::now().date_naive();
        // result.quantity가 0이면 (안전망) self.quantity로 폴백
        let qty = if result.quantity > 0 { result.quantity } else { self.quantity };
        let record = TradeRecord {
            stock_code: self.stock_code.as_str().to_string(),
            stock_name: self.stock_name.clone(),
            side: format!("{:?}", result.side),
            quantity: qty as i64,
            entry_price: result.entry_price,
            exit_price: result.exit_price,
            stop_loss: result.stop_loss,
            take_profit: result.take_profit,
            entry_time: today.and_time(result.entry_time),
            exit_time: today.and_time(result.exit_time),
            exit_reason: format!("{:?}", result.exit_reason),
            pnl_pct: result.pnl_pct(),
            strategy: "orb_fvg".to_string(),
            environment: "paper".to_string(),
            intended_entry_price: result.intended_entry_price,
            entry_slippage: result.entry_price - result.intended_entry_price,
            exit_slippage: 0, // TP 지정가는 슬리피지 0, SL 시장가는 별도 측정 어려움
            order_to_fill_ms: result.order_to_fill_ms,
        };
        if let Err(e) = store.save_trade(&record).await {
            error!("{}: 거래 DB 저장 실패: {e}", self.stock_name);
        } else {
            info!("{}: 거래 DB 저장 완료 ({:?} {:.2}%)", self.stock_name, result.exit_reason, result.pnl_pct());
        }
    }

    /// 잔고 API로 D+2 가수도정산금액(prvs_rcdl_excc_amt) 조회.
    /// 이게 KIS HTS의 "D+2 예수금" = 실제 주문가능 cash와 동일.
    async fn get_available_cash(&self) -> i64 {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("AFHR_FLPR_YN", "N"), ("OFL_YN", ""), ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"), ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"), ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""), ("CTX_AREA_NK100", ""),
        ];
        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self.client
            .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance, Some(&query), None)
            .await;
        if let Ok(r) = resp {
            if let Some(output2) = r.output2 {
                if let Some(first) = output2.first() {
                    // prvs_rcdl_excc_amt = 가수도정산금액 (D+2 예수금)
                    // KIS HTS의 "D+2 예수금"과 동일하며 매도 대금 정산까지 포함됨.
                    let d2_cash: i64 = first.get("prvs_rcdl_excc_amt").and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok()).unwrap_or(0);
                    info!("{}: D+2 예수금(prvs_rcdl_excc_amt) = {}", self.stock_name, d2_cash);
                    return d2_cash;
                }
            }
        }
        0
    }

    /// 잔고 API로 해당 종목 실제 보유 여부 확인
    async fn check_balance_has_stock(&self) -> bool {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("AFHR_FLPR_YN", "N"), ("OFL_YN", ""), ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"), ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"), ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""), ("CTX_AREA_NK100", ""),
        ];
        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self.client
            .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance, Some(&query), None)
            .await;
        if let Ok(r) = resp {
            let items = r.output.or(r.output1).unwrap_or_default();
            for item in &items {
                let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
                let qty: u64 = item.get("hldg_qty").and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok()).unwrap_or(0);
                if code == self.stock_code.as_str() && qty > 0 {
                    return true;
                }
            }
        }
        false
    }

    async fn wait_until(&self, target: NaiveTime) {
        loop {
            if self.is_stopped() { return; }
            let now = Local::now().time();
            if now >= target { return; }
            let diff = (target - now).num_seconds().max(1) as u64;
            let sleep_secs = diff.min(10);
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)) => {}
                _ = self.stop_notify.notified() => {}
            }
        }
    }

    /// TP 지정가 매도 주문 발주.
    /// 가격은 KRX ETF 호가단위(2023 개편: 5,000원 미만 1원, 이상 5원)로 round.
    /// 반환: (주문번호, KRX 거래소번호, 실제 발주된 가격)
    async fn place_tp_limit_order(&self, price: i64, quantity: u64) -> Option<(String, String, i64)> {
        let rounded = round_to_etf_tick(price);
        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "00",
            "ORD_QTY": quantity.to_string(),
            "ORD_UNPR": rounded.to_string(),
        });

        let resp: Result<KisResponse<serde_json::Value>, _> = self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash",
                &TransactionId::OrderCashSell, None, Some(&body))
            .await;

        match resp {
            Ok(r) if r.rt_cd == "0" => {
                let order_no = r.output.as_ref()
                    .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
                    .unwrap_or("").to_string();
                let krx_orgno = r.output.as_ref()
                    .and_then(|v| v.get("KRX_FWDG_ORD_ORGNO").and_then(|o| o.as_str()))
                    .unwrap_or("").to_string();
                if rounded != price {
                    info!("{}: TP 지정가 발주 완료 — {}원(이론 {}원 호가단위 round), 주문번호={}",
                        self.stock_name, rounded, price, order_no);
                } else {
                    info!("{}: TP 지정가 발주 완료 — {}원, 주문번호={}", self.stock_name, rounded, order_no);
                }
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "TP지정가", "Sell",
                        quantity as i64, rounded, &order_no, "체결", "",
                    ).await;
                }
                Some((order_no, krx_orgno, rounded))
            }
            Ok(r) => {
                warn!("{}: TP 지정가 발주 실패 — {} (가격 {}원)", self.stock_name, r.msg1, rounded);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "TP지정가", "Sell",
                        quantity as i64, rounded, "", "실패", &r.msg1,
                    ).await;
                }
                None
            }
            Err(e) => {
                warn!("{}: TP 지정가 발주 에러 — {e}", self.stock_name);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "TP지정가", "Sell",
                        quantity as i64, rounded, "", "에러", &e.to_string(),
                    ).await;
                }
                None
            }
        }
    }

    /// TP 지정가 주문 취소 (3회 재시도)
    async fn cancel_tp_order(&self, order_no: &str, krx_orgno: &str) -> Result<(), KisError> {
        for attempt in 0..3u32 {
            let body = serde_json::json!({
                "CANO": self.client.account_no(),
                "ACNT_PRDT_CD": self.client.account_product_code(),
                "KRX_FWDG_ORD_ORGNO": krx_orgno,
                "ORGN_ODNO": order_no,
                "ORD_DVSN": "00",
                "RVSE_CNCL_DVSN_CD": "02",
                "ORD_QTY": "0",
                "ORD_UNPR": "0",
                "QTY_ALL_ORD_YN": "Y",
            });

            let resp: Result<KisResponse<serde_json::Value>, _> = self.client
                .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-rvsecncl",
                    &TransactionId::OrderCancel, None, Some(&body))
                .await;

            match resp {
                Ok(r) if r.rt_cd == "0" => {
                    info!("{}: TP 주문 취소 완료 — {}", self.stock_name, order_no);
                    return Ok(());
                }
                Ok(r) => {
                    warn!("{}: TP 취소 실패 (시도 {}/3) — {}", self.stock_name, attempt + 1, r.msg1);
                }
                Err(e) => {
                    warn!("{}: TP 취소 에러 (시도 {}/3) — {e}", self.stock_name, attempt + 1);
                }
            }

            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(500 * (1u64 << attempt))).await;
            }
        }
        Err(KisError::Internal(format!("TP 취소 3회 실패: {}", order_no)))
    }

    /// 잔여분 시장가 매도 (부분 체결 후 잔량 청산용)
    async fn place_market_sell(&self, qty: u64) -> Result<(), KisError> {
        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01",
            "ORD_QTY": qty.to_string(),
            "ORD_UNPR": "0",
        });

        let resp: KisResponse<serde_json::Value> = self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash",
                &TransactionId::OrderCashSell, None, Some(&body))
            .await?;

        if resp.rt_cd == "0" {
            info!("{}: 잔여 {}주 시장가 매도 완료", self.stock_name, qty);
            Ok(())
        } else {
            Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1))
        }
    }

    /// 주문 체결 상세 조회 — WS 체결통보 우선 (5초), REST 폴링 fallback (5회, 2초 간격)
    ///
    /// 시장가 주문의 실제 체결가를 반드시 확인해야 함.
    /// 체결가 조회 실패 시 이론가(FVG mid_price)로 fallback되면
    /// 실제 손익과 기록 손익이 괴리됨 (2026-04-13 사고 분석).
    async fn fetch_fill_detail(&self, order_no: &str) -> Option<(i64, u64)> {
        // 1차: WS 체결통보 대기 (최대 5초)
        if let Some(ref tx) = self.trade_tx {
            let mut rx = tx.subscribe();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() { break; }

                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Ok(RealtimeData::ExecutionNotice(notice))) => {
                        if notice.order_no == order_no && notice.is_filled && notice.filled_price > 0 {
                            info!("{}: WS 체결통보 수신 — 주문 {} → {}원 {}주",
                                self.stock_name, order_no, notice.filled_price, notice.filled_qty);
                            return Some((notice.filled_price, notice.filled_qty));
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => break,
                    _ => {}
                }
            }
            warn!("{}: WS 체결통보 5초 내 미수신 — REST fallback", self.stock_name);
        }

        // 2차: REST 폴링 (최대 5회, 2초 간격 = 최대 10초 추가)
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            if let Some((price, qty)) = self.query_execution(order_no).await {
                return Some((price, qty));
            }
            debug!("{}: REST 체결 조회 {}회차 미확인 — 주문 {}", self.stock_name, attempt + 1, order_no);
        }

        // 3차: 최종 대기 후 마지막 시도 (총 20초 후)
        warn!("{}: 체결 조회 5회 실패 — 5초 추가 대기 후 최종 시도", self.stock_name);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Some((price, qty)) = self.query_execution(order_no).await {
            info!("{}: 최종 시도 체결 확인 — 주문 {} → {}원 {}주", self.stock_name, order_no, price, qty);
            return Some((price, qty));
        }

        error!("{}: 체결 조회 최종 실패 — 주문 {} (실제 체결가 미확인, 이론가로 fallback 위험)", self.stock_name, order_no);
        if let Some(ref el) = self.event_logger {
            el.log_event(self.stock_code.as_str(), "system", "fill_query_failed", "error",
                &format!("체결 조회 실패 — 주문 {} (WS 5초 + REST 6회)", order_no),
                serde_json::json!({"order_no": order_no}));
        }
        None
    }

    /// 체결가 조회 편의 메서드 (기존 호출부 호환)
    async fn fetch_fill_price(&self, order_no: &str) -> Option<i64> {
        self.fetch_fill_detail(order_no).await.map(|(price, _)| price)
    }

    /// position_lock의 preempted 상태를 200ms 간격으로 폴링.
    /// preempted=true 감지 시 반환. position_lock이 None이면 영원히 대기(select!에서 다른 branch 양보).
    ///
    /// wait_execution_notice의 select! branch로 사용하여 선점 요청을 5초 → 200ms 이내에 감지.
    async fn wait_preempted(&self) {
        loop {
            if let Some(ref lock) = self.position_lock {
                let state = lock.read().await;
                if let PositionLockState::Pending { preempted: true, .. } = &*state {
                    return;
                }
            } else {
                // position_lock 없으면 preempt 불가 — 영원히 pending으로 다른 branch에 양보
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// WebSocket 체결 통보를 지정 시간만큼 대기. 체결 시 (가격, 수량) 반환.
    /// fetch_fill_detail()과 달리 REST 폴링 없이 순수 WS 수신만 수행하여
    /// 외부 루프에서 타임아웃을 제어할 수 있다.
    async fn wait_execution_notice(&self, order_no: &str, timeout: std::time::Duration) -> Option<(i64, u64)> {
        let Some(ref tx) = self.trade_tx else { return None };
        let mut rx = tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() { return None; }

            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(RealtimeData::ExecutionNotice(notice)) => {
                            if notice.order_no == order_no && notice.is_filled && notice.filled_price > 0 {
                                info!("{}: WS 체결통보 수신 — 주문 {} → {}원 {}주",
                                    self.stock_name, order_no, notice.filled_price, notice.filled_qty);
                                return Some((notice.filled_price, notice.filled_qty));
                            }
                        }
                        Err(_) => return None, // channel closed
                        _ => {} // 다른 RealtimeData variant — 무시하고 계속 대기
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return None; // 타임아웃
                }
                _ = self.stop_notify.notified() => {
                    return None; // 중지 신호
                }
                _ = self.wait_preempted() => {
                    // 선점 요청 감지 — 체결 대기를 즉시 중단 (감지 지연 최대 ~200ms)
                    return None;
                }
            }
        }
    }

    /// 체결 내역 API 1회 조회 — (평균가, 체결수량) 반환
    async fn query_execution(&self, order_no: &str) -> Option<(i64, u64)> {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("INQR_STRT_DT", &Local::now().format("%Y%m%d").to_string()),
            ("INQR_END_DT", &Local::now().format("%Y%m%d").to_string()),
            ("SLL_BUY_DVSN_CD", "00"),
            ("INQR_DVSN", "00"),
            ("PDNO", ""),
            ("CCLD_DVSN", "01"),
            ("ORD_GNO_BRNO", ""),
            ("ODNO", order_no),
            ("INQR_DVSN_3", "00"),
            ("INQR_DVSN_1", ""),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];

        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self.client
            .execute(HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-daily-ccld",
                &TransactionId::InquireDailyExecution, Some(&query), None)
            .await;

        if let Ok(resp) = resp {
            if let Some(items) = resp.output {
                for item in &items {
                    let odno = item.get("odno").and_then(|v| v.as_str()).unwrap_or("");
                    if odno == order_no {
                        let avg_str = item.get("avg_prvs").and_then(|v| v.as_str()).unwrap_or("0");
                        let avg: f64 = avg_str.parse().unwrap_or(0.0);
                        let filled_qty: u64 = item.get("tot_ccld_qty")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        if avg > 0.0 {
                            info!("{}: 체결 조회 — 주문 {} → 평균 {}원, {}주 체결",
                                self.stock_name, order_no, avg as i64, filled_qty);
                            return Some((avg as i64, filled_qty));
                        }
                    }
                }
            }
        }
        None
    }

    /// REST API로 현재가 조회 (fallback용)
    async fn fetch_current_price_rest(&self) -> Result<InquirePrice, KisError> {
        let query = [
            ("FID_COND_MRKT_DIV_CODE", "J"),
            ("FID_INPUT_ISCD", self.stock_code.as_str()),
        ];
        let resp: KisResponse<InquirePrice> = self.client
            .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/quotations/inquire-price",
                &TransactionId::InquirePrice, Some(&query), None)
            .await?;
        resp.into_result()
    }

    /// 분봉 조회 — 전체 + 실시간만 분리 반환
    async fn fetch_candles_split(&self) -> Result<(Vec<MinuteCandle>, Vec<MinuteCandle>), KisError> {
        let Some(ref ws) = self.ws_candles else {
            return Ok((Vec::new(), Vec::new()));
        };

        let store = ws.read().await;
        let completed = store.get(self.stock_code.as_str());
        let today = Local::now().date_naive();

        let mut all = Vec::new();
        let mut realtime_only = Vec::new();

        if let Some(bars) = completed {
            for c in bars {
                let parts: Vec<&str> = c.time.split(':').collect();
                let h: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let m: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let Some(time) = NaiveTime::from_hms_opt(h, m, 0) else { continue };
                let candle = MinuteCandle {
                    date: today, time,
                    open: c.open, high: c.high, low: c.low, close: c.close,
                    volume: c.volume,
                };
                all.push(candle.clone());
                if c.is_realtime {
                    realtime_only.push(candle);
                }
            }
        }

        Ok((all, realtime_only))
    }

    /// 분봉 조회 — WebSocket CandleAggregator 데이터 사용 (API 호출 없음)
    #[allow(dead_code)]
    async fn fetch_candles_cached(&self) -> Result<Vec<MinuteCandle>, KisError> {
        let Some(ref ws) = self.ws_candles else {
            return Ok(Vec::new());
        };

        let store = ws.read().await;
        let completed = store.get(self.stock_code.as_str());

        let today = Local::now().date_naive();
        let candles: Vec<MinuteCandle> = completed
            .map(|bars| {
                bars.iter()
                    .filter_map(|c| {
                        let parts: Vec<&str> = c.time.split(':').collect();
                        let h: u32 = parts.first()?.parse().ok()?;
                        let m: u32 = parts.get(1)?.parse().ok()?;
                        let time = NaiveTime::from_hms_opt(h, m, 0)?;
                        Some(MinuteCandle {
                            date: today,
                            time,
                            open: c.open,
                            high: c.high,
                            low: c.low,
                            close: c.close,
                            volume: c.volume,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(candles)
    }

    /// REST API 분봉 조회 (특정 시각부터)
    async fn fetch_minute_candles_from(&self, from_hour: &str) -> Result<Vec<MinuteCandle>, KisError> {
        let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
        let mut all_items = Vec::new();
        let mut fid_input_hour = from_hour.to_string();

        // 증분 조회이므로 최대 3페이지면 충분
        for page in 0..3 {
            let query = [
                ("FID_ETC_CLS_CODE", ""),
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", self.stock_code.as_str()),
                ("FID_INPUT_HOUR_1", &fid_input_hour),
                ("FID_PW_DATA_INCU_YN", "N"),
            ];

            let resp: KisResponse<Vec<MinutePriceItem>> = self.client
                .execute(HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-time-itemchartprice",
                    &TransactionId::InquireTimePrice, Some(&query), None)
                .await?;

            if resp.rt_cd != "0" {
                if page == 0 {
                    return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
                }
                break;
            }

            let has_next = resp.has_next();
            let items = resp.output.unwrap_or_default();
            if items.is_empty() { break; }

            if let Some(last) = items.last() {
                fid_input_hour = last.stck_cntg_hour.clone();
            }
            all_items.extend(items);

            if !has_next { break; }
            // 페이지 간 충분한 간격
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }

        let mut candles: Vec<MinuteCandle> = all_items.iter()
            .filter_map(|item| {
                let d = chrono::NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let t = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                if t < market_open || t > market_close { return None; }
                Some(MinuteCandle { date: d, time: t, open: item.stck_oprc, high: item.stck_hgpr,
                    low: item.stck_lwpr, close: item.stck_prpr, volume: item.cntg_vol as u64 })
            })
            .collect();

        candles.sort_by_key(|c| c.time);
        Ok(candles)
    }

    /// OR(Opening Range) 외부 수집: KIS API 분봉 → 네이버 분봉 순으로 시도
    /// 09:00~09:15 범위의 high/low를 반환
    async fn fetch_or_from_external(&self, or_start: NaiveTime, or_end: NaiveTime) -> Option<(i64, i64)> {
        // 1차: KIS API 분봉 조회 (OHLCV 정확)
        match self.fetch_minute_candles_from("091500").await {
            Ok(candles) if !candles.is_empty() => {
                let or_candles: Vec<_> = candles.iter()
                    .filter(|c| c.time >= or_start && c.time < or_end)
                    .collect();
                if !or_candles.is_empty() {
                    let h = or_candles.iter().map(|c| c.high).max().unwrap();
                    let l = or_candles.iter().map(|c| c.low).min().unwrap();
                    info!("{}: KIS API에서 OR 수집 — {}봉 사용", self.stock_name, or_candles.len());
                    return Some((h, l));
                }
                warn!("{}: KIS API 분봉 있으나 OR 범위 캔들 없음", self.stock_name);
            }
            Ok(_) => warn!("{}: KIS API 분봉 빈 응답", self.stock_name),
            Err(e) => warn!("{}: KIS API 분봉 조회 실패: {e}", self.stock_name),
        }

        // 2차: 네이버 금융 분봉 (종가 기반 근사값)
        info!("{}: 네이버 금융에서 OR 수집 시도", self.stock_name);
        let client = reqwest::Client::new();
        let url = format!(
            "https://fchart.stock.naver.com/siseJson.naver?symbol={}&requestType=1&startTime=20260101&endTime=20261231&timeframe=minute",
            self.stock_code.as_str()
        );

        let resp = match client.get(&url).header("User-Agent", "Mozilla/5.0").send().await {
            Ok(r) => r,
            Err(e) => { warn!("{}: 네이버 요청 실패: {e}", self.stock_name); return None; }
        };

        let raw = match resp.text().await {
            Ok(t) => t,
            Err(e) => { warn!("{}: 네이버 응답 읽기 실패: {e}", self.stock_name); return None; }
        };

        let cleaned = raw.replace('\'', "\"");
        let parsed: Vec<Vec<serde_json::Value>> = match serde_json::from_str(&cleaned) {
            Ok(v) => v,
            Err(e) => { warn!("{}: 네이버 파싱 실패: {e}", self.stock_name); return None; }
        };

        let today = Local::now().date_naive();
        let ticks: Vec<(NaiveTime, i64)> = parsed.into_iter()
            .skip(1)
            .filter_map(|row| {
                if row.len() < 6 { return None; }
                let dt_str = row[0].as_str()?.trim().trim_matches('"');
                if dt_str.len() != 12 { return None; }
                let dt = chrono::NaiveDateTime::parse_from_str(dt_str, "%Y%m%d%H%M").ok()?;
                if dt.date() != today { return None; }
                let close = row[4].as_i64()?;
                if close <= 0 { return None; }
                Some((dt.time(), close))
            })
            .collect();

        let or_ticks: Vec<_> = ticks.iter()
            .filter(|(t, _)| *t >= or_start && *t < or_end)
            .collect();

        if or_ticks.is_empty() {
            warn!("{}: 네이버 OR 범위 틱 없음", self.stock_name);
            return None;
        }

        let h = or_ticks.iter().map(|(_, p)| *p).max().unwrap();
        let l = or_ticks.iter().map(|(_, p)| *p).min().unwrap();
        info!("{}: 네이버에서 OR 수집 (종가 근사) — {}틱 사용", self.stock_name, or_ticks.len());
        Some((h, l))
    }
}

/// KRX ETF 호가단위로 round-nearest (2023년 KRX 호가단위 개편 후).
/// - 5,000원 미만: 1원 단위 (round 불필요)
/// - 5,000원 이상: 5원 단위
///
/// KODEX 인버스(1,500원대)는 1원 단위, KODEX 레버리지(91,000원대)는 5원 단위다.
fn round_to_etf_tick(price: i64) -> i64 {
    if price < 5_000 {
        price
    } else {
        let tick = 5;
        ((price + tick / 2) / tick) * tick
    }
}

/// `track_order_and_guard_repeat` 의 순수 로직 (상태 변경 없는 queue 갱신 + 중복 카운트).
///
/// 테스트 가능성을 위해 분리. async 메서드에선 `&mut queue, now, price` 만 넘겨 호출한다.
///
/// 1. 5분(300초) 초과 이력 제거
/// 2. 신규 항목 push, 최대 크기 10 유지
/// 3. 새 limit_price 와 ±0.1% tolerance 안에 들어오는 주문 수 반환
fn update_order_queue_and_count_dup(
    queue: &mut VecDeque<(NaiveTime, i64)>,
    now: NaiveTime,
    limit_price: i64,
) -> usize {
    while let Some(&(t, _)) = queue.front() {
        let elapsed = (now - t).num_seconds();
        if elapsed > 300 || elapsed < 0 {
            queue.pop_front();
        } else {
            break;
        }
    }

    queue.push_back((now, limit_price));
    while queue.len() > 10 {
        queue.pop_front();
    }

    let tolerance = (limit_price as f64 * 0.001).max(1.0) as i64;
    queue
        .iter()
        .filter(|(_, p)| (p - limit_price).abs() <= tolerance)
        .count()
}

#[cfg(test)]
mod repeat_guard_tests {
    use super::update_order_queue_and_count_dup;
    use chrono::NaiveTime;
    use std::collections::VecDeque;

    fn hms(h: u32, m: u32, s: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, s).unwrap()
    }

    #[test]
    fn single_order_has_count_one() {
        let mut q = VecDeque::new();
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        assert_eq!(dup, 1);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn three_same_price_triggers_count_three() {
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        update_order_queue_and_count_dup(&mut q, hms(10, 1, 0), 94_465);
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 2, 0), 94_465);
        assert_eq!(dup, 3);
    }

    #[test]
    fn prices_within_0_1_pct_tolerance_counted_as_same() {
        // 94465 의 0.1% = 94.465 → 정수 tolerance 94 (최종 호출 limit_price 기준).
        // 마지막 호출 가격 94465 를 기준으로 나머지 두 건이 ±94 이내인지 확인.
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_400); // 94465-65, 안쪽
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 30), 94_540); // 94465+75, 안쪽
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 1, 0), 94_465);
        assert_eq!(dup, 3);
    }

    #[test]
    fn prices_outside_tolerance_not_counted() {
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        update_order_queue_and_count_dup(&mut q, hms(10, 1, 0), 95_000); // +535, 바깥
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 2, 0), 94_465);
        assert_eq!(dup, 2); // 94_465 두 번만
    }

    #[test]
    fn old_entries_over_5min_expire() {
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        update_order_queue_and_count_dup(&mut q, hms(10, 2, 0), 94_465);
        // 6분 후 — 10:00 이력은 300초 초과로 제거되어야 함
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 6, 0), 94_465);
        assert_eq!(dup, 2); // 10:02 + 10:06 (10:00 은 제거됨)
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn queue_capped_at_10_entries() {
        let mut q = VecDeque::new();
        for i in 0..15 {
            // 초 단위를 달리해 다른 시각으로 삽입
            update_order_queue_and_count_dup(&mut q, hms(10, 0, i), 1_000 + (i as i64) * 10_000);
        }
        assert!(q.len() <= 10);
    }
}

#[cfg(test)]
mod tick_tests {
    use super::round_to_etf_tick;

    #[test]
    fn test_round_to_etf_tick() {
        // 5,000원 미만은 그대로
        assert_eq!(round_to_etf_tick(1_524), 1_524);
        assert_eq!(round_to_etf_tick(1_519), 1_519);
        assert_eq!(round_to_etf_tick(4_999), 4_999);

        // 5,000원 이상은 5원 단위로 round
        assert_eq!(round_to_etf_tick(5_000), 5_000);
        assert_eq!(round_to_etf_tick(5_002), 5_000); // 5002 → 5000
        assert_eq!(round_to_etf_tick(5_003), 5_005); // 5003 → 5005 (반올림)
        assert_eq!(round_to_etf_tick(92_794), 92_795); // 실제 사례
        assert_eq!(round_to_etf_tick(92_792), 92_790);
        assert_eq!(round_to_etf_tick(91_525), 91_525); // 이미 정렬됨
        assert_eq!(round_to_etf_tick(92_102), 92_100); // 92102 → 92100
    }
}
