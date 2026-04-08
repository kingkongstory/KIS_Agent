use async_trait::async_trait;
use std::sync::Arc;

use super::http_client::{HttpMethod, KisHttpClient};
use crate::domain::error::KisError;
use crate::domain::models::order::{
    CancelOrderRequest, ModifyOrderRequest, OrderRequest, OrderResponse, OrderSide,
};
use crate::domain::ports::trading::TradingPort;
use crate::domain::types::TransactionId;

/// KIS 주문 어댑터
pub struct KisTradingAdapter {
    client: Arc<KisHttpClient>,
}

impl KisTradingAdapter {
    pub fn new(client: Arc<KisHttpClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TradingPort for KisTradingAdapter {
    async fn place_order(&self, request: &OrderRequest) -> Result<OrderResponse, KisError> {
        request.validate()?;

        let tr_id = match request.side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": request.stock_code.as_str(),
            "ORD_DVSN": request.ord_dvsn(),
            "ORD_QTY": request.quantity.to_string(),
            "ORD_UNPR": request.ord_unpr(),
        });

        let resp = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &tr_id,
                None,
                Some(&body),
            )
            .await?;

        resp.into_result()
    }

    async fn modify_order(&self, request: &ModifyOrderRequest) -> Result<OrderResponse, KisError> {
        let qty_all = if request.quantity == 0 { "Y" } else { "N" };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "KRX_FWDG_ORD_ORGNO": request.original_krx_orgno,
            "ORGN_ODNO": request.original_order_no,
            "ORD_DVSN": request.order_type.code(),
            "RVSE_CNCL_DVSN_CD": "01", // 정정
            "ORD_QTY": if request.quantity == 0 { "0".to_string() } else { request.quantity.to_string() },
            "ORD_UNPR": request.price.to_string(),
            "QTY_ALL_ORD_YN": qty_all,
        });

        let resp = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-rvsecncl",
                &TransactionId::OrderModify,
                None,
                Some(&body),
            )
            .await?;

        resp.into_result()
    }

    async fn cancel_order(&self, request: &CancelOrderRequest) -> Result<OrderResponse, KisError> {
        let qty_all = if request.quantity == 0 { "Y" } else { "N" };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "KRX_FWDG_ORD_ORGNO": request.original_krx_orgno,
            "ORGN_ODNO": request.original_order_no,
            "ORD_DVSN": "00",
            "RVSE_CNCL_DVSN_CD": "02", // 취소
            "ORD_QTY": if request.quantity == 0 { "0".to_string() } else { request.quantity.to_string() },
            "ORD_UNPR": "0",
            "QTY_ALL_ORD_YN": qty_all,
        });

        let resp = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-rvsecncl",
                &TransactionId::OrderCancel,
                None,
                Some(&body),
            )
            .await?;

        resp.into_result()
    }
}
