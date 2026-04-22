//! Parity Backtest — `SignalEngine + ExecutionPolicy + PositionManager` 조합.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` §12.2 구현.
//!
//! 이 모듈이 제공하는 `ParityDayRunner` 는 legacy 백테스트(`orb_fvg::run_day`)
//! 와 나란히 놓고 비교할 수 있는 **두 번째 실행 경로**다. 차이는 오직
//! ExecutionPolicy 뿐이다.
//!
//! ## 구성
//! - `SignalEngine`: FVG 탐지 (legacy 백테스트와 완전 동일 — `detect_next_fvg_signal`).
//! - `ExecutionPolicy`: 진입 가격·timeout·cancel 규칙. `LegacyMidPriceSim` 또는
//!   `PassiveTopBottomTimeout30s` 주입.
//! - `PositionManager`: 청산 결정 pure helper. 이미 `simulate_exit` 에서 공유됨.
//!
//! ## 체결 시뮬레이터
//! - `LegacyMidPriceSim`: 리트레이스 확정 캔들에서 `gap.mid_price()` 즉시 체결.
//!   기존 `orb_fvg::scan_and_trade` 와 동일.
//! - `PassiveTopBottomTimeout30s`: 리트레이스가 확정된 뒤 1분 입력에서 signal minute
//!   의 order lifetime 안에 `intended_entry_price`(gap.top/bottom) 가 닿으면 체결,
//!   아니면 cancel 후 `search_after` 전진.

use chrono::{Duration, NaiveTime};
use tracing::{debug, info};

use crate::strategy::candle::{self, MinuteCandle};
use crate::strategy::types::{ExitReason, PositionSide, TradeResult};

use super::execution_policy::{ExecutionContext, ExecutionPolicy, FillFeedback, FillOutcome};
use super::position_manager::{
    PositionManagerConfig, gap_exit_reason, is_sl_hit, is_tp_hit, open_gap_breach, sl_exit_reason,
    time_stop_breached_by_candles, update_best_and_trailing,
};
use super::signal_engine::{SignalEngine, SignalEngineConfig};
use super::types::{CancelReason, EntryPlan, FillResolutionSource, FillStatus, SignalId, StageDef};

/// 파리티 백테스트 1일 러너.
pub struct ParityDayRunner<E: SignalEngine, P: ExecutionPolicy> {
    pub signal_engine: E,
    pub policy: P,
    pub strategy_id: String,
    pub stage_name: String,
    pub or_start: NaiveTime,
    pub or_end: NaiveTime,
    pub entry_cutoff: NaiveTime,
    pub force_exit: NaiveTime,
    pub pm_config: PositionManagerConfig,
    pub fvg_expiry_candles: usize,
    pub min_or_breakout_pct: f64,
    pub long_only: bool,
    pub rr_ratio: f64,
    pub max_entry_drift_pct: Option<f64>,
    pub max_daily_trades: usize,
    pub max_daily_loss_pct: f64,
}

