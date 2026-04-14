use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, warn};

use super::auth::TokenManager;
use super::rate_limiter::KisRateLimiter;
use crate::domain::error::KisError;
use crate::domain::types::{Environment, TransactionId};

/// KIS API 공통 응답 래퍼
#[derive(Debug)]
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

/// KIS API 에러 응답 (HTTP 500 등)
#[derive(Deserialize)]
struct KisErrorBody {
    #[serde(default)]
    rt_cd: String,
    #[serde(default)]
    msg_cd: String,
    #[serde(default)]
    msg1: String,
    /// 에러 응답에서 msg_cd 대신 사용되는 필드
    #[serde(default)]
    message: String,
}

/// 내부 Raw 응답 (모든 output을 Value로 받음)
#[derive(Deserialize)]
struct RawKisResponse {
    rt_cd: String,
    #[serde(default)]
    msg_cd: String,
    #[serde(default)]
    msg1: String,
    #[serde(default)]
    output: Option<serde_json::Value>,
    #[serde(default)]
    output1: Option<serde_json::Value>,
    #[serde(default)]
    output2: Option<serde_json::Value>,
    #[serde(default)]
    tr_cont: Option<String>,
}

impl RawKisResponse {
    /// Raw 응답을 타입 T로 변환 (타입 불일치 시 None)
    fn into_typed<T: DeserializeOwned>(self) -> KisResponse<T> {
        KisResponse {
            rt_cd: self.rt_cd,
            msg_cd: self.msg_cd,
            msg1: self.msg1,
            output: self.output.and_then(|v| serde_json::from_value(v).ok()),
            output1: self.output1.and_then(|v| serde_json::from_value(v).ok()),
            output2: self.output2.and_then(|v| serde_json::from_value(v).ok()),
            tr_cont: self.tr_cont,
        }
    }
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
    /// 운영 이벤트 로거 (api_error 기록용).
    /// `KisHttpClient` 는 `pg_store` 보다 먼저 생성되는 경우가 있어 `OnceLock` 으로 사후 주입.
    event_logger: std::sync::OnceLock<Arc<crate::infrastructure::monitoring::event_logger::EventLogger>>,
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
            event_logger: std::sync::OnceLock::new(),
        }
    }

    /// 이벤트 로거 사후 주입. 이미 설정돼 있으면 무시.
    pub fn set_event_logger(
        &self,
        logger: Arc<crate::infrastructure::monitoring::event_logger::EventLogger>,
    ) {
        let _ = self.event_logger.set(logger);
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
                    // 지수 백오프: base_delay * 2^attempt (서버 과부하 시 연쇄 실패 방지)
                    let base = e.retry_delay_ms();
                    let delay = base * (1u64 << attempt);
                    warn!(
                        "API 요청 실패 (시도 {}/{}): {e}, {delay}ms 후 재시도",
                        attempt + 1,
                        max_retries
                    );
                    if let Some(el) = self.event_logger.get() {
                        el.log_event(
                            "", "system", "api_error", "warn",
                            &format!("{:?} {} 실패: {e} (재시도 {}/{})", method, path, attempt + 1, max_retries),
                            serde_json::json!({
                                "path": path,
                                "tr_id": format!("{:?}", tr_id),
                                "attempt": attempt + 1,
                                "max_retries": max_retries,
                                "delay_ms": delay,
                                "error": e.to_string(),
                                "retryable": true,
                            }),
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Err(e) => {
                    // 재시도 불가 또는 마지막 시도 실패 — 최종 에러로 기록
                    if let Some(el) = self.event_logger.get() {
                        let severity = if e.is_retryable() { "error" } else { "warn" };
                        el.log_event(
                            "", "system", "api_error", severity,
                            &format!("{:?} {} 최종 실패: {e}", method, path),
                            serde_json::json!({
                                "path": path,
                                "tr_id": format!("{:?}", tr_id),
                                "attempt": attempt + 1,
                                "max_retries": max_retries,
                                "error": e.to_string(),
                                "retryable": e.is_retryable(),
                                "terminal": true,
                            }),
                        );
                    }
                    return Err(e);
                }
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
            // KIS API는 HTTP 500이어도 JSON body에 에러 코드를 포함 — 파싱하여 분류
            // 에러 응답은 msg_cd 대신 message 필드를 사용하는 경우가 있음
            if let Ok(err) = serde_json::from_str::<KisErrorBody>(&text) {
                let msg_cd = if err.msg_cd.is_empty() { err.message } else { err.msg_cd };
                if !msg_cd.is_empty() {
                    return Err(KisError::classify(err.rt_cd, msg_cd, err.msg1));
                }
            }
            return Err(KisError::HttpError(format!("HTTP {status}: {text}")));
        }

        let raw: RawKisResponse = serde_json::from_str(&text).map_err(|e| {
            KisError::ParseError(format!("JSON 파싱 실패: {e}, 응답: {text}"))
        })?;
        Ok(raw.into_typed())
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
