use async_trait::async_trait;

use crate::domain::error::KisError;
use crate::domain::models::account::{AccountSummary, BuyableInfo, PositionItem};
use crate::domain::models::order::ExecutionItem;
use crate::domain::types::StockCode;

/// 계좌 조회 포트
#[async_trait]
pub trait AccountPort: Send + Sync {
    /// 잔고 조회 (보유 종목 + 계좌 합계)
    async fn get_balance(&self) -> Result<(Vec<PositionItem>, AccountSummary), KisError>;

    /// 일별 주문체결 내역 조회
    async fn get_executions(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<ExecutionItem>, KisError>;

    /// 매수 가능 조회
    async fn get_buyable(
        &self,
        stock_code: &StockCode,
        price: i64,
    ) -> Result<BuyableInfo, KisError>;
}
