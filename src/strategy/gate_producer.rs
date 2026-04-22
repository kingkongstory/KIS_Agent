//! 2026-04-19 PR #1.c/#1.d (execution-followup-plan): 외부 gate 생산자 인터페이스.
//!
//! # 목적
//!
//! Signal Engine / NAV 산출기 / 지수 레짐 엔진 등 **외부 데이터 계층** 이 이 trait 을
//! 구현하여 `ExternalGates` 를 런타임에 주기적으로 갱신한다. 실행 계층 (`LiveRunner`,
//! 후속 execution actor) 은 값을 **소비** 만 하며, 계산 책임은 producer 에 있다.
//!
//! 계획서 (`docs/monitoring/2026-04-19-execution-followup-plan.md` §1) "계산은 전략/
//! 데이터 계층이 한다. 적용은 실행 계층이 한다. `ExternalGates` 는 계산기가 아니라
//! 주입 인터페이스다." 원칙의 런타임 구현.
//!
//! # 사용 예
//!
//! ```ignore
//! use std::sync::Arc;
//! use std::sync::atomic::AtomicBool;
//! use tokio::sync::RwLock;
//! use kis_agent::strategy::gate_producer::{
//!     ExternalGateProducer, GateUpdater, PlaceholderGateProducer,
//! };
//! use kis_agent::strategy::live_runner::ExternalGates;
//!
//! let gates = Arc::new(RwLock::new(ExternalGates::default()));
//! let stop = Arc::new(AtomicBool::new(false));
//! let updater = Arc::new(GateUpdater::new(Arc::clone(&gates), event_logger.clone()));
//! let producer: Box<dyn ExternalGateProducer> =
//!     Box::new(PlaceholderGateProducer::default());
//! let handle = producer.spawn(Arc::clone(&updater), Arc::clone(&stop));
//! // runner.with_external_gates(Arc::clone(&gates));
//! // shutdown: stop.store(true, Relaxed); handle.await.ok();
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use serde_json::json;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::info;

use crate::infrastructure::monitoring::event_logger::EventLogger;

use super::live_runner::{ExternalGates, IndexRegime};

/// 외부 gate 값 갱신용 래퍼. `ExternalGates` 에 대한 write 접근을 중앙화하여
/// 변경 시마다 `event_log` 에 기록한다. producer 구현자는 field 를 직접 쓰는 대신
/// 이 updater 의 setter 를 호출한다.
///
/// 저장되는 category 는 `"external_gate"`, stock_code 는 빈 문자열 — gate 는
/// system-wide 이고 특정 종목에 귀속되지 않기 때문.
pub struct GateUpdater {
    gates: Arc<RwLock<ExternalGates>>,
    event_logger: Option<Arc<EventLogger>>,
}

impl GateUpdater {
    pub fn new(gates: Arc<RwLock<ExternalGates>>, event_logger: Option<Arc<EventLogger>>) -> Self {
        Self {
            gates,
            event_logger,
        }
    }

    pub fn gates(&self) -> Arc<RwLock<ExternalGates>> {
        Arc::clone(&self.gates)
    }

    pub async fn snapshot(&self) -> ExternalGates {
        self.gates.read().await.clone()
    }

    /// 지수 레짐 갱신. 변경되었으면 `event_log` 에 `regime_change` 기록.
    /// `updated_at` 도 함께 갱신.
    pub async fn set_regime(&self, regime: IndexRegime) {
        let (changed, prev_label) = {
            let mut g = self.gates.write().await;
            let prev = g.index_regime;
            let changed = prev != regime;
            g.index_regime = regime;
            g.updated_at = Some(Utc::now());
            (changed, prev.label())
        };
        if changed && let Some(logger) = &self.event_logger {
            logger.log_event(
                "",
                "external_gate",
                "regime_change",
                "info",
                &format!("index_regime: {} → {}", prev_label, regime.label()),
                json!({"from": prev_label, "to": regime.label()}),
            );
        }
    }

    /// NAV gate 통과 여부 갱신.
    pub async fn set_nav_gate_ok(&self, ok: bool) {
        let (changed, prev) = {
            let mut g = self.gates.write().await;
            let prev = g.nav_gate_ok;
            let changed = prev != ok;
            g.nav_gate_ok = ok;
            g.updated_at = Some(Utc::now());
            (changed, prev)
        };
        if changed && let Some(logger) = &self.event_logger {
            logger.log_event(
                "",
                "external_gate",
                "nav_change",
                if ok { "info" } else { "warning" },
                &format!("nav_gate_ok: {} → {}", prev, ok),
                json!({"from": prev, "to": ok}),
            );
        }
    }

