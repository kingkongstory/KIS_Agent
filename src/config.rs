use chrono::NaiveTime;

use crate::domain::market_session::MarketSessionPolicy;
use crate::domain::types::Environment;
use crate::strategy::live_runner::ExecutionPolicyKind;

/// 애플리케이션 설정.
///
/// 환경변수 로드 시 2026-04-16 실전 투입 계획서의 안전 가드 변수를 함께 파싱한다.
/// 실전 여부는 `is_real_mode()` 로 판정한다 (`KIS_ENVIRONMENT=real` 또는
/// `KIS_ENABLE_REAL_TRADING=true`). 실전이면 기본값이 보수적으로 전환된다
/// (max_daily_trades_total=1, 15m stage 고정, entry_cutoff 15:00 등).
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

    // ── 2026-04-16 production-readiness 가드 ──────────────────────────────
    /// 실전 주문 경로 명시 opt-in. `true` 여야 실제 진입 경로가 열린다.
    /// `KIS_ENVIRONMENT=real` 과 독립적으로 한 번 더 확인해 이중 가드 역할.
    pub enable_real_trading: bool,
    /// 자동 시작 플래그 (default: false — 실전/모의 공통 수동 시작 원칙).
    pub auto_start: bool,
    /// 실전 모드에서 DB 연결 실패 시 즉시 종료 (default: true).
    /// false 로 두면 메모리 전용 fallback 을 허용하지만 실전에서는 권장되지 않는다.
    pub require_db_in_real: bool,
    /// 자동매매 허용 종목 화이트리스트. 비어있으면 기본 2종목 사용.
    /// 실전 초기에는 `122630` 단일 종목만 기동 허용하는 것이 권장값.
    pub allowed_codes: Vec<String>,
    /// 시스템 전체 기준 일일 최대 거래 횟수.
    /// 실전 기본 1, 모의 기본 5. 종목별 한도와 별개로 시스템 총합을 강제한다.
    pub max_daily_trades_total: usize,
    /// WS tick 부재 경보 임계 (초). 장중 이 값 초과 동안 시세 tick 이 없으면
    /// watchdog 가 프로세스를 종료한다. default 120.
    pub ws_stale_tick_secs: u64,
    /// WS 전체 메시지 부재 임계 (초). PINGPONG 포함 어떤 메시지도 없으면
    /// fatal 로 판정. default 240.
    pub ws_stale_message_secs: u64,
    /// 주기적 잔고/포지션 reconciliation 간격 (초). default 60.
    pub reconcile_secs: u64,
    /// reconcile 불일치 감지 시 재확인 시도 횟수. default 3.
    /// 2026-04-17 P1 — KIS `inquire-balance` 의 일시 지연을 흡수하기 위해
    /// 재확인을 강화. 첫 일치 시 즉시 통과(early return).
    pub reconcile_confirm_attempts: u32,
    /// reconcile 재확인 시도 간 대기 (초). default 5.
    /// 너무 짧으면 같은 캐시 응답을 반복 받아 의미가 없고, 너무 길면 진짜
    /// hidden position 감지 지연이 커진다.
    pub reconcile_confirm_interval_secs: u64,
    /// 실전 모드 신규 진입 마감 시각. default 15:00 (기존 15:20 보다 보수적).
    pub entry_cutoff_real: NaiveTime,
    /// 실전 모드에서 사용할 OR stage 목록.
    /// default `["15m"]` — 5m/30m stage 신호는 진입에 사용하지 않는다.
    /// 비워두면 모든 stage 허용 (모의투자 동작과 동일).
    pub real_or_stages: Vec<String>,
    /// 라이브 실행 정책. 실전 기본은 시장성 지정가, 모의 기본은 passive.
    pub live_execution_policy_kind: ExecutionPolicyKind,
    /// 라이브 진입 체결 대기 timeout (ms).
    pub live_entry_fill_timeout_ms: u64,
    /// 라이브 청산 검증 budget (ms).
    pub live_exit_verification_budget_ms: u64,
    /// 라이브 신호 탐색 주기 (ms).
    pub live_poll_interval_ms: u64,
    /// 라이브 포지션 관리 주기 (ms).
    pub live_manage_poll_interval_ms: u64,
    /// 2026-04-24 P0-1: 시장 세션 정책 (장외 WS backoff 확대 등).
    ///
    /// `KIS_OFF_HOURS_SUPPRESSION_ENABLED=false` 기본이므로 이 값이 존재해도
    /// 기존 동작을 바꾸지 않는다. env 로만 정책을 조정하고 코드 재시작 없이
    /// 원복할 수 있도록 feature flag 로 관리한다.
    pub market_session: MarketSessionPolicy,
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

        let enable_real_trading = env_bool("KIS_ENABLE_REAL_TRADING", false);
        let is_real_like = matches!(environment, Environment::Real) || enable_real_trading;

        let allowed_codes: Vec<String> = std::env::var("KIS_ALLOWED_CODES")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let default_max_total = if is_real_like { 1 } else { 5 };
        let max_daily_trades_total = env_usize("KIS_MAX_DAILY_TRADES_TOTAL", default_max_total);

        let default_auto_start = false; // 실전/모의 공통: 기본 OFF
        let auto_start = env_bool("KIS_AUTO_START", default_auto_start);

        let real_or_stages: Vec<String> = std::env::var("KIS_REAL_OR_STAGES")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| vec!["15m".to_string()]);

        let entry_cutoff_real = std::env::var("KIS_ENTRY_CUTOFF_REAL")
            .ok()
            .and_then(|s| NaiveTime::parse_from_str(&format!("{s}:00"), "%H:%M:%S").ok())
            .unwrap_or_else(|| NaiveTime::from_hms_opt(15, 0, 0).unwrap());

        let default_execution_policy_kind = if is_real_like {
            ExecutionPolicyKind::MarketableLimit {
                tick_offset: 1,
                slippage_budget_pct: 0.003,
            }
        } else {
            ExecutionPolicyKind::PassiveZoneEdge
        };
        let execution_policy = std::env::var("KIS_EXECUTION_POLICY")
            .ok()
            .map(|v| v.trim().to_lowercase())
            .unwrap_or_else(|| match default_execution_policy_kind {
                ExecutionPolicyKind::PassiveZoneEdge => "passive".to_string(),
                ExecutionPolicyKind::MarketableLimit { .. } => "marketable".to_string(),
            });
        let marketable_tick_offset = env_u32("KIS_EXECUTION_MARKETABLE_TICK_OFFSET", 1).max(1);
        let marketable_slippage_budget_pct =
            env_f64("KIS_EXECUTION_SLIPPAGE_BUDGET_PCT", 0.003).clamp(0.0001, 0.05);
        let live_execution_policy_kind = match execution_policy.as_str() {
            "marketable" | "marketable_limit" | "marketable-limit" => {
                ExecutionPolicyKind::MarketableLimit {
                    tick_offset: marketable_tick_offset,
                    slippage_budget_pct: marketable_slippage_budget_pct,
                }
            }
            _ => ExecutionPolicyKind::PassiveZoneEdge,
        };
        let default_entry_fill_timeout_ms = match live_execution_policy_kind {
            // 2026-04-24 P1-1: paper passive 지정가 체결 tail 이 52.9초까지 관측되어
            // 기본 timeout 을 60초로 늘린다. 실전 marketable 경로는 별도 3초 유지.
            ExecutionPolicyKind::PassiveZoneEdge => 60_000,
            ExecutionPolicyKind::MarketableLimit { .. } => 3_000,
        };
        let default_poll_interval_ms = match live_execution_policy_kind {
            ExecutionPolicyKind::PassiveZoneEdge => 5_000,
            ExecutionPolicyKind::MarketableLimit { .. } => 1_000,
        };
        let live_entry_fill_timeout_ms =
            env_u64("KIS_ENTRY_FILL_TIMEOUT_MS", default_entry_fill_timeout_ms);
        let live_exit_verification_budget_ms = env_u64("KIS_EXIT_VERIFICATION_BUDGET_MS", 15_000);
        let live_poll_interval_ms = env_u64("KIS_POLL_INTERVAL_MS", default_poll_interval_ms);
        let live_manage_poll_interval_ms = env_u64("KIS_MANAGE_POLL_INTERVAL_MS", 3_000);

        Ok(Self {
            appkey: env_required("KIS_APPKEY")?,
            appsecret: env_required("KIS_APPSECRET")?,
            account_no: env_required("KIS_ACCOUNT_NO")?,
            account_product_code: std::env::var("KIS_ACCOUNT_PRODUCT_CODE")
                .unwrap_or_else(|_| "01".to_string()),
            environment,
            server_host: std::env::var("SERVER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            server_port: std::env::var("SERVER_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres@localhost:5433/kis_agent".to_string()),
            enable_real_trading,
            auto_start,
            require_db_in_real: env_bool("KIS_REQUIRE_DB_IN_REAL", true),
            allowed_codes,
            max_daily_trades_total,
            ws_stale_tick_secs: env_u64("KIS_WS_STALE_SECS", 120),
            ws_stale_message_secs: env_u64("KIS_WS_STALE_MESSAGE_SECS", 240),
            reconcile_secs: env_u64("KIS_RECONCILE_SECS", 60),
            reconcile_confirm_attempts: env_u32("KIS_RECONCILE_CONFIRM_ATTEMPTS", 3),
            reconcile_confirm_interval_secs: env_u64("KIS_RECONCILE_CONFIRM_INTERVAL", 5),
            entry_cutoff_real,
            real_or_stages,
            live_execution_policy_kind,
            live_entry_fill_timeout_ms,
            live_exit_verification_budget_ms,
            live_poll_interval_ms,
            live_manage_poll_interval_ms,
            market_session: MarketSessionPolicy::from_env(),
        })
    }

    /// 실전 판정. `KIS_ENVIRONMENT=real` 또는 `KIS_ENABLE_REAL_TRADING=true`.
    /// 두 조건 중 하나만 만족해도 "실전 취급" — 환경변수 오설정으로 실전 주문이
    /// 실수로 모의 플로우에 섞이는 것을 막기 위한 이중 가드.
    pub fn is_real_mode(&self) -> bool {
        matches!(self.environment, Environment::Real) || self.enable_real_trading
    }

    /// 자동매매에 허용된 종목 목록.
    ///
    /// 비어있는 경우:
    /// - 실전 모드: `["122630"]` 단일 종목으로 안전 기본값 적용. 2026-04-16 Go 조건 #2 에
    ///   따라 실전 첫날 운용 원칙과 일치한다 (하루 1회, 단일 종목, 수동 시작).
    /// - 모의 모드: 기존대로 2종목 `["122630", "114800"]` 허용.
    ///
    /// 명시적으로 `KIS_ALLOWED_CODES=` 로 값을 지정하면 그 목록이 우선한다.
    pub fn effective_allowed_codes(&self) -> Vec<String> {
        if !self.allowed_codes.is_empty() {
            return self.allowed_codes.clone();
        }
        if self.is_real_mode() {
            vec!["122630".to_string()]
        } else {
            vec!["122630".to_string(), "114800".to_string()]
        }
    }
}

