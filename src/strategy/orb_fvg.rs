use chrono::NaiveTime;
use tracing::{debug, info};

use super::candle::{self, MinuteCandle};
use super::parity::position_manager::{
    PositionManagerConfig, gap_exit_reason, is_sl_hit, is_tp_hit, open_gap_breach, sl_exit_reason,
    time_stop_breached_by_candles, update_best_and_trailing,
};
use super::parity::signal_engine::{SignalEngineConfig, detect_next_fvg_signal};
use super::types::*;

/// ORB+FVG 전략 설정
#[derive(Debug, Clone)]
pub struct OrbFvgConfig {
    /// 손익비 (기본 2.0)
    pub rr_ratio: f64,
    /// OR 시작 시각
    pub or_start: NaiveTime,
    /// OR 종료 시각 (= 스캐닝 시작)
    pub or_end: NaiveTime,
    /// 신규 진입 마감 시각
    pub entry_cutoff: NaiveTime,
    /// 강제 청산 시각
    pub force_exit: NaiveTime,
    /// 시간 스탑: 진입 후 N캔들 내 1R 미달 시 청산 (기본 6캔들 = 30분)
    pub time_stop_candles: usize,
    /// 트레일링 스탑 간격 (R 배수, 기본 0.5R — best_price에서 이 거리만큼 SL 추적)
    pub trailing_r: f64,
    /// 본전 스탑 활성화 기준 (R 배수, 기본 1.0R — 이 수익 도달 시 SL→진입가)
    pub breakeven_r: f64,
    /// ATR 기반 최소 손절 배수 (기본 1.5 × ATR)
    pub atr_sl_multiplier: f64,
    /// FVG 유효시간 (캔들 수, 기본 4캔들 = 20분)
    pub fvg_expiry_candles: usize,
    /// OR 돌파 최소 강도 (기본 0.1%).
    pub min_or_breakout_pct: f64,
    /// 2차 진입 허용 최소 1차 수익률 (%, 기본 0.5%)
    pub min_first_pnl_for_second: f64,
    /// 일일 최대 누적 손실 한도 (%, 기본 -1.5%)
    pub max_daily_loss_pct: f64,
    /// 일일 최대 거래 횟수 (기본 5회)
    pub max_daily_trades: usize,
    /// Long 전용 모드 (true: Bearish FVG 무시, 실전 동일)
    pub long_only: bool,
}

impl Default for OrbFvgConfig {
    fn default() -> Self {
        Self {
            rr_ratio: 2.5,
            or_start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            or_end: NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            entry_cutoff: NaiveTime::from_hms_opt(15, 20, 0).unwrap(),
            force_exit: NaiveTime::from_hms_opt(15, 25, 0).unwrap(),
            time_stop_candles: 3,
            trailing_r: 0.05,
            breakeven_r: 0.15,
            atr_sl_multiplier: 1.5,
            fvg_expiry_candles: 6,
            min_or_breakout_pct: 0.001,
            min_first_pnl_for_second: 0.0,
            max_daily_loss_pct: -1.5,
            max_daily_trades: 5,
            long_only: true,
        }
    }
}

/// ORB+FVG 전략 엔진
pub struct OrbFvgStrategy {
    pub config: OrbFvgConfig,
}

impl OrbFvgStrategy {
    pub fn new(rr_ratio: f64) -> Self {
        Self {
            config: OrbFvgConfig {
                rr_ratio,
                ..Default::default()
            },
        }
    }

