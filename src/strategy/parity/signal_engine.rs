//! SignalEngine — FVG 돌파 신호 탐지 순수 계층.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` §9 구현.
//!
//! 이 모듈은 백테스트 `orb_fvg::scan_and_trade` 의 FVG 탐지 블록과 라이브
//! `live_runner::poll_and_enter` 의 multi-stage 루프가 **공유해야 하는 로직**
//! 만 담는다. 즉 "어떤 캔들 시퀀스에서 현재 유효한 Long/Short 진입 신호가
//! 있는가" 만 계산하고, 주문 가격 산정과 체결 판정은 ExecutionPolicy 계층이
//! 담당한다.
//!
//! ## 동치성 기준
//! 이 모듈이 돌려주는 `DetectedSignal` 은 legacy `scan_and_trade` 의 1단계
//! (FVG 감지) + 2단계(리트레이스 확정) 조건을 **같은 순서로** 판정해야 한다.
//! - Bullish: `b.is_bullish() && a.high < c.low && b.body_size()*100 >= a.range()*30`
//! - `require_or_breakout` 시 `b.close > or_high`
//! - `fvg_expiry_candles` 초과 시 폐기
//! - `confirmed_side` 가 설정되면 반대 방향 신호 무시
//! - `long_only` 가 설정되면 Bearish 신호 무시
//!
//! 리트레이스는 `c5.low ∈ [gap.bottom, gap.top]` (Long) / `c5.high ∈ [gap.bottom, gap.top]` (Short)
//! 조건으로 판정한다.

use chrono::NaiveTime;

use crate::strategy::candle::MinuteCandle;
use crate::strategy::fvg::{FairValueGap, FvgDirection};
use crate::strategy::types::PositionSide;

use super::conversions::side_from_fvg;
use super::types::{SignalIntent, SignalMetadata};

/// 한 번의 탐지에서 필요한 입력.
///
/// 기존 `OrbFvgConfig` 는 rr_ratio / trailing 등 주문·청산용 값을 포함하지만
/// 여기서는 오직 신호 탐지에 필요한 파라미터만 추출해 가진다.
#[derive(Debug, Clone)]
pub struct SignalEngineConfig {
    /// 전략 식별자. SignalIntent.strategy_id 에 그대로 실린다.
    pub strategy_id: String,
    /// 이 엔진이 계산 대상으로 삼는 stage 라벨 ("5m"/"15m"/"30m" 등).
    pub stage_name: String,
    /// OR 고가/저가. 백테스트/라이브 공통.
    pub or_high: i64,
    pub or_low: i64,
    /// FVG 캔들 유효 기간.
    pub fvg_expiry_candles: usize,
    /// 신규 진입 마감 시각.
    pub entry_cutoff: NaiveTime,
    /// OR 돌파 필수 여부 (1차 진입은 true, 2차 이후 false).
    pub require_or_breakout: bool,
    /// Long 전용 모드 (Short FVG 무시).
    pub long_only: bool,
    /// 이전 거래 방향. Some 이면 그 방향만 허용.
    pub confirmed_side: Option<PositionSide>,
}

/// FVG 신호 + 리트레이스 확정 정보.
///
/// ExecutionPolicy 가 이 신호로부터 `EntryPlan` 을 산정한다.
#[derive(Debug, Clone)]
pub struct DetectedSignal {
    pub intent: SignalIntent,
    /// 리트레이스가 확정된 `candles_5m` 기준 인덱스.
    /// simulate_exit 은 이 인덱스 **다음** 캔들부터 소비한다.
    pub retrace_candle_idx: usize,
    /// 원본 FVG. legacy 경로가 `gap.mid_price()` 로 쓰는 값 계산에 필요.
    pub gap: FairValueGap,
}

/// SignalEngine trait.
///
/// Phase 2 에서는 FVG 기반 단일 구현체만 사용. Phase 7 에서 VWAP pullback 등
/// 새 구현체가 추가된다.
pub trait SignalEngine {
    fn detect_next(
        &self,
        candles_5m: &[MinuteCandle],
        config: &SignalEngineConfig,
    ) -> Option<DetectedSignal>;
}

/// ORB + FVG 신호 엔진.
#[derive(Debug, Clone, Default)]
pub struct OrbFvgSignalEngine;

