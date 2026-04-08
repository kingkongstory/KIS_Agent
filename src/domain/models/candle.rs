use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::price::DailyPriceItem;

/// OHLCV 캔들 데이터
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub date: NaiveDate,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
}

impl Candle {
    /// 캔들의 종가를 f64로 반환 (지표 계산용)
    pub fn close_f64(&self) -> f64 {
        self.close as f64
    }

    /// 캔들의 고가를 f64로 반환
    pub fn high_f64(&self) -> f64 {
        self.high as f64
    }

    /// 캔들의 저가를 f64로 반환
    pub fn low_f64(&self) -> f64 {
        self.low as f64
    }

    /// 캔들의 거래량을 f64로 반환
    pub fn volume_f64(&self) -> f64 {
        self.volume as f64
    }
}

impl From<&DailyPriceItem> for Candle {
    fn from(item: &DailyPriceItem) -> Self {
        Candle {
            date: item.stck_bsop_date,
            open: item.stck_oprc,
            high: item.stck_hgpr,
            low: item.stck_lwpr,
            close: item.stck_clpr,
            volume: item.acml_vol as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_candle_from_daily_price_item() {
        let item = DailyPriceItem {
            stck_bsop_date: NaiveDate::from_ymd_opt(2025, 1, 15).unwrap(),
            stck_oprc: 72000,
            stck_hgpr: 73000,
            stck_lwpr: 71500,
            stck_clpr: 72300,
            acml_vol: 15_000_000,
            acml_tr_pbmn: 0,
            prdy_vrss: 0,
            prdy_vrss_sign: String::new(),
        };
        let candle = Candle::from(&item);
        assert_eq!(candle.open, 72000);
        assert_eq!(candle.close, 72300);
        assert_eq!(candle.volume, 15_000_000);
    }
}
