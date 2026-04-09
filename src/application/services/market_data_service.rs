use std::sync::Arc;

use crate::application::dto::price_dto::{CandleDto, OrderBookDto, PriceDto};
use crate::domain::error::KisError;
use crate::domain::models::candle::Candle;
use crate::domain::ports::cache::CachePort;
use crate::domain::ports::market_data::{MarketDataPort, StockSearchResult};
use crate::domain::types::{PeriodCode, StockCode};
use crate::infrastructure::cache::sqlite_cache::SqliteCache;

/// 시세 서비스 (캐시 + API fallback)
pub struct MarketDataService {
    market_data: Arc<dyn MarketDataPort>,
    price_cache: Arc<dyn CachePort<PriceDto>>,
    orderbook_cache: Arc<dyn CachePort<OrderBookDto>>,
    sqlite_cache: Option<Arc<SqliteCache>>,
}

impl MarketDataService {
    pub fn new(
        market_data: Arc<dyn MarketDataPort>,
        price_cache: Arc<dyn CachePort<PriceDto>>,
        orderbook_cache: Arc<dyn CachePort<OrderBookDto>>,
        sqlite_cache: Option<Arc<SqliteCache>>,
    ) -> Self {
        Self {
            market_data,
            price_cache,
            orderbook_cache,
            sqlite_cache,
        }
    }

    /// 현재가 조회 (5초 캐시)
    pub async fn get_price(&self, stock_code: &StockCode) -> Result<PriceDto, KisError> {
        let key = format!("price:{}", stock_code);
        if let Some(cached) = self.price_cache.get(&key) {
            return Ok(cached);
        }

        let price = self.market_data.get_current_price(stock_code).await?;
        let dto = PriceDto::from(price);
        self.price_cache.set(key, dto.clone());
        Ok(dto)
    }

    /// 호가 조회 (3초 캐시)
    pub async fn get_orderbook(&self, stock_code: &StockCode) -> Result<OrderBookDto, KisError> {
        let key = format!("orderbook:{}", stock_code);
        if let Some(cached) = self.orderbook_cache.get(&key) {
            return Ok(cached);
        }

        let asking = self.market_data.get_asking_price(stock_code).await?;
        let dto = OrderBookDto::from(asking);
        self.orderbook_cache.set(key, dto.clone());
        Ok(dto)
    }

    /// 캔들 데이터 조회 (SQLite 캐시 + API fallback)
    pub async fn get_candles(
        &self,
        stock_code: &StockCode,
        start_date: &str,
        end_date: &str,
        period: PeriodCode,
    ) -> Result<Vec<CandleDto>, KisError> {
        // SQLite 캐시 확인
        if let Some(sqlite) = &self.sqlite_cache {
            let cached = sqlite
                .get_candles(stock_code.as_str(), start_date, end_date)
                .await?;
            if !cached.is_empty() {
                return Ok(cached.into_iter().map(CandleDto::from).collect());
            }
        }

        // API 호출
        let items = self
            .market_data
            .get_daily_prices(stock_code, start_date, end_date, period)
            .await?;

        let candles: Vec<Candle> = items.iter().map(Candle::from).collect();

        // SQLite에 캐싱
        if let Some(sqlite) = &self.sqlite_cache {
            let _ = sqlite.save_candles(stock_code.as_str(), &candles).await;
        }

        Ok(candles.into_iter().map(CandleDto::from).collect())
    }

    /// 종목 검색
    pub async fn search_stock(&self, keyword: &str) -> Result<Vec<StockSearchResult>, KisError> {
        self.market_data.search_stock(keyword).await
    }
}

// PriceDto와 OrderBookDto에 Clone 필요 (캐시용)
impl Clone for PriceDto {
    fn clone(&self) -> Self {
        Self {
            price: self.price,
            change: self.change,
            change_sign: self.change_sign.clone(),
            change_rate: self.change_rate,
            open: self.open,
            high: self.high,
            low: self.low,
            volume: self.volume,
            amount: self.amount,
            name: self.name.clone(),
        }
    }
}

impl Clone for OrderBookDto {
    fn clone(&self) -> Self {
        Self {
            asks: self
                .asks
                .iter()
                .map(|e| crate::application::dto::price_dto::OrderBookEntry {
                    price: e.price,
                    volume: e.volume,
                })
                .collect(),
            bids: self
                .bids
                .iter()
                .map(|e| crate::application::dto::price_dto::OrderBookEntry {
                    price: e.price,
                    volume: e.volume,
                })
                .collect(),
            total_ask_volume: self.total_ask_volume,
            total_bid_volume: self.total_bid_volume,
        }
    }
}