#[derive(Debug, Clone)]
struct PreparedStage {
    stage_index: usize,
    name: String,
    or_high: i64,
    or_low: i64,
    candles_5m: Vec<MinuteCandle>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ParityLoopState {
    search_after: Option<NaiveTime>,
    trade_count: usize,
    cumulative_pnl: f64,
    confirmed_side: Option<PositionSide>,
}

#[derive(Debug, Clone)]
struct StageCandidate {
    stage: PreparedStage,
    detected: super::signal_engine::DetectedSignal,
    retrace_candle_idx_abs: usize,
}

impl<E: SignalEngine, P: ExecutionPolicy> ParityDayRunner<E, P> {
    /// 하루 캔들을 받아 거래 결과 목록을 반환.
    pub fn run_day(&self, day_candles: &[MinuteCandle]) -> Vec<TradeResult> {
        let mut results = Vec::new();
        if day_candles.is_empty() {
            return results;
        }

        let is_5m_input = day_candles.len() >= 2
            && (day_candles[1].time - day_candles[0].time).num_minutes() >= 4;

        // 1) OR 범위 계산.
        let or_candles: Vec<_> = day_candles
            .iter()
            .filter(|c| c.time >= self.or_start && c.time < self.or_end)
            .cloned()
            .collect();
        if or_candles.is_empty() {
            debug!("parity: OR 기간 캔들 없음");
            return results;
        }
        let or_high = or_candles.iter().map(|c| c.high).max().unwrap();
        let or_low = or_candles.iter().map(|c| c.low).min().unwrap();
        info!(
            "parity OR 범위: HIGH={or_high}, LOW={or_low} ({}개 캔들)",
            or_candles.len()
        );

        // 2) 5분봉 준비.
        let scan_candles: Vec<_> = day_candles
            .iter()
            .filter(|c| c.time >= self.or_end)
            .cloned()
            .collect();
        let candles_5m = if is_5m_input {
            scan_candles
        } else {
            candle::aggregate(&scan_candles, 5)
        };
        if candles_5m.is_empty() {
            return results;
        }

        // 3) 메인 루프.
        let mut trade_count = 0usize;
        let mut cumulative_pnl = 0.0f64;
        let mut confirmed_side: Option<PositionSide> = None;
        let mut search_from = 0usize;
        let mut signal_seq: u64 = 1;

        loop {
            if trade_count >= self.max_daily_trades {
                info!("parity: 최대 거래 횟수 도달 ({}회)", self.max_daily_trades);
                break;
            }
            if cumulative_pnl <= self.max_daily_loss_pct {
                info!("parity: 일일 손실 한도 도달 ({:.2}%)", cumulative_pnl);
                break;
            }
            if search_from >= candles_5m.len() {
                break;
            }
            let scan_slice = &candles_5m[search_from..];

            // 2026-04-17 v3 불변식 #1b: require_or_breakout 항상 true.
            let require_or = true;
            let _ = trade_count;
            let engine_cfg = SignalEngineConfig {
                strategy_id: self.strategy_id.clone(),
                stage_name: self.stage_name.clone(),
                or_high,
                or_low,
                fvg_expiry_candles: self.fvg_expiry_candles,
                entry_cutoff: self.entry_cutoff,
                require_or_breakout: require_or,
                min_or_breakout_pct: self.min_or_breakout_pct,
                long_only: self.long_only,
                confirmed_side,
            };
            let detected = match self.signal_engine.detect_next(scan_slice, &engine_cfg) {
                Some(d) => d,
                None => break,
            };

            // 4) EntryPlan 생성 (정책별).
            let mut intent = detected.intent.clone();
            intent.metadata.extra = serde_json::json!({});
            let ctx = ExecutionContext {
                rr_ratio: self.rr_ratio,
                max_entry_drift_pct: self.max_entry_drift_pct,
                slippage_budget_pct: None,
            };
            let mut plan = self.policy.plan(&intent, &ctx);
            plan.signal_id = SignalId(signal_seq);
            signal_seq += 1;

            let retrace_idx_abs = search_from + detected.retrace_candle_idx;
            let retrace_candle = &candles_5m[retrace_idx_abs];

            // 5) Fill 시뮬레이션.
            let fill_feedback = simulate_fill(&plan, retrace_candle);
            let fill = self.policy.resolve_fill(&plan, &fill_feedback);

            match &fill.status {
                FillStatus::Filled | FillStatus::PartialFill => {
                    let entry_price = fill.filled_price.unwrap_or(plan.intended_entry_price);
                    let stop_loss = plan.planned_stop_loss;
                    let take_profit = plan.planned_take_profit;
                    let side = plan.side;

                    // 방향 확인 (confirmed_side가 있으면 같은 방향만 허용).
                    if let Some(cs) = confirmed_side
                        && side != cs
                    {
                        search_from = retrace_idx_abs + 1;
                        continue;
                    }

                    info!(
                        "parity {}: {:?} 진입 — entry={}, SL={}, TP={} (RR=1:{:.1}, policy={})",
                        retrace_candle.time,
                        side,
                        entry_price,
                        stop_loss,
                        take_profit,
                        self.rr_ratio,
                        self.policy.version()
                    );

                    // 6) 청산 시뮬레이션 (공통 PositionManager helper).
                    let trade = simulate_exit_with_pm(
                        side,
                        entry_price,
                        stop_loss,
                        take_profit,
                        retrace_candle.time,
                        &candles_5m[retrace_idx_abs + 1..],
                        &self.pm_config,
                        self.force_exit,
                        plan.intended_entry_price,
                        fill.order_to_fill_ms,
                    );

                    if let Some(trade) = trade {
                        let pnl = trade.pnl_pct();
                        trade_count += 1;
                        cumulative_pnl += pnl;
                        info!(
                            "parity {}차 거래: {:?} {:.2}% (누적 {:.2}%, reason={:?})",
                            trade_count, trade.side, pnl, cumulative_pnl, trade.exit_reason
                        );
                        confirmed_side = if pnl > 0.0 { Some(trade.side) } else { None };

                        let exit_idx_rel = candles_5m[retrace_idx_abs..]
                            .iter()
                            .position(|c| c.time >= trade.exit_time)
                            .map(|p| p + retrace_idx_abs)
                            .unwrap_or(candles_5m.len());
                        search_from = exit_idx_rel.max(retrace_idx_abs + 1);
                        results.push(trade);
                    } else {
                        break;
                    }
                }
                FillStatus::Cancelled(reason) => {
                    info!(
                        "parity {}: 체결 취소 — reason={:?}, search_after 전진",
                        retrace_candle.time, reason
                    );
                    // 같은 FVG 재탐지 방지. 리트레이스 캔들 다음으로 전진.
                    search_from = retrace_idx_abs + 1;
                }
                FillStatus::Rejected(msg) => {
                    info!("parity {}: 체결 거부 — {}", retrace_candle.time, msg);
                    search_from = retrace_idx_abs + 1;
                }
                FillStatus::ManualIntervention(msg) => {
                    info!(
                        "parity {}: manual_intervention — {}",
                        retrace_candle.time, msg
                    );
                    break;
                }
            }
        }

        results
    }

