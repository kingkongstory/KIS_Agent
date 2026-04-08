use async_trait::async_trait;

use crate::domain::error::KisError;
use crate::domain::types::StockCode;

/// 실시간 데이터 구독 유형
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubscriptionType {
    /// 실시간 체결가
    Execution,
    /// 실시간 호가
    OrderBook,
}

/// 실시간 체결 데이터
#[derive(Debug, Clone, serde::Serialize)]
pub struct RealtimeExecution {
    pub stock_code: String,
    pub price: i64,
    pub volume: u64,
    pub change: i64,
    pub change_rate: f64,
    pub time: String,
    pub ask_price: i64,
    pub bid_price: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
}

/// 실시간 호가 데이터
#[derive(Debug, Clone, serde::Serialize)]
pub struct RealtimeOrderBook {
    pub stock_code: String,
    pub asks: Vec<(i64, i64)>,
    pub bids: Vec<(i64, i64)>,
    pub total_ask_volume: i64,
    pub total_bid_volume: i64,
}

/// 실시간 데이터 (통합)
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum RealtimeData {
    Execution(RealtimeExecution),
    OrderBook(RealtimeOrderBook),
}

/// 실시간 데이터 포트
#[async_trait]
pub trait RealtimePort: Send + Sync {
    /// 구독 시작
    async fn subscribe(
        &self,
        stock_code: &StockCode,
        sub_type: SubscriptionType,
    ) -> Result<(), KisError>;

    /// 구독 해제
    async fn unsubscribe(
        &self,
        stock_code: &StockCode,
        sub_type: SubscriptionType,
    ) -> Result<(), KisError>;
}
