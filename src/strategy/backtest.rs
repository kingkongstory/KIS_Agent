use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::infrastructure::cache::postgres_store::PostgresStore;

use super::candle::MinuteCandle;
use super::orb_fvg::OrbFvgStrategy;
use super::parity::types::StageDef;
use super::types::{PositionSide, TradeResult};

/// 종목별 incremental 시뮬레이션 상태 (run_dual_locked 전용)
#[derive(Default)]
struct StockState {
    search_from: usize,
    trade_count: usize,
    cumulative_pnl: f64,
    confirmed_side: Option<PositionSide>,
}

/// 백테스트 실행 버전 태그.
///
/// `docs/strategy/backtest-live-parity-architecture.md` §5 "legacy baseline 보존"
/// 에 따라 보고서가 어떤 전략/실행 모델로 산출됐는지 식별한다.
/// parity/replay 가 추가되면 같은 `stock_code`·`days` 에 대해 다른 값을 찍는다.
pub const LEGACY_STRATEGY_VERSION: &str = "legacy_orb_fvg";
pub const LEGACY_EXECUTION_VERSION: &str = "mid_price_zone_touch";
pub const LEGACY_SIMULATION_MODE: &str = "legacy_backtest";

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
    /// 전략 식별자. 현재 경로는 모두 `legacy_orb_fvg`. Phase 7 이후 새 전략이
    /// 추가되면 `orb_vwap_pullback` 등으로 분기.
    pub strategy_version: String,
    /// 실행 모델 식별자. legacy 경로는 `mid_price_zone_touch`, parity 경로는
    /// `passive_top_bottom_timeout_30s` 등으로 Phase 4 이후 분기.
    pub execution_version: String,
    /// 시뮬레이션 모드: `legacy_backtest` | `parity_backtest` | `replay`.
    pub simulation_mode: String,
}

