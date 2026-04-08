use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::infrastructure::cache::postgres_store::PostgresStore;

use super::candle::MinuteCandle;
use super::orb_fvg::OrbFvgStrategy;
use super::types::TradeResult;

/// 백테스트 보고서
#[derive(Debug)]
pub struct BacktestReport {
    pub stock_code: String,
    pub days_tested: usize,
    pub trades: Vec<(NaiveDate, TradeResult)>,
    pub total_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub total_pnl_pct: f64,
    pub avg_rr: f64,
}

impl std::fmt::Display for BacktestReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n{}", "=".repeat(60))?;
        writeln!(f, "  ORB+FVG 백테스트 결과 — {}", self.stock_code)?;
        writeln!(f, "{}", "=".repeat(60))?;
        writeln!(f, "  테스트 기간: {}일", self.days_tested)?;
        writeln!(f, "  총 거래: {}회", self.total_trades)?;
        writeln!(f, "  승리: {}회 / 패배: {}회", self.wins, self.losses)?;
        writeln!(f, "  승률: {:.1}%", self.win_rate)?;
        writeln!(f, "  총 손익: {:.2}%", self.total_pnl_pct)?;
        writeln!(f, "  평균 R배수: {:.2}R", self.avg_rr)?;
        writeln!(f, "\n  --- 개별 거래 ---")?;
        for (date, trade) in &self.trades {
            let sign = if trade.is_win() { "+" } else { "-" };
            writeln!(
                f,
                "  [{date}] {:?} {sign}{:.2}% (RR={:.2}) entry={} exit={} {}~{} {:?}",
                trade.side,
                trade.pnl_pct().abs(),
                trade.realized_rr(),
                trade.entry_price,
                trade.exit_price,
                trade.entry_time.format("%H:%M"),
                trade.exit_time.format("%H:%M"),
                trade.exit_reason,
            )?;
        }
        writeln!(f, "{}", "=".repeat(60))
    }
}

/// 백테스트 엔진 (PostgreSQL DB 기반)
pub struct BacktestEngine {
    store: Arc<PostgresStore>,
    strategy: OrbFvgStrategy,
    /// DB에 저장된 분봉 간격 (1 또는 5 등)
    source_interval: i16,
}

impl BacktestEngine {
    pub fn new(store: Arc<PostgresStore>, rr_ratio: f64, source_interval: i16) -> Self {
        Self {
            store,
            strategy: OrbFvgStrategy::new(rr_ratio),
            source_interval,
        }
    }

    pub fn with_config(
        store: Arc<PostgresStore>,
        config: super::orb_fvg::OrbFvgConfig,
        source_interval: i16,
    ) -> Self {
        Self {
            store,
            strategy: OrbFvgStrategy { config },
            source_interval,
        }
    }

    /// DB에서 분봉 조회 → 백테스트 실행
    pub async fn run(
        &self,
        stock_code: &str,
        days: usize,
    ) -> Result<BacktestReport, KisError> {
        // 1) DB에서 보유한 날짜 목록 획득
        let dates = self.fetch_available_dates(stock_code, days).await?;
        if dates.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code}: DB에 분봉 데이터가 없습니다. collect-minute-naver로 수집하세요."
            )));
        }

        info!(
            "백테스트 대상: {}일 ({} ~ {})",
            dates.len(),
            dates.first().unwrap(),
            dates.last().unwrap(),
        );

        // 2) 각 날짜별 분봉 → 전략 실행
        let mut trades = Vec::new();
        for date in &dates {
            let candles = self.fetch_day_candles(stock_code, *date).await?;
            if candles.is_empty() {
                warn!("{date}: 분봉 데이터 없음, 건너뜀");
                continue;
            }

            info!("{date}: {}개 캔들 로드", candles.len());
            let day_results = self.strategy.run_day(&candles);
            for result in day_results {
                trades.push((*date, result));
            }
        }

        // 3) 결과 집계
        let total = trades.len();
        let wins = trades.iter().filter(|(_, t)| t.is_win()).count();
        let losses = total - wins;
        let total_pnl: f64 = trades.iter().map(|(_, t)| t.pnl_pct()).sum();
        let avg_rr = if total > 0 {
            trades.iter().map(|(_, t)| t.realized_rr()).sum::<f64>() / total as f64
        } else {
            0.0
        };

        Ok(BacktestReport {
            stock_code: stock_code.to_string(),
            days_tested: dates.len(),
            trades,
            total_trades: total,
            wins,
            losses,
            win_rate: if total > 0 {
                wins as f64 / total as f64 * 100.0
            } else {
                0.0
            },
            total_pnl_pct: total_pnl,
            avg_rr,
        })
    }

    /// DB에서 분봉이 존재하는 날짜 목록 조회 (최근 N일)
    async fn fetch_available_dates(
        &self,
        stock_code: &str,
        days: usize,
    ) -> Result<Vec<NaiveDate>, KisError> {
        let rows: Vec<(NaiveDate,)> = sqlx::query_as(
            "SELECT DISTINCT datetime::date as d FROM minute_ohlcv
             WHERE stock_code = $1 AND interval_min = $2
             ORDER BY d DESC
             LIMIT $3",
        )
        .bind(stock_code)
        .bind(self.source_interval)
        .bind(days as i64)
        .fetch_all(self.store.pool())
        .await
        .map_err(|e| KisError::Internal(format!("날짜 조회 실패: {e}")))?;

        let mut dates: Vec<NaiveDate> = rows.into_iter().map(|(d,)| d).collect();
        dates.sort();
        Ok(dates)
    }

    /// 특정일 분봉 조회 → strategy::MinuteCandle 변환
    async fn fetch_day_candles(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<Vec<MinuteCandle>, KisError> {
        let start = NaiveDateTime::new(date, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        let end = NaiveDateTime::new(date, NaiveTime::from_hms_opt(15, 30, 0).unwrap());

        let db_candles = self
            .store
            .get_minute_ohlcv(stock_code, start, end, self.source_interval)
            .await?;

        Ok(db_candles
            .into_iter()
            .map(|c| MinuteCandle {
                date: c.datetime.date(),
                time: c.datetime.time(),
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                volume: c.volume as u64,
            })
            .collect())
    }
}
