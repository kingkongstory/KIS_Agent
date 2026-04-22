//! 2026-04-19 PR #1.e.2 (execution-followup-plan): NAV 괴리 gate 순수 로직.
//!
//! # 소스 계약 (사용자 결정, 2026-04-19)
//!
//! - **런타임 권위 소스**: KIS WS `H0STNAV0` (ETF NAV 실시간).
//! - **bootstrap (재기동/재구독)**: KIS REST NAV 비교추이(분).
//! - **sanity check**: KIS REST NAV 비교추이(종목), ETF/ETN 현재가.
//!
//! 이 모듈은 **계산 순수 로직** 만 가진다. 네트워킹은 PR #1.e.4 에서 붙인다.
//!
//! # v1 hard block (사용자 결정)
//!
//! - `|price - NAV| / NAV > 0.50%` → `FailDeviation`
//! - NAV snapshot stale (`age_ms > cfg.max_stale_ms`) → `FailStale`
//! - NAV <= 0 (잘못된 payload) → `FailInvalidNav`
//!
//! # v1 보조 (hard block 아님, event_log 만)
//!
//! - "최근 3회 연속 괴리 확대" 와 "가격-NAV 역행" 은 v1 에서 경고/보조 사유.
//!   구현은 PR #1.e.4/5 에서 producer 쪽이 관측만 하고 `event_log` 에 기록.

use chrono::{DateTime, Utc};

/// NAV gate 설정. real/paper 모두 공통 기본값에서 출발 — 환경별 조정은
/// 호출자가 수행 (예: real 만 `max_stale_ms` 더 짧게).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavGateConfig {
    /// 허용 최대 괴리율 (절대값). 기본 0.005 (0.50%).
    pub max_deviation_pct: f64,
    /// NAV snapshot 최대 허용 나이 (ms). 기본 5_000.
    pub max_stale_ms: u64,
}

impl Default for NavGateConfig {
    fn default() -> Self {
        Self {
            max_deviation_pct: 0.005,
            max_stale_ms: 5_000,
        }
    }
}

/// ETF 가격과 NAV 스냅샷. price 는 원 단위 정수, NAV 는 소수점 가능 f64.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavSnapshot {
    /// ETF 현재가 (원).
    pub price: i64,
    /// NAV (원, 소수점).
    pub nav: f64,
    /// NAV 값이 마지막으로 갱신된 시각 (UTC).
    pub nav_updated_at: DateTime<Utc>,
}

/// NAV gate 평가 결과. `Pass` 는 `deviation_pct` 도 함께 노출해서 event_log 기록 시 사용.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NavDecision {
    Pass { deviation_pct: f64 },
    FailDeviation { deviation_pct: f64 },
    FailStale { age_ms: i64 },
    FailInvalidNav,
}

impl NavDecision {
    pub fn is_pass(self) -> bool {
        matches!(self, NavDecision::Pass { .. })
    }

    /// event_log `event_type` 으로 쓰기 위한 라벨.
    pub fn label(self) -> &'static str {
        match self {
            NavDecision::Pass { .. } => "nav_pass",
            NavDecision::FailDeviation { .. } => "nav_fail_deviation",
            NavDecision::FailStale { .. } => "nav_fail_stale",
            NavDecision::FailInvalidNav => "nav_fail_invalid",
        }
    }
}

