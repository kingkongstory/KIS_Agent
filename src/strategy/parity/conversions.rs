//! 기존 타입(`FairValueGap`, `Position`, `ActivePosition`, `TradeResult`) 과
//! `parity::types` 사이의 변환 helper.
//!
//! Phase 1 원칙상 이 helper는 기존 호출부를 수정하지 않고, 새로운 parity
//! 계층이 기존 데이터를 **읽기 위해서만** 사용한다. Phase 2 이후 SignalEngine/
//! ExecutionPolicy/PositionManager가 추출될 때 이 helper들이 경계가 된다.

use chrono::NaiveTime;
use serde_json::json;

use crate::strategy::fvg::{FairValueGap, FvgDirection};
use crate::strategy::types::{ExitReason, Position, PositionSide, TradeResult};

use super::types::{
    EntryMode, EntryPlan, ExitDecision, ExitPriceModel, FillRecheckMode, FillResolutionSource,
    FillResult, FillStatus, PositionSnapshot, SignalId, SignalIntent, SignalMetadata,
};

/// `FvgDirection` → `PositionSide` 단방향 매핑.
pub fn side_from_fvg(direction: FvgDirection) -> PositionSide {
    match direction {
        FvgDirection::Bullish => PositionSide::Long,
        FvgDirection::Bearish => PositionSide::Short,
    }
}

/// `FairValueGap` + OR 범위 → `SignalIntent` 빌더.
///
/// SignalEngine 추출 전 단계에서 테스트 용도로 사용. 주입된 `metadata`가 없으면
/// 기본값(0)으로 채운다.
pub fn build_signal_intent(
    strategy_id: &str,
    stage: &str,
    signal_time: NaiveTime,
    gap: &FairValueGap,
    or_high: i64,
    or_low: i64,
    metadata: SignalMetadata,
) -> SignalIntent {
    SignalIntent {
        strategy_id: strategy_id.to_string(),
        side: side_from_fvg(gap.direction),
        signal_time,
        stage_name: stage.to_string(),
        entry_zone_high: gap.top,
        entry_zone_low: gap.bottom,
        stop_anchor: gap.stop_loss,
        or_high,
        or_low,
        metadata,
    }
}

/// 백테스트 legacy 진입 규칙(`gap.mid_price()`)을 EntryPlan으로 표현.
///
/// `orb_fvg::scan_and_trade`가 내부에서 즉시 계산하던 entry/SL/TP를 같은
/// 공식으로 재현한다. Phase 1 parity 테스트에서 "legacy 계산식 ↔ parity
/// 헬퍼"가 동일 값을 돌려주는지 검증하는 기준.
pub fn legacy_mid_price_plan(intent: &SignalIntent, rr_ratio: f64) -> EntryPlan {
    let intended = (intent.entry_zone_high + intent.entry_zone_low) / 2;
    build_plan(
        intent,
        intended,
        rr_ratio,
        EntryMode::MidPriceZoneTouch,
        None,
        0,
        false,
        FillRecheckMode::Synthetic,
        None,
    )
}

/// 라이브 현행(패시브 top/bottom 지정가) 진입 규칙을 EntryPlan으로 표현.
pub fn live_passive_plan(
    intent: &SignalIntent,
    rr_ratio: f64,
    max_entry_drift_pct: Option<f64>,
    timeout_ms: u64,
) -> EntryPlan {
    let intended = match intent.side {
        PositionSide::Long => intent.entry_zone_high,
        PositionSide::Short => intent.entry_zone_low,
    };
    build_plan(
        intent,
        intended,
        rr_ratio,
        EntryMode::PassiveTopBottom { timeout_ms },
        max_entry_drift_pct,
        timeout_ms,
        true,
        FillRecheckMode::BalanceRecheck,
        Some(intended),
    )
}

