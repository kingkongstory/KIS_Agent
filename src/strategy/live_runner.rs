use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Local, NaiveTime};
use serde::Deserialize;
use tokio::sync::{Notify, RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::domain::error::KisError;
use crate::domain::models::order::OrderSide;
use crate::domain::models::price::InquirePrice;
use crate::domain::ports::realtime::{RealtimeData, TradeNotification};
use crate::infrastructure::cache::postgres_store::{ActivePosition, PostgresStore, TradeRecord};
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
    position_lock: Option<Arc<RwLock<Option<String>>>>,
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
        }
    }

    pub fn with_position_lock(mut self, lock: Arc<RwLock<Option<String>>>) -> Self {
        self.position_lock = Some(lock);
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
                if !saved.tp_order_no.is_empty() {
                    match self.cancel_tp_order(&saved.tp_order_no, &saved.tp_krx_orgno).await {
                        Ok(()) => info!("{}: 이전 TP 주문 취소 성공", self.stock_name),
                        Err(e) => warn!("{}: 이전 TP 주문 취소 실패 (계속 진행): {e}", self.stock_name),
                    }
                    // 취소 후 잔고 반영 대기
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }

                // 새 TP 지정가 발주
                let (tp_order_no, tp_krx_orgno, tp_limit_price) =
                    if let Some((no, orgno)) = self.place_tp_limit_order(saved.take_profit, saved.quantity as u64).await {
                        (Some(no), Some(orgno), Some(saved.take_profit))
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
                    reached_1r: false,
                    best_price: saved.entry_price,
                    original_sl: saved.original_sl,
                });
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
                info!("{}: DB에 활성 포지션 없음 — 깨끗한 상태", self.stock_name);
                // 잔고 API fallback은 사용하지 않음
                // 모의투자에서 잔고잠김 문제로 매도 불가한 잔여 보유가 있을 수 있으나 무시
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
            if let Some((no, orgno)) = self.place_tp_limit_order(take_profit, quantity).await {
                (Some(no), Some(orgno), Some(take_profit))
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
    fn spawn_price_listener(&mut self) {
        if let Some(mut rx) = self.realtime_rx.take() {
            let code = self.stock_code.as_str().to_string();
            let latest = Arc::clone(&self.latest_price);
            let state = Arc::clone(&self.state);

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
                            // 틱마다 best_price 갱신 (백테스트 캔들 high/low와 동등한 정확도)
                            let mut s = state.write().await;
                            if let Some(ref mut pos) = s.current_position {
                                pos.best_price = match pos.side {
                                    PositionSide::Long => pos.best_price.max(exec.price),
                                    PositionSide::Short => pos.best_price.min(exec.price),
                                };
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
            warn!("{}: 실시간 가격 30초 이상 미갱신 — REST 폴백", self.stock_name);
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

        // 장 시작 대기
        self.update_phase("장 시작 대기").await;
        self.wait_until(cfg.or_start).await;
        if self.is_stopped() { return Ok(Vec::new()); }

        // 잔존 포지션이 있으면 장 시작 즉시 시장가 청산 (전일 잔여분 정리)
        let has_leftover = self.state.read().await.current_position.is_some();
        if has_leftover {
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
                        *lock.write().await = None;
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
        let mut all_trades: Vec<TradeResult> = Vec::new();
        let mut trade_count = 0;
        let mut confirmed_side: Option<PositionSide> = None;

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
                        self.save_trade_to_db(&result).await;
                        all_trades.push(result);
                        trade_count += 1;
                        self.update_pnl(&all_trades).await;

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

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
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
        let mut best_signal: Option<(PositionSide, i64, i64, i64, &str)> = None; // (side, entry, sl, tp, stage)

        for (stage_name, or_high, or_low, or_end) in &available_stages {
            let scan: Vec<_> = realtime_candles.iter()
                .filter(|c| c.time >= *or_end)
                .cloned()
                .collect();
            let candles_5m = candle::aggregate(&scan, 5);
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
                        let entry_price = gap.mid_price();
                        let stop_loss = gap.stop_loss;
                        let risk = (entry_price - stop_loss).abs();
                        let take_profit = match side {
                            PositionSide::Long => entry_price + (risk as f64 * cfg.rr_ratio) as i64,
                            PositionSide::Short => entry_price - (risk as f64 * cfg.rr_ratio) as i64,
                        };

                        // 선착순: 이미 다른 단계가 신호를 냈으면 스킵
                        if best_signal.is_none() {
                            best_signal = Some((side, entry_price, stop_loss, take_profit, stage_name));
                        }
                        break; // 이 단계에서 첫 신호 찾으면 종료
                    }
                }
            }
        }

        // 선착순으로 선택된 신호로 진입
        if let Some((side, entry_price, stop_loss, take_profit, stage_name)) = best_signal {
            // 공유 포지션 잠금 (CAS: 원자적 체크+설정, race condition 방지)
            if let Some(ref lock) = self.position_lock {
                let mut guard = lock.write().await;
                if let Some(ref holding_code) = *guard {
                    if holding_code != self.stock_code.as_str() {
                        debug!("{}: 진입 차단 — {}가 포지션 보유 중", self.stock_name, holding_code);
                        return Ok(false);
                    }
                }
                // 즉시 잠금 선점 (execute_entry 전에 설정)
                *guard = Some(self.stock_code.as_str().to_string());
                drop(guard);
            }

            // VI 발동 중이면 진입 차단 (잠금 해제 필요)
            if self.state.read().await.market_halted {
                warn!("{}: VI 발동 중 — 진입 보류", self.stock_name);
                if let Some(ref lock) = self.position_lock {
                    *lock.write().await = None;
                }
                return Ok(false);
            }

            info!("{}: [{}] {:?} 진입 신호 — entry={}, SL={}, TP={}",
                self.stock_name, stage_name, side, entry_price, stop_loss, take_profit);

            // 주문 실행 (실패 시 잠금 해제 + 1초 후 재시도)
            match self.execute_entry(side, entry_price, stop_loss, take_profit).await {
                Ok(_) => return Ok(true),
                Err(e) => {
                    warn!("{}: 주문 실패, 1초 후 재시도: {e}", self.stock_name);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    match self.execute_entry(side, entry_price, stop_loss, take_profit).await {
                        Ok(_) => return Ok(true),
                        Err(e2) => {
                            error!("{}: 재시도 실패 — 잠금 해제: {e2}", self.stock_name);
                            if let Some(ref lock) = self.position_lock {
                                *lock.write().await = None;
                            }
                            return Ok(false);
                        }
                    }
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
                        *lock.write().await = None;
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

        if !pos.reached_1r && profit_from_entry >= breakeven_dist {
            pos.reached_1r = true;
            pos.stop_loss = pos.entry_price; // 본전스탑 (백테스트 동일)
            info!("{}: 1R 도달 — 본전 스탑 활성화 (SL → {})", self.stock_name, pos.stop_loss);
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
            }
        }

        // ── Step 3: SL 체크 (백테스트 동일 판정) ──
        let sl_hit = match pos.side {
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

    /// 시장가 진입 주문
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

        // 주문 직전 매수가능수량 실시간 조회 (가용금액 변동 반영)
        let actual_qty = {
            let price_str = entry_price.to_string();
            let buyable_query = [
                ("CANO", self.client.account_no()),
                ("ACNT_PRDT_CD", self.client.account_product_code()),
                ("PDNO", self.stock_code.as_str()),
                ("ORD_UNPR", price_str.as_str()),
                ("ORD_DVSN", "01"),
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
                    Ok(info) if info.ord_psbl_qty > 1 => {
                        let qty = (info.ord_psbl_qty - 1) as u64; // 1주 여유 (슬리피지 대비)
                        info!("{}: 주문 직전 매수가능 {}주 (API {}주 - 1, 가용 {}원)",
                            self.stock_name, qty, info.ord_psbl_qty, info.ord_psbl_cash);
                        qty
                    }
                    Ok(info) => {
                        warn!("{}: 매수가능수량 부족 ({}주)", self.stock_name, info.ord_psbl_qty);
                        0
                    }
                    Err(e) => {
                        warn!("{}: 매수가능조회 파싱 실패: {e} — 잔고 기반 fallback", self.stock_name);
                        let available = self.get_available_cash().await;
                        if available > 0 && entry_price > 0 {
                            let qty = ((available / entry_price) - 1).max(0) as u64;
                            info!("{}: 잔고 기반 수량 {}주 (가용 {}원)", self.stock_name, qty, available);
                            qty
                        } else {
                            0
                        }
                    }
                },
                Err(e) => {
                    warn!("{}: 매수가능조회 실패: {e} — 잔고 기반 fallback", self.stock_name);
                    // 잔고 API로 가용현금 조회 → 수량 계산
                    let available = self.get_available_cash().await;
                    if available > 0 && entry_price > 0 {
                        let qty = ((available / entry_price) - 1).max(0) as u64;
                        info!("{}: 잔고 기반 수량 {}주 (가용 {}원)", self.stock_name, qty, available);
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
            "ORD_DVSN": "01",
            "ORD_QTY": actual_qty.to_string(),
            "ORD_UNPR": "0",
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let resp: KisResponse<serde_json::Value> = self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash", &tr_id, None, Some(&body))
            .await?;

        let side_str = format!("{:?}", order_side);
        if resp.rt_cd != "0" {
            // 주문 실패 로그
            if let Some(ref store) = self.db_store {
                store.save_order_log(
                    self.stock_code.as_str(), "진입", &side_str,
                    self.quantity as i64, entry_price, "", "실패", &resp.msg1,
                ).await;
            }
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        // 주문번호 추출 → 실제 체결가 조회
        let order_no = resp.output.as_ref()
            .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();
        let actual_entry = self.fetch_fill_price(&order_no).await.unwrap_or(entry_price);

        // 주문 성공 로그
        if let Some(ref store) = self.db_store {
            store.save_order_log(
                self.stock_code.as_str(), "진입", &side_str,
                actual_qty as i64, actual_entry, &order_no, "체결", "",
            ).await;
        }

        info!("{}: {:?} {}주 진입 완료 (이론={}, 실제={})", self.stock_name, order_side, actual_qty, entry_price, actual_entry);
        self.notify_trade("entry");

        // 실제 체결가 기준으로 SL/TP 재계산
        let risk = (actual_entry - stop_loss).abs();
        let rr = self.strategy.config.rr_ratio;
        let actual_tp = match side {
            PositionSide::Long => actual_entry + (risk as f64 * rr) as i64,
            PositionSide::Short => actual_entry - (risk as f64 * rr) as i64,
        };

        // TP 지정가 매도 발주 (실패 시 시장가 fallback)
        let (tp_order_no, tp_krx_orgno, tp_limit_price) =
            if let Some((no, orgno)) = self.place_tp_limit_order(actual_tp, actual_qty).await {
                (Some(no), Some(orgno), Some(actual_tp))
            } else {
                warn!("{}: TP 지정가 발주 실패 — 시장가 fallback 모드", self.stock_name);
                (None, None, None)
            };

        let mut state = self.state.write().await;
        state.current_position = Some(Position {
            side,
            entry_price: actual_entry,
            stop_loss,
            take_profit: actual_tp,
            entry_time: Local::now().time(),
            quantity: actual_qty,
            tp_order_no,
            tp_krx_orgno,
            tp_limit_price,
            reached_1r: false,
            best_price: actual_entry,
            original_sl: stop_loss,
        });
        state.phase = "포지션 보유".to_string();

        // DB에 활성 포지션 저장 (재시작 복구용)
        if let Some(ref store) = self.db_store {
            let state_r = self.state.read().await;
            if let Some(ref pos) = state_r.current_position {
                let ap = ActivePosition {
                    stock_code: self.stock_code.as_str().to_string(),
                    side: format!("{:?}", pos.side),
                    entry_price: pos.entry_price,
                    stop_loss: pos.stop_loss,
                    take_profit: pos.take_profit,
                    quantity: pos.quantity as i64,
                    tp_order_no: pos.tp_order_no.clone().unwrap_or_default(),
                    tp_krx_orgno: pos.tp_krx_orgno.clone().unwrap_or_default(),
                    entry_time: Local::now().naive_local(),
                    original_sl: pos.original_sl,
                };
                drop(state_r);
                if let Err(e) = store.save_active_position(&ap).await {
                    error!("{}: 활성 포지션 DB 저장 실패 — 재시작 시 복구 불가: {e}", self.stock_name);
                }
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
        let exit_price = self.fetch_fill_price(&order_no).await.unwrap_or(ws_price);
        let exit_time = Local::now().time();

        // 청산 성공 로그
        if let Some(ref store) = self.db_store {
            store.save_order_log(
                self.stock_code.as_str(), &format!("청산({:?})", reason), &format!("{:?}", order_side),
                pos.quantity as i64, exit_price, &order_no, "체결", "",
            ).await;
        }

        info!("{}: {:?} 청산 — {} @ {} ({:?}, WS={})", self.stock_name, pos.side, self.quantity, exit_price, reason, ws_price);
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
            *lock.write().await = None;
            info!("{}: 포지션 잠금 해제 (다른 종목 진입 허용)", self.stock_name);
        }

        Ok(result)
    }

    // ── 헬퍼 메서드 ──

    async fn update_phase(&self, phase: &str) {
        self.state.write().await.phase = phase.to_string();
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
        let record = TradeRecord {
            stock_code: self.stock_code.as_str().to_string(),
            stock_name: self.stock_name.clone(),
            side: format!("{:?}", result.side),
            quantity: self.quantity as i64,
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
        };
        if let Err(e) = store.save_trade(&record).await {
            error!("{}: 거래 DB 저장 실패: {e}", self.stock_name);
        } else {
            info!("{}: 거래 DB 저장 완료 ({:?} {:.2}%)", self.stock_name, result.exit_reason, result.pnl_pct());
        }
    }

    /// 잔고 API로 가용현금 조회 (예수금 - 매입금)
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
                    let cash: i64 = first.get("dnca_tot_amt").and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok()).unwrap_or(0);
                    let purchase: i64 = first.get("pchs_amt_smtl_amt").and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok()).unwrap_or(0);
                    return (cash - purchase).max(0);
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

    /// TP 지정가 매도 주문 발주
    async fn place_tp_limit_order(&self, price: i64, quantity: u64) -> Option<(String, String)> {
        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "00",
            "ORD_QTY": quantity.to_string(),
            "ORD_UNPR": price.to_string(),
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
                info!("{}: TP 지정가 발주 완료 — {}원, 주문번호={}", self.stock_name, price, order_no);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "TP지정가", "Sell",
                        quantity as i64, price, &order_no, "체결", "",
                    ).await;
                }
                Some((order_no, krx_orgno))
            }
            Ok(r) => {
                warn!("{}: TP 지정가 발주 실패 — {}", self.stock_name, r.msg1);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "TP지정가", "Sell",
                        quantity as i64, price, "", "실패", &r.msg1,
                    ).await;
                }
                None
            }
            Err(e) => {
                warn!("{}: TP 지정가 발주 에러 — {e}", self.stock_name);
                if let Some(ref store) = self.db_store {
                    store.save_order_log(
                        self.stock_code.as_str(), "TP지정가", "Sell",
                        quantity as i64, price, "", "에러", &e.to_string(),
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

    /// 주문 체결 상세 조회 — WS 체결통보 우선 (3초), REST 폴링 fallback (3회)
    async fn fetch_fill_detail(&self, order_no: &str) -> Option<(i64, u64)> {
        // 1차: WS 체결통보 대기 (최대 3초)
        if let Some(ref tx) = self.trade_tx {
            let mut rx = tx.subscribe();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
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
                    Ok(Err(_)) => break, // 채널 닫힘
                    Err(_) => break,     // 타임아웃
                    _ => {} // 다른 메시지 무시
                }
            }
            debug!("{}: WS 체결통보 3초 내 미수신 — REST fallback", self.stock_name);
        }

        // 2차: REST 폴링 (최대 3회, 1초 간격)
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            if let Some((price, qty)) = self.query_execution(order_no).await {
                return Some((price, qty));
            }
        }
        warn!("{}: 체결 조회 실패 (WS+REST) — 주문 {}", self.stock_name, order_no);
        None
    }

    /// 체결가 조회 편의 메서드 (기존 호출부 호환)
    async fn fetch_fill_price(&self, order_no: &str) -> Option<i64> {
        self.fetch_fill_detail(order_no).await.map(|(price, _)| price)
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
