use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

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
        /// 손익비 (기본: 2.5)
        #[arg(long, default_value = "2.5")]
        rr: f64,
        /// 주문 수량 (기본: 1)
        #[arg(long, default_value = "1")]
        qty: u64,
    },
    /// ORB+FVG 전략 백테스트
    Backtest {
        /// 종목코드 (단일: 122630, dual-locked: 122630,114800)
        stock_code: String,
        /// 테스트 일수 (기본: 30)
        #[arg(long, default_value = "30")]
        days: usize,
        /// 손익비 (기본: 2.5)
        #[arg(long, default_value = "2.5")]
        rr: f64,
        /// 트레일링 간격 R배수 (기본: 0.05)
        #[arg(long, default_value = "0.05")]
        trail: f64,
        /// 시간스탑 캔들 수 (기본: 3 = 15분)
        #[arg(long, default_value = "3")]
        tstop: usize,
        /// 본전스탑 활성화 R배수 (기본: 0.15)
        #[arg(long, default_value = "0.15")]
        be_r: f64,
        /// FVG 유효 캔들 수 (기본: 6 = 30분)
        #[arg(long, default_value = "6")]
        fvg_exp: usize,
        /// 2차진입 최소 1차수익률 % (기본: 0.0)
        #[arg(long, default_value = "0.0")]
        min2nd: f64,
        /// Dynamic Target: 과거 N일 OR 돌파 확장폭 평균으로 RR 설정 (0=비활성)
        #[arg(long, default_value = "0")]
        dynamic_lookback: usize,
        /// Session Reset: 오전/오후 세션 독립 OR 생성
        #[arg(long, default_value = "false")]
        session_reset: bool,
        /// Multi-Stage ORB: 5분/15분/30분 OR 동시 추적
        #[arg(long, default_value = "false")]
        multi_stage: bool,
        /// 두 종목 통합 position_lock (종목코드를 쉼표로 구분: 122630,114800)
        #[arg(long, default_value = "false")]
        dual_locked: bool,
        /// 일일 최대 거래 횟수 (기본: 5, 실전값 일치)
        #[arg(long, default_value = "5")]
        max_trades: usize,
        /// 진입 마감 시각 HH:MM (기본: 15:20)
        #[arg(long, default_value = "15:20")]
        cutoff: String,
        /// 강제 청산 시각 HH:MM (기본: 15:25)
        #[arg(long, default_value = "15:25")]
        force_exit: String,
        /// Parity 백테스트 실행 (passive|legacy). 빈 문자열이면 legacy 경로 사용.
        /// passive = 현재 라이브 규칙(gap.top/bottom 지정가 + 30초 timeout) 재현.
        /// legacy = 기존 mid_price 즉시 체결 (parity 경로로 재현).
        #[arg(long, default_value = "")]
        parity: String,
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
        /// 종목코드 (쉼표 구분, 예: 122630,114800,233740,251340)
        #[arg(long, default_value = "122630,114800,233740,251340")]
        codes: String,
        /// 분봉 간격 (5m, 15m 등)
        #[arg(long, default_value = "5m")]
        interval: String,
        /// 조회 범위 (1mo, 5d 등)
        #[arg(long, default_value = "1mo")]
        range: String,
    },
    /// Replay — 단일 날짜를 legacy / parity-legacy / parity-passive 세 경로로 재생
    Replay {
        /// 종목코드
        #[arg(default_value = "122630")]
        stock_code: String,
        /// 날짜 (YYYY-MM-DD)
        #[arg(long)]
        date: String,
        /// RR (손익비, 기본: 2.5)
        #[arg(long, default_value = "2.5")]
        rr: f64,
    },
    /// 웹 서버 시작
    Server,
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
        Some(Commands::Backtest { stock_code, days, rr, trail, tstop, be_r, fvg_exp, min2nd, dynamic_lookback, session_reset, multi_stage, dual_locked, max_trades, cutoff, force_exit, parity }) => {
            run_backtest(&config, &stock_code, days, rr, trail, tstop, be_r, fvg_exp, min2nd, dynamic_lookback, session_reset, multi_stage, dual_locked, max_trades, &cutoff, &force_exit, &parity).await;
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
        Some(Commands::Replay { stock_code, date, rr }) => {
            run_replay(&config, &stock_code, &date, rr).await;
        }
        Some(Commands::Server) | None => {
            run_server(config).await;
        }
    }
}

