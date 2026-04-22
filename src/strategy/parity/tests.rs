//! Phase 1 단위 테스트.
//!
//! 목표:
//! 1. 기존 `orb_fvg::scan_and_trade` 의 legacy 진입 공식(`gap.mid_price()`)과
//!    `parity::conversions::legacy_mid_price_plan` 이 동일 값을 내는지 검증.
//! 2. 라이브 `poll_and_enter` 의 진입 공식(`gap.top`/`gap.bottom`)과
//!    `live_passive_plan` 이 동일 값을 내는지 검증.
//! 3. `Position` ↔ `PositionSnapshot` 왕복 시 필드 손실이 없는지 검증.

use chrono::NaiveTime;
use serde_json::json;

use crate::strategy::fvg::{FairValueGap, FvgDirection};
use crate::strategy::types::{ExitReason, Position, PositionSide, TradeResult};

use super::conversions::{
    build_signal_intent, legacy_mid_price_plan, live_filled, live_passive_plan,
    position_to_snapshot, side_from_fvg, synthetic_fill, trade_to_exit_decision,
};
use super::types::{
    EntryMode, ExitPriceModel, FillRecheckMode, FillResolutionSource, FillStatus, SignalId,
    SignalMetadata,
};

fn sample_gap_long() -> FairValueGap {
    // A.low=9950, A.high=10050, C.low=10200, (stop=A.low) 에 대응.
    // entry zone = [10050, 10200]. mid = 10125.
    FairValueGap {
        direction: FvgDirection::Bullish,
        top: 10_200,
        bottom: 10_050,
        candle_b_idx: 1,
        stop_loss: 9_950,
    }
}

fn sample_gap_short() -> FairValueGap {
    FairValueGap {
        direction: FvgDirection::Bearish,
        top: 10_200,
        bottom: 10_050,
        candle_b_idx: 1,
        stop_loss: 10_300,
    }
}

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

#[test]
fn side_from_fvg_roundtrip() {
    assert_eq!(side_from_fvg(FvgDirection::Bullish), PositionSide::Long);
    assert_eq!(side_from_fvg(FvgDirection::Bearish), PositionSide::Short);
}

#[test]
fn legacy_mid_price_matches_scan_and_trade_formula() {
    let gap = sample_gap_long();
    let intent = build_signal_intent(
        "orb_fvg",
        "15m",
        t(9, 20),
        &gap,
        10_300,
        9_900,
        SignalMetadata::default(),
    );

    let plan = legacy_mid_price_plan(&intent, 2.5);

    // scan_and_trade: entry = gap.mid_price() = (10200+10050)/2 = 10125
    //                 SL = a.low = 9950 (stop_anchor)
    //                 risk = |10125 - 9950| = 175
    //                 TP = 10125 + 175 * 2.5 = 10562 (i64 truncation)
    assert_eq!(plan.intended_entry_price, 10_125);
    assert_eq!(plan.planned_stop_loss, 9_950);
    assert_eq!(plan.risk_amount, 175);
    assert_eq!(plan.planned_take_profit, 10_125 + (175.0f64 * 2.5) as i64);
    assert_eq!(plan.entry_mode, EntryMode::MidPriceZoneTouch);
    assert!(plan.order_price.is_none());
    assert_eq!(plan.fill_recheck_mode, FillRecheckMode::Synthetic);
    assert!(!plan.cancel_on_timeout);
}

#[test]
fn live_passive_plan_uses_top_for_long() {
    let gap = sample_gap_long();
    let intent = build_signal_intent(
        "orb_fvg",
        "15m",
        t(9, 20),
        &gap,
        10_300,
        9_900,
        SignalMetadata::default(),
    );

    let plan = live_passive_plan(&intent, 2.5, Some(0.3), 30_000);

    // 라이브: Long 의 intended = gap.top = 10200, risk = |10200-9950| = 250,
    //        TP = 10200 + 250*2.5 = 10825
    assert_eq!(plan.intended_entry_price, 10_200);
    assert_eq!(plan.order_price, Some(10_200));
    assert_eq!(plan.risk_amount, 250);
    assert_eq!(plan.planned_take_profit, 10_200 + (250.0f64 * 2.5) as i64);
    assert_eq!(
        plan.entry_mode,
        EntryMode::PassiveTopBottom { timeout_ms: 30_000 }
    );
    assert!(plan.cancel_on_timeout);
    assert_eq!(plan.fill_recheck_mode, FillRecheckMode::BalanceRecheck);
    assert_eq!(plan.max_entry_drift_pct, Some(0.3));
    assert_eq!(plan.timeout_ms, 30_000);
}