    /// 백테스트용: 하루 전체 캔들 입력 → 결과 목록 반환
    ///
    /// 최대 2회 진입:
    /// - 1차: OR 돌파 + FVG 동시 형성 → 리트레이스 진입
    /// - 1차 수익 시: 같은 방향 FVG만으로 2차 진입 (OR 돌파 조건 생략)
    /// - 1차 손실 시: 하루 종료
    pub fn run_day(&self, candles: &[MinuteCandle]) -> Vec<TradeResult> {
        let mut results = Vec::new();

        if candles.is_empty() {
            return results;
        }

        let is_5m_input =
            candles.len() >= 2 && (candles[1].time - candles[0].time).num_minutes() >= 4;

        // 1) OR 범위 확정
        let or_candles: Vec<_> = candles
            .iter()
            .filter(|c| c.time >= self.config.or_start && c.time < self.config.or_end)
            .cloned()
            .collect();

        if or_candles.is_empty() {
            debug!("OR 기간 캔들 없음");
            return results;
        }

        let or_high = or_candles.iter().map(|c| c.high).max().unwrap();
        let or_low = or_candles.iter().map(|c| c.low).min().unwrap();
        info!(
            "OR 범위: HIGH={or_high}, LOW={or_low} ({}개 캔들)",
            or_candles.len()
        );

        // 2) 5분봉 준비
        let scan_candles: Vec<_> = candles
            .iter()
            .filter(|c| c.time >= self.config.or_end)
            .cloned()
            .collect();

        let candles_5m = if is_5m_input {
            scan_candles
        } else {
            candle::aggregate(&scan_candles, 5)
        };
        if candles_5m.is_empty() {
            return results;
        }

        // 3) 실전 동일 루프: 거래 횟수 제한 + 누적 손실 한도 + 방향 관리
        let mut trade_count = 0usize;
        let mut cumulative_pnl = 0.0f64;
        let mut confirmed_side: Option<PositionSide> = None;
        let mut search_from = 0usize; // 다음 탐색 시작 인덱스

        loop {
            // 거래 횟수 한도
            if trade_count >= self.config.max_daily_trades {
                info!("최대 거래 횟수 도달 ({}회)", self.config.max_daily_trades);
                break;
            }

            // 누적 손실 한도
            if cumulative_pnl <= self.config.max_daily_loss_pct {
                info!("일일 손실 한도 도달 ({:.2}%)", cumulative_pnl);
                break;
            }

            // 남은 캔들에서 신호 탐색
            if search_from >= candles_5m.len() {
                break;
            }
            let scan_slice = &candles_5m[search_from..];

            // 2026-04-17 v3 불변식 #1b: require_or_breakout 항상 true (라이브 일치).
            let require_or = true;
            let _ = trade_count; // 더 이상 OR 가드 분기에 사용하지 않음 (호환성 유지)
            let (trade_result, exit_idx_rel, _) =
                self.scan_and_trade(scan_slice, or_high, or_low, require_or);

            match trade_result {
                Some(trade) => {
                    // 방향 확인 (confirmed_side가 있으면 같은 방향만 허용)
                    if let Some(cs) = confirmed_side
                        && trade.side != cs
                    {
                        // 방향 불일치 → 이 거래는 무시하고 탐색 계속
                        search_from += exit_idx_rel.max(1);
                        continue;
                    }

                    let pnl = trade.pnl_pct();
                    trade_count += 1;
                    cumulative_pnl += pnl;

                    info!(
                        "{}차 거래: {:?} {:.2}% (누적 {:.2}%)",
                        trade_count, trade.side, pnl, cumulative_pnl
                    );

                    // 방향 관리: 수익이면 같은 방향 유지, 손실이면 리셋
                    confirmed_side = if pnl > 0.0 { Some(trade.side) } else { None };

                    results.push(trade);
                    search_from += exit_idx_rel;
                }
                None => break, // 더 이상 신호 없음
            }
        }

        results
    }

