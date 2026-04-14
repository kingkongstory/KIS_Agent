use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;
use tracing::{debug, warn};

/// 운영 이벤트 로거 — fire-and-forget 패턴으로 매매 루프를 블로킹하지 않음.
///
/// 모든 중요 이벤트(전략 상태 전이, 신호 감지, 주문 실행, 시스템 에러 등)를
/// 타임스탬프 + 구조화된 메타데이터와 함께 `event_log` 테이블에 영속화한다.
///
/// ## 사용 패턴
/// ```ignore
/// // await 불필요 — 매매 루프 블로킹 0
/// event_logger.log_event("122630", "position", "breakeven_activated", "info",
///     "1R 도달 — 본전스탑 활성화",
///     serde_json::json!({"price": 10100, "new_sl": 10075}));
/// ```
pub struct EventLogger {
    pool: PgPool,
    /// DB 저장 실패 카운터 (모니터링의 모니터링 — /health API에서 노출)
    fail_count: AtomicU64,
}

impl EventLogger {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            fail_count: AtomicU64::new(0),
        }
    }

    /// DB 저장 실패 횟수 (헬스 체크용)
    pub fn fail_count(&self) -> u64 {
        self.fail_count.load(Ordering::Relaxed)
    }

    /// 비동기 DB INSERT (내부용)
    async fn insert(
        &self,
        stock_code: &str,
        category: &str,
        event_type: &str,
        severity: &str,
        message: &str,
        metadata: &serde_json::Value,
    ) {
        let result = sqlx::query(
            "INSERT INTO event_log (stock_code, category, event_type, severity, message, metadata)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(stock_code)
        .bind(category)
        .bind(event_type)
        .bind(severity)
        .bind(message)
        .bind(metadata)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => debug!("이벤트 저장: [{category}] {event_type}"),
            Err(e) => {
                self.fail_count.fetch_add(1, Ordering::Relaxed);
                warn!("이벤트 저장 실패: {e}");
            }
        }
    }

    /// fire-and-forget 이벤트 기록 — `tokio::spawn`으로 비동기 실행, await 불필요.
    /// 매매 루프를 절대 블로킹하지 않으며, 저장 실패해도 매매를 멈추지 않음.
    pub fn log_event(
        self: &Arc<Self>,
        stock_code: &str,
        category: &str,
        event_type: &str,
        severity: &str,
        message: &str,
        metadata: serde_json::Value,
    ) {
        let this = Arc::clone(self);
        let sc = stock_code.to_string();
        let cat = category.to_string();
        let et = event_type.to_string();
        let sev = severity.to_string();
        let msg = message.to_string();
        tokio::spawn(async move {
            this.insert(&sc, &cat, &et, &sev, &msg, &metadata).await;
        });
    }
}
