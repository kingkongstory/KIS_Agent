use async_trait::async_trait;

use crate::domain::error::KisError;
use crate::domain::models::order::{
    CancelOrderRequest, ModifyOrderRequest, OrderRequest, OrderResponse,
};

/// 주문 실행 포트
#[async_trait]
pub trait TradingPort: Send + Sync {
    /// 주문 실행 (매수/매도)
    async fn place_order(&self, request: &OrderRequest) -> Result<OrderResponse, KisError>;

    /// 주문 정정
    async fn modify_order(&self, request: &ModifyOrderRequest) -> Result<OrderResponse, KisError>;

    /// 주문 취소
    async fn cancel_order(&self, request: &CancelOrderRequest) -> Result<OrderResponse, KisError>;
}