#[test]
fn live_passive_plan_uses_bottom_for_short() {
    let gap = sample_gap_short();
    let intent = build_signal_intent(
        "orb_fvg",
        "15m",
        t(9, 25),
        &gap,
        10_400,
        10_000,
        SignalMetadata::default(),
    );

    let plan = live_passive_plan(&intent, 2.5, None, 30_000);

    // Short: intended = gap.bottom = 10050, stop_anchor = 10300 (A.high)
    // risk = |10050 - 10300| = 250
    // TP = 10050 - 250 * 2.5 = 9425
    assert_eq!(plan.intended_entry_price, 10_050);
    assert_eq!(plan.order_price, Some(10_050));
    assert_eq!(plan.planned_stop_loss, 10_300);
    assert_eq!(plan.risk_amount, 250);
    assert_eq!(plan.planned_take_profit, 10_050 - (250.0f64 * 2.5) as i64);
}

#[test]
fn position_to_snapshot_preserves_fields() {
    let pos = Position {
        side: PositionSide::Long,
        entry_price: 10_210,
        stop_loss: 9_950,
        take_profit: 10_850,
        entry_time: t(9, 25),
        quantity: 49,
        tp_order_no: Some("0001234567".into()),
        tp_krx_orgno: Some("0001".into()),
        tp_limit_price: Some(10_850),
        reached_1r: false,
        best_price: 10_310,
        original_sl: 9_950,
        sl_triggered_tick: false,
        intended_entry_price: 10_200,
        order_to_fill_ms: 1_234,
        signal_id: "legacy-test-signal".into(),
    };

    let snap = position_to_snapshot(&pos, SignalId(7), 250);

    assert_eq!(snap.signal_id, SignalId(7));
    assert_eq!(snap.side, PositionSide::Long);
    assert_eq!(snap.entry_price, 10_210);
    assert_eq!(snap.stop_loss, 9_950);
    assert_eq!(snap.take_profit, 10_850);
    assert_eq!(snap.entry_time, t(9, 25));
    assert_eq!(snap.quantity, 49);
    assert_eq!(snap.reached_1r, false);
    assert_eq!(snap.best_price, 10_310);
    assert_eq!(snap.original_sl, 9_950);
    assert_eq!(snap.original_risk, 250);
    assert_eq!(snap.intended_entry_price, 10_200);
    assert_eq!(snap.order_to_fill_ms, 1_234);
    // broker 전용 필드(tp_order_no/sl_triggered_tick 등)는 스냅샷에 포함되지 않음 — 의도된 설계.
}

#[test]
fn synthetic_fill_has_zero_slippage() {
    let gap = sample_gap_long();
    let intent = build_signal_intent(
        "orb_fvg",
        "5m",
        t(9, 20),
        &gap,
        10_300,
        9_900,
        SignalMetadata::default(),
    );
    let mut plan = legacy_mid_price_plan(&intent, 2.0);
    plan.target_quantity_hint = Some(10);

    let fill = synthetic_fill(&plan, t(9, 21));

    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.filled_qty, 10);
    assert_eq!(fill.filled_price, Some(plan.intended_entry_price));
    assert_eq!(fill.slippage, Some(0));
    assert_eq!(fill.resolution_source, FillResolutionSource::HistoricalSim);
    assert_eq!(fill.fill_time, Some(t(9, 21)));
}

