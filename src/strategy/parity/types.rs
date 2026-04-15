//! 백테스트-라이브 정합 아키텍처 공통 데이터 계약.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` §7 참조.
//!
//! Phase 1 단계에서는 **기존 동작을 변경하지 않는다**. 이 모듈은 신규 타입만을
//! 제공하여 SignalEngine / ExecutionPolicy / PositionManager 세 계층이 공유할
//! 공통 어휘를 선언하는 역할이다. Phase 2 이후 실제 로직 추출 시 이 타입들이
//! 입출력 매개가 된다.
//!
//! ## 설계 원칙
//! - 전략 신호 탐지(SignalEngine)는 `SignalIntent`까지만 결정하고 주문 가격은
//!   ExecutionPolicy가 산정한다.
//! - 백테스트와 라이브가 공통으로 사용하는 포지션 상태는 `PositionSnapshot` 한
//!   종류만 유지하고, broker 전용 필드(TP 주문번호 등)는 live adapter에 둔다.
//! - `FillResult.slippage` 는 `filled_price - intended_entry_price` 로 "양수 =
//!   불리" 규약을 따른다. 기존 `trades.entry_slippage` 컬럼과 동일.

use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::strategy::candle::MinuteCandle;
use crate::strategy::types::{ExitReason, PositionSide};

/// §7.1 SignalContext — SignalEngine 입력.
///
/// 신호 탐지에 필요한 캔들/상태/시장 상황을 한 구조체로 전달한다.
/// 백테스트는 하루치 캔들을 일괄 제공하고, 라이브는 매 사이클 최신 캔들까지
/// 자른 슬라이스를 넣는다.
#[derive(Debug, Clone)]
pub struct SignalContext {
    pub session_date: NaiveDate,
    pub strategy_id: String,
    pub stage_candidates: Vec<StageDef>,
    pub candles_1m: Vec<MinuteCandle>,
    pub candles_5m: Vec<MinuteCandle>,
    /// 같은 신호 재탐지를 막는 가드 (라이브 `LiveSignalState.search_after`와 등가).
    /// 백테스트도 Phase 2 이후 시각 기반으로 통일한다.
    pub search_after: Option<NaiveTime>,
    pub trade_count: usize,
    pub confirmed_side: Option<PositionSide>,
    pub market_regime_flags: MarketRegimeFlags,
    pub now: NaiveTime,
}

/// Multi-stage OR 정의 (5m / 15m / 30m 등).
/// 백테스트는 `BacktestEngine::pick_strategy_for_day`에서, 라이브는
/// `LiveRunner::poll_and_enter`의 `or_stages_def`에서 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageDef {
    pub name: String,
    pub or_start: NaiveTime,
    pub or_end: NaiveTime,
}

/// 시장/시스템 상태 플래그. Phase 1에서는 정보만 전달하고 신호 탐지 자체는
/// 이 플래그를 아직 사용하지 않는다. Phase 4 이후 ExecutionPolicy가 degraded/
/// manual intervention 분기에 활용한다.
#[derive(Debug, Clone, Default)]
pub struct MarketRegimeFlags {
    pub market_halted: bool,
    pub degraded: bool,
    pub manual_intervention_required: bool,
}

/// §7.2 SignalIntent — SignalEngine 출력.
///
/// **실제 주문 가격과 수량은 포함되지 않는다.** ExecutionPolicy가 이 intent를
/// 받아 `EntryPlan`을 만들 때 정책별로 결정한다.
#[derive(Debug, Clone)]
pub struct SignalIntent {
    pub strategy_id: String,
    pub side: PositionSide,
    pub signal_time: NaiveTime,
    pub stage_name: String,
    /// FVG zone 상단 (Bullish FVG: C.low = `gap.top`)
    pub entry_zone_high: i64,
    /// FVG zone 하단 (Bullish FVG: A.high = `gap.bottom`)
    pub entry_zone_low: i64,
    /// 손절 기준점 (FVG 형성 캔들 직전 A의 low/high)
    pub stop_anchor: i64,
    pub or_high: i64,
    pub or_low: i64,
    pub metadata: SignalMetadata,
}

/// 신호 품질 및 재현성 분석용 메타데이터.
/// 기존 `BestSignal::to_event_metadata` 출력과 동형이며, parity 비교 시
/// legacy / parity / live 결과를 동일 스키마로 정렬하는 기준이 된다.
#[derive(Debug, Clone, Default)]
pub struct SignalMetadata {
    pub gap_top: i64,
    pub gap_bottom: i64,
    pub gap_size_pct: f64,
    pub b_body_ratio: f64,
    pub or_breakout_pct: f64,
    pub b_volume: u64,
    pub b_close: i64,
    pub a_range: i64,
    pub b_time: NaiveTime,
    pub extra: JsonValue,
}

/// 전역 고유 신호 식별자. Phase 1에서는 값 0의 placeholder로 두고, Phase 2
/// SignalEngine이 추출되면 발급기(시퀀스/UUID)를 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SignalId(pub u64);

impl SignalId {
    pub const UNSET: SignalId = SignalId(0);
}

