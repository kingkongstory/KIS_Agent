//! 2026-04-19 PR #1.e.1 (execution-followup-plan): KOSPI200 지수 레짐 판정 순수 로직.
//!
//! # 소스 계약 (사용자 결정, 2026-04-19)
//!
//! - **권위 소스**: KIS REST 업종 분봉조회 (KOSPI200 1분 OHLC → 5분봉 집계).
//! - **fallback**: KIS 국내업종 시간별지수(분) (권위 소스 불가 시).
//! - **보조**: KIS WS `H0UPCNT0` 업종체결 (armed/intrabar invalidation 전용, regime 판정에는 미사용).
//! - **제외**: Yahoo 지수 데이터 (실행 gate 소스에서 제외).
//!
//! 이 모듈은 데이터 페칭 책임이 **없다**. 호출자가 확정된 `IndexCandle` 시퀀스를
//! 제공하면 레짐을 판정해 돌려주는 순수 계산만 한다. 네트워킹은 별도 계층(PR #1.e.3).
//!
//! # v1 규칙 (사용자 결정)
//!
//! - OR 창: 09:00 ~ 09:15 (`cfg.or_start`, `cfg.or_end`).
//! - 평가 대상: `time >= or_end` 인 확정 5분봉 (즉 09:15 봉부터).
//! - 기본 분류 (`classify_single_candle`):
//!   - `close > OR_HIGH && close > open` → `Bullish`
//!   - `close < OR_LOW && close < open` → `Bearish`
//!   - 그 외 → `Neutral`
//! - 최근 `lookback_candles` (기본 2) 봉 안에 Bullish/Bearish 후보가 **동시에** 존재하면
//!   reversal 로 판정해 `Neutral` 로 강등.
//! - 매핑: Bullish → "122630", Bearish → "114800", Neutral/Unknown → `None`.
//!
//! # 이 모듈에서 하지 않는 일
//!
//! - 지수 분봉 수집/파싱 (PR #1.e.3).
//! - `ExternalGates` 쓰기 (PR #1.e.5 에서 `GateUpdater` 호출).
//! - intrabar invalidation 로직 (armed watch 쪽 PR #2).

use chrono::NaiveTime;

use super::live_runner::IndexRegime;

/// KOSPI200 지수 5분봉 한 개.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexCandle {
    /// 봉 시작 시각 (KST, 예: 09:00 → 09:00 ~ 09:04:59 구간).
    pub time: NaiveTime,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegimeEngineConfig {
    /// OR 시작 시각 (inclusive). 기본 09:00.
    pub or_start: NaiveTime,
    /// OR 종료 시각 (exclusive). 기본 09:15 — 이 시각 이후 봉부터 평가 대상.
    pub or_end: NaiveTime,
    /// reversal 판정 lookback. 현재 봉 포함 최근 N 개 봉 안에
    /// Bullish/Bearish 후보가 동시 존재하면 Neutral 로 강등. 기본 2.
    pub lookback_candles: usize,
}

impl Default for RegimeEngineConfig {
    fn default() -> Self {
        Self {
            or_start: NaiveTime::from_hms_opt(9, 0, 0).expect("09:00 is valid"),
            or_end: NaiveTime::from_hms_opt(9, 15, 0).expect("09:15 is valid"),
            lookback_candles: 2,
        }
    }
}

/// OR 구간에 속하는 봉들에서 고/저를 계산. OR 봉이 하나도 없으면 `None`.
pub fn or_range(candles: &[IndexCandle], cfg: &RegimeEngineConfig) -> Option<(i64, i64)> {
    let mut iter = candles
        .iter()
        .filter(|c| c.time >= cfg.or_start && c.time < cfg.or_end);
    let first = iter.next()?;
    let (mut hi, mut lo) = (first.high, first.low);
    for c in iter {
        if c.high > hi {
            hi = c.high;
        }
        if c.low < lo {
            lo = c.low;
        }
    }
    Some((hi, lo))
}

/// 단일 봉 분류. OR 돌파 + 방향 일치 여부만 본다 (reversal lookback 없음).
pub fn classify_single_candle(c: &IndexCandle, or_high: i64, or_low: i64) -> IndexRegime {
    if c.close > or_high && c.close > c.open {
        IndexRegime::Bullish
    } else if c.close < or_low && c.close < c.open {
        IndexRegime::Bearish
    } else {
        IndexRegime::Neutral
    }
}