/// 2026-04-18 Phase 3 (execution-timing-implementation-plan):
/// 시장성 지정가 (best_ask + N tick / best_bid - N tick) 진입 규칙을 EntryPlan으로 표현.
///
/// `intended_entry_price` 는 zone edge 유지 (슬리피지 측정 기준). 실제 주문가
/// (`order_price`) 는 **adapter 런타임에서** best_ask/bid (또는 현재가 fallback) + tick 으로
/// 결정되므로 여기서는 `None` 을 리턴. `slippage_budget_pct` 로 이탈 한계를 보존한다.
///
/// 현행 `PassiveTopBottomTimeout30s` 대비 변경점:
/// - timeout_ms: 30_000 → 2_000~3_000 (빠르게 실패 확정)
/// - intended 는 동일 (zone edge) 이지만 order_price 는 adapter 가 추가 tick 반영
pub fn live_marketable_plan(
    intent: &SignalIntent,
    rr_ratio: f64,
    max_entry_drift_pct: Option<f64>,
    slippage_budget_pct: f64,
    timeout_ms: u64,
) -> EntryPlan {
    let intended = match intent.side {
        PositionSide::Long => intent.entry_zone_high,
        PositionSide::Short => intent.entry_zone_low,
    };
    build_plan(
        intent,
        intended,
        rr_ratio,
        EntryMode::MarketableLimit {
            slippage_budget_pct,
        },
        max_entry_drift_pct,
        timeout_ms,
        true,
        FillRecheckMode::BalanceRecheck,
        // order_price: None — adapter 가 best_ask/bid + tick 으로 런타임 결정.
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_plan(
    intent: &SignalIntent,
    intended: i64,
    rr_ratio: f64,
    entry_mode: EntryMode,
    max_entry_drift_pct: Option<f64>,
    timeout_ms: u64,
    cancel_on_timeout: bool,
    fill_recheck_mode: FillRecheckMode,
    order_price: Option<i64>,
) -> EntryPlan {
    let risk = (intended - intent.stop_anchor).abs();
    let (sl, tp) = match intent.side {
        PositionSide::Long => (
            intent.stop_anchor,
            intended + (risk as f64 * rr_ratio) as i64,
        ),
        PositionSide::Short => (
            intent.stop_anchor,
            intended - (risk as f64 * rr_ratio) as i64,
        ),
    };
    EntryPlan {
        signal_id: SignalId::UNSET,
        side: intent.side,
        intended_entry_price: intended,
        order_price,
        entry_mode,
        max_entry_drift_pct,
        timeout_ms,
        cancel_on_timeout,
        fill_recheck_mode,
        risk_amount: risk,
        planned_stop_loss: sl,
        planned_take_profit: tp,
        target_quantity_hint: None,
    }
}

/// 라이브 `types::Position` → parity `PositionSnapshot`.
///
/// `tp_order_no`, `sl_triggered_tick` 등 broker 전용 필드는 스냅샷에 실리지
/// 않는다. 이 필드들은 live adapter가 별도로 유지한다.
pub fn position_to_snapshot(
    pos: &Position,
    signal_id: SignalId,
    original_risk: i64,
) -> PositionSnapshot {
    PositionSnapshot {
        signal_id,
        side: pos.side,
        entry_price: pos.entry_price,
        stop_loss: pos.stop_loss,
        take_profit: pos.take_profit,
        quantity: pos.quantity,
        entry_time: pos.entry_time,
        reached_1r: pos.reached_1r,
        best_price: pos.best_price,
        original_sl: pos.original_sl,
        original_risk,
        intended_entry_price: pos.intended_entry_price,
        order_to_fill_ms: pos.order_to_fill_ms,
        strategy_metadata: json!({}),
    }
}

/// 합성 체결(백테스트) FillResult 생성 helper.
///
/// 백테스트는 체결가와 intended_entry가 같다고 가정(`scan_and_trade`가
/// `intended_entry_price: entry_price`로 기록하는 것과 동치).
pub fn synthetic_fill(plan: &EntryPlan, fill_time: NaiveTime) -> FillResult {
    FillResult {
        signal_id: plan.signal_id,
        status: FillStatus::Filled,
        filled_qty: plan.target_quantity_hint.unwrap_or(0),
        filled_price: Some(plan.intended_entry_price),
        slippage: Some(0),
        fill_time: Some(fill_time),
        broker_order_id: None,
        resolution_source: FillResolutionSource::HistoricalSim,
        order_to_fill_ms: 0,
    }
}

/// 라이브 체결 완료 후 FillResult 생성.
///
/// slippage 부호 규약: `filled_price - intended_entry_price` (양수 = 불리).
/// Long이면 계획가보다 비싸게 산 것이 불리, Short이면 싸게 판 것이 불리.
pub fn live_filled(
    plan: &EntryPlan,
    filled_qty: u64,
    filled_price: i64,
    fill_time: NaiveTime,
    broker_order_id: Option<String>,
    resolution_source: FillResolutionSource,
    order_to_fill_ms: i64,
) -> FillResult {
    let slippage = match plan.side {
        PositionSide::Long => filled_price - plan.intended_entry_price,
        PositionSide::Short => plan.intended_entry_price - filled_price,
    };
    FillResult {
        signal_id: plan.signal_id,
        status: FillStatus::Filled,
        filled_qty,
        filled_price: Some(filled_price),
        slippage: Some(slippage),
        fill_time: Some(fill_time),
        broker_order_id,
        resolution_source,
        order_to_fill_ms,
    }
}

/// `TradeResult` + 진입 시점 PositionSnapshot → 최종 ExitDecision 재구성.
///
/// Phase 1 parity 테스트에서 backtest 결과가 ExitDecision 모델로 완전히
/// 표현 가능한지 확인하기 위한 역변환 helper.
pub fn trade_to_exit_decision(
    trade: &TradeResult,
    final_position: PositionSnapshot,
) -> ExitDecision {
    let exit_price_model = match trade.exit_reason {
        ExitReason::TakeProfit => ExitPriceModel::LimitAtLevel,
        ExitReason::StopLoss | ExitReason::BreakevenStop | ExitReason::TrailingStop => {
            if trade.exit_price == trade.stop_loss
                || trade.exit_price == final_position.stop_loss
                || trade.exit_price == final_position.original_sl
            {
                ExitPriceModel::LimitAtLevel
            } else {
                ExitPriceModel::OpenGap
            }
        }
        ExitReason::TimeStop | ExitReason::EndOfDay => ExitPriceModel::CandleClose,
    };
    ExitDecision {
        should_exit: true,
        reason: Some(trade.exit_reason),
        exit_price_model,
        effective_stop: final_position.stop_loss,
        effective_take_profit: final_position.take_profit,
        decision_time: trade.exit_time,
        next_position: final_position,
    }
}
