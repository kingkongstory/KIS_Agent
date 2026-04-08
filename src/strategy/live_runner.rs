use std::sync::Arc;

use chrono::{Local, NaiveTime};
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::domain::error::KisError;
use crate::domain::models::order::{OrderRequest, OrderSide, OrderType};
use crate::domain::models::price::InquirePrice;
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::{StockCode, TransactionId};
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};

use super::candle::MinuteCandle;
use super::orb_fvg::OrbFvgStrategy;
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

/// 실시간 트레이딩 러너
pub struct LiveRunner {
    client: Arc<KisHttpClient>,
    strategy: OrbFvgStrategy,
    stock_code: StockCode,
    /// 주문 수량 (주)
    quantity: u64,
}

impl LiveRunner {
    pub fn new(
        client: Arc<KisHttpClient>,
        stock_code: StockCode,
        rr_ratio: f64,
        quantity: u64,
    ) -> Self {
        Self {
            client,
            strategy: OrbFvgStrategy::new(rr_ratio),
            stock_code,
            quantity,
        }
    }

    /// 장중 실행 루프
    pub async fn run(&self) -> Result<Option<TradeResult>, KisError> {
        let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let or_end = NaiveTime::from_hms_opt(9, 15, 0).unwrap();
        let entry_cutoff = NaiveTime::from_hms_opt(15, 20, 0).unwrap();
        let force_exit = NaiveTime::from_hms_opt(15, 25, 0).unwrap();

        info!("=== ORB+FVG 실시간 트레이딩 시작 ===");
        info!("종목: {}, 수량: {}주, RR: 1:{:.1}",
            self.stock_code, self.quantity, self.strategy.config.rr_ratio);

        // 장 시작 대기
        self.wait_until(market_open).await;
        info!("장 시작 — OR 범위 수집 중 (09:00~09:15)");

        // OR 종료 대기
        self.wait_until(or_end).await;
        info!("OR 기간 완료 — 신호 탐색 시작");

        // 메인 폴링 루프
        let mut position: Option<Position> = None;
        let mut trade_result: Option<TradeResult> = None;

        loop {
            let now = Local::now().time();

            // 시간 초과 체크
            if now >= force_exit {
                // 포지션 보유 중이면 강제 청산
                if let Some(ref pos) = position {
                    info!("15:25 — 포지션 강제 청산");
                    trade_result = Some(self.close_position(pos, ExitReason::EndOfDay).await?);
                }
                break;
            }

            if now >= entry_cutoff && position.is_none() {
                info!("15:20 — 신규 진입 중단, 대기 종료");
                break;
            }

            // 포지션 보유 중: SL/TP 체크
            if let Some(ref pos) = position {
                match self.check_and_exit(pos).await {
                    Ok(Some(result)) => {
                        trade_result = Some(result);
                        break; // 하루 1회 거래
                    }
                    Ok(None) => {} // 유지
                    Err(e) => {
                        error!("SL/TP 체크 실패: {e}");
                    }
                }
            } else {
                // 신호 탐색
                match self.poll_and_evaluate().await {
                    Ok(Some(signal)) => {
                        info!(
                            "신호 발생: {:?} entry={} SL={} TP={}",
                            signal.side, signal.entry_price, signal.stop_loss, signal.take_profit
                        );
                        match self.enter_position(&signal).await {
                            Ok(pos) => {
                                info!("포지션 진입 완료: {:?} {}주 @ {}", pos.side, pos.quantity, pos.entry_price);
                                position = Some(pos);
                            }
                            Err(e) => {
                                error!("주문 실패: {e}");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("분봉 조회/평가 실패: {e}");
                    }
                }
            }

            // 다음 폴링까지 대기 (포지션 보유 시 30초, 미보유 시 60초)
            let interval = if position.is_some() { 30 } else { 60 };
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        }

        if let Some(ref result) = trade_result {
            info!("=== 거래 결과 ===");
            info!(
                "{:?} 진입={} 청산={} 손익={:.2}% RR={:.2} 사유={:?}",
                result.side,
                result.entry_price,
                result.exit_price,
                result.pnl_pct(),
                result.realized_rr(),
                result.exit_reason
            );
        } else {
            info!("=== 오늘 거래 없음 ===");
        }

        Ok(trade_result)
    }

    /// 분봉 폴링 + 전략 평가
    async fn poll_and_evaluate(&self) -> Result<Option<Signal>, KisError> {
        let candles = self.fetch_today_minute_candles().await?;
        if candles.is_empty() {
            return Ok(None);
        }

        let (signal, or_range) = self.strategy.evaluate_candles(&candles);
        if let Some((h, l)) = or_range {
            info!("OR 범위: HIGH={h}, LOW={l} | 캔들 수: {}", candles.len());
        }

        Ok(signal)
    }

    /// SL/TP 체크 후 청산
    async fn check_and_exit(&self, position: &Position) -> Result<Option<TradeResult>, KisError> {
        let price = self.fetch_current_price().await?;
        let current = price.stck_prpr;

        if let Some(reason) = self.strategy.check_exit(position, current) {
            info!("청산 조건 충족: {:?} (현재가={})", reason, current);
            let result = self.close_position(position, reason).await?;
            return Ok(Some(result));
        }

        Ok(None)
    }

    /// 포지션 진입 (시장가 주문)
    async fn enter_position(&self, signal: &Signal) -> Result<Position, KisError> {
        let side = match signal.side {
            PositionSide::Long => OrderSide::Buy,
            PositionSide::Short => OrderSide::Sell,
        };

        let order = OrderRequest {
            stock_code: self.stock_code.clone(),
            side,
            order_type: OrderType::Market,
            quantity: self.quantity,
            price: 0,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": order.stock_code.as_str(),
            "ORD_DVSN": order.ord_dvsn(),
            "ORD_QTY": order.quantity.to_string(),
            "ORD_UNPR": order.ord_unpr(),
        });

        let tr_id = match side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let resp: KisResponse<serde_json::Value> = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &tr_id,
                None,
                Some(&body),
            )
            .await?;

        if resp.rt_cd != "0" {
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        info!("주문 체결: {:?} {}주", side, self.quantity);

        Ok(Position {
            side: signal.side,
            entry_price: signal.entry_price,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            entry_time: Local::now().time(),
            quantity: self.quantity,
        })
    }

    /// 포지션 청산 (시장가 반대 주문)
    async fn close_position(
        &self,
        position: &Position,
        reason: ExitReason,
    ) -> Result<TradeResult, KisError> {
        let side = match position.side {
            PositionSide::Long => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01", // 시장가
            "ORD_QTY": position.quantity.to_string(),
            "ORD_UNPR": "0",
        });

        let tr_id = match side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let resp: KisResponse<serde_json::Value> = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &tr_id,
                None,
                Some(&body),
            )
            .await?;