/// 오늘 확정된 KOSPI200 5분봉 시퀀스에서 현재 레짐을 판정.
///
/// OR 미완성이거나 post-OR 봉이 없으면 `IndexRegime::Unknown` — producer 는
/// 이 값을 그대로 `GateUpdater::set_regime()` 로 세팅하고, `refresh_preflight_metadata`
/// 에서 `regime_confirmed=false` 로 축약되어 신규 진입이 차단된다.
pub fn classify_regime(candles: &[IndexCandle], cfg: &RegimeEngineConfig) -> IndexRegime {
    let (or_high, or_low) = match or_range(candles, cfg) {
        Some(v) => v,
        None => return IndexRegime::Unknown,
    };

    let post_or: Vec<&IndexCandle> = candles.iter().filter(|c| c.time >= cfg.or_end).collect();
    let last = match post_or.last() {
        Some(c) => *c,
        None => return IndexRegime::Unknown,
    };

    // 최근 lookback 개 봉(현재 봉 포함) 에 양 방향 돌파 동시 존재 → reversal.
    let take_n = cfg.lookback_candles.max(1);
    let recent_classifications: Vec<IndexRegime> = post_or
        .iter()
        .rev()
        .take(take_n)
        .map(|c| classify_single_candle(c, or_high, or_low))
        .collect();
    let has_bullish = recent_classifications.contains(&IndexRegime::Bullish);
    let has_bearish = recent_classifications.contains(&IndexRegime::Bearish);
    if has_bullish && has_bearish {
        return IndexRegime::Neutral;
    }

    classify_single_candle(last, or_high, or_low)
}

