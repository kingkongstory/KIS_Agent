//! ExecutionPolicy — 진입 실행 규칙의 명문화.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` §10 구현.
//!
//! SignalEngine 이 만든 `SignalIntent` 를 입력받아, "어떤 가격, 어떤 timeout,
//! 어떤 drift budget, 어떤 체결 규칙으로" 실행할지를 계산한다. 주문 발주·체결
//! 확인 I/O 는 여기서 일어나지 않는다 — broker adapter(라이브)와 backtest
//! fill simulator(parity backtest)가 이 모듈의 반환값을 소비한다.
//!
//! ## Phase 4 구현체
//! - `PassiveTopBottomTimeout30s`: 현재 live 의 entry 규칙을 그대로 명문화.
//!   zone 경계로 지정가, 30초 timeout, cancel 후 balance 재조회.
//! - `LegacyMidPriceSim`: 기존 백테스트의 `gap.mid_price()` 진입 모델. legacy
//!   baseline 재현 전용.
//!
//! ## Phase 5 이후 확장점
//! - `MarketableLimitWithSlippageBudget`: best ask/bid + N tick 으로 시장성
//!   지정가. Phase 7 새 전략(orb_vwap_pullback) 의 기본 정책.

use super::conversions::{legacy_mid_price_plan, live_marketable_plan, live_passive_plan};
use super::types::{
    EntryPlan, FillResolutionSource, FillResult, FillStatus, SignalId, SignalIntent,
};

/// ExecutionPolicy trait.
///
/// `plan()` 은 pure — 같은 signal 이면 같은 EntryPlan 을 돌려준다.
/// `resolve_fill()` 은 체결 결과를 분석하여 FillResult 를 만든다 (역시 pure).
pub trait ExecutionPolicy {
    /// 정책 식별자. BacktestReport.execution_version 과 일치.
    fn version(&self) -> &'static str;

    /// SignalIntent → EntryPlan.
    fn plan(&self, intent: &SignalIntent, ctx: &ExecutionContext) -> EntryPlan;

    /// 체결 피드백을 FillResult 로 정규화. broker adapter 가 만든 feedback 을
    /// 받아 같은 형식으로 돌려준다.
    fn resolve_fill(&self, plan: &EntryPlan, feedback: &FillFeedback) -> FillResult;
}

/// plan() 에 필요한 부가 정보. 상위 adapter 가 채운다.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionContext {
    /// 손익비. PositionManager / ExecutionPolicy 공통 사용.
    pub rr_ratio: f64,
    /// drift 허용치 (Passive 정책에서만 사용). None 이면 무제한.
    pub max_entry_drift_pct: Option<f64>,
    /// 시장성 지정가의 slippage budget (Marketable 정책용).
    pub slippage_budget_pct: Option<f64>,
}

impl ExecutionContext {
    pub fn legacy(rr_ratio: f64) -> Self {
        Self {
            rr_ratio,
            max_entry_drift_pct: None,
            slippage_budget_pct: None,
        }
    }

    pub fn live(rr_ratio: f64, max_entry_drift_pct: Option<f64>) -> Self {
        Self {
            rr_ratio,
            max_entry_drift_pct,
            slippage_budget_pct: None,
        }
    }

    /// 2026-04-18 Phase 3: 시장성 지정가 정책용 context.
    pub fn marketable(
        rr_ratio: f64,
        max_entry_drift_pct: Option<f64>,
        slippage_budget_pct: f64,
    ) -> Self {
        Self {
            rr_ratio,
            max_entry_drift_pct,
            slippage_budget_pct: Some(slippage_budget_pct),
        }
    }
}

/// broker/simulator 가 제공하는 체결 피드백.
#[derive(Debug, Clone)]
pub struct FillFeedback {
    pub signal_id: SignalId,
    pub outcome: FillOutcome,
    pub filled_qty: u64,
    pub filled_price: Option<i64>,
    pub fill_time: Option<chrono::NaiveTime>,
    pub broker_order_id: Option<String>,
    pub resolution_source: FillResolutionSource,
    pub order_to_fill_ms: i64,
}

#[derive(Debug, Clone)]
pub enum FillOutcome {
    Filled,
    PartialFill,
    Cancelled(super::types::CancelReason),
    Rejected(String),
    ManualIntervention(String),
}

// ───────────────────────────────────────────────────────────────
// PassiveTopBottomTimeout30s — 현재 live 정책의 명문화.
// ───────────────────────────────────────────────────────────────

