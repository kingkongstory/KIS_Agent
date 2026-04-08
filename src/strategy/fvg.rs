use super::candle::MinuteCandle;

/// FVG 방향
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FvgDirection {
    Bullish,
    Bearish,
}

/// Fair Value Gap (공정가치갭)
#[derive(Debug, Clone)]
pub struct FairValueGap {
    pub direction: FvgDirection,
    /// 갭 상단 가격
    pub top: i64,
    /// 갭 하단 가격
    pub bottom: i64,
    /// 형성 캔들(B) 인덱스
    pub candle_b_idx: usize,
    /// 손절 가격 (A 캔들의 low/high)
    pub stop_loss: i64,
}

impl FairValueGap {
    /// 가격이 FVG 영역 안에 있는지 (리트레이스 확인)
    pub fn contains_price(&self, price: i64) -> bool {
        price >= self.bottom && price <= self.top
    }

    /// FVG 중간 가격
    pub fn mid_price(&self) -> i64 {
        (self.top + self.bottom) / 2
    }
}

/// 캔들 배열에서 FVG 감지
///
/// 3개 연속 캔들(A, B, C) 검사:
/// - Bullish FVG: A.high < C.low (캔들 B를 건너뛰고 갭 발생)
/// - Bearish FVG: A.low > C.high
pub fn detect_fvg(candles: &[MinuteCandle]) -> Vec<FairValueGap> {
    let mut gaps = Vec::new();

    if candles.len() < 3 {
        return gaps;
    }

    for i in 0..candles.len() - 2 {
        let a = &candles[i];
        let c = &candles[i + 2];

        // Bullish FVG: A의 고가 < C의 저가
        if a.high < c.low {
            gaps.push(FairValueGap {
                direction: FvgDirection::Bullish,
                top: c.low,
                bottom: a.high,
                candle_b_idx: i + 1,
                stop_loss: a.low,
            });
        }
        // Bearish FVG: A의 저가 > C의 고가
        else if a.low > c.high {
            gaps.push(FairValueGap {
                direction: FvgDirection::Bearish,
                top: a.low,
                bottom: c.high,
                candle_b_idx: i + 1,
                stop_loss: a.high,
            });
        }
    }

    gaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    fn candle(min: u32, open: i64, high: i64, low: i64, close: i64) -> MinuteCandle {
        MinuteCandle {
            date: NaiveDate::from_ymd_opt(2026, 4, 8).unwrap(),
            time: NaiveTime::from_hms_opt(9, min, 0).unwrap(),
            open,
            high,
            low,
            close,
            volume: 1000,
        }
    }

    #[test]
    fn test_bullish_fvg() {
        // A: high=100, B: 큰 양봉, C: low=105 → 갭 [100, 105]
        let candles = vec![
            candle(0, 90, 100, 85, 95),
            candle(5, 95, 120, 94, 118),
            candle(10, 118, 125, 105, 122),
        ];
        let gaps = detect_fvg(&candles);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].direction, FvgDirection::Bullish);
        assert_eq!(gaps[0].bottom, 100); // A.high
        assert_eq!(gaps[0].top, 105);    // C.low
        assert_eq!(gaps[0].stop_loss, 85); // A.low
    }

    #[test]
    fn test_bearish_fvg() {
        // A: low=110, B: 큰 음봉, C: high=105 → 갭 [105, 110]
        let candles = vec![
            candle(0, 115, 120, 110, 112),
            candle(5, 112, 113, 90, 92),
            candle(10, 92, 105, 88, 100),
        ];
        let gaps = detect_fvg(&candles);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].direction, FvgDirection::Bearish);
        assert_eq!(gaps[0].top, 110);    // A.low
        assert_eq!(gaps[0].bottom, 105); // C.high
        assert_eq!(gaps[0].stop_loss, 120); // A.high
    }

    #[test]
    fn test_no_fvg() {
        // 겹치는 캔들 → FVG 없음
        let candles = vec![
            candle(0, 100, 110, 90, 105),
            candle(5, 105, 115, 100, 110),
            candle(10, 110, 112, 105, 108),
        ];
        let gaps = detect_fvg(&candles);
        assert!(gaps.is_empty());
    }

    #[test]
    fn test_fvg_contains_price() {
        let fvg = FairValueGap {
            direction: FvgDirection::Bullish,
            top: 105,
            bottom: 100,
            candle_b_idx: 1,
            stop_loss: 85,
        };
        assert!(fvg.contains_price(100));
        assert!(fvg.contains_price(102));
        assert!(fvg.contains_price(105));
        assert!(!fvg.contains_price(99));
        assert!(!fvg.contains_price(106));
    }

    #[test]
    fn test_too_few_candles() {
        let candles = vec![candle(0, 100, 110, 90, 105)];
        assert!(detect_fvg(&candles).is_empty());
    }
}
