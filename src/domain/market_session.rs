//! 시장 세션 정책 — 장 운영 시각/장외 판정/재연결 backoff 선택을 담당한다.
//!
//! 2026-04-24 P0-1: 장외 WS 재연결 폭주를 줄이기 위해 backoff 확대만 먼저
//! 도입한다. 단, `ManualHeld` / pending / open 상태에서 체결통보 감지가 늦어지는
//! 회귀를 막기 위해 호출자가 계산한 안전 플래그가 true 일 때만 확대한다.
//!
//! 설계 원칙:
//! - 순수 함수 + 값 타입. 테스트 가능성이 최우선.
//! - env 기반 설정(feature flag). 문제 발생 시 코드 롤백 없이 env 만으로 원복 가능.
//! - 휴장일 처리는 P0-3 에서 도입할 `is_trading_day` 인수와 합쳐 확장한다.
//!
//! 참고: `src/strategy/live_runner.rs::compute_session_wait_deadline` 도 유사한
//! 세션 기반 로직이며, 이 타입과 통합할 수 있지만 이번 PR 범위는 아니다.

use chrono::NaiveTime;
use std::time::Duration;

/// 시장 세션 정책 (env 기반 설정 스냅샷).
///
/// 기본값은 KRX 정규장(09:00~15:30) 기준이며, 장외 WS 재연결을 10분 간격으로 늦춘다.
/// P0-2 이후에는 balance sentinel 과 상태 기반 억제 필드가 추가될 예정이다.
#[derive(Debug, Clone)]
pub struct MarketSessionPolicy {
    /// 장외 억제(backoff/polling) 전역 on/off 스위치. 기본 false.
    ///
    /// `false` 이면 모든 시간대에서 기존 backoff/polling 이 그대로 유지돼
    /// 문제 발생 시 env 하나만 바꾸면 원복 가능하다.
    pub suppression_enabled: bool,
    /// pre-open 재연결 허용 시작 시각. 기본 08:30.
    ///
    /// 이 시각 이후면 off-hours 가 아니다. KIS 서버는 매 정시(:00)에 연결을
    /// 리셋하므로 08:30 에 warm-up 해도 09:00 리셋은 피할 수 없다 — 목적은
    /// 장 시작 직전 인증/AES key 수립을 미리 끝내 09:00 재연결을 빠르게 하는 것.
    pub pre_open_connect_at: NaiveTime,
    /// 정규장 시작 시각. 기본 09:00. (현재 P0-1 에서는 is_off_hours 판정에
    /// 직접 쓰이지 않지만, P0-3 에서 정시 리셋 grace 계산에 사용된다.)
    pub regular_open_at: NaiveTime,
    /// 정규장 종료 시각. 기본 15:30. (close_settle 시작 시각과 같음.)
    pub regular_close_at: NaiveTime,
    /// 장 마감 후 청산 검증(settle) 종료 시각. 기본 15:45.
    ///
    /// 이 시각 이후면 off-hours. 15:25 force_exit 후에도 청산 검증이 남아
    /// 있으므로 15:45 까지는 정상 polling 을 유지한다.
    pub close_settle_end_at: NaiveTime,
    /// 장외 시간대의 WS 재연결 최소 backoff. 기본 10분(600초).
    ///
    /// connection 계층이 계산한 default backoff(지수 1~60 초)와 `max()` 로 합쳐
    /// 기본 backoff 가 이 값보다 작을 때만 늘어난다. 장중에는 영향 없다.
    pub off_hours_ws_backoff: Duration,
}

impl MarketSessionPolicy {
    /// 안전한 기본값(운영 시작 전 feature flag off 상태).
    pub fn default_off() -> Self {
        Self {
            suppression_enabled: false,
            pre_open_connect_at: NaiveTime::from_hms_opt(8, 30, 0).unwrap(),
            regular_open_at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            regular_close_at: NaiveTime::from_hms_opt(15, 30, 0).unwrap(),
            close_settle_end_at: NaiveTime::from_hms_opt(15, 45, 0).unwrap(),
            off_hours_ws_backoff: Duration::from_secs(600),
        }
    }

