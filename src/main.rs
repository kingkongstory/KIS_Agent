use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use kis_agent::application::services::account_service::AccountService;
use kis_agent::application::services::market_data_service::MarketDataService;
use kis_agent::application::services::trading_service::TradingService;
use kis_agent::config::AppConfig;
use kis_agent::domain::ports::realtime::RealtimeData;
use kis_agent::domain::types::StockCode;
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
use kis_agent::strategy::backtest::BacktestEngine;
use kis_agent::strategy::live_runner::LiveRunner;

#[derive(Parser)]
#[command(name = "kis-agent", about = "KIS 자동매매 에이전트")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// ORB+FVG 전략 실시간 모의투자
    Trade {
        /// 종목코드 (6자리, 예: 005930)
        stock_code: String,
        /// 손익비 (기본: 2.0)
        #[arg(long, default_value = "2.0")]
        rr: f64,
        /// 주문 수량 (기본: 1)
        #[arg(long, default_value = "1")]
        qty: u64,
    },
    /// ORB+FVG 전략 백테스트
    Backtest {
        /// 종목코드 (6자리, 예: 005930)
        stock_code: String,
        /// 테스트 일수 (기본: 30)
        #[arg(long, default_value = "30")]
        days: usize,
        /// 손익비 (기본: 2.0)
        #[arg(long, default_value = "2.0")]
        rr: f64,
        /// 트레일링 간격 R배수 (기본: 0.1)
        #[arg(long, default_value = "0.1")]
        trail: f64,
        /// 시간스탑 캔들 수 (기본: 3 = 15분)
        #[arg(long, default_value = "3")]
        tstop: usize,
        /// 본전스탑 활성화 R배수 (기본: 0.3)
        #[arg(long, default_value = "0.3")]
        be_r: f64,
        /// FVG 유효 캔들 수 (기본: 6 = 30분)
        #[arg(long, default_value = "6")]
        fvg_exp: usize,
        /// 2차진입 최소 1차수익률 % (기본: 0.0 = 수익이면 무조건)
        #[arg(long, default_value = "0.0")]
        min2nd: f64,
    },
    /// 네이버 금융 일봉 수집
    CollectDaily,
    /// KIS API 당일 분봉 수집
    CollectMinute,
    /// 네이버 금융 분봉 수집 (근사 OHLCV)
    CollectMinuteNaver {
        /// 종목코드 (쉼표 구분, 예: 122630,005930)
        #[arg(long, default_value = "122630")]
        codes: String,
        /// 분봉 간격 (기본: 1)
        #[arg(long, default_value = "1")]
        interval: i16,
    },
    /// Yahoo Finance 분봉 수집 (OHLCV, 최대 1달)
    CollectYahoo {
        /// 종목코드 (쉼표 구분, 예: 069500,122630)
        #[arg(long, default_value = "069500,122630")]
        codes: String,
        /// 분봉 간격 (5m, 15m 등)
        #[arg(long, default_value = "5m")]
        interval: String,
        /// 조회 범위 (1mo, 5d 등)
        #[arg(long, default_value = "1mo")]
        range: String,
    },
    /// 웹 서버 시작
    Server,
}

/// KIS HTTP 클라이언트 생성 헬퍼
fn create_kis_client(config: &AppConfig) -> Arc<KisHttpClient> {
    let token_manager = Arc::new(TokenManager::new(
        config.appkey.clone(),
        config.appsecret.clone(),
        config.environment,
    ));
    let rate_limiter = Arc::new(KisRateLimiter::new(config.environment));
    Arc::new(KisHttpClient::new(
        token_manager,
        rate_limiter,
        config.environment,
        config.account_no.clone(),
        config.account_product_code.clone(),
    ))
}

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

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Trade { stock_code, rr, qty }) => {
            run_trade(&config, &stock_code, rr, qty).await;
        }
        Some(Commands::Backtest { stock_code, days, rr, trail, tstop, be_r, fvg_exp, min2nd }) => {
            run_backtest(&config, &stock_code, days, rr, trail, tstop, be_r, fvg_exp, min2nd).await;
        }
        Some(Commands::CollectDaily) => {
            run_collect_daily(&config).await;
        }
        Some(Commands::CollectMinute) => {
            run_collect_minute(&config).await;
        }
        Some(Commands::CollectMinuteNaver { codes, interval }) => {
            run_collect_minute_naver(&config, &codes, interval).await;
        }
        Some(Commands::CollectYahoo { codes, interval, range }) => {
            run_collect_yahoo(&config, &codes, &interval, &range).await;
        }
        Some(Commands::Server) | None => {
            run_server(config).await;
        }
    }
}

