//! orb_vwap_pullback — OR 15분 + VWAP 기반 되돌림 전략 (스켈레톤).
//!
//! `docs/monitoring/strategy-redesign-plan.md` §P1 / `docs/strategy/backtest-live-parity-architecture.md` §13 Phase 7.
//!
//! ## ⚠ 실전 투입 금지 — 현재 스켈레톤
//!
//! `detect_next()` 는 항상 `None` 을 반환한다. 이 엔진을 `LiveRunner` 또는 parity backtest 에
//! 주입하면 **거래가 한 건도 발생하지 않는다**. 2026-04-16 실전 첫날 투입 전략은
//! `orb_fvg` + `LiveRunnerConfig::for_real_mode` (docs/monitoring/2026-04-16-deployment-checklist.md §0)
//! 이며, `orb_vwap_pullback` 본 구현은 PR2/PR3 (strategy-redesign-plan.md) 일정으로 이월됐다.
//!
//! 투입 시점은 아래 체크리스트가 모두 끝난 뒤다. 실전 경로에 연결하기 전 반드시 parity backtest
//! 로 최소 3영업일 동일 규칙 검증 필요.
//!
//! ## 전략 개요
//! 1. **OR 15분 단일화**: `09:00~09:14` 1분봉 기준 OR high/low 하나만 사용.
//! 2. **VWAP 기반 방향 확인**: 09:15 이후 5분봉 2개가 같은 방향으로 OR 밖에서
//!    마감 + VWAP 정합성.
//! 3. **첫 되돌림만 감시**: 방향 확정 후 entry_zone = OR 경계와 VWAP 사이 구간.
//!    가격이 zone 첫 진입 시 `armed` 상태만 설정.
//! 4. **1분 반전 확인 후 진입**: armed 상태에서 1분봉 종가가 VWAP 재돌파.
//! 5. **종목 선택**: 상승 방향 → `122630`, 하락 방향 → `114800`. 하루 최대 1회.
//! 6. **청산 단순화**: 1R 에서 절반 청산 → 본절 이동 → 직전 5분봉 저점/VWAP 이탈
//!    기준으로 추적. 진입 cutoff 10:30, 강제 청산 15:15.
//!
//! ## Phase 7 범위 (현재 스켈레톤)
//! - `OrbVwapPullbackEngine` 구조체 + `SignalEngine` trait 구현 stub.
//! - 실제 VWAP 계산·방향 확인·armed 상태 머신 로직은 **다음 PR 에서 구현**.
//! - parity 인프라(ExecutionPolicy + PositionManager) 는 이미 재사용 가능.
//!
//! ## 구현 체크리스트 (next PR)
//! - [ ] OR 15분 계산 (`09:00~09:14` 1분봉 기준)
//! - [ ] VWAP 이동 계산 (session-reset)
//! - [ ] 방향 확인 상태 머신 (OR 밖 5분봉 2개 + VWAP 정합)
//! - [ ] `armed` 상태 + 1분 반전 확인
//! - [ ] 진입 cutoff 10:30 / 강제 청산 15:15
//! - [ ] 종목 선택 로직 (상승/하락 판정 → 단일 종목)
//! - [ ] PartialExit 50% + 본절 이동 (PositionManager 확장 필요)

use chrono::NaiveTime;

use super::candle::MinuteCandle;
use super::parity::signal_engine::{
    DetectedSignal, SignalEngine, SignalEngineConfig,
};

/// 전략 파라미터. 현재 경로에서는 직접 사용되지 않으며, 다음 PR 에서 채워짐.
#[derive(Debug, Clone)]
pub struct OrbVwapPullbackConfig {
    /// OR 시작·종료 시각 (기본 09:00 ~ 09:15).
    pub or_start: NaiveTime,
    pub or_end: NaiveTime,
    /// 진입 cutoff (기본 10:30).
    pub entry_cutoff: NaiveTime,
    /// 강제 청산 (기본 15:15).
    pub force_exit: NaiveTime,
    /// 방향 확인을 위한 OR 밖 마감 5분봉 수 (기본 2).
    pub direction_confirm_candles: usize,
    /// partial exit 비율 (기본 0.5 = 50%).
    pub partial_exit_ratio: f64,
    /// 하루 최대 진입 (기본 1).
    pub max_daily_trades: usize,
}

