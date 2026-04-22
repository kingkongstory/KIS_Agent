use chrono::NaiveDate;
use sqlx::PgPool;
use tracing::{info, warn};

/// 일일 결산 리포트 생성기
///
/// 장 종료 후 `trades` + `event_log` 데이터를 집계하여
/// `daily_report` 테이블에 UPSERT한다.
pub struct DailyReportGenerator {
    pool: PgPool,
}

impl DailyReportGenerator {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 특정 날짜의 일일 결산 생성 (종목별 + 전체 합산)
    pub async fn generate(&self, date: NaiveDate) {
        // 종목별 결산
        for code in ["122630", "114800"] {
            if let Err(e) = self.generate_for_stock(date, code).await {
                warn!("일일 결산 실패 ({}): {e}", code);
            }
        }

        // 전체 합산 결산 (stock_code='')
        if let Err(e) = self.generate_total(date).await {
            warn!("일일 결산 합산 실패: {e}");
        }

        info!("일일 결산 리포트 생성 완료 ({})", date);
    }

    async fn generate_for_stock(&self, date: NaiveDate, stock_code: &str) -> Result<(), String> {
        // trades 집계
        let trade_stats: Option<(i64, i64, i64, f64, f64)> = sqlx::query_as(
            "SELECT
                COUNT(*)::BIGINT as total,
                COUNT(*) FILTER (WHERE pnl_pct > 0)::BIGINT as wins,
                COUNT(*) FILTER (WHERE pnl_pct <= 0)::BIGINT as losses,
                COALESCE(SUM(pnl_pct), 0.0) as total_pnl,
                COALESCE(MIN(pnl_pct), 0.0) as max_loss
             FROM trades
             WHERE stock_code = $1 AND entry_time::date = $2",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("trades 집계 실패: {e}"))?;

        let (total, wins, losses, total_pnl, max_loss) = trade_stats.unwrap_or((0, 0, 0, 0.0, 0.0));
        let win_rate = if total > 0 {
            wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        // 평균 슬리피지
        let avg_slip: Option<(Option<f64>,)> = sqlx::query_as(
            "SELECT AVG(entry_slippage)::DOUBLE PRECISION FROM trades
             WHERE stock_code = $1 AND entry_time::date = $2 AND entry_slippage != 0",
        )
        .bind(stock_code)
        .bind(date)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("슬리피지 집계 실패: {e}"))?;
        let avg_entry_slippage = avg_slip.and_then(|(v,)| v).unwrap_or(0.0) as i64;

        // event_log 시스템 이벤트 카운트
        let ws_reconnect: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT FROM event_log
             WHERE event_type = 'ws_reconnect' AND event_time::date = $1
             AND (stock_code = $2 OR stock_code = '')",
        )
        .bind(date)
        .bind(stock_code)
        .fetch_one(&self.pool)
        .await
        .unwrap_or((0,));

        let api_error: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT FROM event_log
             WHERE event_type = 'api_error' AND event_time::date = $1
             AND (stock_code = $2 OR stock_code = '')",
        )
        .bind(date)
        .bind(stock_code)
        .fetch_one(&self.pool)
        .await
        .unwrap_or((0,));

        // UPSERT
        sqlx::query(
            "INSERT INTO daily_report (date, stock_code, total_trades, wins, losses, win_rate, total_pnl_pct, max_loss_pnl_pct, avg_entry_slippage, ws_reconnect_count, api_error_count)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (date, stock_code) DO UPDATE
             SET total_trades=$3, wins=$4, losses=$5, win_rate=$6, total_pnl_pct=$7, max_loss_pnl_pct=$8, avg_entry_slippage=$9, ws_reconnect_count=$10, api_error_count=$11",
        )
        .bind(date)
        .bind(stock_code)
        .bind(total as i32)
        .bind(wins as i32)
        .bind(losses as i32)
        .bind(win_rate)
        .bind(total_pnl)
        .bind(max_loss)
        .bind(avg_entry_slippage)
        .bind(ws_reconnect.0 as i32)
        .bind(api_error.0 as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("daily_report UPSERT 실패: {e}"))?;

        Ok(())
    }

    async fn generate_total(&self, date: NaiveDate) -> Result<(), String> {
        // 전체 합산
        let trade_stats: Option<(i64, i64, i64, f64, f64)> = sqlx::query_as(
            "SELECT
                COUNT(*)::BIGINT, COUNT(*) FILTER (WHERE pnl_pct > 0)::BIGINT,
                COUNT(*) FILTER (WHERE pnl_pct <= 0)::BIGINT,
                COALESCE(SUM(pnl_pct), 0.0), COALESCE(MIN(pnl_pct), 0.0)
             FROM trades WHERE entry_time::date = $1",
        )
        .bind(date)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("합산 집계 실패: {e}"))?;

        let (total, wins, losses, total_pnl, max_loss) = trade_stats.unwrap_or((0, 0, 0, 0.0, 0.0));
        let win_rate = if total > 0 {
            wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let ws_reconnect: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT FROM event_log WHERE event_type = 'ws_reconnect' AND event_time::date = $1",
        )
        .bind(date).fetch_one(&self.pool).await.unwrap_or((0,));

        let api_error: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT FROM event_log WHERE event_type = 'api_error' AND event_time::date = $1",
        )
        .bind(date).fetch_one(&self.pool).await.unwrap_or((0,));

        sqlx::query(
            "INSERT INTO daily_report (date, stock_code, total_trades, wins, losses, win_rate, total_pnl_pct, max_loss_pnl_pct, ws_reconnect_count, api_error_count)
             VALUES ($1, '', $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (date, stock_code) DO UPDATE
             SET total_trades=$2, wins=$3, losses=$4, win_rate=$5, total_pnl_pct=$6, max_loss_pnl_pct=$7, ws_reconnect_count=$8, api_error_count=$9",
        )
        .bind(date)
        .bind(total as i32).bind(wins as i32).bind(losses as i32)
        .bind(win_rate).bind(total_pnl).bind(max_loss)
        .bind(ws_reconnect.0 as i32).bind(api_error.0 as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("합산 UPSERT 실패: {e}"))?;

        Ok(())
    }
}