#[test]
fn live_filled_slippage_sign_rules() {
    let gap = sample_gap_long();
    let intent_long = build_signal_intent(
        "orb_fvg",
        "15m",
        t(9, 21),
        &gap,
        10_300,
        9_900,
        SignalMetadata::default(),
    );
    let plan_long = live_passive_plan(&intent_long, 2.5, None, 30_000);
    // 계획가 10200, 실제 체결가 10215 → Long 에게 불리 → slippage +15.
    let fill = live_filled(
        &plan_long,
        49,
        10_215,
        t(9, 22),
        Some("KRX-0001".into()),
        FillResolutionSource::WsNotice,
        1_250,
    );
    assert_eq!(fill.slippage, Some(15));

    // Short: 계획가 10050, 실제 체결가 10030 → Short 에게 불리 → slippage +20.
    let gap_s = sample_gap_short();
    let intent_short = build_signal_intent(
        "orb_fvg",
        "15m",
        t(9, 25),
        &gap_s,
        10_400,
        10_000,
        SignalMetadata::default(),
    );
    let plan_short = live_passive_plan(&intent_short, 2.5, None, 30_000);
    let fill_s = live_filled(
        &plan_short,
        49,
        10_030,
        t(9, 26),
        None,
        FillResolutionSource::DailyCcld,
        1_500,
    );
    assert_eq!(fill_s.slippage, Some(20));
}

#[test]
fn trade_to_exit_decision_picks_model() {
    let snap = PositionSnapshotFixture::default().into_snapshot();

    let tp_trade = TradeResult {
        side: PositionSide::Long,
        entry_price: 10_125,
        exit_price: 10_562,
        stop_loss: 9_950,
        take_profit: 10_562,
        entry_time: t(9, 21),
        exit_time: t(10, 10),
        exit_reason: ExitReason::TakeProfit,
        quantity: 0,
        intended_entry_price: 10_125,
        order_to_fill_ms: 0,
    };
    let dec = trade_to_exit_decision(&tp_trade, snap.clone());
    assert!(dec.should_exit);
    assert_eq!(dec.reason, Some(ExitReason::TakeProfit));
    assert_eq!(dec.exit_price_model, ExitPriceModel::LimitAtLevel);

    let eod_trade = TradeResult {
        exit_reason: ExitReason::EndOfDay,
        exit_price: 9_800,
        exit_time: t(15, 25),
        ..tp_trade.clone()
    };
    let dec_eod = trade_to_exit_decision(&eod_trade, snap.clone());
    assert_eq!(dec_eod.exit_price_model, ExitPriceModel::CandleClose);

    let gap_trade = TradeResult {
        exit_reason: ExitReason::StopLoss,
        exit_price: 9_800, // SL 9950 보다 아래 → 갭 손절 (시가 체결)
        ..tp_trade.clone()
    };
    let dec_gap = trade_to_exit_decision(&gap_trade, snap);
    assert_eq!(dec_gap.exit_price_model, ExitPriceModel::OpenGap);
}

struct PositionSnapshotFixture {
    side: PositionSide,
    entry_price: i64,
    stop_loss: i64,
    take_profit: i64,
}

impl Default for PositionSnapshotFixture {
    fn default() -> Self {
        Self {
            side: PositionSide::Long,
            entry_price: 10_125,
            stop_loss: 9_950,
            take_profit: 10_562,
        }
    }
}

impl PositionSnapshotFixture {
    fn into_snapshot(self) -> super::types::PositionSnapshot {
        super::types::PositionSnapshot {
            signal_id: SignalId(1),
            side: self.side,
            entry_price: self.entry_price,
            stop_loss: self.stop_loss,
            take_profit: self.take_profit,
            quantity: 10,
            entry_time: t(9, 21),
            reached_1r: false,
            best_price: self.entry_price,
            original_sl: self.stop_loss,
            original_risk: (self.entry_price - self.stop_loss).abs(),
            intended_entry_price: self.entry_price,
            order_to_fill_ms: 0,
            strategy_metadata: json!({}),
        }
    }
}
