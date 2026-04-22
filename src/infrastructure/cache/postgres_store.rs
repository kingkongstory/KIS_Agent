use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use tracing::info;

use crate::domain::error::KisError;
use crate::domain::models::candle::Candle;

/// PostgreSQL 데이터 저장소
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// 기본 설정으로 연결. 2026-04-16 production-readiness 계획서에 따라 acquire_timeout
    /// 과 max_connections 를 명시한다. 실전에서 DB 이상 상황이 장시간 대기로 이어지는
    /// 것을 막고, 풀 고갈로 러너가 뭉치는 것을 방지한다.
    pub async fn new(database_url: &str) -> Result<Self, KisError> {
        Self::with_pool_options(database_url, 10, Duration::from_secs(5)).await
    }

    /// 풀 파라미터를 지정해서 연결. 테스트나 운영 튜닝에서 호출.
    pub async fn with_pool_options(
        database_url: &str,
        max_connections: u32,
        acquire_timeout: Duration,
    ) -> Result<Self, KisError> {
        // 모든 새 연결에 KST 세션 timezone 강제.
        // sqlx가 NaiveDateTime을 timestamp(naive)로 인코딩 → timestamptz와 비교 시 세션 TZ 사용.
        // 한국 시각 데이터를 다루므로 명시적으로 Asia/Seoul로 고정해야 함.
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .test_before_acquire(true)
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("SET TIME ZONE 'Asia/Seoul'")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await
            .map_err(|e| KisError::Internal(format!("PostgreSQL 연결 실패: {e}")))?;

        let store = Self { pool };
        store.migrate().await?;
        info!(
            "PostgreSQL 연결 완료 (max_connections={}, acquire_timeout={:?})",
            max_connections, acquire_timeout
        );
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
                intended_entry_price BIGINT DEFAULT 0,
                entry_slippage BIGINT DEFAULT 0,
                exit_slippage BIGINT DEFAULT 0,
                order_to_fill_ms BIGINT DEFAULT 0,
                created_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;
        // trades 스키마 마이그레이션: 기존 테이블에 슬리피지 컬럼 추가
        // 2026-04-18 Phase 1 (execution-timing-implementation-plan):
        // 청산 체결가 미확인(fill_price_unknown) + 체결 확인 경로(exit_resolution_source)
        // 기록. "현재가 fallback 으로 flat 처리" 시 사후 분석이 가능하도록 한다.
        for col_sql in [
            "ALTER TABLE trades ADD COLUMN IF NOT EXISTS intended_entry_price BIGINT DEFAULT 0",
            "ALTER TABLE trades ADD COLUMN IF NOT EXISTS entry_slippage BIGINT DEFAULT 0",
            "ALTER TABLE trades ADD COLUMN IF NOT EXISTS exit_slippage BIGINT DEFAULT 0",
            "ALTER TABLE trades ADD COLUMN IF NOT EXISTS order_to_fill_ms BIGINT DEFAULT 0",
            "ALTER TABLE trades ADD COLUMN IF NOT EXISTS fill_price_unknown BOOLEAN NOT NULL DEFAULT FALSE",
            "ALTER TABLE trades ADD COLUMN IF NOT EXISTS exit_resolution_source VARCHAR(20) NOT NULL DEFAULT ''",
        ] {
            let _ = sqlx::query(col_sql).execute(&self.pool).await;
        }

        // 일별 OR 범위 (서버 재시작 시 복구용)
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS daily_or_range (
                stock_code VARCHAR(10) NOT NULL,
                date DATE NOT NULL,
                or_high BIGINT NOT NULL,
                or_low BIGINT NOT NULL,
                source VARCHAR(10) NOT NULL DEFAULT 'ws',
                or_stage VARCHAR(5) NOT NULL DEFAULT '15m',
                created_at TIMESTAMPTZ DEFAULT NOW(),
                PRIMARY KEY (stock_code, date, or_stage)
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
                original_sl BIGINT DEFAULT 0,
                reached_1r BOOLEAN DEFAULT FALSE,
                 best_price BIGINT DEFAULT 0,
                 or_high BIGINT,
                 or_low BIGINT,
                 or_stage VARCHAR(5),
                 or_source VARCHAR(10),
                 trigger_price BIGINT,
                 original_risk BIGINT,
                 signal_id VARCHAR(80),
                 entry_mode VARCHAR(20),
                 pending_entry_order_no VARCHAR(20) NOT NULL DEFAULT '',
                 pending_entry_krx_orgno VARCHAR(20) NOT NULL DEFAULT '',
                 updated_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // active_positions 스키마 마이그레이션: 기존 테이블에 새 컬럼 추가
        for col_sql in [
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS reached_1r BOOLEAN DEFAULT FALSE",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS best_price BIGINT DEFAULT 0",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS or_high BIGINT",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS or_low BIGINT",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS or_stage VARCHAR(5)",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS or_source VARCHAR(10)",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS trigger_price BIGINT",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS original_risk BIGINT",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS signal_id VARCHAR(80)",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS entry_mode VARCHAR(20)",
            // 2026-04-17 긴급 follow-up: manual_intervention 영속화.
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS manual_intervention_required BOOLEAN NOT NULL DEFAULT FALSE",
            // 2026-04-19 Doc revision 3-1: 재시작 복구 범위 확장.
            // execution_state 를 영속화하여 서버 재시작 시 `Open` 뿐 아니라 `ExitPending` /
            // `ExitPartial` / `EntryPending` 도 복구 또는 manual_intervention 으로 승격 가능.
            // 기본값 'open' 은 기존 데이터 호환 (Open 상태만 저장되던 시절).
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS execution_state VARCHAR(32) NOT NULL DEFAULT 'open'",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS last_exit_order_no VARCHAR(20) NOT NULL DEFAULT ''",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS last_exit_reason VARCHAR(20) NOT NULL DEFAULT ''",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS pending_entry_order_no VARCHAR(20) NOT NULL DEFAULT ''",
            "ALTER TABLE active_positions ADD COLUMN IF NOT EXISTS pending_entry_krx_orgno VARCHAR(20) NOT NULL DEFAULT ''",
        ] {
            let _ = sqlx::query(col_sql).execute(&self.pool).await;
        }

        // 운영 이벤트 로그 (모니터링 — 전략/주문/포지션/시스템 이벤트 영속화)
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS event_log (
                id BIGSERIAL PRIMARY KEY,
                event_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                stock_code VARCHAR(10) NOT NULL DEFAULT '',
                category VARCHAR(20) NOT NULL,
                event_type VARCHAR(50) NOT NULL,
                severity VARCHAR(10) NOT NULL DEFAULT 'info',
                message TEXT NOT NULL DEFAULT '',
                metadata JSONB DEFAULT '{}'
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;
        // event_log 인덱스
        for idx_sql in [
            "CREATE INDEX IF NOT EXISTS idx_event_log_time ON event_log (event_time DESC)",
            "CREATE INDEX IF NOT EXISTS idx_event_log_stock ON event_log (stock_code, event_time DESC)",
        ] {
            let _ = sqlx::query(idx_sql).execute(&self.pool).await;
        }

        // 주문 이벤트 로그 (모든 주문 시도를 비동기 기록)
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS order_log (
                id BIGSERIAL PRIMARY KEY,
                stock_code VARCHAR(10) NOT NULL,
                order_type VARCHAR(20) NOT NULL,
                side VARCHAR(5) NOT NULL,
                quantity BIGINT NOT NULL,
                price BIGINT DEFAULT 0,
                order_no VARCHAR(20) DEFAULT '',
                status VARCHAR(20) NOT NULL,
                message TEXT DEFAULT '',
                created_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // 2026-04-18 Review Finding #5: 주문 라이프사이클 영속화 테이블.
        //
        // 기획서 (docs/monitoring/2026-04-18-execution-timing-implementation-plan.md:125, :560)
        // 의 `execution_journal` 요구사항 반영. order_log 와 달리 phase 별 스냅샷 + 가격/수량/
        // 잔고 상세를 함께 기록해 재시작 복구와 사후 분석에서 "왜 ExitPending 이 깨졌는지" 를
        // 추적할 수 있도록 한다.
        //
        // append-only 로 설계 — 각 phase 가 독립 row. UPSERT 없이 insert 만 수행.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS execution_journal (
                id BIGSERIAL PRIMARY KEY,
                stock_code VARCHAR(10) NOT NULL,
                execution_id VARCHAR(40) NOT NULL DEFAULT '',
                signal_id VARCHAR(40) NOT NULL DEFAULT '',
                broker_order_id VARCHAR(20) NOT NULL DEFAULT '',
                phase VARCHAR(32) NOT NULL,
                side VARCHAR(5) NOT NULL DEFAULT '',
                intended_price BIGINT,
                submitted_price BIGINT,
                filled_price BIGINT,
                filled_qty BIGINT,
                holdings_before BIGINT,
                holdings_after BIGINT,
                resolution_source VARCHAR(20) NOT NULL DEFAULT '',
                reason VARCHAR(50) NOT NULL DEFAULT '',
                error_message TEXT NOT NULL DEFAULT '',
                metadata JSONB NOT NULL DEFAULT '{}',
                sent_at TIMESTAMPTZ,
                ack_at TIMESTAMPTZ,
                first_fill_at TIMESTAMPTZ,
                final_fill_at TIMESTAMPTZ,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        for col_sql in [
            "ALTER TABLE execution_journal ADD COLUMN IF NOT EXISTS sent_at TIMESTAMPTZ",
            "ALTER TABLE execution_journal ADD COLUMN IF NOT EXISTS ack_at TIMESTAMPTZ",
            "ALTER TABLE execution_journal ADD COLUMN IF NOT EXISTS first_fill_at TIMESTAMPTZ",
            "ALTER TABLE execution_journal ADD COLUMN IF NOT EXISTS final_fill_at TIMESTAMPTZ",
        ] {
            let _ = sqlx::query(col_sql).execute(&self.pool).await;
        }

        for idx_sql in [
            "CREATE INDEX IF NOT EXISTS idx_exec_journal_time ON execution_journal (created_at DESC)",
            "CREATE INDEX IF NOT EXISTS idx_exec_journal_stock ON execution_journal (stock_code, created_at DESC)",
            "CREATE INDEX IF NOT EXISTS idx_exec_journal_phase ON execution_journal (phase, created_at DESC)",
            "CREATE INDEX IF NOT EXISTS idx_exec_journal_order ON execution_journal (broker_order_id) WHERE broker_order_id <> ''",
        ] {
            let _ = sqlx::query(idx_sql).execute(&self.pool).await;
        }

        // 일일 결산 리포트
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS daily_report (
                id BIGSERIAL PRIMARY KEY,
                date DATE NOT NULL,
                stock_code VARCHAR(10) NOT NULL DEFAULT '',
                total_trades INT DEFAULT 0,
                wins INT DEFAULT 0,
                losses INT DEFAULT 0,
                win_rate DOUBLE PRECISION DEFAULT 0.0,
                total_pnl_pct DOUBLE PRECISION DEFAULT 0.0,
                max_loss_pnl_pct DOUBLE PRECISION DEFAULT 0.0,
                avg_entry_slippage BIGINT DEFAULT 0,
                ws_reconnect_count INT DEFAULT 0,
                api_error_count INT DEFAULT 0,
                metadata JSONB DEFAULT '{}',
                created_at TIMESTAMPTZ DEFAULT NOW(),
                UNIQUE(date, stock_code)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("마이그레이션 실패: {e}")))?;

        // 인덱스
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_trades_code_date ON trades (stock_code, entry_time)",
        )
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
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM daily_ohlcv WHERE stock_code = $1")
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
             stop_loss, take_profit, entry_time, exit_time, exit_reason, pnl_pct, strategy, environment,
             intended_entry_price, entry_slippage, exit_slippage, order_to_fill_ms,
             fill_price_unknown, exit_resolution_source)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
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
        .bind(trade.intended_entry_price)
        .bind(trade.entry_slippage)
        .bind(trade.exit_slippage)
        .bind(trade.order_to_fill_ms)
        .bind(trade.fill_price_unknown)
        .bind(&trade.exit_resolution_source)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("거래 저장 실패: {e}")))?;
        Ok(())
    }

    // ── 거래 기록: 재시작 복구용 헬퍼 ──
    //
    // 라이브 운영 중 핫픽스로 재시작 시 LiveRunner의 trade_count / confirmed_side /
    // signal_state.search_after 가 0/None 으로 reset 되는 회귀를 막기 위한 헬퍼.
    // 백테스트 BacktestEngine 의 search_from / cumulative state 와 동일한 메커니즘
    // 을 라이브에서 영속화하여, 같은 5분봉의 같은 FVG 가 청산 후 재진입되는 사고
    // (2026-04-10 trade #54~58) 의 재발을 차단한다.

    /// 오늘 마지막 거래의 청산 시각 (NaiveTime). 없으면 None.
    /// LiveSignalState.search_after 복구용.
    pub async fn get_last_trade_exit_today(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<Option<NaiveTime>, KisError> {
        let row: Option<(NaiveDateTime,)> = sqlx::query_as(
            "SELECT MAX(exit_time) FROM trades
             WHERE stock_code = $1 AND exit_time::date = $2",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("오늘 마지막 청산 조회 실패: {e}")))?;

        Ok(row.map(|(dt,)| dt.time()))
    }

    /// 오늘 거래 건수. 재시작 후 trade_count 복구용.
    pub async fn count_trades_today(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<i64, KisError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM trades
             WHERE stock_code = $1 AND entry_time::date = $2",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("오늘 거래 건수 조회 실패: {e}")))?;
        Ok(row.0)
    }

    /// 오늘 마지막 거래의 (side, pnl_pct). confirmed_side 복구용.
    /// 수익이면 같은 방향만 다음 진입 허용 (백테스트 OrbFvgStrategy::run_day 와 동일).
    pub async fn get_last_trade_side_pnl_today(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<Option<(String, f64)>, KisError> {
        let row: Option<(String, f64)> = sqlx::query_as(
            "SELECT side, pnl_pct FROM trades
             WHERE stock_code = $1 AND entry_time::date = $2
             ORDER BY entry_time DESC LIMIT 1",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("오늘 마지막 거래 조회 실패: {e}")))?;
        Ok(row)
    }

    // ── 일별 OR 범위 ──

    /// OR 범위 저장 (단계별)
    pub async fn save_or_range(
        &self,
        stock_code: &str,
        date: NaiveDate,
        or_high: i64,
        or_low: i64,
        source: &str,
    ) -> Result<(), KisError> {
        self.save_or_range_stage(stock_code, date, or_high, or_low, source, "15m")
            .await
    }

    /// OR 범위 저장 (단계 지정)
    pub async fn save_or_range_stage(
        &self,
        stock_code: &str,
        date: NaiveDate,
        or_high: i64,
        or_low: i64,
        source: &str,
        stage: &str,
    ) -> Result<(), KisError> {
        sqlx::query(
            "INSERT INTO daily_or_range (stock_code, date, or_high, or_low, source, or_stage)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (stock_code, date, or_stage) DO UPDATE SET or_high=$3, or_low=$4, source=$5",
        )
        .bind(stock_code)
        .bind(date)
        .bind(or_high)
        .bind(or_low)
        .bind(source)
        .bind(stage)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("OR 저장 실패: {e}")))?;
        Ok(())
    }

    /// OR 범위 조회 (기본 15m)
    pub async fn get_or_range(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<Option<(i64, i64)>, KisError> {
        self.get_or_range_stage(stock_code, date, "15m").await
    }

    /// OR 범위 조회 (단계 지정)
    pub async fn get_or_range_stage(
        &self,
        stock_code: &str,
        date: NaiveDate,
        stage: &str,
    ) -> Result<Option<(i64, i64)>, KisError> {
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT or_high, or_low FROM daily_or_range WHERE stock_code = $1 AND date = $2 AND or_stage = $3",
        )
        .bind(stock_code)
        .bind(date)
        .bind(stage)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("OR 조회 실패: {e}")))?;
        Ok(row)
    }

    /// 모든 OR 단계 조회
    pub async fn get_all_or_stages(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<Vec<(String, i64, i64)>, KisError> {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT or_stage, or_high, or_low FROM daily_or_range WHERE stock_code = $1 AND date = $2 ORDER BY or_stage",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("OR 전체 조회 실패: {e}")))?;
        Ok(rows)
    }

    // ── 활성 포지션 (서버 재시작 복구용) ──

    /// 활성 포지션 저장/갱신
    ///
    /// 2026-04-17 긴급 follow-up: manual_intervention_required 컬럼 포함.
    /// ON CONFLICT 에서는 manual 플래그를 덮어쓰지 않는다 — 운영자가 수동으로
    /// 해제한 상태를 entry 경로 저장이 되돌리지 못하도록 보호. manual 플래그는
    /// 전용 `set_active_position_manual` 메서드로만 변경.
    pub async fn save_active_position(&self, pos: &ActivePosition) -> Result<(), KisError> {
        // 2026-04-19 Doc revision 3-1: execution_state / last_exit_order_no / last_exit_reason 포함.
        // ON CONFLICT 에서는 manual_intervention_required 를 덮어쓰지 않고, 나머지는 최신화.
        sqlx::query(
            "INSERT INTO active_positions (stock_code, side, entry_price, stop_loss, take_profit, quantity, tp_order_no, tp_krx_orgno, entry_time, original_sl, reached_1r, best_price, or_high, or_low, or_stage, or_source, trigger_price, original_risk, signal_id, entry_mode, manual_intervention_required, execution_state, last_exit_order_no, last_exit_reason, pending_entry_order_no, pending_entry_krx_orgno)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26)
             ON CONFLICT (stock_code) DO UPDATE
             SET side=$2, entry_price=$3, stop_loss=$4, take_profit=$5, quantity=$6, tp_order_no=$7, tp_krx_orgno=$8, entry_time=$9, original_sl=$10, reached_1r=$11, best_price=$12, or_high=$13, or_low=$14, or_stage=$15, or_source=$16, trigger_price=$17, original_risk=$18, signal_id=$19, entry_mode=$20, execution_state=$22, last_exit_order_no=$23, last_exit_reason=$24, pending_entry_order_no=$25, pending_entry_krx_orgno=$26, updated_at=NOW()",
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
        .bind(pos.original_sl)
        .bind(pos.reached_1r)
        .bind(pos.best_price)
        .bind(pos.or_high)
        .bind(pos.or_low)
        .bind(&pos.or_stage)
        .bind(&pos.or_source)
        .bind(pos.trigger_price)
        .bind(pos.original_risk)
        .bind(&pos.signal_id)
        .bind(&pos.entry_mode)
        .bind(pos.manual_intervention_required)
        .bind(&pos.execution_state)
        .bind(&pos.last_exit_order_no)
        .bind(&pos.last_exit_reason)
        .bind(&pos.pending_entry_order_no)
        .bind(&pos.pending_entry_krx_orgno)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("활성 포지션 저장 실패: {e}")))?;
        Ok(())
    }

    /// 2026-04-19 Doc revision 3-1: ExitPending 진입 시점에 DB 상태 마킹.
    ///
    /// `close_position_market` 이 시장가 주문을 성공적으로 제출한 직후 호출해야 한다.
    /// 재시작 복구 시 이 값을 보고 `verify_exit_completion` 재시도 또는 manual_intervention
    /// 분기를 결정.
    pub async fn mark_active_position_exit_pending(
        &self,
        stock_code: &str,
        exit_order_no: &str,
        exit_reason: &str,
    ) -> Result<u64, KisError> {
        let result = sqlx::query(
            "UPDATE active_positions
             SET execution_state='exit_pending', last_exit_order_no=$2, last_exit_reason=$3,
                 pending_entry_order_no='', pending_entry_krx_orgno='', updated_at=NOW()
             WHERE stock_code = $1",
        )
        .bind(stock_code)
        .bind(exit_order_no)
        .bind(exit_reason)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("exit_pending 마킹 실패: {e}")))?;
        Ok(result.rows_affected())
    }

    /// 청산 제출 실패 후 broker-side TP 주문 메타를 비운다.
    ///
    /// 시장가 청산 직전 기존 TP 지정가를 취소한 뒤 주문 제출이 실패하면, 메모리뿐 아니라
    /// 재시작 복구용 DB 메타도 함께 정리되어야 한다. 그렇지 않으면 재시작 시 이미 없는
    /// TP 주문번호를 다시 취소하려고 시도하며 운영자가 상황을 오판할 수 있다.
    pub async fn clear_active_position_tp_metadata(
        &self,
        stock_code: &str,
    ) -> Result<u64, KisError> {
        let result = sqlx::query(
            "UPDATE active_positions
             SET tp_order_no='', tp_krx_orgno='', updated_at=NOW()
             WHERE stock_code = $1",
        )
        .bind(stock_code)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("TP 메타 정리 실패: {e}")))?;
        Ok(result.rows_affected())
    }

    /// 2026-04-17 긴급 follow-up: `manual_intervention_required` 플래그만 갱신.
    ///
    /// reconcile mismatch / trigger_manual_intervention 등에서 호출. 재시작 후
    /// `check_and_restore_position` 이 이 값을 읽어 state / stop_flag / position_lock
    /// 을 복원하므로, watchdog 재시작이 manual 보호를 우회하는 회귀 (2026-04-17
    /// 11:31 사고) 를 차단한다.
    pub async fn set_active_position_manual(
        &self,
        stock_code: &str,
        manual: bool,
    ) -> Result<u64, KisError> {
        let result = sqlx::query(
            "UPDATE active_positions
             SET manual_intervention_required=$2,
                 execution_state = CASE WHEN $2 THEN 'manual_intervention' ELSE execution_state END,
                 updated_at=NOW()
             WHERE stock_code = $1",
        )
        .bind(stock_code)
        .bind(manual)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("manual 플래그 갱신 실패: {e}")))?;
        Ok(result.rows_affected())
    }

    /// 활성 포지션 조회
    pub async fn get_active_position(
        &self,
        stock_code: &str,
    ) -> Result<Option<ActivePosition>, KisError> {
        let row: Option<ActivePositionRow> = sqlx::query_as(
            "SELECT stock_code, side, entry_price, stop_loss, take_profit, quantity, tp_order_no, tp_krx_orgno, entry_time, original_sl, reached_1r, best_price, or_high, or_low, or_stage, or_source, trigger_price, original_risk, signal_id, entry_mode, manual_intervention_required, execution_state, last_exit_order_no, last_exit_reason, pending_entry_order_no, pending_entry_krx_orgno
             FROM active_positions WHERE stock_code = $1",
        )
        .bind(stock_code)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("활성 포지션 조회 실패: {e}")))?;

        Ok(row.map(|r| {
            // original_sl이 0이면 (기존 데이터) stop_loss를 사용
            let orig_sl = r.original_sl.unwrap_or(0);
            ActivePosition {
                stock_code: r.stock_code,
                side: r.side,
                entry_price: r.entry_price,
                stop_loss: r.stop_loss,
                take_profit: r.take_profit,
                quantity: r.quantity,
                tp_order_no: r.tp_order_no,
                tp_krx_orgno: r.tp_krx_orgno,
                entry_time: r.entry_time,
                original_sl: if orig_sl != 0 { orig_sl } else { r.stop_loss },
                reached_1r: r.reached_1r.unwrap_or(false),
                best_price: r.best_price.unwrap_or(r.entry_price),
                or_high: r.or_high,
                or_low: r.or_low,
                or_stage: r.or_stage,
                or_source: r.or_source,
                trigger_price: r.trigger_price,
                original_risk: r.original_risk,
                signal_id: r.signal_id,
                entry_mode: r.entry_mode,
                manual_intervention_required: r.manual_intervention_required.unwrap_or(false),
                execution_state: r.execution_state.unwrap_or_else(|| "open".to_string()),
                last_exit_order_no: r.last_exit_order_no.unwrap_or_default(),
                last_exit_reason: r.last_exit_reason.unwrap_or_default(),
                pending_entry_order_no: r.pending_entry_order_no.unwrap_or_default(),
                pending_entry_krx_orgno: r.pending_entry_krx_orgno.unwrap_or_default(),
            }
        }))
    }

    /// 활성 포지션의 트레일링 상태만 갱신 (경량 업데이트).
    ///
    /// 2026-04-17 v3 fixups E: `Result<u64, KisError>` 반환 — 영향받은 행 수.
    /// `Ok(0)` 이면 SQL 자체는 성공했지만 해당 stock_code 의 행이 없는 상태.
    /// 호출자가 "행 부재" vs "정상 갱신" 을 구분해 critical 처리할 수 있도록 한다.
    /// 평소 fire-and-forget 호출(`live_runner.rs:2154`)은 `let _ = ...` 라 영향 없음.
    pub async fn update_position_trailing(
        &self,
        stock_code: &str,
        stop_loss: i64,
        reached_1r: bool,
        best_price: i64,
    ) -> Result<u64, KisError> {
        let result = sqlx::query(
            "UPDATE active_positions SET stop_loss=$2, reached_1r=$3, best_price=$4, updated_at=NOW()
             WHERE stock_code = $1",
        )
        .bind(stock_code)
        .bind(stop_loss)
        .bind(reached_1r)
        .bind(best_price)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("트레일링 상태 갱신 실패: {e}")))?;
        Ok(result.rows_affected())
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

    /// 주문 이벤트 로그 (비동기, 에러 무시)
    pub async fn save_order_log(
        &self,
        stock_code: &str,
        order_type: &str,
        side: &str,
        quantity: i64,
        price: i64,
        order_no: &str,
        status: &str,
        message: &str,
    ) {
        let _ = sqlx::query(
            "INSERT INTO order_log (stock_code, order_type, side, quantity, price, order_no, status, message)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(stock_code)
        .bind(order_type)
        .bind(side)
        .bind(quantity)
        .bind(price)
        .bind(order_no)
        .bind(status)
        .bind(message)
        .execute(&self.pool)
        .await;
    }

    /// 2026-04-18 Review Finding #5: 주문 라이프사이클 한 phase 기록 (append-only).
    ///
    /// 실패해도 panic 하지 않는다 — 운영 중 단 한 건의 누락보다 거래 흐름 자체를
    /// 끊지 않는 것이 우선. 실패 시 error log 로 남기고 Err 반환.
    pub async fn append_execution_journal(
        &self,
        entry: &ExecutionJournalEntry,
    ) -> Result<(), KisError> {
        sqlx::query(
            "INSERT INTO execution_journal (
                stock_code, execution_id, signal_id, broker_order_id, phase, side,
                intended_price, submitted_price, filled_price, filled_qty,
                holdings_before, holdings_after, resolution_source, reason, error_message, metadata,
                sent_at, ack_at, first_fill_at, final_fill_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
        )
        .bind(&entry.stock_code)
        .bind(&entry.execution_id)
        .bind(&entry.signal_id)
        .bind(&entry.broker_order_id)
        .bind(&entry.phase)
        .bind(&entry.side)
        .bind(entry.intended_price)
        .bind(entry.submitted_price)
        .bind(entry.filled_price)
        .bind(entry.filled_qty)
        .bind(entry.holdings_before)
        .bind(entry.holdings_after)
        .bind(&entry.resolution_source)
        .bind(&entry.reason)
        .bind(&entry.error_message)
        .bind(&entry.metadata)
        .bind(entry.sent_at)
        .bind(entry.ack_at)
        .bind(entry.first_fill_at)
        .bind(entry.final_fill_at)
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("execution_journal 저장 실패: {e}")))?;
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
    /// 원래 SL (트레일링 전 기준, risk 계산용)
    pub original_sl: i64,
    /// 1R 도달 여부 (트레일링/본전스탑 활성화 상태 복구용)
    pub reached_1r: bool,
    /// 유리한 방향 최고가 (트레일링 SL 계산 기준 복구용)
    pub best_price: i64,
    /// 진입 시점 OR 고가 (재시작 복구 시 전략 맥락 재구성용)
    pub or_high: Option<i64>,
    /// 진입 시점 OR 저가
    pub or_low: Option<i64>,
    /// 실제 진입을 발생시킨 OR stage ("5m"/"15m"/"30m")
    pub or_stage: Option<String>,
    /// OR 데이터 출처 ("ws"/"yahoo"/"naver")
    pub or_source: Option<String>,
    /// 지정가 발주 기준 가격 (FVG top/bottom 등, 슬리피지 계산 기준)
    pub trigger_price: Option<i64>,
    /// 원래 리스크 |entry - stop_loss| (임의 0.3% 재생성을 대체하는 복구 근거)
    pub original_risk: Option<i64>,
    /// 진입 신호 식별자. execution_journal 상관관계용.
    pub signal_id: Option<String>,
    /// 진입 방식 ("limit_passive", "market_limit" 등). 이후 ExecutionPolicy 확장 대비.
    pub entry_mode: Option<String>,
    /// 2026-04-17 긴급 follow-up: manual_intervention 영속화.
    /// reconcile mismatch / orphan cleanup 등으로 manual 전환된 종목을 watchdog
    /// 재시작 후에도 자동 청산이 다시 일어나지 않도록 DB 에 보존한다.
    /// `check_and_restore_position` 이 이 값을 읽어 state/lock/stop_flag 를 복원.
    pub manual_intervention_required: bool,
    /// 2026-04-19 Doc revision 3-1: 실행 상태머신 영속화.
    /// `ExecutionState::label()` 과 동일한 문자열 (예: "open", "exit_pending", "exit_partial",
    /// "manual_intervention"). 재시작 시 `check_and_restore_position` 이 이 값에 따라 복구
    /// 경로를 분기한다.
    pub execution_state: String,
    /// 2026-04-19 Doc revision 3-1: 마지막 청산 주문번호 (ExitPending 복구용).
    /// `close_position_market` 이 시장가 주문 제출 직후 DB 에 기록해야 재시작 후
    /// `verify_exit_completion` 재시도 또는 주문 상태 조회가 가능하다.
    /// 빈 문자열이면 "해당 시점 청산 진행 중 아님".
    pub last_exit_order_no: String,
    /// 2026-04-19 Doc revision 3-1: 마지막 청산 사유 (ExitReason 라벨).
    pub last_exit_reason: String,
    /// 2026-04-19 후속 보강: 진입 주문 제출 직후 재시작 복구를 위한 pending 주문번호.
    pub pending_entry_order_no: String,
    /// 2026-04-19 후속 보강: 진입 주문 취소/조회용 KRX 조직번호.
    pub pending_entry_krx_orgno: String,
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
    #[sqlx(default)]
    original_sl: Option<i64>,
    #[sqlx(default)]
    reached_1r: Option<bool>,
    #[sqlx(default)]
    best_price: Option<i64>,
    #[sqlx(default)]
    or_high: Option<i64>,
    #[sqlx(default)]
    or_low: Option<i64>,
    #[sqlx(default)]
    or_stage: Option<String>,
    #[sqlx(default)]
    or_source: Option<String>,
    #[sqlx(default)]
    trigger_price: Option<i64>,
    #[sqlx(default)]
    original_risk: Option<i64>,
    #[sqlx(default)]
    signal_id: Option<String>,
    #[sqlx(default)]
    entry_mode: Option<String>,
    #[sqlx(default)]
    manual_intervention_required: Option<bool>,
    #[sqlx(default)]
    execution_state: Option<String>,
    #[sqlx(default)]
    last_exit_order_no: Option<String>,
    #[sqlx(default)]
    last_exit_reason: Option<String>,
    #[sqlx(default)]
    pending_entry_order_no: Option<String>,
    #[sqlx(default)]
    pending_entry_krx_orgno: Option<String>,
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
    /// 이론 진입가 (FVG mid_price)
    pub intended_entry_price: i64,
    /// 진입 슬리피지 (actual_entry - intended_entry, 양수=불리)
    pub entry_slippage: i64,
    /// 청산 슬리피지 (TP 지정가는 0 정상)
    pub exit_slippage: i64,
    /// 주문 → 체결 확인 지연 (밀리초)
    pub order_to_fill_ms: i64,
    /// 2026-04-18 Phase 1: 청산 체결가 미확인 플래그.
    /// `true` 면 `exit_price` 는 WS/REST 체결 확인 대신 현재가 fallback 으로 기록된 값이다.
    /// 잔고 0 이 확인됐기에 flat 전이 자체는 정당하지만, 실제 손익과 기록 손익이
    /// 미세하게 어긋날 수 있음을 사후 분석에 알린다.
    pub fill_price_unknown: bool,
    /// 2026-04-18 Phase 1: 청산 체결 확인 경로 라벨.
    /// - `"ws_notice"`: WS 체결통보로 확정
    /// - `"rest_execution"`: REST 체결조회로 확정
    /// - `"balance_only"`: 체결가 미확인, 잔고 감소로만 확인
    /// - `"unresolved"`: 모든 경로 실패 (manual_intervention 으로 가야 정상)
    pub exit_resolution_source: String,
}