/// 현재 라이브 (`execute_entry`) 의 진입 규칙을 그대로 명문화한 정책.
///
/// 규칙:
/// - intended_entry = Long: gap.top, Short: gap.bottom
/// - order_price = intended (호가단위 정렬은 adapter 책임)
/// - timeout: 30,000ms
/// - cancel_on_timeout: true
/// - fill_recheck_mode: BalanceRecheck
/// - drift guard: `max_entry_drift_pct` 이상이면 plan 만들지 않음 (adapter 가
///   SignalIntent 단계에서 drop). 이 정책은 drift guard 를 plan 에 **기록**만
///   하고, 실제 차단은 adapter 에서 수행.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassiveTopBottomTimeout30s;

impl ExecutionPolicy for PassiveTopBottomTimeout30s {
    fn version(&self) -> &'static str {
        "passive_top_bottom_timeout_30s"
    }

    fn plan(&self, intent: &SignalIntent, ctx: &ExecutionContext) -> EntryPlan {
        live_passive_plan(intent, ctx.rr_ratio, ctx.max_entry_drift_pct, 30_000)
    }

    fn resolve_fill(&self, plan: &EntryPlan, feedback: &FillFeedback) -> FillResult {
        resolve_fill_common(plan, feedback, FillResolutionSource::BalanceRecheck)
    }
}

// ───────────────────────────────────────────────────────────────
// MarketableLimitWithBudget — Phase 3 (2026-04-18) 신규 정책.
// ───────────────────────────────────────────────────────────────

/// 시장성 지정가 + slippage budget.
///
/// `docs/monitoring/2026-04-18-execution-timing-implementation-plan.md` Phase 3.
/// 패시브 지정가가 "신호 타이밍 ↔ 체결 타이밍" 괴리를 만드는 문제를 해결하기 위한 정책.
///
/// 규칙:
/// - `intended_entry` = Long: zone edge top / Short: zone edge bottom (슬리피지 측정 기준)
/// - `order_price` = None — adapter 런타임에서 best_ask + N tick / best_bid - N tick 계산
/// - `timeout_ms` = 2_000~3_000 (빠른 실패 확정)
/// - `cancel_on_timeout` = true
/// - `fill_recheck_mode` = BalanceRecheck
/// - `slippage_budget_pct` = 0.3% 기본 (zone edge 대비 이 이상 떨어진 체결가는 불리 판정)
///
/// 실제 주문 가격은 adapter 가 호가 스냅샷 (또는 현재가 fallback) + tick 으로 결정.
/// adapter 는 ExecutionPolicy 가 계산한 intended_entry 와의 차이가
/// `slippage_budget_pct` 를 초과하면 주문을 skip 해야 한다 (preflight gate).
#[derive(Debug, Clone, Copy)]
pub struct MarketableLimitWithBudget {
    pub slippage_budget_pct: f64,
    pub timeout_ms: u64,
}

impl Default for MarketableLimitWithBudget {
    fn default() -> Self {
        Self {
            slippage_budget_pct: 0.003, // 0.3%
            timeout_ms: 3_000,
        }
    }
}

impl ExecutionPolicy for MarketableLimitWithBudget {
    fn version(&self) -> &'static str {
        "marketable_limit_with_budget_v1"
    }

    fn plan(&self, intent: &SignalIntent, ctx: &ExecutionContext) -> EntryPlan {
        // slippage_budget_pct 는 context 값이 있으면 우선, 없으면 정책 기본값.
        let budget = ctx.slippage_budget_pct.unwrap_or(self.slippage_budget_pct);
        live_marketable_plan(
            intent,
            ctx.rr_ratio,
            ctx.max_entry_drift_pct,
            budget,
            self.timeout_ms,
        )
    }

    fn resolve_fill(&self, plan: &EntryPlan, feedback: &FillFeedback) -> FillResult {
        resolve_fill_common(plan, feedback, FillResolutionSource::BalanceRecheck)
    }
}

// ───────────────────────────────────────────────────────────────
// LegacyMidPriceSim — 기존 백테스트의 mid_price 즉시 체결 모델.
// ───────────────────────────────────────────────────────────────

/// 기존 백테스트(`orb_fvg::scan_and_trade`)의 `gap.mid_price()` 즉시 체결 모델.
///
/// parity backtest 가 legacy 와 비교하기 위해 보존한다. 새로운 실거래 판단의
/// 기준선(baseline)으로만 사용.
#[derive(Debug, Clone, Copy, Default)]
pub struct LegacyMidPriceSim;