    /// env 에서 로드. 파싱 실패/미지정 필드는 `default_off()` 값을 쓴다.
    ///
    /// env 목록:
    /// - `KIS_OFF_HOURS_SUPPRESSION_ENABLED` (bool, 기본 false)
    /// - `KIS_PRE_OPEN_CONNECT_AT` (HH:MM, 기본 08:30)
    /// - `KIS_REGULAR_OPEN_AT` (HH:MM, 기본 09:00)
    /// - `KIS_REGULAR_CLOSE_AT` (HH:MM, 기본 15:30)
    /// - `KIS_CLOSE_SETTLE_END_AT` (HH:MM, 기본 15:45)
    /// - `KIS_OFF_HOURS_WS_BACKOFF_MS` (u64 ms, 기본 600_000)
    pub fn from_env() -> Self {
        let mut policy = Self::default_off();

        if let Ok(v) = std::env::var("KIS_OFF_HOURS_SUPPRESSION_ENABLED") {
            let s = v.trim().to_lowercase();
            policy.suppression_enabled = matches!(s.as_str(), "true" | "1" | "on" | "yes");
        }
        if let Some(t) = parse_hm_env("KIS_PRE_OPEN_CONNECT_AT") {
            policy.pre_open_connect_at = t;
        }
        if let Some(t) = parse_hm_env("KIS_REGULAR_OPEN_AT") {
            policy.regular_open_at = t;
        }
        if let Some(t) = parse_hm_env("KIS_REGULAR_CLOSE_AT") {
            policy.regular_close_at = t;
        }
        if let Some(t) = parse_hm_env("KIS_CLOSE_SETTLE_END_AT") {
            policy.close_settle_end_at = t;
        }
        if let Ok(v) = std::env::var("KIS_OFF_HOURS_WS_BACKOFF_MS")
            && let Ok(ms) = v.trim().parse::<u64>()
        {
            policy.off_hours_ws_backoff = Duration::from_millis(ms);
        }

        policy
    }

    /// 주어진 시각이 장외(off-hours)인가.
    ///
    /// - `now < pre_open_connect_at` (예: 08:29) → true
    /// - `pre_open_connect_at <= now < close_settle_end_at` (예: 10:00) → false
    /// - `now >= close_settle_end_at` (예: 15:45) → true
    ///
    /// 휴장일 처리는 P0-3 에서 별도 거래일 캘린더로 보강한다.
    pub fn is_off_hours(&self, now: NaiveTime) -> bool {
        now < self.pre_open_connect_at || now >= self.close_settle_end_at
    }
}

/// 장외일 때 WS 재연결 backoff 를 확대한 값을 선택한다.
///
/// `suppression_enabled=false` 이거나 장중이면 `default_backoff` 를 그대로 반환.
/// 장외이고 flag on 이더라도 `state_backoff_safe=false` 이면 기존 backoff 를 유지한다.
/// 상태 안전성이 확인된 경우에만 `max(default_backoff, policy.off_hours_ws_backoff)` 를 반환.
///
/// 이 함수는 의도적으로 `Duration` 을 입력/출력으로만 받고 실제 sleep 은 호출자가
/// 한다. 테스트에서 시각/backoff 조합만으로 결과를 단언 가능.
pub fn select_reconnect_backoff(
    policy: &MarketSessionPolicy,
    default_backoff: Duration,
    now: NaiveTime,
    state_backoff_safe: bool,
) -> Duration {
    if policy.suppression_enabled && state_backoff_safe && policy.is_off_hours(now) {
        default_backoff.max(policy.off_hours_ws_backoff)
    } else {
        default_backoff
    }
}