    /// 라이브와 같은 multi-stage / search_after 상태기계를 사용하는 parity 실행 경로.
    ///
    /// - stage 목록을 순회하며 각 stage의 다음 후보 신호를 찾는다.
    /// - 후보 중 가장 빠른 `signal_time`을 채택하고, 동률이면 stage 순서를 따른다.
    /// - cancel/reject 시 `search_after`를 `signal_time`까지 전진시켜 같은 FVG 재감지를 차단한다.
    pub fn run_day_live_like(
        &self,
        day_candles_1m: &[MinuteCandle],
        stages: &[StageDef],
    ) -> Vec<TradeResult> {
        let mut results = Vec::new();
        if day_candles_1m.is_empty() || stages.is_empty() {
            return results;
        }

        let prepared = self.prepare_stage_views(day_candles_1m, stages);
        if prepared.is_empty() {
            debug!("parity/live_like: 준비 가능한 stage 없음");
            return results;
        }

        let mut state = ParityLoopState::default();
        let mut signal_seq: u64 = 1;

        loop {
            if state.trade_count >= self.max_daily_trades {
                info!(
                    "parity/live_like: 최대 거래 횟수 도달 ({}회)",
                    self.max_daily_trades
                );
                break;
            }
            if state.cumulative_pnl <= self.max_daily_loss_pct {
                info!(
                    "parity/live_like: 일일 손실 한도 도달 ({:.2}%)",
                    state.cumulative_pnl
                );
                break;
            }

            let candidate = match self.next_stage_candidate(&prepared, &state) {
                Some(c) => c,
                None => break,
            };

            let mut intent = candidate.detected.intent.clone();
            intent.metadata.extra = serde_json::json!({});
            let ctx = ExecutionContext {
                rr_ratio: self.rr_ratio,
                max_entry_drift_pct: self.max_entry_drift_pct,
                slippage_budget_pct: None,
            };
            let mut plan = self.policy.plan(&intent, &ctx);
            plan.signal_id = SignalId(signal_seq);
            signal_seq += 1;

            let signal_time_1m = resolve_signal_time_1m(&candidate.detected, day_candles_1m);
            let fill_feedback = simulate_fill_intraday(&plan, signal_time_1m, day_candles_1m);
            let fill = self.policy.resolve_fill(&plan, &fill_feedback);
            let retrace_candle = &candidate.stage.candles_5m[candidate.retrace_candle_idx_abs];

            match &fill.status {
                FillStatus::Filled | FillStatus::PartialFill => {
                    let entry_price = fill.filled_price.unwrap_or(plan.intended_entry_price);
                    let stop_loss = plan.planned_stop_loss;
                    let take_profit = plan.planned_take_profit;
                    let side = plan.side;

                    info!(
                        "parity/live_like {} [{}]: {:?} 진입 — entry={}, SL={}, TP={} (policy={})",
                        signal_time_1m,
                        candidate.stage.name,
                        side,
                        entry_price,
                        stop_loss,
                        take_profit,
                        self.policy.version()
                    );

                    let trade = simulate_exit_with_pm(
                        side,
                        entry_price,
                        stop_loss,
                        take_profit,
                        fill.fill_time.unwrap_or(signal_time_1m),
                        &candidate.stage.candles_5m[candidate.retrace_candle_idx_abs + 1..],
                        &self.pm_config,
                        self.force_exit,
                        plan.intended_entry_price,
                        fill.order_to_fill_ms,
                    );

                    if let Some(trade) = trade {
                        let pnl = trade.pnl_pct();
                        state.trade_count += 1;
                        state.cumulative_pnl += pnl;
                        state.confirmed_side = if pnl > 0.0 { Some(trade.side) } else { None };
                        state.search_after = Some(trade.exit_time);
                        info!(
                            "parity/live_like {}차 거래 [{}]: {:?} {:.2}% (누적 {:.2}%, reason={:?})",
                            state.trade_count,
                            candidate.stage.name,
                            trade.side,
                            pnl,
                            state.cumulative_pnl,
                            trade.exit_reason
                        );
                        results.push(trade);
                    } else {
                        debug!(
                            "parity/live_like {} [{}]: 청산 시뮬레이션 결과 없음",
                            retrace_candle.time, candidate.stage.name
                        );
                        break;
                    }
                }
                FillStatus::Cancelled(reason) => {
                    info!(
                        "parity/live_like {} [{}]: 체결 취소 — reason={:?}",
                        retrace_candle.time, candidate.stage.name, reason
                    );
                    advance_search_after(
                        &mut state.search_after,
                        candidate.detected.intent.signal_time,
                    );
                }
                FillStatus::Rejected(msg) => {
                    info!(
                        "parity/live_like {} [{}]: 체결 거부 — {}",
                        retrace_candle.time, candidate.stage.name, msg
                    );
                    advance_search_after(
                        &mut state.search_after,
                        candidate.detected.intent.signal_time,
                    );
                }
                FillStatus::ManualIntervention(msg) => {
                    info!(
                        "parity/live_like {} [{}]: manual_intervention — {}",
                        retrace_candle.time, candidate.stage.name, msg
                    );
                    break;
                }
            }
        }

        results
    }

