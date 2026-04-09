use governor::{Quota, RateLimiter as GovRateLimiter, clock::DefaultClock, state::InMemoryState};
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::debug;

use crate::domain::types::Environment;

type DirectRateLimiter = GovRateLimiter<
    governor::state::NotKeyed,
    InMemoryState,
    DefaultClock,
    governor::middleware::NoOpMiddleware<governor::clock::QuantaInstant>,
>;

/// KIS API 속도 제한기
pub struct KisRateLimiter {
    /// REST API 속도 제한 (초당)
    api_limiter: Arc<DirectRateLimiter>,
    /// 토큰 발급 속도 제한 (분당 1회)
    token_last_issued: Arc<Mutex<Option<Instant>>>,
}

impl KisRateLimiter {
    pub fn new(env: Environment) -> Self {
        let per_second = match env {
            Environment::Real => 15,
            Environment::Paper => 1, // 모의투자: 서버 실제 제한이 공식(3/s)보다 엄격
        };

        let quota = Quota::per_second(NonZeroU32::new(per_second).unwrap());
        let api_limiter = Arc::new(GovRateLimiter::direct(quota));

        Self {
            api_limiter,
            token_last_issued: Arc::new(Mutex::new(None)),
        }
    }

    /// API 호출 전 대기 (속도 제한 준수)
    pub async fn wait_for_api(&self) {
        self.api_limiter.until_ready().await;
        debug!("API 속도 제한 통과");
    }

    /// 토큰 발급 전 대기 (분당 1회 제한 준수)
    pub async fn wait_for_token(&self) {
        let mut guard = self.token_last_issued.lock().await;
        if let Some(last) = *guard {
            let elapsed = last.elapsed();
            if elapsed < std::time::Duration::from_secs(60) {
                let wait = std::time::Duration::from_secs(60) - elapsed;
                debug!("토큰 발급 대기: {:?}", wait);
                tokio::time::sleep(wait).await;
            }
        }
        *guard = Some(Instant::now());
    }
}