    /// 활성 instrument 갱신 (허용 종목코드).
    pub async fn set_active_instrument(&self, code: Option<String>) {
        let (changed, prev) = {
            let mut g = self.gates.write().await;
            let prev = g.active_instrument.clone();
            let changed = prev != code;
            g.active_instrument = code.clone();
            g.updated_at = Some(Utc::now());
            (changed, prev)
        };
        if changed && let Some(logger) = &self.event_logger {
            logger.log_event(
                "",
                "external_gate",
                "active_instrument_change",
                "info",
                &format!(
                    "active_instrument: {} → {}",
                    prev.as_deref().unwrap_or("<none>"),
                    code.as_deref().unwrap_or("<none>"),
                ),
                json!({"from": prev, "to": code}),
            );
        }
    }

    /// 구독 health override 갱신 (None = 외부 미관리).
    pub async fn set_subscription_health(&self, ok: Option<bool>) {
        let (changed, prev) = {
            let mut g = self.gates.write().await;
            let prev = g.subscription_health_ok;
            let changed = prev != ok;
            g.subscription_health_ok = ok;
            g.updated_at = Some(Utc::now());
            (changed, prev)
        };
        if changed && let Some(logger) = &self.event_logger {
            logger.log_event(
                "",
                "external_gate",
                "subscription_health_change",
                "info",
                &format!("subscription_health_ok: {:?} → {:?}", prev, ok),
                json!({"from": prev, "to": ok}),
            );
        }
    }

    /// 스프레드 override 갱신.
    pub async fn set_spread_override(&self, ok: Option<bool>) {
        let (changed, prev) = {
            let mut g = self.gates.write().await;
            let prev = g.spread_ok_override;
            let changed = prev != ok;
            g.spread_ok_override = ok;
            g.updated_at = Some(Utc::now());
            (changed, prev)
        };
        if changed && let Some(logger) = &self.event_logger {
            logger.log_event(
                "",
                "external_gate",
                "spread_override_change",
                "info",
                &format!("spread_ok_override: {:?} → {:?}", prev, ok),
                json!({"from": prev, "to": ok}),
            );
        }
    }

    /// `updated_at` 만 갱신 (keepalive). 다른 값 변경 없음.
    /// event_log 기록 없음 — 이 호출은 폭주 위험.
    pub async fn touch_keepalive(&self) {
        let mut g = self.gates.write().await;
        g.updated_at = Some(Utc::now());
    }
}

/// 외부 gate 생산자 공통 trait.
///
/// `spawn` 은 호출자의 runtime 위에서 백그라운드 태스크를 시작하고 `JoinHandle` 을
/// 반환한다. `stop` 이 `true` 가 되면 graceful 하게 종료해야 한다.
///
/// `Box<Self>` 수신자를 쓰는 이유:
/// - 다형 타입 (`Box<dyn ExternalGateProducer>`) 으로 startup 주입을 쉽게 한다.
/// - producer 는 한 번 spawn 되면 소유권이 task 로 이동 (= 재사용 불가) 이라는
///   의미를 타입으로 드러낸다.
pub trait ExternalGateProducer: Send + Sync {
    /// 백그라운드 루프 시작. `stop` 이 true 가 되면 종료.
    /// producer 는 `updater` 를 통해 `ExternalGates` 를 갱신하며, event_log 기록은
    /// updater 가 자동 수행한다.
    fn spawn(self: Box<Self>, updater: Arc<GateUpdater>, stop: Arc<AtomicBool>) -> JoinHandle<()>;
}

/// 실제 producer 가 연결되기 전 placeholder.
///
/// `keepalive_interval_ms` 마다 `touch_keepalive()` 만 호출하여 stale 체크가
/// false 를 유지하게 한다. 다른 필드는 건드리지 않으므로 `ExternalGates::default()`
/// 값이 그대로 유지 — `nav_gate_ok=true`, `index_regime=Bullish`,
/// `subscription_health_ok=None` 등.
///
/// # 제약
///
/// 이 producer 는 **외부 계산을 수행하지 않는다**. 그러므로:
/// - `paper` 또는 개발 환경에서 "외부 producer 가 연결된 것처럼" 시뮬레이션할 때만 사용.
/// - `real` 에서 이걸 쓰면 `ExternalGates` 가 실제 시장 상태를 전혀 반영하지 못한 채
///   기본값(낙관) 으로 동작하므로 위험하다. 실전에서는 반드시 진짜 producer 로 교체.
pub struct PlaceholderGateProducer {
    pub keepalive_interval_ms: u64,
}