    fn prepare_stage_views(
        &self,
        day_candles_1m: &[MinuteCandle],
        stages: &[StageDef],
    ) -> Vec<PreparedStage> {
        let mut out = Vec::new();
        for (stage_index, stage) in stages.iter().enumerate() {
            let or_candles: Vec<_> = day_candles_1m
                .iter()
                .filter(|c| c.time >= stage.or_start && c.time < stage.or_end)
                .cloned()
                .collect();
            if or_candles.is_empty() {
                continue;
            }
            let or_high = or_candles.iter().map(|c| c.high).max().unwrap();
            let or_low = or_candles.iter().map(|c| c.low).min().unwrap();
            let scan_candles: Vec<_> = day_candles_1m
                .iter()
                .filter(|c| c.time >= stage.or_end)
                .cloned()
                .collect();
            let candles_5m = candle::aggregate(&scan_candles, 5);
            if candles_5m.is_empty() {
                continue;
            }
            out.push(PreparedStage {
                stage_index,
                name: stage.name.clone(),
                or_high,
                or_low,
                candles_5m,
            });
        }
        out
    }

    fn next_stage_candidate(
        &self,
        stages: &[PreparedStage],
        state: &ParityLoopState,
    ) -> Option<StageCandidate> {
        // 2026-04-17 v3 불변식 #1b: require_or_breakout 항상 true.
        let require_or = true;
        let _ = state.trade_count;
        let mut best: Option<StageCandidate> = None;

        for stage in stages {
            let start_idx = match state.search_after {
                Some(after) => stage
                    .candles_5m
                    .iter()
                    .position(|c| c.time > after)
                    .unwrap_or(stage.candles_5m.len()),
                None => 0,
            };
            if stage.candles_5m.len().saturating_sub(start_idx) < 3 {
                continue;
            }

            let scan_slice = &stage.candles_5m[start_idx..];
            let engine_cfg = SignalEngineConfig {
                strategy_id: self.strategy_id.clone(),
                stage_name: stage.name.clone(),
                or_high: stage.or_high,
                or_low: stage.or_low,
                fvg_expiry_candles: self.fvg_expiry_candles,
                entry_cutoff: self.entry_cutoff,
                require_or_breakout: require_or,
                min_or_breakout_pct: self.min_or_breakout_pct,
                long_only: self.long_only,
                confirmed_side: state.confirmed_side,
            };
            let detected = match self.signal_engine.detect_next(scan_slice, &engine_cfg) {
                Some(d) => d,
                None => continue,
            };
            let candidate = StageCandidate {
                stage: stage.clone(),
                retrace_candle_idx_abs: start_idx + detected.retrace_candle_idx,
                detected,
            };

            let replace = match &best {
                None => true,
                Some(current) => {
                    candidate.detected.intent.signal_time < current.detected.intent.signal_time
                        || (candidate.detected.intent.signal_time
                            == current.detected.intent.signal_time
                            && candidate.stage.stage_index < current.stage.stage_index)
                }
            };
            if replace {
                best = Some(candidate);
            }
        }

        best
    }
}

/// 체결 시뮬레이터.
///
/// - legacy mid_price: 리트레이스 확정 캔들은 이미 zone 안. 즉시 체결.
/// - passive top/bottom: 리트레이스 캔들의 [low, high] 안에 intended_entry 가
///   들어있는지 확인. 들어있으면 체결, 아니면 timeout 취소.
fn simulate_fill(plan: &EntryPlan, candle: &MinuteCandle) -> FillFeedback {
    use super::types::EntryMode::*;
    match plan.entry_mode {
        MidPriceZoneTouch => FillFeedback {
            signal_id: plan.signal_id,
            outcome: FillOutcome::Filled,
            filled_qty: plan.target_quantity_hint.unwrap_or(0),
            filled_price: Some(plan.intended_entry_price),
            fill_time: Some(candle.time),
            broker_order_id: None,
            resolution_source: FillResolutionSource::HistoricalSim,
            order_to_fill_ms: 0,
        },
        PassiveTopBottom { timeout_ms } => {
            let touched =
                candle.low <= plan.intended_entry_price && candle.high >= plan.intended_entry_price;
            if touched {
                FillFeedback {
                    signal_id: plan.signal_id,
                    outcome: FillOutcome::Filled,
                    filled_qty: plan.target_quantity_hint.unwrap_or(0),
                    filled_price: Some(plan.intended_entry_price),
                    fill_time: Some(candle.time),
                    broker_order_id: None,
                    resolution_source: FillResolutionSource::HistoricalSim,
                    order_to_fill_ms: 0,
                }
            } else {
                FillFeedback {
                    signal_id: plan.signal_id,
                    outcome: FillOutcome::Cancelled(CancelReason::Timeout),
                    filled_qty: 0,
                    filled_price: None,
                    fill_time: None,
                    broker_order_id: None,
                    resolution_source: FillResolutionSource::HistoricalSim,
                    order_to_fill_ms: timeout_ms as i64,
                }
            }
        }
        MarketableLimit { .. } => FillFeedback {
            signal_id: plan.signal_id,
            outcome: FillOutcome::Rejected("MarketableLimit 시뮬레이터 미구현".into()),
            filled_qty: 0,
            filled_price: None,
            fill_time: None,
            broker_order_id: None,
            resolution_source: FillResolutionSource::HistoricalSim,
            order_to_fill_ms: 0,
        },
    }
}

fn simulate_fill_intraday(
    plan: &EntryPlan,
    signal_time: NaiveTime,
    day_candles: &[MinuteCandle],
) -> FillFeedback {
    use super::types::EntryMode::*;

    match plan.entry_mode {
        MidPriceZoneTouch => FillFeedback {
            signal_id: plan.signal_id,
            outcome: FillOutcome::Filled,
            filled_qty: plan.target_quantity_hint.unwrap_or(0),
            filled_price: Some(plan.intended_entry_price),
            fill_time: Some(signal_time),
            broker_order_id: None,
            resolution_source: FillResolutionSource::HistoricalSim,
            order_to_fill_ms: 0,
        },
        PassiveTopBottom { timeout_ms } => {
            let deadline = signal_time + Duration::milliseconds(timeout_ms as i64);
            let touched_candle = day_candles.iter().find(|c| {
                c.time >= signal_time
                    && c.time <= deadline
                    && c.low <= plan.intended_entry_price
                    && c.high >= plan.intended_entry_price
            });
            if let Some(c) = touched_candle {
                let order_to_fill_ms = (c.time - signal_time).num_milliseconds().max(0);
                FillFeedback {
                    signal_id: plan.signal_id,
                    outcome: FillOutcome::Filled,
                    filled_qty: plan.target_quantity_hint.unwrap_or(0),
                    filled_price: Some(plan.intended_entry_price),
                    fill_time: Some(c.time),
                    broker_order_id: None,
                    resolution_source: FillResolutionSource::HistoricalSim,
                    order_to_fill_ms,
                }
            } else {
                FillFeedback {
                    signal_id: plan.signal_id,
                    outcome: FillOutcome::Cancelled(CancelReason::Timeout),
                    filled_qty: 0,
                    filled_price: None,
                    fill_time: None,
                    broker_order_id: None,
                    resolution_source: FillResolutionSource::HistoricalSim,
                    order_to_fill_ms: timeout_ms as i64,
                }
            }
        }
        MarketableLimit { .. } => FillFeedback {
            signal_id: plan.signal_id,
            outcome: FillOutcome::Rejected("MarketableLimit 시뮬레이터 미구현".into()),
            filled_qty: 0,
            filled_price: None,
            fill_time: None,
            broker_order_id: None,
            resolution_source: FillResolutionSource::HistoricalSim,
            order_to_fill_ms: 0,
        },
    }
}

fn resolve_signal_time_1m(
    detected: &super::signal_engine::DetectedSignal,
    day_candles: &[MinuteCandle],
) -> NaiveTime {
    let bucket_start = detected.intent.signal_time;
    let bucket_end = bucket_start + Duration::minutes(5);
    let side = detected.intent.side;

    day_candles
        .iter()
        .find(|c| {
            c.time >= bucket_start
                && c.time < bucket_end
                && match side {
                    PositionSide::Long => detected.gap.contains_price(c.low),
                    PositionSide::Short => detected.gap.contains_price(c.high),
                }
        })
        .map(|c| c.time)
        .unwrap_or(bucket_start)
}

fn advance_search_after(search_after: &mut Option<NaiveTime>, signal_time: NaiveTime) {
    let new_after = search_after.map_or(signal_time, |t| t.max(signal_time));
    *search_after = Some(new_after);
}

/// PositionManager helper 를 사용하는 청산 시뮬레이션.
/// orb_fvg::simulate_exit 와 동일 로직 — 단일 소스 유지.
#[allow(clippy::too_many_arguments)]
fn simulate_exit_with_pm(
    side: PositionSide,
    entry_price: i64,
    stop_loss: i64,
    take_profit: i64,
    entry_time: NaiveTime,
    remaining: &[MinuteCandle],
    pm_config: &PositionManagerConfig,
    force_exit: NaiveTime,
    intended_entry_price: i64,
    order_to_fill_ms: i64,
) -> Option<TradeResult> {
    let original_risk = (entry_price - stop_loss).abs();
    let mut current_sl = stop_loss;
    let mut best_price = entry_price;
    let mut reached_1r = false;

    for (i, c) in remaining.iter().enumerate() {
        // 갭 손절
        if open_gap_breach(side, current_sl, c.open) {
            let reason = gap_exit_reason(reached_1r);
            return Some(TradeResult {
                side,
                entry_price,
                exit_price: c.open,
                stop_loss,
                take_profit,
                entry_time,
                exit_time: c.time,
                exit_reason: reason,
                quantity: 0,
                intended_entry_price,
                order_to_fill_ms,
            });
        }

        // TP
        let tp_extreme = match side {
            PositionSide::Long => c.high,
            PositionSide::Short => c.low,
        };
        if is_tp_hit(side, take_profit, tp_extreme) {
            return Some(TradeResult {
                side,
                entry_price,
                exit_price: take_profit,
                stop_loss,
                take_profit,
                entry_time,
                exit_time: c.time,
                exit_reason: ExitReason::TakeProfit,
                quantity: 0,
                intended_entry_price,
                order_to_fill_ms,
            });
        }

        // best/1R/트레일링
        let favorable = match side {
            PositionSide::Long => c.high,
            PositionSide::Short => c.low,
        };
        let _ = update_best_and_trailing(
            side,
            entry_price,
            &mut best_price,
            &mut current_sl,
            &mut reached_1r,
            original_risk,
            favorable,
            pm_config,
        );

        // SL
        let sl_extreme = match side {
            PositionSide::Long => c.low,
            PositionSide::Short => c.high,
        };
        if is_sl_hit(side, current_sl, sl_extreme) {
            let reason = sl_exit_reason(side, current_sl, stop_loss, entry_price, reached_1r);
            return Some(TradeResult {
                side,
                entry_price,
                exit_price: current_sl,
                stop_loss,
                take_profit,
                entry_time,
                exit_time: c.time,
                exit_reason: reason,
                quantity: 0,
                intended_entry_price,
                order_to_fill_ms,
            });
        }

        // 시간 스탑
        if time_stop_breached_by_candles(i + 1, pm_config.time_stop_candles, reached_1r) {
            return Some(TradeResult {
                side,
                entry_price,
                exit_price: c.close,
                stop_loss,
                take_profit,
                entry_time,
                exit_time: c.time,
                exit_reason: ExitReason::TimeStop,
                quantity: 0,
                intended_entry_price,
                order_to_fill_ms,
            });
        }

        // EOD
        if c.time >= force_exit {
            return Some(TradeResult {
                side,
                entry_price,
                exit_price: c.close,
                stop_loss,
                take_profit,
                entry_time,
                exit_time: c.time,
                exit_reason: ExitReason::EndOfDay,
                quantity: 0,
                intended_entry_price,
                order_to_fill_ms,
            });
        }
    }

    remaining.last().map(|last| TradeResult {
        side,
        entry_price,
        exit_price: last.close,
        stop_loss,
        take_profit,
        entry_time,
        exit_time: last.time,
        exit_reason: ExitReason::EndOfDay,
        quantity: 0,
        intended_entry_price,
        order_to_fill_ms,
    })
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::super::execution_policy::{LegacyMidPriceSim, PassiveTopBottomTimeout30s};
    use super::super::signal_engine::OrbFvgSignalEngine;
    use super::super::types::{EntryMode, FillRecheckMode, StageDef};
    use super::*;

    fn candle(
        hour: u32,
        min: u32,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        vol: u64,
    ) -> MinuteCandle {
        MinuteCandle {
            date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
            time: NaiveTime::from_hms_opt(hour, min, 0).unwrap(),
            open,
            high,
            low,
            close,
            volume: vol,
        }
    }

    fn pm_cfg() -> PositionManagerConfig {
        PositionManagerConfig {
            rr_ratio: 2.5,
            trailing_r: 0.05,
            breakeven_r: 0.15,
            time_stop_candles: 3,
            candle_interval_min: 5,
        }
    }

    fn runner_legacy() -> ParityDayRunner<OrbFvgSignalEngine, LegacyMidPriceSim> {
        ParityDayRunner {
            signal_engine: OrbFvgSignalEngine,
            policy: LegacyMidPriceSim,
            strategy_id: "orb_fvg".into(),
            stage_name: "legacy_single_stage".into(),
            or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            entry_cutoff: NaiveTime::from_hms_opt(15, 20, 0).unwrap(),
            force_exit: NaiveTime::from_hms_opt(15, 25, 0).unwrap(),
            pm_config: pm_cfg(),
            fvg_expiry_candles: 6,
            min_or_breakout_pct: 0.001,
            long_only: true,
            rr_ratio: 2.5,
            max_entry_drift_pct: None,
            max_daily_trades: 5,
            max_daily_loss_pct: -1.5,
        }
    }

    fn runner_passive() -> ParityDayRunner<OrbFvgSignalEngine, PassiveTopBottomTimeout30s> {
        ParityDayRunner {
            signal_engine: OrbFvgSignalEngine,
            policy: PassiveTopBottomTimeout30s,
            strategy_id: "orb_fvg".into(),
            stage_name: "legacy_single_stage".into(),
            or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            entry_cutoff: NaiveTime::from_hms_opt(15, 20, 0).unwrap(),
            force_exit: NaiveTime::from_hms_opt(15, 25, 0).unwrap(),
            pm_config: pm_cfg(),
            fvg_expiry_candles: 6,
            min_or_breakout_pct: 0.001,
            long_only: true,
            rr_ratio: 2.5,
            max_entry_drift_pct: None,
            max_daily_trades: 5,
            max_daily_loss_pct: -1.5,
        }
    }

    /// OR 구간(09:00~09:10): high=10000, low=9900.
    /// A 캔들(09:15): range=100 (9950~10050).
    /// B 캔들(09:20): bullish body=360, OR 돌파, A.high < C.low.
    /// C 캔들(09:25): low=10200 > A.high=10050 → FVG [10050, 10200].
    /// D 캔들(09:30): retrace (low=10100) + intended(10200) touch.
    /// E 캔들(09:35): 큰 상승 → TP 도달.
    fn sample_day_candles() -> Vec<MinuteCandle> {
        let mut v = vec![
            candle(9, 0, 9_950, 9_980, 9_900, 9_970, 1_000),
            candle(9, 5, 9_970, 9_995, 9_920, 9_960, 1_000),
            candle(9, 10, 9_960, 10_000, 9_940, 9_980, 1_000),
            // A=09:15
            candle(9, 15, 9_960, 10_050, 9_950, 10_020, 1_500),
            // B=09:20 (OR 돌파, bullish, body>=30% of a_range)
            candle(9, 20, 10_020, 10_400, 10_020, 10_380, 3_000),
            // C=09:25 (FVG 형성: C.low=10200 > A.high=10050)
            candle(9, 25, 10_380, 10_420, 10_200, 10_220, 1_200),
            // D=09:30 (retrace + intended touch)
            candle(9, 30, 10_220, 10_230, 10_100, 10_150, 1_000),
            // E=09:35 (TP 도달)
            candle(9, 35, 10_150, 10_900, 10_140, 10_850, 2_500),
        ];
        // EOD 까지 횡보
        let mut t = NaiveTime::from_hms_opt(9, 40, 0).unwrap();
        let eod = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
        while t < eod {
            v.push(MinuteCandle {
                date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
                time: t,
                open: 10_850,
                high: 10_860,
                low: 10_840,
                close: 10_850,
                volume: 300,
            });
            t = t + chrono::Duration::minutes(5);
        }
        v
    }

    #[test]
    fn legacy_parity_runner_produces_trade() {
        let runner = runner_legacy();
        let trades = runner.run_day(&sample_day_candles());
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.side, PositionSide::Long);
        // legacy mid_price: (10050+10200)/2 = 10125.
        assert_eq!(t.entry_price, 10_125);
        // stop_anchor = A.low = 9950, risk = 175, TP = 10125 + 175*2.5 = 10562.
        assert_eq!(t.stop_loss, 9_950);
        assert_eq!(t.take_profit, 10_125 + (175_f64 * 2.5) as i64);
        // E 캔들 high=10900 ≥ TP → TakeProfit.
        assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    }

