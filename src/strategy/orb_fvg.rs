use chrono::NaiveTime;
use tracing::{debug, info};

use super::candle::{self, MinuteCandle};
use super::fvg::{FairValueGap, FvgDirection};
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

        let is_5m_input = candles.len() >= 2
            && (candles[1].time - candles[0].time).num_minutes() >= 4;

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
        info!("OR 범위: HIGH={or_high}, LOW={or_low} ({}개 캔들)", or_candles.len());

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

            // 1차는 OR 돌파 필수, 이후는 FVG만
            let require_or = trade_count == 0;
            let (trade_result, exit_idx_rel, new_side) =
                self.scan_and_trade(scan_slice, or_high, or_low, require_or);

            match trade_result {
                Some(trade) => {
                    // 방향 확인 (confirmed_side가 있으면 같은 방향만 허용)
                    if let Some(cs) = confirmed_side {
                        if trade.side != cs {
                            // 방향 불일치 → 이 거래는 무시하고 탐색 계속
                            search_from += exit_idx_rel.max(1);
                            continue;
                        }
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
        let mut pending_fvg: Option<FairValueGap> = None;
        let mut breakout_side: Option<PositionSide> = None;
        let mut fvg_formed_idx: usize = 0;

        for (idx, c5) in candles_5m.iter().enumerate() {
            if c5.time >= self.config.entry_cutoff {
                break;
            }

            // FVG 유효시간 체크
            if pending_fvg.is_some() && idx - fvg_formed_idx > self.config.fvg_expiry_candles {
                info!(
                    "{}: FVG 유효시간 초과 ({}캔들) — 폐기, 재탐색",
                    c5.time, self.config.fvg_expiry_candles
                );
                pending_fvg = None;
                breakout_side = None;
            }

            // 1단계: FVG 감지
            if pending_fvg.is_none() && idx >= 2 {
                let a = &candles_5m[idx - 2];
                let b = &candles_5m[idx - 1];
                let c = c5;

                let a_range = a.range().max(1);

                // Bullish FVG
                if b.is_bullish() && a.high < c.low && b.body_size() * 100 >= a_range * 30 {
                    let or_ok = !require_or_breakout || b.close > or_high;
                    if or_ok {
                        let label = if require_or_breakout { "OR HIGH 돌파 + " } else { "2차 " };
                        let gap = FairValueGap {
                            direction: FvgDirection::Bullish,
                            top: c.low, bottom: a.high,
                            candle_b_idx: idx - 1, stop_loss: a.low,
                        };
                        info!("{}: {label}Bullish FVG — 갭 [{}, {}], SL={}",
                            b.time, gap.bottom, gap.top, gap.stop_loss);
                        pending_fvg = Some(gap);
                        breakout_side = Some(PositionSide::Long);
                        fvg_formed_idx = idx;
                        continue;
                    }
                }

                // Bearish FVG (long_only=true면 무시 — 실전 동일: 공매도 불가)
                if !self.config.long_only
                    && b.is_bearish() && a.low > c.high && b.body_size() * 100 >= a_range * 30
                {
                    let or_ok = !require_or_breakout || b.close < or_low;
                    if or_ok {
                        let label = if require_or_breakout { "OR LOW 돌파 + " } else { "2차 " };
                        let gap = FairValueGap {
                            direction: FvgDirection::Bearish,
                            top: a.low, bottom: c.high,
                            candle_b_idx: idx - 1, stop_loss: a.high,
                        };
                        info!("{}: {label}Bearish FVG — 갭 [{}, {}], SL={}",
                            b.time, gap.bottom, gap.top, gap.stop_loss);
                        pending_fvg = Some(gap);
                        breakout_side = Some(PositionSide::Short);
                        fvg_formed_idx = idx;
                        continue;
                    }
                }
            }

            // 2단계: FVG 리트레이스 진입
            if let Some(ref gap) = pending_fvg {
                let side = breakout_side.unwrap();
                let retrace_price = match side {
                    PositionSide::Long => c5.low,
                    PositionSide::Short => c5.high,
                };

                if gap.contains_price(retrace_price) {
                    let entry_price = gap.mid_price();
                    let stop_loss = gap.stop_loss;
                    let risk = (entry_price - stop_loss).abs();
                    let take_profit = match side {
                        PositionSide::Long => entry_price + (risk as f64 * self.config.rr_ratio) as i64,
                        PositionSide::Short => entry_price - (risk as f64 * self.config.rr_ratio) as i64,
                    };

                    info!("{}: {:?} 진입 — entry={}, SL={}, TP={} (RR=1:{:.1})",
                        c5.time, side, entry_price, stop_loss, take_profit, self.config.rr_ratio);

                    let result = self.simulate_exit(
                        side, entry_price, stop_loss, take_profit,
                        c5.time, &candles_5m[idx + 1..],
                    );

                    let exit_candle_idx = if let Some(ref r) = result {
                        candles_5m.iter().position(|c| c.time >= r.exit_time)
                            .unwrap_or(candles_5m.len())
                    } else {
                        candles_5m.len()
                    };

                    return (result, exit_candle_idx, Some(side));
                }
            }
        }

        (None, candles_5m.len(), None)
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
        let risk = (entry_price - stop_loss).abs() as f64;
        let trailing_dist = (risk * self.config.trailing_r) as i64;
        let mut current_sl = stop_loss;
        let mut best_price = entry_price;
        let mut reached_1r = false;

        for (i, c) in remaining.iter().enumerate() {
            // ── Step 0: 시가에서 이미 SL 돌파 (갭 손절) ──
            let open_sl_breach = match side {
                PositionSide::Long => c.open <= current_sl,
                PositionSide::Short => c.open >= current_sl,
            };
            if open_sl_breach {
                let reason = if reached_1r {
                    ExitReason::TrailingStop
                } else {
                    ExitReason::StopLoss
                };
                info!("{}: 시가 갭 {:?} — open={}, SL={}", c.time, reason, c.open, current_sl);
                return Some(TradeResult {
                    side, entry_price, exit_price: c.open, stop_loss, take_profit,
                    entry_time, exit_time: c.time, exit_reason: reason,
                    quantity: 0,
                });
            }

            // ── Step 1: TP 지정가 체결 (거래소 우선) ──
            let tp_hit = match side {
                PositionSide::Long => c.high >= take_profit,
                PositionSide::Short => c.low <= take_profit,
            };
            if tp_hit {
                info!("{}: TP 지정가 체결 — TP={}", c.time, take_profit);
                return Some(TradeResult {
                    side, entry_price, exit_price: take_profit, stop_loss, take_profit,
                    entry_time, exit_time: c.time, exit_reason: ExitReason::TakeProfit,
                    quantity: 0,
                });
            }

            // ── Step 2: best_price 갱신 + 1R 도달 + 트레일링 ──
            let favorable = match side {
                PositionSide::Long => c.high,
                PositionSide::Short => c.low,
            };
            best_price = match side {
                PositionSide::Long => best_price.max(favorable),
                PositionSide::Short => best_price.min(favorable),
            };

            let profit_from_entry = match side {
                PositionSide::Long => best_price - entry_price,
                PositionSide::Short => entry_price - best_price,
            };
            let breakeven_dist = (risk * self.config.breakeven_r) as i64;
            if !reached_1r && profit_from_entry >= breakeven_dist {
                reached_1r = true;
                current_sl = entry_price;
                info!("{}: 1R 도달 — 본전 스탑 활성화 (SL → {})", c.time, current_sl);
            }

            if reached_1r {
                let new_sl = match side {
                    PositionSide::Long => best_price - trailing_dist,
                    PositionSide::Short => best_price + trailing_dist,
                };
                let improved = match side {
                    PositionSide::Long => new_sl > current_sl,
                    PositionSide::Short => new_sl < current_sl,
                };
                if improved {
                    current_sl = new_sl;
                    debug!("{}: 트레일링 SL 갱신 → {}", c.time, current_sl);
                }
            }

            // ── Step 3: SL 체크 ──
            let sl_hit = match side {
                PositionSide::Long => c.low <= current_sl,
                PositionSide::Short => c.high >= current_sl,
            };
            if sl_hit {
                let reason = if reached_1r && current_sl > stop_loss {
                    if current_sl == entry_price {
                        ExitReason::BreakevenStop
                    } else {
                        ExitReason::TrailingStop
                    }
                } else {
                    ExitReason::StopLoss
                };
                info!("{}: {:?} — SL={}", c.time, reason, current_sl);
                return Some(TradeResult {
                    side, entry_price, exit_price: current_sl, stop_loss, take_profit,
                    entry_time, exit_time: c.time, exit_reason: reason,
                    quantity: 0,
                });
            }

            // ── Step 4: 시간 스탑 ──
            if !reached_1r && i + 1 >= self.config.time_stop_candles {
                info!("{}: 시간 스탑 — {}캔들 경과, 1R 미달 (close={})",
                    c.time, self.config.time_stop_candles, c.close);
                return Some(TradeResult {
                    side, entry_price, exit_price: c.close, stop_loss, take_profit,
                    entry_time, exit_time: c.time, exit_reason: ExitReason::TimeStop,
                    quantity: 0,
                });
            }

            // ── Step 5: 장마감 강제 청산 ──
            if c.time >= self.config.force_exit {
                info!("{}: 장마감 청산 — close={}", c.time, c.close);
                return Some(TradeResult {
                    side, entry_price, exit_price: c.close, stop_loss, take_profit,
                    entry_time, exit_time: c.time, exit_reason: ExitReason::EndOfDay,
                    quantity: 0,
                });
            }
        }

        remaining.last().map(|last| TradeResult {
            side, entry_price, exit_price: last.close, stop_loss, take_profit,
            entry_time, exit_time: last.time, exit_reason: ExitReason::EndOfDay,
            quantity: 0,
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

        let mut breakout_side: Option<PositionSide> = None;
        let mut pending_fvg: Option<FairValueGap> = None;

        for (idx, c5) in candles_5m.iter().enumerate() {
            if c5.time >= self.config.entry_cutoff {
                break;
            }

            // 1단계: 돌파 + FVG 동시 감지
            if pending_fvg.is_none() && idx >= 2 {
                let a = &candles_5m[idx - 2];
                let b = &candles_5m[idx - 1];
                let c = c5;
                let a_range = a.range().max(1);

                if b.is_bullish() && b.close > or_high && a.high < c.low && b.body_size() * 100 >= a_range * 30 {
                    pending_fvg = Some(FairValueGap {
                        direction: FvgDirection::Bullish,
                        top: c.low,
                        bottom: a.high,
                        candle_b_idx: idx - 1,
                        stop_loss: a.low,
                    });
                    breakout_side = Some(PositionSide::Long);
                    continue;
                }

                if b.is_bearish() && b.close < or_low && a.low > c.high && b.body_size() * 100 >= a_range * 30 {
                    pending_fvg = Some(FairValueGap {
                        direction: FvgDirection::Bearish,
                        top: a.low,
                        bottom: c.high,
                        candle_b_idx: idx - 1,
                        stop_loss: a.high,
                    });
                    breakout_side = Some(PositionSide::Short);
                    continue;
                }
            }

            // 2단계: FVG 리트레이스 진입
            if let Some(ref gap) = pending_fvg {
                let side = breakout_side.unwrap();
                let retrace_price = match side {
                    PositionSide::Long => c5.low,
                    PositionSide::Short => c5.high,
                };

                if gap.contains_price(retrace_price) {
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
                            signal_time: c5.time,
                        }),
                        Some((or_high, or_low)),
                    );
                }
            }
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
        };

        assert_eq!(strategy.check_exit(&pos, 10100), None);
        assert_eq!(strategy.check_exit(&pos, 9900), Some(ExitReason::StopLoss));
        assert_eq!(strategy.check_exit(&pos, 10200), Some(ExitReason::TakeProfit));
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
        };

        assert_eq!(strategy.check_exit(&pos, 9900), None);
        assert_eq!(strategy.check_exit(&pos, 10100), Some(ExitReason::StopLoss));
        assert_eq!(strategy.check_exit(&pos, 9800), Some(ExitReason::TakeProfit));
    }
}