/// ORB+FVG 실시간 트레이딩
async fn run_trade(config: &AppConfig, stock_code: &str, rr: f64, qty: u64) {
    let code = match StockCode::new(stock_code) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("종목코드 오류: {e}");
            std::process::exit(1);
        }
    };

    let client = create_kis_client(config);
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let runner = LiveRunner::new(client, code, stock_code.to_string(), qty, stop_flag);

    match runner.run().await {
        Ok(trades) if !trades.is_empty() => {
            let pnl: f64 = trades.iter().map(|t| t.pnl_pct()).sum();
            info!("거래 완료: {}건, 총 손익={:.2}%", trades.len(), pnl);
        }
        Ok(_) => {
            info!("오늘 거래 신호 없음");
        }
        Err(e) => {
            eprintln!("트레이딩 에러: {e}");
            std::process::exit(1);
        }
    }
}

/// ORB+FVG 백테스트 (DB 기반)
async fn run_backtest(
    config: &AppConfig, stock_code: &str, days: usize, rr: f64,
    trail: f64, tstop: usize, be_r: f64, fvg_exp: usize, min2nd: f64,
) {
    use kis_agent::strategy::orb_fvg::OrbFvgConfig;

    let store = Arc::new(
        PostgresStore::new(&config.database_url)
            .await
            .expect("PostgreSQL 연결 실패"),
    );

    let mut strategy_config = OrbFvgConfig::default();
    strategy_config.rr_ratio = rr;
    strategy_config.trailing_r = trail;
    strategy_config.time_stop_candles = tstop;
    strategy_config.breakeven_r = be_r;
    strategy_config.fvg_expiry_candles = fvg_exp;
    strategy_config.min_first_pnl_for_second = min2nd;

    let source_interval = 5_i16;
    let engine = BacktestEngine::with_config(store, strategy_config, source_interval);

    info!("=== ORB+FVG 백테스트 시작 ===");
    info!("종목: {stock_code}, 기간: {days}일, RR: 1:{rr:.1}");

    match engine.run(stock_code, days).await {
        Ok(report) => {
            println!("{report}");
        }
        Err(e) => {
            eprintln!("백테스트 에러: {e}");
            std::process::exit(1);
        }
    }
}

/// Yahoo Finance 분봉 수집
async fn run_collect_yahoo(config: &AppConfig, codes: &str, interval: &str, range: &str) {
    use kis_agent::infrastructure::collector::yahoo_minute::YahooMinuteCollector;

    // interval 문자열에서 분 단위 추출 (e.g. "5m" → 5)
    let db_interval_min: i16 = interval
        .trim_end_matches('m')
        .parse()
        .unwrap_or(5);

    info!("=== Yahoo Finance 분봉 수집 시작 ({interval}, {range}) ===");

    let store = Arc::new(
        PostgresStore::new(&config.database_url)
            .await
            .expect("PostgreSQL 연결 실패"),
    );
    let collector = YahooMinuteCollector::new(store);

    let stock_codes: Vec<&str> = codes.split(',').map(|s| s.trim()).collect();
    match collector
        .collect_batch(&stock_codes, interval, range, db_interval_min)
        .await
    {
        Ok(count) => info!("Yahoo 분봉 수집 완료: 총 {count}건"),
        Err(e) => eprintln!("Yahoo 분봉 수집 실패: {e}"),
    }
}

/// 네이버 금융 분봉 수집
async fn run_collect_minute_naver(config: &AppConfig, codes: &str, interval: i16) {
    use kis_agent::infrastructure::collector::naver_minute::NaverMinuteCollector;

    info!("=== 네이버 금융 분봉 수집 시작 ({}분봉) ===", interval);

    let store = Arc::new(
        PostgresStore::new(&config.database_url)
            .await
            .expect("PostgreSQL 연결 실패"),
    );
    let collector = NaverMinuteCollector::new(store);

    let stock_codes: Vec<&str> = codes.split(',').map(|s| s.trim()).collect();
    match collector.collect_batch(&stock_codes, interval).await {
        Ok(count) => info!("분봉 수집 완료: 총 {count}건"),
        Err(e) => eprintln!("분봉 수집 실패: {e}"),
    }
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
        strategy_manager: {
            let mgr = kis_agent::presentation::routes::strategy::StrategyManager::new();
            mgr.set_client(Arc::clone(&http_client));
            mgr
        },
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
