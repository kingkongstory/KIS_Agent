use std::sync::Arc;

use crate::application::dto::account_dto::{BalanceDto, BuyableDto, ExecutionDto, PositionDto, SummaryDto};
use crate::domain::error::KisError;
use crate::domain::ports::account::AccountPort;
use crate::domain::types::StockCode;

/// 계좌 서비스
pub struct AccountService {
    account: Arc<dyn AccountPort>,
}

impl AccountService {
    pub fn new(account: Arc<dyn AccountPort>) -> Self {
        Self { account }
    }

    /// 잔고 조회
    pub async fn get_balance(&self) -> Result<BalanceDto, KisError> {
        let (positions, summary) = self.account.get_balance().await?;
        Ok(BalanceDto {
            positions: positions.into_iter().map(PositionDto::from).collect(),
            summary: SummaryDto::from(summary),
        })
    }

    /// 체결 내역 조회
    pub async fn get_executions(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<ExecutionDto>, KisError> {
        let items = self.account.get_executions(start_date, end_date).await?;
        Ok(items.into_iter().map(ExecutionDto::from).collect())
    }

    /// 매수 가능 조회
    pub async fn get_buyable(
        &self,
        stock_code: &StockCode,
        price: i64,
    ) -> Result<BuyableDto, KisError> {
        let info = self.account.get_buyable(stock_code, price).await?;
        Ok(BuyableDto::from(info))
    }
}
