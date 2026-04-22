use std::collections::HashMap;

use crate::domain::models::candle::Candle;

use super::traits::{Indicator, IndicatorError, IndicatorValue, Signal};

/// RSI (Relative Strength Index) — Wilder's smoothing
pub struct Rsi {
    period: usize,
    avg_gain: Option<f64>,
    avg_loss: Option<f64>,
    prev_close: Option<f64>,
    gains: Vec<f64>,
    losses: Vec<f64>,
}

impl Rsi {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            avg_gain: None,
            avg_loss: None,
            prev_close: None,
            gains: Vec::with_capacity(period),
            losses: Vec::with_capacity(period),
        }
    }

    pub fn next(&mut self, close: f64) -> Option<f64> {
        let result = if let Some(prev) = self.prev_close {
            let change = close - prev;
            let gain = if change > 0.0 { change } else { 0.0 };
            let loss = if change < 0.0 { -change } else { 0.0 };

            match (self.avg_gain, self.avg_loss) {
                (Some(ag), Some(al)) => {
                    let n = self.period as f64;
                    let new_ag = (ag * (n - 1.0) + gain) / n;
                    let new_al = (al * (n - 1.0) + loss) / n;
                    self.avg_gain = Some(new_ag);
                    self.avg_loss = Some(new_al);

                    if new_al == 0.0 {
                        Some(100.0)
                    } else {
                        Some(100.0 - (100.0 / (1.0 + new_ag / new_al)))
                    }
                }
                _ => {
                    self.gains.push(gain);
                    self.losses.push(loss);

                    if self.gains.len() == self.period {
                        let ag: f64 = self.gains.iter().sum::<f64>() / self.period as f64;
                        let al: f64 = self.losses.iter().sum::<f64>() / self.period as f64;
                        self.avg_gain = Some(ag);
                        self.avg_loss = Some(al);

                        if al == 0.0 {
                            Some(100.0)
                        } else {
                            Some(100.0 - (100.0 / (1.0 + ag / al)))
                        }
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        };

        self.prev_close = Some(close);
        result
    }

    fn rsi_signal(rsi: f64) -> Signal {
        match rsi {
            v if v >= 80.0 => Signal::StrongSell,
            v if v >= 70.0 => Signal::Sell,
            v if v <= 20.0 => Signal::StrongBuy,
            v if v <= 30.0 => Signal::Buy,
            _ => Signal::Hold,
        }
    }

    pub fn reset(&mut self) {
        self.avg_gain = None;
        self.avg_loss = None;
        self.prev_close = None;
        self.gains.clear();
        self.losses.clear();
    }
}

impl Indicator for Rsi {
    fn name(&self) -> &str {
        "rsi"
    }

    fn required_candles(&self) -> usize {
        self.period + 1
    }

    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError> {
        if candles.len() < self.period + 1 {
            return Err(IndicatorError::InsufficientData {
                required: self.period + 1,
                actual: candles.len(),
            });
        }

        let mut rsi = Rsi::new(self.period);
        let key = format!("rsi_{}", self.period);

        Ok(candles
            .iter()
            .filter_map(|c| {
                rsi.next(c.close_f64()).map(|v| IndicatorValue {
                    date: c.date,
                    values: HashMap::from([(key.clone(), v)]),
                    signal: Rsi::rsi_signal(v),
                })
            })
            .collect())
    }

    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError> {
        let key = format!("rsi_{}", self.period);
        Ok(self.next(candle.close_f64()).map(|v| IndicatorValue {
            date: candle.date,
            values: HashMap::from([(key, v)]),
            signal: Rsi::rsi_signal(v),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rsi_basic() {
        let mut rsi = Rsi::new(14);
        // 15개의 가격 데이터 (14 변화)
        let prices = [
            100.0, 101.0, 102.0, 103.0, 104.0, 103.0, 104.0, 105.0, 106.0, 107.0, 106.0, 107.0,
            108.0, 109.0, 110.0,
        ];
        let mut last_rsi = None;
        for p in prices {
            last_rsi = rsi.next(p);
        }
        let val = last_rsi.unwrap();
        // 대부분 상승 → RSI > 50
        assert!(val > 50.0);
        assert!(val <= 100.0);
    }

    #[test]
    fn test_rsi_all_up() {
        let mut rsi = Rsi::new(5);
        let prices = [100.0, 101.0, 102.0, 103.0, 104.0, 105.0];
        let mut last = None;
        for p in prices {
            last = rsi.next(p);
        }
        // 모두 상승이면 RSI = 100
        assert!((last.unwrap() - 100.0).abs() < 0.001);
    }
}
