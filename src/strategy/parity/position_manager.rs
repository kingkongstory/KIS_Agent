//! PositionManager — 포지션 청산 판정 순수 계층.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` §11 구현.
//!
//! 이 모듈은 백테스트 `simulate_exit` 와 라이브 `manage_position` 가 공유해야
//! 하는 청산 규칙을 pure function 으로 묶는다. broker I/O (TP 지정가 조회,
//! 시장가 발주, DB persist) 는 **호출측 adapter 가 담당**한다.
//!
//! ## 공통화 대상
//! - TP 도달 판정
//! - SL 도달 판정 (틱 / 캔들 양쪽)
//! - 시가 갭 손절 판정 (백테스트 전용)
//! - best_price 갱신 + 1R 본전스탑 + 트레일링 SL 갱신
//! - SL 청산 사유 분류 (StopLoss / BreakevenStop / TrailingStop)
//! - 시간 스탑 조건 (캔들 수 또는 경과 분 모두)

use chrono::NaiveTime;

use crate::strategy::types::{ExitReason, PositionSide};

/// 청산 판정에 필요한 파라미터. `OrbFvgConfig` 의 일부를 pure 한 형태로
/// 전달한다.
#[derive(Debug, Clone, Copy)]
pub struct PositionManagerConfig {
    pub rr_ratio: f64,
    pub trailing_r: f64,
    pub breakeven_r: f64,
    pub time_stop_candles: usize,
    /// 캔들 간격(분). 백테스트 5m, 라이브도 5m.
    pub candle_interval_min: u32,
}

/// 한 tick 에 적용된 트레일링 업데이트 요약.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrailingUpdate {
    pub best_price_changed: bool,
    pub reached_1r_newly: bool,
    pub trailing_changed: bool,
}

/// 포지션의 `best_price`, `reached_1r`, `stop_loss` 를 갱신한다.
///
/// - `favorable_price`: Long 이면 최근 tick 의 high, Short 이면 low.
/// - 결과: 변경 플래그 모음. 호출측은 이 값으로 DB persist 여부를 결정한다.
pub fn update_best_and_trailing(
    side: PositionSide,
    entry_price: i64,
    best_price: &mut i64,
    stop_loss: &mut i64,
    reached_1r: &mut bool,
    original_risk: i64,
    favorable_price: i64,
    cfg: &PositionManagerConfig,
) -> TrailingUpdate {
    let mut out = TrailingUpdate::default();

    let new_best = match side {
        PositionSide::Long => (*best_price).max(favorable_price),
        PositionSide::Short => (*best_price).min(favorable_price),
    };
    if new_best != *best_price {
        *best_price = new_best;
        out.best_price_changed = true;
    }

    let profit_from_entry = match side {
        PositionSide::Long => *best_price - entry_price,
        PositionSide::Short => entry_price - *best_price,
    };
    let breakeven_dist = (original_risk as f64 * cfg.breakeven_r) as i64;
    if !*reached_1r && profit_from_entry >= breakeven_dist {
        *reached_1r = true;
        *stop_loss = entry_price;
        out.reached_1r_newly = true;
        out.trailing_changed = true;
    }

    if *reached_1r {
        let trailing_dist = (original_risk as f64 * cfg.trailing_r) as i64;
        let new_sl = match side {
            PositionSide::Long => *best_price - trailing_dist,
            PositionSide::Short => *best_price + trailing_dist,
        };
        let improved = match side {
            PositionSide::Long => new_sl > *stop_loss,
            PositionSide::Short => new_sl < *stop_loss,
        };
        if improved {
            *stop_loss = new_sl;
            out.trailing_changed = true;
        }
    }

    out
}

/// TP 지정가 도달 여부.
///
/// 백테스트: `candle.high`(long) / `candle.low`(short) 를 전달.
/// 라이브: 현재가 단일 값을 전달. (실제로는 KRX에 걸린 TP 지정가가 선체결되므로
/// 라이브의 이 함수는 보조 판정용.)
pub fn is_tp_hit(side: PositionSide, take_profit: i64, extreme_price: i64) -> bool {
    match side {
        PositionSide::Long => extreme_price >= take_profit,
        PositionSide::Short => extreme_price <= take_profit,
    }
}