    /// FVG 탐색 → 진입 → 청산까지 실행
    ///
    /// `require_or_breakout`: true면 OR 돌파+FVG 동시 감지, false면 FVG만
    /// 반환: (거래 결과, 청산 시점 캔들 인덱스, 확인된 방향)
    pub(crate) fn scan_and_trade(
        &self,
        candles_5m: &[MinuteCandle],
        or_high: i64,
        or_low: i64,
        require_or_breakout: bool,
    ) -> (Option<TradeResult>, usize, Option<PositionSide>) {
        // Phase 2: FVG 탐지는 parity::signal_engine 공통 pure function 사용.
        // 진입가 산정(mid_price)과 청산 시뮬레이션은 이 함수에 그대로 남긴다.
        let cfg = SignalEngineConfig {
            strategy_id: "orb_fvg".into(),
            stage_name: "legacy_single_stage".into(),
            or_high,
            or_low,
            fvg_expiry_candles: self.config.fvg_expiry_candles,
            entry_cutoff: self.config.entry_cutoff,
            require_or_breakout,
            min_or_breakout_pct: self.config.min_or_breakout_pct,
            long_only: self.config.long_only,
            confirmed_side: None, // 방향 필터는 run_day 가 상위에서 처리
        };
        let detected = match detect_next_fvg_signal(candles_5m, &cfg) {
            Some(d) => d,
            None => return (None, candles_5m.len(), None),
        };

        let side = detected.intent.side;
        let gap = &detected.gap;
        let idx = detected.retrace_candle_idx;
        let c5 = &candles_5m[idx];
        let label = if require_or_breakout {
            match side {
                PositionSide::Long => "OR HIGH 돌파 + ",
                PositionSide::Short => "OR LOW 돌파 + ",
            }
        } else {
            "2차 "
        };
        info!(
            "{}: {label}{:?} FVG — 갭 [{}, {}], SL={}",
            detected.intent.metadata.b_time, side, gap.bottom, gap.top, gap.stop_loss
        );

        // legacy 진입 가격: gap.mid_price()
        let entry_price = gap.mid_price();
        let stop_loss = gap.stop_loss;
        let risk = (entry_price - stop_loss).abs();
        let take_profit = match side {
            PositionSide::Long => entry_price + (risk as f64 * self.config.rr_ratio) as i64,
            PositionSide::Short => entry_price - (risk as f64 * self.config.rr_ratio) as i64,
        };

        info!(
            "{}: {:?} 진입 — entry={}, SL={}, TP={} (RR=1:{:.1})",
            c5.time, side, entry_price, stop_loss, take_profit, self.config.rr_ratio
        );

        let result = self.simulate_exit(
            side,
            entry_price,
            stop_loss,
            take_profit,
            c5.time,
            &candles_5m[idx + 1..],
        );

        let exit_candle_idx = if let Some(ref r) = result {
            candles_5m
                .iter()
                .position(|c| c.time >= r.exit_time)
                .unwrap_or(candles_5m.len())
        } else {
            candles_5m.len()
        };

        (result, exit_candle_idx, Some(side))
    }