impl Default for PlaceholderGateProducer {
    fn default() -> Self {
        Self {
            keepalive_interval_ms: 5_000,
        }
    }
}

impl ExternalGateProducer for PlaceholderGateProducer {
    fn spawn(self: Box<Self>, updater: Arc<GateUpdater>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
        let interval_ms = self.keepalive_interval_ms.max(500);
        tokio::spawn(async move {
            info!(
                "PlaceholderGateProducer 시작 (keepalive_interval={}ms)",
                interval_ms
            );
            while !stop.load(Ordering::Relaxed) {
                updater.touch_keepalive().await;
                tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
            }
            info!("PlaceholderGateProducer 종료 (stop 감지)");
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tokio::time::{Duration, sleep};

    fn new_updater() -> Arc<GateUpdater> {
        let gates = Arc::new(RwLock::new(ExternalGates::default()));
        Arc::new(GateUpdater::new(gates, None))
    }

    #[tokio::test]
    async fn placeholder_updates_updated_at_then_stops() {
        let updater = new_updater();
        let gates = updater.gates();
        let stop = Arc::new(AtomicBool::new(false));

        assert!(gates.read().await.updated_at.is_none(), "초기값은 None");

        let producer: Box<dyn ExternalGateProducer> = Box::new(PlaceholderGateProducer {
            keepalive_interval_ms: 500,
        });
        let handle = producer.spawn(Arc::clone(&updater), Arc::clone(&stop));

        sleep(Duration::from_millis(150)).await;
        assert!(
            gates.read().await.updated_at.is_some(),
            "producer 가 updated_at 을 채워야 함"
        );

        stop.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_millis(1_000), handle).await;
    }

    #[tokio::test]
    async fn placeholder_does_not_modify_other_fields() {
        let gates = Arc::new(RwLock::new(ExternalGates {
            nav_gate_ok: false,
            index_regime: IndexRegime::Bearish,
            active_instrument: Some("114800".to_string()),
            subscription_health_ok: Some(true),
            spread_ok_override: Some(false),
            updated_at: None,
        }));
        let updater = Arc::new(GateUpdater::new(Arc::clone(&gates), None));
        let stop = Arc::new(AtomicBool::new(false));

        let producer: Box<dyn ExternalGateProducer> = Box::new(PlaceholderGateProducer {
            keepalive_interval_ms: 500,
        });
        let handle = producer.spawn(Arc::clone(&updater), Arc::clone(&stop));

        sleep(Duration::from_millis(150)).await;
        {
            let g = gates.read().await;
            assert!(
                !g.nav_gate_ok,
                "placeholder 는 nav_gate_ok 를 건드리지 않음"
            );
            assert_eq!(g.index_regime, IndexRegime::Bearish);
            assert_eq!(g.active_instrument.as_deref(), Some("114800"));
            assert_eq!(g.subscription_health_ok, Some(true));
            assert_eq!(g.spread_ok_override, Some(false));
            assert!(g.updated_at.is_some(), "updated_at 만 갱신됐어야 함");
        }

        stop.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_millis(1_000), handle).await;
    }

    #[tokio::test]
    async fn updater_set_regime_changes_value_and_touches_updated_at() {
        let updater = new_updater();
        updater.set_regime(IndexRegime::Bearish).await;
        let g = updater.snapshot().await;
        assert_eq!(g.index_regime, IndexRegime::Bearish);
        assert!(g.updated_at.is_some());
    }

    #[tokio::test]
    async fn updater_setters_are_idempotent_on_unchanged_value() {
        let updater = new_updater();
        updater.set_nav_gate_ok(true).await; // 기본이 true 라 변화 없음
        let g = updater.snapshot().await;
        assert!(g.nav_gate_ok);
        // updated_at 은 항상 갱신 (keepalive 의미).
        assert!(g.updated_at.is_some());
    }

    #[tokio::test]
    async fn updater_setter_round_trip_all_fields() {
        let updater = new_updater();
        updater.set_nav_gate_ok(false).await;
        updater.set_regime(IndexRegime::Neutral).await;
        updater
            .set_active_instrument(Some("122630".to_string()))
            .await;
        updater.set_subscription_health(Some(true)).await;
        updater.set_spread_override(Some(false)).await;

        let g = updater.snapshot().await;
        assert!(!g.nav_gate_ok);
        assert_eq!(g.index_regime, IndexRegime::Neutral);
        assert_eq!(g.active_instrument.as_deref(), Some("122630"));
        assert_eq!(g.subscription_health_ok, Some(true));
        assert_eq!(g.spread_ok_override, Some(false));
    }
}
