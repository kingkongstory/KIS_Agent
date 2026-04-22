use std::collections::HashMap;

use crate::domain::models::candle::Candle;

use super::bollinger::BollingerBands;
use super::macd::Macd;
use super::moving_average::{Ema, Sma};
use super::rsi::Rsi;
use super::stochastic::Stochastic;
use super::traits::{Indicator, IndicatorError, IndicatorValue};
use super::volume::{Obv, VolumeMA, Vwap};

/// 지표 엔진: 이름 목록으로부터 지표 인스턴스를 생성하고 계산
pub struct IndicatorEngine;

impl IndicatorEngine {
    /// 지표 이름 문자열을 파싱하여 지표 인스턴스 생성
    ///
    /// 지원 형식:
    /// - `sma_20`, `ema_12` — 이동평균 (기간 지정)
    /// - `rsi_14` — RSI
    /// - `macd` — 표준 MACD (12, 26, 9)
    /// - `bb` — 표준 볼린저 밴드 (20, 2.0)
    /// - `stochastic` — 표준 스토캐스틱 (14, 3)
    /// - `obv` — OBV
    /// - `vwap` — VWAP
    /// - `volume_ma_20` — 거래량 이동평균
    pub fn create_indicator(name: &str) -> Option<Box<dyn Indicator>> {
        let parts: Vec<&str> = name.split('_').collect();

        match parts[0] {
            "sma" => {
                let period = parts.get(1)?.parse::<usize>().ok()?;
                Some(Box::new(Sma::new(period)))
            }
            "ema" => {
                let period = parts.get(1)?.parse::<usize>().ok()?;
                Some(Box::new(Ema::new(period)))
            }
            "rsi" => {
                let period = parts
                    .get(1)
                    .and_then(|p| p.parse::<usize>().ok())
                    .unwrap_or(14);
                Some(Box::new(Rsi::new(period)))
            }
            "macd" => Some(Box::new(Macd::standard())),
            "bb" => Some(Box::new(BollingerBands::standard())),
            "stochastic" | "stoch" => Some(Box::new(Stochastic::standard())),
            "obv" => Some(Box::new(Obv::new())),
            "vwap" => Some(Box::new(Vwap::new())),
            "volume" if parts.get(1) == Some(&"ma") => {
                let period = parts.get(2)?.parse::<usize>().ok()?;
                Some(Box::new(VolumeMA::new(period)))
            }
            _ => None,
        }
    }

    /// 여러 지표를 일괄 계산
    pub fn calculate_all(
        names: &[String],
        candles: &[Candle],
    ) -> HashMap<String, Result<Vec<IndicatorValue>, IndicatorError>> {
        names
            .iter()
            .filter_map(|name| {
                let indicator = Self::create_indicator(name)?;
                let result = indicator.calculate(candles);
                Some((name.clone(), result))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_indicator() {
        assert!(IndicatorEngine::create_indicator("sma_20").is_some());
        assert!(IndicatorEngine::create_indicator("ema_12").is_some());
        assert!(IndicatorEngine::create_indicator("rsi_14").is_some());
        assert!(IndicatorEngine::create_indicator("macd").is_some());
        assert!(IndicatorEngine::create_indicator("bb").is_some());
        assert!(IndicatorEngine::create_indicator("stochastic").is_some());
        assert!(IndicatorEngine::create_indicator("obv").is_some());
        assert!(IndicatorEngine::create_indicator("vwap").is_some());
        assert!(IndicatorEngine::create_indicator("volume_ma_20").is_some());
        assert!(IndicatorEngine::create_indicator("invalid").is_none());
    }

    #[test]
    fn test_calculate_all() {
        use chrono::NaiveDate;

        let candles: Vec<Candle> = (0..30)
            .map(|i| Candle {
                date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap() + chrono::Duration::days(i),
                open: 100 + i as i64,
                high: 105 + i as i64,
                low: 95 + i as i64,
                close: 102 + i as i64,
                volume: 10000 + i as u64 * 100,
            })
            .collect();

        let names = vec!["sma_5".to_string(), "rsi_14".to_string(), "obv".to_string()];
        let results = IndicatorEngine::calculate_all(&names, &candles);

        assert_eq!(results.len(), 3);
        assert!(results["sma_5"].is_ok());
        assert!(results["rsi_14"].is_ok());
        assert!(results["obv"].is_ok());
    }
}