/// §7.3 EntryPlan — ExecutionPolicy가 SignalIntent로부터 만든 주문 계획.
#[derive(Debug, Clone)]
pub struct EntryPlan {
    pub signal_id: SignalId,
    pub side: PositionSide,
    /// 이론 진입가. 백테스트 legacy: `gap.mid_price()`. 라이브 현행:
    /// `gap.top`(Long) / `gap.bottom`(Short). 실제 체결가와 비교해 슬리피지 산정.
    pub intended_entry_price: i64,
    /// 실제 발주할 지정가. 시장가이거나 백테스트처럼 주문 자체가 없으면 None.
    pub order_price: Option<i64>,
    pub entry_mode: EntryMode,
    pub max_entry_drift_pct: Option<f64>,
    pub timeout_ms: u64,
    pub cancel_on_timeout: bool,
    pub fill_recheck_mode: FillRecheckMode,
    /// |intended_entry - planned_stop_loss|
    pub risk_amount: i64,
    /// 체결 확인 전 계획한 SL / TP (체결 후 재평행이동 전 기준).
    pub planned_stop_loss: i64,
    pub planned_take_profit: i64,
    /// 수량 계산은 상위 adapter 책임. ExecutionPolicy가 힌트만 전달 가능.
    pub target_quantity_hint: Option<u64>,
}

/// 진입 방식 모델. Phase 2/4 확장점.
///
/// `f64` variant 때문에 `Eq` 는 유도할 수 없다. 비교가 필요하면 `PartialEq`
/// 또는 명시적 매칭을 사용한다.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum EntryMode {
    /// 백테스트 legacy: zone 터치 시 `gap.mid_price()` 즉시 체결 가정.
    MidPriceZoneTouch,
    /// 라이브 현행: zone 터치 시 `gap.top`/`gap.bottom` 패시브 지정가, 30초
    /// timeout. 미체결 시 cancel 후 balance 재조회로 race 검증.
    PassiveTopBottom { timeout_ms: u64 },
    /// 2차 parity 이후 도입 예정.
    MarketableLimit { slippage_budget_pct: f64 },
}

/// 체결 결과 확정 방식.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRecheckMode {
    /// 백테스트: 체결 진실값을 OHLC로만 판정.
    Synthetic,
    /// 라이브: cancel 직후 balance 재조회로 race 검증.
    BalanceRecheck,
}

/// §7.4 FillResult — 진입 주문의 최종 결과.
#[derive(Debug, Clone)]
pub struct FillResult {
    pub signal_id: SignalId,
    pub status: FillStatus,
    pub filled_qty: u64,
    pub filled_price: Option<i64>,
    /// `filled_price - intended_entry_price`. **양수 = 불리(long이면 비싸게 삼, short이면 싸게 팜)**.
    /// 기존 `trades.entry_slippage` 컬럼과 부호 동일.
    pub slippage: Option<i64>,
    pub fill_time: Option<NaiveTime>,
    pub broker_order_id: Option<String>,
    pub resolution_source: FillResolutionSource,
    pub order_to_fill_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillStatus {
    Filled,
    PartialFill,
    Cancelled(CancelReason),
    Rejected(String),
    ManualIntervention(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelReason {
    Timeout,
    Preempted,
    Drift,
    VolatilityInterruption,
    EntryCutoff,
    ApiError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillResolutionSource {
    /// 라이브 WS 체결 통보.
    WsNotice,
    /// 라이브 REST 일별 체결(`inquire-daily-ccld`).
    DailyCcld,
    /// 라이브 balance 재확인(`inquire-balance`).
    BalanceRecheck,
    /// 백테스트 / parity / replay 의 모의 체결.
    HistoricalSim,
}

/// §7.5 PositionSnapshot — 백테스트/라이브 공용 포지션 상태.
///
/// 기존 `strategy::types::Position`에서 라이브 전용 필드(tp_order_no,
/// sl_triggered_tick 등)를 제거한 pure view.
#[derive(Debug, Clone)]
pub struct PositionSnapshot {
    pub signal_id: SignalId,
    pub side: PositionSide,
    pub entry_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    pub quantity: u64,
    pub entry_time: NaiveTime,
    pub reached_1r: bool,
    pub best_price: i64,
    /// 1R 이전 기준 SL (ExitReason 판정용).
    pub original_sl: i64,
    /// 원래 리스크 |intended_entry - planned_stop_loss|. 재시작 복구 근거.
    pub original_risk: i64,
    pub intended_entry_price: i64,
    pub order_to_fill_ms: i64,
    pub strategy_metadata: JsonValue,
}

/// §7.6 ExitDecision — PositionManager의 한 tick 결정.
///
/// **pure**. 외부 I/O 없이 `PositionSnapshot`과 현재 캔들/가격만 읽는다.
/// `should_exit=false`일 때도 `next_position`에 갱신된 best_price/stop_loss가
/// 담긴다. 호출측은 무조건 `*pos = dec.next_position`로 덮어쓰면 된다.
#[derive(Debug, Clone)]
pub struct ExitDecision {
    pub should_exit: bool,
    pub reason: Option<ExitReason>,
    pub exit_price_model: ExitPriceModel,
    /// 다음 tick부터 적용될 SL (트레일링으로 갱신된 값).
    pub effective_stop: i64,
    pub effective_take_profit: i64,
    pub decision_time: NaiveTime,
    pub next_position: PositionSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitPriceModel {
    /// 백테스트: 지정가 수준에서 정확히 체결된다고 가정(TP/SL 가격 자체).
    LimitAtLevel,
    /// 백테스트: 갭 손절 — 다음 캔들 시가 체결.
    OpenGap,
    /// 백테스트: 시간 스탑 / EOD — close 체결.
    CandleClose,
    /// 라이브: KRX에 걸린 TP 지정가 실제 체결가 fetch.
    LiveTpFill,
    /// 라이브: 시장가 청산.
    LiveMarket,
}
