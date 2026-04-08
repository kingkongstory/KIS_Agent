use chrono::NaiveDate;
use sqlx::sqlite::SqlitePool;
use tracing::info;

use crate::domain::error::KisError;
use crate::domain::models::candle::Candle;

/// SQLite OHLCV 캐시
pub struct SqliteCache {
    pool: SqlitePool,
}

impl SqliteCache {
    pub async fn new(database_url: &str) -> Result<Self, KisError> {
        let pool = SqlitePool::connect(database_url)
            .await
            .map_err(|e| KisError::Internal(format!("SQLite 연결 실패: {e}")))?;

        let cache = Self { pool };
        cache.migrate().await?;
        Ok(cache)
    }

    /// 테이블 자동 생성
    async fn migrate(&self) -> Result<(), KisError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ohlcv (
                stock_code TEXT NOT NULL,
                date TEXT NOT NULL,
                open INTEGER NOT NULL,
                high INTEGER NOT NULL,
                low INTEGER NOT NULL,
                close INTEGER NOT NULL,
                volume INTEGER NOT NULL,
                PRIMARY KEY (stock_code, date)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        info!("SQLite OHLCV 캐시 테이블 준비 완료");
        Ok(())
    }

    /// 캔들 데이터 저장 (UPSERT)
    pub async fn save_candles(
        &self,
        stock_code: &str,
        candles: &[Candle],
    ) -> Result<(), KisError> {
        for candle in candles {
            sqlx::query(
                "INSERT OR REPLACE INTO ohlcv (stock_code, date, open, high, low, close, volume)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(stock_code)
            .bind(candle.date.format("%Y-%m-%d").to_string())
            .bind(candle.open)
            .bind(candle.high)
            .bind(candle.low)
            .bind(candle.close)
            .bind(candle.volume as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| KisError::Internal(format!("캔들 저장 실패: {e}")))?;
        }
        Ok(())
    }

    /// 캔들 데이터 조회
    pub async fn get_candles(
        &self,
        stock_code: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<Candle>, KisError> {
        let rows = sqlx::query_as::<_, OhlcvRow>(
            "SELECT date, open, high, low, close, volume FROM ohlcv
             WHERE stock_code = ? AND date >= ? AND date <= ?
             ORDER BY date ASC",
        )
        .bind(stock_code)
        .bind(start_date)
        .bind(end_date)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("캔들 조회 실패: {e}")))?;

        Ok(rows.into_iter().filter_map(|r| r.into_candle()).collect())
    }

    /// 특정 종목의 최신 날짜 조회
    pub async fn get_latest_date(&self, stock_code: &str) -> Result<Option<NaiveDate>, KisError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT date FROM ohlcv WHERE stock_code = ? ORDER BY date DESC LIMIT 1",
        )
        .bind(stock_code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("날짜 조회 실패: {e}")))?;

        Ok(row.and_then(|(d,)| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()))
    }
}

#[derive(sqlx::FromRow)]
struct OhlcvRow {
    date: String,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: i64,
}

impl OhlcvRow {
    fn into_candle(self) -> Option<Candle> {
        let date = NaiveDate::parse_from_str(&self.date, "%Y-%m-%d").ok()?;
        Some(Candle {
            date,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume as u64,
        })
    }
}