    /// 진입 후 청산 시뮬레이션 (TP 지정가 우선)
    ///
    /// TP 지정가는 KRX에 걸려있어 가격 도달 즉시 체결 (레이턴시 0)
    /// SL/트레일링은 서버 3초 폴링 (레이턴시 있음)
    ///
    /// 캔들별 청산 판정:
    /// 1. 시가가 이미 SL 돌파 → 갭 손절 (TP 체크 불필요)
    /// 2. TP 도달 → 지정가 체결 (거래소 우선)
    /// 3. best_price 갱신 + 1R 도달 + 트레일링 SL
    /// 4. SL 도달 → 서버 측 시장가 청산
    /// 5. 시간 스탑 / 장마감
    fn simulate_exit(
        &self,
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
        entry_time: NaiveTime,
        remaining: &[MinuteCandle],
    ) -> Option<TradeResult> {
        let original_risk = (entry_price - stop_loss).abs();
        let pm_cfg = PositionManagerConfig {
            rr_ratio: self.config.rr_ratio,
            trailing_r: self.config.trailing_r,
            breakeven_r: self.config.breakeven_r,
            time_stop_candles: self.config.time_stop_candles,
            candle_interval_min: 5,
        };
        let mut current_sl = stop_loss;
        let mut best_price = entry_price;
        let mut reached_1r = false;

        for (i, c) in remaining.iter().enumerate() {
            // ── Step 0: 시가에서 이미 SL 돌파 (갭 손절) ──
            if open_gap_breach(side, current_sl, c.open) {
                let reason = gap_exit_reason(reached_1r);
                info!(
                    "{}: 시가 갭 {:?} — open={}, SL={}",
                    c.time, reason, c.open, current_sl
                );
                return Some(TradeResult {
                    side,
                    entry_price,
                    exit_price: c.open,
                    stop_loss,
                    take_profit,
                    entry_time,
                    exit_time: c.time,
                    exit_reason: reason,
                    quantity: 0,
                    intended_entry_price: entry_price,
                    order_to_fill_ms: 0,
                });
            }

            // ── Step 1: TP 지정가 체결 (거래소 우선) ──
            let tp_extreme = match side {
                PositionSide::Long => c.high,
                PositionSide::Short => c.low,
            };
            if is_tp_hit(side, take_profit, tp_extreme) {
                info!("{}: TP 지정가 체결 — TP={}", c.time, take_profit);
                return Some(TradeResult {
                    side,
                    entry_price,
                    exit_price: take_profit,
                    stop_loss,
                    take_profit,
                    entry_time,
                    exit_time: c.time,
                    exit_reason: ExitReason::TakeProfit,
                    quantity: 0,
                    intended_entry_price: entry_price,
                    order_to_fill_ms: 0,
                });
            }

            // ── Step 2: best_price 갱신 + 1R 도달 + 트레일링 ──
            let favorable = match side {
                PositionSide::Long => c.high,
                PositionSide::Short => c.low,
            };
            let update = update_best_and_trailing(
                side,
                entry_price,
                &mut best_price,
                &mut current_sl,
                &mut reached_1r,
                original_risk,
                favorable,
                &pm_cfg,
            );
            if update.reached_1r_newly {
                info!(
                    "{}: 1R 도달 — 본전 스탑 활성화 (SL → {})",
                    c.time, current_sl
                );
            } else if update.trailing_changed {
                debug!("{}: 트레일링 SL 갱신 → {}", c.time, current_sl);
            }

            // ── Step 3: SL 체크 (갱신된 current_sl 기준) ──
            let sl_extreme = match side {
                PositionSide::Long => c.low,
                PositionSide::Short => c.high,
            };
            if is_sl_hit(side, current_sl, sl_extreme) {
                let reason = sl_exit_reason(side, current_sl, stop_loss, entry_price, reached_1r);
                info!("{}: {:?} — SL={}", c.time, reason, current_sl);
                return Some(TradeResult {
                    side,
                    entry_price,
                    exit_price: current_sl,
                    stop_loss,
                    take_profit,
                    entry_time,
                    exit_time: c.time,
                    exit_reason: reason,
                    quantity: 0,
                    intended_entry_price: entry_price,
                    order_to_fill_ms: 0,
                });
            }

            // ── Step 4: 시간 스탑 ──
            if time_stop_breached_by_candles(i + 1, self.config.time_stop_candles, reached_1r) {
                info!(
                    "{}: 시간 스탑 — {}캔들 경과, 1R 미달 (close={})",
                    c.time, self.config.time_stop_candles, c.close
                );
                return Some(TradeResult {
                    side,
                    entry_price,
                    exit_price: c.close,
                    stop_loss,
                    take_profit,
                    entry_time,
                    exit_time: c.time,
                    exit_reason: ExitReason::TimeStop,
                    quantity: 0,
                    intended_entry_price: entry_price,
                    order_to_fill_ms: 0,
                });
            }

            // ── Step 5: 장마감 강제 청산 ──
            if c.time >= self.config.force_exit {
                info!("{}: 장마감 청산 — close={}", c.time, c.close);
                return Some(TradeResult {
                    side,
                    entry_price,
                    exit_price: c.close,
                    stop_loss,
                    take_profit,
                    entry_time,
                    exit_time: c.time,
                    exit_reason: ExitReason::EndOfDay,
                    quantity: 0,
                    intended_entry_price: entry_price,
                    order_to_fill_ms: 0,
                });
            }
        }

        remaining.last().map(|last| TradeResult {
            side,
            entry_price,
            exit_price: last.close,
            stop_loss,
            take_profit,
            entry_time,
            exit_time: last.time,
            exit_reason: ExitReason::EndOfDay,
            quantity: 0,
            intended_entry_price: entry_price,
            order_to_fill_ms: 0,
        })
    }

