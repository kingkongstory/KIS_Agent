use async_trait::async_trait;
use std::sync::Arc;

use super::http_client::{HttpMethod, KisHttpClient, KisResponse};
use crate::domain::error::KisError;
use crate::domain::models::price::{AskingPrice, DailyPriceItem, InquirePrice};
use crate::domain::ports::market_data::{MarketDataPort, StockSearchResult};
use crate::domain::types::{PeriodCode, StockCode, TransactionId};

/// KIS 시세 어댑터
pub struct KisMarketDataAdapter {
    client: Arc<KisHttpClient>,
}

impl KisMarketDataAdapter {
    pub fn new(client: Arc<KisHttpClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MarketDataPort for KisMarketDataAdapter {
    async fn get_current_price(&self, stock_code: &StockCode) -> Result<InquirePrice, KisError> {
        let query = [
            ("FID_COND_MRKT_DIV_CODE", "J"),
            ("FID_INPUT_ISCD", stock_code.as_str()),
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

    async fn get_asking_price(&self, stock_code: &StockCode) -> Result<AskingPrice, KisError> {
        let query = [
            ("FID_COND_MRKT_DIV_CODE", "J"),
            ("FID_INPUT_ISCD", stock_code.as_str()),
        ];

        let resp: KisResponse<AskingPrice> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/quotations/inquire-asking-price-exp-ccn",
                &TransactionId::InquireAskingPrice,
                Some(&query),
                None,
            )
            .await?;

        resp.into_result()
    }

    async fn get_daily_prices(
        &self,
        stock_code: &StockCode,
        start_date: &str,
        end_date: &str,
        period: PeriodCode,
    ) -> Result<Vec<DailyPriceItem>, KisError> {
        let mut all_items = Vec::new();
        let mut current_end = end_date.to_string();
        let max_pages = 10; // 최대 페이지 수 제한

        for _ in 0..max_pages {
            let query = [
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", stock_code.as_str()),
                ("FID_INPUT_DATE_1", start_date),
                ("FID_INPUT_DATE_2", current_end.as_str()),
                ("FID_PERIOD_DIV_CODE", period.as_str()),
                ("FID_ORG_ADJ_PRC", "0"),
            ];

            let resp: KisResponse<Vec<DailyPriceItem>> = self
                .client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-daily-itemchartprice",
                    &TransactionId::InquireDailyPrice,
                    Some(&query),
                    None,
                )
                .await?;

            let has_next = resp.has_next();

            // 기간별 시세는 output2에 데이터가 담김
            if resp.rt_cd == "0" {
                if let Some(items) = resp.output2 {
                    if items.is_empty() {
                        break;
                    }
                    // 마지막 항목의 날짜를 다음 페이지 종료일로 사용
                    if let Some(last) = items.last() {
                        current_end = last.stck_bsop_date.format("%Y%m%d").to_string();
                    }
                    all_items.extend(items);
                } else {
                    break;
                }
            } else {
                return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
            }

            if !has_next {
                break;
            }
        }

        // 날짜 기준 오름차순 정렬
        all_items.sort_by(|a, b| a.stck_bsop_date.cmp(&b.stck_bsop_date));
        Ok(all_items)
    }

    async fn search_stock(&self, keyword: &str) -> Result<Vec<StockSearchResult>, KisError> {
        // 종목 검색은 간단한 구현 — 실제로는 별도 검색 API 또는 로컬 DB 활용
        let query = [
            ("AUTH", ""),
            ("EXCD", "KRX"),
            ("CO_YN_PRICECUR", ""),
            ("CO_ST_PRICECUR", ""),
            ("CO_EN_PRICECUR", ""),
            ("CO_YN_RATE", ""),
            ("CO_ST_RATE", ""),
            ("CO_EN_RATE", ""),
            ("CO_YN_VALX", ""),
            ("CO_ST_VALX", ""),
            ("CO_EN_VALX", ""),
            ("KEYB", keyword),
        ];

        // 검색 API가 특별한 응답 형식을 사용하므로 별도 처리
        // 실제 구현시에는 로컬 종목 마스터 DB를 사용하는 것이 효율적
        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/quotations/search-stock-info",
                &TransactionId::SearchStock,
                Some(&query),
                None,
            )
            .await;

        match resp {
            Ok(response) => {
                if response.rt_cd == "0" {
                    let items = response.output.unwrap_or_default();
                    Ok(items
                        .into_iter()
                        .filter_map(|v| {
                            Some(StockSearchResult {
                                stock_code: v.get("pdno")?.as_str()?.to_string(),
                                stock_name: v.get("prdt_abrv_name")?.as_str()?.to_string(),
                                market: v
                                    .get("std_pdno")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("KRX")
                                    .to_string(),
                            })
                        })
                        .collect())
                } else {
                    Ok(vec![])
                }
            }
            Err(_) => Ok(vec![]),
        }
    }
}