/// 레짐 → 진입 허용 ETF 종목코드 매핑 (사용자 결정, 2026-04-19):
/// - `Bullish` → `"122630"` (KODEX 레버리지)
/// - `Bearish` → `"114800"` (KODEX 인버스)
/// - `Neutral` / `Unknown` → `None` (신규 진입 차단)
pub fn map_regime_to_instrument(regime: IndexRegime) -> Option<String> {
    match regime {
        IndexRegime::Bullish => Some("122630".to_string()),
        IndexRegime::Bearish => Some("114800".to_string()),
        IndexRegime::Neutral | IndexRegime::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(h: u32, m: u32, open: i64, high: i64, low: i64, close: i64) -> IndexCandle {
        IndexCandle {
            time: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            open,
            high,
            low,
            close,
        }
    }

    #[test]
    fn or_range_returns_none_when_empty() {
        let cfg = RegimeEngineConfig::default();
        assert!(or_range(&[], &cfg).is_none());
    }

    #[test]
    fn or_range_aggregates_three_five_minute_bars() {
        let cfg = RegimeEngineConfig::default();
        // OR = 09:00, 09:05, 09:10 (3 bars)
        let candles = vec![
            candle(9, 0, 400_00, 405_00, 398_00, 403_00), // high=405
            candle(9, 5, 403_00, 410_00, 402_00, 408_00), // high=410
            candle(9, 10, 408_00, 409_00, 395_00, 397_00), // low=395
        ];
        assert_eq!(or_range(&candles, &cfg), Some((410_00, 395_00)));
    }

    #[test]
    fn classify_single_candle_bullish_requires_close_over_or_high_and_positive_candle() {
        let or_high = 410_00;
        let or_low = 395_00;
        // 양봉 + close > OR_HIGH
        let c = candle(9, 15, 408_00, 415_00, 408_00, 412_00);
        assert_eq!(
            classify_single_candle(&c, or_high, or_low),
            IndexRegime::Bullish
        );
        // close > OR_HIGH 지만 음봉
        let c_neg = candle(9, 15, 415_00, 416_00, 411_00, 411_00);
        // close=411 < open=415 음봉. close=411 < OR_HIGH=410? 411>410 → Bullish 후보이지만 close<open → Neutral
        assert_eq!(
            classify_single_candle(&c_neg, or_high, or_low),
            IndexRegime::Neutral
        );
    }

    #[test]
    fn classify_single_candle_bearish_requires_close_under_or_low_and_negative_candle() {
        let or_high = 410_00;
        let or_low = 395_00;
        let c = candle(9, 15, 398_00, 399_00, 390_00, 392_00); // close<OR_LOW, 음봉
        assert_eq!(
            classify_single_candle(&c, or_high, or_low),
            IndexRegime::Bearish
        );
        // close < OR_LOW 이지만 양봉 (꼬리만 뚫음)
        let c_pos = candle(9, 15, 391_00, 396_00, 390_00, 393_00);
        // close=393 < OR_LOW=395 만족, open=391 → close>open 양봉 → Neutral
        assert_eq!(
            classify_single_candle(&c_pos, or_high, or_low),
            IndexRegime::Neutral
        );
    }

    #[test]
    fn classify_regime_unknown_when_or_incomplete() {
        let cfg = RegimeEngineConfig::default();
        // OR 시간 이전 봉만
        let candles = vec![candle(8, 55, 400_00, 405_00, 398_00, 403_00)];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Unknown);
    }

    #[test]
    fn classify_regime_unknown_when_no_post_or_candles() {
        let cfg = RegimeEngineConfig::default();
        // OR 만 있고 09:15 이후 봉 없음
        let candles = vec![
            candle(9, 0, 400_00, 410_00, 395_00, 405_00),
            candle(9, 5, 405_00, 408_00, 403_00, 407_00),
        ];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Unknown);
    }

    #[test]
    fn classify_regime_bullish_when_last_bar_breaks_high_with_positive_body() {
        let cfg = RegimeEngineConfig::default();
        let candles = vec![
            candle(9, 0, 400_00, 410_00, 395_00, 405_00), // OR high=410
            candle(9, 15, 408_00, 415_00, 407_00, 414_00), // close=414>410, 양봉
        ];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Bullish);
    }

    #[test]
    fn classify_regime_bearish_when_last_bar_breaks_low_with_negative_body() {
        let cfg = RegimeEngineConfig::default();
        let candles = vec![
            candle(9, 0, 400_00, 410_00, 395_00, 405_00), // OR low=395
            candle(9, 15, 398_00, 398_00, 388_00, 390_00), // close=390<395, 음봉
        ];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Bearish);
    }

    #[test]
    fn classify_regime_reversal_downgrades_to_neutral() {
        let cfg = RegimeEngineConfig::default();
        // OR high=410, OR low=395.
        // 09:15 Bearish, 09:20 Bullish → 최근 2개에 양 방향 혼재 → Neutral.
        let candles = vec![
            candle(9, 0, 400_00, 410_00, 395_00, 405_00),
            candle(9, 15, 400_00, 400_00, 388_00, 390_00), // Bearish
            candle(9, 20, 392_00, 415_00, 392_00, 414_00), // Bullish
        ];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Neutral);
    }

    #[test]
    fn classify_regime_no_reversal_when_older_than_lookback() {
        let cfg = RegimeEngineConfig::default();
        // lookback=2 기본. 3개 봉 전 Bearish → lookback 밖이라 영향 없음.
        let candles = vec![
            candle(9, 0, 400_00, 410_00, 395_00, 405_00),
            candle(9, 15, 400_00, 400_00, 388_00, 390_00), // Bearish (lookback 밖)
            candle(9, 20, 392_00, 400_00, 392_00, 399_00), // Neutral
            candle(9, 25, 399_00, 416_00, 399_00, 415_00), // Bullish, 직전은 Neutral
        ];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Bullish);
    }

    #[test]
    fn classify_regime_returns_last_bar_when_only_neutral_surrounding() {
        let cfg = RegimeEngineConfig::default();
        let candles = vec![
            candle(9, 0, 400_00, 410_00, 395_00, 405_00),
            candle(9, 15, 405_00, 409_00, 401_00, 407_00), // close<OR_HIGH → Neutral
        ];
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Neutral);
    }

    #[test]
    fn map_regime_to_instrument_covers_all_variants() {
        assert_eq!(
            map_regime_to_instrument(IndexRegime::Bullish).as_deref(),
            Some("122630")
        );
        assert_eq!(
            map_regime_to_instrument(IndexRegime::Bearish).as_deref(),
            Some("114800")
        );
        assert_eq!(map_regime_to_instrument(IndexRegime::Neutral), None);
        assert_eq!(map_regime_to_instrument(IndexRegime::Unknown), None);
    }

    /// 경계 테스트: close == OR_HIGH 는 "> OR_HIGH" 조건 미달 → Bullish 아님.
    #[test]
    fn classify_single_candle_boundary_close_equals_or_high_is_neutral() {
        let or_high = 410_00;
        let or_low = 395_00;
        let c = candle(9, 15, 408_00, 411_00, 408_00, 410_00); // close == OR_HIGH
        assert_eq!(
            classify_single_candle(&c, or_high, or_low),
            IndexRegime::Neutral
        );
    }

    /// OR 봉에 9:14:59 의 애매한 끝자락: `or_end=09:15` exclusive 라 09:10 봉까지만 OR.
    #[test]
    fn or_end_is_exclusive_boundary() {
        let cfg = RegimeEngineConfig::default();
        let candles = vec![
            candle(9, 10, 400_00, 410_00, 395_00, 405_00),
            // 09:15 봉은 post-OR
            candle(9, 15, 405_00, 415_00, 404_00, 414_00),
        ];
        let (hi, lo) = or_range(&candles, &cfg).unwrap();
        assert_eq!(hi, 410_00);
        assert_eq!(lo, 395_00);
        assert_eq!(classify_regime(&candles, &cfg), IndexRegime::Bullish);
    }
}