    /// 실시간용: 현재까지의 5분봉으로 전략 상태 평가
    /// 반환: (진입 신호, OR 범위)
    pub fn evaluate_candles(
        &self,
        candles_1m: &[MinuteCandle],
    ) -> (Option<Signal>, Option<(i64, i64)>) {
        if candles_1m.is_empty() {
            return (None, None);
        }

        // OR 범위
        let or_candles: Vec<_> = candles_1m
            .iter()
            .filter(|c| c.time >= self.config.or_start && c.time < self.config.or_end)
            .cloned()
            .collect();

        if or_candles.is_empty() {
            return (None, None);
        }

        let or_15m = candle::aggregate(&or_candles, 15);
        let or_high = or_15m.iter().map(|c| c.high).max().unwrap();
        let or_low = or_15m.iter().map(|c| c.low).min().unwrap();

        // 스캐닝: 돌파 + FVG 동시 감지 → 리트레이스 진입
        let scan_candles: Vec<_> = candles_1m
            .iter()
            .filter(|c| c.time >= self.config.or_end)
            .cloned()
            .collect();
        let candles_5m = candle::aggregate(&scan_candles, 5);

        let cfg = SignalEngineConfig {
            strategy_id: "orb_fvg".into(),
            stage_name: "legacy_single_stage".into(),
            or_high,
            or_low,
            fvg_expiry_candles: self.config.fvg_expiry_candles,
            entry_cutoff: self.config.entry_cutoff,
            require_or_breakout: true,
            min_or_breakout_pct: self.config.min_or_breakout_pct,
            long_only: self.config.long_only,
            confirmed_side: None,
        };
        if let Some(detected) = detect_next_fvg_signal(&candles_5m, &cfg) {
            let side = detected.intent.side;
            let gap = detected.gap;
            let entry_price = gap.mid_price();
            let stop_loss = gap.stop_loss;
            let risk = (entry_price - stop_loss).abs();
            let take_profit = match side {
                PositionSide::Long => entry_price + (risk as f64 * self.config.rr_ratio) as i64,
                PositionSide::Short => entry_price - (risk as f64 * self.config.rr_ratio) as i64,
            };

            return (
                Some(Signal {
                    side,
                    entry_price,
                    stop_loss,
                    take_profit,
                    signal_time: detected.intent.signal_time,
                }),
                Some((or_high, or_low)),
            );
        }

        (None, Some((or_high, or_low)))
    }