impl Default for OrbVwapPullbackConfig {
    fn default() -> Self {
        Self {
            or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            entry_cutoff: NaiveTime::from_hms_opt(10, 30, 0).unwrap(),
            force_exit: NaiveTime::from_hms_opt(15, 15, 0).unwrap(),
            direction_confirm_candles: 2,
            partial_exit_ratio: 0.5,
            max_daily_trades: 1,
        }
    }
}

/// `SignalEngine` trait 구현체. 현재는 stub 반환.
///
/// parity 인프라(`ParityDayRunner<E, P>`) 에 이 엔진을 주입하면 그대로 parity
/// backtest/replay 경로가 동작한다. 로직만 채우면 됨.
#[derive(Debug, Clone)]
pub struct OrbVwapPullbackEngine {
    pub config: OrbVwapPullbackConfig,
}

impl Default for OrbVwapPullbackEngine {
    fn default() -> Self {
        Self {
            config: OrbVwapPullbackConfig::default(),
        }
    }
}

impl SignalEngine for OrbVwapPullbackEngine {
    fn detect_next(
        &self,
        _candles_5m: &[MinuteCandle],
        _config: &SignalEngineConfig,
    ) -> Option<DetectedSignal> {
        // TODO(Phase 7 본 구현):
        // 1. OR 15분 범위 계산
        // 2. VWAP 누적 계산
        // 3. 방향 확인 (OR 밖 5분봉 `direction_confirm_candles` 개 + VWAP 정합)
        // 4. entry_zone = [OR 경계, VWAP]
        // 5. armed 상태 진입 (zone 첫 터치)
        // 6. 1분 반전 확인 (1분봉 종가가 VWAP 재돌파 + 직전 1분봉 고가 돌파)
        // 7. DetectedSignal 반환
        //
        // 실전 배포 시 실수로 이 스켈레톤이 라이브 경로에 연결되지 않도록 매 호출마다
        // warn 로그를 남긴다. 2026-04-16 실전 첫날은 `orb_fvg` 경로만 활성.
        tracing::warn!(
            target: "orb_vwap_pullback",
            "orb_vwap_pullback 은 스켈레톤 상태 — 실전 투입 금지. 신호 생성 없이 None 반환."
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_none_for_now() {
        let engine = OrbVwapPullbackEngine::default();
        let cfg = SignalEngineConfig {
            strategy_id: "orb_vwap_pullback".into(),
            stage_name: "15m".into(),
            or_high: 10_000,
            or_low: 9_900,
            fvg_expiry_candles: 6,
            entry_cutoff: NaiveTime::from_hms_opt(10, 30, 0).unwrap(),
            require_or_breakout: true,
            long_only: true,
            confirmed_side: None,
        };
        assert!(engine.detect_next(&[], &cfg).is_none());
    }

    #[test]
    fn default_config_matches_redesign_plan() {
        let cfg = OrbVwapPullbackConfig::default();
        assert_eq!(cfg.or_start, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(cfg.or_end, NaiveTime::from_hms_opt(9, 15, 0).unwrap());
        assert_eq!(cfg.entry_cutoff, NaiveTime::from_hms_opt(10, 30, 0).unwrap());
        assert_eq!(cfg.force_exit, NaiveTime::from_hms_opt(15, 15, 0).unwrap());
        assert_eq!(cfg.direction_confirm_candles, 2);
        assert_eq!(cfg.max_daily_trades, 1);
        assert_eq!(cfg.partial_exit_ratio, 0.5);
    }
}
