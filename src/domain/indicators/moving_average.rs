use std::collections::{HashMap, VecDeque};

use crate::domain::models::candle::Candle;

use super::traits::{Indicator, IndicatorError, IndicatorValue, Signal};

/// 단순 이동평균 (SMA)
pub struct Sma {
    period: usize,
    buffer: VecDeque<f64>,
    sum: f64,
}

impl Sma {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            buffer: VecDeque::with_capacity(period),
            sum: 0.0,
        }
    }

    /// 값 하나를 추가하고 SMA 반환
    pub fn next(&mut self, value: f64) -> Option<f64> {
        self.buffer.push_back(value);
        self.sum += value;

        if self.buffer.len() > self.period {
            self.sum -= self.buffer.pop_front().unwrap();
        }

        if self.buffer.len() == self.period {
            Some(self.sum / self.period as f64)
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.sum = 0.0;
    }
}

impl Indicator for Sma {
    fn name(&self) -> &str {
        "sma"
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
        let key = format!("sma_{}", self.period);

        Ok(candles
            .iter()
            .filter_map(|c| {
                sma.next(c.close_f64()).map(|v| IndicatorValue {
                    date: c.date,
                    values: HashMap::from([(key.clone(), v)]),
                    signal: Signal::Hold,
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        let key = format!("sma_{}", self.period);
        Ok(self.next(candle.close_f64()).map(|v| IndicatorValue {
            date: candle.date,
            values: HashMap::from([(key, v)]),
            signal: Signal::Hold,
        }))
    }
}

/// 지수 이동평균 (EMA)
pub struct Ema {
    period: usize,
    k: f64,
    current: Option<f64>,
    sma: Sma,
    initialized: bool,
}

impl Ema {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            k: 2.0 / (period as f64 + 1.0),
            current: None,
            sma: Sma::new(period),
            initialized: false,
        }
    }

    /// 값 하나를 추가하고 EMA 반환
    pub fn next(&mut self, value: f64) -> Option<f64> {
        if self.initialized {
            let prev = self.current.unwrap();
            let ema = value * self.k + prev * (1.0 - self.k);
            self.current = Some(ema);
            Some(ema)
        } else if let Some(sma_val) = self.sma.next(value) {
            self.current = Some(sma_val);
            self.initialized = true;
            Some(sma_val)
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.current = None;
        self.initialized = false;
        self.sma.reset();
    }
}

impl Indicator for Ema {
    fn name(&self) -> &str {
        "ema"
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

        let mut ema = Ema::new(self.period);
        let key = format!("ema_{}", self.period);

        Ok(candles
            .iter()
            .filter_map(|c| {
                ema.next(c.close_f64()).map(|v| IndicatorValue {
                    date: c.date,
                    values: HashMap::from([(key.clone(), v)]),
                    signal: Signal::Hold,
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        let key = format!("ema_{}", self.period);
        Ok(self.next(candle.close_f64()).map(|v| IndicatorValue {
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
    fn test_sma_5() {
        let mut sma = Sma::new(5);
        assert_eq!(sma.next(100.0), None);
        assert_eq!(sma.next(102.0), None);
        assert_eq!(sma.next(104.0), None);
        assert_eq!(sma.next(103.0), None);
        // SMA(5) = (100+102+104+103+105)/5 = 102.8
        let val = sma.next(105.0).unwrap();
        assert!((val - 102.8).abs() < 0.001);
        // Day 6: 107 → SMA = (102+104+103+105+107)/5 = 104.2
        let val = sma.next(107.0).unwrap();
        assert!((val - 104.2).abs() < 0.001);
        // Day 7: 106 → SMA = (104+103+105+107+106)/5 = 105.0
        let val = sma.next(106.0).unwrap();
        assert!((val - 105.0).abs() < 0.001);
    }

    #[test]
    fn test_ema_5() {
        let mut ema = Ema::new(5);
        // k = 2/(5+1) = 0.3333...
        for v in [100.0, 102.0, 104.0, 103.0] {
            assert_eq!(ema.next(v), None);
        }
        // 초기값 = SMA(5) = 102.8
        let val = ema.next(105.0).unwrap();
        assert!((val - 102.8).abs() < 0.001);
        // Day 6: EMA = 107 * 0.333 + 102.8 * 0.667 ≈ 104.2
        let val = ema.next(107.0).unwrap();
        assert!((val - 104.2).abs() < 0.1);
    }
}