    /// 현재가 기준 SL/TP 체크
    pub fn check_exit(&self, position: &Position, current_price: i64) -> Option<ExitReason> {
        match position.side {
            PositionSide::Long => {
                if current_price <= position.stop_loss {
                    Some(ExitReason::StopLoss)
                } else if current_price >= position.take_profit {
                    Some(ExitReason::TakeProfit)
                } else {
                    None
                }
            }
            PositionSide::Short => {
                if current_price >= position.stop_loss {
                    Some(ExitReason::StopLoss)
                } else if current_price <= position.take_profit {
                    Some(ExitReason::TakeProfit)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn mc(hour: u32, min: u32, open: i64, high: i64, low: i64, close: i64) -> MinuteCandle {
        let total_min = hour * 60 + min;
        MinuteCandle {
            date: NaiveDate::from_ymd_opt(2026, 4, 8).unwrap(),
            time: NaiveTime::from_hms_opt(total_min / 60, total_min % 60, 0).unwrap(),
            open,
            high,
            low,
            close,
            volume: 1000,
        }
    }

    fn build_or_candles(or_high: i64, or_low: i64) -> Vec<MinuteCandle> {
        // 09:00~09:14 범위에서 OR HIGH/LOW를 만들어주는 캔들
        let mut candles = Vec::new();
        for m in 0..15 {
            let (o, h, l, c) = if m == 0 {
                (or_low, or_high, or_low, (or_high + or_low) / 2)
            } else {
                let mid = (or_high + or_low) / 2;
                (mid, mid + 10, mid - 10, mid)
            };
            candles.push(mc(9, m, o, h, l, c));
        }
        candles
    }

    #[test]
    fn test_run_day_long_tp() {
        let strategy = OrbFvgStrategy::new(2.0);
        let mut candles = build_or_candles(10000, 9800); // OR: 9800~10000

        // 09:15 돌파 양봉: close > OR HIGH
        candles.extend((0..5).map(|m| mc(9, 15 + m, 9950, 10100, 9940, 10050)));

        // 09:20~09:24: FVG 형성 캔들 A
        candles.extend((0..5).map(|m| mc(9, 20 + m, 10050, 10060, 10040, 10055)));
        // 09:25~09:29: 큰 양봉 B
        candles.extend((0..5).map(|m| mc(9, 25 + m, 10055, 10200, 10050, 10180)));
        // 09:30~09:34: C (low > A.high = 10060 → FVG!)
        candles.extend((0..5).map(|m| mc(9, 30 + m, 10180, 10200, 10080, 10150)));

        // 09:35~09:39: 리트레이스 (low가 FVG 영역 [10060, 10080] 진입)
        candles.extend((0..5).map(|m| mc(9, 35 + m, 10150, 10160, 10065, 10100)));

        // 09:40~ : 상승 → TP 도달
        for i in 0..20 {
            candles.extend((0..5).map(|m| {
                let base = 10100 + (i * 30) as i64;
                mc(9, 40 + i * 5 + m, base, base + 50, base - 10, base + 40)
            }));
        }

        let results = strategy.run_day(&candles);
        assert!(!results.is_empty());
        assert_eq!(results[0].side, PositionSide::Long);
        assert!(results[0].is_win());
    }

    #[test]
    fn test_run_day_no_breakout() {
        let strategy = OrbFvgStrategy::new(2.0);
        let mut candles = build_or_candles(10000, 9800);

        // 09:15 이후 OR 범위 내에서 횡보 → 돌파 없음
        for i in 0..60 {
            candles.push(mc(9, 15 + i, 9900, 9950, 9850, 9920));
        }

        let results = strategy.run_day(&candles);
        assert!(results.is_empty());
    }

    #[test]
    fn test_check_exit_sl() {
        let strategy = OrbFvgStrategy::new(2.0);
        let pos = Position {
            side: PositionSide::Long,
            entry_price: 10000,
            stop_loss: 9900,
            take_profit: 10200,
            entry_time: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            quantity: 1,
            tp_order_no: None,
            tp_krx_orgno: None,
            tp_limit_price: None,
            reached_1r: false,
            best_price: 10000,
            original_sl: 0,
            sl_triggered_tick: false,
            intended_entry_price: 0,
            order_to_fill_ms: 0,
            signal_id: String::new(),
        };

        assert_eq!(strategy.check_exit(&pos, 10100), None);
        assert_eq!(strategy.check_exit(&pos, 9900), Some(ExitReason::StopLoss));
        assert_eq!(
            strategy.check_exit(&pos, 10200),
            Some(ExitReason::TakeProfit)
        );
    }

    #[test]
    fn test_check_exit_short() {
        let strategy = OrbFvgStrategy::new(2.0);
        let pos = Position {
            side: PositionSide::Short,
            entry_price: 10000,
            stop_loss: 10100,
            take_profit: 9800,
            entry_time: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            quantity: 1,
            tp_order_no: None,
            tp_krx_orgno: None,
            tp_limit_price: None,
            reached_1r: false,
            best_price: 10000,
            original_sl: 0,
            sl_triggered_tick: false,
            intended_entry_price: 0,
            order_to_fill_ms: 0,
            signal_id: String::new(),
        };

        assert_eq!(strategy.check_exit(&pos, 9900), None);
        assert_eq!(strategy.check_exit(&pos, 10100), Some(ExitReason::StopLoss));
        assert_eq!(
            strategy.check_exit(&pos, 9800),
            Some(ExitReason::TakeProfit)
        );
    }
}
