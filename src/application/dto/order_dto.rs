use serde::{Deserialize, Serialize};

use crate::domain::models::order::{OrderRequest, OrderResponse, OrderSide, OrderType};
use crate::domain::types::StockCode;

/// 주문 요청 DTO (프론트엔드로부터 수신)
#[derive(Debug, Deserialize)]
pub struct OrderRequestDto {
    pub stock_code: String,
    pub side: String,         // "buy" | "sell"
    pub order_type: String,   // "limit" | "market"
    pub quantity: u64,
    pub price: Option<i64>,
}

impl OrderRequestDto {
    pub fn into_domain(self) -> Result<OrderRequest, String> {
        let stock_code =
            StockCode::new(&self.stock_code).map_err(|e| e.to_string())?;
        let side = match self.side.as_str() {
            "buy" => OrderSide::Buy,
            "sell" => OrderSide::Sell,
            _ => return Err(format!("잘못된 주문 방향: {}", self.side)),
        };
        let order_type = match self.order_type.as_str() {
            "limit" => OrderType::Limit,
            "market" => OrderType::Market,
            "best_opposite" => OrderType::BestOpposite,
            "best_own" => OrderType::BestOwn,
            _ => return Err(format!("잘못된 주문 유형: {}", self.order_type)),
        };

        Ok(OrderRequest {
            stock_code,
            side,
            order_type,
            quantity: self.quantity,
            price: self.price.unwrap_or(0),
        })
    }
}

/// 주문 응답 DTO
#[derive(Debug, Serialize)]
pub struct OrderResponseDto {
    pub order_no: String,
    pub order_time: String,
}

impl From<OrderResponse> for OrderResponseDto {
    fn from(r: OrderResponse) -> Self {
        Self {
            order_no: r.order_no,
            order_time: r.order_time,
        }
    }
}
