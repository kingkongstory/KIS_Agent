//! Candle Replay Comparison — 동일 날짜를 legacy / parity-passive / parity-legacy
//! 세 경로로 실행해 결과를 나란히 비교.
//!
//! `docs/strategy/backtest-live-parity-architecture.md` §12.3 / §14.3 1차 구현.
//!
//! ## 현재 범위 (Phase 6 1차)
//! 이번 단계의 replay 는 **같은 날짜의 캔들 입력 → 세 실행 경로의 결과 비교** 에
//! 한정한다. event_log/WS 타임라인 재생이 아니라, 하루 입력을 기준으로
//! `legacy baseline` 과 `parity` 두 execution model 을 구조적으로 비교하는
//! 리포트다.
//!
//! ## 확장점 (Phase 6 2차, 이후 작업)
//! - event_log 테이블 기반 실제 주문 타임라인 재생
//! - WS / REST / balance 재검증 경로 관찰
//! - search_after, position_lock, manual_intervention 이벤트 시퀀스 검증
//!
//! 현재 event_log 포맷이 유동적이라 1차에서는 다루지 않는다. 대신 legacy 와
//! parity 백테스트의 결과가 같은 날짜에 어떻게 다른지 구조적으로 보여주는 것이
//! 1차 완료 기준이다.

use chrono::NaiveDate;

use crate::strategy::types::TradeResult;

/// Replay 결과 — 같은 날짜에 대해 세 실행 경로의 거래 결과를 모은다.
#[derive(Debug, Clone)]
pub struct ReplayDayReport {
    pub date: NaiveDate,
    pub stock_code: String,
    pub source_interval_used: i16,
    pub legacy: Vec<TradeResult>,
    pub parity_legacy: Vec<TradeResult>,
    pub parity_passive: Vec<TradeResult>,
}

impl ReplayDayReport {
    pub fn total_pnl_legacy(&self) -> f64 {
        self.legacy.iter().map(|t| t.pnl_pct()).sum()
    }
    pub fn total_pnl_parity_legacy(&self) -> f64 {
        self.parity_legacy.iter().map(|t| t.pnl_pct()).sum()
    }
    pub fn total_pnl_parity_passive(&self) -> f64 {
        self.parity_passive.iter().map(|t| t.pnl_pct()).sum()
    }
}

impl std::fmt::Display for ReplayDayReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n{}", "=".repeat(80))?;
        writeln!(
            f,
            "  Candle Replay 비교 — {} / {}",
            self.stock_code, self.date
        )?;
        writeln!(f, "{}", "=".repeat(80))?;
        writeln!(
            f,
            "  parity 입력 분해능: {}분봉{}",
            self.source_interval_used,
            if self.source_interval_used == 1 {
                ""
            } else {
                " (1분봉 부재로 fallback)"
            }
        )?;

        writeln!(
            f,
            "  {:<28} {:>8} {:>8} {:>12}",
            "경로 (simulation_mode)", "거래수", "승률%", "총손익%"
        )?;
        writeln!(f, "  {}", "-".repeat(60))?;

        for (label, trades) in [
            ("legacy_backtest", &self.legacy),
            ("parity_backtest_legacy", &self.parity_legacy),
            ("parity_backtest_passive", &self.parity_passive),
        ] {
            let total = trades.len();
            let wins = trades.iter().filter(|t| t.is_win()).count();
            let win_rate = if total > 0 {
                wins as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            let pnl: f64 = trades.iter().map(|t| t.pnl_pct()).sum();
            writeln!(
                f,
                "  {:<28} {:>8} {:>8.1} {:>12.2}",
                label, total, win_rate, pnl
            )?;
        }

        writeln!(f, "\n  --- 경로별 개별 거래 ---")?;
        for (label, trades) in [
            ("legacy", &self.legacy),
            ("parity/legacy", &self.parity_legacy),
            ("parity/passive", &self.parity_passive),
        ] {
            writeln!(f, "  [{label}]")?;
            if trades.is_empty() {
                writeln!(f, "    (거래 없음)")?;
                continue;
            }
            for t in trades {
                let sign = if t.is_win() { "+" } else { "-" };
                writeln!(
                    f,
                    "    {:?} {sign}{:.2}% (RR={:.2}) entry={} exit={} {}~{} {:?}",
                    t.side,
                    t.pnl_pct().abs(),
                    t.realized_rr(),
                    t.entry_price,
                    t.exit_price,
                    t.entry_time.format("%H:%M"),
                    t.exit_time.format("%H:%M"),
                    t.exit_reason,
                )?;
            }
        }
        writeln!(f, "{}", "=".repeat(80))
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use crate::strategy::types::{ExitReason, PositionSide};

    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn sample_trade(entry: i64, exit: i64) -> TradeResult {
        TradeResult {
            side: PositionSide::Long,
            entry_price: entry,
            exit_price: exit,
            stop_loss: entry - 100,
            take_profit: entry + 250,
            entry_time: t(9, 25),
            exit_time: t(10, 10),
            exit_reason: if exit > entry {
                ExitReason::TakeProfit
            } else {
                ExitReason::StopLoss
            },
            quantity: 0,
            intended_entry_price: entry,
            order_to_fill_ms: 0,
        }
    }

    #[test]
    fn total_pnl_per_path_sums_correctly() {
        let report = ReplayDayReport {
            date: NaiveDate::from_ymd_opt(2026, 4, 15).unwrap(),
            stock_code: "122630".into(),
            source_interval_used: 1,
            legacy: vec![sample_trade(10_000, 10_250)],
            parity_legacy: vec![sample_trade(10_000, 10_250)],
            parity_passive: vec![sample_trade(10_100, 9_900)],
        };
        assert!(report.total_pnl_legacy() > 0.0);
        assert!(report.total_pnl_parity_legacy() > 0.0);
        assert!(report.total_pnl_parity_passive() < 0.0);
    }

    #[test]
    fn display_contains_all_three_paths() {
        let report = ReplayDayReport {
            date: NaiveDate::from_ymd_opt(2026, 4, 15).unwrap(),
            stock_code: "122630".into(),
            source_interval_used: 5,
            legacy: Vec::new(),
            parity_legacy: Vec::new(),
            parity_passive: Vec::new(),
        };
        let s = format!("{report}");
        assert!(s.contains("legacy_backtest"));
        assert!(s.contains("parity_backtest_legacy"));
        assert!(s.contains("parity_backtest_passive"));
        assert!(s.contains("122630"));
        assert!(s.contains("fallback"));
    }
}
