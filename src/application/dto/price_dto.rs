use serde::Serialize;

use crate::domain::models::candle::Candle;
use crate::domain::models::price::{AskingPrice, InquirePrice};
use crate::domain::types::PriceChangeSign;

/// 현재가 응답 DTO
#[derive(Debug, Serialize)]
pub struct PriceDto {
    pub price: i64,
    pub change: i64,
    pub change_sign: String,
    pub change_rate: f64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub volume: i64,
    pub amount: i64,
    pub name: String,
}

impl From<InquirePrice> for PriceDto {
    fn from(p: InquirePrice) -> Self {
        let sign = PriceChangeSign::from_code(&p.prdy_vrss_sign);
        let change = sign
            .map(|s| s.apply_sign(p.prdy_vrss))
            .unwrap_or(p.prdy_vrss);
        Self {
            price: p.stck_prpr,
            change,
            change_sign: p.prdy_vrss_sign,
            change_rate: p.prdy_ctrt,
            open: p.stck_oprc,
            high: p.stck_hgpr,
            low: p.stck_lwpr,
            volume: p.acml_vol,
            amount: p.acml_tr_pbmn,
            name: p.hts_kor_isnm,
        }
    }
}

/// 캔들 DTO
#[derive(Debug, Serialize)]
pub struct CandleDto {
    pub date: String,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
}

impl From<Candle> for CandleDto {
    fn from(c: Candle) -> Self {
        Self {
            date: c.date.format("%Y-%m-%d").to_string(),
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
            volume: c.volume,
        }
    }
}

/// 호가 DTO
#[derive(Debug, Serialize)]
pub struct OrderBookDto {
    pub asks: Vec<OrderBookEntry>,
    pub bids: Vec<OrderBookEntry>,
    pub total_ask_volume: i64,
    pub total_bid_volume: i64,
}

#[derive(Debug, Serialize)]
pub struct OrderBookEntry {
    pub price: i64,
    pub volume: i64,
}

impl From<AskingPrice> for OrderBookDto {
    fn from(a: AskingPrice) -> Self {
        let asks = a
            .asks()
            .into_iter()
            .map(|(p, v)| OrderBookEntry {
                price: p,
                volume: v,
            })
            .collect();
        let bids = a
            .bids()
            .into_iter()
            .map(|(p, v)| OrderBookEntry {
                price: p,
                volume: v,
            })
            .collect();
        Self {
            total_ask_volume: a.total_askp_rsqn,
            total_bid_volume: a.total_bidp_rsqn,
            asks,
            bids,
        }
    }
}
