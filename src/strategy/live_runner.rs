use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Local, NaiveTime};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::domain::error::KisError;
use crate::domain::models::order::{OrderSide, OrderType, OrderRequest};
use crate::domain::models::price::InquirePrice;
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::{StockCode, TransactionId};
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};

use super::candle::{self, MinuteCandle};
use super::fvg::{FairValueGap, FvgDirection};
use super::orb_fvg::{OrbFvgConfig, OrbFvgStrategy};
use super::types::*;

/// KIS 분봉 응답 항목
#[derive(Debug, Clone, Deserialize)]
struct MinutePriceItem {
    stck_bsop_date: String,
    stck_cntg_hour: String,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_oprc: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_hgpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_lwpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_prpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    cntg_vol: i64,
}

/// 실시간 러너 공유 상태 (웹에서 읽기 가능)
#[derive(Debug, Clone)]
pub struct RunnerState {
    pub phase: String,
    pub today_trades: Vec<TradeResult>,
    pub today_pnl: f64,
    pub current_position: Option<Position>,
}

/// 실시간 트레이딩 러너
pub struct LiveRunner {
    client: Arc<KisHttpClient>,
    strategy: OrbFvgStrategy,
    stock_code: StockCode,
    stock_name: String,
    quantity: u64,
    /// 외부에서 중지 요청
    stop_flag: Arc<AtomicBool>,
    /// 공유 상태
    pub state: Arc<RwLock<RunnerState>>,
}