/// ORB+FVG 실시간 트레이딩 (CLI 단독 실행)
async fn run_trade(config: &AppConfig, stock_code: &str, _rr: f64, _qty: u64) {
    let code = match StockCode::new(stock_code) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("종목코드 오류: {e}");
            std::process::exit(1);
        }
    };

    // 인프라 구성 (웹 서버와 동일한 파이프라인)
    let token_manager = Arc::new(TokenManager::new(
        config.appkey.clone(), config.appsecret.clone(), config.environment,
    ));
    let rate_limiter = Arc::new(KisRateLimiter::new(config.environment));
    let client = Arc::new(KisHttpClient::new(
        Arc::clone(&token_manager), Arc::clone(&rate_limiter),
        config.environment, config.account_no.clone(), config.account_product_code.clone(),
    ));

    // DB 연결
    let pg_store = match PostgresStore::new(&config.database_url).await {
        Ok(store) => {
            info!("PostgreSQL 연결 완료");
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("PostgreSQL 연결 실패 (DB 없이 실행): {e}");
            None
        }
    };

    // 실시간 데이터 채널
    let (realtime_tx, _) = broadcast::channel::<RealtimeData>(1024);

    // WebSocket 실시간 스트리밍
    {
        use kis_agent::infrastructure::websocket::connection::KisWebSocketClient;
        let ws_client = Arc::new(KisWebSocketClient::new(
            Arc::clone(&token_manager), config.environment, realtime_tx.clone(),
        ));
        let sub_mgr = ws_client.subscription_manager();
        sub_mgr.add("H0STCNT0", stock_code).await;
        sub_mgr.add("H0STASP0", stock_code).await;
        sub_mgr.add("H0STMKO0", stock_code).await;
        let hts_id = std::env::var("KIS_HTS_ID").unwrap_or_default();
        if !hts_id.is_empty() {
            sub_mgr.add("H0STCNI9", &hts_id).await;
        }
        tokio::spawn(async move { ws_client.run().await });
        info!("WebSocket 실시간 스트리밍 시작 ({})", stock_code);
    }

    // 분봉 집계기
    let ws_candles = {
        use kis_agent::infrastructure::websocket::candle_aggregator::CandleAggregator;
        let mut agg = CandleAggregator::new();
        if let Some(ref store) = pg_store {
            agg = agg.with_store(Arc::clone(store));
        }
        let aggregator = Arc::new(agg);
        aggregator.preload_from_db(&[stock_code]).await;
        aggregator.backfill_or(&[stock_code], false).await;
        let candles = aggregator.completed_candles();
        let rx = realtime_tx.subscribe();
        Arc::clone(&aggregator).spawn(rx, realtime_tx.clone());
        candles
    };

    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut runner = LiveRunner::new(
        Arc::clone(&client), code, stock_code.to_string(), 0, stop_flag,
    );
    runner = runner.with_trade_tx(realtime_tx.clone());
    runner = runner.with_ws_candles(ws_candles);
    if let Some(ref store) = pg_store {
        runner = runner.with_db_store(Arc::clone(store));
        runner = runner.with_event_logger(Arc::new(
            kis_agent::infrastructure::monitoring::event_logger::EventLogger::new(store.pool().clone()),
        ));
    }

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
#[allow(clippy::too_many_arguments)]
async fn run_backtest(
    config: &AppConfig, stock_code: &str, days: usize, rr: f64,
    trail: f64, tstop: usize, be_r: f64, fvg_exp: usize, min2nd: f64,
    dynamic_lookback: usize, session_reset: bool, multi_stage: bool, dual_locked: bool,
    max_trades: usize, cutoff: &str, force_exit: &str, parity: &str,
) {
    use kis_agent::strategy::orb_fvg::OrbFvgConfig;

    let store = match PostgresStore::new(&config.database_url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("백테스트 실패 — PostgreSQL 연결 불가: {e}");
            eprintln!("DATABASE_URL 환경변수와 DB 기동 상태를 확인하세요.");
            std::process::exit(2);
        }
    };

    let mut strategy_config = OrbFvgConfig::default();
    strategy_config.rr_ratio = rr;
    strategy_config.trailing_r = trail;
    strategy_config.time_stop_candles = tstop;
    strategy_config.breakeven_r = be_r;
    strategy_config.fvg_expiry_candles = fvg_exp;
    strategy_config.min_first_pnl_for_second = min2nd;
    strategy_config.max_daily_trades = max_trades;
    if let Ok(t) = chrono::NaiveTime::parse_from_str(&format!("{}:00", cutoff), "%H:%M:%S") {
        strategy_config.entry_cutoff = t;
    }
    if let Ok(t) = chrono::NaiveTime::parse_from_str(&format!("{}:00", force_exit), "%H:%M:%S") {
        strategy_config.force_exit = t;
    }

    let source_interval = 5_i16;
    let engine = BacktestEngine::with_config(store, strategy_config, source_interval);

    // Parity 경로 — SignalEngine + ExecutionPolicy + PositionManager 조합.
    if !parity.is_empty() {
        info!("=== Parity 백테스트 시작 (policy={parity}) ===");
        info!("종목: {stock_code}, 기간: {days}일, RR: 1:{rr:.1}");
        match engine.run_parity(stock_code, days, parity).await {
            Ok(report) => println!("{report}"),
            Err(e) => {
                eprintln!("parity 백테스트 에러: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if dual_locked {
        let codes: Vec<&str> = stock_code.split(',').map(|s| s.trim()).collect();
        if codes.len() != 2 {
            eprintln!("dual-locked 모드는 종목코드 2개를 쉼표로 구분해야 합니다 (예: 122630,114800)");
            std::process::exit(1);
        }
        let (code_a, code_b) = (codes[0], codes[1]);
        info!("=== 두 종목 통합 백테스트 (position_lock) 시작 ===");
        info!("{code_a} + {code_b}, 기간: {days}일, Multi-Stage: {multi_stage}");
        match engine.run_dual_locked(code_a, code_b, days, multi_stage).await {
            Ok((report_a, report_b)) => {
                println!("{report_a}");
                println!("{report_b}");
                let total_pnl = report_a.total_pnl_pct + report_b.total_pnl_pct;
                let total_trades = report_a.total_trades + report_b.total_trades;
                let total_wins = report_a.wins + report_b.wins;
                let actual_days = report_a.days_tested.max(1);
                println!("\n=== 합산 ===");
                println!("  실제 테스트 일수: {}일 (CLI --days={})", report_a.days_tested, days);
                println!("  총 거래: {}회 (승리 {}회)", total_trades, total_wins);
                println!("  총 손익: {:.2}%", total_pnl);
                println!("  일평균: {:.2}%", total_pnl / actual_days as f64);
            }
            Err(e) => { eprintln!("백테스트 에러: {e}"); std::process::exit(1); }
        }
    } else if multi_stage {
        info!("=== Multi-Stage ORB 백테스트 시작 ===");
        info!("종목: {stock_code}, 기간: {days}일 (5분/15분/30분 OR 동시 추적)");
        match engine.run_multi_stage(stock_code, days).await {
            Ok(report) => println!("{report}"),
            Err(e) => { eprintln!("백테스트 에러: {e}"); std::process::exit(1); }
        }
    } else if session_reset {
        info!("=== Session Reset 백테스트 시작 ===");
        info!("종목: {stock_code}, 기간: {days}일 (오전+오후 독립 세션)");
        match engine.run_session_reset(stock_code, days).await {
            Ok(report) => println!("{report}"),
            Err(e) => { eprintln!("백테스트 에러: {e}"); std::process::exit(1); }
        }
    } else if dynamic_lookback > 0 {
        info!("=== Dynamic Target 백테스트 시작 ===");
        info!("종목: {stock_code}, 기간: {days}일, lookback: {dynamic_lookback}일");
        match engine.run_dynamic_target(stock_code, days, dynamic_lookback).await {
            Ok(report) => println!("{report}"),
            Err(e) => { eprintln!("백테스트 에러: {e}"); std::process::exit(1); }
        }
    } else {
        info!("=== ORB+FVG 백테스트 시작 ===");
        info!("종목: {stock_code}, 기간: {days}일, RR: 1:{rr:.1}");
        match engine.run(stock_code, days).await {
            Ok(report) => println!("{report}"),
            Err(e) => { eprintln!("백테스트 에러: {e}"); std::process::exit(1); }
        }
    }
}

/// Candle replay comparison — 단일 날짜를 세 경로(legacy / parity-legacy / parity-passive)로 비교
async fn run_replay(config: &AppConfig, stock_code: &str, date_str: &str, rr: f64) {
    use chrono::NaiveDate;
    use kis_agent::strategy::orb_fvg::OrbFvgConfig;

    let date = match NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("날짜 형식 오류 (YYYY-MM-DD): {e}");
            std::process::exit(1);
        }
    };

    let store = match PostgresStore::new(&config.database_url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("replay 실패 — PostgreSQL 연결 불가: {e}");
            eprintln!("DATABASE_URL 환경변수와 DB 기동 상태를 확인하세요.");
            std::process::exit(2);
        }
    };

    let mut strategy_config = OrbFvgConfig::default();
    strategy_config.rr_ratio = rr;

    let engine = BacktestEngine::with_config(store, strategy_config, 5);

    info!("=== Candle Replay 비교 시작 ({stock_code}, {date}, RR=1:{rr:.1}) ===");
    match engine.replay_day(stock_code, date).await {
        Ok(report) => println!("{report}"),
        Err(e) => {
            eprintln!("replay 에러: {e}");
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

    let store = match PostgresStore::new(&config.database_url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Yahoo 분봉 수집 실패 — PostgreSQL 연결 불가: {e}");
            std::process::exit(2);
        }
    };
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

    let store = match PostgresStore::new(&config.database_url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("네이버 분봉 수집 실패 — PostgreSQL 연결 불가: {e}");
            std::process::exit(2);
        }
    };
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

    let store = match PostgresStore::new(&config.database_url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("네이버 일봉 수집 실패 — PostgreSQL 연결 불가: {e}");
            std::process::exit(2);
        }
    };
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

    let store = match PostgresStore::new(&config.database_url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("KIS 분봉 수집 실패 — PostgreSQL 연결 불가: {e}");
            std::process::exit(2);
        }
    };

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
        "122630", // KODEX 레버리지
        "114800", // KODEX 인버스
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

    // PostgreSQL 저장소 (분봉 + 거래 기록 영속화).
    //
    // 2026-04-16 production-readiness — 실전 모드에서 DB 연결 실패는 즉시 종료 사유다.
    // 메모리 전용 fallback 은 당일 OR / active_positions / trades 를 잃어 재시작 복구가
    // 불가능하며, search_after 추적과 event_log 관측도 끊겨 cancel/fill race 를 복원
    // 불가한 상태로 만든다. 실전에서는 차라리 기동 실패가 안전하다.
    let pg_store = match PostgresStore::new(&config.database_url).await {
        Ok(store) => {
            info!("PostgreSQL 연결 완료 (실시간 데이터 저장 활성)");
            Some(Arc::new(store))
        }
        Err(e) => {
            if config.is_real_mode() && config.require_db_in_real {
                eprintln!(
                    "[FATAL] 실전 모드(is_real_mode=true, require_db_in_real=true) — \
                     PostgreSQL 연결 필수. DB 기동/DATABASE_URL 확인 후 재시도하세요: {e}"
                );
                tracing::error!("실전 모드 DB 연결 실패 — 기동 중단: {e}");
                std::process::exit(3);
            }
            tracing::warn!("PostgreSQL 연결 실패 (메모리만 사용): {e}");
            None
        }
    };

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
    use kis_agent::infrastructure::cache::memory_cache::MemoryCache;
    let price_cache = Arc::new(MemoryCache::new(std::time::Duration::from_secs(5)));
    let orderbook_cache = Arc::new(MemoryCache::new(std::time::Duration::from_secs(3)));
    let market_data_service = Arc::new(MarketDataService::new(
        market_data_adapter, price_cache, orderbook_cache, sqlite_cache,
    ));
    let trading_service = Arc::new(TradingService::new(trading_adapter));
    let account_service = Arc::new(AccountService::new(account_adapter));

    // 실시간 데이터 채널
    let (realtime_tx, _) = broadcast::channel::<RealtimeData>(1024);

    // 운영 이벤트 로거 (fire-and-forget 비동기 DB 저장).
    // WebSocket 재연결, API 에러 등 system 이벤트를 기록하려면 이 시점에 먼저 만들어야 함.
    let event_logger = pg_store.as_ref().map(|store| {
        Arc::new(kis_agent::infrastructure::monitoring::event_logger::EventLogger::new(
            store.pool().clone(),
        ))
    });
    // HTTP 클라이언트에 사후 주입 (api_error 이벤트 기록 활성화)
    if let Some(ref el) = event_logger {
        http_client.set_event_logger(Arc::clone(el));
    }

    // 자동매매 허용 종목 결정 — `KIS_ALLOWED_CODES` 환경변수 또는 기본 2종목.
    // 실전 초기에는 122630 단일 운용을 권장하지만 스펙상 2종목 모두 기본 허용.
    let allowed_codes = config.effective_allowed_codes();
    info!("자동매매 허용 종목: {:?}", allowed_codes);

    // KIS WebSocket 실시간 스트리밍 (체결 + 호가)
    // 2026-04-16 P0: ws_client handle을 유지하여 체결통보 health gate에서 사용.
    let ws_client_handle = {
        use kis_agent::infrastructure::websocket::connection::KisWebSocketClient;

        let mut ws_client = KisWebSocketClient::new(
            Arc::clone(&token_manager),
            config.environment,
            realtime_tx.clone(),
        );
        if let Some(ref el) = event_logger {
            ws_client = ws_client.with_event_logger(Arc::clone(el));
        }
        let ws_client = Arc::new(ws_client);

        // 대상 종목 구독 등록 (연결 시 자동 전송)
        let sub_mgr = ws_client.subscription_manager();
        for code in &allowed_codes {
            sub_mgr.add("H0STCNT0", code).await; // 체결
            sub_mgr.add("H0STASP0", code).await; // 호가
            sub_mgr.add("H0STMKO0", code).await; // 장운영정보
        }
        // 체결 통보 (HTS ID 필요, 모의투자)
        let hts_id = std::env::var("KIS_HTS_ID").unwrap_or_default();
        if !hts_id.is_empty() {
            sub_mgr.add("H0STCNI9", &hts_id).await;
            info!("체결통보 구독 등록 (HTS ID: {hts_id})");
        } else {
            warn!("KIS_HTS_ID 미설정 — 체결통보 구독 생략 (자동매매 차단됨, health gate 실패)");
        }

        // 백그라운드 실행
        let ws_for_run = Arc::clone(&ws_client);
        tokio::spawn(async move { ws_for_run.run().await });
        info!("KIS WebSocket 실시간 스트리밍 시작");
        ws_client
    };

    // 2026-04-16 Task 2: WS watchdog.
    //
    // 장중(09:00~15:35 KST)에 WS tick 이 `KIS_WS_STALE_SECS` (기본 120초) 초과로
    // 들어오지 않거나, 재연결 시도가 max_retries 를 넘어 `terminated=true` 이면
    // 프로세스를 종료한다. 외부 supervisor(nssm, 작업 스케줄러 등)가 자동 재기동
    // 하는 것을 전제. 좀비 프로세스 + 분봉 0건 상태(2026-04-15 재현)를 차단.
    {
        let ws_watch = Arc::clone(&ws_client_handle);
        let event_logger_watch = event_logger.clone();
        let stale_tick_secs = config.ws_stale_tick_secs;
        let stale_message_secs = config.ws_stale_message_secs;
        tokio::spawn(async move {
            // 초기 60초 유예 — 토큰/첫 구독 확인 동안 tick 이 아직 없을 수 있음.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let snapshot = ws_watch.health_snapshot().await;

                // terminated 감지 → 즉시 종료
                if snapshot.terminated {
                    tracing::error!(
                        "[ws-watchdog] WS 영구 종료 (retry_count={}) — 프로세스 종료",
                        snapshot.retry_count
                    );
                    if let Some(ref el) = event_logger_watch {
                        el.log_event(
                            "", "system", "ws_watchdog_exit", "critical",
                            "WS terminated — 프로세스 교체 필요",
                            serde_json::json!({
                                "retry_count": snapshot.retry_count,
                                "reason": "terminated",
                            }),
                        );
                    }
                    std::process::exit(10);
                }

                // 장중만 stale 판정 (장외는 tick 없어도 정상).
                let now_t = chrono::Local::now().time();
                let market_start = chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap();
                let market_end = chrono::NaiveTime::from_hms_opt(15, 35, 0).unwrap();
                if now_t < market_start || now_t > market_end {
                    continue;
                }

                // tick stale 판정. tick 이 한 번도 수신되지 않았어도 장 시작 후
                // 10분 지나면 critical 로 판정.
                let tick_age = snapshot.tick_age_secs();
                let msg_age = snapshot.message_age_secs();
                let critical = match (tick_age, msg_age) {
                    (Some(t), _) if t > stale_tick_secs => Some(format!(
                        "tick stale {t}초 > 임계 {stale_tick_secs}초"
                    )),
                    (None, _) => {
                        let elapsed_since_open = (now_t - market_start).num_seconds().max(0) as u64;
                        if elapsed_since_open > 600 {
                            Some("tick 한 번도 수신되지 않음 (장 시작 10분 이상 경과)".to_string())
                        } else {
                            None
                        }
                    }
                    (_, Some(m)) if m > stale_message_secs => Some(format!(
                        "message stale {m}초 > 임계 {stale_message_secs}초"
                    )),
                    _ => None,
                };

                if let Some(reason) = critical {
                    tracing::error!(
                        "[ws-watchdog] stale 감지 — {} (retry_count={}, tick_age={:?}, msg_age={:?}) — 프로세스 종료",
                        reason, snapshot.retry_count, tick_age, msg_age
                    );
                    if let Some(ref el) = event_logger_watch {
                        el.log_event(
                            "", "system", "ws_watchdog_exit", "critical",
                            &format!("WS stale — {reason}"),
                            serde_json::json!({
                                "retry_count": snapshot.retry_count,
                                "tick_age_secs": tick_age,
                                "message_age_secs": msg_age,
                                "reason": reason,
                            }),
                        );
                    }
                    // event_logger 비동기 플러시 여유.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    std::process::exit(11);
                }
            }
        });
    }

    // 틱 → 분봉 실시간 집계기
    let ws_candles = {
        use kis_agent::infrastructure::websocket::candle_aggregator::CandleAggregator;

        let mut agg = CandleAggregator::new();
        if let Some(ref store) = pg_store {
            agg = agg.with_store(Arc::clone(store));
        }
        let aggregator = Arc::new(agg);
        let codes_ref: Vec<&str> = allowed_codes.iter().map(String::as_str).collect();
        // DB에서 당일 분봉 프리로딩 (장중 재시작 복구)
        aggregator.preload_from_db(&codes_ref).await;
        // OR 데이터 없으면 Yahoo 1순위 / 네이버 fallback으로 자동 보충
        aggregator
            .backfill_or(&codes_ref, false)
            .await;

        let candles = aggregator.completed_candles();
        let rx = realtime_tx.subscribe();
        Arc::clone(&aggregator).spawn(rx, realtime_tx.clone());
        info!("분봉 실시간 집계기 시작");
        (candles, aggregator)
    };
    let (ws_candles, candle_aggregator) = ws_candles;

    // 전략 관리자 (스케줄러와 AppState 공유)
    let strategy_manager = {
        let mut mgr = kis_agent::presentation::routes::strategy::StrategyManager::new();
        mgr.set_client(Arc::clone(&http_client));
        mgr.set_realtime_tx(realtime_tx.clone());
        mgr.set_ws_candles(ws_candles);
        mgr.set_candle_aggregator(Arc::clone(&candle_aggregator));
        if let Some(ref store) = pg_store {
            mgr.set_db_store(Arc::clone(store));
        }
        if let Some(ref el) = event_logger {
            mgr.set_event_logger(Arc::clone(el));
        }
        mgr
    };

    // 2026-04-16 Task 4/5 — 실전 운용 제약 주입.
    // 허용 종목, LiveRunnerConfig, 실전 opt-in 차단 사유를 StrategyManager 로 내려보낸다.
    {
        strategy_manager
            .set_allowed_codes(allowed_codes.clone())
            .await;

        let live_cfg = if config.is_real_mode() {
            use kis_agent::strategy::live_runner::LiveRunnerConfig;
            LiveRunnerConfig::for_real_mode(
                config.real_or_stages.clone(),
                config.max_daily_trades_total.max(1),
                config.entry_cutoff_real,
                config.enable_real_trading,
            )
        } else {
            kis_agent::strategy::live_runner::LiveRunnerConfig::default()
        };
        info!(
            "LiveRunnerConfig — real_mode={}, enable_real_orders={}, allowed_stages={:?}, max_daily_trades_override={:?}, entry_cutoff_override={:?}",
            live_cfg.real_mode,
            live_cfg.enable_real_orders,
            live_cfg.allowed_stages,
            live_cfg.max_daily_trades_override,
            live_cfg.entry_cutoff_override
        );
        strategy_manager.set_live_runner_config(live_cfg).await;

        if config.is_real_mode() && !config.enable_real_trading {
            let reason = "KIS_ENVIRONMENT=real 이지만 KIS_ENABLE_REAL_TRADING=false".to_string();
            warn!(
                "[실전 가드] start 영구 차단 — {}: 주문 경로 활성화하려면 `KIS_ENABLE_REAL_TRADING=true` 설정",
                reason
            );
            strategy_manager.set_block_starts(Some(reason)).await;
        } else {
            strategy_manager.set_block_starts(None).await;
        }
    }

    // 2026-04-16 Task 3 — 주기적 잔고/포지션 reconciliation.
    // 장중 `KIS_RECONCILE_SECS` (기본 60초) 마다 KIS 잔고와 러너 state 를 대조.
    strategy_manager.spawn_reconciliation(config.reconcile_secs);
    info!(
        "잔고 reconciliation 스케줄 기동 — 간격 {}초",
        config.reconcile_secs
    );

    // 시세/잔고 스케줄러 — REST fallback (장외 시간, WS 끊김 시)
    kis_agent::presentation::scheduler::spawn_market_scheduler(
        Arc::clone(&market_data_service),
        Arc::clone(&account_service),
        realtime_tx.clone(),
        strategy_manager.clone(),
    );

    // 장중 Yahoo OR 자동 교체 스케줄러 (09:06, 09:16, 09:31)
    // WS 집계 5분봉은 동시호가 데이터를 포함하지 않아 OR이 백테스트와 다를 수 있음.
    // Yahoo 데이터가 도착하면 OR 구간(09:00~09:30) 캔들을 교체하여 정합성 확보.
    {
        let agg = Arc::clone(&candle_aggregator);
        let allowed_codes_yahoo = allowed_codes.clone();
        tokio::spawn(async move {
            let codes: Vec<&str> = allowed_codes_yahoo.iter().map(String::as_str).collect();
            let schedules = [(9, 6, "5m"), (9, 16, "15m"), (9, 31, "30m")];
            for (hour, min, stage) in schedules {
                // 시작 시각까지 대기
                loop {
                    let now = chrono::Local::now().time();
                    let target = chrono::NaiveTime::from_hms_opt(hour, min, 0).unwrap();
                    if now >= target { break; }
                    let secs = (target - now).num_seconds().max(1).min(10) as u64;
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                }
                // 성공할 때까지 1분 간격 재시도 (최대 20회)
                let mut success = false;
                for attempt in 1..=20u32 {
                    info!("Yahoo OR 교체 시도 ({} OR, {}회차/20)", stage, attempt);
                    if agg.refresh_or_from_yahoo(&codes).await {
                        info!("Yahoo OR 교체 성공 ({} OR, {}회차)", stage, attempt);
                        success = true;
                        break;
                    }
                    if attempt < 20 {
                        warn!("Yahoo OR 교체 실패 ({} OR, {}회차/20) — 60초 후 재시도", stage, attempt);
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    }
                }
                if !success {
                    tracing::error!("Yahoo OR 교체 최종 실패 ({} OR, 20회 시도) — WS OR 유지, 백테스트 정합성 미확보", stage);
                    agg.mark_or_refresh_failed(stage).await;
                }
            }
        });
    }

    // 자동매매 자동 시작.
    //
    // 2026-04-16 P0 원칙:
    // - 기본값 OFF (환경변수 `KIS_AUTO_START=true` 일 때만 자동 시작)
    // - 자동 시작 전 WS 체결통보 health gate 통과 확인
    //   (KIS_HTS_ID 설정 + AES key/iv 수립 완료)
    // - gate 실패 시 NotificationHealth::Failed 로 고정, 수동 start 도 거부
    //
    // 2026-04-15 Codex review #2 대응:
    // - StrategyManager 는 NotificationHealth::Pending 으로 시작하므로
    //   본 gate 가 끝나기 전 UI/수동 start 는 자동 거부된다.
    // - gate 실패 시 `mark_notification_startup_failed` 가 Pending race 로
    //   이미 시작된 러너를 강제 중단한다.
    // - 폴링은 30초 (2초 × 15회) — KIS 응답 지연 여유 확보.
    let auto_start_flag = config.auto_start;
    let is_real_mode_flag = config.is_real_mode();
    let enable_real_trading_flag = config.enable_real_trading;
    {
        let mgr = strategy_manager.clone();
        let ws_client_for_gate = Arc::clone(&ws_client_handle);
        tokio::spawn(async move {
            let hts_id = std::env::var("KIS_HTS_ID").unwrap_or_default();
            let mut notification_ready = false;
            if hts_id.is_empty() {
                warn!("[health gate] KIS_HTS_ID 미설정 — 체결통보 불가, 자동매매 차단");
            } else {
                for attempt in 1..=15u32 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    if ws_client_for_gate.is_notification_ready().await {
                        info!("[health gate] 체결통보 준비 완료 ({}회 시도)", attempt);
                        notification_ready = true;
                        break;
                    }
                    if attempt == 15 {
                        warn!("[health gate] 체결통보 준비 실패 — 30초 대기 타임아웃");
                    }
                }
            }

            if !notification_ready {
                mgr.mark_notification_startup_failed(
                    "KIS_HTS_ID 미설정 또는 AES key/iv 미수립으로 체결통보 불가".to_string(),
                )
                .await;
                error!("[health gate] 자동매매 시작 차단 — Failed 고정");
                return;
            }

            mgr.mark_notification_ready().await;
            info!("[health gate] Notification Ready");

            // 자동 시작은 명시적 opt-in — AppConfig.auto_start 에서 파싱한 값을 사용.
            // 실전 모드에서 KIS_ENABLE_REAL_TRADING 이 꺼져 있으면 auto_start 가 켜져 있어도
            // 실주문이 실제로 나가지 않도록 LiveRunner 단에서 거부된다 (Task 5 가드).
            if auto_start_flag {
                if is_real_mode_flag && !enable_real_trading_flag {
                    warn!(
                        "[health gate] auto_start=true 이지만 KIS_ENABLE_REAL_TRADING=false — \
                         실전 자동 시작 거부. 수동 start 도 main 에서 차단됨."
                    );
                    return;
                }
                info!("[health gate] 통과 + auto_start=true — 자동 시작 진행");
                mgr.auto_start_all().await;
            } else {
                info!("[health gate] 통과 — auto_start=false (수동 시작 대기)");
            }
        });
    }

    // 앱 상태 (shutdown handler용 clone을 먼저 확보)
    let shutdown_mgr = strategy_manager.clone();
    let state = AppState {
        market_data: market_data_service,
        trading: trading_service,
        account: account_service,
        realtime_tx,
        strategy_manager,
        db_pool: pg_store.as_ref().map(|s| s.pool().clone()),
        ws_client: Some(Arc::clone(&ws_client_handle)),
    };

    // CORS 미들웨어 — localhost + 프론트엔드 dev 서버만 허용
    let allowed_origins = [
        "http://localhost:3000".parse().unwrap(),
        "http://localhost:5173".parse().unwrap(),
        "http://127.0.0.1:3000".parse().unwrap(),
        "http://127.0.0.1:5173".parse().unwrap(),
    ];
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
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

    // Graceful shutdown: SIGINT/SIGTERM 수신 시 모든 러너에 stop 신호 전송 후 axum 종료.
    // taskkill /F, SIGKILL 등 OS 레벨 강제 종료는 처리 불가 (재시작 시 cancel_all_pending_orders로 복구).
    let shutdown_signal = async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c().await.expect("SIGINT 핸들러 설치 실패");
        };
        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM 핸들러 설치 실패")
                .recv().await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => { info!("SIGINT 수신 — graceful shutdown 시작"); }
            _ = terminate => { info!("SIGTERM 수신 — graceful shutdown 시작"); }
        }

        // 1) 모든 러너에 stop 신호 → 체결 대기 루프 즉시 탈출 → cancel 경로 실행
        shutdown_mgr.stop_all().await;
        // 2) 러너들이 cancel 완료할 때까지 대기 (체결 대기 30초 + cancel 재시도 여유 = 40초)
        shutdown_mgr.wait_all_stopped(std::time::Duration::from_secs(40)).await;
        info!("모든 러너 종료 확인 — axum shutdown 진행");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .expect("서버 실행 실패");

    info!("서버 종료 완료");
}