impl ExecutionPolicy for LegacyMidPriceSim {
    fn version(&self) -> &'static str {
        "mid_price_zone_touch"
    }

    fn plan(&self, intent: &SignalIntent, ctx: &ExecutionContext) -> EntryPlan {
        legacy_mid_price_plan(intent, ctx.rr_ratio)
    }

    fn resolve_fill(&self, plan: &EntryPlan, feedback: &FillFeedback) -> FillResult {
        resolve_fill_common(plan, feedback, FillResolutionSource::HistoricalSim)
    }
}

fn resolve_fill_common(
    plan: &EntryPlan,
    feedback: &FillFeedback,
    default_resolution: FillResolutionSource,
) -> FillResult {
    let status = match feedback.outcome.clone() {
        FillOutcome::Filled => FillStatus::Filled,
        FillOutcome::PartialFill => FillStatus::PartialFill,
        FillOutcome::Cancelled(reason) => FillStatus::Cancelled(reason),
        FillOutcome::Rejected(msg) => FillStatus::Rejected(msg),
        FillOutcome::ManualIntervention(msg) => FillStatus::ManualIntervention(msg),
    };
    let slippage = feedback.filled_price.map(|fp| match plan.side {
        crate::strategy::types::PositionSide::Long => fp - plan.intended_entry_price,
        crate::strategy::types::PositionSide::Short => plan.intended_entry_price - fp,
    });
    // broker 가 명시한 resolution_source 가 있으면 그걸 우선, 아니면 정책 기본값.
    let resolution_source = if matches!(
        feedback.resolution_source,
        FillResolutionSource::HistoricalSim
    ) {
        default_resolution
    } else {
        feedback.resolution_source
    };
    FillResult {
        signal_id: feedback.signal_id,
        status,
        filled_qty: feedback.filled_qty,
        filled_price: feedback.filled_price,
        slippage,
        fill_time: feedback.fill_time,
        broker_order_id: feedback.broker_order_id.clone(),
        resolution_source,
        order_to_fill_ms: feedback.order_to_fill_ms,
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use super::super::conversions::build_signal_intent;
    use super::super::types::{CancelReason, EntryMode, FillRecheckMode, SignalMetadata};
    use super::*;
    use crate::strategy::fvg::{FairValueGap, FvgDirection};

    fn sample_intent_long() -> SignalIntent {
        let gap = FairValueGap {
            direction: FvgDirection::Bullish,
            top: 10_200,
            bottom: 10_050,
            candle_b_idx: 1,
            stop_loss: 9_950,
        };
        build_signal_intent(
            "orb_fvg",
            "15m",
            NaiveTime::from_hms_opt(9, 20, 0).unwrap(),
            &gap,
            10_300,
            9_900,
            SignalMetadata::default(),
        )
    }

    #[test]
    fn passive_policy_builds_top_plan() {
        let policy = PassiveTopBottomTimeout30s;
        let ctx = ExecutionContext::live(2.5, Some(0.3));
        let plan = policy.plan(&sample_intent_long(), &ctx);
        assert_eq!(plan.intended_entry_price, 10_200);
        assert_eq!(plan.order_price, Some(10_200));
        assert_eq!(plan.max_entry_drift_pct, Some(0.3));
        assert_eq!(plan.timeout_ms, 30_000);
        assert!(plan.cancel_on_timeout);
        assert_eq!(plan.fill_recheck_mode, FillRecheckMode::BalanceRecheck);
        assert_eq!(
            plan.entry_mode,
            EntryMode::PassiveTopBottom { timeout_ms: 30_000 }
        );
        assert_eq!(policy.version(), "passive_top_bottom_timeout_30s");
    }

    #[test]
    fn legacy_policy_builds_mid_price_plan() {
        let policy = LegacyMidPriceSim;
        let ctx = ExecutionContext::legacy(2.5);
        let plan = policy.plan(&sample_intent_long(), &ctx);
        assert_eq!(plan.intended_entry_price, 10_125);
        assert!(plan.order_price.is_none());
        assert_eq!(plan.entry_mode, EntryMode::MidPriceZoneTouch);
        assert_eq!(plan.fill_recheck_mode, FillRecheckMode::Synthetic);
        assert_eq!(policy.version(), "mid_price_zone_touch");
    }

    #[test]
    fn resolve_fill_maps_cancelled_reason() {
        let policy = PassiveTopBottomTimeout30s;
        let ctx = ExecutionContext::live(2.5, None);
        let plan = policy.plan(&sample_intent_long(), &ctx);
        let feedback = FillFeedback {
            signal_id: SignalId(42),
            outcome: FillOutcome::Cancelled(CancelReason::Timeout),
            filled_qty: 0,
            filled_price: None,
            fill_time: None,
            broker_order_id: None,
            resolution_source: FillResolutionSource::BalanceRecheck,
            order_to_fill_ms: 30_000,
        };
        let result = policy.resolve_fill(&plan, &feedback);
        assert_eq!(result.status, FillStatus::Cancelled(CancelReason::Timeout));
        assert_eq!(result.filled_qty, 0);
        assert!(result.slippage.is_none());
        assert_eq!(result.signal_id, SignalId(42));
    }

    #[test]
    fn resolve_fill_computes_slippage_sign() {
        let policy = PassiveTopBottomTimeout30s;
        let ctx = ExecutionContext::live(2.5, None);
        let plan = policy.plan(&sample_intent_long(), &ctx); // intended=10200
        let feedback = FillFeedback {
            signal_id: SignalId(1),
            outcome: FillOutcome::Filled,
            filled_qty: 49,
            filled_price: Some(10_220), // 20원 비싸게 체결 → 불리
            fill_time: Some(NaiveTime::from_hms_opt(9, 21, 0).unwrap()),
            broker_order_id: Some("KRX-0001".into()),
            resolution_source: FillResolutionSource::WsNotice,
            order_to_fill_ms: 1_200,
        };
        let result = policy.resolve_fill(&plan, &feedback);
        // Long: filled(10220) - intended(10200) = +20 (불리)
        assert_eq!(result.slippage, Some(20));
        assert_eq!(result.status, FillStatus::Filled);
        assert_eq!(result.resolution_source, FillResolutionSource::WsNotice);
    }

    /// 2026-04-18 Phase 3: MarketableLimitWithBudget plan 회귀.
    #[test]
    fn marketable_policy_uses_zone_edge_and_leaves_order_price_for_adapter() {
        let policy = MarketableLimitWithBudget::default();
        let ctx = ExecutionContext::live(2.5, Some(0.3));
        let plan = policy.plan(&sample_intent_long(), &ctx);
        // intended_entry 는 PassiveTopBottom 과 동일 (zone edge).
        assert_eq!(plan.intended_entry_price, 10_200);
        // order_price 는 None — adapter 가 best_ask/bid + tick 으로 결정.
        assert!(
            plan.order_price.is_none(),
            "MarketableLimit 은 런타임 호가 정보가 필요해 plan 단계에선 주문가 미정"
        );
        // timeout 은 기본 3초 (Phase 3 권장)
        assert_eq!(plan.timeout_ms, 3_000);
        assert!(plan.cancel_on_timeout);
        assert_eq!(plan.fill_recheck_mode, FillRecheckMode::BalanceRecheck);
        // entry_mode 에 slippage_budget 기록.
        match plan.entry_mode {
            EntryMode::MarketableLimit {
                slippage_budget_pct,
            } => {
                assert!((slippage_budget_pct - 0.003).abs() < 1e-9);
            }
            other => panic!("expected MarketableLimit, got {:?}", other),
        }
        assert_eq!(policy.version(), "marketable_limit_with_budget_v1");
    }

    #[test]
    fn marketable_policy_respects_custom_slippage_budget_context() {
        let policy = MarketableLimitWithBudget {
            slippage_budget_pct: 0.003,
            timeout_ms: 3_000,
        };
        // ExecutionContext::marketable 로 슬리피지 budget override.
        let ctx = ExecutionContext::marketable(2.5, Some(0.3), 0.005);
        let plan = policy.plan(&sample_intent_long(), &ctx);
        match plan.entry_mode {
            EntryMode::MarketableLimit {
                slippage_budget_pct,
            } => {
                assert!(
                    (slippage_budget_pct - 0.005).abs() < 1e-9,
                    "context 에 slippage_budget_pct 가 있으면 policy 기본값 대신 사용"
                );
            }
            other => panic!("expected MarketableLimit, got {:?}", other),
        }
    }

    #[test]
    fn marketable_policy_default_timeout_is_short() {
        let policy = MarketableLimitWithBudget::default();
        assert_eq!(
            policy.timeout_ms, 3_000,
            "Phase 3 권장: 2~3초 — 기본 3_000ms"
        );
        assert!(
            policy.timeout_ms <= 5_000,
            "5초를 넘지 않는다는 Phase 3 불변식"
        );
    }
}

impl PartialEq for FillOutcome {
    fn eq(&self, other: &Self) -> bool {
        use FillOutcome::*;
        match (self, other) {
            (Filled, Filled) => true,
            (PartialFill, PartialFill) => true,
            (Cancelled(a), Cancelled(b)) => a == b,
            (Rejected(a), Rejected(b)) => a == b,
            (ManualIntervention(a), ManualIntervention(b)) => a == b,
            _ => false,
        }
    }
}
