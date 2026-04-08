use crate::domain::types::Environment;

/// 애플리케이션 설정
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// KIS API 앱 키
    pub appkey: String,
    /// KIS API 앱 시크릿
    pub appsecret: String,
    /// 종합계좌번호 (8자리)
    pub account_no: String,
    /// 계좌상품코드 (보통 "01")
    pub account_product_code: String,
    /// 실전/모의투자 환경
    pub environment: Environment,
    /// 서버 바인드 주소
    pub server_host: String,
    /// 서버 포트
    pub server_port: u16,
    /// PostgreSQL 접속 URL
    pub database_url: String,
}

impl AppConfig {
    /// 환경변수에서 설정 로드
    pub fn from_env() -> Result<Self, String> {
        dotenvy::dotenv().ok();

        let env_str = std::env::var("KIS_ENVIRONMENT").unwrap_or_else(|_| "paper".to_string());
        let environment = match env_str.to_lowercase().as_str() {
            "real" | "live" => Environment::Real,
            _ => Environment::Paper,
        };

        Ok(Self {
            appkey: env_required("KIS_APPKEY")?,
            appsecret: env_required("KIS_APPSECRET")?,
            account_no: env_required("KIS_ACCOUNT_NO")?,
            account_product_code: std::env::var("KIS_ACCOUNT_PRODUCT_CODE")
                .unwrap_or_else(|_| "01".to_string()),
            environment,
            server_host: std::env::var("SERVER_HOST")
                .unwrap_or_else(|_| "127.0.0.1".to_string()),
            server_port: std::env::var("SERVER_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres@localhost:5433/kis_agent".to_string()),
        })
    }
}

fn env_required(key: &str) -> Result<String, String> {
    std::env::var(key).map_err(|_| format!("환경변수 {key}가 설정되지 않았습니다"))
}
