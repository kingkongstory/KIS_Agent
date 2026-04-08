use async_trait::async_trait;
use std::sync::Arc;

use super::http_client::{HttpMethod, KisHttpClient, KisResponse};
use crate::domain::error::KisError;
use crate::domain::models::account::{AccountSummary, BuyableInfo, PositionItem};
use crate::domain::models::order::ExecutionItem;
use crate::domain::ports::account::AccountPort;
use crate::domain::types::{StockCode, TransactionId};

/// KIS 계좌 어댑터
pub struct KisAccountAdapter {
    client: Arc<KisHttpClient>,
}

impl KisAccountAdapter {
    pub fn new(client: Arc<KisHttpClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl AccountPort for KisAccountAdapter {
    async fn get_balance(&self) -> Result<(Vec<PositionItem>, AccountSummary), KisError> {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("AFHR_FLPR_YN", "N"),
            ("OFL_YN", ""),
            ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"),
            ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"),
            ("PRCS_DVSN", "01"),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];

        // output1: 보유 종목 목록
        let resp_positions: KisResponse<Vec<PositionItem>> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await?;

        if resp_positions.rt_cd != "0" {
            return Err(KisError::classify(
                resp_positions.rt_cd,
                resp_positions.msg_cd,
                resp_positions.msg1,
            ));
        }

        let positions = resp_positions.output.unwrap_or_default();

        // output2: 계좌 합계 — 별도 호출로 가져오거나 같은 응답에서 추출
        // KIS API는 동일 응답에 output1(종목)과 output2(합계)를 함께 반환
        // 여기서는 합계를 별도로 파싱하기 위해 raw JSON 재요청
        let resp_summary: KisResponse<AccountSummary> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await?;

        let summary = resp_summary.into_result2().unwrap_or(AccountSummary {
            dnca_tot_amt: 0,
            tot_evlu_amt: 0,
            evlu_pfls_smtl_amt: 0,
            pchs_amt_smtl_amt: 0,
        });

        Ok((positions, summary))
    }

    async fn get_executions(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<ExecutionItem>, KisError> {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("INQR_STRT_DT", start_date),
            ("INQR_END_DT", end_date),
            ("SLL_BUY_DVSN_CD", "00"), // 전체
            ("INQR_DVSN", "00"),
            ("PDNO", ""),
            ("CCLD_DVSN", "00"),
            ("ORD_GNO_BRNO", ""),
            ("ODNO", ""),
            ("INQR_DVSN_3", "00"),
            ("INQR_DVSN_1", ""),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];

        let resp: KisResponse<Vec<ExecutionItem>> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-daily-ccld",
                &TransactionId::InquireDailyExecution,
                Some(&query),
                None,
            )
            .await?;

        if resp.rt_cd != "0" {
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        Ok(resp.output.unwrap_or_default())
    }

    async fn get_buyable(
        &self,
        stock_code: &StockCode,
        price: i64,
    ) -> Result<BuyableInfo, KisError> {
        let price_str = price.to_string();
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("PDNO", stock_code.as_str()),
            ("ORD_UNPR", price_str.as_str()),
            ("ORD_DVSN", "00"),
            ("CMA_EVLU_AMT_ICLD_YN", "N"),
            ("OVRS_ICLD_YN", "N"),
        ];

        let resp: KisResponse<BuyableInfo> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-psbl-order",
                &TransactionId::InquireBuyable,
                Some(&query),
                None,
            )
            .await?;

        resp.into_result()
    }
}
