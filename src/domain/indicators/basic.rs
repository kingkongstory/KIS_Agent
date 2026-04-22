use crate::domain::models::candle::Candle;

/// 표준편차 계산
pub fn calc_std(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    variance.sqrt()
}

/// 수익률 계산 (일별 변화율)
pub fn calc_returns(candles: &[Candle]) -> Vec<f64> {
    candles
        .windows(2)
        .map(|w| {
            let prev = w[0].close_f64();
            let curr = w[1].close_f64();
            if prev != 0.0 {
                (curr - prev) / prev * 100.0
            } else {
                0.0
            }
        })
        .collect()
}

/// 이격도 (Disparity = 현재가 / MA * 100)
pub fn calc_disparity(close: f64, ma: f64) -> f64 {
    if ma != 0.0 { close / ma * 100.0 } else { 100.0 }
}

/// 변동성 (수익률의 표준편차)
pub fn calc_volatility(candles: &[Candle]) -> f64 {
    let returns = calc_returns(candles);
    calc_std(&returns)
}

/// ROC (Rate of Change) = (현재 - n일 전) / n일 전 * 100
pub fn calc_roc(candles: &[Candle], period: usize) -> Vec<f64> {
    if candles.len() <= period {
        return vec![];
    }
    candles[period..]
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let prev = candles[i].close_f64();
            if prev != 0.0 {
                (c.close_f64() - prev) / prev * 100.0
            } else {
                0.0
            }
        })
        .collect()
}

/// ATR (Average True Range)
pub fn calc_atr(candles: &[Candle], period: usize) -> Vec<f64> {
    if candles.len() < 2 {
        return vec![];
    }

    // True Range 계산
    let true_ranges: Vec<f64> = candles
        .windows(2)
        .map(|w| {
            let high = w[1].high_f64();
            let low = w[1].low_f64();
            let prev_close = w[0].close_f64();
            let tr1 = high - low;
            let tr2 = (high - prev_close).abs();
            let tr3 = (low - prev_close).abs();
            tr1.max(tr2).max(tr3)
        })
        .collect();

    if true_ranges.len() < period {
        return vec![];
    }

    let mut atr_values = Vec::with_capacity(true_ranges.len() - period + 1);

    // 첫 ATR = 단순 평균
    let first_atr: f64 = true_ranges[..period].iter().sum::<f64>() / period as f64;
    atr_values.push(first_atr);

    // 이후 Wilder's smoothing
    let mut prev_atr = first_atr;
    for &tr in &true_ranges[period..] {
        let atr = (prev_atr * (period as f64 - 1.0) + tr) / period as f64;
        atr_values.push(atr);
        prev_atr = atr;
    }

    atr_values
}

/// N일 최고가
pub fn calc_high_since(candles: &[Candle], period: usize) -> Option<i64> {
    if candles.len() < period {
        return None;
    }
    candles[candles.len() - period..]
        .iter()
        .map(|c| c.high)
        .max()
}

/// N일 최저가
pub fn calc_low_since(candles: &[Candle], period: usize) -> Option<i64> {
    if candles.len() < period {
        return None;
    }
    candles[candles.len() - period..]
        .iter()
        .map(|c| c.low)
        .min()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn make_candle(close: i64, high: i64, low: i64) -> Candle {
        Candle {
            date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
            open: close,
            high,
            low,
            close,
            volume: 1000,
        }
    }

    #[test]
    fn test_calc_std() {
        let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let std = calc_std(&values);
        assert!((std - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_calc_returns() {
        let candles = vec![make_candle(100, 105, 95), make_candle(110, 115, 105)];
        let returns = calc_returns(&candles);
        assert!((returns[0] - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_calc_disparity() {
        assert!((calc_disparity(105.0, 100.0) - 105.0).abs() < 0.001);
    }

    #[test]
    fn test_calc_roc() {
        let candles = vec![
            make_candle(100, 105, 95),
            make_candle(105, 110, 100),
            make_candle(110, 115, 105),
        ];
        let roc = calc_roc(&candles, 1);
        assert!((roc[0] - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_calc_high_low_since() {
        let candles = vec![
            make_candle(100, 110, 90),
            make_candle(105, 115, 95),
            make_candle(102, 108, 92),
        ];
        assert_eq!(calc_high_since(&candles, 3), Some(115));
        assert_eq!(calc_low_since(&candles, 3), Some(90));
    }
}
