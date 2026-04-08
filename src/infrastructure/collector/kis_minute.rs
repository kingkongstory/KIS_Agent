use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::TransactionId;
use crate::infrastructure::cache::postgres_store::{MinuteCandle, PostgresStore};
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};

/// KIS API 당일 분봉 수집기
pub struct KisMinuteCollector {
    client: Arc<KisHttpClient>,
    store: Arc<PostgresStore>,
}

/// KIS 분봉 응답 항목
#[derive(Debug, Clone, Deserialize)]
struct MinutePriceItem {
    /// 영업일자 (YYYYMMDD)
    stck_bsop_date: String,
    /// 체결시간 (HHMMSS)
    stck_cntg_hour: String,
    /// 시가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_oprc: i64,
    /// 고가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_hgpr: i64,
    /// 저가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_lwpr: i64,
    /// 현재가 (종가)
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_prpr: i64,
    /// 거래량
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    cntg_vol: i64,
    /// 누적거래량
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize"
    )]
    acml_vol: i64,
}

impl KisMinuteCollector {
    pub fn new(client: Arc<KisHttpClient>, store: Arc<PostgresStore>) -> Self {
        Self { client, store }
    }

    /// 당일 1분봉 수집 및 PostgreSQL 저장
    pub async fn collect_today(
        &self,
        stock_code: &str,
    ) -> Result<usize, KisError> {
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        self.collect_minute(stock_code, &today, 1).await
    }

    /// 특정일 분봉 수집 (interval: 1분, 5분, 10분 등)
    pub async fn collect_minute(
        &self,
        stock_code: &str,
        date: &str,
        interval: i16,
    ) -> Result<usize, KisError> {
        let mut all_items = Vec::new();
        let mut fid_input_hour = "153000".to_string(); // 장 마감부터 역순 조회
        let max_pages = 10;

        for page in 0..max_pages {
            let query = [
                ("FID_ETC_CLS_CODE", ""),
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", stock_code),
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

            // 마지막 항목의 시간을 다음 페이지 종료 시간으로
            if let Some(last) = items.last() {
                fid_input_hour = last.stck_cntg_hour.clone();
            }

            all_items.extend(items);

            if !has_next {
                break;
            }

            // 속도 제한 준수
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if all_items.is_empty() {
            warn!("KIS 분봉: {stock_code} 데이터 없음 (날짜: {date})");
            return Ok(0);
        }

        // MinutePriceItem → MinuteCandle 변환
        let candles: Vec<MinuteCandle> = all_items
            .iter()
            .filter_map(|item| {
                let date = NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let time = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                let datetime = NaiveDateTime::new(date, time);

                // 장 시간 필터링 (09:00 ~ 15:30)
                let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
                let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
                if time < market_open || time > market_close {
                    return None;
                }

                Some(MinuteCandle {
                    datetime,
                    open: item.stck_oprc,
                    high: item.stck_hgpr,
                    low: item.stck_lwpr,
                    close: item.stck_prpr,
                    volume: item.cntg_vol,
                    interval_min: interval,
                })
            })
            .collect();

        let count = self.store.save_minute_ohlcv(stock_code, &candles).await?;
        info!("KIS 분봉: {stock_code} {interval}분봉 {count}건 저장 완료 (날짜: {date})");
        Ok(count)
    }

    /// 여러 종목 당일 분봉 일괄 수집
    pub async fn collect_today_batch(
        &self,
        stock_codes: &[&str],
    ) -> Result<usize, KisError> {
        let mut total = 0;
        for code in stock_codes {
            match self.collect_today(code).await {
                Ok(count) => total += count,
                Err(e) => warn!("KIS 분봉: {code} 수집 실패: {e}"),
            }
            // 속도 제한 (초당 제한 고려)
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        info!("KIS 분봉: {}종목 당일 분봉 수집 완료, 총 {total}건", stock_codes.len());
        Ok(total)
    }
}
