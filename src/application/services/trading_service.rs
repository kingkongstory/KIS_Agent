use std::sync::Arc;

use crate::application::dto::order_dto::OrderResponseDto;
use crate::domain::error::KisError;
use crate::domain::models::order::{CancelOrderRequest, ModifyOrderRequest, OrderRequest};
use crate::domain::ports::trading::TradingPort;

/// 주문 서비스
pub struct TradingService {
    trading: Arc<dyn TradingPort>,
}

impl TradingService {
    pub fn new(trading: Arc<dyn TradingPort>) -> Self {
        Self { trading }
    }

    /// 주문 실행
    pub async fn place_order(&self, request: OrderRequest) -> Result<OrderResponseDto, KisError> {
        request.validate()?;
        let response = self.trading.place_order(&request).await?;
        Ok(OrderResponseDto::from(response))
    }

    /// 주문 정정
    pub async fn modify_order(
        &self,
        request: ModifyOrderRequest,
    ) -> Result<OrderResponseDto, KisError> {
        let response = self.trading.modify_order(&request).await?;
        Ok(OrderResponseDto::from(response))
    }

    /// 주문 취소
    pub async fn cancel_order(
        &self,
        request: CancelOrderRequest,
    ) -> Result<OrderResponseDto, KisError> {
        let response = self.trading.cancel_order(&request).await?;
        Ok(OrderResponseDto::from(response))
    }
}
