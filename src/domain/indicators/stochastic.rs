use std::collections::{HashMap, VecDeque};

use crate::domain::models::candle::Candle;

use super::moving_average::Sma;
use super::traits::{Indicator, IndicatorError, IndicatorValue, Signal};

/// 스토캐스틱 오실레이터
pub struct Stochastic {
    k_period: usize,
    d_period: usize,
    high_buffer: VecDeque<f64>,
    low_buffer: VecDeque<f64>,
    k_sma: Sma,
    d_sma: Sma,
}

impl Stochastic {
    pub fn new(k_period: usize, d_period: usize) -> Self {
        Self {
            k_period,
            d_period,
            high_buffer: VecDeque::with_capacity(k_period),
            low_buffer: VecDeque::with_capacity(k_period),
            k_sma: Sma::new(d_period),
            d_sma: Sma::new(d_period),
        }
    }

    /// 표준 설정 (14, 3) — Slow Stochastic
    pub fn standard() -> Self {
        Self::new(14, 3)
    }

    pub fn next(&mut self, high: f64, low: f64, close: f64) -> Option<StochasticValues> {
        self.high_buffer.push_back(high);
        self.low_buffer.push_back(low);

        if self.high_buffer.len() > self.k_period {
            self.high_buffer.pop_front();
            self.low_buffer.pop_front();
        }

        if self.high_buffer.len() < self.k_period {
            return None;
        }

        let highest = self
            .high_buffer
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        let lowest = self
            .low_buffer
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);

        let range = highest - lowest;
        let fast_k = if range > 0.0 {
            (close - lowest) / range * 100.0
        } else {
            50.0
        };

        // Slow Stochastic: %K = SMA(Fast %K), %D = SMA(%K)
        let slow_k = self.k_sma.next(fast_k)?;
        let slow_d = self.d_sma.next(slow_k)?;

        Some(StochasticValues {
            k: slow_k,
            d: slow_d,
        })
    }

    fn stoch_signal(k: f64, d: f64) -> Signal {
        if k < 20.0 && k > d {
            Signal::Buy
        } else if k > 80.0 && k < d {
            Signal::Sell
        } else {
            Signal::Hold
        }
    }
}

#[derive(Debug, Clone)]
pub struct StochasticValues {
    pub k: f64,
    pub d: f64,
}

impl Indicator for Stochastic {
    fn name(&self) -> &str {
        "stochastic"
    }

    fn required_candles(&self) -> usize {
        self.k_period + 2 * self.d_period - 2
    }

    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError> {
        let required = self.required_candles();
        if candles.len() < required {
            return Err(IndicatorError::InsufficientData {
                required,
                actual: candles.len(),
            });
        }

        let mut stoch = Stochastic::new(self.k_period, self.d_period);

        Ok(candles
            .iter()
            .filter_map(|c| {
                let vals = stoch.next(c.high_f64(), c.low_f64(), c.close_f64())?;
                Some(IndicatorValue {
                    date: c.date,
                    values: HashMap::from([
                        ("stoch_k".to_string(), vals.k),
                        ("stoch_d".to_string(), vals.d),
                    ]),
                    signal: Stochastic::stoch_signal(vals.k, vals.d),
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        Ok(self
            .next(candle.high_f64(), candle.low_f64(), candle.close_f64())
            .map(|vals| IndicatorValue {
                date: candle.date,
                values: HashMap::from([
                    ("stoch_k".to_string(), vals.k),
                    ("stoch_d".to_string(), vals.d),
                ]),
                signal: Stochastic::stoch_signal(vals.k, vals.d),
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stochastic_requires_data() {
        let stoch = Stochastic::standard();
        // 14 + 2*3 - 2 = 18
        assert_eq!(stoch.required_candles(), 18);
    }
}
