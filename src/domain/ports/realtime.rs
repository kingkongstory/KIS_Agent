use async_trait::async_trait;

use crate::domain::error::KisError;
use crate::domain::types::StockCode;

/// 실시간 데이터 구독 유형
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubscriptionType {
    /// 실시간 체결가 (H0STCNT0)
    Execution,
    /// 실시간 호가 (H0STASP0)
    OrderBook,
    /// 체결 통보 (H0STCNI0 실전 / H0STCNI9 모의)
    ExecutionNotice,
    /// 장운영 정보 (H0STMKO0)
    MarketOperation,
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

/// 체결 통보 (H0STCNI0/H0STCNI9) — AES-256-CBC 복호화 후 데이터
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutionNotice {
    /// 주문번호 [2]
    pub order_no: String,
    /// 종목코드 [8]
    pub stock_code: String,
    /// 종목명 [24]
    pub stock_name: String,
    /// 매도매수구분 [4]: "01"=매도, "02"=매수
    pub side: String,
    /// 체결수량 [9]
    pub filled_qty: u64,
    /// 체결단가 [10]
    pub filled_price: i64,
    /// 주문수량 [16]
    pub order_qty: u64,
    /// 체결 확정 여부 ([13]=="2")
    pub is_filled: bool,
    /// 체결시간 [11]
    pub timestamp: String,
}

/// 장운영 정보 (H0STMKO0) — 평문 전송
#[derive(Debug, Clone, serde::Serialize)]
pub struct MarketOperation {
    /// 종목코드 [0]
    pub stock_code: String,
    /// 거래정지여부 [1]: "Y"/"N"
    pub is_trading_halt: bool,
    /// 거래정지사유 [2]
    pub halt_reason: String,
    /// 장운영구분코드 [3]
    pub market_operation_code: String,
    /// VI적용구분코드 [8]: "0"=미적용, "1"=정적VI, "2"=동적VI, "3"=정적+동적
    pub vi_applied: String,
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
    ExecutionNotice(ExecutionNotice),
    MarketOperation(MarketOperation),
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