/// `HH:MM` 형식 env 를 파싱. 잘못된 값은 `None` 으로 무시해 기본값 유지.
fn parse_hm_env(key: &str) -> Option<NaiveTime> {
    let raw = std::env::var(key).ok()?;
    let trimmed = raw.trim();
    NaiveTime::parse_from_str(&format!("{trimmed}:00"), "%H:%M:%S").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(hh: u32, mm: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hh, mm, 0).unwrap()
    }

    #[test]
    fn default_off_disables_suppression() {
        let p = MarketSessionPolicy::default_off();
        assert!(!p.suppression_enabled);
        assert_eq!(p.pre_open_connect_at, t(8, 30));
        assert_eq!(p.close_settle_end_at, t(15, 45));
        assert_eq!(p.off_hours_ws_backoff, Duration::from_secs(600));
    }

    #[test]
    fn is_off_hours_boundaries() {
        let p = MarketSessionPolicy::default_off();
        // 새벽/이른 아침
        assert!(p.is_off_hours(t(0, 0)));
        assert!(p.is_off_hours(t(6, 0)));
        assert!(p.is_off_hours(t(8, 29)));
        // pre-open 경계
        assert!(!p.is_off_hours(t(8, 30)));
        assert!(!p.is_off_hours(t(8, 45)));
        // 정규장
        assert!(!p.is_off_hours(t(9, 0)));
        assert!(!p.is_off_hours(t(12, 0)));
        assert!(!p.is_off_hours(t(15, 30)));
        // close settle
        assert!(!p.is_off_hours(t(15, 44)));
        // 장외
        assert!(p.is_off_hours(t(15, 45)));
        assert!(p.is_off_hours(t(18, 0)));
        assert!(p.is_off_hours(t(22, 31)));
        assert!(p.is_off_hours(t(23, 59)));
    }

    #[test]
    fn suppression_off_keeps_default_backoff_even_off_hours() {
        let p = MarketSessionPolicy::default_off(); // suppression_enabled=false
        // 장외라도 flag off 면 기본 backoff 유지
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(4), t(23, 0), true),
            Duration::from_secs(4),
            "feature flag off 면 장외여도 재연결 backoff 를 확대하지 않는다"
        );
    }

    #[test]
    fn suppression_on_market_hours_keeps_default_backoff() {
        let mut p = MarketSessionPolicy::default_off();
        p.suppression_enabled = true;
        // 장중이면 flag 가 켜져 있어도 기본 유지
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(4), t(10, 0), true),
            Duration::from_secs(4),
            "장중에는 기본 backoff 를 유지해야 한다"
        );
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(60), t(15, 29), true),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn suppression_on_off_hours_expands_short_backoff_when_state_safe() {
        let mut p = MarketSessionPolicy::default_off();
        p.suppression_enabled = true;
        // 장외 + 기본 4초 → 10분으로 확대
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(4), t(23, 0), true),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn suppression_on_off_hours_keeps_default_when_state_is_not_safe() {
        let mut p = MarketSessionPolicy::default_off();
        p.suppression_enabled = true;
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(4), t(23, 0), false),
            Duration::from_secs(4),
            "manual/pending/open 상태가 의심되면 장외라도 backoff 를 늘리지 않는다"
        );
    }

    #[test]
    fn suppression_on_off_hours_preserves_longer_default_backoff() {
        let mut p = MarketSessionPolicy::default_off();
        p.suppression_enabled = true;
        // 이미 기본 backoff 가 더 길면 장외라도 그대로 유지 (max)
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(900), t(3, 0), true),
            Duration::from_secs(900),
            "이미 기본 backoff 가 정책 값보다 길면 그대로 유지한다"
        );
    }

    #[test]
    fn custom_off_hours_backoff_is_respected() {
        let mut p = MarketSessionPolicy::default_off();
        p.suppression_enabled = true;
        p.off_hours_ws_backoff = Duration::from_secs(900); // 15분
        assert_eq!(
            select_reconnect_backoff(&p, Duration::from_secs(4), t(4, 0), true),
            Duration::from_secs(900)
        );
    }

    #[test]
    fn custom_pre_open_boundary_respected() {
        let mut p = MarketSessionPolicy::default_off();
        p.pre_open_connect_at = t(8, 45);
        // 8:30 이 더 이상 "장중" 이 아님
        assert!(p.is_off_hours(t(8, 30)));
        assert!(p.is_off_hours(t(8, 44)));
        assert!(!p.is_off_hours(t(8, 45)));
    }
}
