use serde::{Deserialize, Serialize};

use crate::domain::error::KisError;
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::{StockCode, align_price, tick_size};

/// 매수/매도 구분
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    /// 매수
    Buy,
    /// 매도
    Sell,
}

/// 주문 유형
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// 지정가 ("00")
    Limit,
    /// 시장가 ("01")
    Market,
    /// 조건부지정가 ("02")
    ConditionalLimit,
    /// 최유리지정가 ("03")
    BestOpposite,
    /// 최우선지정가 ("04")
    BestOwn,
    /// 장전시간외 ("05")
    BeforeMarket,
    /// 장후시간외 ("06")
    AfterMarket,
}

impl OrderType {
    /// KIS API 주문구분 코드
    pub fn code(&self) -> &str {
        match self {
            Self::Limit => "00",
            Self::Market => "01",
            Self::ConditionalLimit => "02",
            Self::BestOpposite => "03",
            Self::BestOwn => "04",
            Self::BeforeMarket => "05",
            Self::AfterMarket => "06",
        }
    }

    /// 가격 입력이 필요한 주문 유형인지
    pub fn requires_price(&self) -> bool {
        matches!(self, Self::Limit | Self::ConditionalLimit)
    }
}

/// 주문 요청
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// 종목코드
    pub stock_code: StockCode,
    /// 매수/매도
    pub side: OrderSide,
    /// 주문 유형
    pub order_type: OrderType,
    /// 주문 수량
    pub quantity: u64,
    /// 주문 단가 (시장가는 0)
    pub price: i64,
}

impl OrderRequest {
    /// 주문 유효성 검증
    pub fn validate(&self) -> Result<(), KisError> {
        if self.quantity == 0 {
            return Err(KisError::OrderValidation(
                "주문 수량은 0보다 커야 합니다".into(),
            ));
        }

        if self.order_type.requires_price() {
            if self.price <= 0 {
                return Err(KisError::OrderValidation(
                    "지정가 주문은 가격을 입력해야 합니다".into(),
                ));
            }

            let tick = tick_size(self.price);
            if self.price % tick != 0 {
                return Err(KisError::OrderValidation(format!(
                    "가격 {}은(는) 호가 단위 {}에 맞지 않습니다. 올바른 가격: {}",
                    self.price,
                    tick,
                    align_price(self.price)
                )));
            }
        }

        Ok(())
    }

    /// KIS API 주문구분 코드
    pub fn ord_dvsn(&self) -> &str {
        self.order_type.code()
    }

    /// KIS API 주문단가 (시장가면 "0")
    pub fn ord_unpr(&self) -> String {
        if self.order_type.requires_price() {
            self.price.to_string()
        } else {
            "0".to_string()
        }
    }
}

/// 주문 응답
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    /// 거래소 전송 주문 조직번호
    #[serde(rename = "KRX_FWDG_ORD_ORGNO")]
    pub krx_fwdg_ord_orgno: String,

    /// 주문번호
    #[serde(rename = "ODNO")]
    pub order_no: String,

    /// 주문시각 (HHMMSS)
    #[serde(rename = "ORD_TMD")]
    pub order_time: String,
}

/// 주문 정정 요청
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifyOrderRequest {
    /// 원 주문번호
    pub original_order_no: String,
    /// 원 거래소 주문 조직번호
    pub original_krx_orgno: String,
    /// 정정 주문 유형
    pub order_type: OrderType,
    /// 정정 수량 (0이면 전량)
    pub quantity: u64,
    /// 정정 가격
    pub price: i64,
}

/// 주문 취소 요청
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderRequest {
    /// 원 주문번호
    pub original_order_no: String,
    /// 원 거래소 주문 조직번호
    pub original_krx_orgno: String,
    /// 취소 수량 (0이면 전량)
    pub quantity: u64,
}

/// 체결 내역 항목
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionItem {
    /// 주문일자
    #[serde(
        default,
        deserialize_with = "crate::domain::serde_utils::optional_kis_date::deserialize",
        serialize_with = "crate::domain::serde_utils::optional_kis_date::serialize"
    )]
    pub ord_dt: Option<chrono::NaiveDate>,

    /// 주문시각 (HHMMSS)
    #[serde(default)]
    pub ord_tmd: String,

    /// 주문번호
    #[serde(default)]
    pub odno: String,

    /// 매도/매수 구분명
    #[serde(default)]
    pub sll_buy_dvsn_cd_name: String,

    /// 종목코드
    #[serde(default)]
    pub pdno: String,

    /// 종목명
    #[serde(default)]
    pub prdt_name: String,

    /// 주문수량
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub ord_qty: i64,

    /// 주문단가
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub ord_unpr: i64,

    /// 총체결수량
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub tot_ccld_qty: i64,

    /// 체결평균가
    #[serde(
        default,
        deserialize_with = "crate::domain::serde_utils::string_to_f64::deserialize",
        serialize_with = "crate::domain::serde_utils::string_to_f64::serialize"
    )]
    pub avg_prvs: f64,

    /// 체결상태명
    #[serde(default)]
    pub ccld_cndt_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_request_validate_market_order() {
        let req = OrderRequest {
            stock_code: StockCode::new("005930").unwrap(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 10,
            price: 0,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_order_request_validate_limit_order_valid() {
        let req = OrderRequest {
            stock_code: StockCode::new("005930").unwrap(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 10,
            price: 72000, // 호가단위 50에 맞음 (20000~50000 구간: 50단위)
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_order_request_validate_zero_quantity() {
        let req = OrderRequest {
            stock_code: StockCode::new("005930").unwrap(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 0,
            price: 72000,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_order_request_validate_invalid_price() {
        let req = OrderRequest {
            stock_code: StockCode::new("005930").unwrap(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 10,
            price: 72003, // 호가단위에 안 맞음
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_order_request_validate_limit_no_price() {
        let req = OrderRequest {
            stock_code: StockCode::new("005930").unwrap(),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            quantity: 5,
            price: 0,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_order_response_deserialize() {
        let json = r#"{
            "KRX_FWDG_ORD_ORGNO": "91252",
            "ODNO": "0000123456",
            "ORD_TMD": "093015"
        }"#;
        let resp: OrderResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.order_no, "0000123456");
        assert_eq!(resp.order_time, "093015");
    }
}
