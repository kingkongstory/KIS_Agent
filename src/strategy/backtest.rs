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
        self.run_impl(stock_code, days, false).await
    }

    /// 두 종목 통합 백테스트: position_lock 시뮬레이션 (선착순 전액 투입)
    /// 같은 날 두 종목 모두 신호 → 먼저 진입한 쪽만 거래, 청산 후 다른 쪽 진입 가능
    pub async fn run_dual_locked(
        &self,
        code_a: &str,
        code_b: &str,
        days: usize,
        multi_stage: bool,
    ) -> Result<(BacktestReport, BacktestReport), KisError> {
        // 두 종목의 날짜 교집합
        let dates_a = self.fetch_available_dates(code_a, days).await?;
        let dates_b = self.fetch_available_dates(code_b, days).await?;
        let dates_set: std::collections::HashSet<NaiveDate> = dates_b.iter().cloned().collect();
        let common_dates: Vec<NaiveDate> = dates_a.iter()
            .filter(|d| dates_set.contains(d))
            .cloned()
            .collect();

        info!("통합 백테스트 (position_lock): {} + {}, {}일", code_a, code_b, common_dates.len());

        let stages = [
            ("5m",  NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 5, 0).unwrap()),
            ("15m", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 15, 0).unwrap()),
            ("30m", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 30, 0).unwrap()),
        ];

        let mut trades_a: Vec<(NaiveDate, TradeResult)> = Vec::new();
        let mut trades_b: Vec<(NaiveDate, TradeResult)> = Vec::new();

        for date in &common_dates {
            let candles_a = self.fetch_day_candles(code_a, *date).await?;
            let candles_b = self.fetch_day_candles(code_b, *date).await?;

            // 각 종목의 Multi-Stage 결과를 먼저 계산
            let results_a = if multi_stage {
                self.run_day_multi_stage(&candles_a, &stages)
            } else {
                self.strategy.run_day(&candles_a)
            };
            let results_b = if multi_stage {
                self.run_day_multi_stage(&candles_b, &stages)
            } else {
                self.strategy.run_day(&candles_b)
            };

            // 선착순: 첫 진입 시각 비교
            let first_a = results_a.first().map(|r| r.entry_time);
            let first_b = results_b.first().map(|r| r.entry_time);

            match (first_a, first_b) {
                (Some(ta), Some(tb)) => {
                    if ta <= tb {
                        // A가 먼저 → A 거래 실행, B는 A 청산 후 가능한 거래만
                        let a_last_exit = results_a.last().map(|r| r.exit_time);
                        for r in &results_a {
                            trades_a.push((*date, r.clone()));
                        }
                        // B에서 A 청산 이후 거래만 포함
                        if let Some(a_exit) = a_last_exit {
                            for r in &results_b {
                                if r.entry_time > a_exit {
                                    trades_b.push((*date, r.clone()));
                                }
                            }
                        }
                    } else {
                        // B가 먼저
                        let b_last_exit = results_b.last().map(|r| r.exit_time);
                        for r in &results_b {
                            trades_b.push((*date, r.clone()));
                        }
                        if let Some(b_exit) = b_last_exit {
                            for r in &results_a {
                                if r.entry_time > b_exit {
                                    trades_a.push((*date, r.clone()));
                                }
                            }
                        }
                    }
                }
                (Some(_), None) => {
                    for r in results_a { trades_a.push((*date, r)); }
                }
                (None, Some(_)) => {
                    for r in results_b { trades_b.push((*date, r)); }
                }
                (None, None) => {}
            }
        }

        let report_a = Self::build_report(code_a, common_dates.len(), trades_a);
        let report_b = Self::build_report(code_b, common_dates.len(), trades_b);
        Ok((report_a, report_b))
    }

    /// 단일 날짜 Multi-Stage 실행 (선착순)
    fn run_day_multi_stage(
        &self,
        candles: &[MinuteCandle],
        stages: &[(&str, NaiveTime, NaiveTime)],
    ) -> Vec<TradeResult> {
        if candles.is_empty() { return Vec::new(); }

        let mut earliest_results: Vec<TradeResult> = Vec::new();
        let mut earliest_entry = NaiveTime::from_hms_opt(23, 59, 59).unwrap();

        for (_, or_start, or_end) in stages {
            let mut cfg = self.strategy.config.clone();
            cfg.or_start = *or_start;
            cfg.or_end = *or_end;
            let strat = OrbFvgStrategy { config: cfg };
            let results = strat.run_day(candles);
            if let Some(first) = results.first() {
                if first.entry_time < earliest_entry {
                    earliest_entry = first.entry_time;
                    earliest_results = results;
                }
            }
        }
        earliest_results
    }

    fn build_report(stock_code: &str, days: usize, trades: Vec<(NaiveDate, TradeResult)>) -> BacktestReport {
        let total = trades.len();
        let wins = trades.iter().filter(|(_, t)| t.is_win()).count();
        let losses = total - wins;
        let total_pnl: f64 = trades.iter().map(|(_, t)| t.pnl_pct()).sum();
        let avg_rr = if total > 0 {
            trades.iter().map(|(_, t)| t.realized_rr()).sum::<f64>() / total as f64
        } else { 0.0 };
        BacktestReport {
            stock_code: stock_code.to_string(),
            days_tested: days,
            trades, total_trades: total, wins, losses,
            win_rate: if total > 0 { wins as f64 / total as f64 * 100.0 } else { 0.0 },
            total_pnl_pct: total_pnl, avg_rr,
        }
    }

    /// Multi-Stage ORB 백테스트: 5분/15분/30분 OR을 동시 추적 (선착순)
    /// 각 OR 단계별 전략을 실행하되, 가장 먼저 진입하는 단계를 채택
    /// 실전에서도 동일하게 구현 가능 (사후 선택 없음)
    pub async fn run_multi_stage(
        &self,
        stock_code: &str,
        days: usize,
    ) -> Result<BacktestReport, KisError> {
        let dates = self.fetch_available_dates(stock_code, days).await?;
        if dates.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code}: DB에 분봉 데이터가 없습니다."
            )));
        }

        info!(
            "Multi-Stage ORB (선착순) 백테스트: {}일 ({} ~ {})",
            dates.len(), dates.first().unwrap(), dates.last().unwrap(),
        );

        // 3개 OR 단계: 5분(09:00~09:05), 15분(09:00~09:15), 30분(09:00~09:30)
        let stages = [
            ("5분OR", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 5, 0).unwrap()),
            ("15분OR", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 15, 0).unwrap()),
            ("30분OR", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 30, 0).unwrap()),
        ];

        let mut trades = Vec::new();

        for date in &dates {
            let candles = self.fetch_day_candles(stock_code, *date).await?;
            if candles.is_empty() {
                continue;
            }

            // 각 OR 단계 실행 → 첫 진입 시각이 가장 빠른 단계 채택
            let mut earliest_stage = "";
            let mut earliest_results: Vec<TradeResult> = Vec::new();
            let mut earliest_entry = NaiveTime::from_hms_opt(23, 59, 59).unwrap();

            for (name, or_start, or_end) in &stages {
                let mut stage_config = self.strategy.config.clone();
                stage_config.or_start = *or_start;
                stage_config.or_end = *or_end;
                let stage_strategy = OrbFvgStrategy { config: stage_config };

                let results = stage_strategy.run_day(&candles);
                if let Some(first) = results.first() {
                    if first.entry_time < earliest_entry {
                        earliest_entry = first.entry_time;
                        earliest_results = results;
                        earliest_stage = name;
                    }
                }
            }

            if !earliest_results.is_empty() {
                let pnl: f64 = earliest_results.iter().map(|t| t.pnl_pct()).sum();
                info!("{date}: 선착순 {} (진입 {}) — {}건, {:.2}%",
                    earliest_stage, earliest_entry, earliest_results.len(), pnl);
                for r in earliest_results {
                    trades.push((*date, r));
                }
            }
        }

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
            win_rate: if total > 0 { wins as f64 / total as f64 * 100.0 } else { 0.0 },
            total_pnl_pct: total_pnl,
            avg_rr,
        })
    }

    /// Session-Based Reset 백테스트: 오전/오후 세션 각각 독립 OR 생성
    pub async fn run_session_reset(
        &self,
        stock_code: &str,
        days: usize,
    ) -> Result<BacktestReport, KisError> {
        let dates = self.fetch_available_dates(stock_code, days).await?;
        if dates.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code}: DB에 분봉 데이터가 없습니다."
            )));
        }

        info!(
            "Session Reset 백테스트: {}일 ({} ~ {})",
            dates.len(), dates.first().unwrap(), dates.last().unwrap(),
        );

        let mut trades = Vec::new();

        for date in &dates {
            let candles = self.fetch_day_candles(stock_code, *date).await?;
            if candles.is_empty() {
                continue;
            }

            // 오전 세션: OR 09:00~09:15, 거래 09:15~12:30
            let am_cutoff = NaiveTime::from_hms_opt(12, 30, 0).unwrap();
            let am_candles: Vec<_> = candles.iter()
                .filter(|c| c.time < am_cutoff)
                .cloned()
                .collect();

            if !am_candles.is_empty() {
                let am_results = self.strategy.run_day(&am_candles);
                info!("{date} 오전: {}개 캔들, {}건 거래", am_candles.len(), am_results.len());
                for r in am_results {
                    trades.push((*date, r));
                }
            }

            // 오후 세션: OR 13:00~13:15, 거래 13:15~15:20
            let pm_or_start = NaiveTime::from_hms_opt(13, 0, 0).unwrap();
            let pm_or_end = NaiveTime::from_hms_opt(13, 15, 0).unwrap();
            let pm_candles: Vec<MinuteCandle> = candles.iter()
                .filter(|c| c.time >= pm_or_start)
                .cloned()
                .collect();

            if !pm_candles.is_empty() {
                // 오후 세션 전용 config: OR 시간만 변경
                let mut pm_config = self.strategy.config.clone();
                pm_config.or_start = pm_or_start;
                pm_config.or_end = pm_or_end;
                let pm_strategy = OrbFvgStrategy { config: pm_config };

                let pm_results = pm_strategy.run_day(&pm_candles);
                info!("{date} 오후: {}개 캔들, {}건 거래", pm_candles.len(), pm_results.len());
                for r in pm_results {
                    trades.push((*date, r));
                }
            }
        }

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
            win_rate: if total > 0 { wins as f64 / total as f64 * 100.0 } else { 0.0 },
            total_pnl_pct: total_pnl,
            avg_rr,
        })
    }

    /// Dynamic Target 백테스트: 과거 N일 OR 돌파 확장폭 평균으로 TP 동적 설정
    pub async fn run_dynamic_target(
        &self,
        stock_code: &str,
        days: usize,
        lookback: usize,
    ) -> Result<BacktestReport, KisError> {
        self.run_impl_dynamic(stock_code, days, lookback).await
    }

    async fn run_impl(
        &self,
        stock_code: &str,
        days: usize,
        _dynamic: bool,
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

    /// Dynamic Target 백테스트 구현
    /// 각 날의 OR 돌파 후 최대 확장폭을 기록하고, 과거 N일 평균을 다음 날 RR로 사용
    async fn run_impl_dynamic(
        &self,
        stock_code: &str,
        days: usize,
        lookback: usize,
    ) -> Result<BacktestReport, KisError> {
        let dates = self.fetch_available_dates(stock_code, days + lookback).await?;
        if dates.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code}: DB에 분봉 데이터가 없습니다."
            )));
        }

        info!(
            "Dynamic Target 백테스트: {}일 (lookback={}일, {} ~ {})",
            dates.len(), lookback, dates.first().unwrap(), dates.last().unwrap(),
        );

        // 1단계: 각 날의 OR 돌파 후 최대 확장폭(R 배수) 기록
        let mut daily_extensions: Vec<f64> = Vec::new();
        let mut trades = Vec::new();

        for (day_idx, date) in dates.iter().enumerate() {
            let candles = self.fetch_day_candles(stock_code, *date).await?;
            if candles.is_empty() {
                daily_extensions.push(0.0);
                continue;
            }

            // OR 범위 계산
            let cfg = &self.strategy.config;
            let or_candles: Vec<_> = candles.iter()
                .filter(|c| c.time >= cfg.or_start && c.time < cfg.or_end)
                .collect();
            if or_candles.is_empty() {
                daily_extensions.push(0.0);
                continue;
            }

            let or_high = or_candles.iter().map(|c| c.high).max().unwrap();
            let or_low = or_candles.iter().map(|c| c.low).min().unwrap();
            let or_range = (or_high - or_low).max(1) as f64;

            // OR 돌파 후 최대 확장폭 (OR 범위 대비 R 배수)
            let max_extension = candles.iter()
                .filter(|c| c.time >= cfg.or_end)
                .map(|c| {
                    let bull_ext = (c.high - or_high).max(0) as f64 / or_range;
                    let bear_ext = (or_low - c.low).max(0) as f64 / or_range;
                    bull_ext.max(bear_ext)
                })
                .fold(0.0f64, |a, b| a.max(b));

            daily_extensions.push(max_extension);

            // lookback 기간이 차면 dynamic RR로 거래 실행
            if day_idx >= lookback {
                let recent = &daily_extensions[day_idx.saturating_sub(lookback)..day_idx];
                let valid: Vec<f64> = recent.iter().filter(|&&x| x > 0.0).cloned().collect();
                let dynamic_rr = if valid.is_empty() {
                    self.strategy.config.rr_ratio // fallback
                } else {
                    let avg = valid.iter().sum::<f64>() / valid.len() as f64;
                    avg.max(1.0).min(5.0) // 최소 1R, 최대 5R 클램프
                };

                // 해당 날의 RR을 동적으로 설정하여 전략 실행
                let mut day_strategy = OrbFvgStrategy { config: self.strategy.config.clone() };
                day_strategy.config.rr_ratio = dynamic_rr;

                info!("{date}: {}개 캔들, dynamic RR={:.2} (과거 {}일 평균 확장={:.2}R)",
                    candles.len(), dynamic_rr, valid.len(), dynamic_rr);

                let day_results = day_strategy.run_day(&candles);
                for result in day_results {
                    trades.push((*date, result));
                }
            }
        }

        // 결과 집계
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
            days_tested: dates.len().saturating_sub(lookback),
            trades,
            total_trades: total,
            wins,
            losses,
            win_rate: if total > 0 { wins as f64 / total as f64 * 100.0 } else { 0.0 },
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
