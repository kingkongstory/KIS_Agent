//! 백테스트-라이브 정합(parity) 계층.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` 의 구현. 이 모듈은
//! legacy 백테스트, parity 백테스트, replay, live 네 런타임이 공유할 **공통
//! 데이터 계약**을 정의한다.
//!
//! ## Phase 별 확장 계획
//! - **Phase 1 (현재)**: `types` + `conversions` 만. 기존 동작 불변.
//! - **Phase 2**: `signal_engine` 추가. `OrbFvgSignalEngine` 구현체.
//! - **Phase 3**: `position_manager` 추가. `OrbFvgPositionManager` 구현체.
//! - **Phase 4**: `execution_policy` 추가. `PassiveTopBottomTimeout30s` 구현체.
//! - **Phase 5+**: `parity_backtest`, `replay` 러너 추가.

pub mod conversions;
pub mod execution_policy;
pub mod parity_backtest;
pub mod position_manager;
pub mod replay;
pub mod signal_engine;
pub mod types;

#[cfg(test)]
mod tests;

pub use conversions::{
    build_signal_intent, legacy_mid_price_plan, live_filled, live_passive_plan,
    position_to_snapshot, side_from_fvg, synthetic_fill, trade_to_exit_decision,
};
pub use execution_policy::{
    ExecutionContext, ExecutionPolicy, FillFeedback, FillOutcome, LegacyMidPriceSim,
    PassiveTopBottomTimeout30s,
};
pub use parity_backtest::ParityDayRunner;
pub use position_manager::{
    PositionManagerConfig, TrailingUpdate, gap_exit_reason, is_sl_hit, is_tp_hit, open_gap_breach,
    sl_exit_reason, time_stop_breached_by_candles, time_stop_breached_by_minutes,
    update_best_and_trailing,
};
pub use replay::ReplayDayReport;
pub use signal_engine::{
    DetectedSignal, OrbFvgSignalEngine, SignalEngine, SignalEngineConfig, detect_next_fvg_signal,
};
pub use types::{
    CancelReason, EntryMode, EntryPlan, ExitDecision, ExitPriceModel, FillRecheckMode,
    FillResolutionSource, FillResult, FillStatus, MarketRegimeFlags, PositionSnapshot,
    SignalContext, SignalId, SignalIntent, SignalMetadata, StageDef,
};
