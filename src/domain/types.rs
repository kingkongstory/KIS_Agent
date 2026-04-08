use serde::{Deserialize, Serialize};
use std::fmt;

use super::error::KisError;

/// 투자 환경 (실전/모의)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Environment {
    /// 실전투자
    Real,
    /// 모의투자
    Paper,
}

impl Environment {
    /// REST API 기본 URL
    pub fn rest_base_url(&self) -> &str {
        match self {
            Self::Real => "https://openapi.koreainvestment.com:9443",
            Self::Paper => "https://openapivts.koreainvestment.com:29443",
        }
    }

    /// WebSocket URL
    pub fn ws_url(&self) -> &str {
        match self {
            Self::Real => "ws://ops.koreainvestment.com:21000",
            Self::Paper => "ws://ops.koreainvestment.com:31000",
        }
    }

    /// 모의투자 여부
    pub fn is_paper(&self) -> bool {
        matches!(self, Self::Paper)
    }
}

/// 거래 ID (실전/모의에 따라 코드가 다름)
#[derive(Debug, Clone)]
pub enum TransactionId {
    /// 현금 매수주문
    OrderCashBuy,
    /// 현금 매도주문
    OrderCashSell,
    /// 주문 정정
    OrderModify,
    /// 주문 취소
    OrderCancel,
    /// 주식 현재가 조회
    InquirePrice,
    /// 주식 호가 조회
    InquireAskingPrice,
    /// 기간별 시세 (일/주/월)
    InquireDailyPrice,
    /// 잔고 조회
    InquireBalance,
    /// 일별 주문체결 조회
    InquireDailyExecution,
    /// 매수 가능 조회
    InquireBuyable,
    /// 주식 당일 분봉 조회
    InquireTimePrice,
    /// 종목 검색 (조건검색)
    SearchStock,
}

impl TransactionId {
    /// 환경에 따른 tr_id 코드 반환
    pub fn code(&self, env: Environment) -> &str {
        match (self, env) {
            // 주문
            (Self::OrderCashBuy, Environment::Real) => "TTTC0802U",
            (Self::OrderCashBuy, Environment::Paper) => "VTTC0802U",
            (Self::OrderCashSell, Environment::Real) => "TTTC0801U",
            (Self::OrderCashSell, Environment::Paper) => "VTTC0801U",
            (Self::OrderModify, Environment::Real) => "TTTC0803U",
            (Self::OrderModify, Environment::Paper) => "VTTC0803U",
            (Self::OrderCancel, Environment::Real) => "TTTC0803U",
            (Self::OrderCancel, Environment::Paper) => "VTTC0803U",
            // 시세
            (Self::InquirePrice, _) => "FHKST01010100",
            (Self::InquireAskingPrice, _) => "FHKST01010200",
            (Self::InquireDailyPrice, _) => "FHKST03010100",
            (Self::InquireTimePrice, _) => "FHKST03010200",
            // 계좌
            (Self::InquireBalance, Environment::Real) => "TTTC8434R",
            (Self::InquireBalance, Environment::Paper) => "VTTC8434R",
            (Self::InquireDailyExecution, Environment::Real) => "TTTC8001R",
            (Self::InquireDailyExecution, Environment::Paper) => "VTTC8001R",
            (Self::InquireBuyable, Environment::Real) => "TTTC8908R",
            (Self::InquireBuyable, Environment::Paper) => "VTTC8908R",
            // 검색
            (Self::SearchStock, _) => "CTPF1002R",
        }
    }
}

/// 종목코드 (6자리 숫자 문자열)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StockCode(String);

