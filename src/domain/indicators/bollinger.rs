use std::collections::{HashMap, VecDeque};

use crate::domain::models::candle::Candle;

use super::traits::{Indicator, IndicatorError, IndicatorValue, Signal};

/// 볼린저 밴드 (Bollinger Bands)
pub struct BollingerBands {
    period: usize,
    multiplier: f64,
    buffer: VecDeque<f64>,
}

impl BollingerBands {
    pub fn new(period: usize, multiplier: f64) -> Self {
        Self {
            period,
            multiplier,
            buffer: VecDeque::with_capacity(period),
        }
    }

    /// 표준 설정 (20, 2.0)
    pub fn standard() -> Self {
        Self::new(20, 2.0)
    }

    pub fn next(&mut self, close: f64) -> Option<BollingerValues> {
        self.buffer.push_back(close);
        if self.buffer.len() > self.period {
            self.buffer.pop_front();
        }

        if self.buffer.len() < self.period {
            return None;
        }

        let mean = self.buffer.iter().sum::<f64>() / self.period as f64;
        let variance = self
            .buffer
            .iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>()
            / self.period as f64;
        let std_dev = variance.sqrt();

        let upper = mean + self.multiplier * std_dev;
        let lower = mean - self.multiplier * std_dev;
        let band_width = upper - lower;

        let bandwidth = if mean != 0.0 {
            band_width / mean * 100.0
        } else {
            0.0
        };

        let percent_b = if band_width != 0.0 {
            (close - lower) / band_width
        } else {
            0.5
        };

        Some(BollingerValues {
            upper,
            middle: mean,
            lower,
            bandwidth,
            percent_b,
        })
    }

    fn bb_signal(percent_b: f64) -> Signal {
        if percent_b < 0.0 {
            Signal::StrongBuy
        } else if percent_b < 0.2 {
            Signal::Buy
        } else if percent_b > 1.0 {
            Signal::StrongSell
        } else if percent_b > 0.8 {
            Signal::Sell
        } else {
            Signal::Hold
        }
    }
}

#[derive(Debug, Clone)]
pub struct BollingerValues {
    pub upper: f64,
    pub middle: f64,
    pub lower: f64,
    pub bandwidth: f64,
    pub percent_b: f64,
}

impl Indicator for BollingerBands {
    fn name(&self) -> &str {
        "bb"
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

        let mut bb = BollingerBands::new(self.period, self.multiplier);

        Ok(candles
            .iter()
            .filter_map(|c| {
                let vals = bb.next(c.close_f64())?;
                Some(IndicatorValue {
                    date: c.date,
                    values: HashMap::from([
                        ("bb_upper".to_string(), vals.upper),
                        ("bb_middle".to_string(), vals.middle),
                        ("bb_lower".to_string(), vals.lower),
                        ("bb_width".to_string(), vals.bandwidth),
                        ("bb_percent".to_string(), vals.percent_b),
                    ]),
                    signal: BollingerBands::bb_signal(vals.percent_b),
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        Ok(self.next(candle.close_f64()).map(|vals| IndicatorValue {
            date: candle.date,
            values: HashMap::from([
                ("bb_upper".to_string(), vals.upper),
                ("bb_middle".to_string(), vals.middle),
                ("bb_lower".to_string(), vals.lower),
                ("bb_width".to_string(), vals.bandwidth),
                ("bb_percent".to_string(), vals.percent_b),
            ]),
            signal: BollingerBands::bb_signal(vals.percent_b),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bollinger_basic() {
        let mut bb = BollingerBands::new(5, 2.0);
        let prices = [100.0, 102.0, 104.0, 103.0, 105.0];
        let mut last = None;
        for p in prices {
            last = bb.next(p);
        }
        let vals = last.unwrap();
        // middle = SMA(5) = 102.8
        assert!((vals.middle - 102.8).abs() < 0.001);
        assert!(vals.upper > vals.middle);
        assert!(vals.lower < vals.middle);
        assert!(vals.bandwidth > 0.0);
    }
}