impl std::fmt::Display for BacktestReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n{}", "=".repeat(60))?;
        writeln!(f, "  ORB+FVG 백테스트 결과 — {}", self.stock_code)?;
        writeln!(f, "{}", "=".repeat(60))?;
        writeln!(
            f,
            "  버전: strategy={} / execution={} / mode={}",
            self.strategy_version, self.execution_version, self.simulation_mode
        )?;
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

    /// 두 종목 통합 백테스트: 라이브 position_lock과 일치하는 incremental 시뮬레이션
    ///
    /// 동작:
    /// 1. multi_stage=true이면 종목별로 5m/15m/30m OR 중 첫 진입한 stage를 사전 결정
    /// 2. 결정된 stage의 strategy로 양 종목 5분봉을 incremental하게 처리
    /// 3. 매 lap마다 양 종목에서 다음 거래 후보를 scan_and_trade로 탐색
    /// 4. 둘 중 entry_time이 더 빠른 후보 채택, 청산 시점까지 lock
    /// 5. 청산 후 양쪽 search 위치를 lock 해제 시각 이후로 동기화하여 다음 lap
    /// 6. max_daily_trades / max_daily_loss_pct / confirmed_side는 종목별 독립 적용 (라이브 LiveRunner와 동일)
    pub async fn run_dual_locked(
        &self,
        code_a: &str,
        code_b: &str,
        days: usize,
        multi_stage: bool,
    ) -> Result<(BacktestReport, BacktestReport), KisError> {
        let dates_a = self.fetch_available_dates(code_a, days).await?;
        let dates_b = self.fetch_available_dates(code_b, days).await?;
        let dates_set: std::collections::HashSet<NaiveDate> = dates_b.iter().cloned().collect();
        let common_dates: Vec<NaiveDate> = dates_a.iter()
            .filter(|d| dates_set.contains(d))
            .cloned()
            .collect();

        info!("통합 백테스트 (incremental position_lock): {} + {}, {}일", code_a, code_b, common_dates.len());

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

            let day_pair = self.simulate_day_locked(&candles_a, &candles_b, multi_stage, &stages);
            for r in day_pair.0 { trades_a.push((*date, r)); }
            for r in day_pair.1 { trades_b.push((*date, r)); }
        }

        let report_a = Self::build_report(code_a, common_dates.len(), trades_a);
        let report_b = Self::build_report(code_b, common_dates.len(), trades_b);
        Ok((report_a, report_b))
    }

    /// 하루치 두 종목 캔들로 incremental position_lock 시뮬레이션
    fn simulate_day_locked(
        &self,
        candles_a: &[MinuteCandle],
        candles_b: &[MinuteCandle],
        multi_stage: bool,
        stages: &[(&str, NaiveTime, NaiveTime)],
    ) -> (Vec<TradeResult>, Vec<TradeResult>) {
        // 1) 종목별 strategy 결정 (multi_stage이면 가장 빠른 진입 stage)
        let strat_a = self.pick_strategy_for_day(candles_a, multi_stage, stages);
        let strat_b = self.pick_strategy_for_day(candles_b, multi_stage, stages);

        let (strat_a, candles_5m_a, or_high_a, or_low_a) = match strat_a {
            Some(x) => x,
            None => {
                // A는 진입 신호 없음 → B만 단독으로 처리
                return (Vec::new(), strat_b
                    .map(|(s, c, h, l)| Self::run_solo(&s, &c, h, l))
                    .unwrap_or_default());
            }
        };
        let (strat_b, candles_5m_b, or_high_b, or_low_b) = match strat_b {
            Some(x) => x,
            None => {
                // B는 신호 없음 → A만 단독으로 처리
                return (Self::run_solo(&strat_a, &candles_5m_a, or_high_a, or_low_a), Vec::new());
            }
        };

        // 2) incremental 시뮬레이션 — lock-aware
        let mut state_a = StockState::default();
        let mut state_b = StockState::default();
        let mut last_unlock_time: NaiveTime = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
        let mut results_a: Vec<TradeResult> = Vec::new();
        let mut results_b: Vec<TradeResult> = Vec::new();

        loop {
            // 한도/종료 조건
            let a_done = state_a.trade_count >= strat_a.config.max_daily_trades
                || state_a.cumulative_pnl <= strat_a.config.max_daily_loss_pct;
            let b_done = state_b.trade_count >= strat_b.config.max_daily_trades
                || state_b.cumulative_pnl <= strat_b.config.max_daily_loss_pct;
            if a_done && b_done { break; }

            // 양쪽에서 다음 거래 후보 탐색 (lock 해제 시각 이후로 search 동기화)
            Self::sync_search_to(&mut state_a.search_from, &candles_5m_a, last_unlock_time);
            Self::sync_search_to(&mut state_b.search_from, &candles_5m_b, last_unlock_time);

            let cand_a = if a_done { None } else {
                Self::next_valid_candidate(&strat_a, &candles_5m_a, or_high_a, or_low_a, &mut state_a)
            };
            let cand_b = if b_done { None } else {
                Self::next_valid_candidate(&strat_b, &candles_5m_b, or_high_b, or_low_b, &mut state_b)
            };

            // 둘 중 더 빠른 진입 채택
            match (cand_a, cand_b) {
                (Some((trade_a, exit_idx_a)), Some((trade_b, exit_idx_b))) => {
                    if trade_a.entry_time <= trade_b.entry_time {
                        Self::accept_trade(&trade_a, &mut state_a, exit_idx_a);
                        last_unlock_time = trade_a.exit_time;
                        results_a.push(trade_a);
                    } else {
                        Self::accept_trade(&trade_b, &mut state_b, exit_idx_b);
                        last_unlock_time = trade_b.exit_time;
                        results_b.push(trade_b);
                    }
                }
                (Some((trade_a, exit_idx_a)), None) => {
                    Self::accept_trade(&trade_a, &mut state_a, exit_idx_a);
                    last_unlock_time = trade_a.exit_time;
                    results_a.push(trade_a);
                }
                (None, Some((trade_b, exit_idx_b))) => {
                    Self::accept_trade(&trade_b, &mut state_b, exit_idx_b);
                    last_unlock_time = trade_b.exit_time;
                    results_b.push(trade_b);
                }
                (None, None) => break,
            }
        }

        (results_a, results_b)
    }

    /// 종목 단독 처리 (다른 종목이 신호 없을 때)
    fn run_solo(
        strat: &OrbFvgStrategy,
        candles_5m: &[MinuteCandle],
        or_high: i64,
        or_low: i64,
    ) -> Vec<TradeResult> {
        let mut state = StockState::default();
        let mut results = Vec::new();
        loop {
            if state.trade_count >= strat.config.max_daily_trades { break; }
            if state.cumulative_pnl <= strat.config.max_daily_loss_pct { break; }
            match Self::next_valid_candidate(strat, candles_5m, or_high, or_low, &mut state) {
                Some((trade, exit_idx)) => {
                    Self::accept_trade(&trade, &mut state, exit_idx);
                    results.push(trade);
                }
                None => break,
            }
        }
        results
    }

    /// 종목별 strategy + 사용할 5분봉 + OR 범위 결정
    /// multi_stage=true이면 5m/15m/30m OR 중 첫 진입이 가장 빠른 stage 채택
    fn pick_strategy_for_day(
        &self,
        candles: &[MinuteCandle],
        multi_stage: bool,
        stages: &[(&str, NaiveTime, NaiveTime)],
    ) -> Option<(OrbFvgStrategy, Vec<MinuteCandle>, i64, i64)> {
        if candles.is_empty() { return None; }

        let prepare = |cfg: super::orb_fvg::OrbFvgConfig| -> Option<(OrbFvgStrategy, Vec<MinuteCandle>, i64, i64)> {
            let or_candles: Vec<_> = candles.iter()
                .filter(|c| c.time >= cfg.or_start && c.time < cfg.or_end)
                .cloned().collect();
            if or_candles.is_empty() { return None; }
            let or_high = or_candles.iter().map(|c| c.high).max().unwrap();
            let or_low = or_candles.iter().map(|c| c.low).min().unwrap();
            let scan: Vec<_> = candles.iter().filter(|c| c.time >= cfg.or_end).cloned().collect();
            if scan.is_empty() { return None; }
            Some((OrbFvgStrategy { config: cfg }, scan, or_high, or_low))
        };

        if !multi_stage {
            return prepare(self.strategy.config.clone());
        }

        // Multi-Stage: 각 stage별로 첫 진입을 dry-run으로 측정 → 가장 빠른 stage 채택
        let mut earliest_entry = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
        let mut earliest: Option<(OrbFvgStrategy, Vec<MinuteCandle>, i64, i64)> = None;
        for (_, or_start, or_end) in stages {
            let mut cfg = self.strategy.config.clone();
            cfg.or_start = *or_start;
            cfg.or_end = *or_end;
            let prepared = match prepare(cfg) {
                Some(p) => p,
                None => continue,
            };
            // dry-run: scan_and_trade로 첫 거래 entry_time만 측정
            let (first_trade, _, _) = prepared.0.scan_and_trade(&prepared.1, prepared.2, prepared.3, true);
            if let Some(t) = first_trade {
                if t.entry_time < earliest_entry {
                    earliest_entry = t.entry_time;
                    earliest = Some(prepared);
                }
            }
        }
        earliest
    }

    /// search_from 인덱스를 target 시각 이후 첫 캔들 위치로 동기화
    fn sync_search_to(search_from: &mut usize, candles: &[MinuteCandle], target: NaiveTime) {
        let new_pos = candles.iter().position(|c| c.time > target).unwrap_or(candles.len());
        if new_pos > *search_from {
            *search_from = new_pos;
        }
    }

    /// 종목 state로 다음 유효 거래 후보 탐색 (confirmed_side / search_from 진행 처리)
    /// 반환: (TradeResult, search_from에 더할 진행 인덱스)
    fn next_valid_candidate(
        strat: &OrbFvgStrategy,
        candles_5m: &[MinuteCandle],
        or_high: i64,
        or_low: i64,
        state: &mut StockState,
    ) -> Option<(TradeResult, usize)> {
        loop {
            if state.search_from >= candles_5m.len() { return None; }
            let scan_slice = &candles_5m[state.search_from..];
            let require_or = state.trade_count == 0;
            let (trade_result, exit_idx_rel, _) =
                strat.scan_and_trade(scan_slice, or_high, or_low, require_or);

            match trade_result {
                Some(trade) => {
                    // 방향 일치 체크 (1차 이후)
                    if let Some(cs) = state.confirmed_side {
                        if trade.side != cs {
                            // 거부 → 다음 신호 탐색
                            state.search_from += exit_idx_rel.max(1);
                            continue;
                        }
                    }
                    return Some((trade, exit_idx_rel));
                }
                None => return None,
            }
        }
    }

    /// 채택된 거래로 state 업데이트
    fn accept_trade(trade: &TradeResult, state: &mut StockState, exit_idx_rel: usize) {
        let pnl = trade.pnl_pct();
        state.trade_count += 1;
        state.cumulative_pnl += pnl;
        state.confirmed_side = if pnl > 0.0 { Some(trade.side) } else { None };
        state.search_from += exit_idx_rel;
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
            strategy_version: LEGACY_STRATEGY_VERSION.to_string(),
            execution_version: LEGACY_EXECUTION_VERSION.to_string(),
            simulation_mode: LEGACY_SIMULATION_MODE.to_string(),
        }
    }

    fn live_stage_defs(&self) -> Vec<StageDef> {
        let or_start = self.strategy.config.or_start;
        vec![
            StageDef {
                name: "5m".into(),
                or_start,
                or_end: NaiveTime::from_hms_opt(9, 5, 0).unwrap(),
            },
            StageDef {
                name: "15m".into(),
                or_start,
                or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            },
            StageDef {
                name: "30m".into(),
                or_start,
                or_end: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            },
        ]
    }

    async fn fetch_day_candles_with_interval(
        &self,
        stock_code: &str,
        date: NaiveDate,
        interval_min: i16,
    ) -> Result<Vec<MinuteCandle>, KisError> {
        let start = NaiveDateTime::new(date, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        let end = NaiveDateTime::new(date, NaiveTime::from_hms_opt(15, 30, 0).unwrap());

        let db_candles = self
            .store
            .get_minute_ohlcv(stock_code, start, end, interval_min)
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

    async fn fetch_day_candles_prefer_1m(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<(Vec<MinuteCandle>, i16), KisError> {
        if self.source_interval != 1 {
            let candles_1m = self.fetch_day_candles_with_interval(stock_code, date, 1).await?;
            if !candles_1m.is_empty() {
                return Ok((candles_1m, 1));
            }
        }

        let candles = self
            .fetch_day_candles_with_interval(stock_code, date, self.source_interval)
            .await?;
        Ok((candles, self.source_interval))
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
            strategy_version: LEGACY_STRATEGY_VERSION.to_string(),
            execution_version: LEGACY_EXECUTION_VERSION.to_string(),
            simulation_mode: LEGACY_SIMULATION_MODE.to_string(),
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
            strategy_version: LEGACY_STRATEGY_VERSION.to_string(),
            execution_version: LEGACY_EXECUTION_VERSION.to_string(),
            simulation_mode: LEGACY_SIMULATION_MODE.to_string(),
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
            strategy_version: LEGACY_STRATEGY_VERSION.to_string(),
            execution_version: LEGACY_EXECUTION_VERSION.to_string(),
            simulation_mode: LEGACY_SIMULATION_MODE.to_string(),
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
            strategy_version: LEGACY_STRATEGY_VERSION.to_string(),
            execution_version: LEGACY_EXECUTION_VERSION.to_string(),
            simulation_mode: LEGACY_SIMULATION_MODE.to_string(),
        })
    }

    /// Parity 백테스트: `SignalEngine + ExecutionPolicy + PositionManager` 조합.
    ///
    /// `policy_id` 에 따라 분기:
    /// - "passive" (기본): `PassiveTopBottomTimeout30s` — 현재 라이브 규칙 재현.
    /// - "legacy": `LegacyMidPriceSim` — 기존 legacy baseline 과 동일한 mid_price 진입.
    ///
    /// 보고서의 `execution_version` / `simulation_mode` 가 legacy 경로와
    /// 다르게 찍히므로 같은 날짜에 대해 legacy vs parity 비교가 가능하다.
    pub async fn run_parity(
        &self,
        stock_code: &str,
        days: usize,
        policy_id: &str,
    ) -> Result<BacktestReport, KisError> {
        use super::parity::{
            execution_policy::{LegacyMidPriceSim, PassiveTopBottomTimeout30s},
            parity_backtest::ParityDayRunner,
            position_manager::PositionManagerConfig,
            signal_engine::OrbFvgSignalEngine,
        };

        let dates = self.fetch_available_dates(stock_code, days).await?;
        if dates.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code}: DB에 분봉 데이터가 없습니다."
            )));
        }

        info!(
            "parity 백테스트 대상: {}일 ({} ~ {}), policy={}",
            dates.len(),
            dates.first().unwrap(),
            dates.last().unwrap(),
            policy_id
        );

        let cfg = &self.strategy.config;
        let pm_config = PositionManagerConfig {
            rr_ratio: cfg.rr_ratio,
            trailing_r: cfg.trailing_r,
            breakeven_r: cfg.breakeven_r,
            time_stop_candles: cfg.time_stop_candles,
            candle_interval_min: 5,
        };

        let (execution_version, simulation_mode) = match policy_id {
            "legacy" => ("mid_price_zone_touch", "parity_backtest_legacy"),
            _ => ("passive_top_bottom_timeout_30s", "parity_backtest_passive"),
        };

        let stage_defs = self.live_stage_defs();
        let mut trades = Vec::new();
        for date in &dates {
            let (candles, interval_used) = self.fetch_day_candles_prefer_1m(stock_code, *date).await?;
            if candles.is_empty() {
                warn!("{date}: 분봉 데이터 없음, 건너뜀");
                continue;
            }
            if interval_used != 1 {
                warn!(
                    "{date}: 1분봉 없음 — parity를 {}분 입력으로 근사 실행",
                    interval_used
                );
            }

            info!("{date}: {}개 캔들 로드 (parity)", candles.len());
            let day_results: Vec<TradeResult> = match policy_id {
                "legacy" => {
                    let runner = ParityDayRunner {
                        signal_engine: OrbFvgSignalEngine,
                        policy: LegacyMidPriceSim,
                        strategy_id: "orb_fvg".into(),
                        stage_name: "parity_single_stage".into(),
                        or_start: cfg.or_start,
                        or_end: cfg.or_end,
                        entry_cutoff: cfg.entry_cutoff,
                        force_exit: cfg.force_exit,
                        pm_config,
                        fvg_expiry_candles: cfg.fvg_expiry_candles,
                        long_only: cfg.long_only,
                        rr_ratio: cfg.rr_ratio,
                        max_entry_drift_pct: None,
                        max_daily_trades: cfg.max_daily_trades,
                        max_daily_loss_pct: cfg.max_daily_loss_pct,
                    };
                    runner.run_day_live_like(&candles, &stage_defs)
                }
                _ => {
                    let runner = ParityDayRunner {
                        signal_engine: OrbFvgSignalEngine,
                        policy: PassiveTopBottomTimeout30s,
                        strategy_id: "orb_fvg".into(),
                        stage_name: "parity_single_stage".into(),
                        or_start: cfg.or_start,
                        or_end: cfg.or_end,
                        entry_cutoff: cfg.entry_cutoff,
                        force_exit: cfg.force_exit,
                        pm_config,
                        fvg_expiry_candles: cfg.fvg_expiry_candles,
                        long_only: cfg.long_only,
                        rr_ratio: cfg.rr_ratio,
                        max_entry_drift_pct: None,
                        max_daily_trades: cfg.max_daily_trades,
                        max_daily_loss_pct: cfg.max_daily_loss_pct,
                    };
                    runner.run_day_live_like(&candles, &stage_defs)
                }
            };
            for result in day_results {
                trades.push((*date, result));
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
            strategy_version: LEGACY_STRATEGY_VERSION.to_string(),
            execution_version: execution_version.to_string(),
            simulation_mode: simulation_mode.to_string(),
        })
    }

    /// Candle replay comparison — 단일 날짜에 대해 legacy / parity-legacy /
    /// parity-passive 세 경로를 실행하여 결과를 나란히 반환.
    ///
    /// `docs/strategy/backtest-live-parity-architecture.md` §12.3 / §14.3.
    pub async fn replay_day(
        &self,
        stock_code: &str,
        date: NaiveDate,
    ) -> Result<super::parity::replay::ReplayDayReport, KisError> {
        use super::parity::{
            execution_policy::{LegacyMidPriceSim, PassiveTopBottomTimeout30s},
            parity_backtest::ParityDayRunner,
            position_manager::PositionManagerConfig,
            replay::ReplayDayReport,
            signal_engine::OrbFvgSignalEngine,
        };

        let candles = self.fetch_day_candles(stock_code, date).await?;
        if candles.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code} / {date}: DB에 분봉 데이터가 없습니다."
            )));
        }
        let (parity_candles, interval_used) = self.fetch_day_candles_prefer_1m(stock_code, date).await?;
        if parity_candles.is_empty() {
            return Err(KisError::Internal(format!(
                "{stock_code} / {date}: parity 실행용 분봉 데이터가 없습니다."
            )));
        }
        if interval_used != 1 {
            warn!(
                "{stock_code} / {date}: 1분봉 없음 — replay parity를 {}분 입력으로 근사 실행",
                interval_used
            );
        }

        let stages = self.live_stage_defs();
        let stage_tuples = [
            ("5분OR", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 5, 0).unwrap()),
            ("15분OR", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 15, 0).unwrap()),
            ("30분OR", NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(9, 30, 0).unwrap()),
        ];

        let legacy = self.run_day_multi_stage(&candles, &stage_tuples);

        let cfg = &self.strategy.config;
        let pm_config = PositionManagerConfig {
            rr_ratio: cfg.rr_ratio,
            trailing_r: cfg.trailing_r,
            breakeven_r: cfg.breakeven_r,
            time_stop_candles: cfg.time_stop_candles,
            candle_interval_min: 5,
        };

        let runner_legacy_policy = ParityDayRunner {
            signal_engine: OrbFvgSignalEngine,
            policy: LegacyMidPriceSim,
            strategy_id: "orb_fvg".into(),
            stage_name: "parity_single_stage".into(),
            or_start: cfg.or_start,
            or_end: cfg.or_end,
            entry_cutoff: cfg.entry_cutoff,
            force_exit: cfg.force_exit,
            pm_config,
            fvg_expiry_candles: cfg.fvg_expiry_candles,
            long_only: cfg.long_only,
            rr_ratio: cfg.rr_ratio,
            max_entry_drift_pct: None,
            max_daily_trades: cfg.max_daily_trades,
            max_daily_loss_pct: cfg.max_daily_loss_pct,
        };
        let parity_legacy = runner_legacy_policy.run_day_live_like(&parity_candles, &stages);

        let runner_passive = ParityDayRunner {
            signal_engine: OrbFvgSignalEngine,
            policy: PassiveTopBottomTimeout30s,
            strategy_id: "orb_fvg".into(),
            stage_name: "parity_single_stage".into(),
            or_start: cfg.or_start,
            or_end: cfg.or_end,
            entry_cutoff: cfg.entry_cutoff,
            force_exit: cfg.force_exit,
            pm_config,
            fvg_expiry_candles: cfg.fvg_expiry_candles,
            long_only: cfg.long_only,
            rr_ratio: cfg.rr_ratio,
            max_entry_drift_pct: None,
            max_daily_trades: cfg.max_daily_trades,
            max_daily_loss_pct: cfg.max_daily_loss_pct,
        };
        let parity_passive = runner_passive.run_day_live_like(&parity_candles, &stages);

        Ok(ReplayDayReport {
            date,
            stock_code: stock_code.to_string(),
            legacy,
            parity_legacy,
            parity_passive,
            source_interval_used: interval_used,
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
        self.fetch_day_candles_with_interval(stock_code, date, self.source_interval)
            .await
    }
}
