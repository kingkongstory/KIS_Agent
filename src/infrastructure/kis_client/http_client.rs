use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, warn};

use super::auth::TokenManager;
use super::rate_limiter::KisRateLimiter;
use crate::domain::error::KisError;
use crate::domain::types::{Environment, TransactionId};

/// KIS API 공통 응답 래퍼
#[derive(Debug, Deserialize)]
pub struct KisResponse<T> {
    /// 응답코드 ("0" = 성공)
    pub rt_cd: String,
    /// 메시지코드
    pub msg_cd: String,
    /// 메시지
    pub msg1: String,
    /// 응답 데이터 (단일)
    pub output: Option<T>,
    /// 응답 데이터 (목록/두번째)
    pub output1: Option<T>,
    /// 응답 데이터 (목록용)
    pub output2: Option<T>,
    /// 연속조회 키
    pub tr_cont: Option<String>,
}

impl<T> KisResponse<T> {
    /// 성공 여부 확인 후 데이터 반환
    pub fn into_result(self) -> Result<T, KisError> {
        if self.rt_cd == "0" {
            self.output
                .or(self.output1)
                .ok_or_else(|| KisError::ParseError("응답 데이터가 비어 있습니다".into()))
        } else {
            Err(KisError::classify(self.rt_cd, self.msg_cd, self.msg1))
        }
    }

    /// 성공 여부 확인 후 output2 데이터 반환
    pub fn into_result2(self) -> Result<T, KisError> {
        if self.rt_cd == "0" {
            self.output2
                .ok_or_else(|| KisError::ParseError("output2 데이터가 비어 있습니다".into()))
        } else {
            Err(KisError::classify(self.rt_cd, self.msg_cd, self.msg1))
        }
    }

    /// 연속조회 가능 여부
    pub fn has_next(&self) -> bool {
        self.tr_cont.as_deref() == Some("M") || self.tr_cont.as_deref() == Some("F")
    }
}

/// HTTP 요청 메서드
#[derive(Debug, Clone, Copy)]
pub enum HttpMethod {
    Get,
    Post,
}

/// KIS HTTP 클라이언트
pub struct KisHttpClient {
    client: reqwest::Client,
    token_manager: Arc<TokenManager>,
    rate_limiter: Arc<KisRateLimiter>,
    environment: Environment,
    account_no: String,
    account_product_code: String,
}

impl KisHttpClient {
    pub fn new(
        token_manager: Arc<TokenManager>,
        rate_limiter: Arc<KisRateLimiter>,
        environment: Environment,
        account_no: String,
        account_product_code: String,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            token_manager,
            rate_limiter,
            environment,
            account_no,
            account_product_code,
        }
    }

    /// API 호출 실행 (재시도 포함, 최대 3회)
    pub async fn execute<T: DeserializeOwned>(
        &self,
        method: HttpMethod,
        path: &str,
        tr_id: &TransactionId,
        query: Option<&[(&str, &str)]>,
        body: Option<&serde_json::Value>,
    ) -> Result<KisResponse<T>, KisError> {
        let max_retries = 3;

        for attempt in 0..max_retries {
            self.rate_limiter.wait_for_api().await;

            match self.do_request(method, path, tr_id, query, body).await {
                Ok(resp) => return Ok(resp),
                Err(e) if e.is_retryable() && attempt < max_retries - 1 => {
                    let delay = e.retry_delay_ms();
                    warn!(
                        "API 요청 실패 (시도 {}/{}): {e}, {delay}ms 후 재시도",
                        attempt + 1,
                        max_retries
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Err(e) => return Err(e),
            }
        }

        unreachable!()
    }

    /// 단일 HTTP 요청 실행
    async fn do_request<T: DeserializeOwned>(
        &self,
        method: HttpMethod,
        path: &str,
        tr_id: &TransactionId,
        query: Option<&[(&str, &str)]>,
        body: Option<&serde_json::Value>,
    ) -> Result<KisResponse<T>, KisError> {
        let token = self.token_manager.get_token().await?;
        let url = format!("{}{path}", self.environment.rest_base_url());

        debug!("KIS API 요청: {method:?} {url}");

        let mut request = match method {
            HttpMethod::Get => self.client.get(&url),
            HttpMethod::Post => self.client.post(&url),
        };

        // 공통 헤더
        request = request
            .header("Content-Type", "application/json; charset=utf-8")
            .header("authorization", format!("Bearer {token}"))
            .header("appkey", self.token_manager.appkey())
            .header("appsecret", self.token_manager.appsecret())
            .header("tr_id", tr_id.code(self.environment))
            .header("custtype", "P");

        if let Some(q) = query {
            request = request.query(q);
        }

        if let Some(b) = body {
            request = request.json(b);
        }

        let response = request.send().await.map_err(|e| KisError::HttpError(e.to_string()))?;

        let status = response.status();
        let text = response.text().await.map_err(|e| KisError::HttpError(e.to_string()))?;

        if !status.is_success() {
            return Err(KisError::HttpError(format!("HTTP {status}: {text}")));
        }

        serde_json::from_str(&text).map_err(|e| {
            KisError::ParseError(format!("JSON 파싱 실패: {e}, 응답: {text}"))
        })
    }

    pub fn account_no(&self) -> &str {
        &self.account_no
    }

    pub fn account_product_code(&self) -> &str {
        &self.account_product_code
    }

    pub fn environment(&self) -> Environment {
        self.environment
    }
}
