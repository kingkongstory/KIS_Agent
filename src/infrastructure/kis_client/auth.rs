use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::domain::error::KisError;
use crate::domain::types::Environment;

/// 토큰 정보
struct TokenInfo {
    access_token: String,
    expires_at: DateTime<Utc>,
}

/// 토큰 발급 요청
#[derive(Serialize)]
struct TokenRequest {
    grant_type: String,
    appkey: String,
    appsecret: String,
}

/// 토큰 발급 응답
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    access_token_token_expired: String,
    #[allow(dead_code)]
    token_type: String,
    #[allow(dead_code)]
    expires_in: u64,
}

/// WebSocket 승인키 응답
#[derive(Deserialize)]
pub struct ApprovalResponse {
    pub approval_key: String,
}

/// 토큰 관리자 (자동 갱신, double-check 패턴)
pub struct TokenManager {
    token: Arc<RwLock<Option<TokenInfo>>>,
    appkey: String,
    appsecret: String,
    environment: Environment,
    http_client: reqwest::Client,
}

impl TokenManager {
    pub fn new(appkey: String, appsecret: String, environment: Environment) -> Self {
        Self {
            token: Arc::new(RwLock::new(None)),
            appkey,
            appsecret,
            environment,
            http_client: reqwest::Client::new(),
        }
    }

    /// 유효한 액세스 토큰 반환 (필요시 자동 갱신)
    pub async fn get_token(&self) -> Result<String, KisError> {
        // 읽기 잠금으로 먼저 확인
        {
            let guard = self.token.read().await;
            if let Some(info) = guard.as_ref()
                && self.is_valid(info)
            {
                return Ok(info.access_token.clone());
            }
        }

        // 만료/없음 → 갱신
        self.refresh_token().await
    }

    /// 토큰 갱신 (double-check 패턴)
    async fn refresh_token(&self) -> Result<String, KisError> {
        let mut guard = self.token.write().await;

        // Double-check: 다른 태스크가 이미 갱신했을 수 있음
        if let Some(info) = guard.as_ref()
            && self.is_valid(info)
        {
            return Ok(info.access_token.clone());
        }

        info!("토큰 발급 요청 중...");

        let url = format!("{}/oauth2/tokenP", self.environment.rest_base_url());
        let body = TokenRequest {
            grant_type: "client_credentials".to_string(),
            appkey: self.appkey.clone(),
            appsecret: self.appsecret.clone(),
        };

        let response = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| KisError::HttpError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(KisError::AuthenticationFailed(format!(
                "토큰 발급 실패 (HTTP {status}): {text}"
            )));
        }

        let token_resp: TokenResponse = response
            .json()
            .await
            .map_err(|e| KisError::ParseError(e.to_string()))?;

        // "YYYY-MM-DD HH:MM:SS" 형식 파싱
        let expires_at = NaiveDateTime::parse_from_str(
            &token_resp.access_token_token_expired,
            "%Y-%m-%d %H:%M:%S",
        )
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now() + Duration::hours(23));

        info!("토큰 발급 완료, 만료: {expires_at}");

        let access_token = token_resp.access_token.clone();
        *guard = Some(TokenInfo {
            access_token: token_resp.access_token,
            expires_at,
        });

        Ok(access_token)
    }

    /// 토큰 유효성 검사 (만료 30분 전에 갱신)
    fn is_valid(&self, info: &TokenInfo) -> bool {
        Utc::now() < info.expires_at - Duration::minutes(30)
    }

    /// WebSocket 승인키 발급
    pub async fn get_approval_key(&self) -> Result<String, KisError> {
        let url = format!("{}/oauth2/Approval", self.environment.rest_base_url());
        let body = serde_json::json!({
            "grant_type": "client_credentials",
            "appkey": self.appkey,
            "secretkey": self.appsecret,
        });

        let response = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| KisError::HttpError(e.to_string()))?;

        let resp: ApprovalResponse = response
            .json()
            .await
            .map_err(|e| KisError::ParseError(e.to_string()))?;

        info!("WebSocket 승인키 발급 완료");
        Ok(resp.approval_key)
    }

    pub fn appkey(&self) -> &str {
        &self.appkey
    }

    pub fn appsecret(&self) -> &str {
        &self.appsecret
    }

    pub fn environment(&self) -> Environment {
        self.environment
    }
}