        if resp.rt_cd != "0" {
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        let price = self.fetch_current_price().await?;
        let exit_price = price.stck_prpr;

        info!("청산 완료: {:?} {}주 @ {}", side, position.quantity, exit_price);

        Ok(TradeResult {
            side: position.side,
            entry_price: position.entry_price,
            exit_price,
            stop_loss: position.stop_loss,
            take_profit: position.take_profit,
            entry_time: position.entry_time,
            exit_time: Local::now().time(),
            exit_reason: reason,
        })
    }

    /// 당일 1분봉 전체 조회
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

            let resp: KisResponse<Vec<MinutePriceItem>> = self
                .client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-time-itemchartprice",
                    &TransactionId::InquireTimePrice,
                    Some(&query),
                    None,
                )
                .await?;

            if resp.rt_cd != "0" {
                if page == 0 {
                    return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
                }
                break;
            }

            let has_next = resp.has_next();
            let items = resp.output.unwrap_or_default();
            if items.is_empty() {
                break;
            }

            if let Some(last) = items.last() {
                fid_input_hour = last.stck_cntg_hour.clone();
            }
            all_items.extend(items);

            if !has_next {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        let mut candles: Vec<MinuteCandle> = all_items
            .iter()
            .filter_map(|item| {
                let d = chrono::NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let t = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                if t < market_open || t > market_close {
                    return None;
                }
                Some(MinuteCandle {
                    date: d,
                    time: t,
                    open: item.stck_oprc,
                    high: item.stck_hgpr,
                    low: item.stck_lwpr,
                    close: item.stck_prpr,
                    volume: item.cntg_vol as u64,
                })
            })
            .collect();

        candles.sort_by_key(|c| c.time);
        Ok(candles)
    }

    /// 현재가 조회
    async fn fetch_current_price(&self) -> Result<InquirePrice, KisError> {
        let query = [
            ("FID_COND_MRKT_DIV_CODE", "J"),
            ("FID_INPUT_ISCD", self.stock_code.as_str()),
        ];

        let resp: KisResponse<InquirePrice> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/quotations/inquire-price",
                &TransactionId::InquirePrice,
                Some(&query),
                None,
            )
            .await?;

        resp.into_result()
    }

    /// 특정 시각까지 대기
    async fn wait_until(&self, target: NaiveTime) {
        loop {
            let now = Local::now().time();
            if now >= target {
                return;
            }
            let diff = (target - now).num_seconds().max(1) as u64;
            let sleep_secs = diff.min(30); // 최대 30초씩 체크
            info!("{}까지 대기 중... ({}초 남음)", target.format("%H:%M:%S"), diff);
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        }
    }
}
