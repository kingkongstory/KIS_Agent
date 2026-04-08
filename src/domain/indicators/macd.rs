use std::collections::HashMap;

use crate::domain::models::candle::Candle;

use super::moving_average::Ema;
use super::traits::{Indicator, IndicatorError, IndicatorValue, Signal};

/// MACD (Moving Average Convergence Divergence)
pub struct Macd {
    fast_period: usize,
    slow_period: usize,
    signal_period: usize,
    fast_ema: Ema,
    slow_ema: Ema,
    signal_ema: Ema,
}

impl Macd {
    pub fn new(fast: usize, slow: usize, signal: usize) -> Self {
        Self {
            fast_period: fast,
            slow_period: slow,
            signal_period: signal,
            fast_ema: Ema::new(fast),
            slow_ema: Ema::new(slow),
            signal_ema: Ema::new(signal),
        }
    }

    /// 표준 MACD (12, 26, 9)
    pub fn standard() -> Self {
        Self::new(12, 26, 9)
    }

    pub fn next(&mut self, close: f64) -> Option<MacdValues> {
        let fast = self.fast_ema.next(close)?;
        let slow = self.slow_ema.next(close)?;
        let macd_line = fast - slow;
        let signal_line = self.signal_ema.next(macd_line)?;
        let histogram = macd_line - signal_line;

        Some(MacdValues {
            macd: macd_line,
            signal: signal_line,
            histogram,
        })
    }

    fn macd_signal(histogram: f64, prev_histogram: Option<f64>) -> Signal {
        match prev_histogram {
            Some(prev) if prev <= 0.0 && histogram > 0.0 => Signal::Buy,
            Some(prev) if prev >= 0.0 && histogram < 0.0 => Signal::Sell,
            _ => Signal::Hold,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MacdValues {
    pub macd: f64,
    pub signal: f64,
    pub histogram: f64,
}

impl Indicator for Macd {
    fn name(&self) -> &str {
        "macd"
    }

    fn required_candles(&self) -> usize {
        self.slow_period + self.signal_period - 1
    }

    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError> {
        let required = self.required_candles();
        if candles.len() < required {
            return Err(IndicatorError::InsufficientData {
                required,
                actual: candles.len(),
            });
        }

        let mut macd = Macd::new(self.fast_period, self.slow_period, self.signal_period);
        let mut prev_histogram: Option<f64> = None;

        Ok(candles
            .iter()
            .filter_map(|c| {
                let vals = macd.next(c.close_f64())?;
                let signal = Macd::macd_signal(vals.histogram, prev_histogram);
                prev_histogram = Some(vals.histogram);
                Some(IndicatorValue {
                    date: c.date,
                    values: HashMap::from([
                        ("macd".to_string(), vals.macd),
                        ("macd_signal".to_string(), vals.signal),
                        ("macd_histogram".to_string(), vals.histogram),
                    ]),
                    signal,
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        Ok(self.next(candle.close_f64()).map(|vals| IndicatorValue {
            date: candle.date,
            values: HashMap::from([
                ("macd".to_string(), vals.macd),
                ("macd_signal".to_string(), vals.signal),
                ("macd_histogram".to_string(), vals.histogram),
            ]),
            signal: Signal::Hold,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macd_requires_enough_data() {
        let macd = Macd::standard();
        // 필요: 26 + 9 - 1 = 34
        assert_eq!(macd.required_candles(), 34);
    }

    #[test]
    fn test_macd_basic_calculation() {
        let mut macd = Macd::new(3, 5, 3);
        // 충분한 데이터 투입
        let prices = [
            100.0, 102.0, 104.0, 103.0, 105.0, 107.0, 106.0, 108.0, 110.0, 109.0,
        ];
        let mut last = None;
        for p in prices {
            last = macd.next(p);
        }
        let vals = last.unwrap();
        // 상승 추세면 MACD > 0
        assert!(vals.macd > 0.0);
    }
}