impl LiveRunner {
    pub fn new(
        client: Arc<KisHttpClient>,
        stock_code: StockCode,
        stock_name: String,
        quantity: u64,
        stop_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            client,
            strategy: OrbFvgStrategy { config: OrbFvgConfig::default() },
            stock_code,
            stock_name,
            quantity,
            stop_flag,
            state: Arc::new(RwLock::new(RunnerState {
                phase: "초기화".to_string(),
                today_trades: Vec::new(),
                today_pnl: 0.0,
                current_position: None,
            })),
        }
    }

    /// 장중 실행 루프 (토글 OFF 시 중단 가능)
    pub async fn run(&self) -> Result<Vec<TradeResult>, KisError> {
        let cfg = &self.strategy.config;

        info!("=== {} ({}) 자동매매 시작 ===", self.stock_name, self.stock_code);
        info!("수량: {}주, RR: 1:{:.1}, 트레일링: {:.1}R, 본전: {:.1}R",
            self.quantity, cfg.rr_ratio, cfg.trailing_r, cfg.breakeven_r);

        // 장 시작 대기
        self.update_phase("장 시작 대기").await;
        self.wait_until(cfg.or_start).await;
        if self.is_stopped() { return Ok(Vec::new()); }

        self.update_phase("OR 수집 중").await;
        self.wait_until(cfg.or_end).await;
        if self.is_stopped() { return Ok(Vec::new()); }

        // 메인 루프: 분봉 폴링 → 전략 평가 → 주문 실행
        let mut all_trades: Vec<TradeResult> = Vec::new();
        let mut trade_count = 0;
        let max_trades = 2;
        let mut confirmed_side: Option<PositionSide> = None;

        self.update_phase("신호 탐색").await;

        loop {
            if self.is_stopped() {
                info!("{}: 외부 중지 요청", self.stock_name);
                // 포지션 보유 중이면 청산
                let state = self.state.read().await;
                if let Some(ref pos) = state.current_position {
                    drop(state);
                    if let Ok(result) = self.close_position_market(ExitReason::EndOfDay).await {
                        all_trades.push(result);
                    }
                }
                break;
            }

            let now = Local::now().time();

            // 강제 청산 시각
            if now >= cfg.force_exit {
                let state = self.state.read().await;
                if state.current_position.is_some() {
                    drop(state);
                    info!("{}: 장마감 강제 청산", self.stock_name);
                    if let Ok(result) = self.close_position_market(ExitReason::EndOfDay).await {
                        all_trades.push(result);
                    }
                }
                break;
            }

            // 진입 마감
            if now >= cfg.entry_cutoff {
                let state = self.state.read().await;
                if state.current_position.is_none() {
                    info!("{}: 진입 마감 (15:20)", self.stock_name);
                    break;
                }
                drop(state);
            }

            // 포지션 보유 중: 실시간 청산 관리
            let has_position = self.state.read().await.current_position.is_some();
            if has_position {
                match self.manage_position().await {
                    Ok(Some(result)) => {
                        let is_win = result.is_win();
                        let pnl = result.pnl_pct();
                        let side = result.side;
                        all_trades.push(result);
                        trade_count += 1;
                        self.update_pnl(&all_trades).await;

                        if trade_count >= max_trades {
                            info!("{}: 최대 거래 횟수 도달 ({}회)", self.stock_name, max_trades);
                            break;
                        }

                        if !is_win {
                            info!("{}: 1차 손실 — 하루 종료", self.stock_name);
                            break;
                        }

                        if pnl < self.strategy.config.min_first_pnl_for_second && trade_count == 1 {
                            info!("{}: 1차 수익 {:.2}% — 부족, 2차 진입 안 함", self.stock_name, pnl);
                            break;
                        }

                        confirmed_side = Some(side);
                        self.update_phase("2차 신호 탐색").await;
                    }
                    Ok(None) => {} // 유지
                    Err(e) => {
                        warn!("{}: 포지션 관리 실패: {e}", self.stock_name);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }

            // 분봉 폴링 → 신호 탐색 → 진입
            match self.poll_and_enter(trade_count == 0, confirmed_side).await {
                Ok(true) => {
                    self.update_phase("포지션 보유").await;
                }
                Ok(false) => {} // 신호 없음
                Err(e) => {
                    warn!("{}: 폴링 실패: {e}", self.stock_name);
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }

        self.update_phase("종료").await;
        self.update_pnl(&all_trades).await;

        for (i, t) in all_trades.iter().enumerate() {
            info!(
                "{}: {}차 거래 {:?} {:.2}% ({:?})",
                self.stock_name, i + 1, t.side, t.pnl_pct(), t.exit_reason
            );
        }

        Ok(all_trades)
    }

    /// 분봉 폴링 → 전략 평가 → 진입
    async fn poll_and_enter(
        &self,
        require_or_breakout: bool,
        confirmed_side: Option<PositionSide>,
    ) -> Result<bool, KisError> {
        let candles = self.fetch_today_minute_candles().await?;
        if candles.is_empty() {
            return Ok(false);
        }

        // OR 범위
        let cfg = &self.strategy.config;
        let or_candles: Vec<_> = candles.iter()
            .filter(|c| c.time >= cfg.or_start && c.time < cfg.or_end)
            .collect();

        if or_candles.is_empty() {
            return Ok(false);
        }

        let or_high = or_candles.iter().map(|c| c.high).max().unwrap();
        let or_low = or_candles.iter().map(|c| c.low).min().unwrap();

        // 5분봉 집계
        let scan: Vec<_> = candles.iter()
            .filter(|c| c.time >= cfg.or_end)
            .cloned()
            .collect();
        let candles_5m = candle::aggregate(&scan, 5);

        // FVG 탐색
        let mut pending_fvg: Option<FairValueGap> = None;
        let mut fvg_side: Option<PositionSide> = None;
        let mut fvg_formed_idx: usize = 0;

        for (idx, c5) in candles_5m.iter().enumerate() {
            if c5.time >= cfg.entry_cutoff { break; }

            // FVG 유효시간 체크
            if pending_fvg.is_some() && idx - fvg_formed_idx > cfg.fvg_expiry_candles {
                pending_fvg = None;
                fvg_side = None;
            }

            // FVG 감지
            if pending_fvg.is_none() && idx >= 2 {
                let a = &candles_5m[idx - 2];
                let b = &candles_5m[idx - 1];
                let c = c5;

                if b.is_bullish() && a.high < c.low {
                    let or_ok = !require_or_breakout || b.close > or_high;
                    let side_ok = confirmed_side.map_or(true, |s| s == PositionSide::Long);
                    if or_ok && side_ok {
                        pending_fvg = Some(FairValueGap {
                            direction: FvgDirection::Bullish,
                            top: c.low, bottom: a.high,
                            candle_b_idx: idx - 1, stop_loss: a.low,
                        });
                        fvg_side = Some(PositionSide::Long);
                        fvg_formed_idx = idx;
                    }
                }

                if pending_fvg.is_none() && b.is_bearish() && a.low > c.high {
                    let or_ok = !require_or_breakout || b.close < or_low;
                    let side_ok = confirmed_side.map_or(true, |s| s == PositionSide::Short);
                    if or_ok && side_ok {
                        pending_fvg = Some(FairValueGap {
                            direction: FvgDirection::Bearish,
                            top: a.low, bottom: c.high,
                            candle_b_idx: idx - 1, stop_loss: a.high,
                        });
                        fvg_side = Some(PositionSide::Short);
                        fvg_formed_idx = idx;
                    }
                }
            }

            // 리트레이스 진입 확인
            if let Some(ref gap) = pending_fvg {
                let side = fvg_side.unwrap();
                let retrace = match side {
                    PositionSide::Long => c5.low,
                    PositionSide::Short => c5.high,
                };

                if gap.contains_price(retrace) {
                    let entry_price = gap.mid_price();
                    let stop_loss = gap.stop_loss;
                    let risk = (entry_price - stop_loss).abs();
                    let take_profit = match side {
                        PositionSide::Long => entry_price + (risk as f64 * cfg.rr_ratio) as i64,
                        PositionSide::Short => entry_price - (risk as f64 * cfg.rr_ratio) as i64,
                    };

                    info!(
                        "{}: {:?} 진입 신호 — entry={}, SL={}, TP={}",
                        self.stock_name, side, entry_price, stop_loss, take_profit
                    );

                    // 주문 실행
                    match self.execute_entry(side, entry_price, stop_loss, take_profit).await {
                        Ok(_) => return Ok(true),
                        Err(e) => {
                            error!("{}: 주문 실패: {e}", self.stock_name);
                            return Ok(false);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// 포지션 실시간 관리 (본전스탑/트레일링/시간스탑)
    async fn manage_position(&self) -> Result<Option<TradeResult>, KisError> {
        let price = self.fetch_current_price().await?;
        let current = price.stck_prpr;
        let now = Local::now().time();
        let cfg = &self.strategy.config;

        let mut state = self.state.write().await;
        let pos = match state.current_position.as_mut() {
            Some(p) => p,
            None => return Ok(None),
        };

        let risk = (pos.entry_price - pos.stop_loss).abs() as f64;
        let breakeven_dist = (risk * cfg.breakeven_r) as i64;
        let trailing_dist = (risk * cfg.trailing_r) as i64;

        // 유리한 방향 최적가 추적 (best_price를 entry_time에 임시 저장 → 별도 필드 필요)
        // 간소화: 현재가로만 판단
        let profit = match pos.side {
            PositionSide::Long => current - pos.entry_price,
            PositionSide::Short => pos.entry_price - current,
        };

        // 본전스탑 + 트레일링
        if profit >= breakeven_dist {
            let new_sl = match pos.side {
                PositionSide::Long => current - trailing_dist,
                PositionSide::Short => current + trailing_dist,
            };
            let improved = match pos.side {
                PositionSide::Long => new_sl > pos.stop_loss,
                PositionSide::Short => new_sl < pos.stop_loss,
            };
            if improved {
                debug!("{}: 트레일링 SL 갱신 {} → {}", self.stock_name, pos.stop_loss, new_sl);
                pos.stop_loss = new_sl;
            }
        }

        // 손절 체크
        let sl_hit = match pos.side {
            PositionSide::Long => current <= pos.stop_loss,
            PositionSide::Short => current >= pos.stop_loss,
        };
        if sl_hit {
            let reason = if pos.stop_loss != pos.entry_price && profit > 0 {
                ExitReason::TrailingStop
            } else if pos.stop_loss == pos.entry_price {
                ExitReason::BreakevenStop
            } else {
                ExitReason::StopLoss
            };
            drop(state);
            let result = self.close_position_market(reason).await?;
            return Ok(Some(result));
        }

        // 익절 체크
        let tp_hit = match pos.side {
            PositionSide::Long => current >= pos.take_profit,
            PositionSide::Short => current <= pos.take_profit,
        };
        if tp_hit {
            drop(state);
            let result = self.close_position_market(ExitReason::TakeProfit).await?;
            return Ok(Some(result));
        }

        // 시간스탑: 진입 후 N캔들(× 5분) 경과
        let elapsed_min = (now - pos.entry_time).num_minutes();
        let time_limit = cfg.time_stop_candles as i64 * 5;
        if elapsed_min >= time_limit && profit < breakeven_dist {
            drop(state);
            let result = self.close_position_market(ExitReason::TimeStop).await?;
            return Ok(Some(result));
        }

        Ok(None)
    }

    /// 시장가 진입 주문
    async fn execute_entry(
        &self,
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
    ) -> Result<(), KisError> {
        let order_side = match side {
            PositionSide::Long => OrderSide::Buy,
            PositionSide::Short => OrderSide::Sell,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01",
            "ORD_QTY": self.quantity.to_string(),
            "ORD_UNPR": "0",
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let resp: KisResponse<serde_json::Value> = self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash", &tr_id, None, Some(&body))
            .await?;

        if resp.rt_cd != "0" {
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        info!("{}: {:?} {}주 진입 완료", self.stock_name, order_side, self.quantity);

        let mut state = self.state.write().await;
        state.current_position = Some(Position {
            side,
            entry_price,
            stop_loss,
            take_profit,
            entry_time: Local::now().time(),
            quantity: self.quantity,
        });
        state.phase = "포지션 보유".to_string();

        Ok(())
    }

    /// 시장가 청산 주문
    async fn close_position_market(&self, reason: ExitReason) -> Result<TradeResult, KisError> {
        let mut state = self.state.write().await;
        let pos = state.current_position.take()
            .ok_or_else(|| KisError::Internal("청산할 포지션 없음".into()))?;
        drop(state);

        let order_side = match pos.side {
            PositionSide::Long => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01",
            "ORD_QTY": pos.quantity.to_string(),
            "ORD_UNPR": "0",
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let resp: KisResponse<serde_json::Value> = self.client
            .execute(HttpMethod::Post, "/uapi/domestic-stock/v1/trading/order-cash", &tr_id, None, Some(&body))
            .await?;

        if resp.rt_cd != "0" {
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        let price = self.fetch_current_price().await?;
        let exit_price = price.stck_prpr;
        let exit_time = Local::now().time();

        info!("{}: {:?} 청산 — {} @ {} ({:?})", self.stock_name, pos.side, self.quantity, exit_price, reason);

        let result = TradeResult {
            side: pos.side,
            entry_price: pos.entry_price,
            exit_price,
            stop_loss: pos.stop_loss,
            take_profit: pos.take_profit,
            entry_time: pos.entry_time,
            exit_time,
            exit_reason: reason,
        };

        let mut state = self.state.write().await;
        state.current_position = None;
        state.phase = "신호 탐색".to_string();

        Ok(result)
    }

    // ── 헬퍼 메서드 ──

    async fn update_phase(&self, phase: &str) {
        self.state.write().await.phase = phase.to_string();
    }

    async fn update_pnl(&self, trades: &[TradeResult]) {
        let mut state = self.state.write().await;
        state.today_trades = trades.to_vec();
        state.today_pnl = trades.iter().map(|t| t.pnl_pct()).sum();
    }

    fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::Relaxed)
    }

    async fn wait_until(&self, target: NaiveTime) {
        loop {
            if self.is_stopped() { return; }
            let now = Local::now().time();
            if now >= target { return; }
            let diff = (target - now).num_seconds().max(1) as u64;
            let sleep_secs = diff.min(10);
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        }
    }

    async fn fetch_current_price(&self) -> Result<InquirePrice, KisError> {
        let query = [
            ("FID_COND_MRKT_DIV_CODE", "J"),
            ("FID_INPUT_ISCD", self.stock_code.as_str()),
        ];
        let resp: KisResponse<InquirePrice> = self.client
            .execute(HttpMethod::Get, "/uapi/domestic-stock/v1/quotations/inquire-price",
                &TransactionId::InquirePrice, Some(&query), None)
            .await?;
        resp.into_result()
    }

    async fn fetch_today_minute_candles(&self) -> Result<Vec<MinuteCandle>, KisError> {
        let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
        let mut all_items = Vec::new();
        let mut fid_input_hour = "153000".to_string();

        for page in 0..10 {
            let query = [
                ("FID_ETC_CLS_CODE", ""),
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", self.stock_code.as_str()),
                ("FID_INPUT_HOUR_1", &fid_input_hour),
                ("FID_PW_DATA_INCU_YN", "N"),
            ];

            let resp: KisResponse<Vec<MinutePriceItem>> = self.client
                .execute(HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-time-itemchartprice",
                    &TransactionId::InquireTimePrice, Some(&query), None)
                .await?;

            if resp.rt_cd != "0" {
                if page == 0 {
                    return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
                }
                break;
            }

            let has_next = resp.has_next();
            let items = resp.output.unwrap_or_default();
            if items.is_empty() { break; }

            if let Some(last) = items.last() {
                fid_input_hour = last.stck_cntg_hour.clone();
            }
            all_items.extend(items);

            if !has_next { break; }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        let mut candles: Vec<MinuteCandle> = all_items.iter()
            .filter_map(|item| {
                let d = chrono::NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let t = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                if t < market_open || t > market_close { return None; }
                Some(MinuteCandle { date: d, time: t, open: item.stck_oprc, high: item.stck_hgpr,
                    low: item.stck_lwpr, close: item.stck_prpr, volume: item.cntg_vol as u64 })
            })
            .collect();

        candles.sort_by_key(|c| c.time);
        Ok(candles)
    }
}