/// SL 도달 여부.
///
/// 백테스트: `candle.low`(long) / `candle.high`(short) 를 전달.
/// 라이브: 현재가 단일 값 전달.
pub fn is_sl_hit(side: PositionSide, stop_loss: i64, extreme_price: i64) -> bool {
    match side {
        PositionSide::Long => extreme_price <= stop_loss,
        PositionSide::Short => extreme_price >= stop_loss,
    }
}

/// 시가 갭 손절 여부 (백테스트 전용).
pub fn open_gap_breach(side: PositionSide, stop_loss: i64, open_price: i64) -> bool {
    match side {
        PositionSide::Long => open_price <= stop_loss,
        PositionSide::Short => open_price >= stop_loss,
    }
}

/// SL 도달 시의 청산 사유.
///
/// - `stop_loss` 가 `original_sl` 보다 유리한 방향으로 이동했고 entry_price 와
///   같다면 → BreakevenStop
/// - 그 밖에 유리 방향으로 이동했다면 → TrailingStop
/// - 이동이 없거나 불리했다면 → StopLoss
pub fn sl_exit_reason(
    side: PositionSide,
    current_stop_loss: i64,
    original_stop_loss: i64,
    entry_price: i64,
    reached_1r: bool,
) -> ExitReason {
    let improved = match side {
        PositionSide::Long => current_stop_loss > original_stop_loss,
        PositionSide::Short => current_stop_loss < original_stop_loss,
    };
    if reached_1r && improved {
        if current_stop_loss == entry_price {
            ExitReason::BreakevenStop
        } else {
            ExitReason::TrailingStop
        }
    } else {
        ExitReason::StopLoss
    }
}

/// 갭 손절 시의 청산 사유 (reached_1r 이면 TrailingStop, 아니면 StopLoss).
pub fn gap_exit_reason(reached_1r: bool) -> ExitReason {
    if reached_1r {
        ExitReason::TrailingStop
    } else {
        ExitReason::StopLoss
    }
}

/// 시간 스탑 조건 (캔들 수 기반 — 백테스트).
pub fn time_stop_breached_by_candles(
    candles_since_entry: usize,
    time_stop_candles: usize,
    reached_1r: bool,
) -> bool {
    !reached_1r && candles_since_entry >= time_stop_candles
}

