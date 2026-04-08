use async_trait::async_trait;

use crate::domain::error::KisError;
use crate::domain::models::price::{AskingPrice, DailyPriceItem, InquirePrice};
use crate::domain::types::{PeriodCode, StockCode};

/// 시세 데이터 포트
#[async_trait]
pub trait MarketDataPort: Send + Sync {
    /// 현재가 조회
    async fn get_current_price(&self, stock_code: &StockCode) -> Result<InquirePrice, KisError>;

    /// 호가 조회
    async fn get_asking_price(&self, stock_code: &StockCode) -> Result<AskingPrice, KisError>;

    /// 기간별 시세 조회 (OHLCV)
    async fn get_daily_prices(
        &self,
        stock_code: &StockCode,
        start_date: &str,
        end_date: &str,
        period: PeriodCode,
    ) -> Result<Vec<DailyPriceItem>, KisError>;

    /// 종목 검색
    async fn search_stock(&self, keyword: &str) -> Result<Vec<StockSearchResult>, KisError>;
}

/// 종목 검색 결과
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StockSearchResult {
    /// 종목코드
    pub stock_code: String,
    /// 종목명
    pub stock_name: String,
    /// 시장 구분
    pub market: String,
}
