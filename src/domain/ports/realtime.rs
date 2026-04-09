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

/// 실시간 분봉 캔들 (틱 집계)
#[derive(Debug, Clone, serde::Serialize)]
pub struct RealtimeCandleUpdate {
    pub stock_code: String,
    /// 캔들 시작 시각 "HH:MM"
    pub time: String,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
    /// true: 완성된 캔들, false: 진행 중
    pub is_closed: bool,
}

/// 주문 체결 알림 (잔고 갱신 트리거)
#[derive(Debug, Clone, serde::Serialize)]
pub struct TradeNotification {
    pub stock_code: String,
    pub stock_name: String,
    /// "entry" | "exit"
    pub action: String,
}

/// 현재가 스냅샷 (스케줄러 → 프론트엔드 push)
#[derive(Debug, Clone, serde::Serialize)]
pub struct PriceSnapshot {
    pub stock_code: String,
    pub name: String,
    pub price: i64,
    pub change: i64,
    pub change_sign: String,
    pub change_rate: f64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub volume: i64,
    pub amount: i64,
}

/// 잔고 스냅샷 (스케줄러 → 프론트엔드 push)
#[derive(Debug, Clone, serde::Serialize)]
pub struct BalanceSnapshot {
    pub positions: Vec<BalancePosition>,
    pub summary: BalanceSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BalancePosition {
    pub stock_code: String,
    pub stock_name: String,
    pub quantity: i64,
    pub avg_price: f64,
    pub current_price: i64,
    pub profit_loss: i64,
    pub profit_loss_rate: f64,
    pub purchase_amount: i64,
    pub eval_amount: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BalanceSummary {
    pub cash: i64,
    pub total_eval: i64,
    pub total_profit_loss: i64,
    pub total_purchase: i64,
}

/// 실시간 데이터 (통합)
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum RealtimeData {
    Execution(RealtimeExecution),
    OrderBook(RealtimeOrderBook),
    CandleUpdate(RealtimeCandleUpdate),
    TradeNotification(TradeNotification),
    PriceSnapshot(PriceSnapshot),
    BalanceSnapshot(BalanceSnapshot),
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