impl SignalEngine for OrbFvgSignalEngine {
    fn detect_next(
        &self,
        candles_5m: &[MinuteCandle],
        config: &SignalEngineConfig,
    ) -> Option<DetectedSignal> {
        detect_next_fvg_signal(candles_5m, config)
    }
}

/// legacy scan_and_trade 와 등가인 FVG 탐지 + 리트레이스 확정 pure function.
///
/// 반환값은 첫 번째 확정 신호만. 여러 신호 중 최초 한 개만 산출한다는 점에서
/// 백테스트 루프와 라이브 multi-stage 루프의 공통 단위 연산이 된다.
pub fn detect_next_fvg_signal(
    candles_5m: &[MinuteCandle],
    config: &SignalEngineConfig,
) -> Option<DetectedSignal> {
    let mut pending_fvg: Option<FairValueGap> = None;
    let mut pending_meta: Option<PendingMeta> = None;
    let mut fvg_formed_idx: usize = 0;

    for (idx, c5) in candles_5m.iter().enumerate() {
        if c5.time >= config.entry_cutoff {
            break;
        }

        // FVG 유효시간 초과 → 폐기
        if pending_fvg.is_some() && idx - fvg_formed_idx > config.fvg_expiry_candles {
            pending_fvg = None;
            pending_meta = None;
        }

        // ── 1단계: FVG 감지 ──
        if pending_fvg.is_none() && idx >= 2 {
            let a = &candles_5m[idx - 2];
            let b = &candles_5m[idx - 1];
            let c = c5;
            let a_range_raw = a.range();
            let a_range = a_range_raw.max(1);
            let body_ok = b.body_size() * 100 >= a_range * 30;

            // Bullish FVG
            if b.is_bullish() && a.high < c.low && body_ok {
                let or_ok = !config.require_or_breakout || b.close > config.or_high;
                let side_ok = config
                    .confirmed_side
                    .map_or(true, |s| s == PositionSide::Long);
                if or_ok && side_ok {
                    pending_fvg = Some(FairValueGap {
                        direction: FvgDirection::Bullish,
                        top: c.low,
                        bottom: a.high,
                        candle_b_idx: idx - 1,
                        stop_loss: a.low,
                    });
                    fvg_formed_idx = idx;
                    pending_meta = Some(build_meta(a, b, a_range_raw, config, PositionSide::Long));
                    continue;
                }
            }

            // Bearish FVG
            if !config.long_only && b.is_bearish() && a.low > c.high && body_ok {
                let or_ok = !config.require_or_breakout || b.close < config.or_low;
                let side_ok = config
                    .confirmed_side
                    .map_or(true, |s| s == PositionSide::Short);
                if or_ok && side_ok {
                    pending_fvg = Some(FairValueGap {
                        direction: FvgDirection::Bearish,
                        top: a.low,
                        bottom: c.high,
                        candle_b_idx: idx - 1,
                        stop_loss: a.high,
                    });
                    fvg_formed_idx = idx;
                    pending_meta = Some(build_meta(a, b, a_range_raw, config, PositionSide::Short));
                    continue;
                }
            }
        }

        // ── 2단계: 리트레이스 진입 ──
        if let Some(ref gap) = pending_fvg {
            let side = side_from_fvg(gap.direction);
            let retrace = match side {
                PositionSide::Long => c5.low,
                PositionSide::Short => c5.high,
            };
            if gap.contains_price(retrace) {
                let meta = pending_meta.unwrap_or_else(|| PendingMeta::empty(c5.time));
                let intent = build_intent(gap, c5.time, config, meta);
                return Some(DetectedSignal {
                    intent,
                    retrace_candle_idx: idx,
                    gap: gap.clone(),
                });
            }
        }
    }

    None
}

#[derive(Debug, Clone)]
struct PendingMeta {
    b_time: NaiveTime,
    b_volume: u64,
    b_body_ratio: f64,
    or_breakout_pct: f64,
    b_close: i64,
    a_range: i64,
}

impl PendingMeta {
    fn empty(b_time: NaiveTime) -> Self {
        Self {
            b_time,
            b_volume: 0,
            b_body_ratio: 0.0,
            or_breakout_pct: 0.0,
            b_close: 0,
            a_range: 0,
        }
    }
}

