use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::application::services::account_service::AccountService;
use crate::application::services::market_data_service::MarketDataService;
use crate::domain::ports::realtime::{
    BalancePosition, BalanceSnapshot, BalanceSummary, PriceSnapshot, RealtimeData,
};
use crate::domain::types::StockCode;
use crate::presentation::routes::strategy::StrategyManager;

/// 시세/잔고 스케줄러 설정
struct SchedulerConfig {
    /// 가격 폴링 간격 — 평시 (초)
    price_interval_normal: u64,
    /// 가격 폴링 간격 — 자동매매 활성 시 (초)
    price_interval_trading: u64,
    /// 잔고 폴링 간격 (초)
    balance_interval_secs: u64,
    /// 대상 종목
    stock_codes: Vec<&'static str>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            price_interval_normal: 30,
            price_interval_trading: 120, // 자동매매 중엔 LiveRunner가 WS로 가격 수신 — REST 최소화
            balance_interval_secs: 120,
            stock_codes: vec!["122630", "114800"],
        }
    }
}

/// 시세/잔고 데이터를 주기적으로 KIS API에서 조회하여 WebSocket으로 push하는 스케줄러
pub fn spawn_market_scheduler(
    market_data: Arc<MarketDataService>,
    account: Arc<AccountService>,
    tx: broadcast::Sender<RealtimeData>,
    strategy_manager: StrategyManager,
) {
    let cfg = SchedulerConfig::default();

    // 가격 스케줄러
    let price_tx = tx.clone();
    let price_market = Arc::clone(&market_data);
    let price_codes: Vec<String> = cfg.stock_codes.iter().map(|s| s.to_string()).collect();
    let price_mgr = strategy_manager.clone();
    tokio::spawn(async move {
        // 서버 시작 직후 토큰 발급/WS 연결과 겹치지 않도록 대기
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        loop {
            // 자동매매 활성 여부에 따라 폴링 간격 동적 조절
            let interval_secs = if price_mgr.has_active_runners().await {
                cfg.price_interval_trading
            } else {
                cfg.price_interval_normal
            };

            for code_str in &price_codes {
                let code = match StockCode::new(code_str) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                match price_market.get_price(&code).await {
                    Ok(dto) => {
                        let snapshot = PriceSnapshot {
                            stock_code: code_str.clone(),
                            name: dto.name,
                            price: dto.price,
                            change: dto.change,
                            change_sign: dto.change_sign,
                            change_rate: dto.change_rate,
                            open: dto.open,
                            high: dto.high,
                            low: dto.low,
                            volume: dto.volume,
                            amount: dto.amount,
                        };
                        let _ = price_tx.send(RealtimeData::PriceSnapshot(snapshot));
                        debug!("가격 push: {} {}", code_str, dto.price);
                    }
                    Err(e) => {
                        warn!("가격 조회 실패 ({}): {e}", code_str);
                    }
                }
                // 요청 간 간격: KIS 모의투자 rate limit 방지 (서버 제한 엄격)
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }

            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    });

    // 잔고 스케줄러
    let balance_tx = tx;
    let balance_account = Arc::clone(&account);
    tokio::spawn(async move {
        // 가격 스케줄러와 시차를 두어 동시 호출 방지
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(cfg.balance_interval_secs));
        loop {
            interval.tick().await;
            match balance_account.get_balance().await {
                Ok(dto) => {
                    let snapshot = BalanceSnapshot {
                        positions: dto
                            .positions
                            .into_iter()
                            .map(|p| BalancePosition {
                                stock_code: p.stock_code,
                                stock_name: p.stock_name,
                                quantity: p.quantity,
                                avg_price: p.avg_price,
                                current_price: p.current_price,
                                profit_loss: p.profit_loss,
                                profit_loss_rate: p.profit_loss_rate,
                                purchase_amount: p.purchase_amount,
                                eval_amount: p.eval_amount,
                            })
                            .collect(),
                        summary: BalanceSummary {
                            cash: dto.summary.cash,
                            total_eval: dto.summary.total_eval,
                            total_profit_loss: dto.summary.total_profit_loss,
                            total_purchase: dto.summary.total_purchase,
                        },
                    };
                    let _ = balance_tx.send(RealtimeData::BalanceSnapshot(snapshot));
                    debug!("잔고 push 완료");
                }
                Err(e) => {
                    warn!("잔고 조회 실패: {e}");
                }
            }
        }
    });
}