fn env_required(key: &str) -> Result<String, String> {
    std::env::var(key).map_err(|_| format!("환경변수 {key}가 설정되지 않았습니다"))
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => {
            let s = v.trim().to_lowercase();
            matches!(s.as_str(), "true" | "1" | "on" | "yes")
        }
        Err(_) => default,
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_env() {
        for k in [
            "KIS_ENVIRONMENT",
            "KIS_ENABLE_REAL_TRADING",
            "KIS_AUTO_START",
            "KIS_REQUIRE_DB_IN_REAL",
            "KIS_ALLOWED_CODES",
            "KIS_MAX_DAILY_TRADES_TOTAL",
            "KIS_WS_STALE_SECS",
            "KIS_WS_STALE_MESSAGE_SECS",
            "KIS_RECONCILE_SECS",
            "KIS_RECONCILE_CONFIRM_ATTEMPTS",
            "KIS_RECONCILE_CONFIRM_INTERVAL",
            "KIS_ENTRY_CUTOFF_REAL",
            "KIS_REAL_OR_STAGES",
            "KIS_EXECUTION_POLICY",
            "KIS_EXECUTION_MARKETABLE_TICK_OFFSET",
            "KIS_EXECUTION_SLIPPAGE_BUDGET_PCT",
            "KIS_ENTRY_FILL_TIMEOUT_MS",
            "KIS_EXIT_VERIFICATION_BUDGET_MS",
            "KIS_POLL_INTERVAL_MS",
            "KIS_MANAGE_POLL_INTERVAL_MS",
            "KIS_OFF_HOURS_SUPPRESSION_ENABLED",
            "KIS_PRE_OPEN_CONNECT_AT",
            "KIS_REGULAR_OPEN_AT",
            "KIS_REGULAR_CLOSE_AT",
            "KIS_CLOSE_SETTLE_END_AT",
            "KIS_OFF_HOURS_WS_BACKOFF_MS",
        ] {
            unsafe {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn is_real_mode_via_environment() {
        reset_env();
        let mut cfg = dummy_config();
        cfg.environment = Environment::Real;
        cfg.enable_real_trading = false;
        assert!(cfg.is_real_mode());
    }

    #[test]
    fn is_real_mode_via_enable_flag() {
        reset_env();
        let mut cfg = dummy_config();
        cfg.environment = Environment::Paper;
        cfg.enable_real_trading = true;
        assert!(cfg.is_real_mode());
    }

    #[test]
    fn is_real_mode_false_when_both_off() {
        reset_env();
        let mut cfg = dummy_config();
        cfg.environment = Environment::Paper;
        cfg.enable_real_trading = false;
        assert!(!cfg.is_real_mode());
    }

    #[test]
    fn effective_allowed_codes_defaults_to_two_etfs() {
        reset_env();
        let mut cfg = dummy_config();
        cfg.allowed_codes = Vec::new();
        let codes = cfg.effective_allowed_codes();
        assert_eq!(codes, vec!["122630".to_string(), "114800".to_string()]);
    }

    #[test]
    fn effective_allowed_codes_respects_whitelist() {
        reset_env();
        let mut cfg = dummy_config();
        cfg.allowed_codes = vec!["122630".to_string()];
        let codes = cfg.effective_allowed_codes();
        assert_eq!(codes, vec!["122630".to_string()]);
    }

    fn dummy_config() -> AppConfig {
        AppConfig {
            appkey: String::new(),
            appsecret: String::new(),
            account_no: String::new(),
            account_product_code: "01".to_string(),
            environment: Environment::Paper,
            server_host: "127.0.0.1".to_string(),
            server_port: 3000,
            database_url: String::new(),
            enable_real_trading: false,
            auto_start: false,
            require_db_in_real: true,
            allowed_codes: Vec::new(),
            max_daily_trades_total: 5,
            ws_stale_tick_secs: 120,
            ws_stale_message_secs: 240,
            reconcile_secs: 60,
            reconcile_confirm_attempts: 3,
            reconcile_confirm_interval_secs: 5,
            entry_cutoff_real: NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
            real_or_stages: vec!["15m".to_string()],
            live_execution_policy_kind: ExecutionPolicyKind::PassiveZoneEdge,
            live_entry_fill_timeout_ms: 60_000,
            live_exit_verification_budget_ms: 15_000,
            live_poll_interval_ms: 5_000,
            live_manage_poll_interval_ms: 3_000,
            market_session: MarketSessionPolicy::default_off(),
        }
    }
}