/// 2026-04-18 Review Finding #5: 주문 라이프사이클 한 phase 기록 단위.
///
/// 기획서 `execution_journal` 요구사항 (docs/monitoring/2026-04-18-execution-timing-implementation-plan.md:125)
/// 을 그대로 반영한다. phase 예시:
/// - `entry_submitted`, `entry_acknowledged`, `entry_first_fill`, `entry_final_fill`,
///   `entry_cancelled`, `entry_manual_intervention`
/// - `exit_submitted`, `exit_final_fill`, `exit_cancelled`, `exit_manual_intervention`
/// - `exit_retry_submitted`, `exit_retry_final_fill`, `exit_retry_manual_intervention`
///
/// 모든 가격/수량 필드는 `Option<i64>` — 특정 phase 에서 의미 없으면 `None`.
#[derive(Debug, Clone)]
pub struct ExecutionJournalEntry {
    pub stock_code: String,
    /// 주문 단위 식별자 (broker order_no 또는 runtime UUID). 빈 문자열이면 "미배정".
    pub execution_id: String,
    /// SignalEngine 이 발급한 신호 ID. 현재는 Phase 2 에서 placeholder — 대부분 빈값.
    pub signal_id: String,
    /// 브로커(KIS) 주문번호. 주문 제출 성공 후 채워짐.
    pub broker_order_id: String,
    /// 라이프사이클 단계 — 위 doc comment 참고.
    pub phase: String,
    /// `Long`/`Short`/`Buy`/`Sell` 등. 빈값 허용.
    pub side: String,
    pub intended_price: Option<i64>,
    pub submitted_price: Option<i64>,
    pub filled_price: Option<i64>,
    pub filled_qty: Option<i64>,
    pub holdings_before: Option<i64>,
    pub holdings_after: Option<i64>,
    /// `ExitResolutionSource` 라벨 또는 빈문자열.
    pub resolution_source: String,
    /// 상위 호출자가 넣는 "사유" 라벨 (예: `close_position_market`, `retry_after_network_error`).
    pub reason: String,
    /// 에러 문자열. 정상 경로면 빈문자열.
    pub error_message: String,
    /// 추가 JSONB 메타데이터 (호가, slippage 등).
    pub metadata: serde_json::Value,
    pub sent_at: Option<DateTime<Utc>>,
    pub ack_at: Option<DateTime<Utc>>,
    pub first_fill_at: Option<DateTime<Utc>>,
    pub final_fill_at: Option<DateTime<Utc>>,
}

impl ExecutionJournalEntry {
    /// 모든 선택 필드가 `None` / 빈값인 최소 skeleton.
    pub fn new(stock_code: impl Into<String>, phase: impl Into<String>) -> Self {
        Self {
            stock_code: stock_code.into(),
            execution_id: String::new(),
            signal_id: String::new(),
            broker_order_id: String::new(),
            phase: phase.into(),
            side: String::new(),
            intended_price: None,
            submitted_price: None,
            filled_price: None,
            filled_qty: None,
            holdings_before: None,
            holdings_after: None,
            resolution_source: String::new(),
            reason: String::new(),
            error_message: String::new(),
            metadata: serde_json::json!({}),
            sent_at: None,
            ack_at: None,
            first_fill_at: None,
            final_fill_at: None,
        }
    }
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
