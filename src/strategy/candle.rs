use chrono::{NaiveDate, NaiveTime};

/// 분봉 캔들 데이터
#[derive(Debug, Clone)]
pub struct MinuteCandle {
    pub date: NaiveDate,
    pub time: NaiveTime, // 캔들 시작 시각
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
}

impl MinuteCandle {
    /// 양봉 여부
    pub fn is_bullish(&self) -> bool {
        self.close > self.open
    }

    /// 음봉 여부
    pub fn is_bearish(&self) -> bool {
        self.close < self.open
    }

    /// 캔들 범위 (high - low)
    pub fn range(&self) -> i64 {
        self.high - self.low
    }

    /// 몸통 크기 (|close - open|)
    pub fn body_size(&self) -> i64 {
        (self.close - self.open).abs()
    }
}

/// 1분봉을 N분봉으로 집계
///
/// 그룹핑 기준: 09:00 기준 분 단위 슬롯 (minute_from_open / interval)
/// 각 슬롯의 OHLCV: first.open, max(high), min(low), last.close, sum(volume)
pub fn aggregate(candles: &[MinuteCandle], interval_min: u32) -> Vec<MinuteCandle> {
    if candles.is_empty() || interval_min <= 1 {
        return candles.to_vec();
    }

    let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();

    // 슬롯 번호 계산: 09:00 기준 몇 번째 interval 슬롯인지
    let slot_key = |c: &MinuteCandle| -> (NaiveDate, u32) {
        let mins_from_open = (c.time - market_open).num_minutes().max(0) as u32;
        (c.date, mins_from_open / interval_min)
    };

    let mut result = Vec::new();
    let mut i = 0;

    while i < candles.len() {
        let key = slot_key(&candles[i]);
        let mut high = candles[i].high;
        let mut low = candles[i].low;
        let mut volume = candles[i].volume;
        let open = candles[i].open;
        let date = candles[i].date;
        let time = candles[i].time;
        let mut close = candles[i].close;
        let mut j = i + 1;

        while j < candles.len() && slot_key(&candles[j]) == key {
            high = high.max(candles[j].high);
            low = low.min(candles[j].low);
            volume += candles[j].volume;
            close = candles[j].close;
            j += 1;
        }

        result.push(MinuteCandle {
            date,
            time,
            open,
            high,
            low,
            close,
            volume,
        });

        i = j;
    }

    result
}

/// 5분봉 ATR (Average True Range) 계산
///
/// 최근 N개 캔들의 True Range 평균.
/// True Range = max(high-low, |high-prev_close|, |low-prev_close|)
pub fn calc_atr(candles: &[MinuteCandle], period: usize) -> i64 {
    if candles.len() < 2 {
        return if candles.is_empty() {
            0
        } else {
            candles[0].high - candles[0].low
        };
    }

    let start = if candles.len() > period {
        candles.len() - period
    } else {
        0
    };

    let mut sum = 0i64;
    let mut count = 0;
    for i in start.max(1)..candles.len() {
        let prev_close = candles[i - 1].close;
        let tr = (candles[i].high - candles[i].low)
            .max((candles[i].high - prev_close).abs())
            .max((candles[i].low - prev_close).abs());
        sum += tr;
        count += 1;
    }

    if count == 0 { 0 } else { sum / count as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candle(hour: u32, min: u32, open: i64, high: i64, low: i64, close: i64) -> MinuteCandle {
        MinuteCandle {
            date: NaiveDate::from_ymd_opt(2026, 4, 8).unwrap(),
            time: NaiveTime::from_hms_opt(hour, min, 0).unwrap(),
            open,
            high,
            low,
            close,
            volume: 1000,
        }
    }

    #[test]
    fn test_aggregate_1m_to_5m() {
        let candles = vec![
            make_candle(9, 0, 100, 110, 95, 105),
            make_candle(9, 1, 105, 115, 100, 110),
            make_candle(9, 2, 110, 120, 105, 115),
            make_candle(9, 3, 115, 118, 108, 112),
            make_candle(9, 4, 112, 125, 110, 120),
            // 다음 5분 슬롯
            make_candle(9, 5, 120, 130, 118, 125),
            make_candle(9, 6, 125, 128, 120, 122),
        ];

        let agg = aggregate(&candles, 5);
        assert_eq!(agg.len(), 2);

        // 첫 5분봉: open=100, high=125, low=95, close=120
        assert_eq!(agg[0].open, 100);
        assert_eq!(agg[0].high, 125);
        assert_eq!(agg[0].low, 95);
        assert_eq!(agg[0].close, 120);
        assert_eq!(agg[0].volume, 5000);

        // 두 번째 5분봉 (미완성): open=120, high=130, low=118, close=122
        assert_eq!(agg[1].open, 120);
        assert_eq!(agg[1].high, 130);
        assert_eq!(agg[1].low, 118);
        assert_eq!(agg[1].close, 122);
    }

    #[test]
    fn test_aggregate_1m_to_15m() {
        // 09:00 ~ 09:14 = 슬롯 0 (OR 캔들)
        let mut candles = Vec::new();
        for m in 0..15 {
            candles.push(make_candle(9, m, 100 + m as i64, 110 + m as i64, 90, 105 + m as i64));
        }
        // 09:15 = 슬롯 1
        candles.push(make_candle(9, 15, 200, 210, 195, 205));

        let agg = aggregate(&candles, 15);
        assert_eq!(agg.len(), 2);
        // 첫 15분봉: open=100 (first), low=90 (min), high=124 (max of 110+14)
        assert_eq!(agg[0].open, 100);
        assert_eq!(agg[0].high, 124);
        assert_eq!(agg[0].low, 90);
        assert_eq!(agg[0].close, 119); // last: 105+14
    }

    #[test]
    fn test_aggregate_empty() {
        let agg = aggregate(&[], 5);
        assert!(agg.is_empty());
    }

    #[test]
    fn test_bullish_bearish() {
        let bull = make_candle(9, 0, 100, 110, 95, 105);
        assert!(bull.is_bullish());
        assert!(!bull.is_bearish());

        let bear = make_candle(9, 0, 105, 110, 95, 100);
        assert!(bear.is_bearish());
        assert!(!bear.is_bullish());
    }
}