impl StockCode {
    /// 6자리 ASCII 숫자 검증 후 생성
    pub fn new(code: &str) -> Result<Self, KisError> {
        if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Self(code.to_string()))
        } else {
            Err(KisError::InvalidStockCode(format!(
                "종목코드는 6자리 숫자여야 합니다: {code}"
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StockCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 시장 구분 코드
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MarketCode {
    /// 주식/ETF/ETN
    Stock,
    /// ELW
    Elw,
}

impl MarketCode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stock => "J",
            Self::Elw => "W",
        }
    }
}

/// 전일대비 부호
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceChangeSign {
    /// 상한 ("1")
    UpperLimit,
    /// 상승 ("2")
    Rise,
    /// 보합 ("3")
    Flat,
    /// 하한 ("4")
    LowerLimit,
    /// 하락 ("5")
    Fall,
}

impl PriceChangeSign {
    /// 문자열 코드에서 변환
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "1" => Some(Self::UpperLimit),
            "2" => Some(Self::Rise),
            "3" => Some(Self::Flat),
            "4" => Some(Self::LowerLimit),
            "5" => Some(Self::Fall),
            _ => None,
        }
    }

    /// 값에 부호 적용 (하락/하한이면 음수)
    pub fn apply_sign(&self, value: i64) -> i64 {
        match self {
            Self::LowerLimit | Self::Fall => -value.abs(),
            Self::Flat => 0,
            _ => value.abs(),
        }
    }

    /// 상승 여부
    pub fn is_rise(&self) -> bool {
        matches!(self, Self::UpperLimit | Self::Rise)
    }
}

/// 호가 단위 (가격대별)
pub fn tick_size(price: i64) -> i64 {
    match price {
        p if p < 2_000 => 1,
        p if p < 5_000 => 5,
        p if p < 20_000 => 10,
        p if p < 50_000 => 50,
        p if p < 200_000 => 100,
        p if p < 500_000 => 500,
        _ => 1_000,
    }
}

/// 가격을 호가 단위에 맞춤 (내림)
pub fn align_price(price: i64) -> i64 {
    let tick = tick_size(price);
    (price / tick) * tick
}

/// 기간 구분
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PeriodCode {
    /// 일봉
    Day,
    /// 주봉
    Week,
    /// 월봉
    Month,
}

impl PeriodCode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Day => "D",
            Self::Week => "W",
            Self::Month => "M",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_urls() {
        assert!(Environment::Real.rest_base_url().contains("openapi.koreainvestment"));
        assert!(Environment::Paper.rest_base_url().contains("openapivts"));
        assert!(Environment::Real.ws_url().contains("21000"));
        assert!(Environment::Paper.ws_url().contains("31000"));
    }

    #[test]
    fn test_transaction_id_codes() {
        assert_eq!(TransactionId::OrderCashBuy.code(Environment::Real), "TTTC0802U");
        assert_eq!(TransactionId::OrderCashBuy.code(Environment::Paper), "VTTC0802U");
        assert_eq!(TransactionId::InquirePrice.code(Environment::Real), "FHKST01010100");
        assert_eq!(TransactionId::InquirePrice.code(Environment::Paper), "FHKST01010100");
    }

    #[test]
    fn test_stock_code_valid() {
        assert!(StockCode::new("005930").is_ok());
        assert!(StockCode::new("000660").is_ok());
    }

    #[test]
    fn test_stock_code_invalid() {
        assert!(StockCode::new("12345").is_err()); // 5자리
        assert!(StockCode::new("1234567").is_err()); // 7자리
        assert!(StockCode::new("ABCDEF").is_err()); // 영문
        assert!(StockCode::new("00593A").is_err()); // 숫자+영문
    }

    #[test]
    fn test_tick_size() {
        assert_eq!(tick_size(1_500), 1);
        assert_eq!(tick_size(3_000), 5);
        assert_eq!(tick_size(15_000), 10);
        assert_eq!(tick_size(30_000), 50);
        assert_eq!(tick_size(100_000), 100);
        assert_eq!(tick_size(300_000), 500);
        assert_eq!(tick_size(600_000), 1_000);
    }

    #[test]
    fn test_align_price() {
        assert_eq!(align_price(15_003), 15_000);
        assert_eq!(align_price(15_010), 15_010);
        assert_eq!(align_price(50_150), 50_100);
        assert_eq!(align_price(301_234), 301_000);
    }

    #[test]
    fn test_price_change_sign() {
        assert_eq!(PriceChangeSign::from_code("2"), Some(PriceChangeSign::Rise));
        assert_eq!(PriceChangeSign::Rise.apply_sign(500), 500);
        assert_eq!(PriceChangeSign::Fall.apply_sign(500), -500);
        assert_eq!(PriceChangeSign::Flat.apply_sign(500), 0);
    }
}
