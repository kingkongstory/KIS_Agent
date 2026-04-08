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
use kis_agent::infrastructure::cache::postgres_store::PostgresStore;
use kis_agent::infrastructure::cache::sqlite_cache::SqliteCache;
use kis_agent::infrastructure::collector::kis_minute::KisMinuteCollector;
use kis_agent::infrastructure::collector::naver_daily::NaverDailyCollector;
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

    // CLI 서브커맨드 처리
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "collect-daily" => {
                run_collect_daily(&config).await;
                return;
            }
            "collect-minute" => {
                run_collect_minute(&config).await;
                return;
            }
            _ => {
                eprintln!("사용법:");
                eprintln!("  cargo run                  — 웹 서버 시작");
                eprintln!("  cargo run -- collect-daily  — 네이버 금융 일봉 수집");
                eprintln!("  cargo run -- collect-minute — KIS API 당일 분봉 수집");
                std::process::exit(1);
            }
        }
    }

    // 웹 서버 모드
    run_server(config).await;
}

/// 네이버 금융 일봉 + 지수 수집
async fn run_collect_daily(config: &AppConfig) {
    info!("=== 네이버 금융 일봉 수집 시작 ===");

    let store = Arc::new(
        PostgresStore::new(&config.database_url)
            .await
            .expect("PostgreSQL 연결 실패"),
    );
    let collector = NaverDailyCollector::new(Arc::clone(&store));

    // 수집 기간: 최근 3년
    let end_date = chrono::Local::now().format("%Y%m%d").to_string();
    let start_date = {
        let now = chrono::Local::now();
        (now - chrono::Duration::days(365 * 3)).format("%Y%m%d").to_string()
    };

    info!("수집 기간: {start_date} ~ {end_date}");

    // 1. 코스피/코스닥 지수 수집
    info!("--- 지수 수집 ---");
    match collector.collect_all_indices(&start_date, &end_date).await {
        Ok(count) => info!("지수 수집 완료: {count}건"),
        Err(e) => tracing::error!("지수 수집 실패: {e}"),
    }

    // 2. 주요 종목 일봉 수집
    info!("--- 종목 일봉 수집 ---");
    let major_stocks = [
        "005930", // 삼성전자
        "000660", // SK하이닉스
        "373220", // LG에너지솔루션
        "207940", // 삼성바이오로직스
        "005380", // 현대차
        "000270", // 기아
        "068270", // 셀트리온
        "035420", // NAVER
        "035720", // 카카오
        "051910", // LG화학
        "006400", // 삼성SDI
        "028260", // 삼성물산
        "105560", // KB금융
        "055550", // 신한지주
        "003670", // 포스코퓨처엠
        "066570", // LG전자
        "034730", // SK
        "015760", // 한국전력
        "003550", // LG
        "032830", // 삼성생명
    ];

    match collector
        .collect_stocks_batch(&major_stocks, &start_date, &end_date)
        .await
    {
        Ok(count) => info!("종목 일봉 수집 완료: 총 {count}건"),
        Err(e) => tracing::error!("종목 수집 실패: {e}"),
    }

    info!("=== 수집 완료 ===");
}

/// KIS API 당일 분봉 수집
async fn run_collect_minute(config: &AppConfig) {
    info!("=== KIS API 당일 분봉 수집 시작 ===");

    let store = Arc::new(
        PostgresStore::new(&config.database_url)
            .await
            .expect("PostgreSQL 연결 실패"),
    );

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

    let collector = KisMinuteCollector::new(http_client, store);

    let stocks = [
        "005930", // 삼성전자
        "000660", // SK하이닉스
        "005380", // 현대차
        "035420", // NAVER
        "035720", // 카카오
    ];

    match collector.collect_today_batch(&stocks).await {
        Ok(count) => info!("분봉 수집 완료: 총 {count}건"),
        Err(e) => tracing::error!("분봉 수집 실패: {e}"),
    }

    info!("=== 수집 완료 ===");
}

/// 웹 서버 실행
async fn run_server(config: AppConfig) {
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

    // SQLite 캐시 (기존 호환)
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
