use std::sync::Arc;

use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use kis_agent::application::services::account_service::AccountService;
use kis_agent::application::services::market_data_service::MarketDataService;
use kis_agent::application::services::trading_service::TradingService;
use kis_agent::config::AppConfig;
use kis_agent::domain::ports::realtime::RealtimeData;
use kis_agent::infrastructure::cache::sqlite_cache::SqliteCache;
use kis_agent::infrastructure::kis_client::account::KisAccountAdapter;
use kis_agent::infrastructure::kis_client::auth::TokenManager;
use kis_agent::infrastructure::kis_client::http_client::KisHttpClient;
use kis_agent::infrastructure::kis_client::quotations::KisMarketDataAdapter;
use kis_agent::infrastructure::kis_client::rate_limiter::KisRateLimiter;
use kis_agent::infrastructure::kis_client::trading::KisTradingAdapter;
use kis_agent::presentation::app_state::AppState;
use kis_agent::presentation::routes::create_router;

#[tokio::main]
async fn main() {
    // 로깅 초기화
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kis_agent=info,tower_http=info".parse().unwrap()),
        )
        .init();

    // 설정 로드
    let config = match AppConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("설정 로드 실패: {e}");
            eprintln!("환경변수 KIS_APPKEY, KIS_APPSECRET, KIS_ACCOUNT_NO를 설정하세요.");
            std::process::exit(1);
        }
    };

    info!(
        "KIS Agent 시작 — 환경: {:?}, 서버: {}:{}",
        config.environment, config.server_host, config.server_port
    );

    // 인프라 구성
    let token_manager = Arc::new(TokenManager::new(
        config.appkey.clone(),
        config.appsecret.clone(),
        config.environment,
    ));
    let rate_limiter = Arc::new(KisRateLimiter::new(config.environment));

    let http_client = Arc::new(KisHttpClient::new(
        Arc::clone(&token_manager),
        Arc::clone(&rate_limiter),
        config.environment,
        config.account_no.clone(),
        config.account_product_code.clone(),
    ));

    // SQLite 캐시
    let sqlite_cache = match SqliteCache::new("sqlite:kis_cache.db?mode=rwc").await {
        Ok(cache) => Some(Arc::new(cache)),
        Err(e) => {
            tracing::warn!("SQLite 캐시 초기화 실패 (인메모리만 사용): {e}");
            None
        }
    };

    // 어댑터 생성
    let market_data_adapter = Arc::new(KisMarketDataAdapter::new(Arc::clone(&http_client)));
    let trading_adapter = Arc::new(KisTradingAdapter::new(Arc::clone(&http_client)));
    let account_adapter = Arc::new(KisAccountAdapter::new(Arc::clone(&http_client)));

    // 서비스 생성
    let market_data_service = Arc::new(MarketDataService::new(market_data_adapter, sqlite_cache));
    let trading_service = Arc::new(TradingService::new(trading_adapter));
    let account_service = Arc::new(AccountService::new(account_adapter));

    // 실시간 데이터 채널
    let (realtime_tx, _) = broadcast::channel::<RealtimeData>(256);

    // 앱 상태
    let state = AppState {
        market_data: market_data_service,
        trading: trading_service,
        account: account_service,
        realtime_tx,
    };

    // CORS 미들웨어
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // 라우터 구성
    let app = create_router(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // 서버 시작
    let addr = format!("{}:{}", config.server_host, config.server_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("서버 바인드 실패");

    info!("서버 시작: http://{addr}");
    info!("API 문서: http://{addr}/api/v1/health");

    axum::serve(listener, app)
        .await
        .expect("서버 실행 실패");
}
