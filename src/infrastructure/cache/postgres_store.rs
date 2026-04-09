use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use sqlx::postgres::PgPool;
use tracing::info;

use crate::domain::error::KisError;
use crate::domain::models::candle::Candle;

/// PostgreSQL 데이터 저장소
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub async fn new(database_url: &str) -> Result<Self, KisError> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| KisError::Internal(format!("PostgreSQL 연결 실패: {e}")))?;

        let store = Self { pool };
        store.migrate().await?;
        info!("PostgreSQL 연결 완료");
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), KisError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS daily_ohlcv (
                stock_code VARCHAR(10) NOT NULL,
                date DATE NOT NULL,
                open BIGINT NOT NULL,
                high BIGINT NOT NULL,
                low BIGINT NOT NULL,
                close BIGINT NOT NULL,
                volume BIGINT NOT NULL,
                amount BIGINT DEFAULT 0,
                change_value BIGINT DEFAULT 0,
                change_sign VARCHAR(1) DEFAULT '',
                created_at TIMESTAMPTZ DEFAULT NOW(),
                PRIMARY KEY (stock_code, date)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS minute_ohlcv (
                stock_code VARCHAR(10) NOT NULL,
                datetime TIMESTAMP NOT NULL,
                open BIGINT NOT NULL,
                high BIGINT NOT NULL,
                low BIGINT NOT NULL,
                close BIGINT NOT NULL,
                volume BIGINT NOT NULL,
                interval_min SMALLINT NOT NULL DEFAULT 1,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                PRIMARY KEY (stock_code, datetime, interval_min)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS index_daily (
                index_code VARCHAR(10) NOT NULL,
                date DATE NOT NULL,
                open DOUBLE PRECISION NOT NULL,
                high DOUBLE PRECISION NOT NULL,
                low DOUBLE PRECISION NOT NULL,
                close DOUBLE PRECISION NOT NULL,
                volume BIGINT NOT NULL,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                PRIMARY KEY (index_code, date)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS stock_master (
                stock_code VARCHAR(10) PRIMARY KEY,
                stock_name VARCHAR(100) NOT NULL,
                market VARCHAR(10) NOT NULL DEFAULT 'KOSPI',
                sector VARCHAR(100) DEFAULT '',
                is_active BOOLEAN DEFAULT TRUE,
                updated_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // 거래 기록 테이블
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS trades (
                id BIGSERIAL PRIMARY KEY,
                stock_code VARCHAR(10) NOT NULL,
                stock_name VARCHAR(50) NOT NULL DEFAULT '',
                side VARCHAR(5) NOT NULL,
                quantity BIGINT NOT NULL DEFAULT 0,
                entry_price BIGINT NOT NULL,
                exit_price BIGINT NOT NULL,
                stop_loss BIGINT NOT NULL DEFAULT 0,
                take_profit BIGINT NOT NULL DEFAULT 0,
                entry_time TIMESTAMP NOT NULL,
                exit_time TIMESTAMP NOT NULL,
                exit_reason VARCHAR(20) NOT NULL,
                pnl_pct DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                strategy VARCHAR(30) NOT NULL DEFAULT 'orb_fvg',
                environment VARCHAR(10) NOT NULL DEFAULT 'paper',
                created_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // 일별 OR 범위 (서버 재시작 시 복구용)
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS daily_or_range (
                stock_code VARCHAR(10) NOT NULL,
                date DATE NOT NULL,
                or_high BIGINT NOT NULL,
                or_low BIGINT NOT NULL,
                source VARCHAR(10) NOT NULL DEFAULT 'ws',
                created_at TIMESTAMPTZ DEFAULT NOW(),
                PRIMARY KEY (stock_code, date)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // 활성 포지션 테이블 (서버 재시작 시 복구용)
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS active_positions (
                stock_code VARCHAR(10) PRIMARY KEY,
                side VARCHAR(5) NOT NULL,
                entry_price BIGINT NOT NULL,
                stop_loss BIGINT NOT NULL,
                take_profit BIGINT NOT NULL,
                quantity BIGINT NOT NULL,
                tp_order_no VARCHAR(20) DEFAULT '',
                tp_krx_orgno VARCHAR(20) DEFAULT '',
                entry_time TIMESTAMP NOT NULL,
                updated_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // 인덱스
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_trades_code_date ON trades (stock_code, entry_time)")
            .execute(&self.pool)
            .await
            .ok();
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_daily_ohlcv_date ON daily_ohlcv (date)")
            .execute(&self.pool)
            .await
            .ok();
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_minute_ohlcv_code_interval ON minute_ohlcv (stock_code, interval_min, datetime)",
        )
        .execute(&self.pool)
        .await
        .ok();
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_index_daily_date ON index_daily (date)")
            .execute(&self.pool)
            .await
            .ok();

        Ok(())
    }

    // ── 일봉 OHLCV ──

    /// 일봉 데이터 일괄 저장 (UPSERT)
    pub async fn save_daily_ohlcv(
        &self,
        stock_code: &str,
        candles: &[Candle],
    ) -> Result<usize, KisError> {
        let mut count = 0;
        for c in candles {
            sqlx::query(
                "INSERT INTO daily_ohlcv (stock_code, date, open, high, low, close, volume)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (stock_code, date) DO UPDATE
                 SET open=$3, high=$4, low=$5, close=$6, volume=$7",
            )
            .bind(stock_code)
            .bind(c.date)
            .bind(c.open)
            .bind(c.high)
            .bind(c.low)
            .bind(c.close)
            .bind(c.volume as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| KisError::Internal(format!("일봉 저장 실패: {e}")))?;
            count += 1;
        }
        Ok(count)
    }

    /// 일봉 데이터 조회
    pub async fn get_daily_ohlcv(
        &self,
        stock_code: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<Candle>, KisError> {
        let rows: Vec<DailyRow> = sqlx::query_as(
            "SELECT date, open, high, low, close, volume FROM daily_ohlcv
             WHERE stock_code = $1 AND date >= $2 AND date <= $3
             ORDER BY date ASC",
        )
        .bind(stock_code)
        .bind(start_date)
        .bind(end_date)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("일봉 조회 실패: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| Candle {
                date: r.date,
                open: r.open,
                high: r.high,
                low: r.low,
                close: r.close,
                volume: r.volume as u64,
            })
            .collect())
    }

    /// 특정 종목의 최신 일봉 날짜
    pub async fn get_latest_daily_date(
        &self,
        stock_code: &str,
    ) -> Result<Option<NaiveDate>, KisError> {
        let row: Option<(NaiveDate,)> = sqlx::query_as(
            "SELECT date FROM daily_ohlcv WHERE stock_code = $1 ORDER BY date DESC LIMIT 1",
        )
        .bind(stock_code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("날짜 조회 실패: {e}")))?;

        Ok(row.map(|(d,)| d))
    }

    /// 일봉 데이터 건수
    pub async fn count_daily_ohlcv(&self, stock_code: &str) -> Result<i64, KisError> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM daily_ohlcv WHERE stock_code = $1")
                .bind(stock_code)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| KisError::Internal(format!("건수 조회 실패: {e}")))?;
        Ok(row.0)
    }

    // ── 분봉 OHLCV ──

    /// 분봉 데이터 일괄 저장 (UPSERT)
    pub async fn save_minute_ohlcv(
        &self,
        stock_code: &str,
        minutes: &[MinuteCandle],
    ) -> Result<usize, KisError> {
        let mut count = 0;
        for m in minutes {
            sqlx::query(
                "INSERT INTO minute_ohlcv (stock_code, datetime, open, high, low, close, volume, interval_min)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT (stock_code, datetime, interval_min) DO UPDATE
                 SET open=$3, high=$4, low=$5, close=$6, volume=$7",
            )
            .bind(stock_code)
            .bind(m.datetime)
            .bind(m.open)
            .bind(m.high)
            .bind(m.low)
            .bind(m.close)
            .bind(m.volume)
            .bind(m.interval_min)
            .execute(&self.pool)
            .await
            .map_err(|e| KisError::Internal(format!("분봉 저장 실패: {e}")))?;
            count += 1;
        }
        Ok(count)
    }

    /// 분봉 데이터 조회
    pub async fn get_minute_ohlcv(
        &self,
        stock_code: &str,
        start: NaiveDateTime,
        end: NaiveDateTime,
        interval_min: i16,
    ) -> Result<Vec<MinuteCandle>, KisError> {
        let rows: Vec<MinuteRow> = sqlx::query_as(
            "SELECT datetime::timestamp as datetime, open, high, low, close, volume, interval_min FROM minute_ohlcv
             WHERE stock_code = $1 AND datetime >= $2 AND datetime <= $3 AND interval_min = $4
             ORDER BY datetime ASC",
        )
        .bind(stock_code)
        .bind(start)
        .bind(end)
        .bind(interval_min)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("분봉 조회 실패: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| MinuteCandle {
                datetime: r.datetime,
                open: r.open,
                high: r.high,
                low: r.low,
                close: r.close,
                volume: r.volume,
                interval_min: r.interval_min,
            })
            .collect())
    }

    /// 특정 종목의 최신 분봉 날짜
    pub async fn get_latest_minute_date(
        &self,
        stock_code: &str,
    ) -> Result<Option<NaiveDate>, KisError> {
        let row: Option<(NaiveDateTime,)> = sqlx::query_as(
            "SELECT datetime::timestamp as datetime FROM minute_ohlcv WHERE stock_code = $1 ORDER BY datetime DESC LIMIT 1",
        )
        .bind(stock_code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("날짜 조회 실패: {e}")))?;

        Ok(row.map(|(dt,)| dt.date()))
    }

    // ── 지수 일봉 ──

    /// 지수 일봉 저장 (UPSERT)
    pub async fn save_index_daily(
        &self,
        index_code: &str,
        data: &[IndexCandle],
    ) -> Result<usize, KisError> {
        let mut count = 0;
        for d in data {
            sqlx::query(
                "INSERT INTO index_daily (index_code, date, open, high, low, close, volume)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (index_code, date) DO UPDATE
                 SET open=$3, high=$4, low=$5, close=$6, volume=$7",
            )
            .bind(index_code)
            .bind(d.date)
            .bind(d.open)
            .bind(d.high)
            .bind(d.low)
            .bind(d.close)
            .bind(d.volume)
            .execute(&self.pool)
            .await
            .map_err(|e| KisError::Internal(format!("지수 저장 실패: {e}")))?;
            count += 1;
        }
        Ok(count)
    }

    /// 지수 일봉 조회
    pub async fn get_index_daily(
        &self,
        index_code: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<IndexCandle>, KisError> {
        let rows: Vec<IndexRow> = sqlx::query_as(
            "SELECT date, open, high, low, close, volume FROM index_daily
             WHERE index_code = $1 AND date >= $2 AND date <= $3
             ORDER BY date ASC",
        )
        .bind(index_code)
        .bind(start_date)
        .bind(end_date)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("지수 조회 실패: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| IndexCandle {
                date: r.date,
                open: r.open,
                high: r.high,
                low: r.low,
                close: r.close,
                volume: r.volume,
            })
            .collect())
    }

    // ── 거래 기록 ──

    /// 거래 결과 저장
    pub async fn save_trade(&self, trade: &TradeRecord) -> Result<(), KisError> {
        sqlx::query(
            "INSERT INTO trades (stock_code, stock_name, side, quantity, entry_price, exit_price,
             stop_loss, take_profit, entry_time, exit_time, exit_reason, pnl_pct, strategy, environment)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(&trade.stock_code)
        .bind(&trade.stock_name)
        .bind(&trade.side)
        .bind(trade.quantity)
        .bind(trade.entry_price)
        .bind(trade.exit_price)
        .bind(trade.stop_loss)
        .bind(trade.take_profit)
        .bind(trade.entry_time)
        .bind(trade.exit_time)
        .bind(&trade.exit_reason)
        .bind(trade.pnl_pct)
        .bind(&trade.strategy)
        .bind(&trade.environment)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("거래 저장 실패: {e}")))?;
        Ok(())
    }

    // ── 일별 OR 범위 ──

    /// OR 범위 저장
    pub async fn save_or_range(&self, stock_code: &str, date: NaiveDate, or_high: i64, or_low: i64, source: &str) -> Result<(), KisError> {
        sqlx::query(
            "INSERT INTO daily_or_range (stock_code, date, or_high, or_low, source)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (stock_code, date) DO UPDATE SET or_high=$3, or_low=$4, source=$5",
        )
        .bind(stock_code)
        .bind(date)
        .bind(or_high)
        .bind(or_low)
        .bind(source)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("OR 저장 실패: {e}")))?;
        Ok(())
    }

    /// OR 범위 조회
    pub async fn get_or_range(&self, stock_code: &str, date: NaiveDate) -> Result<Option<(i64, i64)>, KisError> {
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT or_high, or_low FROM daily_or_range WHERE stock_code = $1 AND date = $2",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("OR 조회 실패: {e}")))?;
        Ok(row)
    }

    // ── 활성 포지션 (서버 재시작 복구용) ──

    /// 활성 포지션 저장/갱신
    pub async fn save_active_position(&self, pos: &ActivePosition) -> Result<(), KisError> {
        sqlx::query(
            "INSERT INTO active_positions (stock_code, side, entry_price, stop_loss, take_profit, quantity, tp_order_no, tp_krx_orgno, entry_time)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (stock_code) DO UPDATE
             SET side=$2, entry_price=$3, stop_loss=$4, take_profit=$5, quantity=$6, tp_order_no=$7, tp_krx_orgno=$8, entry_time=$9, updated_at=NOW()",
        )
        .bind(&pos.stock_code)
        .bind(&pos.side)
        .bind(pos.entry_price)
        .bind(pos.stop_loss)
        .bind(pos.take_profit)
        .bind(pos.quantity)
        .bind(&pos.tp_order_no)
        .bind(&pos.tp_krx_orgno)
        .bind(pos.entry_time)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("활성 포지션 저장 실패: {e}")))?;
        Ok(())
    }

    /// 활성 포지션 조회
    pub async fn get_active_position(&self, stock_code: &str) -> Result<Option<ActivePosition>, KisError> {
        let row: Option<ActivePositionRow> = sqlx::query_as(
            "SELECT stock_code, side, entry_price, stop_loss, take_profit, quantity, tp_order_no, tp_krx_orgno, entry_time
             FROM active_positions WHERE stock_code = $1",
        )
        .bind(stock_code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("활성 포지션 조회 실패: {e}")))?;

        Ok(row.map(|r| ActivePosition {
            stock_code: r.stock_code,
            side: r.side,
            entry_price: r.entry_price,
            stop_loss: r.stop_loss,
            take_profit: r.take_profit,
            quantity: r.quantity,
            tp_order_no: r.tp_order_no,
            tp_krx_orgno: r.tp_krx_orgno,
            entry_time: r.entry_time,
        }))
    }

    /// 활성 포지션 삭제 (청산 완료 시)
    pub async fn delete_active_position(&self, stock_code: &str) -> Result<(), KisError> {
        sqlx::query("DELETE FROM active_positions WHERE stock_code = $1")
            .bind(stock_code)
            .execute(&self.pool)
            .await
            .map_err(|e| KisError::Internal(format!("활성 포지션 삭제 실패: {e}")))?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ── 데이터 구조체 ──

/// 활성 포지션 (서버 재시작 복구용)
#[derive(Debug, Clone)]
pub struct ActivePosition {
    pub stock_code: String,
    pub side: String,
    pub entry_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    pub quantity: i64,
    pub tp_order_no: String,
    pub tp_krx_orgno: String,
    pub entry_time: NaiveDateTime,
}

#[derive(sqlx::FromRow)]
struct ActivePositionRow {
    stock_code: String,
    side: String,
    entry_price: i64,
    stop_loss: i64,
    take_profit: i64,
    quantity: i64,
    tp_order_no: String,
    tp_krx_orgno: String,
    entry_time: NaiveDateTime,
}

/// 거래 기록
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub stock_code: String,
    pub stock_name: String,
    pub side: String,
    pub quantity: i64,
    pub entry_price: i64,
    pub exit_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    pub entry_time: NaiveDateTime,
    pub exit_time: NaiveDateTime,
    pub exit_reason: String,
    pub pnl_pct: f64,
    pub strategy: String,
    pub environment: String,
}

/// 분봉 캔들
#[derive(Debug, Clone)]
pub struct MinuteCandle {
    pub datetime: NaiveDateTime,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
    pub interval_min: i16,
}

/// 지수 캔들
#[derive(Debug, Clone)]
pub struct IndexCandle {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
}

// ── sqlx 매핑 ──

#[derive(sqlx::FromRow)]
struct DailyRow {
    date: NaiveDate,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: i64,
}

#[derive(sqlx::FromRow)]
struct MinuteRow {
    datetime: NaiveDateTime,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: i64,
    interval_min: i16,
}

#[derive(sqlx::FromRow)]
struct IndexRow {
    date: NaiveDate,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: i64,
}