    #[test]
    fn passive_parity_runner_uses_top_price() {
        let runner = runner_passive();
        let trades = runner.run_day(&sample_day_candles());
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        // passive: entry = gap.top = 10200, risk = 250, TP = 10200 + 250*2.5 = 10825.
        assert_eq!(t.entry_price, 10_200);
        assert_eq!(t.stop_loss, 9_950);
        assert_eq!(t.take_profit, 10_200 + (250_f64 * 2.5) as i64);
        // E 캔들 high=10900 ≥ TP → TakeProfit.
        assert_eq!(t.exit_reason, ExitReason::TakeProfit);
        // 슬리피지 0 (intended_entry 에 정확히 체결).
        assert_eq!(t.intended_entry_price, 10_200);
    }

    #[test]
    fn passive_cancels_when_intended_not_touched() {
        // retrace 는 확정되지만 intended(gap.top=10200) 이 D 캔들 범위에서 빠짐.
        let candles = vec![
            candle(9, 0, 9_950, 9_980, 9_900, 9_970, 1_000),
            candle(9, 5, 9_970, 9_995, 9_920, 9_960, 1_000),
            candle(9, 10, 9_960, 10_000, 9_940, 9_980, 1_000),
            candle(9, 15, 9_960, 10_050, 9_950, 10_020, 1_500),
            candle(9, 20, 10_020, 10_400, 10_020, 10_380, 3_000),
            candle(9, 25, 10_380, 10_420, 10_200, 10_220, 1_200),
            // D: retrace (low=10100 ∈ [10050, 10200]) 이지만 intended=10200 touch 안 함 (high=10199)
            candle(9, 30, 10_150, 10_199, 10_100, 10_150, 1_000),
        ];
        let runner = runner_passive();
        let trades = runner.run_day(&candles);
        assert!(
            trades.is_empty(),
            "intended_entry 미터치 → cancel 되어 거래 없음"
        );
    }