/// 시간 스탑 조건 (경과 분 기반 — 라이브).
pub fn time_stop_breached_by_minutes(
    entry_time: NaiveTime,
    now: NaiveTime,
    cfg: &PositionManagerConfig,
    reached_1r: bool,
) -> bool {
    if reached_1r {
        return false;
    }
    let elapsed_min = (now - entry_time).num_minutes();
    let limit = cfg.time_stop_candles as i64 * cfg.candle_interval_min as i64;
    elapsed_min >= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PositionManagerConfig {
        PositionManagerConfig {
            rr_ratio: 2.5,
            trailing_r: 0.05,
            breakeven_r: 0.15,
            time_stop_candles: 3,
            candle_interval_min: 5,
        }
    }

    #[test]
    fn update_best_triggers_1r_and_trailing() {
        let cfg = cfg();
        let entry = 10_000i64;
        let original_risk = 100i64; // risk = entry - sl = 100
        let mut best = entry;
        let mut sl = 9_900; // entry - risk
        let mut reached = false;

        // breakeven_r=0.15 → 1R 트리거 거리 = 0.15 * 100 = 15.
        // favorable 10020 → profit 20 ≥ 15 → reached_1r.
        // 같은 호출 안에서 1) BE(sl→entry)로 이동, 2) 트레일링 개선(sl→best-5) 이 연쇄된다.
        // trailing_dist = 0.05R * 100 = 5, new_sl = 10020 - 5 = 10015 ≥ entry → 개선.
        let u = update_best_and_trailing(
            PositionSide::Long,
            entry,
            &mut best,
            &mut sl,
            &mut reached,
            original_risk,
            10_020,
            &cfg,
        );
        assert!(u.reached_1r_newly);
        assert!(u.trailing_changed);
        assert!(reached);
        // 최종 sl = 10015 (BE → trailing 로 덮어씀)
        assert_eq!(sl, 10_015);
        assert_eq!(best, 10_020);
    }

    #[test]
    fn trailing_only_improves_never_loosens() {
        let cfg = cfg();
        let entry = 10_000i64;
        let original_risk = 100i64;
        let mut best = entry;
        let mut sl = 9_900;
        let mut reached = true; // 이미 도달

        // best가 이미 10_100 이라 가정하고 sl=10_095 (best-5)
        best = 10_100;
        sl = 10_095;

        // 현재 favorable 10_050 (후퇴). best 변경 없음, sl 개선 없음.
        let u = update_best_and_trailing(
            PositionSide::Long,
            entry,
            &mut best,
            &mut sl,
            &mut reached,
            original_risk,
            10_050,
            &cfg,
        );
        assert!(!u.best_price_changed);
        assert!(!u.trailing_changed);
        assert_eq!(best, 10_100);
        assert_eq!(sl, 10_095);
    }

    #[test]
    fn sl_reason_classification() {
        // reached_1r=false → StopLoss
        assert_eq!(
            sl_exit_reason(PositionSide::Long, 9_900, 9_900, 10_000, false),
            ExitReason::StopLoss
        );
        // reached, sl == entry → BreakevenStop
        assert_eq!(
            sl_exit_reason(PositionSide::Long, 10_000, 9_900, 10_000, true),
            ExitReason::BreakevenStop
        );
        // reached, sl > entry (long) → TrailingStop
        assert_eq!(
            sl_exit_reason(PositionSide::Long, 10_050, 9_900, 10_000, true),
            ExitReason::TrailingStop
        );
        // reached 지만 개선 없음(short, sl 높아짐) → StopLoss
        assert_eq!(
            sl_exit_reason(PositionSide::Short, 10_100, 10_100, 10_000, true),
            ExitReason::StopLoss
        );
    }

    #[test]
    fn tp_sl_hit_helpers() {
        assert!(is_tp_hit(PositionSide::Long, 10_200, 10_250));
        assert!(!is_tp_hit(PositionSide::Long, 10_200, 10_150));
        assert!(is_tp_hit(PositionSide::Short, 9_800, 9_700));

        assert!(is_sl_hit(PositionSide::Long, 9_900, 9_850));
        assert!(!is_sl_hit(PositionSide::Long, 9_900, 9_950));
        assert!(is_sl_hit(PositionSide::Short, 10_100, 10_200));
    }

    #[test]
    fn open_gap_breach_detects_both_sides() {
        assert!(open_gap_breach(PositionSide::Long, 9_900, 9_850));
        assert!(!open_gap_breach(PositionSide::Long, 9_900, 9_950));
        assert!(open_gap_breach(PositionSide::Short, 10_100, 10_200));
    }

    #[test]
    fn time_stop_by_candles_respects_reached_1r() {
        assert!(time_stop_breached_by_candles(3, 3, false));
        assert!(!time_stop_breached_by_candles(3, 3, true));
        assert!(!time_stop_breached_by_candles(2, 3, false));
    }

    #[test]
    fn time_stop_by_minutes_converts_candles_to_minutes() {
        let cfg = cfg(); // time_stop=3 candles × 5m = 15분
        let entry = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let now_14 = NaiveTime::from_hms_opt(10, 14, 0).unwrap();
        let now_15 = NaiveTime::from_hms_opt(10, 15, 0).unwrap();
        assert!(!time_stop_breached_by_minutes(entry, now_14, &cfg, false));
        assert!(time_stop_breached_by_minutes(entry, now_15, &cfg, false));
        assert!(!time_stop_breached_by_minutes(entry, now_15, &cfg, true));
    }
}