/// NAV 스냅샷 + 설정 + 현재 시각으로 gate 판정.
///
/// 규칙 순서:
/// 1. `nav <= 0` → `FailInvalidNav` (이후 나머지 체크 skip — 계산 불가)
/// 2. stale (`age_ms > max_stale_ms` 또는 음수 — clock skew) → `FailStale`
/// 3. `|price - nav| / nav > max_deviation_pct` → `FailDeviation`
/// 4. 그 외 → `Pass`
pub fn evaluate_nav_gate(
    snapshot: &NavSnapshot,
    cfg: &NavGateConfig,
    now: DateTime<Utc>,
) -> NavDecision {
    if snapshot.nav <= 0.0 || !snapshot.nav.is_finite() {
        return NavDecision::FailInvalidNav;
    }
    let age_ms = now
        .signed_duration_since(snapshot.nav_updated_at)
        .num_milliseconds();
    if age_ms < 0 || (age_ms as u64) > cfg.max_stale_ms {
        return NavDecision::FailStale { age_ms };
    }
    let deviation_pct = ((snapshot.price as f64) - snapshot.nav).abs() / snapshot.nav;
    if deviation_pct > cfg.max_deviation_pct {
        NavDecision::FailDeviation { deviation_pct }
    } else {
        NavDecision::Pass { deviation_pct }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn snapshot(price: i64, nav: f64, age_ms: i64) -> (NavSnapshot, DateTime<Utc>) {
        let now = Utc::now();
        let snap = NavSnapshot {
            price,
            nav,
            nav_updated_at: now - Duration::milliseconds(age_ms),
        };
        (snap, now)
    }

    #[test]
    fn pass_when_deviation_is_small_and_fresh() {
        let cfg = NavGateConfig::default();
        // NAV 10000, price 10020 → 0.20% < 0.50%
        let (s, now) = snapshot(10_020, 10_000.0, 100);
        let d = evaluate_nav_gate(&s, &cfg, now);
        assert!(d.is_pass());
        match d {
            NavDecision::Pass { deviation_pct } => {
                assert!((deviation_pct - 0.002).abs() < 1e-9);
            }
            _ => panic!("expected Pass"),
        }
    }

    #[test]
    fn fail_deviation_when_spread_exceeds_max() {
        let cfg = NavGateConfig::default();
        // NAV 10000, price 10100 → 1.00% > 0.50%
        let (s, now) = snapshot(10_100, 10_000.0, 100);
        let d = evaluate_nav_gate(&s, &cfg, now);
        match d {
            NavDecision::FailDeviation { deviation_pct } => {
                assert!((deviation_pct - 0.01).abs() < 1e-9);
            }
            _ => panic!("expected FailDeviation, got {:?}", d),
        }
    }

    #[test]
    fn fail_stale_when_older_than_max_stale() {
        let cfg = NavGateConfig::default(); // max_stale 5_000ms
        let (s, now) = snapshot(10_000, 10_000.0, 10_000);
        match evaluate_nav_gate(&s, &cfg, now) {
            NavDecision::FailStale { age_ms } => {
                assert!(age_ms >= 10_000);
            }
            other => panic!("expected FailStale, got {:?}", other),
        }
    }

    #[test]
    fn fail_stale_on_clock_skew_negative_age() {
        let cfg = NavGateConfig::default();
        let now = Utc::now();
        // nav_updated_at 이 미래 → age 음수
        let s = NavSnapshot {
            price: 10_000,
            nav: 10_000.0,
            nav_updated_at: now + Duration::seconds(10),
        };
        match evaluate_nav_gate(&s, &cfg, now) {
            NavDecision::FailStale { age_ms } => assert!(age_ms < 0),
            other => panic!("expected FailStale, got {:?}", other),
        }
    }

    #[test]
    fn fail_invalid_when_nav_is_zero() {
        let cfg = NavGateConfig::default();
        let (s, now) = snapshot(10_000, 0.0, 100);
        assert_eq!(
            evaluate_nav_gate(&s, &cfg, now),
            NavDecision::FailInvalidNav
        );
    }

    #[test]
    fn fail_invalid_when_nav_is_negative() {
        let cfg = NavGateConfig::default();
        let (s, now) = snapshot(10_000, -100.0, 100);
        assert_eq!(
            evaluate_nav_gate(&s, &cfg, now),
            NavDecision::FailInvalidNav
        );
    }

    #[test]
    fn fail_invalid_when_nav_is_nan_or_infinite() {
        let cfg = NavGateConfig::default();
        let (s_nan, now) = snapshot(10_000, f64::NAN, 100);
        assert_eq!(
            evaluate_nav_gate(&s_nan, &cfg, now),
            NavDecision::FailInvalidNav
        );
        let (s_inf, now) = snapshot(10_000, f64::INFINITY, 100);
        assert_eq!(
            evaluate_nav_gate(&s_inf, &cfg, now),
            NavDecision::FailInvalidNav
        );
    }

    /// 경계: deviation_pct == max_deviation_pct → Pass (조건은 strict `>`).
    #[test]
    fn pass_exactly_on_boundary() {
        let cfg = NavGateConfig {
            max_deviation_pct: 0.005,
            max_stale_ms: 5_000,
        };
        // NAV 10000, price 10050 → 0.5% 정확
        let (s, now) = snapshot(10_050, 10_000.0, 100);
        let d = evaluate_nav_gate(&s, &cfg, now);
        assert!(d.is_pass(), "exactly 0.50% should still Pass, got {:?}", d);
    }

    /// 경계: age_ms == max_stale_ms → Pass (조건은 strict `>`).
    #[test]
    fn pass_exactly_on_stale_boundary() {
        let cfg = NavGateConfig {
            max_deviation_pct: 0.005,
            max_stale_ms: 5_000,
        };
        let (s, now) = snapshot(10_000, 10_000.0, 5_000);
        let d = evaluate_nav_gate(&s, &cfg, now);
        // age 가 정확히 max_stale_ms 면 아직 stale 아님.
        assert!(d.is_pass(), "age=max_stale should still Pass, got {:?}", d);
    }

    #[test]
    fn label_maps_each_variant() {
        assert_eq!(NavDecision::Pass { deviation_pct: 0.0 }.label(), "nav_pass");
        assert_eq!(
            NavDecision::FailDeviation {
                deviation_pct: 0.02
            }
            .label(),
            "nav_fail_deviation"
        );
        assert_eq!(
            NavDecision::FailStale { age_ms: 10_000 }.label(),
            "nav_fail_stale"
        );
        assert_eq!(NavDecision::FailInvalidNav.label(), "nav_fail_invalid");
    }
}