    #[test]
    fn next_stage_candidate_returns_earliest_valid_stage() {
        let runner = runner_passive();
        let stages = vec![
            StageDef {
                name: "5m".into(),
                or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                or_end: NaiveTime::from_hms_opt(9, 5, 0).unwrap(),
            },
            StageDef {
                name: "15m".into(),
                or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            },
        ];
        let prepared = runner.prepare_stage_views(&sample_day_candles(), &stages);
        let candidate = runner
            .next_stage_candidate(&prepared, &ParityLoopState::default())
            .expect("candidate");
        assert_eq!(candidate.stage.name, "15m");
    }

    #[test]
    fn live_like_runner_produces_trade_with_multi_stage_input() {
        let runner = runner_passive();
        let stages = vec![
            StageDef {
                name: "5m".into(),
                or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                or_end: NaiveTime::from_hms_opt(9, 5, 0).unwrap(),
            },
            StageDef {
                name: "15m".into(),
                or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            },
            StageDef {
                name: "30m".into(),
                or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                or_end: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            },
        ];
        let trades = runner.run_day_live_like(&sample_day_candles(), &stages);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].entry_price, 10_200);
        assert_eq!(trades[0].exit_reason, ExitReason::TakeProfit);
    }

    #[test]
    fn passive_intraday_fill_respects_30s_timeout() {
        let plan = EntryPlan {
            signal_id: SignalId(7),
            side: PositionSide::Long,
            intended_entry_price: 10_200,
            order_price: Some(10_200),
            entry_mode: EntryMode::PassiveTopBottom { timeout_ms: 30_000 },
            max_entry_drift_pct: None,
            timeout_ms: 30_000,
            cancel_on_timeout: true,
            fill_recheck_mode: FillRecheckMode::BalanceRecheck,
            risk_amount: 250,
            planned_stop_loss: 9_950,
            planned_take_profit: 10_825,
            target_quantity_hint: Some(1),
        };
        let day = vec![
            candle(9, 30, 10_150, 10_199, 10_100, 10_150, 1_000),
            candle(9, 31, 10_150, 10_220, 10_140, 10_210, 1_000),
        ];
        let fill = simulate_fill_intraday(&plan, NaiveTime::from_hms_opt(9, 30, 0).unwrap(), &day);
        assert!(matches!(
            fill.outcome,
            FillOutcome::Cancelled(CancelReason::Timeout)
        ));
    }
}
