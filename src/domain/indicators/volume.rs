use std::collections::HashMap;

use crate::domain::models::candle::Candle;

use super::moving_average::Sma;
use super::traits::{Indicator, IndicatorError, IndicatorValue, Signal};

/// OBV (On Balance Volume)
pub struct Obv {
    current: i64,
    prev_close: Option<f64>,
}

impl Default for Obv {
    fn default() -> Self {
        Self::new()
    }
}

impl Obv {
    pub fn new() -> Self {
        Self {
            current: 0,
            prev_close: None,
        }
    }

    pub fn next(&mut self, close: f64, volume: u64) -> i64 {
        if let Some(prev) = self.prev_close {
            if close > prev {
                self.current += volume as i64;
            } else if close < prev {
                self.current -= volume as i64;
            }
        } else {
            self.current = volume as i64;
        }
        self.prev_close = Some(close);
        self.current
    }
}

impl Indicator for Obv {
    fn name(&self) -> &str {
        "obv"
    }

    fn required_candles(&self) -> usize {
        1
    }

    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError> {
        let mut obv = Obv::new();
        Ok(candles
            .iter()
            .map(|c| {
                let val = obv.next(c.close_f64(), c.volume);
                IndicatorValue {
                    date: c.date,
                    values: HashMap::from([("obv".to_string(), val as f64)]),
                    signal: Signal::Hold,
                }
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        let val = self.next(candle.close_f64(), candle.volume);
        Ok(Some(IndicatorValue {
            date: candle.date,
            values: HashMap::from([("obv".to_string(), val as f64)]),
            signal: Signal::Hold,
        }))
    }
}

/// VWAP (Volume Weighted Average Price)
pub struct Vwap {
    cum_tp_vol: f64,
    cum_vol: f64,
}

impl Default for Vwap {
    fn default() -> Self {
        Self::new()
    }
}

impl Vwap {
    pub fn new() -> Self {
        Self {
            cum_tp_vol: 0.0,
            cum_vol: 0.0,
        }
    }

    pub fn next(&mut self, high: f64, low: f64, close: f64, volume: u64) -> f64 {
        let tp = (high + low + close) / 3.0;
        let vol = volume as f64;
        self.cum_tp_vol += tp * vol;
        self.cum_vol += vol;

        if self.cum_vol > 0.0 {
            self.cum_tp_vol / self.cum_vol
        } else {
            0.0
        }
    }
}

impl Indicator for Vwap {
    fn name(&self) -> &str {
        "vwap"
    }

    fn required_candles(&self) -> usize {
        1
    }

    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError> {
        let mut vwap = Vwap::new();
        Ok(candles
            .iter()
            .map(|c| {
                let val = vwap.next(c.high_f64(), c.low_f64(), c.close_f64(), c.volume);
                IndicatorValue {
                    date: c.date,
                    values: HashMap::from([("vwap".to_string(), val)]),
                    signal: Signal::Hold,
                }
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        let val = self.next(
            candle.high_f64(),
            candle.low_f64(),
            candle.close_f64(),
            candle.volume,
        );
        Ok(Some(IndicatorValue {
            date: candle.date,
            values: HashMap::from([("vwap".to_string(), val)]),
            signal: Signal::Hold,
        }))
    }
}

/// 거래량 이동평균
pub struct VolumeMA {
    sma: Sma,
    period: usize,
}

impl VolumeMA {
    pub fn new(period: usize) -> Self {
        Self {
            sma: Sma::new(period),
            period,
        }
    }
}

impl Indicator for VolumeMA {
    fn name(&self) -> &str {
        "volume_ma"
    }

    fn required_candles(&self) -> usize {
        self.period
    }

    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError> {
        if candles.len() < self.period {
            return Err(IndicatorError::InsufficientData {
                required: self.period,
                actual: candles.len(),
            });
        }

        let mut sma = Sma::new(self.period);
        let key = format!("volume_ma_{}", self.period);

        Ok(candles
            .iter()
            .filter_map(|c| {
                sma.next(c.volume_f64()).map(|v| IndicatorValue {
                    date: c.date,
                    values: HashMap::from([(key.clone(), v)]),
                    signal: Signal::Hold,
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        let key = format!("volume_ma_{}", self.period);
        Ok(self.sma.next(candle.volume_f64()).map(|v| IndicatorValue {
            date: candle.date,
            values: HashMap::from([(key, v)]),
            signal: Signal::Hold,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_obv() {
        let mut obv = Obv::new();
        assert_eq!(obv.next(100.0, 50_000), 50_000);
        assert_eq!(obv.next(102.0, 60_000), 110_000); // 상승 → +
        assert_eq!(obv.next(101.0, 40_000), 70_000); // 하락 → -
        assert_eq!(obv.next(104.0, 80_000), 150_000); // 상승 → +
    }

    #[test]
    fn test_vwap() {
        let mut vwap = Vwap::new();
        // TP = (100+100+100)/3 = 100, VWAP = 100
        let val = vwap.next(100.0, 100.0, 100.0, 50_000);
        assert!((val - 100.0).abs() < 0.001);
    }
}