fn build_meta(
    a: &MinuteCandle,
    b: &MinuteCandle,
    a_range_raw: i64,
    config: &SignalEngineConfig,
    side: PositionSide,
) -> PendingMeta {
    let a_range_safe = a_range_raw.max(1);
    let b_body_ratio = b.body_size() as f64 / a_range_safe as f64;
    let or_breakout_pct = match side {
        PositionSide::Long if config.or_high > 0 => {
            (b.close - config.or_high) as f64 / config.or_high as f64
        }
        PositionSide::Short if config.or_low > 0 => {
            (config.or_low - b.close) as f64 / config.or_low as f64
        }
        _ => 0.0,
    };
    let _ = a; // A 캔들 자체는 현재 메타에 넣지 않지만 시그니처 유지용으로 받음.
    PendingMeta {
        b_time: b.time,
        b_volume: b.volume,
        b_body_ratio,
        or_breakout_pct,
        b_close: b.close,
        a_range: a_range_raw,
    }
}

fn build_intent(
    gap: &FairValueGap,
    signal_time: NaiveTime,
    config: &SignalEngineConfig,
    meta: PendingMeta,
) -> SignalIntent {
    let side = side_from_fvg(gap.direction);
    let gap_size_pct = if gap.bottom > 0 {
        (gap.top - gap.bottom) as f64 / gap.bottom as f64
    } else {
        0.0
    };
    SignalIntent {
        strategy_id: config.strategy_id.clone(),
        side,
        signal_time,
        stage_name: config.stage_name.clone(),
        entry_zone_high: gap.top,
        entry_zone_low: gap.bottom,
        stop_anchor: gap.stop_loss,
        or_high: config.or_high,
        or_low: config.or_low,
        metadata: SignalMetadata {
            gap_top: gap.top,
            gap_bottom: gap.bottom,
            gap_size_pct,
            b_body_ratio: meta.b_body_ratio,
            or_breakout_pct: meta.or_breakout_pct,
            b_volume: meta.b_volume,
            b_close: meta.b_close,
            a_range: meta.a_range,
            b_time: meta.b_time,
            extra: serde_json::Value::Null,
        },
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn candle(hour: u32, min: u32, open: i64, high: i64, low: i64, close: i64, vol: u64) -> MinuteCandle {
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

    fn base_config() -> SignalEngineConfig {
        SignalEngineConfig {
            strategy_id: "orb_fvg".into(),
            stage_name: "15m".into(),
            or_high: 10_100,
            or_low: 9_900,
            fvg_expiry_candles: 6,
            entry_cutoff: NaiveTime::from_hms_opt(15, 20, 0).unwrap(),
            require_or_breakout: true,
            long_only: true,
            confirmed_side: None,
        }
    }

    #[test]
    fn bullish_fvg_then_retrace_is_detected() {
        // A: 9900~10000, B: 10000~10200 (양봉 돌파), C: 10210~10220 (FVG 형성),
        // D: 10205 리트레이스
        let candles = vec![
            candle(9, 15, 9_950, 10_000, 9_900, 9_980, 1_000), // A
            candle(9, 20, 9_980, 10_200, 9_980, 10_180, 2_000), // B — bullish, body=200 >= 30% of a_range(100)=30
            candle(9, 25, 10_210, 10_230, 10_210, 10_220, 1_500), // C — low=10210 > a.high=10000
            candle(9, 30, 10_220, 10_230, 10_050, 10_150, 1_300), // D — low=10050 ∈ [10000, 10210]
        ];
        let signal = detect_next_fvg_signal(&candles, &base_config()).expect("신호 있어야 함");
        assert_eq!(signal.intent.side, PositionSide::Long);
        assert_eq!(signal.retrace_candle_idx, 3);
        assert_eq!(signal.intent.entry_zone_high, 10_210);
        assert_eq!(signal.intent.entry_zone_low, 10_000);
        assert_eq!(signal.intent.stop_anchor, 9_900);
        assert_eq!(signal.intent.stage_name, "15m");
        assert!(signal.intent.metadata.b_volume > 0);
    }

    #[test]
    fn or_breakout_guard_rejects_if_disabled() {
        // B.close 가 or_high 이하 → require_or_breakout=true 면 신호 무시
        let candles = vec![
            candle(9, 15, 9_900, 9_950, 9_890, 9_930, 1_000), // A (high=9950)
            candle(9, 20, 9_930, 10_050, 9_930, 10_020, 2_000), // B.close=10020 < or_high=10100
            candle(9, 25, 10_060, 10_080, 10_060, 10_070, 1_500), // C
            candle(9, 30, 10_070, 10_075, 9_960, 9_970, 1_300), // retrace into [9950, 10060]
        ];
        let mut cfg = base_config();
        cfg.or_high = 10_100; // 돌파 미달
        assert!(detect_next_fvg_signal(&candles, &cfg).is_none());

        // OR 돌파 조건 풀면 즉시 잡힌다
        cfg.require_or_breakout = false;
        assert!(detect_next_fvg_signal(&candles, &cfg).is_some());
    }

    #[test]
    fn long_only_filters_bearish() {
        // Bearish FVG: A.low > C.high
        let candles = vec![
            candle(9, 15, 10_100, 10_150, 10_080, 10_120, 1_000), // A (low=10080)
            candle(9, 20, 10_120, 10_120, 9_900, 9_920, 2_000),    // B — bearish body
            candle(9, 25, 9_920, 9_950, 9_880, 9_900, 1_500),      // C — high=9950 < A.low=10080
            candle(9, 30, 9_900, 10_020, 9_870, 9_920, 1_300),     // retrace: c5.high=10020 ∈ [9950, 10080]
        ];
        let mut cfg = base_config();
        cfg.or_low = 10_000;
        cfg.or_high = 10_200;

        // long_only=true 면 Bearish 무시
        assert!(detect_next_fvg_signal(&candles, &cfg).is_none());

        // long_only 끄면 Short 신호 감지
        cfg.long_only = false;
        let sig = detect_next_fvg_signal(&candles, &cfg).expect("Short 신호 있어야 함");
        assert_eq!(sig.intent.side, PositionSide::Short);
        assert_eq!(sig.intent.entry_zone_high, 10_080);
        assert_eq!(sig.intent.entry_zone_low, 9_950);
        assert_eq!(sig.intent.stop_anchor, 10_150);
    }

    #[test]
    fn expiry_drops_fvg() {
        // FVG 형성 후 retrace 없이 N 캔들 지나면 폐기 → 이후 후보가 없다면 None
        let a = candle(9, 15, 9_950, 10_000, 9_900, 9_980, 1_000);
        let b = candle(9, 20, 9_980, 10_200, 9_980, 10_180, 2_000);
        let c = candle(9, 25, 10_210, 10_230, 10_210, 10_220, 1_500);
        // retrace 없이 가격 계속 위로만 (09:30, 09:35, ... 10:15 까지 10개)
        let filler: Vec<_> = (0..10)
            .map(|i| {
                let total = 30 + i * 5; // minutes from 09:00
                let (h, m) = (9 + total / 60, total % 60);
                candle(h, m as u32, 10_230, 10_240, 10_225, 10_235, 500)
            })
            .collect();
        let mut candles = vec![a, b, c];
        candles.extend(filler);

        let mut cfg = base_config();
        cfg.fvg_expiry_candles = 3;
        let result = detect_next_fvg_signal(&candles, &cfg);
        assert!(result.is_none(), "expiry 로 폐기되어야 함");
    }

    #[test]
    fn confirmed_side_long_blocks_short() {
        // Bearish FVG 이지만 confirmed_side=Long 이면 거부
        let candles = vec![
            candle(9, 15, 10_100, 10_150, 10_080, 10_120, 1_000),
            candle(9, 20, 10_120, 10_120, 9_900, 9_920, 2_000),
            candle(9, 25, 9_920, 9_950, 9_880, 9_900, 1_500),
            candle(9, 30, 9_900, 10_020, 9_870, 9_920, 1_300),
        ];
        let mut cfg = base_config();
        cfg.or_low = 10_000;
        cfg.long_only = false;
        cfg.confirmed_side = Some(PositionSide::Long);
        assert!(detect_next_fvg_signal(&candles, &cfg).is_none());
    }

    #[test]
    fn entry_cutoff_stops_scan() {
        // cutoff 이후 캔들은 무시
        let candles = vec![
            candle(9, 15, 9_950, 10_000, 9_900, 9_980, 1_000),
            candle(9, 20, 9_980, 10_200, 9_980, 10_180, 2_000),
            candle(9, 25, 10_210, 10_230, 10_210, 10_220, 1_500),
            candle(15, 25, 10_230, 10_240, 10_050, 10_150, 1_300), // retrace 지만 cutoff 뒤
        ];
        let cfg = base_config();
        assert!(detect_next_fvg_signal(&candles, &cfg).is_none());
    }
}
