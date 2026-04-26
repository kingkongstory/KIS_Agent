use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Local, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, RwLock, broadcast, watch};
use tracing::{debug, error, info, warn};

use crate::domain::error::KisError;
use crate::domain::models::order::OrderSide;
use crate::domain::models::price::InquirePrice;
use crate::domain::ports::realtime::{RealtimeData, TradeNotification};
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::{StockCode, TransactionId};
use crate::infrastructure::cache::postgres_store::{
    ActivePosition, ExecutionJournalEntry, PostgresStore, TradeRecord,
};
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};
use crate::infrastructure::monitoring::event_logger::EventLogger;
use crate::infrastructure::websocket::candle_aggregator::CompletedCandle;

use super::armed_taxonomy as armed_tx;
use super::candle::{self, MinuteCandle};
use super::execution_actor::{
    ExecutionCommand, ExecutionEvent, TimeoutPhase, TimeoutPolicy, next_timeout_for,
};
use super::orb_fvg::{OrbFvgConfig, OrbFvgStrategy};
use super::parity::position_manager::{
    PositionManagerConfig, is_tp_hit, sl_exit_reason, time_stop_breached_by_minutes,
    update_best_and_trailing,
};
use super::parity::signal_engine::{SignalEngineConfig, detect_next_fvg_signal};
use super::restart_recovery::RestartAction;
use super::types::*;

/// KIS 분봉 응답 항목 — `fetch_minute_candles_from` 에서 역직렬화만 수행하므로
/// dead_code lint 가 구조체와 개별 필드 양쪽에 적용된다. 역직렬화 scaffold 유지.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct MinutePriceItem {
    stck_bsop_date: String,
    stck_cntg_hour: String,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_oprc: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_hgpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_lwpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_prpr: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    cntg_vol: i64,
}

/// 실시간 러너 공유 상태 (웹에서 읽기 가능)
#[derive(Debug, Clone)]
pub struct RunnerState {
    pub phase: String,
    pub today_trades: Vec<TradeResult>,
    pub today_pnl: f64,
    pub current_position: Option<Position>,
    /// OR(Opening Range) 고가/저가 (기본 15분 — 호환용)
    pub or_high: Option<i64>,
    pub or_low: Option<i64>,
    /// Multi-Stage OR: (단계명, high, low) — "5m", "15m", "30m"
    pub or_stages: Vec<(String, i64, i64)>,
    /// 장 중단 여부 (VI 발동 또는 거래정지)
    pub market_halted: bool,
    /// degraded 모드 — WS 체결통보 비정상 등 시스템 신뢰성 미달 시 true.
    /// 진입 전체가 차단되며 관찰만 허용된다. (2026-04-16 P0)
    pub degraded: bool,
    /// 사람 개입 필요 — 재시작 복구 시 메타 부족으로 포지션을 임의 재구성할 수 없을 때 true.
    /// 자동매매가 자기 자신을 중단하고 운영자의 HTS 수동 확인을 요구한다. (2026-04-16 P0)
    pub manual_intervention_required: bool,
    /// manual/degraded 사유 (UI/로그용)
    pub degraded_reason: Option<String>,
    /// 2026-04-18 Phase 1 (execution-timing-implementation-plan):
    /// 청산 진행 중 플래그 (`ExitPending`). `close_position_market` 이 주문 제출
    /// 직전에 `true` 로 설정하고, 보유 0 확인 또는 `manual_intervention` 확정
    /// 시점에만 해제한다.
    ///
    /// 불변식:
    /// - `exit_pending=true` 인 동안 `manage_position` 은 TP/SL/시간스탑 판정을
    ///   전부 스킵한다 (불변식 4: pending 중 추가 주문 금지).
    /// - `current_position` 은 `exit_pending=true` 인 동안 **절대 `take()` 되지 않는다**.
    ///   보유수량 0 이 검증된 뒤에만 `None` 으로 전이 (불변식 2: flat 은 holdings=0
    ///   확인 이후에만).
    pub exit_pending: bool,
    /// 2026-04-18 Phase 2: 명시적 실행 상태머신.
    ///
    /// 기존 bool 필드들 (`current_position.is_some()`, `exit_pending`,
    /// `manual_intervention_required`, `degraded`) 의 조합을 하나의 enum 으로 표현.
    /// 한 세션에 전체 치환 시 Big Bang 리스크가 크므로 **기존 bool 은 유지**하고
    /// 새 `execution_state` 가 함께 갱신되는 방식으로 점진 마이그레이션한다.
    ///
    /// Phase 3/4 에서 execution actor 로 이관될 때 이 필드가 단일 진실(single source of truth)이 된다.
    pub execution_state: ExecutionState,
    /// 2026-04-18 Phase 2: Signal Engine → Execution Engine 으로 넘어오는 사전 검사 메타데이터.
    ///
    /// 현재는 Default 값(모든 gate 통과)으로 초기화. Phase 3/4 에서 Signal Engine 이
    /// entry_cutoff / spread_ok / nav_gate_ok / regime_confirmed 를 실제로 채운다.
    pub preflight_metadata: PreflightMetadata,
    /// 2026-04-19 PR #1.d: 직전 `refresh_preflight_metadata` 호출에서 external gate 가
    /// stale 로 관측되었는지. flip (false→true 또는 true→false) 에서만 event_log 에
    /// 기록하여 폭주 방지.
    pub last_external_gate_stale: bool,
    /// 2026-04-23 장마감 후속조치: `Flat` 전이 시각.
    /// reconcile 이 청산 직후 잔고 전파 지연을 hidden position 으로 오판하지 않도록
    /// 짧은 settle window 판정에 사용한다.
    pub last_flat_transition_at: Option<std::time::Instant>,
    /// 2026-04-23 parser 후속 방어선: 최근 비정상 호가 사유.
    /// 마지막 정상 호가를 유지하더라도 신규 진입은 차단하고, preflight 사유를
    /// `spread_exceeded` 대신 `quote_invalid` 로 분리하기 위해 상태에 남긴다.
    pub last_quote_sanity_issue: Option<String>,
    /// 2026-04-19 PR #2.b: armed watch 종료 사유별 집계 + duration 통계.
    pub armed_stats: ArmedWatchStats,
    /// 2026-04-19 PR #2.c: 현재 armed watch 가 감시 중인 signal_id.
    /// `None` = 감시 중 아님. 동일 id 로 재진입하면 `AlreadyArmed` 로 즉시 반환.
    pub current_armed_signal_id: Option<String>,
}

/// 실시간 트레이딩 러너
pub struct LiveRunner {
    client: Arc<KisHttpClient>,
    strategy: OrbFvgStrategy,
    stock_code: StockCode,
    stock_name: String,
    quantity: u64,
    /// 외부에서 중지 요청
    stop_flag: Arc<AtomicBool>,
    /// 공유 상태
    pub state: Arc<RwLock<RunnerState>>,
    /// 주문 체결 알림 전송
    trade_tx: Option<broadcast::Sender<RealtimeData>>,
    /// WebSocket 실시간 데이터 수신 (체결가)
    realtime_rx: Option<broadcast::Receiver<RealtimeData>>,
    /// 최신 실시간 가격 (WebSocket에서 갱신) + 갱신 시각
    latest_price: Arc<RwLock<Option<(i64, tokio::time::Instant)>>>,
    /// 최신 최우선 호가(best ask, best bid) + 갱신 시각
    latest_quote: Arc<RwLock<Option<(i64, i64, tokio::time::Instant)>>>,
    /// 외부 중지 알림 (sleep 즉시 깨우기)
    stop_notify: Arc<Notify>,
    /// 2026-04-19 PR #2.e → 2026-04-20 PR #2.g: armed watcher 를 tick 주기 없이
    /// 즉시 깨우기 위한 generation counter 채널.
    ///
    /// `Notify` 에서 `watch::Sender<u64>` 로 교체된 이유: `Notify::notify_waiters()`
    /// 는 "호출 시점 등록된 waiter" 만 깨우므로, `Notified` future 를 `set()` 으로
    /// 재생성하는 짧은 창에 오는 신호가 유실된다. `watch::Receiver::changed()` 는
    /// missed update 를 건너뛰지 않으며 cancel-safe 이므로 race-free 가 완전 보장된다.
    ///
    /// 값(u64) 은 generation counter. `transition_to_degraded` /
    /// `trigger_manual_intervention` 이 `send_modify(|g| *g = g.wrapping_add(1))`
    /// 로 증가시킨다.
    armed_wake_tx: Arc<watch::Sender<u64>>,
    /// WebSocket 실시간 분봉 (CandleAggregator 공유)
    ws_candles: Option<Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>>,
    /// DB 저장소 (거래 기록 영속화)
    db_store: Option<Arc<PostgresStore>>,
    /// 공유 포지션 잠금: 한 종목이 포지션 보유 시 다른 종목 진입 차단
    position_lock: Option<Arc<RwLock<PositionLockState>>>,
    /// 신호 탐색 진행 상태 (백테스트 BacktestEngine state.search_from 등가).
    /// 청산 시각 이후의 5분봉만 탐색하도록 하여 같은 FVG 재진입을 차단한다.
    /// 2026-04-10 사고(trade #54~58 같은 entry_price 4회 반복) 핫픽스.
    signal_state: Arc<RwLock<LiveSignalState>>,
    /// 운영 이벤트 로거 (fire-and-forget 비동기 DB 저장)
    event_logger: Option<Arc<EventLogger>>,
    /// 라이브 전용 설정 (drift 가드 등). 백테스트 미사용.
    live_cfg: LiveRunnerConfig,
    /// 최근 발주 이력 — 반복 발주 자동 안전장치용.
    /// 5분 창 내 동일가 ±0.1% 주문이 3회 이상이면 runner 자동 중단.
    recent_orders: Arc<Mutex<VecDeque<(NaiveTime, i64)>>>,
    /// 2026-04-19 Doc review P2: Signal Engine 이 주입하는 NAV/레짐/활성 ETF 게이트.
    /// `None` 이면 훅 비활성 (preflight 는 placeholder 기본값 사용 — 기존 동작 보존).
    /// `Some(Arc<_>)` 면 `refresh_preflight_metadata` 가 매 호출마다 read-lock 으로 최신 값 반영.
    external_gates: Option<Arc<RwLock<ExternalGates>>>,
}

/// 공유 포지션 잠금 상태 — 두 종목(122630, 114800) 간 진입 조율.
///
/// Pending 단계에서 다른 종목이 선점(preempt) 요청을 할 수 있어,
/// 미체결 대기(최대 30초)로 인한 교착을 방지한다.
/// - `Free` → `Pending` → `Held` → `Free` (정상 플로우)
/// - `Pending(preempted=true)` → `Free` (선점 당해 취소)
/// - `Pending(preempted=true)` → `Held` (선점 요청 있으나 이미 체결 → 체결 우선)
///
/// 2026-04-17 v3 fixups H: `ManualHeld` variant 추가.
/// `enter_manual_intervention_state` 가 설정. close 경로
/// (`release_lock_if_self_held`) 가 무시 (자기 lock 만 풀음). `poll_and_enter`
/// 의 진입 가드는 다른 종목 ManualHeld 면 차단. 즉 manual 종목의 unresolved
/// live position 이 남아있는 동안 다른 종목 신규 진입을 dual-locked invariant
/// 로 강제 차단한다.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum PositionLockState {
    /// 잠금 없음 — 모든 종목 진입 가능
    #[default]
    Free,
    /// 지정가 발주 후 체결 대기 중.
    /// `preempted`=true이면 다른 종목이 선점을 요청한 상태 → 즉시 취소해야 함.
    Pending { code: String, preempted: bool },
    /// 포지션 보유 확정 — 다른 종목 진입 차단
    Held(String),
    /// manual_intervention 으로 보유 중인 종목.
    /// `release_lock_if_self_held` 가 무시하므로 운영자 수동 처리 또는 재시작
    /// 복구 전까지 lock 이 유지된다. 다른 종목 신규 진입은 `poll_and_enter`
    /// 가드에서 차단.
    ManualHeld(String),
}

/// 라이브 신호 탐색 상태 — 청산 시각 이후의 5분봉만 탐색하도록 한다.
/// 백테스트 `BacktestEngine::next_valid_candidate` 의 `state.search_from` 과
/// `simulate_day_locked` 의 `sync_search_to(target=last_unlock_time)` 에 대응.
#[derive(Debug, Default, Clone)]
pub(crate) struct LiveSignalState {
    /// 이 시각보다 큰 5분봉만 탐색 (`c.time > search_after`).
    /// None이면 제한 없이 모든 5분봉 탐색 (장 시작 직후).
    search_after: Option<NaiveTime>,
}

/// `trigger_manual_intervention` 호출 시 공유 포지션 잠금 처리 방식.
///
/// 2026-04-15 Codex review #3/#4 대응. manual_intervention 전환은
/// 종목별 상태(`RunnerState.manual_intervention_required`)뿐 아니라
/// dual-locked 환경의 shared `PositionLockState` 도 함께 관리해야,
/// 다른 종목 러너가 이 종목의 숨은 live position 과 중첩 노출되지 않는다.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ManualInterventionMode {
    /// shared lock 을 `Held(self_code)` 로 고정 유지.
    /// safe_orphan_cleanup 의 보유 감지 / cancel_verify_failed 경로 기본값.
    KeepLock,
}

/// 시스템 전체 일일 거래 카운터.
///
/// 2026-04-16 실전 Go 조건 #2 대응 — 종목별 `max_daily_trades_override`(기본 1) 와
/// 독립적으로 "시스템 전체 기준 하루 N회"를 강제한다. 122630/114800 두 러너가 각자
/// 1회 진입해서 총 2회가 되는 것을 막는 유일한 안전장치.
///
/// 사용 흐름:
/// 1. `main.rs` 가 `KIS_MAX_DAILY_TRADES_TOTAL` 값으로 `new()` 호출 → Arc<RwLock<_>> 로 포장
/// 2. 모든 `LiveRunner` 가 같은 Arc 를 공유 (`LiveRunnerConfig.global_trade_gate`)
/// 3. 진입 직전 `can_enter(today)` 체크 → false 면 해당 진입 skip
/// 4. 체결 완료 후 `record_trade(today)` 로 카운트 증가
///
/// 날짜 전환은 첫 호출 시 자동 리셋. 자정 이후에도 서버가 돌면 새 날짜의 count 0 으로 시작.
#[derive(Debug, Default)]
pub struct GlobalTradeGate {
    /// 마지막으로 기록된 날짜. `None` 이면 아직 기록 없음.
    pub date: Option<chrono::NaiveDate>,
    /// 해당 날짜에 누적된 거래 수.
    pub count: usize,
    /// 시스템 전체 하루 허용 한도.
    pub max: usize,
}

impl GlobalTradeGate {
    pub fn new(max: usize) -> Self {
        Self {
            date: None,
            count: 0,
            max: max.max(1),
        }
    }

    /// 날짜 전환 감지 및 필요 시 카운터 리셋. 이후 현재 count 반환.
    fn roll_and_count(&mut self, today: chrono::NaiveDate) -> usize {
        if self.date != Some(today) {
            self.date = Some(today);
            self.count = 0;
        }
        self.count
    }

    /// 오늘 진입을 허용하는지. true 면 호출자는 이어서 진입을 시도하고,
    /// 체결 확정 후 `record_trade` 를 호출해야 한다.
    pub fn can_enter(&mut self, today: chrono::NaiveDate) -> bool {
        self.roll_and_count(today) < self.max
    }

    /// 거래 1건 기록. `can_enter` 가 true 를 반환한 뒤 체결 확정 시점에만 호출.
    pub fn record_trade(&mut self, today: chrono::NaiveDate) {
        self.roll_and_count(today);
        self.count += 1;
    }

    /// 오늘 현재 count / max (UI / 로그용).
    pub fn snapshot(&mut self, today: chrono::NaiveDate) -> (usize, usize) {
        (self.roll_and_count(today), self.max)
    }
}

/// 라이브 러너 고유 설정 (백테스트와 분리).
///
/// `OrbFvgConfig` 는 백테스트/라이브 공용이라 백테스트 결과에 영향을 주지 않으려는
/// 라이브 전용 가드는 이곳에 둔다.
#[derive(Debug, Clone)]
pub struct LiveRunnerConfig {
    /// 진입 지정가 대비 현재가 이탈 허용 한계 (Long: (cur - entry) / entry).
    /// `None` 이면 가드 비활성 (Day+0 기본). `Some(0.005)` 는 0.5%.
    pub max_entry_drift_pct: Option<f64>,
    /// 진입 허용 OR stage 목록. `None` 이면 모든 stage(5m/15m/30m) 허용 — 모의 기본.
    /// `Some(vec!["15m"])` 실전 기본 — 원리 그대로의 15분 OR 하나만 진입 자격으로 사용.
    /// 리스트에 없는 stage 의 신호는 entry 경로에서 건너뛴다 (UI 에는 여전히 표시).
    pub allowed_stages: Option<Vec<String>>,
    /// 일일 최대 거래 횟수 override. `None` 이면 `OrbFvgConfig.max_daily_trades` 사용.
    /// 실전 기본 `Some(1)` — 하루 1회만 진입하고, 결과와 무관하게 같은 날 재진입 금지.
    pub max_daily_trades_override: Option<usize>,
    /// 신규 진입 마감 시각 override. `None` 이면 `OrbFvgConfig.entry_cutoff` 사용.
    /// 실전 기본 15:00 — 오후 약한 신호를 차단하고 관리/청산 시간을 여유있게 확보.
    pub entry_cutoff_override: Option<NaiveTime>,
    /// 실전 주문 경로 명시 opt-in. `false` 이면 실제 진입/TP/시장가 경로를 전부 차단한다.
    /// `AppConfig.is_real_mode() && !AppConfig.enable_real_trading` 조합에서 `false`.
    /// 기본 `true` (모의투자 기본 동작 유지).
    pub enable_real_orders: bool,
    /// 실전 식별 플래그. 로그/trade.environment 필드에 사용.
    pub real_mode: bool,
    /// 시스템 전체 일일 거래 카운터 공유 핸들. `None` 이면 전역 가드 비활성.
    /// 2026-04-16 실전 Go 조건 #2 — 두 러너가 각자 1회씩 총 2회 진입되는 것을 막는다.
    pub global_trade_gate: Option<Arc<RwLock<GlobalTradeGate>>>,
    /// 2026-04-18 Phase 3 (execution-timing-implementation-plan):
    /// 진입 체결 대기 timeout (ms). 기존 `execute_entry` 의 하드코딩된 30_000 ms 를
    /// 설정으로 빼낸다. 이 값은 **환경변수/CLI 옵션으로 단축** 가능해야 실전 운영자가
    /// 패시브 지정가 → 시장성 지정가 정책 전환 시 함께 낮출 수 있다.
    ///
    /// 권장 방향 (문서 Phase 3):
    /// - paper/passive 기본: 60_000 ms.
    /// - `MarketableLimitWithBudget` 정책 도입 후: 2_000 ~ 3_000 ms.
    /// - 최대 5_000 ms 를 넘기지 않는다.
    ///
    /// 기본값은 paper 패시브 지정가의 느린 모의 체결을 흡수하기 위해 60_000.
    /// real 기본은 `for_real_mode()` / `AppConfig` 가 3_000 으로 낮춘다.
    pub entry_fill_timeout_ms: u64,
    /// 2026-04-18 Phase 3: 청산 검증 budget (ms).
    /// `verify_exit_completion` 내부 WS+REST+balance 3단 검증의 전체 예산이며,
    /// 기본 15_000 ms 는 현재 로직 (WS 3s + REST 3*1s + balance 3*1s = ~9s) 에 여유를 더한 값.
    /// 현재는 verify_exit_completion 내부 고정 상수 시간을 쓰므로 **관찰/기록용 필드** 이며,
    /// Phase 4 에서 이 값을 내부 세부 timeout 으로 분배하는 리팩터링 예정.
    pub exit_verification_budget_ms: u64,
    /// 2026-04-18 Phase 4 (execution-timing-implementation-plan):
    /// 신호 탐색 주기 (ms). 기본 5_000 — 기존 `run()` 의 5초 `tokio::time::sleep` 동등.
    ///
    /// ETF 단기 FVG 재진입 타이밍이 빠를 때 5초가 느릴 수 있어 설정 필드로 뺐다.
    /// real 기본은 1_000ms, paper 는 기존 5_000ms 를 유지한다. 진정한 이벤트
    /// 기반(5분봉 확정 notify) 은 별도 단계에서 CandleAggregator notify 로 확장한다.
    ///
    /// Phase 4 권장: 1_000~2_000 ms (이벤트 기반 전환 전 중간 단계).
    pub poll_interval_ms: u64,
    /// 2026-04-18 Phase 4: 포지션 관리(manage_position) 재평가 주기 (ms).
    /// 기본 3_000 — 기존 동등. 이 값은 SL 틱 감지가 `spawn_price_listener` 의 WS 리스너에서
    /// 별도로 수행되므로 급하지 않다. TP 체결 확인 폴링의 주기로만 작용.
    pub manage_poll_interval_ms: u64,
    /// 2026-04-18 Review (Finding #3): 라이브 진입 정책.
    /// paper 기본은 `PassiveZoneEdge`, real 기본은 `MarketableLimit`.
    /// `execute_entry` 가 최우선 호가 기반 시장성 지정가 + slippage budget gate
    /// 경로를 타게 하여 ETF 진입 타이밍 손실을 줄인다.
    pub execution_policy_kind: ExecutionPolicyKind,
    /// 2026-04-19 Doc review P1 (Phase 4 armed watch): SignalArmed 상태에서 execute_entry 직전
    /// 가격 tick / preflight 재확인을 위해 대기할 budget (ms). 기본 `0` = 비활성 (즉시 진입).
    ///
    /// `> 0` 으로 설정하면 signal 확정 후 이 budget 동안 다음을 반복 재확인한다:
    /// - WS 가격 tick (100~300ms 의 paced check)
    /// - preflight.all_ok() 유지 여부
    /// - drift 한계 유지 여부
    /// - entry_cutoff 도달 여부
    ///
    /// 하나라도 실패하면 armed 취소 (abort_entry). budget 소진하면 execute_entry 진행.
    /// 권장 (문서 Phase 4): 200~300ms.
    pub armed_watch_budget_ms: u64,
    /// armed watch 루프의 tick 체크 간격 (ms). 기본 150. WS tick 이 없을 때 paced re-check 주기.
    pub armed_tick_check_interval_ms: u64,
    /// 2026-04-19 Doc revision (Phase 3 timeout 분해): 청산 WS 체결통보 대기 (ms).
    /// 기본 3_000. verify_exit_completion 의 첫 단계로 짧게 대기 후 REST fallback.
    pub exit_ws_notice_timeout_ms: u64,
    /// 청산 REST 체결조회 재시도 횟수. 기본 3.
    pub exit_rest_retries: u32,
    /// 청산 REST 체결조회 재시도 간격 (ms). 기본 1_000.
    pub exit_rest_retry_interval_ms: u64,
    /// 청산 balance 재확인 재시도 횟수. 기본 3.
    pub exit_balance_retries: u32,
    /// 청산 balance 재확인 재시도 간격 (ms). 기본 1_000.
    pub exit_balance_retry_interval_ms: u64,
    /// 2026-04-19 PR #1.a (execution-followup-plan): 외부 gate 의 `updated_at` 허용 최대 나이 (ms).
    /// 기본 paper 60_000 / real 15_000. 외부 producer 가 아직 연결 전이면 `updated_at=None` 이라
    /// is_stale 은 false — 기본 동작 보존. stale 감지 시 PR #1.b 에서 real 은 fail-close.
    pub external_gate_max_age_ms: u64,
}

/// 2026-04-18 Review round (Finding #3): 라이브 실행 정책 선택지.
///
/// paper 기본값은 기존 동작 보존(`PassiveZoneEdge`). `MarketableLimit` 으로 전환하면
/// 최우선 호가 기준 `±N tick` 으로 시장성 지정가를 발주하여 체결 확률과 타이밍을 우선한다.
///
/// 하드코딩되던 `round_to_etf_tick(entry_price)` 경로를 이 enum 으로 분기하고,
/// `entry_mode` 라벨과 `ActivePosition.entry_mode` 저장값도 정책에 따라 달라진다.
///
/// `MarketableLimit` 는 호가 구독(best ask/bid)을 우선 사용하고, 호가가 stale 이면
/// `get_current_price()` 를 근사치로 fallback 한다. `slippage_budget_pct` 는
/// "지정가가 intended_entry 에서 이 비율 이상 벗어나면 진입 포기" 의 preflight gate 로
/// 작동한다.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ExecutionPolicyKind {
    /// 기존 라이브 기본 — zone edge 패시브 지정가 (`gap.top`/`gap.bottom`).
    #[default]
    PassiveZoneEdge,
    /// 시장성 지정가 + 슬리피지 budget.
    MarketableLimit {
        /// 매수: 현재가 + N tick, 매도: 현재가 - N tick. 기본 1.
        tick_offset: u32,
        /// |limit_price - intended_entry| / intended_entry 허용 한계 (0.003 = 0.3%).
        /// 초과 시 진입 포기 — ETF 문서 "1호가 스프레드 제한" 과 등가의 최소 gate.
        slippage_budget_pct: f64,
    },
}

impl ExecutionPolicyKind {
    /// DB 저장용 라벨 (`active_positions.entry_mode`).
    pub fn entry_mode_label(self) -> &'static str {
        match self {
            Self::PassiveZoneEdge => "limit_passive",
            Self::MarketableLimit { .. } => "marketable_limit",
        }
    }
}

/// 2026-04-19 Doc review P2 (ETF gate 주입): Signal Engine 이 런타임에 주입하는
/// NAV / 레짐 / 활성 ETF 게이트.
///
/// 실행 계층이 직접 계산하지 않는다 (문서 경계선: Signal Engine 책임).
/// `Arc<RwLock<ExternalGates>>` 로 공유하고, 외부(NAV 산출기 / 레짐 엔진 / REST 관리 API 등)가
/// 런타임에 `update_*` 호출로 값을 갱신한다. 값이 한 번이라도 주입되기 전까지는 기본값(true)을
/// 유지하여 기존 동작을 보존한다.
///
/// Phase 5 이후 Signal Engine 이 이 구조를 채우면 `refresh_preflight_metadata` 가 자동으로
/// 반영하여 신규 진입이 gate 에 연결된다.
/// 2026-04-19 PR #1.a (execution-followup-plan): KOSPI200 지수 레짐 분류.
///
/// 외부(Signal Engine) 가 실행 계층에 주입하는 trinary 상태 + Unknown.
/// `ExternalGates.index_regime` 에 저장되며 `refresh_preflight_metadata` 가
/// `PreflightMetadata.regime_confirmed` (bool) 로 축약해서 gate 적용.
///
/// 변수명 매핑 (ETF 규칙 초안 기준):
/// - `Bullish` → 상승 레짐 확정, 122630 (레버리지) 진입 허용
/// - `Bearish` → 하락 레짐 확정, 114800 (인버스) 진입 허용
/// - `Neutral` → 경계/횡보, 신규 진입 차단
/// - `Unknown` → 미확정 또는 데이터 부족 (stale 직후 초기값), 신규 진입 차단
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexRegime {
    Bullish,
    Bearish,
    Neutral,
    Unknown,
}

impl IndexRegime {
    /// 방향성 확정 여부. `PreflightMetadata.regime_confirmed` 로 축약될 때 사용.
    pub fn is_confirmed_directional(self) -> bool {
        matches!(self, IndexRegime::Bullish | IndexRegime::Bearish)
    }

    /// DB/event_log 라벨. 변경 시 사후 쿼리 호환성 주의.
    pub fn label(self) -> &'static str {
        match self {
            IndexRegime::Bullish => "bullish",
            IndexRegime::Bearish => "bearish",
            IndexRegime::Neutral => "neutral",
            IndexRegime::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExternalGates {
    /// NAV 괴리율 gate 통과 여부 — Signal Engine 이 괴리 계산 후 세팅.
    pub nav_gate_ok: bool,
    /// 지수 레짐 분류. 상승/하락 확정이면 진입 허용, Neutral/Unknown 이면 차단.
    /// `PreflightMetadata.regime_confirmed` 는 `is_confirmed_directional()` 축약값.
    pub index_regime: IndexRegime,
    /// 현재 활성 ETF 종목코드 (122630 = 레버리지 long / 114800 = 인버스).
    /// `None` 이면 placeholder — 모든 종목 허용.
    pub active_instrument: Option<String>,
    /// 구독 health override. `None` = `refresh_preflight_metadata` 내부 계산 사용.
    /// `Some(false)` = 외부에서 강제 차단 (예: NAV 스트림 stale, 지수 스트림 단절).
    /// `Some(true)` = 외부가 OK 로 판정해도 내부 stale 체크와 AND 로 결합.
    pub subscription_health_ok: Option<bool>,
    /// 스프레드 gate override. `None` = 내부 best ask/bid 계산 사용.
    /// `Some(false)` = 외부가 강제 차단, `Some(true)` = 외부가 통과로 선언.
    pub spread_ok_override: Option<bool>,
    /// 마지막 외부 업데이트 시각. `None` 이면 외부 producer 가 아직 주입 전 —
    /// placeholder 단계 (`is_stale` 이 항상 false 반환). `Some(t)` 이면 `now - t`
    /// 가 `LiveRunnerConfig.external_gate_max_age_ms` 를 넘으면 stale.
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for ExternalGates {
    fn default() -> Self {
        Self {
            nav_gate_ok: true,
            index_regime: IndexRegime::Bullish,
            active_instrument: None,
            subscription_health_ok: None,
            spread_ok_override: None,
            updated_at: None,
        }
    }
}

impl ExternalGates {
    /// 외부 producer 가 `updated_at` 을 최근에 갱신했는지.
    ///
    /// `updated_at = None` (placeholder 단계) 이면 항상 false — 외부 연결 전 기본 동작 보존.
    /// `Some(t)` 이면 `(now - t).as_millis() > max_age_ms` 일 때 true.
    /// 음수 diff (clock skew) 도 stale 로 간주.
    pub fn is_stale(&self, now: DateTime<Utc>, max_age_ms: u64) -> bool {
        match self.updated_at {
            None => false,
            Some(t) => {
                let age_ms = now.signed_duration_since(t).num_milliseconds();
                age_ms < 0 || (age_ms as u64) > max_age_ms
            }
        }
    }
}

/// 2026-04-19 PR #1.b: external gate 를 preflight 계산에 반영한 결과.
///
/// `refresh_preflight_metadata` 가 `ExternalGates` 스냅샷과 내부 계산값을 합쳐
/// PreflightMetadata 각 필드로 쓰기 전 중간 값. 순수 계산이라 unit test 가능.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalGateApplication {
    pub nav_gate_ok: bool,
    pub regime_confirmed: bool,
    pub active_instrument: Option<String>,
    pub subscription_health_ok: bool,
    pub spread_ok: bool,
    /// 외부가 stale 로 판정되어 fail-close 또는 warn 경로를 탔는지. 로깅/event_log 용.
    pub stale_detected: bool,
}

/// 2026-04-19 PR #1.b: external gate 스냅샷 + 내부 계산값을 preflight 반영 규칙으로 합성.
///
/// 정책:
/// - `updated_at=None` (placeholder) → `is_stale=false` → 외부 값 그대로 사용.
/// - `stale && real_mode` → nav/regime 을 강제 false 로 덮어써 fail-close.
/// - `stale && !real_mode` → 외부 값 그대로 사용 (paper 경고만).
/// - `subscription_health_ok`: 내부 체크와 외부 override 를 AND 결합. 외부 None 이면 내부만.
/// - `spread_ok_override`: 외부가 명시적 Some 이면 내부 계산 대신 그대로 사용.
/// - `active_instrument`: 외부가 None 이면 자기 종목 코드로 fallback (기존 동작 유지).
pub(crate) fn apply_external_gates(
    gates: &ExternalGates,
    now: DateTime<Utc>,
    max_age_ms: u64,
    real_mode: bool,
    self_stock_code: &str,
    internal_subscription_health_ok: bool,
    internal_spread_ok: bool,
) -> ExternalGateApplication {
    let stale = gates.is_stale(now, max_age_ms);
    let (nav_gate_ok, regime_confirmed) = if stale && real_mode {
        (false, false)
    } else {
        (
            gates.nav_gate_ok,
            gates.index_regime.is_confirmed_directional(),
        )
    };
    let active_instrument = gates
        .active_instrument
        .clone()
        .or_else(|| Some(self_stock_code.to_string()));
    let subscription_health_ok = match gates.subscription_health_ok {
        Some(external) => internal_subscription_health_ok && external,
        None => internal_subscription_health_ok,
    };
    let spread_ok = gates.spread_ok_override.unwrap_or(internal_spread_ok);
    ExternalGateApplication {
        nav_gate_ok,
        regime_confirmed,
        active_instrument,
        subscription_health_ok,
        spread_ok,
        stale_detected: stale,
    }
}

/// 2026-04-19 Doc review P1 (Phase 4 armed watch): armed 상태 재확인 루프 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArmedWaitOutcome {
    /// budget 동안 조건이 유지되어 진입 진행 가능.
    Ready,
    /// entry_cutoff 도달 → search_after 전진 필요.
    CutoffReached,
    /// preflight gate 가 도중에 깨짐 (subscription stale / spread 급확대 등).
    PreflightFailed(&'static str),
    /// max_entry_drift_pct 초과.
    DriftExceeded,
    /// stop_flag 발동.
    Stopped,
    /// 2026-04-19 PR #2.d: manual_intervention / degraded 전이로 armed watcher 를
    /// 중단. reason 은 `armed_manual_intervention` / `armed_degraded` 등.
    Aborted(&'static str),
    /// 2026-04-19 PR #2.c: 같은 signal_id 에 이미 armed watch 가 진행 중 — 중복
    /// 루프 방지. 호출자는 신규 진입을 건너뛰어야 한다 (진입 허용 아님).
    AlreadyArmed,
}

impl ArmedWaitOutcome {
    /// event_log `event_type` 메타 라벨 (variant 이름).
    ///
    /// 2026-04-19 PR #2.f: 문자열은 `armed_taxonomy::OUTCOME_*` 상수에서 가져온다.
    /// `armed_taxonomy::tests::outcome_labels_match_armed_wait_outcome` 이 양쪽
    /// 일치를 강제.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => armed_tx::OUTCOME_READY,
            Self::CutoffReached => armed_tx::OUTCOME_CUTOFF_REACHED,
            Self::PreflightFailed(_) => armed_tx::OUTCOME_PREFLIGHT_FAILED,
            Self::DriftExceeded => armed_tx::OUTCOME_DRIFT_EXCEEDED,
            Self::Stopped => armed_tx::OUTCOME_STOPPED,
            Self::Aborted(_) => armed_tx::OUTCOME_ABORTED,
            Self::AlreadyArmed => armed_tx::OUTCOME_ALREADY_ARMED,
        }
    }

    /// variant 안에 실린 세부 사유 (`PreflightFailed("armed_preflight_nav_failed")` 등).
    pub fn detail(&self) -> Option<&'static str> {
        match self {
            Self::PreflightFailed(r) | Self::Aborted(r) => Some(r),
            _ => None,
        }
    }

    /// event_log severity — warn 이상으로 끌어올릴 outcome 인지.
    pub fn severity(&self) -> &'static str {
        match self {
            Self::Ready | Self::CutoffReached | Self::Stopped | Self::AlreadyArmed => "info",
            Self::PreflightFailed(_) | Self::DriftExceeded | Self::Aborted(_) => "warning",
        }
    }
}

/// 2026-04-19 PR #2.b: armed watch 종료 사유별 집계 + duration 통계.
///
/// `RunnerState.armed_stats` 에 위치. fire-and-forget 기록용 — UI/REST 노출은
/// 후속 단계. record() 호출 시 해당 variant counter 를 1 증가시키고 duration 을
/// 누적/max 에 반영.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ArmedWatchStats {
    pub ready_count: u64,
    pub cutoff_count: u64,
    pub preflight_failed_count: u64,
    pub drift_exceeded_count: u64,
    pub stopped_count: u64,
    pub aborted_count: u64,
    pub already_armed_count: u64,
    pub total_duration_ms: u64,
    pub max_duration_ms: u64,
    // 2026-04-19 PR #2.e: outcome 별 duration 세분화.
    //
    // `ready_*` 는 "성공한 armed watch 가 실제로 얼마나 대기했는가" — 지연 예산 튜닝에 사용.
    // `aborted_*` 는 "manual/degraded 전이 후 watcher 가 얼마나 빨리 종료됐는가" —
    // notify 기반 즉시 종료(#2.e) 효과 측정에 사용.
    pub ready_duration_ms_sum: u64,
    pub ready_duration_ms_max: u64,
    pub aborted_duration_ms_sum: u64,
    pub aborted_duration_ms_max: u64,
}

impl ArmedWatchStats {
    pub(crate) fn record(&mut self, outcome: &ArmedWaitOutcome, duration_ms: u64) {
        match outcome {
            ArmedWaitOutcome::Ready => {
                self.ready_count += 1;
                self.ready_duration_ms_sum = self.ready_duration_ms_sum.saturating_add(duration_ms);
                if duration_ms > self.ready_duration_ms_max {
                    self.ready_duration_ms_max = duration_ms;
                }
            }
            ArmedWaitOutcome::CutoffReached => self.cutoff_count += 1,
            ArmedWaitOutcome::PreflightFailed(_) => self.preflight_failed_count += 1,
            ArmedWaitOutcome::DriftExceeded => self.drift_exceeded_count += 1,
            ArmedWaitOutcome::Stopped => self.stopped_count += 1,
            ArmedWaitOutcome::Aborted(_) => {
                self.aborted_count += 1;
                self.aborted_duration_ms_sum =
                    self.aborted_duration_ms_sum.saturating_add(duration_ms);
                if duration_ms > self.aborted_duration_ms_max {
                    self.aborted_duration_ms_max = duration_ms;
                }
            }
            ArmedWaitOutcome::AlreadyArmed => self.already_armed_count += 1,
        }
        self.total_duration_ms = self.total_duration_ms.saturating_add(duration_ms);
        if duration_ms > self.max_duration_ms {
            self.max_duration_ms = duration_ms;
        }
    }

    pub fn total_count(&self) -> u64 {
        self.ready_count
            + self.cutoff_count
            + self.preflight_failed_count
            + self.drift_exceeded_count
            + self.stopped_count
            + self.aborted_count
            + self.already_armed_count
    }

    /// PR #2.e: 성공 armed watch 평균 대기(ms). ready 건 없으면 `None`.
    pub fn ready_avg_duration_ms(&self) -> Option<f64> {
        if self.ready_count == 0 {
            None
        } else {
            Some(self.ready_duration_ms_sum as f64 / self.ready_count as f64)
        }
    }

    /// PR #2.e: manual/degraded abort 평균 깨움 지연(ms). aborted 건 없으면 `None`.
    pub fn aborted_avg_duration_ms(&self) -> Option<f64> {
        if self.aborted_count == 0 {
            None
        } else {
            Some(self.aborted_duration_ms_sum as f64 / self.aborted_count as f64)
        }
    }
}

/// 2026-04-18 Phase 2 (execution-timing-implementation-plan): 종목별 실행 상태머신.
///
/// 기존 `RunnerState` 가 `current_position: Option<Position>`, `exit_pending: bool`,
/// `manual_intervention_required: bool`, `degraded: bool` 등 여러 bool 로 표현하던
/// 상태를 명시적 enum 으로 추가한다. 한 단계에서 통합하면 Big Bang 이라 기존 bool 은
/// **당분간 유지**하고, 새 `execution_state` 필드가 함께 동기화된다.
///
/// 상태:
/// - `Flat`: 포지션 없음 (기본). 기존 `current_position=None && !exit_pending` 과 동치.
/// - `SignalArmed`: 신호 유효, 주문 미발주 (Phase 4 에서 enabled; 현재는 즉시 EntryPending 전이).
/// - `EntryPending`: 진입 주문 제출 후 체결 확인 중 (현재 `PositionLockState::Pending` 과 유사).
/// - `EntryPartial`: 진입 일부 체결, 잔량 정리 (cancel/fill race 경로).
/// - `Open`: 포지션 보유 확정 (기존 `current_position=Some && !exit_pending`).
/// - `ExitPending`: 청산 주문 제출 후 verify 진행 중 (기존 `exit_pending=true`).
/// - `ExitPartial`: 청산 일부 체결, 잔량 재청산 (기존 `handle_exit_remaining` 중간).
/// - `ManualIntervention`: 자동 복구 금지 (기존 `manual_intervention_required=true`).
/// - `Degraded`: 시스템 신뢰도 부족 — 신규 진입만 차단 (기존 `degraded=true`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionState {
    #[default]
    Flat,
    SignalArmed,
    EntryPending,
    EntryPartial,
    Open,
    ExitPending,
    ExitPartial,
    ManualIntervention,
    Degraded,
}

impl ExecutionState {
    /// DB/event_log 에 기록될 라벨. 변경되면 사후 쿼리가 깨짐.
    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::SignalArmed => "signal_armed",
            Self::EntryPending => "entry_pending",
            Self::EntryPartial => "entry_partial",
            Self::Open => "open",
            Self::ExitPending => "exit_pending",
            Self::ExitPartial => "exit_partial",
            Self::ManualIntervention => "manual_intervention",
            Self::Degraded => "degraded",
        }
    }

    /// 신규 진입 주문을 만들 수 있는 상태인가.
    /// 불변식 4 (pending 중 추가 주문 금지) 를 명시적으로 반영.
    pub fn can_place_entry(self) -> bool {
        matches!(self, Self::Flat | Self::SignalArmed)
    }

    /// 자동 청산 경로를 탈 수 있는 상태인가.
    pub fn can_auto_exit(self) -> bool {
        matches!(self, Self::Open | Self::EntryPartial | Self::ExitPartial)
    }

    /// 포지션 관리 (TP/SL/Time) 가능 여부.
    /// `ExitPending` / `ExitPartial` / `ManualIntervention` / `Degraded` 에선 금지.
    pub fn is_manageable_position(self) -> bool {
        matches!(self, Self::Open | Self::EntryPartial)
    }

    /// ExecutionState 와 legacy bool 플래그를 양방향 동기화하기 위한 helper 의 반대 방향.
    /// 기존 RunnerState bool 을 보고 ExecutionState 를 추정 — 리팩터링 중간 단계에서만 사용.
    pub fn derive_from_legacy(
        has_position: bool,
        exit_pending: bool,
        manual: bool,
        degraded: bool,
    ) -> Self {
        if manual {
            Self::ManualIntervention
        } else if exit_pending {
            Self::ExitPending
        } else if has_position {
            Self::Open
        } else if degraded {
            Self::Degraded
        } else {
            Self::Flat
        }
    }
}

/// 2026-04-18 Phase 2: 진입 신호에 동반되는 사전 검사 메타데이터.
///
/// Signal Engine (ORB+FVG / 레짐 / NAV 괴리 / 프로그램매매 / 거래량) 이 생성한 신호에
/// 실행 계층이 참고하는 부가 정보. 실행 계층은 이 값을 **저장만** 하고, gate 계산은
/// Signal Engine 에서 끝낸다 (책임 경계: `docs/monitoring/2026-04-18-execution-timing-implementation-plan.md`
/// 의 "구현 담당자를 위한 경계선" 섹션).
///
/// 현재 기본 생성자는 모든 gate 가 `true` (통과) 인 중립값을 반환 — 아직 Signal Engine
/// 이 이 필드를 채우지 않으므로 기본값으로 리팩터링 중 기존 동작을 보존한다. Phase 3/4
/// 에서 Signal Engine 이 실제 값을 주입하면 execution actor 가 이 필드를 정책 gate 로 사용.
#[derive(Debug, Clone)]
pub struct PreflightMetadata {
    /// 오전 신규 진입 마감 시각 (ETF 문서 10:30 등). 현재는 `OrbFvgConfig.entry_cutoff` 만 사용.
    pub entry_cutoff: Option<NaiveTime>,
    /// 1호가 스프레드 제한 통과 여부.
    pub spread_ok: bool,
    /// 최신 호가 ask 원값.
    pub spread_ask: Option<i64>,
    /// 최신 호가 bid 원값.
    pub spread_bid: Option<i64>,
    /// spread 평가에 사용된 중간값.
    pub spread_mid: Option<i64>,
    /// ask-bid 원값.
    pub spread_raw: Option<i64>,
    /// 평가에 사용한 ETF tick.
    pub spread_tick: Option<i64>,
    /// spread gate 한계 tick 수.
    pub spread_threshold_ticks: u32,
    /// NAV 괴리 gate 통과 여부.
    pub nav_gate_ok: bool,
    /// 지수 레짐 (상승/하락/중립) 확정 여부.
    pub regime_confirmed: bool,
    /// 레짐 판정 후 선택된 활성 ETF 종목코드 (122630 / 114800).
    pub active_instrument: Option<String>,
    /// 실시간 구독 health (체결/호가/NAV/지수). 하나라도 비정상이면 false — 신규 진입 차단.
    pub subscription_health_ok: bool,
    /// 마지막 호가 sanity 실패 사유. `Some` 이면 오래된 정상 호가가 남아 있어도
    /// 신규 진입은 차단하고 운영 로그를 `quote_invalid` 로 분리한다.
    pub quote_issue: Option<String>,
}

impl Default for PreflightMetadata {
    fn default() -> Self {
        Self {
            entry_cutoff: None,
            spread_ok: true,
            spread_ask: None,
            spread_bid: None,
            spread_mid: None,
            spread_raw: None,
            spread_tick: None,
            spread_threshold_ticks: DEFAULT_SPREAD_THRESHOLD_TICKS,
            nav_gate_ok: true,
            regime_confirmed: true,
            active_instrument: None,
            subscription_health_ok: true,
            quote_issue: None,
        }
    }
}

impl PreflightMetadata {
    /// 모든 preflight gate 가 통과한 상태인가.
    /// Phase 3/4 에서 entry actor 의 진입 조건으로 사용 예정.
    pub fn all_ok(&self) -> bool {
        self.spread_ok
            && self.nav_gate_ok
            && self.regime_confirmed
            && self.subscription_health_ok
            && self.quote_issue.is_none()
    }

    pub fn entry_cutoff_ok(&self, now: NaiveTime) -> bool {
        self.entry_cutoff.is_none_or(|cutoff| now < cutoff)
    }

    pub fn active_instrument_ok(&self, stock_code: &str) -> bool {
        self.active_instrument
            .as_ref()
            .is_none_or(|code| code == stock_code)
    }

    pub fn allows_entry_for(&self, stock_code: &str, now: NaiveTime) -> bool {
        self.all_ok() && self.entry_cutoff_ok(now) && self.active_instrument_ok(stock_code)
    }

    pub fn spread_in_ticks(&self) -> Option<f64> {
        self.spread_raw
            .zip(self.spread_tick)
            .and_then(|(raw, tick)| {
                if tick <= 0 {
                    None
                } else {
                    Some(raw as f64 / tick as f64)
                }
            })
    }
}

const DEFAULT_SPREAD_THRESHOLD_TICKS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpreadGateEvaluation {
    spread_ok: bool,
    spread_ask: Option<i64>,
    spread_bid: Option<i64>,
    spread_mid: Option<i64>,
    spread_raw: Option<i64>,
    spread_tick: Option<i64>,
    spread_threshold_ticks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteSanityEvaluation {
    Valid,
    Invalid(&'static str),
}

fn validate_quote_sanity(ask: i64, bid: i64, current_price: Option<i64>) -> QuoteSanityEvaluation {
    if ask <= 0 || bid <= 0 {
        return QuoteSanityEvaluation::Invalid("nonpositive");
    }
    if ask < bid {
        return QuoteSanityEvaluation::Invalid("inverted");
    }

    if let Some(price) = current_price.filter(|price| *price > 0) {
        let mid = ((ask + bid) / 2).max(1);
        let deviation_pct =
            (((mid as i128 - price as i128).abs() * 100) / i128::from(price.max(1))) as i64;
        if deviation_pct > 5 {
            return QuoteSanityEvaluation::Invalid("off_market");
        }
    }

    QuoteSanityEvaluation::Valid
}

/// 비정상 호가를 spread gate 와 분리해 추적한다.
///
/// parser 회귀나 WS 이상치가 들어와도 마지막 정상 호가를 덮어쓰지 않고,
/// preflight 는 `RunnerState.last_quote_sanity_issue` 를 보고 신규 진입을 차단한다.
async fn apply_quote_update(
    latest_quote: &Arc<RwLock<Option<(i64, i64, tokio::time::Instant)>>>,
    state: &Arc<RwLock<RunnerState>>,
    event_logger: Option<&Arc<EventLogger>>,
    stock_code: &str,
    source: &'static str,
    ask: i64,
    bid: i64,
    current_price: Option<i64>,
    now: tokio::time::Instant,
) {
    match validate_quote_sanity(ask, bid, current_price) {
        QuoteSanityEvaluation::Valid => {
            *latest_quote.write().await = Some((ask, bid, now));
            let mut guard = state.write().await;
            guard.last_quote_sanity_issue = None;
        }
        QuoteSanityEvaluation::Invalid(reason) => {
            let changed = {
                let mut guard = state.write().await;
                let changed = guard.last_quote_sanity_issue.as_deref() != Some(reason);
                guard.last_quote_sanity_issue = Some(reason.to_string());
                changed
            };
            if changed && let Some(el) = event_logger {
                el.log_event(
                    stock_code,
                    "market",
                    "quote_invalid",
                    "warn",
                    &format!("비정상 호가 수신 — source={source}, reason={reason}"),
                    serde_json::json!({
                        "source": source,
                        "reason": reason,
                        "ask": ask,
                        "bid": bid,
                        "current_price": current_price,
                    }),
                );
            }
        }
    }
}

fn evaluate_spread_gate(
    quote_snapshot: Option<(i64, i64)>,
    real_mode: bool,
    threshold_ticks: u32,
) -> SpreadGateEvaluation {
    match quote_snapshot {
        Some((ask, bid)) => {
            let mid = ((ask + bid) / 2).max(1);
            let tick = etf_tick_size(mid);
            let spread_raw = ask - bid;
            SpreadGateEvaluation {
                spread_ok: ask >= bid && spread_raw <= tick * i64::from(threshold_ticks),
                spread_ask: Some(ask),
                spread_bid: Some(bid),
                spread_mid: Some(mid),
                spread_raw: Some(spread_raw),
                spread_tick: Some(tick),
                spread_threshold_ticks: threshold_ticks,
            }
        }
        None => SpreadGateEvaluation {
            spread_ok: !real_mode,
            spread_ask: None,
            spread_bid: None,
            spread_mid: None,
            spread_raw: None,
            spread_tick: None,
            spread_threshold_ticks: threshold_ticks,
        },
    }
}

/// 2026-04-18 Phase 1 (execution-timing-implementation-plan):
/// 청산 체결 확인 경로 라벨. `TradeRecord.exit_resolution_source` 와 동일한 의미.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitResolutionSource {
    /// WS 체결통보로 체결가 확인.
    WsNotice,
    /// REST 체결조회로 체결가 확인.
    RestExecution,
    /// 체결가 미확인, 잔고 감소/0 으로만 체결 확인.
    BalanceOnly,
    /// 모든 경로 실패 — verification 실패. 호출자가 manual_intervention 분기로 보내야 한다.
    Unresolved,
    /// TP 지정가 정상 체결 경로 (KRX 자동 체결).
    TpLimit,
}

impl ExitResolutionSource {
    pub fn label(self) -> &'static str {
        match self {
            ExitResolutionSource::WsNotice => "ws_notice",
            ExitResolutionSource::RestExecution => "rest_execution",
            ExitResolutionSource::BalanceOnly => "balance_only",
            ExitResolutionSource::Unresolved => "unresolved",
            ExitResolutionSource::TpLimit => "tp_limit",
        }
    }
}

/// 2026-04-18 Phase 1: 청산 결과 사후 기록용 메타데이터.
///
/// `TradeResult` 는 백테스트/라이브 공용이라 필드를 확장하지 않는다. 대신 라이브 청산
/// 경로 (`close_position_market`, `manage_position` TP 체결) 에서 `ExitTraceMeta` 를
/// 추가로 반환하여 `save_trade_to_db` 가 `TradeRecord.fill_price_unknown` 과
/// `exit_resolution_source` 에 그대로 기록한다.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExitTraceMeta {
    /// 체결가 미확인 플래그. `true` 면 `TradeResult.exit_price` 는 현재가 fallback.
    /// 보유수량 0 확인이 끝난 경우에만 이 상태로 flat 전이가 허용된다 (불변식 2).
    pub fill_price_unknown: bool,
    /// 체결 확인 경로 라벨. `TradeRecord.exit_resolution_source` 에 저장.
    pub exit_resolution_source: String,
}

impl ExitTraceMeta {
    /// TP 지정가 정상 체결 경로 (거래소 자동 매칭 + `fetch_fill_detail` 확인).
    pub fn tp_limit() -> Self {
        Self {
            fill_price_unknown: false,
            exit_resolution_source: ExitResolutionSource::TpLimit.label().to_string(),
        }
    }

    /// 체결가 확인 완료 경로 (holdings=0 + filled_price Some).
    pub fn resolved(source: ExitResolutionSource) -> Self {
        Self {
            fill_price_unknown: false,
            exit_resolution_source: source.label().to_string(),
        }
    }

    /// 체결가 미확인 but holdings=0 확인 — fill_price_unknown=true.
    pub fn price_unknown(source: ExitResolutionSource) -> Self {
        Self {
            fill_price_unknown: true,
            exit_resolution_source: source.label().to_string(),
        }
    }
}

/// 2026-04-18 Phase 1: 시장가 청산 후 체결/잔고 검증 결과.
///
/// WS 체결통보 → REST 체결조회 → balance 재확인 3단 검증의 통합 결과. `can_flat()`
/// 이 `true` 인 경우에만 호출자가 `Flat` 으로 전이할 수 있다 (불변식 2).
#[derive(Debug, Clone)]
pub(crate) struct ExitVerification {
    /// 체결이 확인된 수량. WS/REST 로 확인된 값, 또는 보유 감소로 역산된 값.
    pub filled_qty: u64,
    /// 확정된 체결가. `None` 이면 체결가 미확인 — `fill_price_unknown=true` 로 기록.
    pub filled_price: Option<i64>,
    /// balance 조회 결과 최종 보유수량. `None` 이면 balance 조회 자체 실패.
    /// `Some(0)` 이어야만 `Flat` 전이 가능 (불변식 2).
    pub holdings_after: Option<u64>,
    /// 체결 확인 주요 경로.
    pub resolution_source: ExitResolutionSource,
    /// WS 체결통보 수신 여부 — 분석/로그용.
    pub ws_received: bool,
    /// REST 체결조회 시도별 진단 정보.
    /// `balance_only` 로 내려간 경우에도 왜 REST 가 체결가를 못 줬는지 사후 분석하기 위해 남긴다.
    pub rest_execution_attempts: Vec<ExecutionQueryDiagnostics>,
}

impl ExitVerification {
    /// `Flat` 전이 가능 여부. 보유수량 0 이 확인되었을 때만 true (불변식 2).
    /// 체결가 미확인과 flat 미확인은 다른 문제이므로, filled_price 는 이 판정에 관여하지 않는다.
    ///
    /// 현재 프로덕션 경로는 분기 의도를 명확히 하기 위해 `has_remaining()` 을 사용한다.
    /// 이 메서드는 테스트에서 `has_remaining` 의 반대 축을 직접 검증할 때 쓴다.
    #[allow(dead_code)]
    pub fn can_flat(&self) -> bool {
        matches!(self.holdings_after, Some(0))
    }

    /// 잔량이 남아있는지 여부. `Some(q>0)` 또는 조회 실패면 true 로 간주해 잔량 처리 분기로.
    pub fn has_remaining(&self) -> bool {
        match self.holdings_after {
            Some(0) => false,
            Some(_) => true,
            None => true, // balance 조회 실패 — 안전하게 잔량 있다고 간주 (불변식 5)
        }
    }

    pub fn rest_execution_last_status(&self) -> Option<&str> {
        self.rest_execution_attempts
            .last()
            .map(|attempt| attempt.status.as_str())
    }

    pub fn rest_execution_diagnostics_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.rest_execution_attempts)
            .unwrap_or_else(|_| serde_json::json!([]))
    }
}

/// REST 체결조회 1회 시도의 결과 진단.
///
/// 기존 `query_execution` 은 `None` 만 반환해 HTTP 실패, 응답 없음, 주문번호 불일치,
/// 0수량/0가격을 구분할 수 없었다. 청산이 `balance_only` 로 내려간 원인을 DB metadata 에
/// 남기기 위해 이 구조체를 사용한다.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct ExecutionQueryDiagnostics {
    pub phase: String,
    pub attempt: u32,
    pub order_no: String,
    pub status: String,
    pub output_rows: usize,
    pub matched_rows: usize,
    pub matched_qty: u64,
    pub priced_qty: u64,
    pub avg_price: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct ExecutionQueryOutcome {
    fill: Option<(i64, u64)>,
    diagnostics: ExecutionQueryDiagnostics,
}

fn json_field_as_string(item: &serde_json::Value, key: &str) -> Option<String> {
    item.get(key).and_then(|value| {
        value
            .as_str()
            .map(|s| s.trim().to_string())
            .or_else(|| value.as_i64().map(|n| n.to_string()))
            .or_else(|| value.as_u64().map(|n| n.to_string()))
            .or_else(|| value.as_f64().map(|n| n.to_string()))
    })
}

fn parse_execution_query_items(
    order_no: &str,
    phase: &str,
    attempt: u32,
    items: &[serde_json::Value],
) -> ExecutionQueryOutcome {
    let mut diagnostics = ExecutionQueryDiagnostics {
        phase: phase.to_string(),
        attempt,
        order_no: order_no.to_string(),
        status: "order_not_found".to_string(),
        output_rows: items.len(),
        ..ExecutionQueryDiagnostics::default()
    };
    let mut weighted_price_sum = 0.0_f64;

    for item in items {
        let odno = json_field_as_string(item, "odno").unwrap_or_default();
        if odno != order_no {
            continue;
        }

        diagnostics.matched_rows += 1;
        let qty = json_field_as_string(item, "tot_ccld_qty")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let avg = json_field_as_string(item, "avg_prvs")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        diagnostics.matched_qty = diagnostics.matched_qty.saturating_add(qty);
        if qty > 0 && avg > 0.0 {
            diagnostics.priced_qty = diagnostics.priced_qty.saturating_add(qty);
            weighted_price_sum += avg * qty as f64;
        }
    }

    if diagnostics.matched_rows == 0 {
        return ExecutionQueryOutcome {
            fill: None,
            diagnostics,
        };
    }
    if diagnostics.matched_qty == 0 {
        diagnostics.status = "matched_zero_qty".to_string();
        return ExecutionQueryOutcome {
            fill: None,
            diagnostics,
        };
    }
    if diagnostics.priced_qty == 0 {
        diagnostics.status = "matched_no_price".to_string();
        return ExecutionQueryOutcome {
            fill: None,
            diagnostics,
        };
    }

    let avg_price = (weighted_price_sum / diagnostics.priced_qty as f64).round() as i64;
    diagnostics.avg_price = Some(avg_price);
    diagnostics.status = if diagnostics.priced_qty == diagnostics.matched_qty {
        "filled".to_string()
    } else {
        "filled_partial_price".to_string()
    };
    ExecutionQueryOutcome {
        fill: Some((avg_price, diagnostics.matched_qty)),
        diagnostics,
    }
}

#[cfg(test)]
mod global_trade_gate_tests {
    use super::GlobalTradeGate;
    use chrono::NaiveDate;

    #[test]
    fn new_sets_max_with_floor_of_one() {
        let g = GlobalTradeGate::new(0);
        assert_eq!(g.max, 1, "max 는 최소 1로 보정되어야 함");
    }

    #[test]
    fn can_enter_returns_true_until_limit_reached() {
        let mut g = GlobalTradeGate::new(2);
        let today = NaiveDate::from_ymd_opt(2026, 4, 16).unwrap();
        assert!(g.can_enter(today));
        g.record_trade(today);
        assert!(g.can_enter(today));
        g.record_trade(today);
        assert!(!g.can_enter(today), "한도 도달 후엔 거부");
    }

    #[test]
    fn rolls_counter_on_date_change() {
        let mut g = GlobalTradeGate::new(1);
        let d1 = NaiveDate::from_ymd_opt(2026, 4, 16).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 4, 17).unwrap();
        g.record_trade(d1);
        assert!(!g.can_enter(d1));
        assert!(g.can_enter(d2), "새 날짜면 카운터 리셋 후 허용");
        assert_eq!(g.snapshot(d2).0, 0);
    }

    #[test]
    fn record_trade_increments_count() {
        let mut g = GlobalTradeGate::new(3);
        let today = NaiveDate::from_ymd_opt(2026, 4, 16).unwrap();
        g.record_trade(today);
        g.record_trade(today);
        assert_eq!(g.snapshot(today), (2, 3));
    }

    #[test]
    fn snapshot_initial_state_returns_zero() {
        let mut g = GlobalTradeGate::new(1);
        let today = NaiveDate::from_ymd_opt(2026, 4, 16).unwrap();
        assert_eq!(g.snapshot(today), (0, 1));
    }
}

/// 2026-04-17 v3 불변식 #2 P3: canonical 5m merge precedence 회귀 테스트.
/// 과거 완료 버킷은 Yahoo (interval_min=5) 우선, 현재 진행 버킷은 WS 집계만,
/// WS fallback 은 backfill 없는 과거 버킷에서만 적용되는지 못박는다.
#[cfg(test)]
mod canonical_merge_tests {
    use crate::infrastructure::websocket::candle_aggregator::CompletedCandle;
    use chrono::{NaiveTime, Timelike};
    use std::collections::BTreeMap;

    /// 본 테스트는 `fetch_canonical_5m_candles` 의 핵심 merge 로직을 순수 함수로
    /// 재현하여 precedence 정책을 검증한다. 실제 메서드는 async + self 의존이라
    /// 단위 테스트 대상에서 제외 (통합 테스트 경로는 별도).
    fn merge_canonical(
        ws_1m: &[CompletedCandle],
        backfill_5m: &[CompletedCandle],
        now: NaiveTime,
    ) -> Vec<(NaiveTime, i64, i64, i64, i64)> {
        let parse = |t: &str| -> NaiveTime {
            let parts: Vec<&str> = t.split(':').collect();
            let h: u32 = parts[0].parse().unwrap();
            let m: u32 = parts[1].parse().unwrap();
            NaiveTime::from_hms_opt(h, m, 0).unwrap()
        };

        // 간이 aggregate: 같은 5m 버킷 (start = t/5*5) 으로 묶어 OHLC 생성
        let mut ws_buckets: BTreeMap<NaiveTime, (i64, i64, i64, i64)> = BTreeMap::new();
        for c in ws_1m {
            let t = parse(&c.time);
            let bucket_min = (t.minute() / 5) * 5;
            let bucket_start = NaiveTime::from_hms_opt(t.hour(), bucket_min, 0).unwrap();
            let bucket_end = bucket_start + chrono::Duration::minutes(5);
            if bucket_end > now {
                continue; // 미완성 버킷 제거
            }
            let entry = ws_buckets
                .entry(bucket_start)
                .or_insert((c.open, c.high, c.low, c.close));
            entry.1 = entry.1.max(c.high);
            entry.2 = entry.2.min(c.low);
            entry.3 = c.close;
        }

        let mut by_time: BTreeMap<NaiveTime, (i64, i64, i64, i64)> = BTreeMap::new();

        // 과거 완료 버킷: backfill 우선
        for b in backfill_5m {
            let t = parse(&b.time);
            let end = t + chrono::Duration::minutes(5);
            if end <= now {
                by_time.insert(t, (b.open, b.high, b.low, b.close));
            }
        }

        // WS 5m 집계 결과: 같은 시각 미존재일 때만 삽입
        for (t, ohlc) in ws_buckets {
            by_time.entry(t).or_insert(ohlc);
        }

        by_time
            .into_iter()
            .map(|(t, (o, h, l, c))| (t, o, h, l, c))
            .collect()
    }

    fn candle(
        time: &str,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        interval_min: u32,
    ) -> CompletedCandle {
        CompletedCandle {
            stock_code: "122630".to_string(),
            time: time.to_string(),
            open,
            high,
            low,
            close,
            volume: 0,
            is_realtime: interval_min == 1,
            interval_min,
        }
    }

    #[test]
    fn past_bucket_prefers_backfill_over_ws() {
        // 9:40 버킷이 WS (102,810) 와 Yahoo (102,830) 둘 다 존재.
        // 2026-04-17 실제 사고: WS 는 high tick 누락으로 102,740. Yahoo 가 진실.
        // 과거 완료 버킷은 backfill 우선이어야 함.
        let now = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let ws_1m = vec![
            candle("09:40", 102330, 102810, 102250, 102760, 1),
            candle("09:41", 102760, 102790, 102700, 102750, 1),
            candle("09:42", 102750, 102790, 102700, 102730, 1),
            candle("09:43", 102730, 102780, 102700, 102740, 1),
            candle("09:44", 102740, 102810, 102700, 102760, 1),
        ];
        let backfill_5m = vec![
            candle("09:40", 102330, 102830, 102250, 102760, 5), // Yahoo: high 102,830
        ];
        let merged = merge_canonical(&ws_1m, &backfill_5m, now);

        assert_eq!(merged.len(), 1);
        let (_, _, high, _, _) = merged[0];
        assert_eq!(
            high, 102830,
            "과거 완료 버킷은 Yahoo backfill 의 high 우선이어야 함"
        );
    }

    #[test]
    fn past_bucket_falls_back_to_ws_if_no_backfill() {
        let now = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let ws_1m = vec![
            candle("09:40", 102330, 102810, 102250, 102760, 1),
            candle("09:41", 102760, 102820, 102700, 102750, 1),
            candle("09:42", 102750, 102830, 102700, 102730, 1), // WS high = 102,830
        ];
        let backfill_5m: Vec<CompletedCandle> = Vec::new();
        let merged = merge_canonical(&ws_1m, &backfill_5m, now);

        assert_eq!(merged.len(), 1);
        let (_, _, high, _, _) = merged[0];
        assert_eq!(
            high, 102830,
            "backfill 이 없으면 WS fallback — high 는 1m 집계의 max"
        );
    }

    #[test]
    fn current_bucket_uses_ws_only() {
        // 현재 시각이 진행 중 버킷 09:55 중간 (09:57).
        // Yahoo 는 아직 이 버킷 미생성. WS 집계도 아직 미완성 → 결과 없음.
        let now = NaiveTime::from_hms_opt(9, 57, 0).unwrap();
        let ws_1m = vec![
            candle("09:55", 102300, 102400, 102250, 102350, 1),
            candle("09:56", 102350, 102380, 102300, 102330, 1),
        ];
        let backfill_5m: Vec<CompletedCandle> = Vec::new();
        let merged = merge_canonical(&ws_1m, &backfill_5m, now);

        // 진행 중 버킷은 aggregate 가 제외 → 빈 결과
        assert!(merged.is_empty(), "진행 중 버킷은 canonical 에서 제외");
    }
}

/// 2026-04-17 v3 불변식 #1 회귀 테스트.
/// paper 기본 `LiveRunnerConfig::default()` 가 15m 단일 stage 로 고정된 상태를
/// 유지하고, 5m/30m 이 차단되는지 감시. 전략 명세(`AGENTS.md:4`) 불변식.
#[cfg(test)]
mod live_runner_config_default_tests {
    use super::{
        ArmedWaitOutcome, ArmedWatchStats, ExecutionPolicyKind, ExternalGates, IndexRegime,
        LiveRunnerConfig, TimeoutPolicy,
    };
    use chrono::NaiveTime;
    use chrono::Utc;

    #[test]
    fn paper_default_allowed_stages_is_15m_only() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(
            cfg.allowed_stages,
            Some(vec!["15m".to_string()]),
            "paper 기본 allowed_stages 는 15m 단일이어야 한다 (v3 불변식 #1a)"
        );
    }

    #[test]
    fn default_blocks_5m_and_30m_stages() {
        let cfg = LiveRunnerConfig::default();
        assert!(!cfg.is_stage_allowed("5m"), "5m 차단");
        assert!(!cfg.is_stage_allowed("30m"), "30m 차단");
    }

    #[test]
    fn default_accepts_15m_stage() {
        let cfg = LiveRunnerConfig::default();
        assert!(cfg.is_stage_allowed("15m"));
    }

    // v3 불변식 #1 Stage 3: `first_allowed_stage_name` 회귀.
    // 웹 상태(state.or_high/or_low) 에 노출되는 값이 **진입 허용 stage** 의
    // 첫 항목을 따라가는지 확인한다. `available_stages` 가 5m/15m 둘 다
    // 가용하더라도 allowed_stages 가 15m 뿐이면 15m 를 반환해야 한다.

    #[test]
    fn first_allowed_stage_picks_allowed_15m_over_earlier_5m() {
        let cfg = LiveRunnerConfig::default(); // allowed = 15m only
        let got = cfg.first_allowed_stage_name(["5m", "15m", "30m"]);
        assert_eq!(got, Some("15m"));
    }

    #[test]
    fn first_allowed_stage_returns_none_when_no_overlap() {
        let cfg = LiveRunnerConfig::default(); // allowed = 15m only
        let got = cfg.first_allowed_stage_name(["5m", "30m"]);
        assert_eq!(
            got, None,
            "허용 stage 가 없으면 or_high/or_low 는 비어 있어야 한다"
        );
    }

    #[test]
    fn first_allowed_stage_returns_first_when_allowed_is_none() {
        let cfg = LiveRunnerConfig {
            allowed_stages: None,
            ..LiveRunnerConfig::default()
        };
        let got = cfg.first_allowed_stage_name(["5m", "15m", "30m"]);
        assert_eq!(got, Some("5m"));
    }

    #[test]
    fn first_allowed_stage_returns_none_for_empty_input() {
        let cfg = LiveRunnerConfig::default();
        let empty: [&str; 0] = [];
        let got = cfg.first_allowed_stage_name(empty);
        assert_eq!(got, None);
    }

    /// 2026-04-18 Phase 3: entry_fill_timeout 기본값 + clamp 회귀.
    #[test]
    fn entry_fill_timeout_default_is_60_seconds() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(
            cfg.entry_fill_timeout_ms, 60_000,
            "paper passive 기본값은 60초 — 52.9초 tail 체결을 흡수해야 한다"
        );
        assert_eq!(cfg.entry_fill_timeout(), std::time::Duration::from_secs(60));
    }

    #[test]
    fn real_mode_defaults_use_marketable_and_fast_timing() {
        let cfg = LiveRunnerConfig::for_real_mode(
            vec!["15m".to_string()],
            NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
            true,
            None,
        );

        assert_eq!(cfg.entry_fill_timeout_ms, 3_000);
        assert_eq!(cfg.poll_interval_ms, 1_000);
        assert_eq!(cfg.manage_poll_interval_ms, 3_000);
        assert_eq!(
            cfg.execution_policy_kind,
            ExecutionPolicyKind::MarketableLimit {
                tick_offset: 1,
                slippage_budget_pct: 0.003,
            }
        );
    }

    #[test]
    fn entry_fill_timeout_clamps_too_short_value() {
        let cfg = LiveRunnerConfig {
            entry_fill_timeout_ms: 50, // 너무 짧음
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            cfg.entry_fill_timeout(),
            std::time::Duration::from_millis(500),
            "500ms 미만은 500ms 로 clamp — 네트워크 왕복조차 불가능한 값 방어"
        );
    }

    #[test]
    fn entry_fill_timeout_clamps_excessive_value() {
        let cfg = LiveRunnerConfig {
            entry_fill_timeout_ms: 120_000,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            cfg.entry_fill_timeout(),
            std::time::Duration::from_millis(60_000),
            "60초 초과는 60초로 clamp — ETF 단기 신호에 2분은 전략 부적합"
        );
    }

    #[test]
    fn entry_fill_timeout_accepts_phase3_recommended_range() {
        // Phase 3 권장: 2~3 초
        let cfg = LiveRunnerConfig {
            entry_fill_timeout_ms: 3_000,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            cfg.entry_fill_timeout(),
            std::time::Duration::from_millis(3_000)
        );
    }

    #[test]
    fn exit_verification_budget_default_is_15_seconds() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(cfg.exit_verification_budget_ms, 15_000);
    }

    #[test]
    fn exit_fill_timeout_uses_verify_budget_when_it_is_longer() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(cfg.exit_fill_timeout(), std::time::Duration::from_secs(15));
    }

    #[test]
    fn execution_timeout_policy_uses_live_config_values() {
        let cfg = LiveRunnerConfig {
            real_mode: true,
            entry_fill_timeout_ms: 4_321,
            exit_verification_budget_ms: 12_000,
            ..LiveRunnerConfig::default()
        };
        let policy = cfg.execution_timeout_policy();
        assert_eq!(
            policy.entry_submit,
            TimeoutPolicy::real_defaults().entry_submit
        );
        assert_eq!(policy.entry_fill, std::time::Duration::from_millis(4_321));
        assert_eq!(policy.exit_fill, std::time::Duration::from_millis(12_000));
    }

    /// 2026-04-18 Phase 4: 신호 탐색 주기 설정 + clamp 회귀.
    #[test]
    fn poll_interval_default_is_5_seconds() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(cfg.poll_interval_ms, 5_000);
        assert_eq!(cfg.poll_interval(), std::time::Duration::from_secs(5));
    }

    #[test]
    fn poll_interval_clamps_too_short_value() {
        let cfg = LiveRunnerConfig {
            poll_interval_ms: 10,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            cfg.poll_interval(),
            std::time::Duration::from_millis(200),
            "200ms 미만은 CPU 낭비 — 200ms 로 clamp"
        );
    }

    #[test]
    fn poll_interval_clamps_excessive_value() {
        let cfg = LiveRunnerConfig {
            poll_interval_ms: 120_000,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            cfg.poll_interval(),
            std::time::Duration::from_millis(30_000),
            "30초 초과는 신호 타이밍 열화 우려 — 30초로 clamp"
        );
    }

    #[test]
    fn poll_interval_accepts_phase4_recommended_range() {
        // Phase 4 권장: 1~2 초
        let cfg = LiveRunnerConfig {
            poll_interval_ms: 1_500,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(cfg.poll_interval(), std::time::Duration::from_millis(1_500));
    }

    #[test]
    fn manage_poll_interval_default_is_3_seconds() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(cfg.manage_poll_interval_ms, 3_000);
        assert_eq!(
            cfg.manage_poll_interval(),
            std::time::Duration::from_secs(3)
        );
    }

    #[test]
    fn manage_poll_interval_clamps_extremes() {
        let too_short = LiveRunnerConfig {
            manage_poll_interval_ms: 50,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            too_short.manage_poll_interval(),
            std::time::Duration::from_millis(200)
        );

        let too_long = LiveRunnerConfig {
            manage_poll_interval_ms: 60_000,
            ..LiveRunnerConfig::default()
        };
        assert_eq!(
            too_long.manage_poll_interval(),
            std::time::Duration::from_millis(10_000),
            "관리 주기가 10초 초과면 TP 체결 지연이 과도함 — clamp"
        );
    }

    /// 2026-04-19 Doc review P1: armed watch 기본값 — paper 는 비활성(0), real 은 200ms.
    #[test]
    fn armed_watch_paper_default_is_zero_real_is_two_hundred_ms() {
        let paper = LiveRunnerConfig::default();
        assert_eq!(
            paper.armed_watch_budget_ms, 0,
            "paper 기본은 armed watch 비활성 — 즉시 진입 (기존 동작 보존)"
        );
        let real = LiveRunnerConfig::for_real_mode(
            vec!["15m".to_string()],
            chrono::NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
            true,
            None,
        );
        assert_eq!(
            real.armed_watch_budget_ms, 200,
            "real 기본은 200ms — 문서 Phase 4 권장 범위 중간값"
        );
    }

    #[test]
    fn armed_tick_check_interval_default_is_150ms() {
        let cfg = LiveRunnerConfig::default();
        assert_eq!(cfg.armed_tick_check_interval_ms, 150);
    }

    /// 2026-04-19 Doc review P2: ExternalGates 기본값 — 모든 게이트 열림.
    #[test]
    fn external_gates_default_is_open() {
        let g = ExternalGates::default();
        assert!(g.nav_gate_ok);
        assert_eq!(g.index_regime, IndexRegime::Bullish);
        assert!(g.index_regime.is_confirmed_directional());
        assert!(g.active_instrument.is_none());
        assert!(g.subscription_health_ok.is_none());
        assert!(g.spread_ok_override.is_none());
        assert!(g.updated_at.is_none());
    }

    /// PR #1.a: IndexRegime → regime_confirmed 축약 일관성.
    #[test]
    fn index_regime_confirmed_only_for_directional() {
        assert!(IndexRegime::Bullish.is_confirmed_directional());
        assert!(IndexRegime::Bearish.is_confirmed_directional());
        assert!(!IndexRegime::Neutral.is_confirmed_directional());
        assert!(!IndexRegime::Unknown.is_confirmed_directional());
    }

    /// PR #1.a: is_stale — updated_at=None 은 placeholder 이므로 절대 stale 이 아니어야 한다.
    #[test]
    fn is_stale_returns_false_when_placeholder() {
        let g = ExternalGates::default();
        assert!(!g.is_stale(Utc::now(), 1_000));
    }

    /// PR #1.a: is_stale — 음수 diff (clock skew) 는 stale 로 간주.
    #[test]
    fn is_stale_treats_clock_skew_as_stale() {
        use chrono::Duration;
        let future = Utc::now() + Duration::seconds(10);
        let g = ExternalGates {
            updated_at: Some(future),
            ..ExternalGates::default()
        };
        assert!(g.is_stale(Utc::now(), 60_000));
    }

    /// PR #1.a: is_stale — max_age_ms 경계.
    #[test]
    fn is_stale_boundary_on_max_age() {
        use chrono::Duration;
        let now = Utc::now();
        // 5초 전 갱신, 한계 1초 → stale
        let g_old = ExternalGates {
            updated_at: Some(now - Duration::seconds(5)),
            ..ExternalGates::default()
        };
        assert!(g_old.is_stale(now, 1_000));
        // 5초 전 갱신, 한계 10초 → fresh
        assert!(!g_old.is_stale(now, 10_000));
    }

    /// PR #1.a: LiveRunnerConfig 기본값과 for_real_mode 가 external_gate_max_age_ms 를 채운다.
    #[test]
    fn external_gate_max_age_defaults() {
        let paper = LiveRunnerConfig::default();
        assert_eq!(paper.external_gate_max_age_ms, 60_000);
        let real = LiveRunnerConfig::for_real_mode(
            vec!["15m".to_string()],
            NaiveTime::from_hms_opt(10, 30, 0).unwrap(),
            true,
            None,
        );
        assert_eq!(real.external_gate_max_age_ms, 15_000);
    }

    /// PR #1.b: placeholder(updated_at=None) → stale 아님, 모든 값 그대로 통과.
    #[test]
    fn apply_external_gates_placeholder_preserves_values() {
        use super::apply_external_gates;
        let g = ExternalGates::default();
        let app = apply_external_gates(
            &g,
            Utc::now(),
            60_000,
            true, // real_mode 여도 stale 아니면 차단 없음
            "122630",
            true,
            true,
        );
        assert!(!app.stale_detected);
        assert!(app.nav_gate_ok);
        assert!(app.regime_confirmed);
        assert_eq!(app.active_instrument.as_deref(), Some("122630"));
        assert!(app.subscription_health_ok);
        assert!(app.spread_ok);
    }

    /// PR #1.b: real_mode + stale → nav/regime 강제 false (fail-close).
    #[test]
    fn apply_external_gates_real_mode_stale_fails_closed() {
        use super::apply_external_gates;
        use chrono::Duration;
        let now = Utc::now();
        let g = ExternalGates {
            updated_at: Some(now - Duration::seconds(30)),
            ..ExternalGates::default()
        };
        let app = apply_external_gates(&g, now, 15_000, true, "122630", true, true);
        assert!(app.stale_detected);
        assert!(!app.nav_gate_ok);
        assert!(!app.regime_confirmed);
    }

    /// PR #1.b: paper_mode + stale → 외부 값 그대로 사용 (차단 없음).
    #[test]
    fn apply_external_gates_paper_mode_stale_passes_values_through() {
        use super::apply_external_gates;
        use chrono::Duration;
        let now = Utc::now();
        let g = ExternalGates {
            updated_at: Some(now - Duration::seconds(120)),
            nav_gate_ok: true,
            index_regime: IndexRegime::Bullish,
            ..ExternalGates::default()
        };
        let app = apply_external_gates(&g, now, 60_000, false, "122630", true, true);
        assert!(app.stale_detected);
        assert!(app.nav_gate_ok);
        assert!(app.regime_confirmed);
    }

    /// PR #1.b: subscription_health_ok 은 내부와 외부의 AND.
    #[test]
    fn apply_external_gates_subscription_health_is_and() {
        use super::apply_external_gates;
        let now = Utc::now();
        // 외부 None → 내부 그대로
        let g_none = ExternalGates {
            subscription_health_ok: None,
            ..ExternalGates::default()
        };
        let a = apply_external_gates(&g_none, now, 60_000, false, "122630", false, true);
        assert!(!a.subscription_health_ok);

        // 외부 Some(false) → AND 로 항상 false
        let g_false = ExternalGates {
            subscription_health_ok: Some(false),
            ..ExternalGates::default()
        };
        let b = apply_external_gates(&g_false, now, 60_000, false, "122630", true, true);
        assert!(!b.subscription_health_ok);

        // 외부 Some(true) + 내부 true → true
        let g_true = ExternalGates {
            subscription_health_ok: Some(true),
            ..ExternalGates::default()
        };
        let c = apply_external_gates(&g_true, now, 60_000, false, "122630", true, true);
        assert!(c.subscription_health_ok);
    }

    /// PR #1.b: spread_ok_override 는 Some 이면 내부 대신 사용.
    #[test]
    fn apply_external_gates_spread_override_replaces_internal() {
        use super::apply_external_gates;
        let now = Utc::now();
        // 외부 None → 내부 그대로
        let g_none = ExternalGates::default();
        let a = apply_external_gates(&g_none, now, 60_000, false, "122630", true, true);
        assert!(a.spread_ok);
        let b = apply_external_gates(&g_none, now, 60_000, false, "122630", true, false);
        assert!(!b.spread_ok);

        // 외부 Some(false) 는 내부 true 를 덮음
        let g_false = ExternalGates {
            spread_ok_override: Some(false),
            ..ExternalGates::default()
        };
        let c = apply_external_gates(&g_false, now, 60_000, false, "122630", true, true);
        assert!(!c.spread_ok);

        // 외부 Some(true) 는 내부 false 를 덮음 (명시적 승인)
        let g_true = ExternalGates {
            spread_ok_override: Some(true),
            ..ExternalGates::default()
        };
        let d = apply_external_gates(&g_true, now, 60_000, false, "122630", true, false);
        assert!(d.spread_ok);
    }

    /// PR #1.b: Neutral/Unknown regime 은 confirmed 아님 → regime_confirmed=false.
    #[test]
    fn apply_external_gates_non_directional_regime_blocks_entry() {
        use super::apply_external_gates;
        let now = Utc::now();
        for regime in [IndexRegime::Neutral, IndexRegime::Unknown] {
            let g = ExternalGates {
                index_regime: regime,
                ..ExternalGates::default()
            };
            let app = apply_external_gates(&g, now, 60_000, true, "122630", true, true);
            assert!(
                !app.regime_confirmed,
                "regime {:?} 는 confirmed 가 아니어야 함",
                regime
            );
        }
    }

    /// PR #1.b: active_instrument 가 None 이면 자기 종목으로 fallback.
    #[test]
    fn apply_external_gates_active_instrument_fallback_to_self() {
        use super::apply_external_gates;
        let now = Utc::now();
        let g = ExternalGates {
            active_instrument: None,
            ..ExternalGates::default()
        };
        let app = apply_external_gates(&g, now, 60_000, false, "114800", true, true);
        assert_eq!(app.active_instrument.as_deref(), Some("114800"));
    }

    // ─── PR #2.a/#2.b/#2.c/#2.d: armed watch 관측성 ──────────────────────────

    /// PR #2.a: 모든 outcome 이 고유 label 을 가진다.
    #[test]
    fn armed_wait_outcome_labels_are_unique_per_variant() {
        let labels: Vec<&'static str> = [
            ArmedWaitOutcome::Ready,
            ArmedWaitOutcome::CutoffReached,
            ArmedWaitOutcome::PreflightFailed("reason"),
            ArmedWaitOutcome::DriftExceeded,
            ArmedWaitOutcome::Stopped,
            ArmedWaitOutcome::Aborted("reason"),
            ArmedWaitOutcome::AlreadyArmed,
        ]
        .iter()
        .map(|o| o.label())
        .collect();
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "label 이 중복되면 집계가 섞임");
    }

    /// PR #2.a: detail 은 reason 을 실은 variant 에만 Some.
    #[test]
    fn armed_wait_outcome_detail_only_for_parametric_variants() {
        assert_eq!(ArmedWaitOutcome::Ready.detail(), None);
        assert_eq!(ArmedWaitOutcome::CutoffReached.detail(), None);
        assert_eq!(ArmedWaitOutcome::DriftExceeded.detail(), None);
        assert_eq!(ArmedWaitOutcome::Stopped.detail(), None);
        assert_eq!(ArmedWaitOutcome::AlreadyArmed.detail(), None);
        assert_eq!(
            ArmedWaitOutcome::PreflightFailed("armed_preflight_nav_failed").detail(),
            Some("armed_preflight_nav_failed")
        );
        assert_eq!(
            ArmedWaitOutcome::Aborted("armed_manual_intervention").detail(),
            Some("armed_manual_intervention")
        );
    }

    /// PR #2.a: severity — 차단 성격 outcome 만 warning.
    #[test]
    fn armed_wait_outcome_severity_flags_warning_cases() {
        let warnings = [
            ArmedWaitOutcome::PreflightFailed("x"),
            ArmedWaitOutcome::DriftExceeded,
            ArmedWaitOutcome::Aborted("x"),
        ];
        for o in warnings {
            assert_eq!(o.severity(), "warning", "{:?}", o);
        }
        let infos = [
            ArmedWaitOutcome::Ready,
            ArmedWaitOutcome::CutoffReached,
            ArmedWaitOutcome::Stopped,
            ArmedWaitOutcome::AlreadyArmed,
        ];
        for o in infos {
            assert_eq!(o.severity(), "info", "{:?}", o);
        }
    }

    /// PR #2.b: 각 outcome record() 시 해당 counter 만 1 증가, duration 누적/max 반영.
    #[test]
    fn armed_watch_stats_record_increments_correct_counter() {
        let mut s = ArmedWatchStats::default();
        s.record(&ArmedWaitOutcome::Ready, 180);
        s.record(&ArmedWaitOutcome::PreflightFailed("x"), 50);
        s.record(&ArmedWaitOutcome::DriftExceeded, 120);
        s.record(&ArmedWaitOutcome::CutoffReached, 30);
        s.record(&ArmedWaitOutcome::Stopped, 20);
        s.record(&ArmedWaitOutcome::Aborted("y"), 90);
        s.record(&ArmedWaitOutcome::AlreadyArmed, 0);
        assert_eq!(s.ready_count, 1);
        assert_eq!(s.preflight_failed_count, 1);
        assert_eq!(s.drift_exceeded_count, 1);
        assert_eq!(s.cutoff_count, 1);
        assert_eq!(s.stopped_count, 1);
        assert_eq!(s.aborted_count, 1);
        assert_eq!(s.already_armed_count, 1);
        assert_eq!(s.total_count(), 7);
        assert_eq!(s.total_duration_ms, 180 + 50 + 120 + 30 + 20 + 90);
        assert_eq!(s.max_duration_ms, 180);
    }

    /// PR #2.b: 동일 outcome 여러 번 기록하면 합산되고 max 는 최대값 유지.
    #[test]
    fn armed_watch_stats_repeated_record_accumulates_and_tracks_max() {
        let mut s = ArmedWatchStats::default();
        s.record(&ArmedWaitOutcome::Ready, 100);
        s.record(&ArmedWaitOutcome::Ready, 250);
        s.record(&ArmedWaitOutcome::Ready, 50);
        assert_eq!(s.ready_count, 3);
        assert_eq!(s.total_duration_ms, 400);
        assert_eq!(s.max_duration_ms, 250);
    }

    /// PR #2.b: total_duration_ms 는 saturating_add — overflow 되지 않는다.
    #[test]
    fn armed_watch_stats_total_duration_saturates() {
        let mut s = ArmedWatchStats {
            total_duration_ms: u64::MAX - 10,
            ..ArmedWatchStats::default()
        };
        s.record(&ArmedWaitOutcome::Ready, 1_000);
        assert_eq!(s.total_duration_ms, u64::MAX);
    }

    /// PR #2.e: Ready 기록은 ready_duration_ms_sum/max 도 같이 갱신.
    #[test]
    fn armed_watch_stats_ready_duration_tracked_separately() {
        let mut s = ArmedWatchStats::default();
        s.record(&ArmedWaitOutcome::Ready, 180);
        s.record(&ArmedWaitOutcome::Ready, 220);
        s.record(&ArmedWaitOutcome::Ready, 100);
        // 다른 outcome 은 ready duration 에 영향 없음.
        s.record(&ArmedWaitOutcome::PreflightFailed("x"), 50);
        s.record(&ArmedWaitOutcome::DriftExceeded, 300);

        assert_eq!(s.ready_count, 3);
        assert_eq!(s.ready_duration_ms_sum, 500);
        assert_eq!(s.ready_duration_ms_max, 220);
        // preflight/drift duration 은 ready 에 누적되면 안 된다.
        assert_eq!(s.ready_avg_duration_ms(), Some(500.0 / 3.0));
        // 전체 total 에는 누적됨 (기존 계약 유지).
        assert_eq!(s.total_duration_ms, 180 + 220 + 100 + 50 + 300);
    }

    /// PR #2.e: Aborted 기록은 aborted_duration_ms_sum/max 도 같이 갱신.
    /// — manual/degraded 전이 후 watcher 깨움 지연 측정용.
    #[test]
    fn armed_watch_stats_aborted_duration_tracked_separately() {
        let mut s = ArmedWatchStats::default();
        s.record(&ArmedWaitOutcome::Aborted("armed_manual_intervention"), 45);
        s.record(&ArmedWaitOutcome::Aborted("armed_degraded"), 12);
        s.record(&ArmedWaitOutcome::Aborted("armed_manual_intervention"), 200);
        // Ready 는 aborted 에 영향 없음.
        s.record(&ArmedWaitOutcome::Ready, 500);

        assert_eq!(s.aborted_count, 3);
        assert_eq!(s.aborted_duration_ms_sum, 45 + 12 + 200);
        assert_eq!(s.aborted_duration_ms_max, 200);
        assert_eq!(
            s.aborted_avg_duration_ms(),
            Some((45 + 12 + 200) as f64 / 3.0)
        );
        // Ready 누적은 독립.
        assert_eq!(s.ready_duration_ms_sum, 500);
    }

    /// PR #2.e: 기록이 없으면 avg 는 None — 0으로 나눔 방지.
    #[test]
    fn armed_watch_stats_avg_returns_none_when_empty() {
        let s = ArmedWatchStats::default();
        assert_eq!(s.ready_avg_duration_ms(), None);
        assert_eq!(s.aborted_avg_duration_ms(), None);
    }

    // ── PR #2.g: race-free wake-up via watch channel ──────────────────────

    /// `mark_unchanged()` 직후 send 된 값을 `changed()` 가 즉시 반환해야 한다.
    /// 이전 Notify 구현에서는 set(new future) 직후 ~ 다음 enable() 이전 창에서
    /// 유실되던 update 가, watch channel 에서는 Sender 쪽 generation 이
    /// 영속되므로 놓치지 않음을 검증.
    #[tokio::test]
    async fn wake_channel_detects_update_after_mark_unchanged() {
        use std::sync::Arc;
        let (tx, _) = tokio::sync::watch::channel(0u64);
        let tx = Arc::new(tx);
        let mut rx = tx.subscribe();
        rx.mark_unchanged();

        // armed_wait_for_entry iteration 상상: mark_unchanged 직후 send 발생.
        tx.send_modify(|g| *g = g.wrapping_add(1));

        // changed() 는 50ms 내에 반드시 반환되어야 한다 (Ok = 변경 감지).
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx.changed()).await;
        assert!(result.is_ok(), "changed() 가 50ms 안에 반환되어야 함");
        assert!(result.unwrap().is_ok(), "Sender 살아있으므로 Ok");
        // 값 확인 — 기준값(0) 과 다름.
        assert_ne!(*rx.borrow(), 0u64);
    }

    /// changed() 가 select! branch 에서 취소되어도 generation 은 보존되어,
    /// 다음 `changed()` 호출이 놓치지 않음을 검증.
    #[tokio::test]
    async fn wake_channel_cancel_safe_across_select() {
        use std::sync::Arc;
        let (tx, _) = tokio::sync::watch::channel(0u64);
        let tx = Arc::new(tx);
        let mut rx = tx.subscribe();
        rx.mark_unchanged();

        // 첫 번째 select: sleep 이 이기도록 아직 send 안 됨.
        tokio::select! {
            biased;
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            _ = rx.changed() => panic!("send 전인데 changed 가 반환됨"),
        }

        // select 종료 후 send — race 재현.
        tx.send_modify(|g| *g = g.wrapping_add(1));

        // 두 번째 select: changed 가 즉시 반환되어야 (미등록 창에 온 신호 보존).
        let got_change = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => false,
            _ = rx.changed() => true,
        };
        assert!(
            got_change,
            "cancel-safe 라면 두 번째 select 에서 changed 감지"
        );
    }
}

impl Default for LiveRunnerConfig {
    fn default() -> Self {
        Self {
            max_entry_drift_pct: None,
            // 2026-04-17 v3 불변식 #1a: paper 기본도 15m 단일 stage 고정.
            // AGENTS.md 의 전략 명세 "15 분 OR 기준, 5 분봉으로 FVG" 를 라이브
            // 기본값과 일치시킨다. 기존 None 은 5m/15m/30m 모두 허용이었고
            // 2026-04-17 사고에서 5m 신호가 거래 2 건의 기반이 됐다.
            allowed_stages: Some(vec!["15m".to_string()]),
            max_daily_trades_override: None,
            entry_cutoff_override: None,
            // 기본은 주문 허용(모의). 실전에서 차단하려면 명시적으로 false.
            enable_real_orders: true,
            real_mode: false,
            global_trade_gate: None,
            // 2026-04-24 P1-1: paper 패시브 지정가 tail 체결을 흡수하기 위해 60초로 상향.
            entry_fill_timeout_ms: 60_000,
            // Phase 3: verify_exit_completion 내부 3단 검증 예상 소요 + 여유.
            exit_verification_budget_ms: 15_000,
            // Phase 4: 기존 5초 폴링 보존 (운영자 opt-in 으로만 단축).
            poll_interval_ms: 5_000,
            // Phase 4: 기존 3초 관리 주기 보존.
            manage_poll_interval_ms: 3_000,
            // Review Finding #3: 기본 정책은 기존 passive 유지 (행동 변경 없음).
            execution_policy_kind: ExecutionPolicyKind::PassiveZoneEdge,
            // Doc review P1 (Phase 4): paper 기본은 armed watch 비활성 (즉시 진입).
            // 운영자가 env/CLI 로 조절. 실전 기본은 for_real_mode 에서 200ms 설정.
            armed_watch_budget_ms: 0,
            armed_tick_check_interval_ms: 150,
            // Doc revision Phase 3: verify_exit_completion 내부 상수 (기존 동작 보존).
            exit_ws_notice_timeout_ms: 3_000,
            exit_rest_retries: 3,
            exit_rest_retry_interval_ms: 1_000,
            exit_balance_retries: 3,
            exit_balance_retry_interval_ms: 1_000,
            // PR #1.a: paper 기본 60초 — 외부 producer 가 없어도 동작.
            external_gate_max_age_ms: 60_000,
        }
    }
}

impl LiveRunnerConfig {
    /// 실전 플래그를 반영해 환경변수 기반 보수적 기본값으로 구성.
    ///
    /// 전역 1회 거래 가드는 `global_trade_gate` 가 수행한다. 종목별 1회(`max_daily_trades_override=1`)
    /// 는 시스템 전체 한도와 별개의 2차 방어선 — 예컨대 전역 한도가 2 라도 한 종목이 2회 연속 진입하는 것을 막는다.
    pub fn for_real_mode(
        allowed_stages: Vec<String>,
        entry_cutoff: NaiveTime,
        enable_real_orders: bool,
        global_trade_gate: Option<Arc<RwLock<GlobalTradeGate>>>,
    ) -> Self {
        Self {
            max_entry_drift_pct: Some(0.005),
            allowed_stages: Some(allowed_stages),
            max_daily_trades_override: Some(1),
            entry_cutoff_override: Some(entry_cutoff),
            enable_real_orders,
            real_mode: true,
            global_trade_gate,
            entry_fill_timeout_ms: 3_000,
            exit_verification_budget_ms: 15_000,
            poll_interval_ms: 1_000,
            manage_poll_interval_ms: 3_000,
            execution_policy_kind: ExecutionPolicyKind::MarketableLimit {
                tick_offset: 1,
                slippage_budget_pct: 0.003,
            },
            // Doc review P1 (Phase 4): 실전 기본은 200ms armed watch — 문서 권장 범위의 중간값.
            armed_watch_budget_ms: 200,
            armed_tick_check_interval_ms: 150,
            // Phase 3 timeout 분해 — 실전도 기본값 유지 (운영자가 별도 CLI 로 단축 가능).
            exit_ws_notice_timeout_ms: 3_000,
            exit_rest_retries: 3,
            exit_rest_retry_interval_ms: 1_000,
            exit_balance_retries: 3,
            exit_balance_retry_interval_ms: 1_000,
            // PR #1.a: 실전은 15초 — 지수/NAV 스트림 1~2초 주기를 고려한 보수적 상한.
            external_gate_max_age_ms: 15_000,
        }
    }

    /// 2026-04-18 Phase 3: 진입 체결 대기 timeout (`Duration`).
    /// `entry_fill_timeout_ms` 를 Duration 으로 변환. clamp [500ms, 60_000ms]
    /// 로 잘못된 설정(0 또는 과도한 값)을 방어.
    pub fn entry_fill_timeout(&self) -> std::time::Duration {
        let ms = self.entry_fill_timeout_ms.clamp(500, 60_000);
        std::time::Duration::from_millis(ms)
    }

    /// 2026-04-21 PR #5.d-wire: 청산 verify 전체를 감싸는 outer timeout.
    ///
    /// 내부 WS/REST/balance 재시도 합보다 짧은 outer timeout 을 두면 정상 검증 경로가
    /// 먼저 끊길 수 있다. 따라서 명시 budget 과 세부 재시도 합 중 더 긴 값을 사용한다.
    pub fn exit_fill_timeout(&self) -> std::time::Duration {
        let budget_ms = self.exit_verification_budget_ms.clamp(1_000, 60_000);
        let ws_ms = self.exit_ws_notice_timeout_ms.clamp(500, 30_000);
        let rest_ms = (self.exit_rest_retries.clamp(1, 10) as u64)
            .saturating_mul(self.exit_rest_retry_interval_ms.clamp(200, 5_000));
        let balance_ms = (self.exit_balance_retries.clamp(1, 10) as u64)
            .saturating_mul(self.exit_balance_retry_interval_ms.clamp(200, 5_000));
        let outer_ms = budget_ms.max(ws_ms.saturating_add(rest_ms).saturating_add(balance_ms));
        std::time::Duration::from_millis(outer_ms)
    }

    /// 2026-04-21 PR #5.d-wire: execution actor timeout policy 실제 연결.
    pub fn execution_timeout_policy(&self) -> TimeoutPolicy {
        let mut policy = if self.real_mode {
            TimeoutPolicy::real_defaults()
        } else {
            TimeoutPolicy::paper_defaults()
        };
        policy.entry_fill = self.entry_fill_timeout();
        policy.exit_fill = self.exit_fill_timeout();
        policy
    }

    /// 2026-04-18 Phase 4: 신호 탐색 sleep interval. clamp [200ms, 30_000ms].
    /// 200ms 미만은 CPU 낭비, 30s 초과는 신호 타이밍 열화 우려 — 둘 다 방어.
    pub fn poll_interval(&self) -> std::time::Duration {
        let ms = self.poll_interval_ms.clamp(200, 30_000);
        std::time::Duration::from_millis(ms)
    }

    /// 2026-04-18 Phase 4: 포지션 관리 sleep interval. clamp [200ms, 10_000ms].
    pub fn manage_poll_interval(&self) -> std::time::Duration {
        let ms = self.manage_poll_interval_ms.clamp(200, 10_000);
        std::time::Duration::from_millis(ms)
    }

    /// 실전 또는 모의 환경에서 허용 stage 인지 판정.
    pub fn is_stage_allowed(&self, stage_name: &str) -> bool {
        match &self.allowed_stages {
            None => true,
            Some(list) => list.iter().any(|s| s == stage_name),
        }
    }

    /// 사용 가능한 stage 목록에서 **진입 허용** 인 첫 번째 stage 이름을 반환.
    ///
    /// v3 불변식 #1 Stage 2: 웹 상태(`state.or_high`/`state.or_low`) 는
    /// 관찰용 UI 에 노출되는데, 허용 stage 가 15m 뿐인 실전 모드에서도
    /// `available_stages[0]` 이 5m 이면 5m OR 값이 표시되는 혼선이 있었다.
    /// 본 메서드는 **진입 경로와 동일한 필터** 를 적용하여 첫 허용 stage 만
    /// 반환한다. 허용 stage 중 아직 집계되지 않은 경우 None.
    pub fn first_allowed_stage_name<'a>(
        &self,
        available_stage_names: impl IntoIterator<Item = &'a str>,
    ) -> Option<&'a str> {
        available_stage_names
            .into_iter()
            .find(|name| self.is_stage_allowed(name))
    }

    /// max_daily_trades 실효값 — override 가 있으면 그 값을, 없으면 주어진 기본값 사용.
    pub fn effective_max_daily_trades(&self, default_value: usize) -> usize {
        self.max_daily_trades_override.unwrap_or(default_value)
    }

    /// entry_cutoff 실효값 — override 가 있으면 그 값을, 없으면 주어진 기본값 사용.
    pub fn effective_entry_cutoff(&self, default_value: NaiveTime) -> NaiveTime {
        self.entry_cutoff_override.unwrap_or(default_value)
    }
}

/// 실시간 1분봉을 N분봉으로 집계하되, 아직 닫히지 않은 마지막 버킷은 제외한다.
///
/// 라이브는 분 단위 완료 캔들을 5초마다 재평가하므로, 이 보정이 없으면 10:03 시점에
/// 10:00~10:04 미완성 5분봉을 신호 캔들처럼 읽어 백테스트/문서 규칙보다 앞서 진입할
/// 수 있다. 마지막 bucket_end <= now 인 경우에만 확정 봉으로 간주한다.
fn aggregate_completed_bars(
    candles: &[MinuteCandle],
    interval_min: u32,
    now: NaiveTime,
) -> Vec<MinuteCandle> {
    let mut aggregated = candle::aggregate(candles, interval_min);
    if let Some(last) = aggregated.last() {
        let bucket_end = last.time + chrono::Duration::minutes(interval_min as i64);
        if now < bucket_end {
            aggregated.pop();
        }
    }
    aggregated
}

/// Multi-stage OR 탐색에서 선착순으로 확정된 진입 신호.
///
/// `signal_time` 은 FVG 리트레이스가 확인된 5분봉 시각(`c5.time`).
/// `abort_entry` 로 진입 포기 시 search_after 를 이 시각으로 전진하여 같은 FVG 재감지를 차단한다.
///
/// FVG 메타데이터는 수익 분석용:
/// - 어떤 gap 크기/돌파 강도/거래량의 FVG가 체결·수익을 냈는지 `trades` 와 조인하여 분석.
/// - `abort_entry` 이벤트에도 같은 메타데이터를 붙여, 버려진 FVG의 가상 성과 사후 계산에 활용.
#[derive(Debug, Clone)]
struct BestSignal {
    side: PositionSide,
    entry_price: i64,
    stop_loss: i64,
    take_profit: i64,
    stage_name: &'static str,
    or_high: i64,
    or_low: i64,
    signal_time: NaiveTime,
    // FVG 품질 지표
    gap_top: i64,
    gap_bottom: i64,
    gap_size_pct: f64,    // (top - bottom) / bottom
    b_body_ratio: f64,    // b.body_size() / a.range()
    or_breakout_pct: f64, // (b.close - or_high) / or_high (Long) / (or_low - b.close) / or_low (Short)
    b_volume: u64,
    b_close: i64,
    a_range: i64,
    b_time: NaiveTime, // FVG 형성 B 캔들 시각
}

type AvailableOrStage = (&'static str, i64, i64, NaiveTime);

impl BestSignal {
    fn signal_id(&self, stock_code: &str) -> String {
        format!(
            "{stock_code}:{}:{}:{}:{:?}:{}",
            self.stage_name,
            self.signal_time.format("%H%M%S"),
            self.b_time.format("%H%M%S"),
            self.side,
            self.entry_price
        )
    }

    /// entry_signal / abort_entry 이벤트의 공통 metadata JSON.
    fn to_event_metadata(&self, current_price: Option<i64>) -> serde_json::Value {
        serde_json::json!({
            "stage": self.stage_name,
            "side": format!("{:?}", self.side),
            "entry": self.entry_price,
            "sl": self.stop_loss,
            "tp": self.take_profit,
            "signal_time": self.signal_time.to_string(),
            "b_time": self.b_time.to_string(),
            "gap_top": self.gap_top,
            "gap_bottom": self.gap_bottom,
            "gap_size_pct": self.gap_size_pct,
            "b_body_ratio": self.b_body_ratio,
            "or_breakout_pct": self.or_breakout_pct,
            "b_volume": self.b_volume,
            "b_close": self.b_close,
            "a_range": self.a_range,
            "current_price": current_price,
        })
    }
}

impl LiveRunner {
    pub fn new(
        client: Arc<KisHttpClient>,
        stock_code: StockCode,
        stock_name: String,
        quantity: u64,
        stop_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            client,
            strategy: OrbFvgStrategy {
                config: OrbFvgConfig::default(),
            },
            stock_code,
            stock_name,
            quantity,
            stop_flag,
            state: Arc::new(RwLock::new(RunnerState {
                phase: "초기화".to_string(),
                today_trades: Vec::new(),
                today_pnl: 0.0,
                current_position: None,
                or_high: None,
                or_low: None,
                or_stages: Vec::new(),
                market_halted: false,
                degraded: false,
                manual_intervention_required: false,
                degraded_reason: None,
                exit_pending: false,
                execution_state: ExecutionState::Flat,
                preflight_metadata: PreflightMetadata::default(),
                last_external_gate_stale: false,
                last_flat_transition_at: None,
                last_quote_sanity_issue: None,
                armed_stats: ArmedWatchStats::default(),
                current_armed_signal_id: None,
            })),
            trade_tx: None,
            realtime_rx: None,
            latest_price: Arc::new(RwLock::new(None)),
            latest_quote: Arc::new(RwLock::new(None)),
            stop_notify: Arc::new(Notify::new()),
            armed_wake_tx: Arc::new(watch::channel(0u64).0),
            ws_candles: None,
            db_store: None,
            position_lock: None,
            signal_state: Arc::new(RwLock::new(LiveSignalState::default())),
            event_logger: None,
            live_cfg: LiveRunnerConfig::default(),
            recent_orders: Arc::new(Mutex::new(VecDeque::new())),
            external_gates: None,
        }
    }

    pub(crate) fn with_position_lock(mut self, lock: Arc<RwLock<PositionLockState>>) -> Self {
        self.position_lock = Some(lock);
        self
    }

    /// 라이브 전용 설정 주입 (drift 가드 활성화 등).
    pub fn with_live_config(mut self, cfg: LiveRunnerConfig) -> Self {
        self.live_cfg = cfg;
        self
    }

    /// 2026-04-19 Doc review P2: Signal Engine 의 ETF 게이트 주입 훅.
    /// Arc 공유로 외부가 `update_nav_gate` / `update_regime` / `update_active_instrument`
    /// 같은 REST 관리 API 에서 런타임에 값을 갱신할 수 있다.
    pub fn with_external_gates(mut self, gates: Arc<RwLock<ExternalGates>>) -> Self {
        self.external_gates = Some(gates);
        self
    }

    pub fn with_event_logger(mut self, logger: Arc<EventLogger>) -> Self {
        self.event_logger = Some(logger);
        self
    }

    pub fn with_db_store(mut self, store: Arc<PostgresStore>) -> Self {
        self.db_store = Some(store);
        self
    }

    pub fn with_ws_candles(
        mut self,
        candles: Arc<RwLock<std::collections::HashMap<String, Vec<CompletedCandle>>>>,
    ) -> Self {
        self.ws_candles = Some(candles);
        self
    }

    /// 미체결 주문 전체 취소 — 이 종목에 한정.
    ///
    /// 반환: 취소 성공한 건수 (0 = 미체결이 없거나 전부 실패).
    /// 조회/취소 중 API 오류 발생 시 `Err`. 호출자는 `Err` 를 "미체결이 남아있을 수도 있음" 으로
    /// 간주하고 후속 경로(balance 재검증 등)로 방어해야 한다.
    ///
    /// `label` — event_log / tracing 라벨 구분용.
    /// - `"orphan_cleanup"`: 재시작 시 DB 에 active position 없는데 KIS 에 잔존한 주문 정리
    /// - `"entry_cancel_fallback"`: `execute_entry` 에서 cancel_tp_order 3회 실패 후 fallback
    /// - `"manual"`: 기타 호출 경로
    async fn cancel_all_pending_orders(&self, label: &str) -> Result<usize, KisError> {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
            ("INQR_DVSN_1", ""),
            ("INQR_DVSN_2", ""),
        ];

        let resp: KisResponse<Vec<serde_json::Value>> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-psbl-rvsecncl",
                &TransactionId::InquirePsblOrder,
                Some(&query),
                None,
            )
            .await?;

        let items = resp.output.or(resp.output1).unwrap_or_default();
        let mut cancelled = 0usize;
        let mut failures = 0usize;
        for item in &items {
            let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
            if code != self.stock_code.as_str() {
                continue;
            }

            let order_no = item.get("odno").and_then(|v| v.as_str()).unwrap_or("");
            let qty_str = item.get("psbl_qty").and_then(|v| v.as_str()).unwrap_or("0");

            if order_no.is_empty() {
                continue;
            }

            let cancel_body = serde_json::json!({
                "CANO": self.client.account_no(),
                "ACNT_PRDT_CD": self.client.account_product_code(),
                "KRX_FWDG_ORD_ORGNO": "",
                "ORGN_ODNO": order_no,
                "ORD_DVSN": "00",
                "RVSE_CNCL_DVSN_CD": "02",
                "ORD_QTY": qty_str,
                "ORD_UNPR": "0",
                "QTY_ALL_ORD_YN": "Y",
            });

            let cancel_resp: Result<KisResponse<serde_json::Value>, _> = self
                .client
                .execute(
                    HttpMethod::Post,
                    "/uapi/domestic-stock/v1/trading/order-rvsecncl",
                    &TransactionId::OrderCancel,
                    None,
                    Some(&cancel_body),
                )
                .await;

            match cancel_resp {
                Ok(cr) if cr.rt_cd == "0" => {
                    cancelled += 1;
                    info!(
                        "{}: [{}] 미체결 주문 취소 — 주문번호={}",
                        self.stock_name, label, order_no
                    );
                }
                Ok(cr) => {
                    failures += 1;
                    warn!(
                        "{}: [{}] 미체결 주문 취소 실패 — {}: {}",
                        self.stock_name, label, order_no, cr.msg1
                    );
                }
                Err(e) => {
                    failures += 1;
                    warn!(
                        "{}: [{}] 미체결 주문 취소 에러 — {}: {e}",
                        self.stock_name, label, order_no
                    );
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        if cancelled > 0 || failures > 0 {
            info!(
                "{}: [{}] cancel_all 완료 — 성공 {}건 / 실패 {}건",
                self.stock_name, label, cancelled, failures
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "order",
                    "cancel_all_pending",
                    if failures > 0 { "warn" } else { "info" },
                    &format!(
                        "[{}] 취소 성공 {}건 / 실패 {}건",
                        label, cancelled, failures
                    ),
                    serde_json::json!({
                        "label": label,
                        "cancelled": cancelled,
                        "failures": failures,
                    }),
                );
            }
        }

        if cancelled > 0 {
            // 취소 후 잔고 반영 대기
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Ok(cancelled)
    }

    /// DB에서 활성 포지션 복구 (TP 주문번호 포함)
    /// 2026-04-19 Doc revision 3-1: 재시작 시 ExitPending/ExitPartial 복구.
    ///
    /// 서버 재시작 전에 청산 주문 제출까지는 갔지만 검증 완료 전에 종료된 경우. 자동 재청산은
    /// 중복 매도 위험이 크므로 아래 보수적 정책을 따른다 (문서 불변식 5).
    ///
    /// 1. 현재 잔고 조회
    /// 2. `holdings=0` → 재시작 동안 KRX 가 체결 완료한 경우. DB 정리 + Flat 완료.
    ///    단, 체결가는 미확인 (`fill_price_unknown=true` 로 journal 기록, trade 기록 없음).
    /// 3. `holdings>0` → 실제 보유 중 자동 재청산 금지. manual_intervention + 포지션 복구 (운영자 확인용).
    /// 4. 조회 실패 → 상태 불명. manual_intervention.
    async fn restore_exit_pending_state(&self, saved: &ActivePosition) {
        warn!(
            "{}: 재시작 복구 — ExitPending/ExitPartial 상태 (exit_order_no={}, reason={})",
            self.stock_name, saved.last_exit_order_no, saved.last_exit_reason
        );

        let holding = match self.fetch_holding_snapshot().await {
            Ok(Some((qty, _))) => Some(qty),
            Ok(None) => Some(0u64),
            Err(e) => {
                error!(
                    "{}: 재시작 복구 ExitPending — 잔고 조회 실패: {e}",
                    self.stock_name
                );
                None
            }
        };

        match holding {
            Some(0) => {
                // 재시작 동안 체결 완료 — Flat 자동 정리.
                info!(
                    "{}: 재시작 복구 ExitPending — 잔고 0 확인, Flat 자동 정리",
                    self.stock_name
                );
                if let Some(ref store) = self.db_store {
                    let _ = store.delete_active_position(self.stock_code.as_str()).await;
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: saved.last_exit_order_no.clone(),
                        signal_id: saved.signal_id.clone().unwrap_or_default(),
                        broker_order_id: saved.last_exit_order_no.clone(),
                        phase: RestartAction::ExitPendingAutoFlat.journal_phase().into(),
                        side: saved.side.clone(),
                        intended_price: Some(saved.entry_price),
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: Some(saved.quantity),
                        holdings_before: Some(saved.quantity),
                        holdings_after: Some(0),
                        resolution_source: "balance_only".into(),
                        reason: format!("restart_recovery:{}", saved.last_exit_reason),
                        error_message: String::new(),
                        metadata: serde_json::json!({
                            "fill_price_unknown": true,
                            "prior_execution_state": saved.execution_state,
                            "last_exit_reason": saved.last_exit_reason,
                            "last_exit_order_no": saved.last_exit_order_no,
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                if let Some(ref lock) = self.position_lock {
                    let mut guard = lock.write().await;
                    match &*guard {
                        PositionLockState::Held(c)
                        | PositionLockState::Pending { code: c, .. }
                        | PositionLockState::ManualHeld(c)
                            if c == self.stock_code.as_str() =>
                        {
                            *guard = PositionLockState::Free;
                        }
                        _ => {}
                    }
                }
                // state.execution_state 는 Flat 유지 (초기화 시 기본값).
            }
            Some(qty) => {
                // 잔고 남음 — manual_intervention. 포지션 복구하되 자동 청산 금지.
                error!(
                    "{}: 재시작 복구 ExitPending — 잔고 {}주 잔존. manual_intervention 전환",
                    self.stock_name, qty
                );
                self.enter_manual_after_restart_with_position(
                    saved,
                    qty,
                    "exit_pending_holdings_remaining",
                )
                .await;
            }
            None => {
                // 조회 실패 — 불변식 5 에 따라 manual.
                error!(
                    "{}: 재시작 복구 ExitPending — 잔고 조회 실패. manual_intervention 전환",
                    self.stock_name
                );
                self.enter_manual_after_restart_with_position(
                    saved,
                    saved.quantity.max(0) as u64,
                    "exit_pending_balance_lookup_failed",
                )
                .await;
            }
        }
    }

    /// 2026-04-19 Doc revision 3-1: 재시작 복구 manual 경로에서 포지션을 복구하고 manual 전이.
    ///
    /// `restore_exit_pending_state` 에서 manual 경로 결정 시 중복 코드를 줄이기 위해 분리.
    async fn enter_manual_after_restart_with_position(
        &self,
        saved: &ActivePosition,
        holdings_qty: u64,
        reason_tag: &str,
    ) {
        let side = match saved.side.as_str() {
            "Short" => PositionSide::Short,
            _ => PositionSide::Long,
        };
        {
            let mut state = self.state.write().await;
            state.current_position = Some(build_restart_restored_position(
                saved,
                side,
                holdings_qty,
                saved.entry_price,
                saved.stop_loss,
                saved.take_profit,
                saved.best_price,
                saved.original_sl,
                saved.reached_1r,
            ));
            state.phase = "수동 개입 필요 (재시작 복구)".to_string();
        }

        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: saved.last_exit_order_no.clone(),
                signal_id: saved.signal_id.clone().unwrap_or_default(),
                broker_order_id: saved.last_exit_order_no.clone(),
                phase: RestartAction::ExitPendingManualRestore { qty: holdings_qty }
                    .journal_phase()
                    .into(),
                side: saved.side.clone(),
                intended_price: Some(saved.entry_price),
                submitted_price: None,
                filled_price: None,
                filled_qty: None,
                holdings_before: Some(saved.quantity),
                holdings_after: Some(holdings_qty as i64),
                resolution_source: "unresolved".into(),
                reason: format!("restart_recovery:{reason_tag}"),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "prior_execution_state": saved.execution_state,
                    "manual_category": reason_tag,
                    "last_exit_reason": saved.last_exit_reason,
                    "last_exit_order_no": saved.last_exit_order_no,
                    "reason_tag": reason_tag,
                    "holdings_qty": holdings_qty,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: None,
                final_fill_at: None,
            };
            let _ = store.append_execution_journal(&entry).await;
        }

        self.trigger_manual_intervention(
            format!(
                "재시작 복구: 이전 ExitPending 이었고 현재 잔고 {}주 ({reason_tag})",
                holdings_qty
            ),
            serde_json::json!({
                "path": "check_and_restore_position.restore_exit_pending",
                "prior_execution_state": saved.execution_state,
                "holdings_qty": holdings_qty,
                "reason_tag": reason_tag,
            }),
            ManualInterventionMode::KeepLock,
        )
        .await;
    }

    async fn restore_entry_pending_state(&self, saved: &ActivePosition) {
        warn!(
            "{}: 재시작 복구 — EntryPending 상태 (entry_order_no={})",
            self.stock_name, saved.pending_entry_order_no
        );

        let execution_fill = if !saved.pending_entry_order_no.is_empty() {
            self.query_execution(&saved.pending_entry_order_no).await
        } else {
            None
        };

        let holding = match self.fetch_holding_snapshot().await {
            Ok(Some((qty, avg_opt))) => Some((qty, avg_opt)),
            Ok(None) => Some((0u64, None)),
            Err(e) => {
                error!(
                    "{}: 재시작 복구 EntryPending — 잔고 조회 실패: {e}",
                    self.stock_name
                );
                None
            }
        };

        match holding {
            Some((qty, avg_opt)) if qty > 0 => {
                let resolved_entry = resolve_restart_entry_price(
                    avg_opt,
                    execution_fill.map(|(price, _)| price),
                    saved.entry_price,
                );
                self.restore_entry_pending_as_manual(
                    saved,
                    qty,
                    resolved_entry,
                    "entry_pending_holdings_remaining",
                )
                .await;
            }
            Some((0, _)) => {
                if !saved.pending_entry_order_no.is_empty()
                    && !saved.pending_entry_krx_orgno.is_empty()
                {
                    match self
                        .cancel_tp_order(
                            &saved.pending_entry_order_no,
                            &saved.pending_entry_krx_orgno,
                        )
                        .await
                    {
                        Ok(()) => {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            match self.fetch_holding_snapshot().await {
                                Ok(Some((qty, avg_opt))) if qty > 0 => {
                                    let resolved_entry = resolve_restart_entry_price(
                                        avg_opt,
                                        execution_fill.map(|(price, _)| price),
                                        saved.entry_price,
                                    );
                                    self.restore_entry_pending_as_manual(
                                        saved,
                                        qty,
                                        resolved_entry,
                                        "entry_pending_restart_cancel_race",
                                    )
                                    .await;
                                }
                                Ok(_) => {
                                    self.clear_stale_entry_pending(
                                        saved,
                                        "entry_pending_restart_cancelled_flat",
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    if let Some(ref store) = self.db_store {
                                        let entry = ExecutionJournalEntry {
                                            stock_code: self.stock_code.as_str().to_string(),
                                            execution_id: saved.pending_entry_order_no.clone(),
                                            signal_id: saved.signal_id.clone().unwrap_or_default(),
                                            broker_order_id: saved.pending_entry_order_no.clone(),
                                            phase: RestartAction::EntryPendingManualOnQueryFail {
                                                reason: "post_cancel_balance_lookup_failed".into(),
                                            }
                                            .journal_phase()
                                            .into(),
                                            side: saved.side.clone(),
                                            intended_price: Some(
                                                saved.trigger_price.unwrap_or(saved.entry_price),
                                            ),
                                            submitted_price: Some(saved.entry_price),
                                            filled_price: None,
                                            filled_qty: None,
                                            holdings_before: Some(saved.quantity),
                                            holdings_after: None,
                                            resolution_source: "unresolved".into(),
                                            reason: "restart_entry_pending_post_cancel_balance_lookup_failed"
                                                .into(),
                                            error_message: e.to_string(),
                                            metadata: serde_json::json!({
                                                "prior_execution_state": saved.execution_state,
                                                "pending_entry_order_no": saved.pending_entry_order_no,
                                                "manual_category": "post_cancel_balance_lookup_failed",
                                            }),
                                            sent_at: None,
                                            ack_at: None,
                                            first_fill_at: None,
                                            final_fill_at: None,
                                        };
                                        let _ = store.append_execution_journal(&entry).await;
                                    }
                                    self.trigger_manual_intervention(
                                        format!(
                                            "재시작 복구 entry_pending 잔고 재확인 실패: {}",
                                            e
                                        ),
                                        serde_json::json!({
                                            "path": "check_and_restore_position.restore_entry_pending",
                                            "pending_entry_order_no": saved.pending_entry_order_no,
                                            "error": e.to_string(),
                                        }),
                                        ManualInterventionMode::KeepLock,
                                    )
                                    .await;
                                }
                            }
                        }
                        Err(e) => {
                            if let Some(ref store) = self.db_store {
                                let entry = ExecutionJournalEntry {
                                    stock_code: self.stock_code.as_str().to_string(),
                                    execution_id: saved.pending_entry_order_no.clone(),
                                    signal_id: saved.signal_id.clone().unwrap_or_default(),
                                    broker_order_id: saved.pending_entry_order_no.clone(),
                                    phase: RestartAction::EntryPendingManualOnQueryFail {
                                        reason: "cancel_failed".into(),
                                    }
                                    .journal_phase()
                                    .into(),
                                    side: saved.side.clone(),
                                    intended_price: Some(
                                        saved.trigger_price.unwrap_or(saved.entry_price),
                                    ),
                                    submitted_price: Some(saved.entry_price),
                                    filled_price: execution_fill.map(|(price, _)| price),
                                    filled_qty: execution_fill.map(|(_, qty)| qty as i64),
                                    holdings_before: Some(saved.quantity),
                                    holdings_after: Some(0),
                                    resolution_source: "unresolved".into(),
                                    reason: "restart_entry_pending_cancel_failed".into(),
                                    error_message: e.to_string(),
                                    metadata: serde_json::json!({
                                        "prior_execution_state": saved.execution_state,
                                        "pending_entry_order_no": saved.pending_entry_order_no,
                                        "manual_category": "cancel_failed",
                                    }),
                                    sent_at: None,
                                    ack_at: None,
                                    first_fill_at: None,
                                    final_fill_at: None,
                                };
                                let _ = store.append_execution_journal(&entry).await;
                            }
                            self.trigger_manual_intervention(
                                format!("재시작 복구 entry_pending 주문 취소 실패: {}", e),
                                serde_json::json!({
                                    "path": "check_and_restore_position.restore_entry_pending",
                                    "pending_entry_order_no": saved.pending_entry_order_no,
                                    "error": e.to_string(),
                                }),
                                ManualInterventionMode::KeepLock,
                            )
                            .await;
                        }
                    }
                } else {
                    self.clear_stale_entry_pending(saved, "entry_pending_restart_no_holdings")
                        .await;
                }
            }
            None => {
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: saved.pending_entry_order_no.clone(),
                        signal_id: saved.signal_id.clone().unwrap_or_default(),
                        broker_order_id: saved.pending_entry_order_no.clone(),
                        phase: RestartAction::EntryPendingManualOnQueryFail {
                            reason: "initial_balance_lookup_failed".into(),
                        }
                        .journal_phase()
                        .into(),
                        side: saved.side.clone(),
                        intended_price: Some(saved.trigger_price.unwrap_or(saved.entry_price)),
                        submitted_price: Some(saved.entry_price),
                        filled_price: execution_fill.map(|(price, _)| price),
                        filled_qty: execution_fill.map(|(_, qty)| qty as i64),
                        holdings_before: Some(saved.quantity),
                        holdings_after: None,
                        resolution_source: "unresolved".into(),
                        reason: "restart_entry_pending_balance_lookup_failed".into(),
                        error_message: "balance_lookup_failed".into(),
                        metadata: serde_json::json!({
                            "prior_execution_state": saved.execution_state,
                            "pending_entry_order_no": saved.pending_entry_order_no,
                            "manual_category": "initial_balance_lookup_failed",
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                self.trigger_manual_intervention(
                    format!(
                        "재시작 복구 entry_pending 잔고 조회 실패: {}",
                        saved.pending_entry_order_no
                    ),
                    serde_json::json!({
                        "path": "check_and_restore_position.restore_entry_pending",
                        "pending_entry_order_no": saved.pending_entry_order_no,
                    }),
                    ManualInterventionMode::KeepLock,
                )
                .await;
            }
            Some((qty, avg_opt)) => {
                let resolved_entry = resolve_restart_entry_price(
                    avg_opt,
                    execution_fill.map(|(price, _)| price),
                    saved.entry_price,
                );
                self.restore_entry_pending_as_manual(
                    saved,
                    qty,
                    resolved_entry,
                    "entry_pending_holdings_remaining",
                )
                .await;
            }
        }
    }

    async fn restore_entry_pending_as_manual(
        &self,
        saved: &ActivePosition,
        holdings_qty: u64,
        resolved_entry: i64,
        reason_tag: &str,
    ) {
        let side = match saved.side.as_str() {
            "Short" => PositionSide::Short,
            _ => PositionSide::Long,
        };
        let (restored_sl, restored_tp) =
            restore_bracket_from_saved(saved, side, resolved_entry, self.strategy.config.rr_ratio);

        {
            let mut state = self.state.write().await;
            state.or_high = saved.or_high;
            state.or_low = saved.or_low;
            state.current_position = Some(build_restart_restored_position(
                saved,
                side,
                holdings_qty,
                resolved_entry,
                restored_sl,
                restored_tp,
                resolved_entry,
                restored_sl,
                false,
            ));
        }

        if let Some(ref store) = self.db_store {
            let mut restored = saved.clone();
            restored.entry_price = resolved_entry;
            restored.stop_loss = restored_sl;
            restored.take_profit = restored_tp;
            restored.quantity = holdings_qty as i64;
            restored.original_sl = restored_sl;
            restored.reached_1r = false;
            restored.best_price = resolved_entry;
            restored.tp_order_no.clear();
            restored.tp_krx_orgno.clear();
            restored.execution_state = "manual_intervention".to_string();
            restored.last_exit_order_no.clear();
            restored.last_exit_reason.clear();
            restored.pending_entry_order_no.clear();
            restored.pending_entry_krx_orgno.clear();
            let _ = store.save_active_position(&restored).await;

            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: saved.pending_entry_order_no.clone(),
                signal_id: saved.signal_id.clone().unwrap_or_default(),
                broker_order_id: saved.pending_entry_order_no.clone(),
                phase: RestartAction::EntryPendingManualRestore { qty: holdings_qty }
                    .journal_phase()
                    .into(),
                side: saved.side.clone(),
                intended_price: Some(saved.trigger_price.unwrap_or(saved.entry_price)),
                submitted_price: Some(saved.entry_price),
                filled_price: Some(resolved_entry),
                filled_qty: Some(holdings_qty as i64),
                holdings_before: Some(saved.quantity),
                holdings_after: Some(holdings_qty as i64),
                resolution_source: "balance_only".into(),
                reason: format!("restart_recovery:{reason_tag}"),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "prior_execution_state": saved.execution_state,
                    "pending_entry_order_no": saved.pending_entry_order_no,
                    "restored_stop_loss": restored_sl,
                    "restored_take_profit": restored_tp,
                    "reason_tag": reason_tag,
                    "holdings_qty": holdings_qty,
                    // 2026-04-19 PR #3.f: 이전 버전은 reason_tag 를 무시하고
                    // "entry_pending_holdings_remaining" 을 하드코딩했으나, 실제
                    // reason_tag 는 `entry_pending_restart_cancel_race` 등도 들어온다.
                    "manual_category": reason_tag,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: None,
                final_fill_at: None,
            };
            let _ = store.append_execution_journal(&entry).await;
        }

        self.trigger_manual_intervention(
            format!(
                "재시작 복구 entry_pending 잔고 {}주 감지 — manual_intervention",
                holdings_qty
            ),
            serde_json::json!({
                "path": "check_and_restore_position.restore_entry_pending",
                "pending_entry_order_no": saved.pending_entry_order_no,
                "holdings_qty": holdings_qty,
                "restored_entry": resolved_entry,
                "reason_tag": reason_tag,
            }),
            ManualInterventionMode::KeepLock,
        )
        .await;
    }

    async fn clear_stale_entry_pending(&self, saved: &ActivePosition, reason_tag: &str) {
        if let Some(ref store) = self.db_store {
            let _ = store.delete_active_position(self.stock_code.as_str()).await;
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: saved.pending_entry_order_no.clone(),
                signal_id: saved.signal_id.clone().unwrap_or_default(),
                broker_order_id: saved.pending_entry_order_no.clone(),
                phase: RestartAction::EntryPendingAutoClear.journal_phase().into(),
                side: saved.side.clone(),
                intended_price: Some(saved.trigger_price.unwrap_or(saved.entry_price)),
                submitted_price: Some(saved.entry_price),
                filled_price: None,
                filled_qty: Some(0),
                holdings_before: Some(saved.quantity),
                holdings_after: Some(0),
                resolution_source: "balance_only".into(),
                reason: reason_tag.to_string(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "prior_execution_state": saved.execution_state,
                    "pending_entry_order_no": saved.pending_entry_order_no,
                    "reason_tag": reason_tag,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: None,
                final_fill_at: None,
            };
            let _ = store.append_execution_journal(&entry).await;
        }

        if let Some(ref lock) = self.position_lock {
            release_lock_if_self_held(lock, self.stock_code.as_str()).await;
        }
    }

    async fn check_and_restore_position(&self) {
        let Some(ref store) = self.db_store else {
            // DB 없으면 balance-first orphan cleanup 으로 fallback.
            // 2026-04-15 Codex review #3: 과거의 `check_and_restore_from_balance`
            // (cancel 먼저 + 보유 있으면 0.3% 임의 재구성) 경로를 `safe_orphan_cleanup`
            // 으로 교체하여 live TP 가 삭제되는 회귀를 차단한다.
            self.safe_orphan_cleanup("restore_fallback").await;
            return;
        };

        match store.get_active_position(self.stock_code.as_str()).await {
            Ok(Some(saved)) => {
                // 2026-04-19 Doc revision 3-1: ExitPending/ExitPartial 재시작 복구 분기.
                // saved.execution_state 가 'exit_pending' / 'exit_partial' 이면 재시작 전에 청산
                // 진행 중이었던 상태. 잔고를 확인해 자동 flat 또는 manual 승격.
                if matches!(
                    saved.execution_state.as_str(),
                    "exit_pending" | "exit_partial"
                ) {
                    self.restore_exit_pending_state(&saved).await;
                    return;
                }
                if saved.execution_state == "entry_pending" {
                    self.restore_entry_pending_state(&saved).await;
                    return;
                }

                info!(
                    "{}: DB에서 활성 포지션 복구 — {}주 @ {}원, TP주문={}",
                    self.stock_name, saved.quantity, saved.entry_price, saved.tp_order_no
                );

                // 이전 TP 지정가 취소 (주문번호를 알고 있으므로 확실히 취소 가능)
                let mut tp_cancel_failed = false;
                if !saved.tp_order_no.is_empty() {
                    match self
                        .cancel_tp_order(&saved.tp_order_no, &saved.tp_krx_orgno)
                        .await
                    {
                        Ok(()) => info!("{}: 이전 TP 주문 취소 성공", self.stock_name),
                        Err(e) => {
                            warn!("{}: 이전 TP 주문 취소 실패: {e}", self.stock_name);
                            tp_cancel_failed = true;
                        }
                    }
                    // 취소 후 잔고 반영 대기
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }

                // TP 취소 실패 시 잔고를 확인하여 유령 포지션 방지.
                // 재시작 동안 KRX가 TP를 체결했으면 보유 0주 → DB만 정리하고 깨끗하게 시작.
                if tp_cancel_failed && !self.check_balance_has_stock().await {
                    warn!(
                        "{}: TP 취소 실패 + 보유 0주 — 재시작 중 TP 체결된 것으로 판단, DB 정리",
                        self.stock_name
                    );
                    let _ = store.delete_active_position(self.stock_code.as_str()).await;
                    // PR #3.d: 유령 포지션 정리도 execution_journal 에 기록.
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: saved.signal_id.clone().unwrap_or_default(),
                        signal_id: saved.signal_id.clone().unwrap_or_default(),
                        broker_order_id: saved.tp_order_no.clone(),
                        phase: RestartAction::OpenPhantomCleanup.journal_phase().into(),
                        side: saved.side.clone(),
                        intended_price: Some(saved.entry_price),
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: Some(saved.quantity),
                        holdings_before: Some(saved.quantity),
                        holdings_after: Some(0),
                        resolution_source: "balance_only".into(),
                        reason: "restart_open_phantom_tp_cancel_failed".into(),
                        error_message: String::new(),
                        metadata: serde_json::json!({
                            "prior_execution_state": saved.execution_state,
                            "prior_tp_order_no": saved.tp_order_no,
                            "prior_tp_krx_orgno": saved.tp_krx_orgno,
                            "fill_price_unknown": true,
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                    // 포지션 잠금도 해제 (다른 종목 진입 허용).
                    // 2026-04-17 v3 fixups H: ManualHeld(self) 도 정리 대상.
                    // start_runner 가 manual 종목을 거부하므로 본 분기에 도달했다는 건
                    // 운영자가 manual 을 해제한 후 재시작한 정상 시나리오.
                    if let Some(ref lock) = self.position_lock {
                        let mut guard = lock.write().await;
                        match &*guard {
                            PositionLockState::Held(c)
                            | PositionLockState::Pending { code: c, .. }
                            | PositionLockState::ManualHeld(c)
                                if c == self.stock_code.as_str() =>
                            {
                                *guard = PositionLockState::Free;
                            }
                            _ => {}
                        }
                    }
                    info!(
                        "{}: 유령 포지션 정리 완료 — 깨끗한 상태에서 시작",
                        self.stock_name
                    );
                    return;
                }

                // 새 TP 지정가 발주
                let (tp_order_no, tp_krx_orgno, tp_limit_price) =
                    if let Some((no, orgno, placed_price)) = self
                        .place_tp_limit_order(saved.take_profit, saved.quantity as u64)
                        .await
                    {
                        (Some(no), Some(orgno), Some(placed_price))
                    } else {
                        warn!(
                            "{}: 복구 TP 지정가 발주 실패 — 시장가 fallback",
                            self.stock_name
                        );
                        (None, None, None)
                    };

                let mut state = self.state.write().await;
                state.current_position = Some(Position {
                    side: PositionSide::Long,
                    entry_price: saved.entry_price,
                    stop_loss: saved.stop_loss,
                    take_profit: saved.take_profit,
                    entry_time: saved.entry_time.time(),
                    quantity: saved.quantity as u64,
                    tp_order_no: tp_order_no.clone(),
                    tp_krx_orgno: tp_krx_orgno.clone(),
                    tp_limit_price,
                    reached_1r: saved.reached_1r,
                    best_price: saved.best_price,
                    original_sl: saved.original_sl,
                    sl_triggered_tick: false,
                    intended_entry_price: saved.entry_price,
                    order_to_fill_ms: 0,
                    signal_id: saved.signal_id.clone().unwrap_or_default(),
                });
                if saved.reached_1r {
                    info!(
                        "{}: 트레일링 상태 복구 — reached_1r=true, best_price={}",
                        self.stock_name, saved.best_price
                    );
                }
                state.phase = "포지션 보유 (복구)".to_string();

                // DB에서 모든 OR 단계 복구
                let today = Local::now().date_naive();
                if let Ok(stages) = store
                    .get_all_or_stages(self.stock_code.as_str(), today)
                    .await
                {
                    if !stages.is_empty() {
                        state.or_stages = stages.clone();
                        let restored_stage = saved.or_stage.as_deref().and_then(|wanted| {
                            stages
                                .iter()
                                .find(|(stage, _, _)| stage == wanted)
                                .map(|(_, high, low)| (wanted, *high, *low))
                        });
                        if let Some((stage, high, low)) = restored_stage {
                            state.or_high = Some(high);
                            state.or_low = Some(low);
                            info!(
                                "{}: OR 범위 DB 복구 — 진입 stage {} 기준으로 복원",
                                self.stock_name, stage
                            );
                        } else {
                            state.or_high = saved.or_high.or_else(|| stages.first().map(|s| s.1));
                            state.or_low = saved.or_low.or_else(|| stages.first().map(|s| s.2));
                            info!(
                                "{}: OR 범위 DB 복구 — stage 메타 미일치, 저장된 OR/fallback 사용",
                                self.stock_name
                            );
                        }
                        info!(
                            "{}: OR 범위 DB 복구 — {}단계",
                            self.stock_name,
                            stages.len()
                        );
                    } else {
                        state.or_high = saved.or_high;
                        state.or_low = saved.or_low;
                        state.or_stages = Vec::new();
                    }
                } else {
                    state.or_high = saved.or_high;
                    state.or_low = saved.or_low;
                }

                // DB에 새 TP 주문번호 갱신
                if let (Some(no), Some(orgno)) = (&tp_order_no, &tp_krx_orgno) {
                    let mut updated = saved.clone();
                    updated.tp_order_no = no.clone();
                    updated.tp_krx_orgno = orgno.clone();
                    let _ = store.save_active_position(&updated).await;
                }

                // 2026-04-17 긴급 follow-up: manual_intervention 영속화 복원.
                // watchdog 재시작 전 manual 전환된 종목은 재시작 후에도 자동 청산
                // 경로로 빠지지 않고 운영자 수동 처리를 대기해야 한다 (2026-04-17
                // 11:31 TimeStop 청산 사고 대응). state + stop_flag + position_lock
                // 을 helper 로 일괄 복원.
                if saved.manual_intervention_required {
                    warn!(
                        "{}: DB 복구 — manual_intervention 활성 상태 발견, state/lock/stop 복원",
                        self.stock_name
                    );
                    state.phase = "수동 개입 필요".to_string();
                    state.manual_intervention_required = true;
                    state.degraded = true;
                    state.degraded_reason =
                        Some("watchdog 재시작 전 manual_intervention 상태 복구".to_string());
                    // Phase 2: execution_state 도 함께 ManualIntervention 로.
                    state.execution_state = ExecutionState::ManualIntervention;
                    drop(state);
                    if let Some(ref lock) = self.position_lock {
                        *lock.write().await =
                            PositionLockState::ManualHeld(self.stock_code.as_str().to_string());
                    }
                    self.stop_flag.store(true, Ordering::Relaxed);
                    self.stop_notify.notify_waiters();
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            self.stock_code.as_str(),
                            "position",
                            "manual_intervention_restored",
                            "critical",
                            "재시작 — DB 에서 manual_intervention 상태 복원, 자동 운영 중단",
                            serde_json::json!({
                                "entry_price": saved.entry_price,
                                "quantity": saved.quantity,
                                "stop_loss": saved.stop_loss,
                                "take_profit": saved.take_profit,
                            }),
                        );
                    }
                    // PR #3.d: execution_journal 에도 manual 복원 기록.
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: saved.signal_id.clone().unwrap_or_default(),
                        signal_id: saved.signal_id.clone().unwrap_or_default(),
                        broker_order_id: saved.tp_order_no.clone(),
                        phase: RestartAction::ManualRestore {
                            qty: saved.quantity.max(0) as u64,
                        }
                        .journal_phase()
                        .into(),
                        side: saved.side.clone(),
                        intended_price: Some(saved.entry_price),
                        submitted_price: None,
                        filled_price: Some(saved.entry_price),
                        filled_qty: Some(saved.quantity),
                        holdings_before: Some(saved.quantity),
                        holdings_after: Some(saved.quantity),
                        resolution_source: "balance_only".into(),
                        reason: "restart_manual_intervention_preserved".into(),
                        error_message: String::new(),
                        metadata: serde_json::json!({
                            "prior_execution_state": saved.execution_state,
                            "stop_loss": saved.stop_loss,
                            "take_profit": saved.take_profit,
                            "manual_category": "watchdog_restart_preserved",
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                    info!(
                        "{}: 포지션 복구 완료 (manual 보호 유지) — SL={}, TP={}",
                        self.stock_name, saved.stop_loss, saved.take_profit
                    );
                    return;
                }

                // 정상 복구 경로 — state write lock 은 아직 잡혀 있으니 먼저 drop.
                drop(state);
                // Phase 2: execution_state → Open.
                self.transition_execution_state(ExecutionState::Open, "check_and_restore_position")
                    .await;

                // PR #3.d: Open 정상 복구도 execution_journal 에 기록 (재시작 복구 통계 완성).
                let side_val = match saved.side.as_str() {
                    "Short" => PositionSide::Short,
                    _ => PositionSide::Long,
                };
                let restart_action = RestartAction::OpenPositionRestore {
                    qty: saved.quantity.max(0) as u64,
                    side: side_val,
                    entry_price: saved.entry_price,
                    stop_loss: saved.stop_loss,
                    take_profit: saved.take_profit,
                };
                let entry = ExecutionJournalEntry {
                    stock_code: self.stock_code.as_str().to_string(),
                    execution_id: saved.signal_id.clone().unwrap_or_default(),
                    signal_id: saved.signal_id.clone().unwrap_or_default(),
                    broker_order_id: tp_order_no.clone().unwrap_or_default(),
                    phase: restart_action.journal_phase().into(),
                    side: saved.side.clone(),
                    intended_price: Some(saved.entry_price),
                    submitted_price: tp_limit_price,
                    filled_price: Some(saved.entry_price),
                    filled_qty: Some(saved.quantity),
                    holdings_before: Some(saved.quantity),
                    holdings_after: Some(saved.quantity),
                    resolution_source: "balance_only".into(),
                    reason: "restart_open_position_restore".into(),
                    error_message: String::new(),
                    metadata: serde_json::json!({
                        "prior_execution_state": saved.execution_state,
                        "restored_stop_loss": saved.stop_loss,
                        "restored_take_profit": saved.take_profit,
                        "reached_1r": saved.reached_1r,
                        "best_price": saved.best_price,
                        "new_tp_order_no": tp_order_no.clone().unwrap_or_default(),
                        "tp_limit_price": tp_limit_price,
                        "tp_rearm_success": tp_order_no.is_some(),
                    }),
                    sent_at: None,
                    ack_at: None,
                    first_fill_at: None,
                    final_fill_at: None,
                };
                let _ = store.append_execution_journal(&entry).await;

                info!(
                    "{}: 포지션 복구 완료 — SL={}, TP={}",
                    self.stock_name, saved.stop_loss, saved.take_profit
                );
                return;
            }
            Ok(None) => {
                info!(
                    "{}: DB에 활성 포지션 없음 — balance-first orphan cleanup",
                    self.stock_name
                );
                // 2026-04-15 Codex review #3 대응: 기존에는 `cancel_all_pending_orders`
                // 를 먼저 부르는 순서였으나, DB save 실패 후 재시작 시나리오에서
                // 실제 보유 중인 live position 의 broker-side TP 까지 함께 삭제돼
                // 보호 없는 포지션을 남기는 회귀가 있었다. `safe_orphan_cleanup` 은
                // 잔고 조회를 먼저 하여 정상 포지션이 있으면 cancel 을 스킵한다.
                self.safe_orphan_cleanup("orphan_cleanup").await;
                return;
            }
            Err(e) => warn!("{}: DB 포지션 조회 실패: {e}", self.stock_name),
        }

        // DB 조회 실패 시에만 잔고 API 경로로 fallback (balance-first).
        self.safe_orphan_cleanup("restore_fallback").await;
    }

    /// 이 종목의 현재 보유 스냅샷 (qty + avg_opt).
    ///
    /// 2026-04-15 Codex review #3 대응. `safe_orphan_cleanup` / cancel-fill race
    /// 재검증 / 진입 전 스냅샷 — 세 경로 모두 avg 제공 여부에 관계없이 qty>0 만으로
    /// 보유를 판정해야 한다. 모의투자나 특수 상태(avg=0 응답)에서도 live position 을
    /// orphan 으로 오분류하면 안 되기 때문이다.
    ///
    /// 성공:
    /// - `Ok(Some((qty, Some(avg))))`: 보유 + 평균매입가 조회됨
    /// - `Ok(Some((qty, None)))`: 보유 (qty>0) 인데 avg 가 0 이거나 파싱 불가
    /// - `Ok(None)`: 미보유 (응답에 해당 종목 없거나 qty=0)
    ///
    /// 실패 (`Err`): API 장애 — 호출자 fail-safe 처리 필요.
    async fn fetch_holding_snapshot(&self) -> Result<Option<(u64, Option<i64>)>, KisError> {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("AFHR_FLPR_YN", "N"),
            ("OFL_YN", ""),
            ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"),
            ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"),
            ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];
        let resp: KisResponse<Vec<serde_json::Value>> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await?;
        let items = resp.output.or(resp.output1).unwrap_or_default();
        Ok(parse_holding_snapshot(&items, self.stock_code.as_str()))
    }

    /// Balance-first orphan cleanup.
    ///
    /// 실제 잔고를 먼저 확인하여, 정상 포지션이 있으면 절대 미체결 주문을 건드리지 않는다.
    /// 2026-04-15 Codex review #3 대응 — 기존 `cancel_all_pending_orders` 선호출 경로가
    /// DB 메타 부재 상황에서 live position 의 broker-side TP 지정가까지 함께 삭제해버리던
    /// 회귀를 차단한다.
    ///
    /// 분기:
    /// - 잔고 0 주: cancel_all 안전 수행 (기존 orphan_cleanup 동작)
    /// - 잔고 > 0 주: cancel 스킵 + `trigger_manual_intervention(KeepLock)` — 기존 TP 보존
    /// - 조회 실패: cancel 스킵 + `trigger_manual_intervention(KeepLock)` — fail-safe
    ///
    /// `label` — 호출 지점 태그 (`orphan_cleanup` / `restore_fallback`).
    async fn safe_orphan_cleanup(&self, label: &str) {
        match self.fetch_holding_snapshot().await {
            Ok(None) => {
                // 잔고 없음 — orphan 취소 안전
                let cancel_err = self.cancel_all_pending_orders(label).await.err();
                if let Some(ref e) = cancel_err {
                    warn!(
                        "{}: [{}] orphan cancel_all 실패 — 재시작 복구 잠재 위험: {e}",
                        self.stock_name, label
                    );
                }
                // PR #3.d: orphan cleanup 도 execution_journal 에 기록.
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: String::new(),
                        signal_id: String::new(),
                        broker_order_id: String::new(),
                        phase: RestartAction::OrphanCleanup.journal_phase().into(),
                        side: String::new(),
                        intended_price: None,
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: Some(0),
                        holdings_before: Some(0),
                        holdings_after: Some(0),
                        resolution_source: "balance_only".into(),
                        reason: format!("safe_orphan_cleanup:{label}"),
                        error_message: cancel_err
                            .as_ref()
                            .map(|e| e.to_string())
                            .unwrap_or_default(),
                        metadata: serde_json::json!({
                            // 2026-04-19 PR #3.f: DB `active_positions` 레코드 부재 상태를
                            // "absent" 로 명시. taxonomy §1.5 참고.
                            "prior_execution_state": "absent",
                            "manual_category": "orphan_cleanup",
                            "label": label,
                            "cancel_all_success": cancel_err.is_none(),
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
            }
            Ok(Some((qty, avg_opt))) => {
                // 잔고 있음 — 기존 미체결 주문(특히 broker-side TP)을 보존하고
                // 자동 운영을 중단하여 운영자 개입을 유도한다.
                warn!(
                    "{}: [{}] 잔고 {}주 감지 — 기존 미체결 주문 보존 + 수동 개입 전환",
                    self.stock_name, label, qty
                );
                let reason = format!(
                    "재시작 복구: 잔고 {}주{} — DB 메타 부재. KIS 기존 TP 확인 필수.",
                    qty,
                    avg_opt
                        .map(|avg| format!(" @ {}원", avg))
                        .unwrap_or_default(),
                );
                // PR #3.d: orphan + 잔고 detect 도 execution_journal 에 기록.
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: String::new(),
                        signal_id: String::new(),
                        broker_order_id: String::new(),
                        phase: RestartAction::OrphanHoldingDetected {
                            qty,
                            avg_price: avg_opt,
                        }
                        .journal_phase()
                        .into(),
                        side: String::new(),
                        intended_price: None,
                        submitted_price: None,
                        filled_price: avg_opt,
                        filled_qty: Some(qty as i64),
                        holdings_before: Some(qty as i64),
                        holdings_after: Some(qty as i64),
                        resolution_source: "balance_only".into(),
                        reason: format!("safe_orphan_cleanup:{label}"),
                        error_message: String::new(),
                        metadata: serde_json::json!({
                            // 2026-04-19 PR #3.f: DB 레코드 부재를 "absent" 로 명시.
                            "prior_execution_state": "absent",
                            "manual_category": "orphan_holding_detected",
                            "label": label,
                            "quantity": qty,
                            "avg_price": avg_opt,
                            "tp_cancelled": false,
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                let metadata = serde_json::json!({
                    "label": label,
                    "quantity": qty,
                    "avg_price": avg_opt,
                    "tp_cancelled": false,
                    "trigger": "safe_orphan_cleanup_holding_detected",
                });
                self.trigger_manual_intervention(
                    reason,
                    metadata,
                    ManualInterventionMode::KeepLock,
                )
                .await;
            }
            Err(e) => {
                // 상태 불명 — fail-safe: 미체결 주문에 손대지 않음.
                error!(
                    "{}: [{}] balance 조회 실패 — orphan cleanup 스킵, 수동 개입 전환: {e}",
                    self.stock_name, label
                );
                let reason = format!("재시작 복구: balance 조회 실패 ({})", e);
                // PR #3.d: balance 조회 실패도 execution_journal 에 기록.
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: String::new(),
                        signal_id: String::new(),
                        broker_order_id: String::new(),
                        phase: RestartAction::ManualFailSafe {
                            reason: e.to_string(),
                        }
                        .journal_phase()
                        .into(),
                        side: String::new(),
                        intended_price: None,
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: None,
                        holdings_before: None,
                        holdings_after: None,
                        resolution_source: "unresolved".into(),
                        reason: format!("safe_orphan_cleanup:{label}"),
                        error_message: e.to_string(),
                        metadata: serde_json::json!({
                            // 2026-04-19 PR #3.f: DB 레코드 부재 + 잔고 조회 자체 실패 조합.
                            "prior_execution_state": "absent",
                            "manual_category": "balance_lookup_failed",
                            "label": label,
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                let metadata = serde_json::json!({
                    "label": label,
                    "error": e.to_string(),
                    "trigger": "safe_orphan_cleanup_balance_error",
                });
                self.trigger_manual_intervention(
                    reason,
                    metadata,
                    ManualInterventionMode::KeepLock,
                )
                .await;
            }
        }
    }

    /// `manual_intervention_required` 공통 설정 경로.
    ///
    /// state / event_log / stop_flag / stop_notify / shared position_lock 을 한 번에 처리.
    /// `mode=KeepLock` 은 shared `position_lock` 을 `Held(self_code)` 로 고정하여,
    /// 다른 종목 러너가 같은 사이클에서 새 진입을 시도하지 못하게 한다.
    ///
    /// 2026-04-15 Codex review #3/#4 대응. 기존 산재된 manual_intervention 로직
    /// (`restore_position`, `cancel_verify_failed` 분기 등)을 이 헬퍼로 일원화.
    async fn trigger_manual_intervention(
        &self,
        reason: String,
        metadata: serde_json::Value,
        mode: ManualInterventionMode,
    ) {
        error!("{}: 수동 개입 필요 — {}", self.stock_name, reason);
        // 2026-04-17 v3 F3: 정책 일원화. mode 가 KeepLock 이면 position_lock 전달,
        // 아니면 None (현 시점 KeepLock variant 만 존재해 실질 동등).
        let lock = if matches!(mode, ManualInterventionMode::KeepLock) {
            self.position_lock.as_ref()
        } else {
            None
        };
        enter_manual_intervention_state(
            &self.state,
            lock,
            &self.stop_flag,
            &self.stop_notify,
            self.event_logger.as_ref(),
            self.stock_code.as_str(),
            "manual_intervention_required",
            reason,
            metadata,
        )
        .await;

        // 2026-04-17 긴급 follow-up: manual 플래그 DB 영속화. watchdog 재시작 시
        // `check_and_restore_position` 이 이 값을 읽어 state/lock/stop_flag 복원.
        if let Some(ref store) = self.db_store {
            match store
                .set_active_position_manual(self.stock_code.as_str(), true)
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    error!(
                        "{}: manual 플래그 DB 저장 실패 — 재시작 시 보호 우회 위험: {e}",
                        self.stock_name
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            self.stock_code.as_str(),
                            "storage",
                            "manual_flag_persist_failed",
                            "critical",
                            &format!("manual_intervention_required DB 저장 실패: {e}"),
                            serde_json::json!({"error": e.to_string()}),
                        );
                    }
                }
            }
        }

        // 2026-04-19 PR #2.e / 2026-04-20 PR #2.g: armed watcher 즉시 깨움.
        // generation counter 증가 — watch::Receiver::changed() 가 cancel-safe 하게
        // 감지, notified() 재생성 race 없음.
        self.armed_wake_tx.send_modify(|g| *g = g.wrapping_add(1));
    }

    /// 2026-04-17 v3 F2: manual_intervention 스킵 진입 시 active_position 동적 필드를
    /// 명시적 동기 저장한다.
    ///
    /// 트레일링 갱신은 평소 `update_position_trailing` 을 fire-and-forget spawn 으로
    /// 호출(`live_runner.rs:2154`)하므로 manual 시점에 그 spawn 이 미완료/실패였을
    /// 가능성이 있다. 이 helper 는 manual 분기에서 명시적 await 로 한 번 더 저장하여
    /// 재시작 복구(`check_and_restore_position`) 와 운영자 수동 처리 시 최신 SL/best/
    /// reached_1r 가 보장되도록 한다.
    ///
    /// 동작:
    /// - db_store / current_position 둘 중 하나라도 없으면 no-op
    /// - 기존 `update_position_trailing(stock_code, sl, r1r, best)` 메서드 재사용
    ///   (postgres_store.rs:813)
    /// - 실패 시 `active_position_persist_failed` critical 이벤트만 기록.
    ///   자동 청산 차단 정책은 그대로 유지 (저장 실패가 청산 실행 트리거가 되면 안 됨)
    async fn persist_current_position_for_manual_recovery(&self) {
        let Some(ref store) = self.db_store else {
            return;
        };
        let snapshot = {
            let state = self.state.read().await;
            state
                .current_position
                .as_ref()
                .map(|p| (p.stop_loss, p.reached_1r, p.best_price))
        };
        let Some((sl, r1r, bp)) = snapshot else {
            return;
        };

        match store
            .update_position_trailing(self.stock_code.as_str(), sl, r1r, bp)
            .await
        {
            Ok(rows) if rows > 0 => info!(
                "{}: manual 스킵 — active_position 동적 필드 재저장 (sl={}, r1r={}, best={})",
                self.stock_name, sl, r1r, bp
            ),
            Ok(_) => {
                // 2026-04-17 v3 fixups E: SQL 자체는 성공했지만 행이 없음.
                // entry 시점 save_active_position 실패 또는 외부 삭제로 active_positions
                // 행 자체가 부재. F2 가 의도한 "동적 필드 보존" 자체가 불가능.
                error!(
                    "{}: manual 스킵 — active_position 행 자체 없음 (rows=0) — 재시작 복구 불가",
                    self.stock_name
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "storage",
                        "active_position_missing",
                        "critical",
                        "manual 스킵 시 active_positions 행 부재 — 재시작 복구 불가",
                        serde_json::json!({
                            "stop_loss": sl,
                            "reached_1r": r1r,
                            "best_price": bp,
                        }),
                    );
                }
            }
            Err(e) => {
                error!(
                    "{}: manual 스킵 — active_position 재저장 실패: {e}",
                    self.stock_name
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "storage",
                        "active_position_persist_failed",
                        "critical",
                        &format!("manual_intervention 스킵에서 active_position 재저장 실패: {e}"),
                        serde_json::json!({
                            "stop_loss": sl,
                            "reached_1r": r1r,
                            "best_price": bp,
                            "error": e.to_string(),
                        }),
                    );
                }
            }
        }
    }

    /// 외부 중지 알림 핸들 (StrategyManager에서 사용)
    pub fn stop_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.stop_notify)
    }

    pub fn with_trade_tx(mut self, tx: broadcast::Sender<RealtimeData>) -> Self {
        // 실시간 수신용 receiver도 생성
        self.realtime_rx = Some(tx.subscribe());
        self.trade_tx = Some(tx);
        self
    }

    /// 주문 체결 알림 전송
    fn notify_trade(&self, action: &str) {
        if let Some(ref tx) = self.trade_tx {
            let _ = tx.send(RealtimeData::TradeNotification(TradeNotification {
                stock_code: self.stock_code.as_str().to_string(),
                stock_name: self.stock_name.clone(),
                action: action.to_string(),
            }));
        }
    }

    /// WebSocket 실시간 가격 + 장운영정보 수신 태스크 시작
    ///
    /// 틱마다 수행:
    /// 1. 최신 가격 캐시 갱신 (`latest_price`)
    /// 2. 포지션 보유 중이면 백테스트 `simulate_exit` Step 2+3 재현:
    ///    - best_price 갱신
    ///    - 1R 본전 스탑 활성화
    ///    - trailing SL 갱신
    ///    - 갱신된 SL 기준 돌파 감지 → `sl_triggered_tick` 플래그 + notify
    /// 3. 장중단(VI/거래정지) 상태 전환 감지
    ///
    /// SL 감지는 manage_position 3초 루프를 우회하여 틱 해상도로 수행.
    /// TP는 거래소(KRX)가 지정가 선발주로 직접 체결하므로 여기서 다루지 않음.
    fn spawn_price_listener(&mut self) {
        if let Some(mut rx) = self.realtime_rx.take() {
            let code = self.stock_code.as_str().to_string();
            let stock_name = self.stock_name.clone();
            let latest = Arc::clone(&self.latest_price);
            let latest_quote = Arc::clone(&self.latest_quote);
            let state = Arc::clone(&self.state);
            let notify = Arc::clone(&self.stop_notify);
            let event_logger = self.event_logger.clone();
            let breakeven_r = self.strategy.config.breakeven_r;
            let trailing_r = self.strategy.config.trailing_r;

            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(RealtimeData::MarketOperation(op)) if op.stock_code == code => {
                            let halted = op.is_trading_halt || op.vi_applied != "0";
                            let mut s = state.write().await;
                            if halted != s.market_halted {
                                if halted {
                                    info!(
                                        "{}: 장 중단 감지 (VI={}, 거래정지={})",
                                        code, op.vi_applied, op.is_trading_halt
                                    );
                                } else {
                                    info!("{}: 장 정상화", code);
                                }
                                s.market_halted = halted;
                            }
                        }
                        Ok(RealtimeData::Execution(exec)) if exec.stock_code == code => {
                            let now = tokio::time::Instant::now();
                            *latest.write().await = Some((exec.price, now));
                            if exec.ask_price != 0 || exec.bid_price != 0 {
                                apply_quote_update(
                                    &latest_quote,
                                    &state,
                                    event_logger.as_ref(),
                                    &code,
                                    "execution",
                                    exec.ask_price,
                                    exec.bid_price,
                                    Some(exec.price),
                                    now,
                                )
                                .await;
                            }

                            // 백테스트 simulate_exit Step 2+3 틱 재현
                            let wake = {
                                let mut s = state.write().await;
                                if let Some(ref mut pos) = s.current_position {
                                    // Step 2-a: best_price 갱신
                                    pos.best_price = match pos.side {
                                        PositionSide::Long => pos.best_price.max(exec.price),
                                        PositionSide::Short => pos.best_price.min(exec.price),
                                    };

                                    // Step 2-b: 1R 본전 스탑 활성화
                                    let risk = (pos.entry_price - pos.original_sl).abs() as f64;
                                    let breakeven_dist = (risk * breakeven_r) as i64;
                                    let trailing_dist = (risk * trailing_r) as i64;

                                    let profit = match pos.side {
                                        PositionSide::Long => pos.best_price - pos.entry_price,
                                        PositionSide::Short => pos.entry_price - pos.best_price,
                                    };
                                    if !pos.reached_1r && profit >= breakeven_dist {
                                        pos.reached_1r = true;
                                        pos.stop_loss = pos.entry_price;
                                        info!(
                                            "{}: 1R 도달 (틱) — 본전 스탑 활성화 (SL → {})",
                                            stock_name, pos.stop_loss
                                        );
                                    }

                                    // Step 2-c: trailing SL 갱신 (best_price 기준)
                                    if pos.reached_1r {
                                        let new_sl = match pos.side {
                                            PositionSide::Long => pos.best_price - trailing_dist,
                                            PositionSide::Short => pos.best_price + trailing_dist,
                                        };
                                        let improved = match pos.side {
                                            PositionSide::Long => new_sl > pos.stop_loss,
                                            PositionSide::Short => new_sl < pos.stop_loss,
                                        };
                                        if improved {
                                            pos.stop_loss = new_sl;
                                        }
                                    }

                                    // Step 3: SL 돌파 감지 (갱신된 stop_loss 기준)
                                    let sl_hit = match pos.side {
                                        PositionSide::Long => exec.price <= pos.stop_loss,
                                        PositionSide::Short => exec.price >= pos.stop_loss,
                                    };
                                    if sl_hit && !pos.sl_triggered_tick {
                                        pos.sl_triggered_tick = true;
                                        info!(
                                            "{}: SL 틱 감지 — price={}, SL={}",
                                            stock_name, exec.price, pos.stop_loss
                                        );
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            };

                            if wake {
                                // manage_position sleep 즉시 깨움
                                notify.notify_waiters();
                            }
                        }
                        Ok(RealtimeData::OrderBook(book)) if book.stock_code == code => {
                            let best_ask = book.asks.first().map(|(price, _)| *price).unwrap_or(0);
                            let best_bid = book.bids.first().map(|(price, _)| *price).unwrap_or(0);
                            if best_ask != 0 || best_bid != 0 {
                                let current_price =
                                    latest.read().await.as_ref().map(|(price, _)| *price);
                                apply_quote_update(
                                    &latest_quote,
                                    &state,
                                    event_logger.as_ref(),
                                    &code,
                                    "orderbook",
                                    best_ask,
                                    best_bid,
                                    current_price,
                                    tokio::time::Instant::now(),
                                )
                                .await;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        _ => {} // 다른 종목이나 다른 타입은 무시
                    }
                }
            });
        }
    }

    /// 현재가 조회 — WebSocket 우선 (30초 이내), fallback으로 REST
    async fn get_current_price(&self) -> Result<i64, KisError> {
        if let Some((price, updated_at)) = *self.latest_price.read().await {
            if updated_at.elapsed() < std::time::Duration::from_secs(30) {
                return Ok(price);
            }
            let age_secs = updated_at.elapsed().as_secs();
            warn!(
                "{}: 실시간 가격 {}초 미갱신 — REST 폴백",
                self.stock_name, age_secs
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "system",
                    "ws_tick_gap",
                    "warn",
                    &format!("틱 {}초 미수신 — REST 폴백", age_secs),
                    serde_json::json!({"last_tick_age_secs": age_secs}),
                );
            }
        }
        // fallback: REST API (장외 시간, WS 끊김, 30초 이상 미갱신 시)
        let resp = self.fetch_current_price_rest().await?;
        Ok(resp.stck_prpr)
    }

    async fn latest_quote_snapshot(&self) -> Option<(i64, i64)> {
        let snapshot = *self.latest_quote.read().await;
        snapshot.and_then(|(ask, bid, updated_at)| {
            if updated_at.elapsed() < std::time::Duration::from_secs(30)
                && ask > 0
                && bid > 0
                && ask >= bid
            {
                Some((ask, bid))
            } else {
                None
            }
        })
    }

    async fn best_quote_for_side(&self, side: PositionSide) -> Option<i64> {
        self.latest_quote_snapshot()
            .await
            .map(|(ask, bid)| match side {
                PositionSide::Long => ask,
                PositionSide::Short => bid,
            })
    }

    /// 2026-04-19 PR #2.a/#2.b: armed watch 종료 처리 중앙화.
    ///
    /// 모든 exit 경로가 이 helper 를 통과하도록 하여 다음을 통일:
    /// - `RunnerState.armed_stats` 카운터/duration 기록
    /// - `current_armed_signal_id` 초기화
    /// - `event_log` 에 `armed_watch_exit` 기록 (outcome label + detail + duration)
    async fn record_armed_exit(
        &self,
        outcome: ArmedWaitOutcome,
        duration_ms: u64,
        signal_id: &str,
        signal_entry_price: i64,
    ) -> ArmedWaitOutcome {
        // AlreadyArmed 는 현재 armed 중인 다른 호출이 signal_id 를 소유하고 있으므로
        // clear 하면 안 된다. 그 외 경로에서는 armed watcher 를 종료하므로 clear.
        {
            let mut state = self.state.write().await;
            state.armed_stats.record(&outcome, duration_ms);
            if !matches!(outcome, ArmedWaitOutcome::AlreadyArmed) {
                state.current_armed_signal_id = None;
            }
        }
        if let Some(ref logger) = self.event_logger {
            let detail = outcome.detail().unwrap_or("");
            let message = if detail.is_empty() {
                format!(
                    "armed_watch_exit: {} (duration={}ms)",
                    outcome.label(),
                    duration_ms
                )
            } else {
                format!(
                    "armed_watch_exit: {} [{}] (duration={}ms)",
                    outcome.label(),
                    detail,
                    duration_ms
                )
            };
            logger.log_event(
                self.stock_code.as_str(),
                armed_tx::EVENT_LOG_CATEGORY,
                armed_tx::EVENT_LOG_TYPE_EXIT,
                outcome.severity(),
                &message,
                serde_json::json!({
                    "outcome": outcome.label(),
                    "detail": detail,
                    "duration_ms": duration_ms,
                    "signal_id": signal_id,
                    "signal_entry_price": signal_entry_price,
                }),
            );
        }
        // 2026-04-19 PR #2.e: execution_journal 에도 armed_wait_exit phase 기록.
        // PR #2.f: phase 는 armed_taxonomy 상수 참조.
        if let Some(ref store) = self.db_store {
            let detail = outcome.detail().unwrap_or("");
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: signal_id.to_string(),
                signal_id: signal_id.to_string(),
                broker_order_id: String::new(),
                phase: armed_tx::JOURNAL_PHASE_EXIT.into(),
                side: String::new(),
                intended_price: Some(signal_entry_price),
                submitted_price: None,
                filled_price: None,
                filled_qty: None,
                holdings_before: None,
                holdings_after: None,
                resolution_source: outcome.label().into(),
                reason: if detail.is_empty() {
                    outcome.label().to_string()
                } else {
                    format!("{}:{}", outcome.label(), detail)
                },
                error_message: String::new(),
                metadata: serde_json::json!({
                    "outcome": outcome.label(),
                    "detail": detail,
                    "severity": outcome.severity(),
                    "duration_ms": duration_ms,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: None,
                final_fill_at: Some(Utc::now()),
            };
            let _ = store.append_execution_journal(&entry).await;
        }
        outcome
    }

    /// 2026-04-19 Doc review P1 (Phase 4 armed watch) + PR #2.a~#2.d:
    /// armed 상태에서 진입 직전 재확인 루프.
    ///
    /// PR #2 보강 요약:
    /// - 진입/해제 이벤트 + duration 을 `event_log` 와 `RunnerState.armed_stats` 에 남김.
    /// - 같은 signal 에 대한 armed 루프 중복 생성을 `current_armed_signal_id` 로 차단.
    /// - `manual_intervention_required` / `degraded` 전이를 tick 단위(최대 `armed_tick_check_interval_ms`)
    ///   내에 감지해 `Aborted` 로 즉시 종료.
    async fn armed_wait_for_entry(
        &self,
        sig: &BestSignal,
        current_price_at_signal: Option<i64>,
    ) -> ArmedWaitOutcome {
        let budget_ms = self.live_cfg.armed_watch_budget_ms;
        if budget_ms == 0 {
            // 비활성 — 기존 즉시 진입 동작 보존. stats/event_log 미기록.
            return ArmedWaitOutcome::Ready;
        }

        let signal_id = self.signal_execution_id(sig.signal_time, sig.entry_price, self.quantity);

        // PR #2.c: 중복 armed 루프 방지. 이미 이 signal_id 로 armed 감시 중이면 skip.
        {
            let mut state = self.state.write().await;
            if state.current_armed_signal_id.as_deref() == Some(signal_id.as_str()) {
                drop(state);
                return self
                    .record_armed_exit(
                        ArmedWaitOutcome::AlreadyArmed,
                        0,
                        &signal_id,
                        sig.entry_price,
                    )
                    .await;
            }
            state.current_armed_signal_id = Some(signal_id.clone());
        }

        if let Some(ref logger) = self.event_logger {
            logger.log_event(
                self.stock_code.as_str(),
                armed_tx::EVENT_LOG_CATEGORY,
                armed_tx::EVENT_LOG_TYPE_ENTER,
                "info",
                &format!("armed_watch_enter (budget={}ms)", budget_ms),
                serde_json::json!({
                    "signal_id": signal_id,
                    "budget_ms": budget_ms,
                    "signal_entry_price": sig.entry_price,
                    "current_at_signal": current_price_at_signal,
                }),
            );
        }
        // 2026-04-19 PR #2.e: execution_journal 에 armed_wait_enter phase 기록.
        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: signal_id.clone(),
                signal_id: signal_id.clone(),
                broker_order_id: String::new(),
                phase: armed_tx::JOURNAL_PHASE_ENTER.into(),
                side: String::new(),
                intended_price: Some(sig.entry_price),
                submitted_price: None,
                filled_price: None,
                filled_qty: None,
                holdings_before: None,
                holdings_after: None,
                resolution_source: String::new(),
                reason: "armed_watch_begin".into(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "budget_ms": budget_ms,
                    "tick_interval_ms": self.live_cfg.armed_tick_check_interval_ms,
                    "signal_entry_price": sig.entry_price,
                    "current_at_signal": current_price_at_signal,
                }),
                sent_at: Some(Utc::now()),
                ack_at: None,
                first_fill_at: None,
                final_fill_at: None,
            };
            let _ = store.append_execution_journal(&entry).await;
        }

        let watch_budget = std::time::Duration::from_millis(budget_ms.clamp(50, 5_000));
        let entry_submit_timeout = self
            .execution_timeout_for_state(ExecutionState::SignalArmed)
            .unwrap_or((
                TimeoutPhase::EntrySubmit,
                TimeoutPolicy::default().entry_submit,
            ));
        let tick_interval = std::time::Duration::from_millis(
            self.live_cfg.armed_tick_check_interval_ms.clamp(50, 1_000),
        );
        let watch_start = tokio::time::Instant::now();
        let entry_deadline = self
            .live_cfg
            .effective_entry_cutoff(self.strategy.config.entry_cutoff);

        // trade_tx 가 있으면 subscribe — tick 수신 시 즉시 loop 재진입.
        let mut rx = self.trade_tx.as_ref().map(|tx| tx.subscribe());

        // 2026-04-20 PR #2.g: race-free wake-up via watch channel.
        //
        // 이전 구현 (PR #2.f) 은 `Notify::notify_waiters()` + `tokio::pin!` / `enable()`
        // / `set()` 으로 closed 창을 좁혔지만, `set()` 직후 ~ 다음 `enable()` 이전의
        // 짧은 미등록 창에 오는 notify 는 여전히 유실될 수 있었다. 유실되면 다음
        // state check 로 간접 감지되지만 "tick 대기 없는 즉시 종료" 를 100% 보장
        // 하지는 못했다.
        //
        // `watch::Receiver::changed()` 는 "마지막으로 본 값 이후 값이 바뀌었으면
        // 즉시 반환" 의미론이라 미등록 창이 존재하지 않는다. Sender 가 `send_modify`
        // 로 gen 을 올리는 순간 그 값은 영속적으로 보관되며, Receiver 가 나중에
        // `changed()` 를 call 해도 변경이 사라지지 않는다 (cancel-safe).
        //
        // 초기 `mark_unchanged()` 는 subscribe 직후 바로 `changed()` 가 ready 상태가
        // 되는 것을 방지하기 위함 (현재 값을 기준점으로 삼는다).
        let mut armed_wake_rx = self.armed_wake_tx.subscribe();
        armed_wake_rx.mark_unchanged();

        loop {
            if self.is_stopped() {
                let d = watch_start.elapsed().as_millis() as u64;
                return self
                    .record_armed_exit(ArmedWaitOutcome::Stopped, d, &signal_id, sig.entry_price)
                    .await;
            }

            // PR #2.d: manual_intervention / degraded 전이 즉시 감지.
            // PR #2.f: wake-up race-free 패턴 덕에 tick 주기 없이도 즉시 감지.
            {
                let s = self.state.read().await;
                if s.manual_intervention_required {
                    drop(s);
                    let d = watch_start.elapsed().as_millis() as u64;
                    return self
                        .record_armed_exit(
                            ArmedWaitOutcome::Aborted(armed_tx::DETAIL_ABORTED_MANUAL_INTERVENTION),
                            d,
                            &signal_id,
                            sig.entry_price,
                        )
                        .await;
                }
                if s.degraded {
                    drop(s);
                    let d = watch_start.elapsed().as_millis() as u64;
                    return self
                        .record_armed_exit(
                            ArmedWaitOutcome::Aborted(armed_tx::DETAIL_ABORTED_DEGRADED),
                            d,
                            &signal_id,
                            sig.entry_price,
                        )
                        .await;
                }
            }

            if watch_start.elapsed() >= entry_submit_timeout.1 {
                let d = watch_start.elapsed().as_millis() as u64;
                self.handle_execution_timeout_expired(
                    entry_submit_timeout.0,
                    "armed_wait_entry_submit_timeout",
                )
                .await;
                return self
                    .record_armed_exit(
                        ArmedWaitOutcome::Aborted(armed_tx::DETAIL_ABORTED_ENTRY_SUBMIT_TIMEOUT),
                        d,
                        &signal_id,
                        sig.entry_price,
                    )
                    .await;
            }

            if Local::now().time() >= entry_deadline {
                let d = watch_start.elapsed().as_millis() as u64;
                return self
                    .record_armed_exit(
                        ArmedWaitOutcome::CutoffReached,
                        d,
                        &signal_id,
                        sig.entry_price,
                    )
                    .await;
            }

            // preflight 재확인 — signal 확정 후 gate 가 깨졌는지
            let preflight = self.refresh_preflight_metadata().await;
            if !preflight.all_ok() {
                let reason: &'static str = if !preflight.subscription_health_ok {
                    armed_tx::DETAIL_PREFLIGHT_SUBSCRIPTION_STALE
                } else if !preflight.spread_ok {
                    armed_tx::DETAIL_PREFLIGHT_SPREAD_EXCEEDED
                } else if !preflight.nav_gate_ok {
                    armed_tx::DETAIL_PREFLIGHT_NAV_FAILED
                } else if !preflight.regime_confirmed {
                    armed_tx::DETAIL_PREFLIGHT_REGIME_UNCONFIRMED
                } else {
                    armed_tx::DETAIL_PREFLIGHT_GATE
                };
                let d = watch_start.elapsed().as_millis() as u64;
                return self
                    .record_armed_exit(
                        ArmedWaitOutcome::PreflightFailed(reason),
                        d,
                        &signal_id,
                        sig.entry_price,
                    )
                    .await;
            }

            // drift 재확인 (drift 가드 활성 시만)
            if let Some(drift_limit) = self.live_cfg.max_entry_drift_pct
                && let Ok(current) = self.get_current_price().await
            {
                let base = sig.entry_price.max(1) as f64;
                let drift = (current - sig.entry_price).abs() as f64 / base;
                if drift > drift_limit {
                    let d = watch_start.elapsed().as_millis() as u64;
                    return self
                        .record_armed_exit(
                            ArmedWaitOutcome::DriftExceeded,
                            d,
                            &signal_id,
                            sig.entry_price,
                        )
                        .await;
                }
            }

            // budget 소진 시 ready
            let elapsed = watch_start.elapsed();
            if elapsed >= watch_budget {
                let d = elapsed.as_millis() as u64;
                return self
                    .record_armed_exit(ArmedWaitOutcome::Ready, d, &signal_id, sig.entry_price)
                    .await;
            }

            let remaining = watch_budget.saturating_sub(elapsed);
            let submit_remaining = entry_submit_timeout.1.saturating_sub(elapsed);
            let wait_for = tick_interval.min(remaining).min(submit_remaining);

            // 2026-04-20 PR #2.g: race-free wake-up.
            // `armed_wake_rx.changed()` 는 cancel-safe 이고 missed update 를 놓치지
            // 않는다. branch 가 선택되면 `mark_unchanged()` 로 "이후 변경만" 추적.
            // 선택되지 않고 sleep/stop 이 이긴 경우에도 generation 값은 그대로
            // 남아 있어 다음 iteration 의 `changed()` 에서 즉시 반환된다.
            if let Some(ref mut r) = rx {
                tokio::select! {
                    _ = r.recv() => {}
                    _ = tokio::time::sleep(wait_for) => {}
                    _ = self.stop_notify.notified() => {
                        let d = watch_start.elapsed().as_millis() as u64;
                        return self
                            .record_armed_exit(
                                ArmedWaitOutcome::Stopped,
                                d,
                                &signal_id,
                                sig.entry_price,
                            )
                            .await;
                    }
                    res = armed_wake_rx.changed() => {
                        // Sender 가 drop 됐을 수도 있으나 우리는 LiveRunner 가
                        // Arc 로 보유하므로 실무적으로는 Ok. Err 여도 loop 로 진행
                        // → state check 가 다음 단계에서 최종 판단.
                        let _ = res;
                        armed_wake_rx.mark_unchanged();
                    }
                }
            } else {
                tokio::select! {
                    _ = tokio::time::sleep(wait_for) => {}
                    _ = self.stop_notify.notified() => {
                        let d = watch_start.elapsed().as_millis() as u64;
                        return self
                            .record_armed_exit(
                                ArmedWaitOutcome::Stopped,
                                d,
                                &signal_id,
                                sig.entry_price,
                            )
                            .await;
                    }
                    res = armed_wake_rx.changed() => {
                        let _ = res;
                        armed_wake_rx.mark_unchanged();
                    }
                }
            }
        }
    }

    /// 2026-04-19 Doc revision P2: Degraded 실 전이.
    ///
    /// preflight gate 실패(subscription stale / spread 급확대 / NAV gate 실패 / regime 미확정) 시 호출.
    /// manual_intervention 이 우선이므로 manual 활성일 때는 no-op.
    async fn transition_to_degraded(&self, reason: String) {
        let (prev_degraded, prev_exec, is_manual) = {
            let state = self.state.read().await;
            (
                state.degraded,
                state.execution_state,
                state.manual_intervention_required,
            )
        };
        if is_manual {
            return;
        }
        {
            let mut state = self.state.write().await;
            state.degraded = true;
            state.degraded_reason = Some(reason.clone());
        }
        // 상태 진행 중 (EntryPending / ExitPending / ExitPartial) 은 덮어쓰지 않는다 —
        // pending 중 Degraded 로 전이하면 후속 검증 경로가 혼동된다.
        let degrade_ok = matches!(
            prev_exec,
            ExecutionState::Flat | ExecutionState::SignalArmed | ExecutionState::Open
        );
        if degrade_ok {
            self.transition_execution_state(ExecutionState::Degraded, &reason)
                .await;
        }
        if !prev_degraded {
            info!("{}: Degraded 진입 — {}", self.stock_name, reason);
        }
        // 2026-04-19 PR #2.e / 2026-04-20 PR #2.g: armed watcher 즉시 깨움.
        // watch::Sender 의 generation counter 를 올리면 Receiver::changed() 가
        // race-free 로 감지 (미등록 창에 호출돼도 값 변경은 보존됨).
        self.armed_wake_tx.send_modify(|g| *g = g.wrapping_add(1));
    }

    /// 2026-04-19 Doc revision P2: preflight 가 다시 healthy 해졌을 때 Degraded 해제.
    ///
    /// current_position 유무에 따라 Open 또는 Flat 으로 복원. manual 이면 no-op.
    async fn clear_degraded_if_healthy(&self) {
        let (was_degraded, has_position, is_manual, current_exec) = {
            let state = self.state.read().await;
            (
                state.degraded,
                state.current_position.is_some(),
                state.manual_intervention_required,
                state.execution_state,
            )
        };
        if is_manual || !was_degraded {
            return;
        }
        {
            let mut state = self.state.write().await;
            state.degraded = false;
            state.degraded_reason = None;
        }
        if current_exec == ExecutionState::Degraded {
            // Degraded 에서 상태 복원: 포지션 있으면 Open, 없으면 Flat.
            let target = if has_position {
                ExecutionState::Open
            } else {
                ExecutionState::Flat
            };
            self.transition_execution_state(target, "degraded_cleared")
                .await;
        }
        info!("{}: Degraded 해제 — preflight 정상화", self.stock_name);
    }

    async fn refresh_preflight_metadata(&self) -> PreflightMetadata {
        let entry_cutoff = self
            .live_cfg
            .effective_entry_cutoff(self.strategy.config.entry_cutoff);
        let internal_subscription_health_ok =
            self.latest_price
                .read()
                .await
                .as_ref()
                .is_some_and(|(_, updated_at)| {
                    updated_at.elapsed() < std::time::Duration::from_secs(30)
                });
        let quote_issue = self.state.read().await.last_quote_sanity_issue.clone();
        let spread_eval = evaluate_spread_gate(
            self.latest_quote_snapshot().await,
            self.live_cfg.real_mode,
            DEFAULT_SPREAD_THRESHOLD_TICKS,
        );
        let internal_spread_ok = spread_eval.spread_ok && quote_issue.is_none();

        // 2026-04-19 PR #1.b: external_gates 가 주입되어 있으면 Signal Engine 의 값 사용.
        // 정책 및 합성은 `apply_external_gates` 순수 함수가 담당 (unit test 가능).
        // 여기서는 스냅샷을 뽑고 호출 후 stale 감지 시 warn 로그만 남긴다.
        // PR #1.d: stale flip(false↔true) 에서 event_log 에도 기록 (폭주 방지).
        let (
            nav_gate_ok,
            regime_confirmed,
            active_instrument,
            subscription_health_ok,
            spread_ok,
            stale_detected,
            stale_age_ms,
        ) = match &self.external_gates {
            Some(gates) => {
                let snapshot = gates.read().await.clone();
                let now = Utc::now();
                let app = apply_external_gates(
                    &snapshot,
                    now,
                    self.live_cfg.external_gate_max_age_ms,
                    self.live_cfg.real_mode,
                    self.stock_code.as_str(),
                    internal_subscription_health_ok,
                    internal_spread_ok,
                );
                let age_ms = snapshot
                    .updated_at
                    .map(|t| now.signed_duration_since(t).num_milliseconds())
                    .unwrap_or(0);
                if app.stale_detected {
                    if self.live_cfg.real_mode {
                        warn!(
                            "{}: external gate stale (age={}ms > {}ms) — real mode fail-close",
                            self.stock_name, age_ms, self.live_cfg.external_gate_max_age_ms
                        );
                    } else {
                        warn!(
                            "{}: external gate stale (age={}ms > {}ms) — paper mode 경고만",
                            self.stock_name, age_ms, self.live_cfg.external_gate_max_age_ms
                        );
                    }
                }
                (
                    app.nav_gate_ok,
                    app.regime_confirmed,
                    app.active_instrument,
                    app.subscription_health_ok,
                    app.spread_ok,
                    app.stale_detected,
                    age_ms,
                )
            }
            None => (
                true,
                true,
                Some(self.stock_code.as_str().to_string()),
                internal_subscription_health_ok,
                internal_spread_ok,
                false,
                0,
            ),
        };

        let preflight = PreflightMetadata {
            entry_cutoff: Some(entry_cutoff),
            spread_ok,
            spread_ask: spread_eval.spread_ask,
            spread_bid: spread_eval.spread_bid,
            spread_mid: spread_eval.spread_mid,
            spread_raw: spread_eval.spread_raw,
            spread_tick: spread_eval.spread_tick,
            spread_threshold_ticks: spread_eval.spread_threshold_ticks,
            nav_gate_ok,
            regime_confirmed,
            active_instrument,
            subscription_health_ok,
            quote_issue,
        };

        let prev_stale = {
            let mut state = self.state.write().await;
            let prev = state.last_external_gate_stale;
            state.preflight_metadata = preflight.clone();
            state.last_external_gate_stale = stale_detected;
            prev
        };

        // PR #1.d: flip 감지 시에만 event_log 기록. 같은 stale 상태 반복 시 로깅하지 않음.
        if stale_detected != prev_stale
            && let Some(logger) = &self.event_logger
        {
            let (event_type, severity, message) = if stale_detected {
                let sev = if self.live_cfg.real_mode {
                    "warning"
                } else {
                    "info"
                };
                (
                    "gate_stale_detected",
                    sev,
                    "external gate stale 감지 — 새로 발생",
                )
            } else {
                (
                    "gate_stale_recovered",
                    "info",
                    "external gate 정상화 — stale 해제",
                )
            };
            logger.log_event(
                self.stock_code.as_str(),
                "external_gate",
                event_type,
                severity,
                message,
                serde_json::json!({
                    "real_mode": self.live_cfg.real_mode,
                    "max_age_ms": self.live_cfg.external_gate_max_age_ms,
                    "age_ms": stale_age_ms,
                }),
            );
        }

        preflight
    }

    async fn refresh_or_stages_from_canonical(
        &self,
        canonical_candles: &[MinuteCandle],
        now: NaiveTime,
        today: NaiveDate,
    ) -> Vec<AvailableOrStage> {
        let cfg = &self.strategy.config;
        let or_stages_def = [
            (
                "5m",
                cfg.or_start,
                NaiveTime::from_hms_opt(9, 5, 0).unwrap(),
            ),
            (
                "15m",
                cfg.or_start,
                NaiveTime::from_hms_opt(9, 15, 0).unwrap(),
            ),
            (
                "30m",
                cfg.or_start,
                NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            ),
        ];

        let mut available_stages: Vec<AvailableOrStage> = Vec::new();
        for (stage_name, or_start, or_end) in &or_stages_def {
            if now < *or_end {
                continue;
            }

            let or_candles: Vec<_> = canonical_candles
                .iter()
                .filter(|c| c.time >= *or_start && c.time < *or_end)
                .collect();

            let (h, l) = if !or_candles.is_empty() {
                let h = or_candles.iter().map(|c| c.high).max().unwrap();
                let l = or_candles.iter().map(|c| c.low).min().unwrap();
                if let Some(ref store) = self.db_store
                    && let Err(e) = store
                        .save_or_range_stage(
                            self.stock_code.as_str(),
                            today,
                            h,
                            l,
                            "candle",
                            stage_name,
                        )
                        .await
                {
                    warn!(
                        "{}: OR DB 저장 실패 (stage={}): {e}",
                        self.stock_name, stage_name
                    );
                }
                (h, l)
            } else if let Some(ref store) = self.db_store {
                if let Ok(Some((h, l))) = store
                    .get_or_range_stage(self.stock_code.as_str(), today, stage_name)
                    .await
                {
                    (h, l)
                } else {
                    debug!(
                        "{}: OR {} 캔들 부족+DB 미저장 — 단계 스킵",
                        self.stock_name, stage_name
                    );
                    continue;
                }
            } else {
                continue;
            };

            available_stages.push((*stage_name, h, l, *or_end));
        }

        available_stages
    }

    async fn update_or_state_from_stages(&self, available_stages: &[AvailableOrStage]) {
        let mut state = self.state.write().await;
        if available_stages.is_empty() {
            state.or_high = None;
            state.or_low = None;
            state.or_stages.clear();
            return;
        }

        let allowed_primary = self
            .live_cfg
            .first_allowed_stage_name(available_stages.iter().map(|(n, _, _, _)| *n))
            .and_then(|name| {
                available_stages
                    .iter()
                    .find(|(n, _, _, _)| *n == name)
                    .map(|(_, h, l, _)| (*h, *l))
            });
        state.or_high = allowed_primary.map(|(h, _)| h);
        state.or_low = allowed_primary.map(|(_, l)| l);
        state.or_stages = available_stages
            .iter()
            .map(|(name, h, l, _)| (name.to_string(), *h, *l))
            .collect();
    }

    fn signal_execution_id(
        &self,
        entry_time: NaiveTime,
        entry_price: i64,
        quantity: u64,
    ) -> String {
        format!(
            "{}:{}:{}:{}",
            self.stock_code.as_str(),
            entry_time.format("%H%M%S"),
            entry_price,
            quantity
        )
    }

    fn position_execution_id(&self, pos: &Position) -> String {
        self.signal_execution_id(pos.entry_time, pos.entry_price, pos.quantity)
    }

    /// 장중 실행 루프 (토글 OFF 시 중단 가능)
    pub async fn run(&mut self) -> Result<Vec<TradeResult>, KisError> {
        // WebSocket 가격 수신 태스크 시작 (self 빌림 해제 전에 호출)
        self.spawn_price_listener();

        let cfg = &self.strategy.config;

        info!(
            "=== {} ({}) 자동매매 시작 ===",
            self.stock_name, self.stock_code
        );
        info!(
            "수량: {}주, RR: 1:{:.1}, 트레일링: {:.1}R, 본전: {:.1}R",
            self.quantity, cfg.rr_ratio, cfg.trailing_r, cfg.breakeven_r
        );

        // 잔존 포지션 확인
        self.check_and_restore_position().await;

        // 장 시작 대기
        self.update_phase("장 시작 대기").await;
        self.wait_until_session_milestone(cfg.or_start, cfg.force_exit)
            .await;
        if self.is_stopped() {
            return Ok(Vec::new());
        }

        // 잔존 포지션이 있으면 장 시작 즉시 시장가 청산 (전일 잔여분 정리)
        // 단, "장 시작 직후"(or_start ~ or_start+5분)일 때만 동작.
        // 장중 재시작 시(예: 13:39 핫픽스 후 재시작) 오늘 들어간 정상 포지션을
        // 잘못 청산하지 않도록 시간 게이팅.
        let now_time = Local::now().time();
        let elapsed_secs_since_open = (now_time - cfg.or_start).num_seconds();
        let is_market_open_window = (0..=300).contains(&elapsed_secs_since_open);
        let has_leftover = self.state.read().await.current_position.is_some();
        if has_leftover && !is_market_open_window {
            info!(
                "{}: 잔존 포지션 발견했지만 장 시작 직후가 아님(elapsed={}s) — 정상 운용 진행",
                self.stock_name, elapsed_secs_since_open
            );
        }
        if has_leftover && is_market_open_window {
            info!(
                "{}: 전일 잔여 포지션 — 장 시작 시장가 청산",
                self.stock_name
            );
            self.update_phase("잔여 포지션 청산").await;
            // 장 시작 직후 체결 안정화 대기 (5초)
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            match self.close_position_market(ExitReason::EndOfDay).await {
                Ok((result, meta)) => {
                    info!(
                        "{}: 잔여 포지션 청산 완료 — {:.2}%",
                        self.stock_name,
                        result.pnl_pct()
                    );
                    self.save_trade_to_db(&result, &meta).await;
                }
                Err(e) => {
                    // 2026-04-18 Phase 1 (review round): Finding #1 수정.
                    // 기존에는 실패 시 current_position=None + Flat 전이로 강제 폐기했으나,
                    // holdings=0 을 확인하지 않고 Flat 으로 전이하는 것은 불변식 2 위반.
                    // 실계좌에서는 이 경로가 타면 "실제 보유하고 있는데 내부만 Flat" 상태가 되어
                    // 신규 매매가 중첩 노출된다.
                    //
                    // 정책:
                    // - real_mode=true: **절대 자동 폐기 금지**. manual_intervention 으로 전환하여
                    //   운영자 수동 확인을 강제. 러너 중단.
                    // - real_mode=false (paper): 모의투자 잔고잠김 시나리오 대응으로 기존처럼 폐기.
                    //   paper 실패는 실계좌에 영향 없고, 잔고 lock 이 풀리지 않는 모의 특유의 상황이라
                    //   자동 폐기 없이는 무한 루프 위험.
                    if self.live_cfg.real_mode {
                        error!(
                            "{}: 실전 잔여 포지션 청산 실패 — 자동 폐기 금지, manual_intervention 전환: {e}",
                            self.stock_name
                        );
                        self.trigger_manual_intervention(
                            format!(
                                "장 시작 잔여 포지션 청산 실패 (실전) — holdings 미확인 상태 자동 폐기 금지: {}",
                                e
                            ),
                            serde_json::json!({
                                "path": "startup_leftover_cleanup",
                                "real_mode": true,
                                "error": e.to_string(),
                            }),
                            ManualInterventionMode::KeepLock,
                        )
                        .await;
                        return Ok(Vec::new());
                    }
                    // paper 전용 fallback — 잔고잠김 등 모의투자 특수 상황.
                    warn!(
                        "{}: 모의투자 잔여 포지션 청산 실패: {e} — paper 한정 강제 폐기",
                        self.stock_name
                    );
                    let mut state = self.state.write().await;
                    state.current_position = None;
                    state.exit_pending = false;
                    state.phase = "신호 탐색".to_string();
                    if let Some(ref store) = self.db_store {
                        let _ = store.delete_active_position(self.stock_code.as_str()).await;
                    }
                    drop(state);
                    if let Some(ref lock) = self.position_lock {
                        // 2026-04-17 v3 fixups H: 자기 lock 만 풀기.
                        release_lock_if_self_held(lock, self.stock_code.as_str()).await;
                    }
                    self.transition_execution_state(
                        ExecutionState::Flat,
                        "paper_leftover_force_discard",
                    )
                    .await;
                    warn!(
                        "{}: paper 포지션 강제 폐기 완료 — 깨끗한 상태에서 시작",
                        self.stock_name
                    );
                }
            }
        }

        // Multi-Stage OR: 5분 OR(09:05)부터 시작, 15분/30분은 poll_and_enter 안에서
        // available_stages 로 직접 계산하므로 여기선 5분 경계만 필요.
        let or_5m_end = NaiveTime::from_hms_opt(9, 5, 0).unwrap();

        self.update_phase("OR 수집 중 (5분)").await;
        self.wait_until_session_milestone(or_5m_end, cfg.force_exit)
            .await;
        if self.is_stopped() {
            return Ok(Vec::new());
        }

        // 메인 루프: 분봉 폴링 → 전략 평가 → 주문 실행
        // 재시작 복구: trade_count / confirmed_side 를 DB의 오늘 거래에서 복원하여
        // 백테스트와 라이브의 일별 상태가 항상 일치하도록 한다.
        // 야간 기동 후 다음날 09:00 까지 대기할 수 있으므로, 일자는 대기 이후 다시 계산한다.
        let today = Local::now().date_naive();
        if let Some(ref store) = self.db_store
            && let Ok(Some(t)) = store
                .get_last_trade_exit_today(self.stock_code.as_str(), today)
                .await
        {
            self.signal_state.write().await.search_after = Some(t);
            info!(
                "{}: 재시작 복구 — signal_state.search_after = {} (DB)",
                self.stock_name, t
            );
        }
        let mut all_trades: Vec<TradeResult> = Vec::new();
        let mut trade_count: usize = if let Some(ref store) = self.db_store {
            store
                .count_trades_today(self.stock_code.as_str(), today)
                .await
                .unwrap_or(0) as usize
        } else {
            0
        };
        let mut confirmed_side: Option<PositionSide> = if let Some(ref store) = self.db_store {
            match store
                .get_last_trade_side_pnl_today(self.stock_code.as_str(), today)
                .await
            {
                Ok(Some((side, pnl))) if pnl > 0.0 => match side.as_str() {
                    "Long" => Some(PositionSide::Long),
                    "Short" => Some(PositionSide::Short),
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        if trade_count > 0 {
            info!(
                "{}: 재시작 복구 — trade_count={}, confirmed_side={:?} (DB)",
                self.stock_name, trade_count, confirmed_side
            );
        }

        self.update_phase("신호 탐색").await;

        loop {
            if self.is_stopped() {
                // 2026-04-17 P0 + v3 F4+: stop 분기 결정을 helper 로 일원화.
                // reconcile_once 등이 stop_flag 를 세웠는데 manual_intervention_required 가
                // true 라면 "잔고 API 지연 → 숨은 포지션 오판 → 시장가 강제 청산" 회로를
                // 끊는다. 포지션은 active_positions DB 행에 그대로 보존되어 운영자
                // 수동 처리 또는 재시작 복구 경로(`check_and_restore_position`)로 다룬다.
                // shared position_lock 도 해제하지 않는다 — 다른 종목 러너의 신규 진입을
                // 차단해 unresolved live position 과 중첩 노출되는 회귀를 막는다.
                let action = classify_main_loop_stop(&*self.state.read().await);

                match action {
                    MainLoopStopAction::SkipForManual { had_position } => {
                        warn!(
                            "{}: manual_intervention 활성 — 자동 청산 스킵, 포지션 DB 보존",
                            self.stock_name
                        );
                        // 2026-04-17 v3 F2: trailing/best_price/reached_1r 의 동기 저장.
                        // 평소 fire-and-forget spawn 이라 manual 시점에 race 가능.
                        self.persist_current_position_for_manual_recovery().await;
                        if let Some(ref el) = self.event_logger {
                            el.log_event(
                                self.stock_code.as_str(),
                                "position",
                                "auto_close_skipped",
                                "warn",
                                "manual_intervention 활성 — 운영자 수동 처리 대기 (main_loop)",
                                serde_json::json!({
                                    "path": "main_loop",
                                    "had_position": had_position,
                                }),
                            );
                        }
                        break;
                    }
                    MainLoopStopAction::CloseAndExit { has_position } => {
                        info!("{}: 외부 중지 요청", self.stock_name);
                        if has_position
                            && let Ok((result, meta)) =
                                self.close_position_market(ExitReason::EndOfDay).await
                        {
                            self.save_trade_to_db(&result, &meta).await;
                            all_trades.push(result);
                        }
                        break;
                    }
                }
            }

            let now = Local::now().time();

            // 강제 청산 시각
            if now >= cfg.force_exit {
                let state = self.state.read().await;
                if state.current_position.is_some() {
                    drop(state);
                    info!("{}: 장마감 강제 청산", self.stock_name);
                    if let Ok((result, meta)) =
                        self.close_position_market(ExitReason::EndOfDay).await
                    {
                        self.save_trade_to_db(&result, &meta).await;
                        all_trades.push(result);
                    }
                }
                break;
            }

            // 진입 마감 — 실전 모드에서 LiveRunnerConfig 의 entry_cutoff_override 가
            // 있으면 OrbFvgConfig.entry_cutoff 보다 이른 시각으로 마감한다 (Task 4).
            let effective_cutoff = self.live_cfg.effective_entry_cutoff(cfg.entry_cutoff);
            if now >= effective_cutoff {
                let state = self.state.read().await;
                if state.current_position.is_none() {
                    info!(
                        "{}: 진입 마감 ({})",
                        self.stock_name,
                        effective_cutoff.format("%H:%M")
                    );
                    break;
                }
                drop(state);
            }

            // 포지션 보유 중: 실시간 청산 관리
            let has_position = self.state.read().await.current_position.is_some();
            if has_position {
                match self.manage_position().await {
                    Ok(Some((result, meta))) => {
                        let pnl = result.pnl_pct();
                        let side = result.side;
                        let exit_time = result.exit_time;
                        self.save_trade_to_db(&result, &meta).await;
                        all_trades.push(result);
                        trade_count += 1;
                        self.update_pnl(&all_trades).await;

                        // 백테스트 sync_search_to(last_unlock_time) 등가:
                        // 다음 신호 탐색을 청산 시각 이후의 5분봉으로 한정.
                        // 같은 FVG 재진입 차단 (2026-04-10 trade #54~58 사고 핫픽스).
                        {
                            let mut ss = self.signal_state.write().await;
                            ss.search_after = Some(exit_time);
                        }

                        let cumulative_pnl: f64 = all_trades.iter().map(|t| t.pnl_pct()).sum();

                        // 일일 최대 거래 횟수 — LiveRunnerConfig 의 override 가 우선.
                        // 실전 모드에서는 max_daily_trades_override=Some(1) 이 기본이라
                        // 하루 1회 진입 후 결과와 관계없이 러너가 멈춘다 (Task 4).
                        let effective_max = self
                            .live_cfg
                            .effective_max_daily_trades(cfg.max_daily_trades);
                        if trade_count >= effective_max {
                            info!(
                                "{}: 최대 거래 횟수 도달 ({}회, real_mode={})",
                                self.stock_name, effective_max, self.live_cfg.real_mode
                            );
                            break;
                        }

                        // 일일 누적 손실 한도
                        if cumulative_pnl <= cfg.max_daily_loss_pct {
                            info!(
                                "{}: 일일 손실 한도 도달 ({:.2}%)",
                                self.stock_name, cumulative_pnl
                            );
                            break;
                        }

                        info!(
                            "{}: {}차 거래 {:.2}% (누적 {:.2}%) — 신호 탐색 계속",
                            self.stock_name, trade_count, pnl, cumulative_pnl
                        );

                        confirmed_side = if pnl > 0.0 { Some(side) } else { None };
                        self.update_phase("신호 탐색").await;
                    }
                    Ok(None) => {} // 유지
                    Err(e) => {
                        warn!("{}: 포지션 관리 실패: {e}", self.stock_name);
                    }
                }
                // 2026-04-18 Phase 4: sleep 주기를 `live_cfg.manage_poll_interval()` 로 설정화.
                // WebSocket 가격 리스너가 별도 task 로 SL 틱을 처리하므로 여기는
                // TP 체결 확인 주기 용도. stop 시 즉시 깨움.
                tokio::select! {
                    _ = tokio::time::sleep(self.live_cfg.manage_poll_interval()) => {}
                    _ = self.stop_notify.notified() => {}
                }
                continue;
            }

            // VI 발동/거래정지 중이면 신호 탐색 건너뛰기
            if self.state.read().await.market_halted {
                debug!("{}: 장 중단 중 — 신호 탐색 대기", self.stock_name);
                tokio::select! {
                    _ = tokio::time::sleep(self.live_cfg.poll_interval()) => {}
                    _ = self.stop_notify.notified() => {}
                }
                continue;
            }

            // 분봉 폴링 → 신호 탐색 → 진입
            // 2026-04-17 v3 불변식 #1b: require_or_breakout 는 모든 거래에서 true.
            // 기존 `trade_count == 0` 분기는 2 차 거래 이후 OR 돌파 가드를 꺼서
            // 2026-04-17 2 차 `or_breakout_pct=-0.054%` 음수 진입을 허용했다.
            match self.poll_and_enter(true, confirmed_side).await {
                Ok(true) => {
                    self.update_phase("포지션 보유").await;
                }
                Ok(false) => {} // 신호 없음
                Err(e) => {
                    warn!("{}: 폴링 실패: {e}", self.stock_name);
                }
            }

            // 폴링 주기: 두 종목 LiveRunner 가 거의 동시에 깨어나도록 하여
            // 한 종목이 청산 후 즉시 재진입하면서 다른 종목이 lock 잡을 윈도우 자체가
            // 사라지는 race 를 줄임. fetch_candles_split 은 local 메모리 조회라 부하 영향 없음.
            // 2026-04-18 Phase 4: 기존 5초 하드코딩 → `live_cfg.poll_interval()`.
            // 기본값 5_000 ms 로 기존 동작 보존. 운영자가 CLI / env 로 단축 가능.
            // 권장 (Phase 4): 1_000~2_000 ms (이벤트 기반 전환 전 중간 단계).
            tokio::select! {
                _ = tokio::time::sleep(self.live_cfg.poll_interval()) => {}
                _ = self.stop_notify.notified() => {}
            }
        }

        // 2026-04-15 Codex re-review: manual_intervention 상태면 phase 를 "종료" 로
        // 덮어쓰지 않는다. 운영자에게 가장 보여줘야 할 "수동 개입 필요" 상태가 러너 종료
        // 시점에 약화되지 않도록 한다 (status 동기화 태스크와 러너 종료 후처리가 이
        // phase 를 그대로 UI 로 노출한다).
        let is_manual = self.state.read().await.manual_intervention_required;
        if !is_manual {
            self.update_phase("종료").await;
        } else {
            info!(
                "{}: manual_intervention 유지 — 종료 phase 전환 스킵",
                self.stock_name
            );
        }
        self.update_pnl(&all_trades).await;

        for (i, t) in all_trades.iter().enumerate() {
            info!(
                "{}: {}차 거래 {:?} {:.2}% ({:?})",
                self.stock_name,
                i + 1,
                t.side,
                t.pnl_pct(),
                t.exit_reason
            );
        }

        Ok(all_trades)
    }

    // (track_order_and_guard_repeat 의 순수 로직은 파일 하단 free function 으로 분리해 테스트 가능하게 함)

    /// 진입 시도 중단 공통 경로.
    ///
    /// - `advance_search=true`: 해당 FVG 재탐색 차단 (search_after 전진).
    ///   미체결 timeout / drift 초과 / 진입 cutoff 등 "이 FVG는 포기" 사유.
    /// - `advance_search=false`: lock 만 해제. search_after 불변.
    ///   API 일시 장애 / 선점(preempt) / 현재가 조회 실패 등 "FVG 자체는 살려둠" 사유.
    ///
    /// lock 순서: `signal_state` write → drop → `position_lock` write.
    /// 두 락을 동시에 보유하지 않아 향후 다른 경로와 데드락 위험 없음.
    ///
    /// `sig` 의 FVG 메타데이터는 event_log 에 함께 기록되어 "버려진 FVG 의 사후 분석"
    /// (가상 성과 계산, 진입 실패 원인별 품질 차이 등) 에 사용된다.
    async fn abort_entry(
        &self,
        sig: &BestSignal,
        advance_search: bool,
        reason: &str,
        current_price: Option<i64>,
    ) {
        let signal_time = sig.signal_time;
        if advance_search {
            let mut ss = self.signal_state.write().await;
            let new_after = ss.search_after.map_or(signal_time, |t| t.max(signal_time));
            ss.search_after = Some(new_after);
        }
        if let Some(ref lock) = self.position_lock {
            // 2026-04-17 v3 fixups H: 자기 lock 만 풀기.
            release_lock_if_self_held(lock, self.stock_code.as_str()).await;
        }
        info!(
            "{}: 진입 중단 ({}) — advance_search={}",
            self.stock_name, reason, advance_search
        );
        if let Some(ref el) = self.event_logger {
            let mut meta = sig.to_event_metadata(current_price);
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("advance_search".into(), serde_json::json!(advance_search));
                obj.insert("reason".into(), serde_json::json!(reason));
            }
            el.log_event(
                self.stock_code.as_str(),
                "strategy",
                "abort_entry",
                "info",
                reason,
                meta,
            );
        }
        // Phase 2: 진입 포기 경로는 Flat 으로 되돌림. 현재 prev 는 대개 Flat 이라 idempotent,
        // Phase 3/4 에서 SignalArmed/EntryPending 이 적극 쓰이기 시작하면 의미를 가진다.
        let abort_reason = format!("abort_entry:{}", reason);
        self.transition_execution_state(ExecutionState::Flat, &abort_reason)
            .await;
    }

    /// 반복 발주 자동 안전장치.
    ///
    /// 최근 5분 창의 발주 이력을 유지하면서, 동일 가격(±0.1% tolerance)의 주문이
    /// 3회 이상 누적되면 `stop_flag` 를 세워 해당 종목 runner 를 자동 중단한다.
    /// 전체 봇을 중단하지 않아 dual-locked 다른 종목은 살린다.
    ///
    /// P0/P1 이 놓치는 미지 버그에 대한 최종 방어선.
    async fn track_order_and_guard_repeat(&self, limit_price: i64) {
        let now = Local::now().time();
        let mut queue = self.recent_orders.lock().await;

        let dup_count = update_order_queue_and_count_dup(&mut queue, now, limit_price);

        if dup_count >= 3 {
            error!(
                "{}: 동일가 {}원 5분 내 {}회 발주 감지 — runner 자동 중단",
                self.stock_name, limit_price, dup_count
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "strategy",
                    "repeated_order",
                    "critical",
                    &format!(
                        "동일가 {}원 {}회 반복 — runner stop",
                        limit_price, dup_count
                    ),
                    serde_json::json!({
                        "price": limit_price,
                        "count": dup_count,
                    }),
                );
            }
            // 해당 러너만 중단. 전체 봇은 살아있어 다른 종목은 계속 운영.
            self.stop_flag.store(true, Ordering::SeqCst);
            self.stop_notify.notify_waiters();
        }
    }

    /// Multi-Stage ORB: 분봉 폴링 → 각 OR 단계별 FVG 탐색 → 선착순 진입
    async fn poll_and_enter(
        &self,
        require_or_breakout: bool,
        confirmed_side: Option<PositionSide>,
    ) -> Result<bool, KisError> {
        let canonical_candles = self.fetch_canonical_5m_candles().await?;
        if canonical_candles.is_empty() {
            warn!("{}: 분봉 데이터 비어 있음", self.stock_name);
            return Ok(false);
        }

        let now = Local::now().time();
        let today = Local::now().date_naive();
        let available_stages = self
            .refresh_or_stages_from_canonical(&canonical_candles, now, today)
            .await;
        self.update_or_state_from_stages(&available_stages).await;
        if available_stages.is_empty() {
            warn!("{}: 가용 OR 단계 없음", self.stock_name);
            return Ok(false);
        }

        // 2026-04-19 Doc revision P2: preflight gate + Degraded 실 전이.
        //
        // refresh_preflight_metadata 가 실데이터 (spread/subscription_health/entry_cutoff/
        // active_instrument) 를 주입. gate 실패 원인에 따라 구분:
        // - all_ok=false (spread/nav/regime/subscription) → Degraded 전이
        // - all_ok=true 지만 entry_cutoff 도달 / active_instrument 불일치 → 단순 return (신호 무효)
        //
        // Degraded 는 신규 진입만 차단하고 기존 포지션 관리는 계속 (manage_position 경로 영향 없음).
        let preflight = self.refresh_preflight_metadata().await;
        if !preflight.all_ok() {
            let reason = if !preflight.subscription_health_ok {
                "subscription_health_stale"
            } else if preflight.quote_issue.is_some() {
                "quote_invalid"
            } else if !preflight.spread_ok {
                "spread_exceeded"
            } else if !preflight.nav_gate_ok {
                "nav_gap_exceeded"
            } else if !preflight.regime_confirmed {
                "regime_unconfirmed"
            } else {
                "preflight_gate_unknown"
            };
            self.transition_to_degraded(reason.to_string()).await;
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "strategy",
                    "preflight_gate_blocked",
                    "warn",
                    &format!("preflight gate 실패 ({reason}) — 신규 진입 차단"),
                    serde_json::json!({
                        "reason": reason,
                        "spread_ok": preflight.spread_ok,
                        "spread_ask": preflight.spread_ask,
                        "spread_bid": preflight.spread_bid,
                        "spread_mid": preflight.spread_mid,
                        "spread_raw": preflight.spread_raw,
                        "spread_tick": preflight.spread_tick,
                        "spread_in_ticks": preflight.spread_in_ticks(),
                        "spread_threshold_ticks": preflight.spread_threshold_ticks,
                        "quote_issue": preflight.quote_issue,
                        "nav_gate_ok": preflight.nav_gate_ok,
                        "regime_confirmed": preflight.regime_confirmed,
                        "subscription_health_ok": preflight.subscription_health_ok,
                        "entry_cutoff": preflight.entry_cutoff.map(|v| v.to_string()),
                        "active_instrument": preflight.active_instrument,
                    }),
                );
            }
            return Ok(false);
        }

        // 모든 gate 통과 — 이전에 Degraded 였다면 해제.
        self.clear_degraded_if_healthy().await;
        // cutoff 도달 / active_instrument 불일치 는 Degraded 아니라 단순 신호 무효.
        if !preflight.allows_entry_for(self.stock_code.as_str(), now) {
            return Ok(false);
        }

        let cfg = &self.strategy.config;

        // 각 OR 단계별 FVG 탐색 → 선착순 (가장 빠른 진입 시각)
        let mut best_signal: Option<BestSignal> = None;

        // 백테스트 sync_search_to 등가: 이전 청산 시각 이후의 캔들만 탐색.
        // 같은 5분봉의 같은 FVG 가 청산 후 재발견되는 것을 차단한다.
        let search_after = self.signal_state.read().await.search_after;

        for (stage_name, or_high, or_low, or_end) in &available_stages {
            // 이미 다른 stage 에서 최초 신호가 나왔으면 이 stage 는 건너뛴다.
            // 기존 루프가 `best_signal.is_none()` 체크로 overwrite 만 막던 것과 동치.
            if best_signal.is_some() {
                break;
            }

            // 2026-04-16 Task 4: 실전 안전 모드 — allowed_stages 가 지정된 경우
            // 허용 목록에 없는 stage 는 진입 경로에서 제외한다. UI/상태(or_stages)
            // 에는 그대로 노출하므로 관찰은 계속 가능.
            if !self.live_cfg.is_stage_allowed(stage_name) {
                continue;
            }

            // v3 불변식 #2: canonical 5m 시퀀스 직접 사용. 이미 5m granularity
            // 이므로 aggregate_completed_bars 재집계 불필요.
            let candles_5m_full: Vec<_> = canonical_candles
                .iter()
                .filter(|c| c.time >= *or_end)
                .cloned()
                .collect();
            // 백테스트 BacktestEngine::next_valid_candidate 의 search_from 진행과 등가
            let candles_5m: Vec<_> = match search_after {
                Some(t) => candles_5m_full.into_iter().filter(|c| c.time > t).collect(),
                None => candles_5m_full,
            };
            if candles_5m.len() < 3 {
                continue;
            }

            // Phase 2: 백테스트와 공유하는 공통 SignalEngine pure function 호출.
            // 탐지/리트레이스 확정 규칙은 여기서 한 번만 유지하고, 진입 가격 산정과
            // BestSignal 매핑만 라이브가 추가 처리한다.
            let engine_cfg = SignalEngineConfig {
                strategy_id: "orb_fvg".to_string(),
                stage_name: (*stage_name).to_string(),
                or_high: *or_high,
                or_low: *or_low,
                fvg_expiry_candles: cfg.fvg_expiry_candles,
                entry_cutoff: cfg.entry_cutoff,
                require_or_breakout,
                min_or_breakout_pct: cfg.min_or_breakout_pct,
                long_only: cfg.long_only,
                confirmed_side,
            };
            let detected = match detect_next_fvg_signal(&candles_5m, &engine_cfg) {
                Some(d) => d,
                None => continue,
            };

            let side = detected.intent.side;
            let gap = &detected.gap;
            // 라이브 진입가: zone 진입 즉시 체결되도록 가장 먼 경계로 지정가 설정.
            // Bullish FVG (Long): 가격이 위→아래로 내려오므로 top에서 먼저 체결.
            // Bearish FVG (Short): 가격이 아래→위로 올라오므로 bottom에서 먼저 체결.
            let entry_price = match side {
                PositionSide::Long => gap.top,
                PositionSide::Short => gap.bottom,
            };
            let stop_loss = gap.stop_loss;
            let risk = (entry_price - stop_loss).abs();
            let take_profit = match side {
                PositionSide::Long => entry_price + (risk as f64 * cfg.rr_ratio) as i64,
                PositionSide::Short => entry_price - (risk as f64 * cfg.rr_ratio) as i64,
            };

            let meta = &detected.intent.metadata;
            best_signal = Some(BestSignal {
                side,
                entry_price,
                stop_loss,
                take_profit,
                stage_name,
                or_high: *or_high,
                or_low: *or_low,
                signal_time: detected.intent.signal_time,
                gap_top: gap.top,
                gap_bottom: gap.bottom,
                gap_size_pct: meta.gap_size_pct,
                b_body_ratio: meta.b_body_ratio,
                or_breakout_pct: meta.or_breakout_pct,
                b_volume: meta.b_volume,
                b_close: meta.b_close,
                a_range: meta.a_range,
                b_time: meta.b_time,
            });
        }

        // 선착순으로 선택된 신호로 진입
        if let Some(sig) = best_signal {
            // 필드 참조 (sig 자체는 abort_entry 에 전달할 수 있도록 scope 유지)
            let side = sig.side;
            let entry_price = sig.entry_price;
            let stop_loss = sig.stop_loss;
            let take_profit = sig.take_profit;
            let stage_name = sig.stage_name;
            let or_high = sig.or_high;
            let or_low = sig.or_low;
            let signal_time = sig.signal_time;
            let _ = signal_time; // (사용 없음 — abort_entry 가 sig.signal_time 사용)

            // 공유 포지션 잠금 (2단계: Pending → Held)
            if let Some(ref lock) = self.position_lock {
                let mut guard = lock.write().await;
                match &*guard {
                    PositionLockState::Held(code) => {
                        // 포지션 확정 보유 중 — 다른 종목 진입 차단
                        if code != self.stock_code.as_str() {
                            debug!(
                                "{}: 진입 차단 — {}가 포지션 확정 보유 중",
                                self.stock_name, code
                            );
                        }
                        return Ok(false);
                    }
                    PositionLockState::ManualHeld(code) => {
                        // 2026-04-17 v3 fixups H: manual 종목의 unresolved live position
                        // 동안 다른 종목 신규 진입 차단. 자기 자신이 ManualHeld 라면
                        // 어차피 manual_intervention_required 로 상위에서 거부.
                        debug!(
                            "{}: 진입 차단 — {}가 manual 보유 중 (unresolved live position)",
                            self.stock_name, code
                        );
                        return Ok(false);
                    }
                    PositionLockState::Pending { code, preempted } => {
                        if code == self.stock_code.as_str() {
                            // 같은 종목이 이미 대기 중 — 중복 발주 방지
                            return Ok(false);
                        }
                        // 다른 종목이 대기 중 → 선점 요청 (preempted=true)
                        if !preempted {
                            info!("{}: {} 미체결 대기 중 — 선점 요청", self.stock_name, code);
                            *guard = PositionLockState::Pending {
                                code: code.clone(),
                                preempted: true,
                            };
                        }
                        // 이번 사이클은 진입하지 않음 — 다음 사이클에서 Free 확인 후 진입
                        return Ok(false);
                    }
                    PositionLockState::Free => {
                        // Pending으로 잠금 선점 (execute_entry 전에 설정)
                        *guard = PositionLockState::Pending {
                            code: self.stock_code.as_str().to_string(),
                            preempted: false,
                        };
                    }
                }
                drop(guard);
            }

            // VI 발동 중이면 진입 차단 (잠금 해제 필요)
            if self.state.read().await.market_halted {
                warn!("{}: VI 발동 중 — 진입 보류", self.stock_name);
                // FVG 자체는 유효 — search_after 전진 금지
                self.abort_entry(&sig, false, "vi_halted", None).await;
                return Ok(false);
            }

            // 신호 시점 현재가 (entry_signal 이벤트 및 drift 가드 공통 사용).
            // drift 가드가 켜졌으면 확보됨, 꺼졌으면 None.
            let mut current_price_at_signal: Option<i64> = None;

            // P1: FVG zone 이탈 가드 (Day+0 배포 시 max_entry_drift_pct=None 이면 스킵)
            if let Some(threshold) = self.live_cfg.max_entry_drift_pct {
                match self.get_current_price().await {
                    Ok(cur) => {
                        current_price_at_signal = Some(cur);
                        let drift = match side {
                            PositionSide::Long => (cur - entry_price) as f64 / entry_price as f64,
                            PositionSide::Short => (entry_price - cur) as f64 / entry_price as f64,
                        };
                        if drift > threshold {
                            warn!(
                                "{}: FVG zone 이탈 — current={}, entry={}, drift={:.3}% (한계 {:.3}%)",
                                self.stock_name,
                                cur,
                                entry_price,
                                drift * 100.0,
                                threshold * 100.0
                            );
                            if let Some(ref el) = self.event_logger {
                                let mut meta = sig.to_event_metadata(Some(cur));
                                if let Some(obj) = meta.as_object_mut() {
                                    obj.insert("drift".into(), serde_json::json!(drift));
                                    obj.insert("threshold".into(), serde_json::json!(threshold));
                                }
                                el.log_event(
                                    self.stock_code.as_str(),
                                    "strategy",
                                    "drift_rejected",
                                    "warn",
                                    &format!(
                                        "drift={:.3}% > {:.3}%",
                                        drift * 100.0,
                                        threshold * 100.0
                                    ),
                                    meta,
                                );
                            }
                            self.abort_entry(&sig, true, "drift_exceeded", Some(cur))
                                .await;
                            return Ok(false);
                        }
                    }
                    Err(e) => {
                        // fail-close: 현재가 모르면 지정가 발주 자체가 위험.
                        // 단, 일시 장애이므로 search_after 전진은 안 함.
                        warn!("{}: 현재가 조회 실패 — 진입 포기: {e}", self.stock_name);
                        self.abort_entry(&sig, false, "price_fetch_failed", None)
                            .await;
                        return Ok(false);
                    }
                }
            }

            info!(
                "{}: [{}] {:?} 진입 신호 — entry={}, SL={}, TP={}",
                self.stock_name, stage_name, side, entry_price, stop_loss, take_profit
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "strategy",
                    "entry_signal",
                    "info",
                    &format!(
                        "[{}] {:?} entry={}, SL={}, TP={}",
                        stage_name, side, entry_price, stop_loss, take_profit
                    ),
                    sig.to_event_metadata(current_price_at_signal),
                );
            }

            let signal_id = sig.signal_id(self.stock_code.as_str());

            // 2026-04-18 Review Finding #4: 신호 확정 ~ 주문 제출 구간을 SignalArmed 로 명시.
            // 2026-04-20 PR #5.b: actor rule 에 dispatch (Flat → SignalArmed).
            // `transition_execution_state` 직접 호출 대신 event 기반.
            self.dispatch_execution_event(
                crate::strategy::execution_actor::ExecutionEvent::SignalTriggered {
                    signal_id: signal_id.clone(),
                },
                "entry_signal_armed",
            )
            .await;

            // 2026-04-19 Doc review P1 (Phase 4 armed watch):
            // armed_watch_budget_ms > 0 이면 signal 확정 후 execute_entry 직전에 WS tick 기반
            // 재확인 루프를 돈다. preflight/drift/cutoff 재점검으로 신호 직후 급변을 흡수.
            // 기본 0 (paper) = 즉시 진입, 실전 200ms = 문서 권장.
            match self
                .armed_wait_for_entry(&sig, current_price_at_signal)
                .await
            {
                ArmedWaitOutcome::Ready => {
                    // 조건 유지 — 진입 진행
                }
                ArmedWaitOutcome::CutoffReached => {
                    self.abort_entry(&sig, true, "armed_cutoff_reached", current_price_at_signal)
                        .await;
                    return Ok(false);
                }
                ArmedWaitOutcome::PreflightFailed(reason) => {
                    self.abort_entry(&sig, false, reason, current_price_at_signal)
                        .await;
                    return Ok(false);
                }
                ArmedWaitOutcome::DriftExceeded => {
                    self.abort_entry(&sig, true, "armed_drift_exceeded", current_price_at_signal)
                        .await;
                    return Ok(false);
                }
                ArmedWaitOutcome::Stopped => {
                    self.abort_entry(&sig, false, "armed_stopped", current_price_at_signal)
                        .await;
                    return Ok(false);
                }
                ArmedWaitOutcome::Aborted(reason) => {
                    // PR #2.d: manual/degraded 전이로 armed watcher 가 중단된 경우.
                    // search_after 전진은 하지 않음 (신호 자체가 무효가 아니라 시스템 상태로 중단).
                    self.abort_entry(&sig, false, reason, current_price_at_signal)
                        .await;
                    return Ok(false);
                }
                ArmedWaitOutcome::AlreadyArmed => {
                    // PR #2.c: 같은 signal 에 이미 armed watcher 가 진행 중 — 중복 호출 skip.
                    // search_after 전진 금지 (기존 감시자가 원래 경로를 진행 중).
                    self.abort_entry(&sig, false, "armed_already_armed", current_price_at_signal)
                        .await;
                    return Ok(false);
                }
            }

            // 주문 실행 (API 에러만 재시도, 미체결은 재시도 불필요)
            match self
                .execute_entry(
                    side,
                    entry_price,
                    stop_loss,
                    take_profit,
                    stage_name,
                    or_high,
                    or_low,
                    &signal_id,
                )
                .await
            {
                Ok(_) => return Ok(true),
                Err(KisError::ManualIntervention(msg)) => {
                    // 2026-04-15 Codex review #4 대응: manual_intervention 전용 경로.
                    // abort_entry 를 부르지 않아 shared position_lock 이 `Held(self)` 그대로
                    // 유지되고, signal_state.search_after 도 건드리지 않는다.
                    // trigger_manual_intervention 이 이미 stop_flag 를 세웠으므로
                    // 상위 run() 루프는 다음 is_stopped 체크에서 탈출한다.
                    error!("{}: manual_intervention 경로 — {}", self.stock_name, msg);
                    return Err(KisError::ManualIntervention(msg));
                }
                Err(KisError::Preempted) => {
                    // 다른 종목 선점 요청으로 양보. FVG 자체는 유효 → search_after 전진 금지.
                    self.abort_entry(&sig, false, "preempted", current_price_at_signal)
                        .await;
                    return Ok(false);
                }
                Err(e) if e.is_retryable() => {
                    // API 에러(네트워크, 토큰 만료 등) — 1회 재시도
                    let delay = e.retry_delay_ms().max(500);
                    warn!("{}: API 에러, {}ms 후 재시도: {e}", self.stock_name, delay);
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    match self
                        .execute_entry(
                            side,
                            entry_price,
                            stop_loss,
                            take_profit,
                            stage_name,
                            or_high,
                            or_low,
                            &signal_id,
                        )
                        .await
                    {
                        Ok(_) => return Ok(true),
                        Err(KisError::ManualIntervention(msg)) => {
                            error!(
                                "{}: 재시도 중 manual_intervention 경로 — {}",
                                self.stock_name, msg
                            );
                            return Err(KisError::ManualIntervention(msg));
                        }
                        Err(KisError::Preempted) => {
                            self.abort_entry(
                                &sig,
                                false,
                                "preempted_on_retry",
                                current_price_at_signal,
                            )
                            .await;
                            return Ok(false);
                        }
                        Err(e2) => {
                            error!("{}: 재시도 실패 — 잠금 해제: {e2}", self.stock_name);
                            // API 일시 장애 — search_after 전진 금지 (다음 사이클 자연 재시도)
                            self.abort_entry(
                                &sig,
                                false,
                                "api_error_retry_failed",
                                current_price_at_signal,
                            )
                            .await;
                            return Ok(false);
                        }
                    }
                }
                Err(e) => {
                    // 미체결 취소, 진입 마감 임박 등 — 이 FVG 는 포기 (search_after 전진)
                    info!("{}: 진입 미성사 — 잠금 해제: {e}", self.stock_name);
                    self.abort_entry(
                        &sig,
                        true,
                        "fill_timeout_or_cutoff",
                        current_price_at_signal,
                    )
                    .await;
                    return Ok(false);
                }
            }
        }

        Ok(false)
    }

    /// 2026-04-19 Doc revision (불변식 2 확장 / 금지사항 #5):
    /// TP 지정가 **완전** 체결 후 Flat 전이 공용 경로.
    ///
    /// `fetch_fill_detail` 이 WS/REST 로 tp_filled_qty == expected_qty 를 확정한 뒤 호출된다.
    /// KRX 매칭 엔진이 holdings=0 을 보장하므로 추가 balance 조회 없이 Flat 으로 바로 전이한다.
    async fn finalize_tp_full_fill(
        &self,
        tp_order_no: String,
        pos: Position,
        tp_price: i64,
        tp_fill_price: i64,
    ) -> Result<(TradeResult, ExitTraceMeta), KisError> {
        let exit_time = Local::now().time();
        let result = TradeResult {
            side: pos.side,
            entry_price: pos.entry_price,
            exit_price: tp_fill_price,
            stop_loss: pos.stop_loss,
            take_profit: tp_price,
            entry_time: pos.entry_time,
            exit_time,
            exit_reason: ExitReason::TakeProfit,
            quantity: pos.quantity,
            intended_entry_price: pos.intended_entry_price,
            order_to_fill_ms: pos.order_to_fill_ms,
        };

        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: tp_order_no.clone(),
                signal_id: String::new(),
                broker_order_id: tp_order_no.clone(),
                phase: "exit_final_fill".into(),
                side: format!("{:?}", pos.side),
                intended_price: Some(tp_price),
                submitted_price: Some(tp_price),
                filled_price: Some(tp_fill_price),
                filled_qty: Some(pos.quantity as i64),
                holdings_before: Some(pos.quantity as i64),
                holdings_after: Some(0),
                resolution_source: "tp_limit".into(),
                reason: "tp_full_fill".into(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "exit_reason": "TakeProfit",
                    "tp_price": tp_price,
                    "fill_price_unknown": false,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: Some(Utc::now()),
                final_fill_at: Some(Utc::now()),
            };
            if let Err(e) = store.append_execution_journal(&entry).await {
                warn!(
                    "{}: execution_journal 저장 실패 (tp_full_fill): {e}",
                    self.stock_name
                );
            }
        }

        self.notify_trade("exit");
        self.finalize_flat_transition().await;
        info!("{}: TP 완전 체결 → Flat 전이", self.stock_name);
        Ok((result, ExitTraceMeta::tp_limit()))
    }

    /// 2026-04-19 Doc revision (불변식 2 확장 / 금지사항 #5):
    /// TP 지정가 **부분** 체결 후 잔량 시장가 청산 + verify 회로.
    ///
    /// 이전 구현은 잔량 시장가 주문 직후 `current_position.take()` + Flat 전이 했는데
    /// 이는 문서 금지사항 #5 직접 위반. 올바른 순서:
    ///
    /// 1. `ExitPartial` 전이 (execution_state enum + exit_pending=true)
    /// 2. TP 취소 (잔량)
    /// 3. 잔량 `place_market_sell` → market_order_no 확보
    /// 4. `verify_exit_completion(market_order_no, remaining, remaining)` WS/REST/balance 3단 검증
    /// 5. `has_remaining()` → manual_intervention
    /// 6. holdings=0 확인 시에만 Flat (TP+시장가 가중평균가 기록, 미확인이면 price_unknown)
    async fn finalize_tp_partial_fill(
        &self,
        tp_order_no: String,
        pos: Position,
        tp_price: i64,
        tp_fill_price: i64,
        tp_filled_qty: u64,
        expected_qty: u64,
    ) -> Result<(TradeResult, ExitTraceMeta), KisError> {
        let remaining = expected_qty - tp_filled_qty;
        warn!(
            "{}: TP 부분 체결 — {}주/{}주 (잔여 {}주) — ExitPartial 전이 후 verify 회로",
            self.stock_name, tp_filled_qty, expected_qty, remaining
        );

        // Step 1: ExitPartial 전이 + exit_pending 가드 (manage_position 중복 실행 방지).
        {
            let mut state = self.state.write().await;
            state.exit_pending = true;
        }
        self.transition_execution_state(ExecutionState::ExitPartial, "tp_partial_fill")
            .await;

        // Step 2: TP 잔량 취소 (best-effort — 이미 KRX 가 취소했을 수도 있음).
        let tp_orgno = {
            let s = self.state.read().await;
            s.current_position
                .as_ref()
                .and_then(|p| p.tp_krx_orgno.clone())
        };
        if let Some(ref orgno) = tp_orgno
            && let Err(e) = self.cancel_tp_order(&tp_order_no, orgno).await
        {
            warn!(
                "{}: TP 잔량 취소 실패 (시장가 재청산 진행): {e}",
                self.stock_name
            );
        }

        // Step 3: 잔량 시장가 매도 — 주문번호 확보.
        let market_order_no = match self.place_market_sell(remaining).await {
            Ok(no) => {
                // DB ExitPending 마킹 (TP 부분 체결 경로에서도 재시작 복구에 필요).
                if let Some(ref store) = self.db_store
                    && let Err(e) = store
                        .mark_active_position_exit_pending(
                            self.stock_code.as_str(),
                            &no,
                            "TakeProfit_partial",
                        )
                        .await
                {
                    warn!(
                        "{}: TP 부분 체결 exit_pending DB 마킹 실패: {e}",
                        self.stock_name
                    );
                }
                no
            }
            Err(e) => {
                error!(
                    "{}: TP 부분 체결 잔량 시장가 청산 실패 — manual_intervention: {e}",
                    self.stock_name
                );
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: tp_order_no.clone(),
                        signal_id: String::new(),
                        broker_order_id: String::new(),
                        phase: "exit_retry_submit_failed".into(),
                        side: format!("{:?}", pos.side),
                        intended_price: Some(tp_price),
                        submitted_price: None,
                        filled_price: Some(tp_fill_price),
                        filled_qty: Some(tp_filled_qty as i64),
                        holdings_before: Some(remaining as i64),
                        holdings_after: None,
                        resolution_source: "unresolved".into(),
                        reason: "tp_partial_retry_submit_failed".into(),
                        error_message: e.to_string(),
                        metadata: serde_json::json!({
                            "prior_order_no": tp_order_no,
                            "tp_filled_qty": tp_filled_qty,
                            "expected_qty": expected_qty,
                            "remaining": remaining,
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                self.trigger_manual_intervention(
                    format!("TP 부분 체결 잔량 {}주 시장가 청산 실패: {}", remaining, e),
                    serde_json::json!({
                        "path": "manage_position.tp_partial_fill",
                        "tp_order_no": tp_order_no,
                        "tp_fill_price": tp_fill_price,
                        "tp_filled_qty": tp_filled_qty,
                        "expected_qty": expected_qty,
                        "remaining": remaining,
                        "error": e.to_string(),
                    }),
                    ManualInterventionMode::KeepLock,
                )
                .await;
                return Err(KisError::ManualIntervention(format!(
                    "TP 부분 체결 잔량 청산 실패: {e}"
                )));
            }
        };

        // Step 4: verify 회로 재사용 — 시장가 잔량에 대해 WS/REST/balance 3단 검증.
        let verification = self
            .verify_exit_completion_with_actor_timeout(
                ExecutionState::ExitPartial,
                &market_order_no,
                remaining,
                remaining,
                "tp_partial_fill_exit_verify_timeout",
            )
            .await?;
        info!(
            "{}: TP 잔량 verify 결과 — source={}, filled_qty={}, filled_price={:?}, holdings_after={:?}",
            self.stock_name,
            verification.resolution_source.label(),
            verification.filled_qty,
            verification.filled_price,
            verification.holdings_after,
        );

        // Step 5: 잔량 남음 → manual_intervention (불변식 5).
        if verification.has_remaining() {
            let still_held = verification.holdings_after.unwrap_or(remaining);
            error!(
                "{}: TP 잔량 시장가 후에도 {}주 잔존 — manual_intervention",
                self.stock_name, still_held
            );
            if let Some(ref store) = self.db_store {
                let entry = ExecutionJournalEntry {
                    stock_code: self.stock_code.as_str().to_string(),
                    execution_id: tp_order_no.clone(),
                    signal_id: String::new(),
                    broker_order_id: market_order_no.clone(),
                    phase: "exit_retry_still_remaining_manual".into(),
                    side: format!("{:?}", pos.side),
                    intended_price: Some(tp_price),
                    submitted_price: None,
                    filled_price: verification.filled_price,
                    filled_qty: Some(verification.filled_qty as i64),
                    holdings_before: Some(remaining as i64),
                    holdings_after: Some(still_held as i64),
                    resolution_source: verification.resolution_source.label().into(),
                    reason: "tp_partial_retry_still_remaining".into(),
                    error_message: String::new(),
                    metadata: serde_json::json!({
                        "prior_order_no": tp_order_no,
                        "tp_fill_price": tp_fill_price,
                        "tp_filled_qty": tp_filled_qty,
                        "expected_qty": expected_qty,
                        "still_held": still_held,
                    }),
                    sent_at: None,
                    ack_at: None,
                    first_fill_at: None,
                    final_fill_at: None,
                };
                let _ = store.append_execution_journal(&entry).await;
            }
            self.trigger_manual_intervention(
                format!("TP 부분 체결 + 잔량 시장가 후에도 {}주 남음", still_held),
                serde_json::json!({
                    "path": "manage_position.tp_partial_fill",
                    "tp_order_no": tp_order_no,
                    "market_order_no": market_order_no,
                    "tp_filled_qty": tp_filled_qty,
                    "expected_qty": expected_qty,
                    "still_held": still_held,
                }),
                ManualInterventionMode::KeepLock,
            )
            .await;
            return Err(KisError::ManualIntervention(format!(
                "TP 잔량 + 시장가 후에도 {}주 남음",
                still_held
            )));
        }

        // Step 6: holdings=0 확인 → Flat 전이. TP 체결가 + 시장가 잔량 체결가 가중평균.
        let (final_exit_price, exit_meta) = match verification.filled_price {
            Some(market_fp) => {
                let total_qty = tp_filled_qty + verification.filled_qty;
                let avg = if total_qty > 0 {
                    let total_value = tp_fill_price as i128 * tp_filled_qty as i128
                        + market_fp as i128 * verification.filled_qty as i128;
                    (total_value / total_qty as i128) as i64
                } else {
                    tp_fill_price
                };
                (avg, ExitTraceMeta::resolved(verification.resolution_source))
            }
            None => {
                // 시장가 체결가 미확인 but holdings=0 확정 — fill_price_unknown 기록.
                // TP 체결가로 fallback (TP 부분분만의 기록가). 가중평균 불가.
                warn!(
                    "{}: TP 잔량 시장가 체결가 미확인 (holdings=0 확인) — TP 체결가로 fallback, fill_price_unknown=true",
                    self.stock_name
                );
                (
                    tp_fill_price,
                    ExitTraceMeta::price_unknown(verification.resolution_source),
                )
            }
        };

        let exit_time = Local::now().time();
        let result = TradeResult {
            side: pos.side,
            entry_price: pos.entry_price,
            exit_price: final_exit_price,
            stop_loss: pos.stop_loss,
            take_profit: tp_price,
            entry_time: pos.entry_time,
            exit_time,
            exit_reason: ExitReason::TakeProfit,
            quantity: pos.quantity,
            intended_entry_price: pos.intended_entry_price,
            order_to_fill_ms: pos.order_to_fill_ms,
        };

        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: tp_order_no.clone(),
                signal_id: String::new(),
                broker_order_id: market_order_no.clone(),
                phase: "exit_retry_final_fill".into(),
                side: format!("{:?}", pos.side),
                intended_price: Some(tp_price),
                submitted_price: None,
                filled_price: Some(final_exit_price),
                filled_qty: Some(remaining as i64),
                holdings_before: Some(remaining as i64),
                holdings_after: Some(0),
                resolution_source: verification.resolution_source.label().into(),
                reason: "tp_partial_retry_final_fill".into(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "prior_order_no": tp_order_no,
                    "tp_fill_price": tp_fill_price,
                    "tp_filled_qty": tp_filled_qty,
                    "expected_qty": expected_qty,
                    "market_filled_price": verification.filled_price,
                    "market_filled_qty": verification.filled_qty,
                    "fill_price_unknown": exit_meta.fill_price_unknown,
                    "weighted_avg_price": final_exit_price,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: Some(Utc::now()),
                final_fill_at: Some(Utc::now()),
            };
            let _ = store.append_execution_journal(&entry).await;
        }

        self.notify_trade("exit");
        self.finalize_flat_transition().await;
        info!(
            "{}: TP 부분 체결 경로 완료 (weighted avg {}원) → Flat 전이",
            self.stock_name, final_exit_price
        );
        Ok((result, exit_meta))
    }

    /// 포지션 실시간 관리 — 백테스트 simulate_exit 동일 로직
    ///
    /// 청산 우선순위 (백테스트와 동일):
    /// 1. TP 지정가 체결 (KRX 거래소 우선, 레이턴시 0)
    /// 2. best_price 갱신 + 1R 본전스탑 + 트레일링 SL
    /// 3. SL 도달 → 서버 측 시장가 청산
    /// 4. 시간스탑 / 장마감
    async fn manage_position(&self) -> Result<Option<(TradeResult, ExitTraceMeta)>, KisError> {
        // stop 토글 시 즉시 청산
        if self.is_stopped() {
            // 2026-04-17 P0: manual_intervention 경로의 자동 청산 차단.
            // 메인 루프 1252 분기와 동일한 보호 — 두 경로 모두 막아야 reconcile
            // 강제 청산을 완전히 차단할 수 있다 (메인 루프가 manage_position 보다
            // 먼저 stop 분기를 탔지만, 이 함수가 직접 호출되는 다른 경로도 보호).
            let is_manual = should_skip_auto_close_for_manual(&*self.state.read().await);
            if is_manual {
                warn!(
                    "{}: manual_intervention 활성 — manage_position 자동 청산 스킵",
                    self.stock_name
                );
                // 2026-04-17 v3 F2: trailing/best_price/reached_1r 의 동기 저장.
                self.persist_current_position_for_manual_recovery().await;
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "position",
                        "auto_close_skipped",
                        "warn",
                        "manual_intervention 활성 — 운영자 수동 처리 대기 (manage_position)",
                        serde_json::json!({"path": "manage_position"}),
                    );
                }
                return Ok(None);
            }
            let (result, meta) = self.close_position_market(ExitReason::EndOfDay).await?;
            return Ok(Some((result, meta)));
        }

        // 2026-04-18 Phase 1 (execution-timing-implementation-plan) 불변식 4:
        // `exit_pending=true` 이면 이미 청산 주문이 진행 중이므로 TP/SL/Time 판정을 전부
        // 스킵한다. `close_position_market` 이 verify 단계에 있는 동안 중복 청산 주문이
        // 발생하면 원래 보유 수량 초과 매도로 이어지거나 manual_intervention 우회 경로가
        // 열릴 수 있음 (불변식 4: pending 중 추가 주문 금지).
        if self.state.read().await.exit_pending {
            debug!(
                "{}: exit_pending 활성 — manage_position 판정 스킵",
                self.stock_name
            );
            return Ok(None);
        }

        let current = self.get_current_price().await?;
        let now = Local::now().time();
        let cfg = &self.strategy.config;

        let mut state = self.state.write().await;
        let pos = match state.current_position.as_mut() {
            Some(p) => p,
            None => return Ok(None),
        };

        let original_risk = (pos.entry_price - pos.original_sl).abs();
        let pm_cfg = PositionManagerConfig {
            rr_ratio: cfg.rr_ratio,
            trailing_r: cfg.trailing_r,
            breakeven_r: cfg.breakeven_r,
            time_stop_candles: cfg.time_stop_candles,
            candle_interval_min: 5,
        };

        // ── Step 1: TP 지정가 체결 체크 (거래소 우선, SL보다 먼저) ──
        if is_tp_hit(pos.side, pos.take_profit, current) {
            if let Some(ref tp_no) = pos.tp_order_no {
                // TP 지정가 체결 확인 (내부에서 폴링 재시도)
                let tp_no = tp_no.clone();
                let tp_price = pos.take_profit;
                let expected_qty = pos.quantity;
                let pos_snapshot = pos.clone();
                drop(state);

                if let Some((tp_fill_price, tp_filled_qty)) = self.fetch_fill_detail(&tp_no).await {
                    info!(
                        "{}: TP 지정가 체결 확인 — {}원, {}주/{}주",
                        self.stock_name, tp_fill_price, tp_filled_qty, expected_qty
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            self.stock_code.as_str(),
                            "position",
                            "exit_tp",
                            "info",
                            &format!(
                                "TP 체결 — {}원, {}주/{}주",
                                tp_fill_price, tp_filled_qty, expected_qty
                            ),
                            serde_json::json!({
                                "fill_price": tp_fill_price,
                                "tp_price": tp_price,
                                "filled_qty": tp_filled_qty,
                                "expected_qty": expected_qty,
                            }),
                        );
                    }

                    // ── 분기 1: 완전 체결 ──
                    // KRX 매칭 엔진 기준 holdings=0 이 보장되는 경로.
                    // fetch_fill_detail 이 WS+REST 검증을 이미 수행 (체결 확정).
                    if tp_filled_qty >= expected_qty {
                        return self
                            .finalize_tp_full_fill(tp_no, pos_snapshot, tp_price, tp_fill_price)
                            .await
                            .map(Some);
                    }

                    // ── 분기 2: 부분 체결 (doc 불변식 2 확장 + 금지사항 #5) ──
                    // 잔량 시장가 주문 직후 Flat 금지. ExitPartial 전이 → verify → holdings=0 후 Flat.
                    if tp_filled_qty > 0 {
                        return self
                            .finalize_tp_partial_fill(
                                tp_no,
                                pos_snapshot,
                                tp_price,
                                tp_fill_price,
                                tp_filled_qty,
                                expected_qty,
                            )
                            .await
                            .map(Some);
                    }

                    // ── 분기 3: filled_qty == 0 ──
                    debug!(
                        "{}: TP 체결 미확인 (filled_qty=0) — 다음 루프 재시도",
                        self.stock_name
                    );
                    return Ok(None);
                }
                // fetch_fill_detail 자체 실패 — 다음 루프 재시도 (체결 지연 가능)
                debug!(
                    "{}: TP 체결 미확인 (fetch 실패) — 다음 루프 재시도",
                    self.stock_name
                );
            } else {
                // TP 지정가 없음 — 시장가 fallback
                drop(state);
                let (result, meta) = self.close_position_market(ExitReason::TakeProfit).await?;
                return Ok(Some((result, meta)));
            }
            return Ok(None);
        }

        // ── Step 2: best_price 갱신 + 1R 본전스탑 + 트레일링 (공통 helper) ──
        let prev_sl = pos.stop_loss;
        let update = update_best_and_trailing(
            pos.side,
            pos.entry_price,
            &mut pos.best_price,
            &mut pos.stop_loss,
            &mut pos.reached_1r,
            original_risk,
            current,
            &pm_cfg,
        );
        if update.reached_1r_newly {
            info!(
                "{}: 1R 도달 — 본전 스탑 활성화 (SL → {})",
                self.stock_name, pos.stop_loss
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "position",
                    "breakeven_activated",
                    "info",
                    &format!("1R 도달 — SL → {}", pos.stop_loss),
                    serde_json::json!({
                        "price": current,
                        "new_sl": pos.stop_loss,
                        "best_price": pos.best_price
                    }),
                );
            }
        } else if update.trailing_changed {
            debug!(
                "{}: 트레일링 SL 갱신 {} → {}",
                self.stock_name, prev_sl, pos.stop_loss
            );
        }

        // 트레일링 상태 변경 시 DB에 저장 (재시작 복구용)
        if update.trailing_changed
            && let Some(ref store) = self.db_store
        {
            let code = self.stock_code.as_str().to_string();
            let sl = pos.stop_loss;
            let r1r = pos.reached_1r;
            let bp = pos.best_price;
            let store = Arc::clone(store);
            tokio::spawn(async move {
                let _ = store.update_position_trailing(&code, sl, r1r, bp).await;
            });
        }

        // ── Step 3: SL 체크 ──
        // WS 틱 리스너가 이미 감지한 경우(`sl_triggered_tick=true`) 는 우선 존중하고,
        // 그 외에는 REST 폴백/최신 캐시 `current` 로 판정.
        let sl_breached = pos.sl_triggered_tick
            || match pos.side {
                PositionSide::Long => current <= pos.stop_loss,
                PositionSide::Short => current >= pos.stop_loss,
            };
        if sl_breached {
            let reason = sl_exit_reason(
                pos.side,
                pos.stop_loss,
                pos.original_sl,
                pos.entry_price,
                pos.reached_1r,
            );
            drop(state);
            let (result, meta) = self.close_position_market(reason).await?;
            return Ok(Some((result, meta)));
        }

        // ── Step 4: 시간스탑 (백테스트와 공통 helper) ──
        if time_stop_breached_by_minutes(pos.entry_time, now, &pm_cfg, pos.reached_1r) {
            drop(state);
            let (result, meta) = self.close_position_market(ExitReason::TimeStop).await?;
            return Ok(Some((result, meta)));
        }

        Ok(None)
    }

    /// 지정가 진입 주문 (현행 live 정책: zone edge 패시브 지정가 / 또는 MarketableLimit).
    async fn execute_entry(
        &self,
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        _take_profit: i64,
        stage_name: &str,
        or_high: i64,
        or_low: i64,
        signal_id: &str,
    ) -> Result<(), KisError> {
        // 2026-04-18 Review Finding #4 → 2026-04-20 PR #5.b 변경:
        // 이전에는 함수 진입 즉시 `EntryPending` 으로 전이했으나, actor rule 은
        // `EntryOrderAccepted` (주문 접수 = broker_order_id 수령) 이벤트로만 SignalArmed →
        // EntryPending 을 허용한다. 그래서 execute_entry 전 구간은 SignalArmed 를 유지하고,
        // 주문 성공 직후 (place_limit_buy 응답 파싱 완료 시점) 에 `EntryOrderAccepted`
        // 이벤트를 dispatch 한다. pre-check 실패 경로는 기존 `abort_entry` 가 SignalArmed → Flat
        // (`TimeoutExpired{EntrySubmit}` 과 동일한 허용 전이) 로 되돌린다.

        let order_side = match side {
            PositionSide::Long => OrderSide::Buy,
            PositionSide::Short => OrderSide::Sell,
        };

        // ── 2026-04-16 실전 Go 조건 #2: 전역 일일 거래 한도 체크 ──
        // 종목별 max_daily_trades_override(실전 1) 와 별개로 시스템 전체 한도를 강제한다.
        // 두 러너가 각자 1회씩 진입해 총 2회가 되는 경로를 차단.
        // 주의: 실제 체결 완료 후에만 record_trade 를 호출하므로, 여기서 can_enter 가 true 라도
        // 체결 실패 시 카운트는 늘지 않는다. 동일 시점 두 러너가 병렬로 can_enter 를 통과할 수
        // 있으나, 공유 `position_lock` 이 Pending/Held 로 한 종목만 진행시키므로 실제 record 는
        // 순차적이다.
        if let Some(ref gate) = self.live_cfg.global_trade_gate {
            let today = Local::now().date_naive();
            let mut g = gate.write().await;
            let (count, max) = g.snapshot(today);
            if count >= max {
                warn!(
                    "{}: 전역 일일 거래 한도 도달 ({}/{}) — 진입 차단",
                    self.stock_name, count, max
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "strategy",
                        "global_trade_gate_blocked",
                        "warn",
                        &format!("전역 한도 도달 {}/{} — 진입 거부", count, max),
                        serde_json::json!({ "count": count, "max": max }),
                    );
                }
                return Err(KisError::Internal(format!(
                    "전역 일일 거래 한도 도달 ({}/{})",
                    count, max
                )));
            }
        }

        // ── 시간 경계 사전 체크 ──
        // 지정가 대기(최대 30초) 고려하여 15:19:30 이후 진입 포기
        let now = Local::now().time();
        if now >= NaiveTime::from_hms_opt(15, 19, 30).unwrap() {
            warn!(
                "{}: 진입 마감 30초 전 — 지정가 대기 불가, 진입 포기",
                self.stock_name
            );
            return Err(KisError::Internal("진입 마감 임박".into()));
        }

        // 2026-04-18 Review Finding #3: 실행 정책에 따라 주문가/라벨 분기.
        // - PassiveZoneEdge: 기존 경로 — zone edge intended entry 를 호가단위 정렬 (기본).
        // - MarketableLimit: best ask/bid ± N tick 으로 시장성 지정가 + slippage budget preflight.
        let (limit_price, entry_mode_label) = match self.live_cfg.execution_policy_kind {
            ExecutionPolicyKind::PassiveZoneEdge => {
                (round_to_etf_tick(entry_price), "limit_passive")
            }
            ExecutionPolicyKind::MarketableLimit {
                tick_offset,
                slippage_budget_pct,
            } => {
                let quote_anchor = self.best_quote_for_side(side).await;
                let anchor = match quote_anchor {
                    Some(price) => price,
                    None => self.get_current_price().await.unwrap_or(entry_price),
                };
                let tick = etf_tick_size(anchor);
                let offset = (tick_offset as i64).max(1) * tick;
                let marketable = match side {
                    PositionSide::Long => anchor + offset,
                    PositionSide::Short => (anchor - offset).max(tick),
                };
                let rounded = round_to_etf_tick(marketable);

                // preflight gate: intended_entry(zone edge) 대비 이탈률이 budget 을 넘으면 포기.
                // ETF 문서 "1호가 스프레드 제한" 과 등가의 최소 방어선 — 시장성이 지나치게
                // 공격적으로 체결되는 추격매매를 차단.
                let base = entry_price.max(1);
                let slippage_pct = (rounded - entry_price).abs() as f64 / base as f64;
                if slippage_pct > slippage_budget_pct {
                    warn!(
                        "{}: marketable slippage budget 초과 — 주문가 {} vs intended {} ({:.3}% > {:.3}%) 진입 포기",
                        self.stock_name,
                        rounded,
                        entry_price,
                        slippage_pct * 100.0,
                        slippage_budget_pct * 100.0
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            self.stock_code.as_str(),
                            "order",
                            "marketable_slippage_budget_exceeded",
                            "warn",
                            &format!(
                                "slippage {:.3}% > budget {:.3}% — 진입 포기",
                                slippage_pct * 100.0,
                                slippage_budget_pct * 100.0
                            ),
                            serde_json::json!({
                                "intended_entry": entry_price,
                                "marketable_price": rounded,
                                "quote_anchor": anchor,
                                "quote_source": if quote_anchor.is_some() { "best_quote" } else { "last_trade" },
                                "tick_offset": tick_offset,
                                "slippage_pct": slippage_pct,
                                "budget_pct": slippage_budget_pct,
                                "side": format!("{:?}", side),
                            }),
                        );
                    }
                    return Err(KisError::Internal(format!(
                        "marketable slippage {:.3}% > budget {:.3}%",
                        slippage_pct * 100.0,
                        slippage_budget_pct * 100.0
                    )));
                }

                (rounded, "marketable_limit")
            }
        };

        // 주문 직전 매수가능수량 실시간 조회 (가용금액 변동 반영)
        let actual_qty = {
            let price_str = limit_price.to_string();
            let buyable_query = [
                ("CANO", self.client.account_no()),
                ("ACNT_PRDT_CD", self.client.account_product_code()),
                ("PDNO", self.stock_code.as_str()),
                ("ORD_UNPR", price_str.as_str()),
                ("ORD_DVSN", "00"),
                ("CMA_EVLU_AMT_ICLD_YN", "N"),
                ("OVRS_ICLD_YN", "N"),
            ];
            use crate::domain::models::account::BuyableInfo;
            let resp: Result<KisResponse<BuyableInfo>, _> = self
                .client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/trading/inquire-psbl-order",
                    &TransactionId::InquireBuyable,
                    Some(&buyable_query),
                    None,
                )
                .await;
            match resp {
                Ok(r) => match r.into_result() {
                    Ok(info) if info.orderable_qty() > 1 => {
                        let api_qty = info.orderable_qty();
                        let qty = (api_qty - 1) as u64; // 1주 여유 (슬리피지 대비)
                        info!(
                            "{}: 매수가능조회 상세 — ord_psbl_cash={}, nrcvb_buy_amt={}, nrcvb_buy_qty={}, max_buy_amt={}, max_buy_qty={}",
                            self.stock_name,
                            info.ord_psbl_cash,
                            info.nrcvb_buy_amt,
                            info.nrcvb_buy_qty,
                            info.max_buy_amt,
                            info.max_buy_qty
                        );
                        info!(
                            "{}: 주문 직전 매수가능 {}주 (API {}주 - 1)",
                            self.stock_name, qty, api_qty
                        );
                        qty
                    }
                    Ok(info) => {
                        warn!(
                            "{}: 매수가능수량 부족 ({}주)",
                            self.stock_name,
                            info.orderable_qty()
                        );
                        0
                    }
                    Err(e) => {
                        warn!(
                            "{}: 매수가능조회 파싱 실패: {e} — 잔고 기반 fallback (증거금율 미반영, 80%)",
                            self.stock_name
                        );
                        let raw = self.get_available_cash().await;
                        let available = (raw as f64 * 0.80) as i64;
                        if available > 0 && entry_price > 0 {
                            let qty = (available / entry_price) as u64;
                            info!(
                                "{}: 잔고 기반 수량 {}주 (가용 {}원의 80%={}원)",
                                self.stock_name, qty, raw, available
                            );
                            qty
                        } else {
                            0
                        }
                    }
                },
                Err(e) => {
                    warn!(
                        "{}: 매수가능조회 실패: {e} — 잔고 기반 fallback (증거금율 미반영, 80%)",
                        self.stock_name
                    );
                    let raw = self.get_available_cash().await;
                    let available = (raw as f64 * 0.80) as i64;
                    if available > 0 && entry_price > 0 {
                        let qty = (available / entry_price) as u64;
                        info!(
                            "{}: 잔고 기반 수량 {}주 (가용 {}원의 80%={}원)",
                            self.stock_name, qty, raw, available
                        );
                        qty
                    } else {
                        0
                    }
                }
            }
        };

        if actual_qty == 0 {
            return Err(KisError::InsufficientBalance("매수가능수량 0주".into()));
        }

        // cancel/fill race 방어용 진입 전 보유 수량 스냅샷 (2026-04-15 사고 대응).
        // balance 조회 실패 시 race 감지 불가 → fail-close 로 진입 포기.
        // 2026-04-15 Codex re-review: avg=0/미제공이어도 qty>0 이면 보유로 간주해야
        // 이후 cancel 재검증이 "미보유" 로 오판되지 않는다 — `fetch_holding_snapshot` 사용.
        let holding_before: u64 = match self.fetch_holding_snapshot().await {
            Ok(Some((qty, _avg_opt))) => qty,
            Ok(None) => 0,
            Err(e) => {
                warn!(
                    "{}: 진입 전 잔고 조회 실패 — race 방어 불가로 진입 포기: {e}",
                    self.stock_name
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "order",
                        "entry_aborted_no_balance",
                        "warn",
                        "진입 전 잔고 조회 실패",
                        serde_json::json!({"error": e.to_string()}),
                    );
                }
                return Err(KisError::Internal(format!("진입 전 잔고 조회 실패: {e}")));
            }
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "00",
            "ORD_QTY": actual_qty.to_string(),
            "ORD_UNPR": limit_price.to_string(),
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        let side_str = format!("{:?}", order_side);
        let order_start = tokio::time::Instant::now();
        let submit_sent_at = Utc::now();
        let resp: KisResponse<serde_json::Value> = match self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &tr_id,
                None,
                Some(&body),
            )
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: signal_id.to_string(),
                        signal_id: signal_id.to_string(),
                        broker_order_id: String::new(),
                        phase: "entry_submit_failed".into(),
                        side: side_str.clone(),
                        intended_price: Some(entry_price),
                        submitted_price: Some(limit_price),
                        filled_price: None,
                        filled_qty: None,
                        holdings_before: Some(holding_before as i64),
                        holdings_after: Some(holding_before as i64),
                        resolution_source: "unresolved".into(),
                        reason: "submit_network_error".into(),
                        error_message: e.to_string(),
                        metadata: serde_json::json!({
                            "stage": stage_name,
                            "entry_mode": entry_mode_label,
                            "quantity": actual_qty,
                        }),
                        sent_at: Some(submit_sent_at),
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                return Err(e);
            }
        };
        let submit_ack_at = Utc::now();
        let submit_ack_elapsed_ms = order_start.elapsed().as_millis() as u64;

        if resp.rt_cd != "0" {
            // 주문 실패 로그
            if let Some(ref store) = self.db_store {
                store
                    .save_order_log(
                        self.stock_code.as_str(),
                        "진입",
                        &side_str,
                        actual_qty as i64,
                        limit_price,
                        "",
                        "실패",
                        &resp.msg1,
                    )
                    .await;

                let entry = ExecutionJournalEntry {
                    stock_code: self.stock_code.as_str().to_string(),
                    execution_id: signal_id.to_string(),
                    signal_id: signal_id.to_string(),
                    broker_order_id: String::new(),
                    phase: "entry_submit_rejected".into(),
                    side: side_str.clone(),
                    intended_price: Some(entry_price),
                    submitted_price: Some(limit_price),
                    filled_price: None,
                    filled_qty: None,
                    holdings_before: Some(holding_before as i64),
                    holdings_after: Some(holding_before as i64),
                    resolution_source: "unresolved".into(),
                    reason: "broker_rejected".into(),
                    error_message: resp.msg1.clone(),
                    metadata: serde_json::json!({
                        "stage": stage_name,
                        "entry_mode": entry_mode_label,
                        "rt_cd": resp.rt_cd,
                        "msg_cd": resp.msg_cd,
                        "quantity": actual_qty,
                    }),
                    sent_at: Some(submit_sent_at),
                    ack_at: Some(submit_ack_at),
                    first_fill_at: None,
                    final_fill_at: None,
                };
                let _ = store.append_execution_journal(&entry).await;
            }
            return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
        }

        // 주문번호 + KRX 거래소번호 추출 (취소 시 필요)
        let order_no = resp
            .output
            .as_ref()
            .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();
        let krx_orgno = resp
            .output
            .as_ref()
            .and_then(|v| v.get("KRX_FWDG_ORD_ORGNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();

        info!(
            "{}: 지정가 매수 발주 완료 — {}원 {}주, 주문번호={}",
            self.stock_name, limit_price, actual_qty, order_no
        );

        // 2026-04-20 PR #5.b: 주문 접수 확정 시점의 actor event. broker_order_id
        // 수령 + REST rt_cd=0 성공이 EntryOrderAccepted 의 정의와 일치.
        // actor rule: SignalArmed → EntryPending, commands: [PersistState].
        self.dispatch_execution_event(
            crate::strategy::execution_actor::ExecutionEvent::EntryOrderAccepted {
                broker_order_id: order_no.clone(),
            },
            "execute_entry_submitted",
        )
        .await;

        if let Some(ref store) = self.db_store {
            store
                .save_order_log(
                    self.stock_code.as_str(),
                    "진입",
                    &side_str,
                    actual_qty as i64,
                    limit_price,
                    &order_no,
                    "발주",
                    "",
                )
                .await;

            let pending_theoretical_risk = (entry_price - stop_loss).abs();
            let pending = ActivePosition {
                stock_code: self.stock_code.as_str().to_string(),
                side: format!("{:?}", side),
                entry_price: limit_price,
                stop_loss,
                take_profit: _take_profit,
                quantity: actual_qty as i64,
                tp_order_no: String::new(),
                tp_krx_orgno: String::new(),
                entry_time: Local::now().naive_local(),
                original_sl: stop_loss,
                reached_1r: false,
                best_price: limit_price,
                or_high: Some(or_high),
                or_low: Some(or_low),
                or_stage: Some(stage_name.to_string()),
                or_source: None,
                trigger_price: Some(entry_price),
                original_risk: Some(pending_theoretical_risk),
                signal_id: Some(signal_id.to_string()),
                entry_mode: Some(entry_mode_label.to_string()),
                manual_intervention_required: false,
                execution_state: "entry_pending".to_string(),
                last_exit_order_no: String::new(),
                last_exit_reason: String::new(),
                pending_entry_order_no: order_no.clone(),
                pending_entry_krx_orgno: krx_orgno.clone(),
            };

            let mut pending_saved = false;
            let mut last_err: Option<String> = None;
            for attempt in 0..3u32 {
                match store.save_active_position(&pending).await {
                    Ok(_) => {
                        if attempt > 0 {
                            info!(
                                "{}: entry_pending DB 저장 재시도 후 성공 ({}회)",
                                self.stock_name, attempt
                            );
                        }
                        pending_saved = true;
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e.to_string());
                        tokio::time::sleep(std::time::Duration::from_millis(
                            200 * (attempt + 1) as u64,
                        ))
                        .await;
                    }
                }
            }
            if !pending_saved {
                let reason = format!(
                    "entry_pending DB 저장 실패 — order_no={}, error={}",
                    order_no,
                    last_err.unwrap_or_else(|| "unknown".to_string())
                );
                self.trigger_manual_intervention(
                    reason.clone(),
                    serde_json::json!({
                        "path": "execute_entry.persist_entry_pending",
                        "order_no": order_no,
                        "quantity": actual_qty,
                        "limit_price": limit_price,
                    }),
                    ManualInterventionMode::KeepLock,
                )
                .await;
                return Err(KisError::ManualIntervention(reason));
            }

            let submitted = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: order_no.clone(),
                signal_id: signal_id.to_string(),
                broker_order_id: String::new(),
                phase: "entry_submitted".into(),
                side: side_str.clone(),
                intended_price: Some(entry_price),
                submitted_price: Some(limit_price),
                filled_price: None,
                filled_qty: None,
                holdings_before: Some(holding_before as i64),
                holdings_after: Some(holding_before as i64),
                resolution_source: String::new(),
                reason: "order_submitted".into(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "stage": stage_name,
                    "entry_mode": entry_mode_label,
                    "quantity": actual_qty,
                }),
                sent_at: Some(submit_sent_at),
                ack_at: None,
                first_fill_at: None,
                final_fill_at: None,
            };
            let _ = store.append_execution_journal(&submitted).await;

            let acknowledged = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: order_no.clone(),
                signal_id: signal_id.to_string(),
                broker_order_id: order_no.clone(),
                phase: "entry_acknowledged".into(),
                side: side_str.clone(),
                intended_price: Some(entry_price),
                submitted_price: Some(limit_price),
                filled_price: None,
                filled_qty: None,
                holdings_before: Some(holding_before as i64),
                holdings_after: Some(holding_before as i64),
                resolution_source: String::new(),
                reason: "broker_acknowledged".into(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "stage": stage_name,
                    "entry_mode": entry_mode_label,
                    "quantity": actual_qty,
                    "krx_orgno": krx_orgno,
                }),
                sent_at: Some(submit_sent_at),
                ack_at: Some(submit_ack_at),
                first_fill_at: None,
                final_fill_at: None,
            };
            let _ = store.append_execution_journal(&acknowledged).await;
        }

        // 반복 발주 자동 안전장치: 5분 창 안에서 동일가 ±0.1% 주문이 3회 이상 발생하면
        // 해당 종목 runner 자동 중단 (P0/P1 우회 버그 최종 방어선).
        // 2026-04-14 사고(동일 94465 지정가 282회 발주) 재발 방지.
        self.track_order_and_guard_repeat(limit_price).await;

        // ── 체결 대기 루프 (LiveRunnerConfig.entry_fill_timeout_ms, WebSocket 우선 + REST 폴백) ──
        // 2026-04-18 Phase 3: 하드코딩 timeout 을 설정화.
        // 2026-04-24 P1-1: paper passive 기본값은 60_000 ms 로 상향했다.
        let (entry_fill_phase, fill_timeout) = self
            .execution_timeout_for_state(ExecutionState::EntryPending)
            .unwrap_or((TimeoutPhase::EntryFill, self.live_cfg.entry_fill_timeout()));
        let entry_deadline = NaiveTime::from_hms_opt(15, 20, 0).unwrap();
        let mut filled_qty: u64 = 0;
        let mut fill_price: i64 = 0;

        let mut was_preempted = false;
        let mut entry_fill_timed_out = false;
        let mut timeout_elapsed_ms: Option<u64> = None;
        let mut first_fill_notice_at_ms: Option<u64> = None;
        loop {
            if self.is_stopped() {
                break;
            }
            if order_start.elapsed() >= fill_timeout {
                entry_fill_timed_out = true;
                timeout_elapsed_ms = Some(order_start.elapsed().as_millis() as u64);
                break;
            }
            if Local::now().time() >= entry_deadline {
                warn!(
                    "{}: 대기 중 진입 마감(15:20) 도달 — 미체결 취소",
                    self.stock_name
                );
                break;
            }
            // 선점 체크 — 다른 종목이 진입 요청 시 즉시 양보
            if let Some(ref lock) = self.position_lock {
                let state = lock.read().await;
                if let PositionLockState::Pending {
                    preempted: true, ..
                } = &*state
                {
                    warn!(
                        "{}: 다른 종목 선점 요청 감지 — 미체결 취소",
                        self.stock_name
                    );
                    was_preempted = true;
                    break;
                }
            }

            let remaining = fill_timeout.saturating_sub(order_start.elapsed());
            let wait_dur = remaining.min(std::time::Duration::from_secs(5));

            // WebSocket 체결 통보 대기 (최대 5초 단위)
            if let Some((price, qty)) = self.wait_execution_notice(&order_no, wait_dur).await {
                first_fill_notice_at_ms
                    .get_or_insert_with(|| order_start.elapsed().as_millis() as u64);
                fill_price = price;
                filled_qty = qty;
                if filled_qty >= actual_qty {
                    break;
                }
            }

            // REST 1회 조회 (WebSocket 미수신 보완)
            if filled_qty < actual_qty
                && let Some((price, qty)) = self.query_execution(&order_no).await
            {
                fill_price = price;
                filled_qty = qty;
                if filled_qty >= actual_qty {
                    break;
                }
            }
        }

        // ── 미체결/부분 체결 처리 ──
        if filled_qty < actual_qty {
            if entry_fill_timed_out {
                let mut timeout_partial_source: Option<&'static str> = None;
                let mut timeout_holding_after: Option<u64> = None;

                // 2026-04-24 P1-1: timeout 직후 곧바로 Flat+cancel 로 내리면,
                // 뒤늦은 부분체결이 `Flat -> EntryPartial` race 로 보인다.
                // 먼저 REST/잔고를 확인해 이미 체결된 수량은 EntryPartial 로 정상 전이한다.
                if filled_qty < actual_qty
                    && let Some((price, qty)) = self.query_execution(&order_no).await
                {
                    fill_price = price;
                    filled_qty = qty.min(actual_qty);
                    if filled_qty > 0 {
                        timeout_partial_source = Some("rest_execution_before_cancel");
                    }
                }

                if filled_qty == 0 {
                    match self.fetch_holding_snapshot().await {
                        Ok(Some((qty_after, avg_opt))) if qty_after > holding_before => {
                            let gained = (qty_after - holding_before).min(actual_qty);
                            filled_qty = gained;
                            fill_price = avg_opt.unwrap_or(limit_price);
                            timeout_holding_after = Some(qty_after);
                            timeout_partial_source = Some("balance_before_cancel");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(
                                "{}: timeout 직후 balance 확인 실패 — cancel 후 재검증으로 넘김: {e}",
                                self.stock_name
                            );
                        }
                    }
                }

                if filled_qty > 0 && filled_qty < actual_qty {
                    self.transition_execution_state(
                        ExecutionState::EntryPartial,
                        "entry_timeout_partial_detected",
                    )
                    .await;
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            self.stock_code.as_str(),
                            "order",
                            "entry_timeout_partial_detected",
                            "warning",
                            &format!(
                                "timeout 직후 부분체결 확인 — {}주/{}주, 잔량 취소 진행",
                                filled_qty, actual_qty
                            ),
                            serde_json::json!({
                                "order_no": &order_no,
                                "limit_price": limit_price,
                                "filled_qty": filled_qty,
                                "expected_qty": actual_qty,
                                "source": timeout_partial_source,
                                "holding_before": holding_before,
                                "holding_after": timeout_holding_after,
                                "timeout_elapsed_ms": timeout_elapsed_ms,
                                "first_fill_notice_at_ms": first_fill_notice_at_ms,
                                "entry_fill_timeout_ms_cfg": self.live_cfg.entry_fill_timeout_ms,
                            }),
                        );
                    }
                    if let Some(ref store) = self.db_store {
                        let resolution_source = match timeout_partial_source {
                            Some("rest_execution_before_cancel") => "rest_execution",
                            Some("balance_before_cancel") => "balance_only",
                            _ => "unknown",
                        };
                        let entry = ExecutionJournalEntry {
                            stock_code: self.stock_code.as_str().to_string(),
                            execution_id: order_no.clone(),
                            signal_id: signal_id.to_string(),
                            broker_order_id: order_no.clone(),
                            phase: "entry_partial".into(),
                            side: side_str.clone(),
                            intended_price: Some(entry_price),
                            submitted_price: Some(limit_price),
                            filled_price: Some(if fill_price > 0 {
                                fill_price
                            } else {
                                limit_price
                            }),
                            filled_qty: Some(filled_qty as i64),
                            holdings_before: Some(holding_before as i64),
                            holdings_after: timeout_holding_after.map(|qty| qty as i64),
                            resolution_source: resolution_source.into(),
                            reason: "entry_timeout_partial_detected".into(),
                            error_message: String::new(),
                            metadata: serde_json::json!({
                                "expected_qty": actual_qty,
                                "source": timeout_partial_source,
                            }),
                            sent_at: Some(submit_sent_at),
                            ack_at: Some(submit_ack_at),
                            first_fill_at: Some(Utc::now()),
                            final_fill_at: Some(Utc::now()),
                        };
                        let _ = store.append_execution_journal(&entry).await;
                    }
                } else if filled_qty < actual_qty {
                    self.handle_execution_timeout_expired(
                        entry_fill_phase,
                        "execute_entry_fill_timeout",
                    )
                    .await;
                }
            }

            if filled_qty < actual_qty {
                let was_partial = filled_qty > 0;
                if was_partial {
                    info!(
                        "{}: 부분 체결 {}주/{}주 — 잔량 취소 시도",
                        self.stock_name, filled_qty, actual_qty
                    );
                } else {
                    info!(
                        "{}: 미체결 — 주문 취소 시도 (주문번호={})",
                        self.stock_name, order_no
                    );
                }

                // 미체결 잔량 취소 (cancel_tp_order는 범용 취소 — 동일 API).
                // cancel 3회 재시도 모두 실패 시 cancel_all fallback으로 이 종목의 모든 미체결 정리.
                // 이 지점은 체결 확정 전 + TP 발주 전이라 이 종목의 정상 TP는 존재하지 않음 → 안전.
                let cancel_attempted_at_ms = order_start.elapsed().as_millis() as u64;
                if let Err(e) = self.cancel_tp_order(&order_no, &krx_orgno).await {
                    warn!(
                        "{}: 주문 취소 3회 실패 — cancel_all fallback: {e}",
                        self.stock_name
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            self.stock_code.as_str(),
                            "order",
                            "cancel_fallback",
                            "warn",
                            &format!(
                                "주문 {} cancel 실패 → cancel_all_pending_orders 실행",
                                order_no
                            ),
                            serde_json::json!({"order_no": &order_no}),
                        );
                    }
                    if let Err(e) = self
                        .cancel_all_pending_orders("entry_cancel_fallback")
                        .await
                    {
                        warn!("{}: entry_cancel_fallback 실패: {e}", self.stock_name);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                // 취소 직후 체결 재조회 (레이스 컨디션 방지: 취소 직전 체결 가능)
                if let Some((price, qty)) = self.query_execution(&order_no).await {
                    if qty > filled_qty {
                        fill_price = price;
                        filled_qty = qty.min(actual_qty);
                    } else if fill_price == 0 && price > 0 {
                        fill_price = price;
                    }
                }

                if filled_qty == 0 {
                    // ── balance 재검증 (2026-04-15 사고 대응) ──
                    // query_execution 이 0을 반환해도 실제로는 체결된 후 cancel 이 된 상황 가능.
                    // inquire-balance 로 보유 수량 증가를 직접 확인하여 유령 포지션을 방지한다.
                    //
                    // 2026-04-15 Codex re-review: qty>0 이면 avg 제공 여부와 관계없이
                    // race 로 판정해야 한다 (과거 `fetch_holding_qty` 가 avg>0 까지 요구해
                    // 이 경로를 블라인드 스팟으로 남겼음 — `fetch_holding_snapshot` 으로 일원화).
                    match self.fetch_holding_snapshot().await {
                        Ok(Some((qty_after, avg_opt))) if qty_after > holding_before => {
                            let gained = qty_after - holding_before;
                            // avg 미제공 시 지정가를 fallback 체결가로 사용 (슬리피지 0 가정).
                            let fill_price_resolved = avg_opt.unwrap_or(limit_price);
                            error!(
                                "{}: ⚠ cancel/fill race 감지 — filled_qty=0 이지만 보유 {}주 증가 (before={}, after={}, avg={})",
                                self.stock_name,
                                gained,
                                holding_before,
                                qty_after,
                                avg_opt
                                    .map(|v| format!("{}원", v))
                                    .unwrap_or_else(|| "미제공".to_string())
                            );
                            if let Some(ref el) = self.event_logger {
                                el.log_event(
                                self.stock_code.as_str(),
                                "order",
                                "cancel_fill_race_detected",
                                "critical",
                                &format!(
                                    "cancel 후 보유 증가 — {}주 (before={}, after={})",
                                    gained, holding_before, qty_after
                                ),
                                serde_json::json!({
                                    "order_no": &order_no,
                                    "limit_price": limit_price,
                                    "holding_before": holding_before,
                                    "holding_after": qty_after,
                                    "gained": gained,
                                    "avg_price": avg_opt,
                                    "fill_price_resolved": fill_price_resolved,
                                    "submit_ack_elapsed_ms": submit_ack_elapsed_ms,
                                    "timeout_elapsed_ms": timeout_elapsed_ms,
                                    "first_fill_notice_at_ms": first_fill_notice_at_ms,
                                    "cancel_attempted_at_ms": cancel_attempted_at_ms,
                                    "entry_fill_timeout_ms_cfg": self.live_cfg.entry_fill_timeout_ms,
                                }),
                            );
                            }
                            // 2026-04-19 Doc revision P2: EntryPartial 실 전이.
                            // cancel/fill race 에서 확인된 체결이 expected 미달이면 EntryPartial 로
                            // 명시한다. 체결 확정 경로로 fall-through 하면 execute_entry 끝에서
                            // Open 전이가 적용되어 최종적으로 Open 으로 수렴하지만, 중간 phase 가
                            // journal 에 남아 holdings 증가 기준 "부분 체결 확정" 사건을 분석 가능.
                            if gained < actual_qty {
                                self.transition_execution_state(
                                    ExecutionState::EntryPartial,
                                    "cancel_fill_race_partial",
                                )
                                .await;
                                if let Some(ref store) = self.db_store {
                                    let entry = ExecutionJournalEntry {
                                        stock_code: self.stock_code.as_str().to_string(),
                                        execution_id: order_no.clone(),
                                        signal_id: signal_id.to_string(),
                                        broker_order_id: order_no.clone(),
                                        phase: "entry_partial".into(),
                                        side: format!("{:?}", order_side),
                                        intended_price: Some(entry_price),
                                        submitted_price: Some(limit_price),
                                        filled_price: Some(fill_price_resolved),
                                        filled_qty: Some(gained as i64),
                                        holdings_before: Some(holding_before as i64),
                                        holdings_after: Some(qty_after as i64),
                                        resolution_source: "balance_only".into(),
                                        reason: "cancel_fill_race_partial".into(),
                                        error_message: String::new(),
                                        metadata: serde_json::json!({
                                            "expected_qty": actual_qty,
                                            "gained": gained,
                                        }),
                                        sent_at: Some(submit_sent_at),
                                        ack_at: Some(submit_ack_at),
                                        first_fill_at: Some(Utc::now()),
                                        final_fill_at: Some(Utc::now()),
                                    };
                                    let _ = store.append_execution_journal(&entry).await;
                                }
                            }
                            // 실제 체결된 것으로 처리 → 체결 확정 경로로 fall-through.
                            filled_qty = gained.min(actual_qty);
                            fill_price = fill_price_resolved;
                            // 아래 "체결 확정" 블록으로 진행
                        }
                        Ok(_) => {
                            // 잔고 증가 없음 → 정상 미체결
                            let cancel_reason = if was_preempted {
                                "선점으로 미체결 취소"
                            } else {
                                "지정가 매수 미체결 확정"
                            };
                            info!(
                                "{}: {} — 주문 {} 취소 완료",
                                self.stock_name, cancel_reason, order_no
                            );
                            if let Some(ref store) = self.db_store {
                                store
                                    .save_order_log(
                                        self.stock_code.as_str(),
                                        "진입",
                                        &side_str,
                                        actual_qty as i64,
                                        limit_price,
                                        &order_no,
                                        "미체결취소",
                                        "",
                                    )
                                    .await;

                                let _ =
                                    store.delete_active_position(self.stock_code.as_str()).await;
                                let entry = ExecutionJournalEntry {
                                    stock_code: self.stock_code.as_str().to_string(),
                                    execution_id: order_no.clone(),
                                    signal_id: signal_id.to_string(),
                                    broker_order_id: order_no.clone(),
                                    phase: "entry_cancelled".into(),
                                    side: side_str.clone(),
                                    intended_price: Some(entry_price),
                                    submitted_price: Some(limit_price),
                                    filled_price: None,
                                    filled_qty: Some(0),
                                    holdings_before: Some(holding_before as i64),
                                    holdings_after: Some(holding_before as i64),
                                    resolution_source: "balance_only".into(),
                                    reason: if was_preempted {
                                        "entry_preempted_cancelled".into()
                                    } else {
                                        "entry_timeout_cancelled".into()
                                    },
                                    error_message: String::new(),
                                    metadata: serde_json::json!({
                                        "preempted": was_preempted,
                                        "holding_verified": true,
                                    }),
                                    sent_at: Some(submit_sent_at),
                                    ack_at: Some(submit_ack_at),
                                    first_fill_at: None,
                                    final_fill_at: None,
                                };
                                let _ = store.append_execution_journal(&entry).await;
                            }
                            if let Some(ref el) = self.event_logger {
                                el.log_event(
                                    self.stock_code.as_str(),
                                    "order",
                                    "entry_cancelled",
                                    "info",
                                    &format!(
                                        "{} — {}원 {}주",
                                        cancel_reason, limit_price, actual_qty
                                    ),
                                    serde_json::json!({
                                        "order_no": &order_no,
                                        "limit_price": limit_price,
                                        "quantity": actual_qty,
                                        "preempted": was_preempted,
                                        "holding_verified": true,
                                    }),
                                );
                            }
                            if was_preempted {
                                return Err(KisError::Preempted);
                            }
                            return Err(KisError::Internal("지정가 매수 미체결".into()));
                        }
                        Err(e) => {
                            // balance 조회 실패 → 실제 보유 여부 모름 → manual_intervention.
                            // 2026-04-15 Codex review #4 대응: `KisError::ManualIntervention` 로
                            // 반환해 상위 `poll_and_enter` 가 일반 진입 실패로 abort_entry 호출 +
                            // shared lock 해제하는 회귀를 차단한다.
                            let reason = format!(
                                "cancel_verify_failed: order_no={} limit={} qty={} error={}",
                                order_no, limit_price, actual_qty, e
                            );
                            if let Some(ref el) = self.event_logger {
                                el.log_event(
                                    self.stock_code.as_str(),
                                    "order",
                                    "cancel_verify_failed",
                                    "critical",
                                    "cancel 후 balance 조회 실패 — 유령 포지션 가능",
                                    serde_json::json!({
                                        "order_no": &order_no,
                                        "limit_price": limit_price,
                                        "quantity": actual_qty,
                                        "error": e.to_string(),
                                    }),
                                );
                            }
                            self.trigger_manual_intervention(
                                reason.clone(),
                                serde_json::json!({
                                    "trigger": "cancel_verify_failed",
                                    "order_no": &order_no,
                                    "limit_price": limit_price,
                                    "quantity": actual_qty,
                                    "error": e.to_string(),
                                }),
                                ManualInterventionMode::KeepLock,
                            )
                            .await;
                            return Err(KisError::ManualIntervention(reason));
                        }
                    }
                }
            }
        }

        // ── 체결 확정 (전량 또는 부분) ──
        let actual_qty = filled_qty; // 최종 체결 수량으로 갱신
        // 지정가 체결가 = 주문가 (슬리피지 0). fill_price가 0이면 주문가 fallback.
        let actual_entry = if fill_price > 0 {
            fill_price
        } else {
            limit_price
        };
        let order_to_fill_ms = order_start.elapsed().as_millis() as i64;
        let entry_time = Local::now().time();
        let entry_timestamp = Local::now().naive_local();
        let execution_id = self.signal_execution_id(entry_time, actual_entry, actual_qty);

        // 주문 체결 로그
        if let Some(ref store) = self.db_store {
            store
                .save_order_log(
                    self.stock_code.as_str(),
                    "진입",
                    &side_str,
                    actual_qty as i64,
                    actual_entry,
                    &order_no,
                    "체결",
                    "",
                )
                .await;
        }

        info!(
            "{}: {:?} {}주 지정가 진입 완료 (지정가={}, 체결가={})",
            self.stock_name, order_side, actual_qty, limit_price, actual_entry
        );
        self.notify_trade("entry");

        // Pending → Held 전환 (체결 확정 — 선점 요청이 있어도 체결이 우선)
        if let Some(ref lock) = self.position_lock {
            *lock.write().await = PositionLockState::Held(self.stock_code.as_str().to_string());
        }

        // 실제 체결가 기준으로 SL/TP 재계산.
        // R(=theoretical risk)은 (이론 entry - 이론 SL)로 유지하고, actual entry에 평행 이동.
        let theoretical_risk = (entry_price - stop_loss).abs();
        let rr = self.strategy.config.rr_ratio;
        let (actual_sl, actual_tp) = match side {
            PositionSide::Long => (
                actual_entry - theoretical_risk,
                actual_entry + (theoretical_risk as f64 * rr) as i64,
            ),
            PositionSide::Short => (
                actual_entry + theoretical_risk,
                actual_entry - (theoretical_risk as f64 * rr) as i64,
            ),
        };
        let slippage = actual_entry - entry_price;
        info!(
            "{}: SL/TP 재계산 — actual entry {}원, R={}원, SL={}원, TP={}원, 슬리피지={}원",
            self.stock_name, actual_entry, theoretical_risk, actual_sl, actual_tp, slippage
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "order",
                "entry_executed",
                "info",
                &format!(
                    "{:?} {}주 지정가 진입 (지정가={}, 체결가={}, 슬리피지={})",
                    side, actual_qty, limit_price, actual_entry, slippage
                ),
                serde_json::json!({
                    "side": format!("{:?}", side),
                    "quantity": actual_qty,
                    "limit_price": limit_price,
                    "actual_price": actual_entry,
                    "slippage": slippage,
                    "stop_loss": actual_sl,
                    "take_profit": actual_tp,
                    "order_no": order_no,
                }),
            );
        }

        // TP 지정가 매도 발주 (실패 시 시장가 fallback)
        let (tp_order_no, tp_krx_orgno, tp_limit_price) = if let Some((no, orgno, placed_price)) =
            self.place_tp_limit_order(actual_tp, actual_qty).await
        {
            (Some(no), Some(orgno), Some(placed_price))
        } else {
            warn!(
                "{}: TP 지정가 발주 실패 — 시장가 fallback 모드",
                self.stock_name
            );
            (None, None, None)
        };

        // ActivePosition을 write lock 안에서 만들어 두고 lock 해제 후 DB 저장.
        // 같은 task에서 read+write를 동시에 잡으면 tokio RwLock에서 deadlock 발생.
        let ap_to_save = {
            let mut state = self.state.write().await;
            // 실제 진입을 발생시킨 OR stage의 범위를 상태/DB에 고정한다.
            state.or_high = Some(or_high);
            state.or_low = Some(or_low);
            state.current_position = Some(Position {
                side,
                entry_price: actual_entry,
                stop_loss: actual_sl,
                take_profit: actual_tp,
                entry_time,
                quantity: actual_qty,
                tp_order_no: tp_order_no.clone(),
                tp_krx_orgno: tp_krx_orgno.clone(),
                tp_limit_price,
                reached_1r: false,
                best_price: actual_entry,
                original_sl: actual_sl,
                sl_triggered_tick: false,
                intended_entry_price: entry_price,
                order_to_fill_ms,
                signal_id: signal_id.to_string(),
            });
            state.phase = "포지션 보유".to_string();

            self.db_store.as_ref().map(|_| ActivePosition {
                stock_code: self.stock_code.as_str().to_string(),
                side: format!("{:?}", side),
                entry_price: actual_entry,
                stop_loss: actual_sl,
                take_profit: actual_tp,
                quantity: actual_qty as i64,
                tp_order_no: tp_order_no.clone().unwrap_or_default(),
                tp_krx_orgno: tp_krx_orgno.clone().unwrap_or_default(),
                entry_time: entry_timestamp,
                original_sl: actual_sl,
                reached_1r: false,
                best_price: actual_entry,
                or_high: Some(or_high),
                or_low: Some(or_low),
                or_stage: Some(stage_name.to_string()),
                or_source: None,
                trigger_price: Some(entry_price),
                original_risk: Some(theoretical_risk),
                signal_id: Some(signal_id.to_string()),
                entry_mode: Some(entry_mode_label.to_string()),
                // 2026-04-17 긴급 follow-up: 신규 진입 시점은 manual 아님.
                // reconcile mismatch / trigger_manual_intervention 시 별도 경로
                // (`set_active_position_manual`) 로 true 로 전이.
                manual_intervention_required: false,
                // 2026-04-19 Doc revision 3-1: 진입 체결 완료 시점은 Open. close_position_market
                // 이 `mark_active_position_exit_pending` 으로 'exit_pending' 갱신.
                execution_state: "open".to_string(),
                last_exit_order_no: String::new(),
                last_exit_reason: String::new(),
                pending_entry_order_no: String::new(),
                pending_entry_krx_orgno: String::new(),
            })
        }; // write lock dropped here

        // DB save (lock 해제 후) — 3회 재시도.
        // 이 저장이 실패하면 재시작 시 포지션을 복구할 수 없어 SL/TP 미발동 → 실제 돈 손실 위험.
        if let (Some(store), Some(ap)) = (self.db_store.as_ref(), ap_to_save) {
            let mut last_err: Option<String> = None;
            let mut saved = false;
            for attempt in 0..3u32 {
                match store.save_active_position(&ap).await {
                    Ok(_) => {
                        info!(
                            "{}: 활성 포지션 DB 저장 완료{}",
                            self.stock_name,
                            if attempt > 0 {
                                format!(" — {}회 재시도 후 성공", attempt)
                            } else {
                                String::new()
                            }
                        );
                        saved = true;
                        break;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        warn!(
                            "{}: 활성 포지션 DB 저장 실패 (시도 {}/3): {msg}",
                            self.stock_name,
                            attempt + 1
                        );
                        last_err = Some(msg);
                        if attempt < 2 {
                            let delay = 500u64 * (1u64 << attempt);
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                    }
                }
            }
            if !saved {
                let err_msg = last_err.unwrap_or_else(|| "unknown".into());
                error!(
                    "{}: 활성 포지션 DB 저장 최종 실패 — 재시작 시 복구 불가: {err_msg}",
                    self.stock_name
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "storage",
                        "save_active_position_failed",
                        "critical",
                        &format!("활성 포지션 저장 3회 실패: {err_msg}"),
                        serde_json::json!({
                            "error": err_msg,
                            "position": {
                                "side": ap.side,
                                "entry_price": ap.entry_price,
                                "stop_loss": ap.stop_loss,
                                "take_profit": ap.take_profit,
                                "quantity": ap.quantity,
                                "tp_order_no": ap.tp_order_no,
                                "entry_time": ap.entry_time.to_string(),
                            }
                        }),
                    );
                }
            }
        }

        // 포지션 잠금은 poll_and_enter에서 CAS로 이미 설정됨

        // 2026-04-16 실전 Go 조건 #2: 전역 일일 거래 카운터 기록.
        // 매수 체결 + DB 저장까지 끝난 시점에서 1회로 카운트 — 이후 청산 결과와 무관하게
        // 신규 진입은 전역 한도 아래에서만 허용된다.
        if let Some(ref gate) = self.live_cfg.global_trade_gate {
            let today = Local::now().date_naive();
            let mut g = gate.write().await;
            g.record_trade(today);
            let (count, max) = g.snapshot(today);
            info!(
                "{}: 전역 거래 카운터 +1 → {}/{}",
                self.stock_name, count, max
            );
        }

        // Phase 2: 진입 체결 + DB 저장 + 공유 lock 전환 완료 → Open 전이.
        // 이전 상태는 Flat (정상 진입) 또는 EntryPending (Phase 3/4 에서 poll_and_enter 에서 전이).
        self.transition_execution_state(ExecutionState::Open, "execute_entry_filled")
            .await;

        // 2026-04-18 Review Finding #5: execution_journal entry_final_fill 기록.
        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: execution_id.clone(),
                signal_id: signal_id.to_string(),
                broker_order_id: order_no.clone(),
                phase: "entry_final_fill".into(),
                side: format!("{:?}", side),
                intended_price: Some(entry_price),
                submitted_price: Some(limit_price),
                filled_price: Some(actual_entry),
                filled_qty: Some(actual_qty as i64),
                holdings_before: Some(holding_before as i64),
                holdings_after: Some((holding_before + actual_qty) as i64),
                resolution_source: String::new(),
                reason: "execute_entry_filled".into(),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "stage": stage_name,
                    "or_high": or_high,
                    "or_low": or_low,
                    "entry_mode": entry_mode_label,
                    "slippage": slippage,
                    "order_to_fill_ms": order_to_fill_ms,
                    "actual_sl": actual_sl,
                    "actual_tp": actual_tp,
                }),
                sent_at: Some(submit_sent_at),
                ack_at: Some(submit_ack_at),
                first_fill_at: Some(Utc::now()),
                final_fill_at: Some(Utc::now()),
            };
            if let Err(e) = store.append_execution_journal(&entry).await {
                warn!(
                    "{}: execution_journal 저장 실패 (entry_final_fill): {e}",
                    self.stock_name
                );
            }
        }

        Ok(())
    }

    /// 2026-04-18 Phase 1: 청산 후 WS → REST → balance 3단 검증.
    ///
    /// `close_position_market` 이 시장가 주문을 성공적으로 제출한 뒤 이 함수를 호출한다.
    /// 반환된 `ExitVerification.can_flat()` 이 `true` 인 경우에만 `Flat` 전이가 허용된다
    /// (불변식 2: 보유수량 0 확인 후에만 Flat).
    ///
    /// 각 단계:
    /// 1. WS 체결통보 대기 (최대 3초) — 빠른 경로 확인
    /// 2. REST 체결조회 (최대 3회, 1초 간격) — WS 누락 대비
    /// 3. balance 재확인 (최대 3회, 1초 간격) — 최종 진실 기준
    ///
    /// balance 감소로 역산된 체결이면 `resolution_source = BalanceOnly`, 체결가는
    /// `None` 으로 반환하여 호출자가 `fill_price_unknown=true` 로 기록하도록 한다.
    async fn verify_exit_completion(
        &self,
        order_no: &str,
        expected_qty: u64,
        holding_before: u64,
    ) -> ExitVerification {
        let mut filled_qty: u64 = 0;
        let mut filled_price: Option<i64> = None;
        let mut resolution_source = ExitResolutionSource::Unresolved;
        let mut ws_received = false;
        let mut rest_execution_attempts: Vec<ExecutionQueryDiagnostics> = Vec::new();

        // 2026-04-19 Doc revision (Phase 3): timeout 세부 값을 LiveRunnerConfig 참조.
        // exit_ws_notice_timeout_ms / exit_rest_retries / exit_balance_retries 등.
        let ws_timeout = std::time::Duration::from_millis(
            self.live_cfg.exit_ws_notice_timeout_ms.clamp(500, 30_000),
        );
        let rest_retries = self.live_cfg.exit_rest_retries.clamp(1, 10);
        let rest_interval = std::time::Duration::from_millis(
            self.live_cfg.exit_rest_retry_interval_ms.clamp(200, 5_000),
        );
        let balance_retries = self.live_cfg.exit_balance_retries.clamp(1, 10);
        let balance_interval = std::time::Duration::from_millis(
            self.live_cfg
                .exit_balance_retry_interval_ms
                .clamp(200, 5_000),
        );

        // Step 1: WS 체결통보 대기 (config 제어)
        if !order_no.is_empty() {
            if let Some((price, qty)) = self.wait_execution_notice(order_no, ws_timeout).await {
                filled_price = Some(price);
                filled_qty = qty;
                resolution_source = ExitResolutionSource::WsNotice;
                ws_received = true;
            } else {
                info!(
                    "{}: 청산 WS 체결통보 {}ms 미수신 — REST/잔고 fallback",
                    self.stock_name,
                    ws_timeout.as_millis()
                );
            }
        }

        // Step 2: REST 체결조회 (config 제어 — retries + interval)
        if filled_qty < expected_qty && !order_no.is_empty() {
            for attempt in 0..rest_retries {
                if attempt > 0 {
                    tokio::time::sleep(rest_interval).await;
                }
                let outcome = self
                    .query_execution_detailed(order_no, "pre_balance", attempt + 1)
                    .await;
                let fill = outcome.fill;
                rest_execution_attempts.push(outcome.diagnostics);
                if let Some((price, qty)) = fill {
                    let normalized_qty = qty.min(expected_qty);
                    if normalized_qty > filled_qty {
                        filled_qty = normalized_qty;
                        if filled_price.is_none() {
                            filled_price = Some(price);
                            resolution_source = ExitResolutionSource::RestExecution;
                        }
                    }
                    if filled_qty >= expected_qty {
                        break;
                    }
                }
            }
        }

        // Step 3: balance 재확인 (config 제어) — Flat 판정의 진실 기준
        let mut holdings_after: Option<u64> = None;
        for attempt in 0..balance_retries {
            if attempt > 0 {
                tokio::time::sleep(balance_interval).await;
            }
            match self.fetch_holding_snapshot().await {
                Ok(Some((qty, _avg))) => {
                    holdings_after = Some(qty);
                    if qty == 0 {
                        break; // 빠른 종료
                    }
                    // 아직 감소 미반영 가능 — 다음 루프 재시도
                }
                Ok(None) => {
                    holdings_after = Some(0);
                    break;
                }
                Err(e) => {
                    warn!(
                        "{}: 청산 후 잔고 조회 실패 ({}/{}): {e}",
                        self.stock_name,
                        attempt + 1,
                        balance_retries
                    );
                }
            }
        }

        // Step 3.5: balance 가 0을 확인한 뒤에도 체결가가 없으면 REST 를 한 번 더 본다.
        // 2026-04-24 관측처럼 시장가 청산 체결이 늦게 발생하면, pre-balance REST 3회가
        // 체결 전 시점에 모두 소진되고 balance 만 0을 확인해 `balance_only` 로 떨어질 수 있다.
        if filled_price.is_none() && matches!(holdings_after, Some(0)) && !order_no.is_empty() {
            for attempt in 0..rest_retries {
                if attempt > 0 {
                    tokio::time::sleep(rest_interval).await;
                }
                let outcome = self
                    .query_execution_detailed(order_no, "post_balance_flat", attempt + 1)
                    .await;
                let fill = outcome.fill;
                rest_execution_attempts.push(outcome.diagnostics);
                if let Some((price, qty)) = fill {
                    let normalized_qty = qty.min(expected_qty);
                    if normalized_qty > filled_qty {
                        filled_qty = normalized_qty;
                    }
                    filled_price = Some(price);
                    resolution_source = ExitResolutionSource::RestExecution;
                    break;
                }
            }
        }

        // WS/REST 모두 누락됐지만 실제 잔고가 감소했으면 BalanceOnly 로 역산
        if filled_price.is_none()
            && filled_qty == 0
            && let Some(after) = holdings_after
            && after < holding_before
        {
            filled_qty = holding_before - after;
            resolution_source = ExitResolutionSource::BalanceOnly;
            // filled_price 는 여전히 None → fill_price_unknown
        }

        ExitVerification {
            filled_qty,
            filled_price,
            holdings_after,
            resolution_source,
            ws_received,
            rest_execution_attempts,
        }
    }

    async fn verify_exit_completion_with_actor_timeout(
        &self,
        timeout_state: ExecutionState,
        order_no: &str,
        expected_qty: u64,
        holding_before: u64,
        timeout_reason: &str,
    ) -> Result<ExitVerification, KisError> {
        let Some((phase, timeout)) = self.execution_timeout_for_state(timeout_state) else {
            return Ok(self
                .verify_exit_completion(order_no, expected_qty, holding_before)
                .await);
        };
        match tokio::time::timeout(
            timeout,
            self.verify_exit_completion(order_no, expected_qty, holding_before),
        )
        .await
        {
            Ok(verification) => Ok(verification),
            Err(_) => {
                self.handle_execution_timeout_expired(phase, timeout_reason)
                    .await;
                Err(KisError::ManualIntervention(format!(
                    "청산 검증 timeout: {timeout_reason}"
                )))
            }
        }
    }

    /// 2026-04-18 Phase 1: 청산 후 잔량이 남아있을 때의 처리.
    ///
    /// 검증 결과 `holdings_after != Some(0)` 인 경우 호출된다.
    /// - `Some(q>0)`: q 주 만큼 시장가 재청산 1회 시도 → 재검증 → 여전히 잔존 시 manual
    /// - `None` (balance 조회 실패): **자동 추정 복구 금지** (불변식 5) → 바로 manual
    ///
    /// 반환:
    /// - `Ok((result, meta))`: 재청산 후 holdings=0 확인 → flat 전이 (체결가 미확인이므로 price_unknown)
    /// - `Err(ManualIntervention)`: 재청산 실패 또는 여전히 잔존 → manual_intervention 전환
    async fn handle_exit_remaining(
        &self,
        pos: Position,
        reason: ExitReason,
        execution_id: &str,
        order_no: &str,
        verification: &ExitVerification,
    ) -> Result<(TradeResult, ExitTraceMeta), KisError> {
        let signal_id = pos.signal_id.clone();

        // 불변식 5: balance 조회 실패 시 자동 추정 복구 금지
        let remaining = match verification.holdings_after {
            Some(q) if q > 0 => q,
            None => {
                error!(
                    "{}: 청산 후 잔고 조회 실패 — 자동 재청산 금지, manual_intervention 전환",
                    self.stock_name
                );
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: execution_id.to_string(),
                        signal_id: signal_id.clone(),
                        broker_order_id: order_no.to_string(),
                        phase: "exit_balance_lookup_failed_manual".into(),
                        side: format!("{:?}", pos.side),
                        intended_price: Some(pos.entry_price),
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: Some(verification.filled_qty as i64),
                        holdings_before: None,
                        holdings_after: None,
                        resolution_source: verification.resolution_source.label().into(),
                        reason: format!("balance_lookup_failed:{:?}", reason),
                        error_message: String::new(),
                        metadata: serde_json::json!({
                            "order_no": order_no,
                            "ws_received": verification.ws_received,
                            "filled_qty_hint": verification.filled_qty,
                            "rest_execution_attempts": verification.rest_execution_attempts.len(),
                            "rest_execution_last_status": verification.rest_execution_last_status(),
                            "rest_execution_diagnostics": verification.rest_execution_diagnostics_json(),
                            "exit_reason": format!("{:?}", reason),
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                self.trigger_manual_intervention(
                    format!(
                        "청산 후 잔고 조회 실패 ({:?}) — 재청산 시 중복 매도 위험",
                        reason
                    ),
                    serde_json::json!({
                        "path": "close_position_market.handle_exit_remaining",
                        "reason": format!("{:?}", reason),
                        "order_no": order_no,
                        "ws_received": verification.ws_received,
                        "filled_qty_hint": verification.filled_qty,
                        "rest_execution_attempts": verification.rest_execution_attempts.len(),
                        "rest_execution_last_status": verification.rest_execution_last_status(),
                        "rest_execution_diagnostics": verification.rest_execution_diagnostics_json(),
                        "balance_lookup": "failed",
                    }),
                    ManualInterventionMode::KeepLock,
                )
                .await;
                return Err(KisError::ManualIntervention(
                    "청산 후 잔고 조회 실패".into(),
                ));
            }
            Some(_) => unreachable!("can_flat=true 경로는 상위에서 분기"),
        };

        self.transition_execution_state(ExecutionState::ExitPartial, "exit_remaining_retry")
            .await;

        warn!(
            "{}: 청산 후 보유 {}주 잔존 (source={}) — 시장가 재청산 1회 시도",
            self.stock_name,
            remaining,
            verification.resolution_source.label()
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "position",
                "exit_remaining_retry",
                "warn",
                &format!("청산 후 잔량 {}주 — 재청산 시도", remaining),
                serde_json::json!({
                    "reason": format!("{:?}", reason),
                    "remaining": remaining,
                    "order_no": order_no,
                    "resolution_source": verification.resolution_source.label(),
                    "filled_qty_hint": verification.filled_qty,
                    "rest_execution_attempts": verification.rest_execution_attempts.len(),
                    "rest_execution_last_status": verification.rest_execution_last_status(),
                }),
            );
        }

        if matches!(pos.side, PositionSide::Short) {
            error!(
                "{}: Short 포지션 잔량 {}주 재청산 경로 미구현 — manual_intervention",
                self.stock_name, remaining
            );
            self.trigger_manual_intervention(
                format!("Short 청산 잔량 {}주 — 수동 처리 필요", remaining),
                serde_json::json!({
                    "path": "close_position_market.handle_exit_remaining",
                    "reason": format!("{:?}", reason),
                    "side": "Short",
                    "remaining": remaining,
                }),
                ManualInterventionMode::KeepLock,
            )
            .await;
            return Err(KisError::ManualIntervention(
                "Short 청산 잔량 수동 처리".into(),
            ));
        }

        let retry_order_no = match self.place_market_sell(remaining).await {
            Ok(order_no) => order_no,
            Err(e) => {
                error!(
                    "{}: 재청산 주문 실패 — manual_intervention 전환: {e}",
                    self.stock_name
                );
                if let Some(ref store) = self.db_store {
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: execution_id.to_string(),
                        signal_id: signal_id.clone(),
                        broker_order_id: String::new(),
                        phase: "exit_retry_submit_failed".into(),
                        side: format!("{:?}", pos.side),
                        intended_price: Some(pos.entry_price),
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: Some(verification.filled_qty as i64),
                        holdings_before: Some(remaining as i64),
                        holdings_after: None,
                        resolution_source: "unresolved".into(),
                        reason: format!("retry_submit_failed:{:?}", reason),
                        error_message: e.to_string(),
                        metadata: serde_json::json!({
                            "prior_order_no": order_no,
                            "remaining": remaining,
                            "exit_reason": format!("{:?}", reason),
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                self.trigger_manual_intervention(
                    format!(
                        "청산 잔량 {}주 — 재청산 주문 실패: {} ({:?})",
                        remaining, e, reason
                    ),
                    serde_json::json!({
                        "path": "close_position_market.handle_exit_remaining",
                        "reason": format!("{:?}", reason),
                        "prior_order_no": order_no,
                        "remaining": remaining,
                        "retry_error": e.to_string(),
                    }),
                    ManualInterventionMode::KeepLock,
                )
                .await;
                return Err(KisError::ManualIntervention(format!(
                    "재청산 주문 실패: {e}"
                )));
            }
        };

        let retry_verification = self
            .verify_exit_completion_with_actor_timeout(
                ExecutionState::ExitPartial,
                &retry_order_no,
                remaining,
                remaining,
                "exit_remaining_retry_verify_timeout",
            )
            .await?;

        if retry_verification.has_remaining() {
            let still_held = retry_verification.holdings_after.unwrap_or(remaining);
            error!(
                "{}: 재청산 후에도 {}주 잔존 — manual_intervention 전환",
                self.stock_name, still_held
            );
            if let Some(ref store) = self.db_store {
                let entry = ExecutionJournalEntry {
                    stock_code: self.stock_code.as_str().to_string(),
                    execution_id: execution_id.to_string(),
                    signal_id: signal_id.clone(),
                    broker_order_id: retry_order_no.clone(),
                    phase: "exit_retry_still_remaining_manual".into(),
                    side: format!("{:?}", pos.side),
                    intended_price: Some(pos.entry_price),
                    submitted_price: None,
                    filled_price: retry_verification.filled_price,
                    filled_qty: Some(retry_verification.filled_qty as i64),
                    holdings_before: Some(remaining as i64),
                    holdings_after: Some(still_held as i64),
                    resolution_source: retry_verification.resolution_source.label().into(),
                    reason: format!("retry_still_remaining:{:?}", reason),
                    error_message: String::new(),
                    metadata: serde_json::json!({
                        "prior_order_no": order_no,
                        "retry_order_no": retry_order_no.clone(),
                        "initial_remaining": remaining,
                        "after_retry": retry_verification.holdings_after,
                        "retry_ws_received": retry_verification.ws_received,
                        "exit_reason": format!("{:?}", reason),
                    }),
                    sent_at: None,
                    ack_at: None,
                    first_fill_at: None,
                    final_fill_at: None,
                };
                let _ = store.append_execution_journal(&entry).await;
            }
            self.trigger_manual_intervention(
                format!(
                    "청산 잔량 {}주 — 재청산 후에도 해결되지 않음 ({:?})",
                    still_held, reason
                ),
                serde_json::json!({
                    "path": "close_position_market.handle_exit_remaining",
                    "reason": format!("{:?}", reason),
                    "prior_order_no": order_no,
                    "retry_order_no": retry_order_no,
                    "initial_remaining": remaining,
                    "after_retry": retry_verification.holdings_after,
                }),
                ManualInterventionMode::KeepLock,
            )
            .await;
            return Err(KisError::ManualIntervention(format!(
                "청산 잔량 {}주 — 재청산 후에도 남음",
                still_held
            )));
        }

        let (exit_price, exit_meta) = match retry_verification.filled_price {
            Some(price) => (
                price,
                ExitTraceMeta::resolved(retry_verification.resolution_source),
            ),
            None => {
                let ws_price = self.get_current_price().await.unwrap_or(pos.entry_price);
                warn!(
                    "{}: 재청산 성공 (holdings=0 확인) — 체결가 미확인, 현재가 {}원 fallback 기록",
                    self.stock_name, ws_price
                );
                (
                    ws_price,
                    ExitTraceMeta::price_unknown(retry_verification.resolution_source),
                )
            }
        };

        let exit_time = Local::now().time();
        let result = TradeResult {
            side: pos.side,
            entry_price: pos.entry_price,
            exit_price,
            stop_loss: pos.stop_loss,
            take_profit: pos.take_profit,
            entry_time: pos.entry_time,
            exit_time,
            exit_reason: reason,
            quantity: pos.quantity,
            intended_entry_price: pos.intended_entry_price,
            order_to_fill_ms: pos.order_to_fill_ms,
        };

        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: execution_id.to_string(),
                signal_id,
                broker_order_id: retry_order_no.clone(),
                phase: "exit_retry_final_fill".into(),
                side: format!("{:?}", pos.side),
                intended_price: Some(pos.entry_price),
                submitted_price: None,
                filled_price: Some(exit_price),
                filled_qty: Some(retry_verification.filled_qty as i64),
                holdings_before: Some(remaining as i64),
                holdings_after: Some(0),
                resolution_source: exit_meta.exit_resolution_source.clone(),
                reason: format!("retry_final_fill:{:?}", reason),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "prior_order_no": order_no,
                    "retry_order_no": retry_order_no,
                    "fill_price_unknown": exit_meta.fill_price_unknown,
                    "initial_remaining": remaining,
                    "retry_ws_received": retry_verification.ws_received,
                    "exit_reason": format!("{:?}", reason),
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: Some(Utc::now()),
                final_fill_at: Some(Utc::now()),
            };
            let _ = store.append_execution_journal(&entry).await;
        }

        self.finalize_flat_transition().await;
        self.notify_trade("exit");
        Ok((result, exit_meta))
    }

    /// 2026-04-18 Phase 1: Flat 전이 확정 시 state/DB/lock 정리 (공용 경로).
    ///
    /// Phase 2 추가: `execution_state` 를 `Flat` 으로 전이하며 이벤트 로그 기록.
    /// PR #5.c 변경: holdings=0 확인 후에만 이 함수에 도달한다는 불변식 (#2) 을
    /// actor event 로 표현 — `BalanceReconciled { holdings_qty: 0 }` dispatch.
    /// actor rule 에서 `ExitPending/ExitPartial + BalanceReconciled{0}` → `Flat`.
    /// manual_intervention 상태에서는 이 함수에 도달하지 않는다 (holdings=0 확인 + !manual 경로만).
    async fn finalize_flat_transition(&self) {
        {
            let mut state = self.state.write().await;
            state.current_position = None;
            state.exit_pending = false;
            state.phase = "신호 탐색".to_string();
        } // state lock 해제

        // Phase 2 + PR #5.c: execution_state 전이를 actor event 로 위임.
        // idempotent — 이미 Flat 이면 `apply_execution_event` 가 no-op transition
        // 을 반환하고 `transition_execution_state` early return.
        // caller 가 exit 경로에서만 호출하므로 prev state 는 항상 ExitPending/ExitPartial.
        self.dispatch_execution_event(
            crate::strategy::execution_actor::ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
            "flat_transition",
        )
        .await;

        if let Some(ref store) = self.db_store {
            let _ = store.delete_active_position(self.stock_code.as_str()).await;
        }

        if let Some(ref lock) = self.position_lock {
            // 2026-04-17 v3 fixups H: 자기 lock 만 풀기.
            release_lock_if_self_held(lock, self.stock_code.as_str()).await;
            info!(
                "{}: 포지션 잠금 해제 (다른 종목 진입 허용)",
                self.stock_name
            );
        }
    }

    /// 시장가 청산 — Phase 1 (2026-04-18): ExitPending 방식.
    ///
    /// 변경 사항 (이전: 주문 제출 직후 `current_position.take()` + 체결가 fallback 으로 flat):
    /// - 주문 제출 전 `state.exit_pending = true` 로 표시 (불변식 4: pending 중 추가 주문 금지)
    /// - `current_position` 은 **clone 만** — 보유수량 0 이 확인되기 전까지 state 에 유지 (불변식 2)
    /// - WS / REST / balance 3 단 검증 후 holdings=0 확인 시에만 Flat 전이
    /// - holdings 잔존 시 재청산 1회 → 재검증 → 여전히 남으면 manual_intervention
    /// - balance 조회 실패 (`None`) 는 **자동 추정 금지** (불변식 5) → 즉시 manual
    /// - 체결가 미확인 but holdings=0 이면 현재가 fallback + `fill_price_unknown=true` 기록
    async fn close_position_market(
        &self,
        reason: ExitReason,
    ) -> Result<(TradeResult, ExitTraceMeta), KisError> {
        // Step 1: 포지션 확보 (clone — take 하지 않음) + exit_pending 가드 설정
        let pos: Position = {
            let mut state = self.state.write().await;
            // 불변식 4: 이미 다른 경로가 청산 진행 중이면 중복 진입 금지.
            if state.exit_pending {
                return Err(KisError::Internal(
                    "이미 청산 진행 중 (exit_pending)".into(),
                ));
            }
            let cloned = match state.current_position.as_ref() {
                Some(p) => p.clone(),
                None => return Err(KisError::Internal("청산할 포지션 없음".into())),
            };
            state.exit_pending = true;
            cloned
        };
        // Phase 2 + PR #5.c: execution_state 전이를 actor event 로 위임.
        // actor rule: Open + ExitRequested → ExitPending, commands: [SubmitExitOrder].
        // SubmitExitOrder 는 현재 intent 로만 기록되고 실제 시장가 주문은 아래 단계가 수행.
        // 아주 짧은 window 동안 (exit_pending=true, execution_state=Open) 이 관찰될 수 있으나,
        // 읽는 쪽이 두 필드를 동시 의존하는 경로는 없어 실용적으로 허용.
        let exit_reason_label = format!("{:?}", reason);
        self.dispatch_execution_event(
            crate::strategy::execution_actor::ExecutionEvent::ExitRequested {
                reason: exit_reason_label.clone(),
            },
            "close_position_market",
        )
        .await;
        let execution_id = self.position_execution_id(&pos);

        // Step 2: TP 지정가 취소 (실패해도 시장가 청산 진행)
        if let (Some(no), Some(orgno)) = (&pos.tp_order_no, &pos.tp_krx_orgno)
            && let Err(e) = self.cancel_tp_order(no, orgno).await
        {
            warn!("{}: TP 취소 실패 (시장가 청산 진행): {e}", self.stock_name);
        }

        // Step 3: 청산 전 보유 수량 스냅샷 (체결 역산용).
        // 조회 실패 시 pos.quantity 를 fallback — 청산 완료 후 after=0 이면 차이가
        // quantity 와 동일해 balance-only 경로 역산이 여전히 유효.
        let holding_before: u64 = match self.fetch_holding_snapshot().await {
            Ok(Some((qty, _))) => qty,
            Ok(None) => 0,
            Err(e) => {
                warn!(
                    "{}: 청산 전 잔고 조회 실패 — pos.quantity 를 fallback 으로 사용: {e}",
                    self.stock_name
                );
                pos.quantity
            }
        };

        // Step 4: 시장가 주문 제출
        let order_side = match pos.side {
            PositionSide::Long => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
        };

        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01",
            "ORD_QTY": pos.quantity.to_string(),
            "ORD_UNPR": "0",
        });

        let tr_id = match order_side {
            OrderSide::Buy => TransactionId::OrderCashBuy,
            OrderSide::Sell => TransactionId::OrderCashSell,
        };

        // 2026-04-18 Review round (Finding #2/#6): 주문 실패 공용 처리.
        //
        // 이전 구현은 exit_pending 만 해제하고 Err 반환했는데, TP 지정가는 이미 위에서 취소된
        // 상태라 포지션이 **보호 없는 상태로 남는 치명적 결함**이었다. manage_position 은
        // pos.tp_order_no 가 여전히 있다고 보고 TP 체결 조회만 반복하고, execution_state 는
        // ExitPending 그대로 남아있다.
        //
        // 수정: 주문 실패 시 ①pos.tp_* 메타 제거 (manage_position 오판 차단) ②exit_pending 해제
        // ③manual_intervention 전환 (execution_state = ManualIntervention, stop_flag, shared lock
        // KeepLock). 자동 재청산/재발주 금지 — 운영자 HTS 수동 확인 필수.
        let clear_tp_and_exit_pending = || async {
            let mut s = self.state.write().await;
            s.exit_pending = false;
            if let Some(ref mut p) = s.current_position {
                p.tp_order_no = None;
                p.tp_krx_orgno = None;
                p.tp_limit_price = None;
            }
            drop(s);
            if let Some(ref store) = self.db_store {
                let _ = store
                    .clear_active_position_tp_metadata(self.stock_code.as_str())
                    .await;
            }
        };

        let resp: KisResponse<serde_json::Value> = match self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &tr_id,
                None,
                Some(&body),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!(
                    "{}: 청산 주문 네트워크 오류 + TP 취소된 상태 — manual_intervention 전환: {e}",
                    self.stock_name
                );
                if let Some(ref store) = self.db_store {
                    store
                        .save_order_log(
                            self.stock_code.as_str(),
                            "청산",
                            &format!("{:?}", order_side),
                            pos.quantity as i64,
                            0,
                            "",
                            "네트워크오류",
                            &e.to_string(),
                        )
                        .await;
                    let entry = ExecutionJournalEntry {
                        stock_code: self.stock_code.as_str().to_string(),
                        execution_id: execution_id.clone(),
                        signal_id: pos.signal_id.clone(),
                        broker_order_id: String::new(),
                        phase: "exit_submit_failed".into(),
                        side: format!("{:?}", pos.side),
                        intended_price: Some(pos.entry_price),
                        submitted_price: None,
                        filled_price: None,
                        filled_qty: None,
                        holdings_before: Some(holding_before as i64),
                        holdings_after: None,
                        resolution_source: "unresolved".into(),
                        reason: format!("close_market_network_error:{:?}", reason),
                        error_message: e.to_string(),
                        metadata: serde_json::json!({
                            "tp_was_cancelled": pos.tp_order_no.is_some(),
                            "quantity": pos.quantity,
                            "exit_reason": format!("{:?}", reason),
                        }),
                        sent_at: None,
                        ack_at: None,
                        first_fill_at: None,
                        final_fill_at: None,
                    };
                    let _ = store.append_execution_journal(&entry).await;
                }
                clear_tp_and_exit_pending().await;
                self.trigger_manual_intervention(
                    format!(
                        "청산 주문 네트워크 오류 + TP 취소된 상태 ({:?}): {}",
                        reason, e
                    ),
                    serde_json::json!({
                        "path": "close_position_market.submit_network_error",
                        "reason": format!("{:?}", reason),
                        "tp_was_cancelled": pos.tp_order_no.is_some(),
                        "error": e.to_string(),
                    }),
                    ManualInterventionMode::KeepLock,
                )
                .await;
                return Err(e);
            }
        };

        if resp.rt_cd != "0" {
            error!(
                "{}: 청산 주문 거부 + TP 취소된 상태 — manual_intervention 전환: {}",
                self.stock_name, resp.msg1
            );
            if let Some(ref store) = self.db_store {
                store
                    .save_order_log(
                        self.stock_code.as_str(),
                        "청산",
                        &format!("{:?}", order_side),
                        pos.quantity as i64,
                        0,
                        "",
                        "거부",
                        &resp.msg1,
                    )
                    .await;
                let entry = ExecutionJournalEntry {
                    stock_code: self.stock_code.as_str().to_string(),
                    execution_id: execution_id.clone(),
                    signal_id: pos.signal_id.clone(),
                    broker_order_id: String::new(),
                    phase: "exit_submit_rejected".into(),
                    side: format!("{:?}", pos.side),
                    intended_price: Some(pos.entry_price),
                    submitted_price: None,
                    filled_price: None,
                    filled_qty: None,
                    holdings_before: Some(holding_before as i64),
                    holdings_after: None,
                    resolution_source: "unresolved".into(),
                    reason: format!("close_market_rejected:{:?}", reason),
                    error_message: resp.msg1.clone(),
                    metadata: serde_json::json!({
                        "tp_was_cancelled": pos.tp_order_no.is_some(),
                        "quantity": pos.quantity,
                        "exit_reason": format!("{:?}", reason),
                        "rt_cd": resp.rt_cd,
                        "msg_cd": resp.msg_cd,
                    }),
                    sent_at: None,
                    ack_at: None,
                    first_fill_at: None,
                    final_fill_at: None,
                };
                let _ = store.append_execution_journal(&entry).await;
            }
            clear_tp_and_exit_pending().await;
            let err =
                KisError::classify(resp.rt_cd.clone(), resp.msg_cd.clone(), resp.msg1.clone());
            self.trigger_manual_intervention(
                format!(
                    "청산 주문 거부 + TP 취소된 상태 ({:?}): {}",
                    reason, resp.msg1
                ),
                serde_json::json!({
                    "path": "close_position_market.submit_rejected",
                    "reason": format!("{:?}", reason),
                    "tp_was_cancelled": pos.tp_order_no.is_some(),
                    "rt_cd": resp.rt_cd,
                    "msg_cd": resp.msg_cd,
                    "msg1": resp.msg1,
                }),
                ManualInterventionMode::KeepLock,
            )
            .await;
            return Err(err);
        }

        // Step 5: 주문번호 추출
        let order_no = resp
            .output
            .as_ref()
            .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
            .unwrap_or("")
            .to_string();
        info!(
            "{}: 청산 시장가 발주 — {}주 (주문번호={}, 사유={:?})",
            self.stock_name, pos.quantity, order_no, reason
        );

        // 2026-04-19 Doc revision 3-1: DB 에 ExitPending 상태 마킹.
        // 재시작 복구 시 `check_and_restore_position` 이 이 값을 보고 verify 재시도 또는
        // manual_intervention 분기를 결정한다.
        if let Some(ref store) = self.db_store {
            let reason_label = format!("{:?}", reason);
            if let Err(e) = store
                .mark_active_position_exit_pending(
                    self.stock_code.as_str(),
                    &order_no,
                    &reason_label,
                )
                .await
            {
                warn!(
                    "{}: exit_pending DB 마킹 실패 (재시작 복구 정확도 저하 가능): {e}",
                    self.stock_name
                );
            }
        }

        // Step 6: WS → REST → balance 3단 검증 (불변식 2)
        let verification = self
            .verify_exit_completion_with_actor_timeout(
                ExecutionState::ExitPending,
                &order_no,
                pos.quantity,
                holding_before,
                "close_position_market_verify_timeout",
            )
            .await?;
        info!(
            "{}: 청산 검증 결과 — source={}, filled_qty={}, filled_price={:?}, holdings_after={:?}, ws_received={}",
            self.stock_name,
            verification.resolution_source.label(),
            verification.filled_qty,
            verification.filled_price,
            verification.holdings_after,
            verification.ws_received,
        );

        // Step 7: 결과 분기 — `has_remaining()` 은 holdings>0 또는 조회 실패 모두 포함.
        // 조회 실패는 자동 추정 금지(불변식 5)로 handle_exit_remaining 안에서 즉시 manual.
        if verification.has_remaining() {
            return self
                .handle_exit_remaining(pos, reason, &execution_id, &order_no, &verification)
                .await;
        }

        // holdings=0 확인됨 → Flat 전이 허용
        let (exit_price, exit_meta): (i64, ExitTraceMeta) = match verification.filled_price {
            Some(price) => (
                price,
                ExitTraceMeta::resolved(verification.resolution_source),
            ),
            None => {
                // 체결가 미확인 but holdings=0 — fill_price_unknown 기록 후 현재가 fallback
                let ws_price = self.get_current_price().await.unwrap_or(pos.entry_price);
                error!(
                    "{}: 청산 체결가 미확인 (holdings=0 확인) — 현재가 {}원 fallback, fill_price_unknown=true",
                    self.stock_name, ws_price
                );
                (
                    ws_price,
                    ExitTraceMeta::price_unknown(verification.resolution_source),
                )
            }
        };
        let exit_time = Local::now().time();

        // 청산 성공 로그
        if let Some(ref store) = self.db_store {
            store
                .save_order_log(
                    self.stock_code.as_str(),
                    &format!("청산({:?})", reason),
                    &format!("{:?}", order_side),
                    pos.quantity as i64,
                    exit_price,
                    &order_no,
                    "체결",
                    "",
                )
                .await;
        }

        info!(
            "{}: {:?} 청산 — {}주 @ {} ({:?}, source={})",
            self.stock_name,
            pos.side,
            pos.quantity,
            exit_price,
            reason,
            exit_meta.exit_resolution_source,
        );
        if let Some(ref el) = self.event_logger {
            let pnl_pct = match pos.side {
                PositionSide::Long => {
                    (exit_price - pos.entry_price) as f64 / pos.entry_price as f64 * 100.0
                }
                PositionSide::Short => {
                    (pos.entry_price - exit_price) as f64 / pos.entry_price as f64 * 100.0
                }
            };
            el.log_event(
                self.stock_code.as_str(),
                "position",
                "exit_market",
                "info",
                &format!(
                    "{:?} 청산 — {}주 @ {} ({:?})",
                    pos.side, pos.quantity, exit_price, reason
                ),
                serde_json::json!({
                    "reason": format!("{:?}", reason),
                    "entry_price": pos.entry_price,
                    "exit_price": exit_price,
                    "quantity": pos.quantity,
                    "pnl_pct": format!("{:.2}", pnl_pct),
                    "stop_loss": pos.stop_loss,
                    "take_profit": pos.take_profit,
                    "fill_price_unknown": exit_meta.fill_price_unknown,
                    "exit_resolution_source": exit_meta.exit_resolution_source,
                    "ws_received": verification.ws_received,
                    "holdings_after": verification.holdings_after,
                    "rest_execution_attempts": verification.rest_execution_attempts.len(),
                    "rest_execution_last_status": verification.rest_execution_last_status(),
                    "rest_execution_diagnostics": verification.rest_execution_diagnostics_json(),
                }),
            );
        }
        self.notify_trade("exit");

        let result = TradeResult {
            side: pos.side,
            entry_price: pos.entry_price,
            exit_price,
            stop_loss: pos.stop_loss,
            take_profit: pos.take_profit,
            entry_time: pos.entry_time,
            exit_time,
            exit_reason: reason,
            quantity: pos.quantity,
            intended_entry_price: pos.intended_entry_price,
            order_to_fill_ms: pos.order_to_fill_ms,
        };

        // 2026-04-18 Review Finding #5: execution_journal exit_final_fill 기록.
        if let Some(ref store) = self.db_store {
            let entry = ExecutionJournalEntry {
                stock_code: self.stock_code.as_str().to_string(),
                execution_id: execution_id.clone(),
                signal_id: pos.signal_id.clone(),
                broker_order_id: order_no.clone(),
                phase: "exit_final_fill".into(),
                side: format!("{:?}", pos.side),
                intended_price: Some(pos.entry_price),
                submitted_price: None,
                filled_price: Some(exit_price),
                filled_qty: Some(verification.filled_qty as i64),
                holdings_before: Some(holding_before as i64),
                holdings_after: verification.holdings_after.map(|h| h as i64),
                resolution_source: exit_meta.exit_resolution_source.clone(),
                reason: format!("close_market:{:?}", reason),
                error_message: String::new(),
                metadata: serde_json::json!({
                    "fill_price_unknown": exit_meta.fill_price_unknown,
                    "ws_received": verification.ws_received,
                    "rest_execution_attempts": verification.rest_execution_attempts.len(),
                    "rest_execution_last_status": verification.rest_execution_last_status(),
                    "rest_execution_diagnostics": verification.rest_execution_diagnostics_json(),
                    "exit_reason": format!("{:?}", reason),
                    "quantity": pos.quantity,
                }),
                sent_at: None,
                ack_at: None,
                first_fill_at: Some(Utc::now()),
                final_fill_at: Some(Utc::now()),
            };
            if let Err(e) = store.append_execution_journal(&entry).await {
                warn!(
                    "{}: execution_journal 저장 실패 (exit_final_fill): {e}",
                    self.stock_name
                );
            }
        }

        // Flat 전이 확정
        self.finalize_flat_transition().await;

        Ok((result, exit_meta))
    }

    // ── 헬퍼 메서드 ──

    async fn update_phase(&self, phase: &str) {
        let prev = self.state.read().await.phase.clone();
        self.state.write().await.phase = phase.to_string();
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "strategy",
                "phase_change",
                "info",
                &format!("{} → {}", prev, phase),
                serde_json::json!({"from": prev, "to": phase}),
            );
        }
    }

    /// 2026-04-18 Phase 2: `execution_state` 전이 + 로깅 일원화.
    ///
    /// - idempotent: 동일 상태 재설정은 no-op (state write lock 만 잡고 바로 return).
    /// - 다른 상태로 전이하면 info! + event_log 에 기록.
    /// - legacy bool 필드 (exit_pending, manual_intervention_required 등) 와의 동기화는
    ///   호출자 책임 — 리팩터링 중간 단계. Phase 3/4 에서 execution actor 통합 시 단일화.
    ///
    /// 2026-04-20 PR #5.b + #5.c: actor event 를 해석해 state 전이를 수행.
    ///
    /// `apply_execution_event` 로 허용 여부를 판정하고, 새 상태가 다르면
    /// `transition_execution_state` 로 전이한다. 이 helper 는 **state 결정** 을
    /// actor rule 에 위임하는 것이 목적.
    ///
    /// Command 처리 (PR #5.c):
    /// - `ReconcileBalance`: 실제로 `fetch_holding_snapshot` 호출하고 결과를
    ///   `event_log.actor_reconcile_balance` 에 기록. state 전이는 **하지 않음**
    ///   (self-dispatch recursion 회피). caller 가 결과를 보고 필요 시
    ///   `BalanceReconciled` 이벤트를 별도 dispatch.
    /// - 그 외 모든 command (SubmitEntryOrder / SubmitExitOrder / CancelOrder 등):
    ///   현재 단계에서는 **intent 만 기록** (`event_log.actor_command_intent`),
    ///   실제 I/O 는 기존 경로가 담당. 후속 PR (#5.d~e) 에서 actor task 로 이관.
    ///
    /// `reason` 은 기존 `transition_execution_state` 로그와 호환되도록 호출자가
    /// 지정하며, 일반적으로 event label 과 같거나 더 구체적인 문자열.
    async fn dispatch_execution_event(&self, event: ExecutionEvent, reason: &str) {
        use crate::strategy::execution_actor::{apply_execution_event, command_label, event_label};

        let prev = self.state.read().await.execution_state;
        let ev_label = event_label(&event);
        match apply_execution_event(prev, event) {
            Ok(transition) => {
                if transition.new_state != prev {
                    self.transition_execution_state(transition.new_state, reason)
                        .await;
                }
                let manual_reason = transition
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        ExecutionCommand::EnterManualIntervention { reason } => {
                            Some(reason.clone())
                        }
                        _ => None,
                    });
                // PR #5.c: ReconcileBalance command 는 실제 실행으로 연결.
                // PR #5.d-wire: EnterManualIntervention command 도 runtime 실행 연결.
                // 나머지 command 는 여전히 intent 로만 기록.
                let has_reconcile = transition
                    .commands
                    .iter()
                    .any(|c| matches!(c, ExecutionCommand::ReconcileBalance));
                if let Some(manual_reason) = manual_reason {
                    self.execute_enter_manual_intervention_command(
                        ev_label,
                        reason,
                        &manual_reason,
                    )
                    .await;
                }
                if has_reconcile {
                    self.execute_reconcile_balance_command(ev_label, reason)
                        .await;
                }
                if !transition.commands.is_empty()
                    && let Some(ref el) = self.event_logger
                {
                    let labels: Vec<&'static str> =
                        transition.commands.iter().map(command_label).collect();
                    el.log_event(
                        self.stock_code.as_str(),
                        "strategy",
                        "actor_command_intent",
                        "info",
                        &format!(
                            "event={} → state={} → commands={:?}",
                            ev_label,
                            transition.new_state.label(),
                            labels,
                        ),
                        serde_json::json!({
                            "event": ev_label,
                            "from": prev.label(),
                            "to": transition.new_state.label(),
                            "commands": labels,
                            "reason": reason,
                        }),
                    );
                }
            }
            Err(invalid) => {
                warn!(
                    "{}: actor rejected event {} from state {} (reason={})",
                    self.stock_name,
                    invalid.event_label,
                    prev.label(),
                    reason,
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "strategy",
                        "actor_invalid_transition",
                        "warning",
                        &format!(
                            "actor rule rejected event {} from state {} (reason={})",
                            invalid.event_label,
                            prev.label(),
                            reason,
                        ),
                        serde_json::json!({
                            "event": invalid.event_label,
                            "from": invalid.state.label(),
                            "reason": reason,
                        }),
                    );
                }
            }
        }
    }

    fn execution_timeout_for_state(
        &self,
        state: ExecutionState,
    ) -> Option<(TimeoutPhase, std::time::Duration)> {
        let policy = self.live_cfg.execution_timeout_policy();
        next_timeout_for(state, &policy)
    }

    async fn handle_execution_timeout_expired(&self, phase: TimeoutPhase, reason: &str) {
        let state = self.state.read().await.execution_state;
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "strategy",
                "actor_timeout_expired",
                "warning",
                &format!(
                    "actor timeout expired: phase={:?}, state={}, reason={}",
                    phase,
                    state.label(),
                    reason
                ),
                serde_json::json!({
                    "phase": format!("{:?}", phase),
                    "state": state.label(),
                    "reason": reason,
                }),
            );
        }
        self.dispatch_execution_event(ExecutionEvent::TimeoutExpired { phase }, reason)
            .await;
    }

    /// 2026-04-20 PR #5.c: `ReconcileBalance` command 실행.
    ///
    /// `fetch_holding_snapshot` 을 호출해 현재 잔고를 조회하고 결과를
    /// `event_log.actor_reconcile_balance` 에 기록한다. state 전이는 수행하지 않는다
    /// — caller 가 결과를 해석해 적절한 이벤트 (`BalanceReconciled { holdings_qty }`)
    /// 를 별도 dispatch 할 수 있다.
    ///
    /// 실패 시 warning 만 기록 — fail-safe 경로는 상위 verify/reconcile 로직이
    /// 담당 (예: `safe_orphan_cleanup`, `trigger_manual_intervention`).
    async fn execute_reconcile_balance_command(&self, source_event: &str, reason: &str) {
        match self.fetch_holding_snapshot().await {
            Ok(Some((qty, avg_opt))) => {
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "strategy",
                        "actor_reconcile_balance",
                        "info",
                        &format!(
                            "actor ReconcileBalance: holdings={} (source_event={}, reason={})",
                            qty, source_event, reason,
                        ),
                        serde_json::json!({
                            "source_event": source_event,
                            "reason": reason,
                            "holdings_qty": qty,
                            "avg_price": avg_opt,
                        }),
                    );
                }
            }
            Ok(None) => {
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "strategy",
                        "actor_reconcile_balance",
                        "info",
                        &format!(
                            "actor ReconcileBalance: holdings=0 (source_event={}, reason={})",
                            source_event, reason,
                        ),
                        serde_json::json!({
                            "source_event": source_event,
                            "reason": reason,
                            "holdings_qty": 0,
                        }),
                    );
                }
            }
            Err(e) => {
                warn!(
                    "{}: actor ReconcileBalance fetch_holding_snapshot failed: {e}",
                    self.stock_name
                );
                if let Some(ref el) = self.event_logger {
                    el.log_event(
                        self.stock_code.as_str(),
                        "strategy",
                        "actor_reconcile_balance",
                        "warning",
                        &format!(
                            "actor ReconcileBalance 조회 실패: {e} (source_event={}, reason={})",
                            source_event, reason,
                        ),
                        serde_json::json!({
                            "source_event": source_event,
                            "reason": reason,
                            "error": e.to_string(),
                        }),
                    );
                }
            }
        }
    }

    async fn execute_enter_manual_intervention_command(
        &self,
        source_event: &str,
        reason: &str,
        manual_reason: &str,
    ) {
        self.persist_current_position_for_manual_recovery().await;
        self.trigger_manual_intervention(
            manual_reason.to_string(),
            serde_json::json!({
                "path": "dispatch_execution_event",
                "source_event": source_event,
                "reason": reason,
                "actor_command": "enter_manual_intervention",
            }),
            ManualInterventionMode::KeepLock,
        )
        .await;
    }

    async fn transition_execution_state(&self, new: ExecutionState, reason: &str) {
        let prev = {
            let mut state = self.state.write().await;
            if state.execution_state == new {
                return;
            }
            let prev = state.execution_state;
            state.execution_state = new;
            if new == ExecutionState::Flat {
                // 청산 직후 settle window 의 기준 시각.
                state.last_flat_transition_at = Some(std::time::Instant::now());
            }
            prev
        };
        info!(
            "{}: execution_state {} → {} ({})",
            self.stock_name,
            prev.label(),
            new.label(),
            reason,
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "execution_state",
                "transition",
                "info",
                &format!("{} → {} ({})", prev.label(), new.label(), reason),
                serde_json::json!({
                    "from": prev.label(),
                    "to": new.label(),
                    "reason": reason,
                }),
            );
        }

        // 2026-04-20 PR #5.a: shadow validation — actor rule 이 이 전이를 설명할 수
        // 있는가. false 여도 state 는 이미 변경됐으므로 rollback 하지 않는다 (회귀
        // 위험 0 원칙). warning + event_log 로 기록만 하여 후속 PR 에서 실제 경로
        // 교체 시 "actor rule 이 cover 하지 못하는 전이" 를 먼저 identify.
        use crate::strategy::execution_actor::is_actor_allowed_transition;
        if !is_actor_allowed_transition(prev, new) {
            warn!(
                "{}: actor rule mismatch — {} → {} not explainable by pure actor events (reason={})",
                self.stock_name,
                prev.label(),
                new.label(),
                reason,
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "strategy",
                    "actor_rule_mismatch",
                    "warning",
                    &format!(
                        "transition_execution_state {} → {} not explainable by actor rule (reason={})",
                        prev.label(),
                        new.label(),
                        reason,
                    ),
                    serde_json::json!({
                        "from": prev.label(),
                        "to": new.label(),
                        "reason": reason,
                        "shadow_validation": true,
                    }),
                );
            }
        }
    }

    async fn update_pnl(&self, trades: &[TradeResult]) {
        let mut state = self.state.write().await;
        state.today_trades = trades.to_vec();
        state.today_pnl = trades.iter().map(|t| t.pnl_pct()).sum();
    }

    fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::Relaxed)
    }

    /// 거래 결과를 DB에 저장.
    ///
    /// 2026-04-18 Phase 1: `exit_meta` 로 청산 검증 결과를 함께 기록.
    /// `fill_price_unknown=true` 면 `result.exit_price` 는 현재가 fallback 값이고
    /// 실제 체결가와 다를 수 있다 (사후 분석 필요).
    async fn save_trade_to_db(&self, result: &TradeResult, exit_meta: &ExitTraceMeta) {
        let Some(ref store) = self.db_store else {
            return;
        };
        let today = Local::now().date_naive();
        // result.quantity가 0이면 (안전망) self.quantity로 폴백
        let qty = if result.quantity > 0 {
            result.quantity
        } else {
            self.quantity
        };
        let record = TradeRecord {
            stock_code: self.stock_code.as_str().to_string(),
            stock_name: self.stock_name.clone(),
            side: format!("{:?}", result.side),
            quantity: qty as i64,
            entry_price: result.entry_price,
            exit_price: result.exit_price,
            stop_loss: result.stop_loss,
            take_profit: result.take_profit,
            entry_time: today.and_time(result.entry_time),
            exit_time: today.and_time(result.exit_time),
            exit_reason: format!("{:?}", result.exit_reason),
            pnl_pct: result.pnl_pct(),
            strategy: "orb_fvg".to_string(),
            // 2026-04-17 권고 #2: 하드코딩 제거. LiveRunnerConfig.real_mode 가
            // main.rs 에서 AppConfig.is_real_mode() 와 일치하도록 채워진다.
            environment: if self.live_cfg.real_mode {
                "real"
            } else {
                "paper"
            }
            .to_string(),
            intended_entry_price: result.intended_entry_price,
            entry_slippage: result.entry_price - result.intended_entry_price,
            // 2026-04-17 권고 #1: SL 계열 시장가 청산 슬리피지 측정.
            // - StopLoss/BreakevenStop/TrailingStop: intended = pos.stop_loss,
            //   actual = result.exit_price (verify_exit_completion 으로 확인). 양수면 더 손해.
            // - TakeProfit: TP 지정가 체결이라 슬리피지 0 (KRX 정확 매칭).
            // - TimeStop/EndOfDay: intended 가 없는 시장가 — 측정 의미 약해 0 유지.
            exit_slippage: match result.exit_reason {
                ExitReason::StopLoss | ExitReason::BreakevenStop | ExitReason::TrailingStop => {
                    match result.side {
                        PositionSide::Long => result.stop_loss - result.exit_price,
                        PositionSide::Short => result.exit_price - result.stop_loss,
                    }
                }
                ExitReason::TakeProfit | ExitReason::TimeStop | ExitReason::EndOfDay => 0,
            },
            order_to_fill_ms: result.order_to_fill_ms,
            // 2026-04-18 Phase 1: 청산 검증 경로/체결가 확인 여부 기록
            fill_price_unknown: exit_meta.fill_price_unknown,
            exit_resolution_source: exit_meta.exit_resolution_source.clone(),
        };
        // 3회 재시도 (500ms → 1s → 2s).
        // 거래 기록은 수익 분석의 근본 데이터 — 손실 시 최소한 raw 를 event_log 에 덤프.
        let mut last_err: Option<String> = None;
        for attempt in 0..3u32 {
            match store.save_trade(&record).await {
                Ok(_) => {
                    info!(
                        "{}: 거래 DB 저장 완료 ({:?} {:.2}%){}",
                        self.stock_name,
                        result.exit_reason,
                        result.pnl_pct(),
                        if attempt > 0 {
                            format!(" — {}회 재시도 후 성공", attempt)
                        } else {
                            String::new()
                        }
                    );
                    return;
                }
                Err(e) => {
                    let msg = e.to_string();
                    warn!(
                        "{}: 거래 DB 저장 실패 (시도 {}/3): {msg}",
                        self.stock_name,
                        attempt + 1
                    );
                    last_err = Some(msg);
                    if attempt < 2 {
                        let delay = 500u64 * (1u64 << attempt);
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
            }
        }
        // 3회 실패 — raw data 를 event_log 에 critical 로 덤프 (최후의 복원 수단)
        let err_msg = last_err.unwrap_or_else(|| "unknown".into());
        error!(
            "{}: 거래 DB 저장 최종 실패: {err_msg} — event_log 에 raw 덤프",
            self.stock_name
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "storage",
                "save_trade_failed",
                "critical",
                &format!("거래 저장 3회 실패: {err_msg}"),
                serde_json::json!({
                    "error": err_msg,
                    "record": {
                        "side": record.side,
                        "quantity": record.quantity,
                        "entry_price": record.entry_price,
                        "exit_price": record.exit_price,
                        "stop_loss": record.stop_loss,
                        "take_profit": record.take_profit,
                        "entry_time": record.entry_time.to_string(),
                        "exit_time": record.exit_time.to_string(),
                        "exit_reason": record.exit_reason,
                        "pnl_pct": record.pnl_pct,
                        "intended_entry_price": record.intended_entry_price,
                        "entry_slippage": record.entry_slippage,
                        "order_to_fill_ms": record.order_to_fill_ms,
                        "fill_price_unknown": record.fill_price_unknown,
                        "exit_resolution_source": record.exit_resolution_source,
                    }
                }),
            );
        }
    }

    /// 잔고 API로 D+2 가수도정산금액(prvs_rcdl_excc_amt) 조회.
    /// 이게 KIS HTS의 "D+2 예수금" = 실제 주문가능 cash와 동일.
    async fn get_available_cash(&self) -> i64 {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("AFHR_FLPR_YN", "N"),
            ("OFL_YN", ""),
            ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"),
            ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"),
            ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];
        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await;
        if let Ok(r) = resp
            && let Some(output2) = r.output2
            && let Some(first) = output2.first()
        {
            // prvs_rcdl_excc_amt = 가수도정산금액 (D+2 예수금)
            // KIS HTS의 "D+2 예수금"과 동일하며 매도 대금 정산까지 포함됨.
            let d2_cash: i64 = first
                .get("prvs_rcdl_excc_amt")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            info!(
                "{}: D+2 예수금(prvs_rcdl_excc_amt) = {}",
                self.stock_name, d2_cash
            );
            return d2_cash;
        }
        0
    }

    /// 잔고 API로 해당 종목 실제 보유 여부 확인
    async fn check_balance_has_stock(&self) -> bool {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("AFHR_FLPR_YN", "N"),
            ("OFL_YN", ""),
            ("INQR_DVSN", "02"),
            ("UNPR_DVSN", "01"),
            ("FUND_STTL_ICLD_YN", "N"),
            ("FNCG_AMT_AUTO_RDPT_YN", "N"),
            ("PRCS_DVSN", "00"),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];
        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-balance",
                &TransactionId::InquireBalance,
                Some(&query),
                None,
            )
            .await;
        if let Ok(r) = resp {
            let items = r.output.or(r.output1).unwrap_or_default();
            for item in &items {
                let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
                let qty: u64 = item
                    .get("hldg_qty")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if code == self.stock_code.as_str() && qty > 0 {
                    return true;
                }
            }
        }
        false
    }

    async fn wait_until_session_milestone(&self, target: NaiveTime, session_end: NaiveTime) {
        // 2026-04-24 수정: 장외 재시작은 다음 세션까지 대기해야 하지만,
        // 장중 핫픽스 재시작은 이미 지난 OR milestone 을 즉시 통과해야 한다.
        // 그래서 단순 시간 비교가 아니라 "오늘 세션이 아직 유효한가" 를 함께 본다.
        let deadline =
            compute_session_wait_deadline(Local::now().naive_local(), target, session_end);
        loop {
            if self.is_stopped() {
                return;
            }
            let now = Local::now().naive_local();
            if now >= deadline {
                return;
            }
            let diff = (deadline - now).num_seconds().max(1) as u64;
            let sleep_secs = diff.min(10);
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)) => {}
                _ = self.stop_notify.notified() => {}
            }
        }
    }

    /// TP 지정가 매도 주문 발주.
    /// 가격은 KRX ETF 호가단위(2023 개편: 5,000원 미만 1원, 이상 5원)로 round.
    /// 반환: (주문번호, KRX 거래소번호, 실제 발주된 가격)
    async fn place_tp_limit_order(
        &self,
        price: i64,
        quantity: u64,
    ) -> Option<(String, String, i64)> {
        let rounded = round_to_etf_tick(price);
        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "00",
            "ORD_QTY": quantity.to_string(),
            "ORD_UNPR": rounded.to_string(),
        });

        let resp: Result<KisResponse<serde_json::Value>, _> = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &TransactionId::OrderCashSell,
                None,
                Some(&body),
            )
            .await;

        match resp {
            Ok(r) if r.rt_cd == "0" => {
                let order_no = r
                    .output
                    .as_ref()
                    .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
                    .unwrap_or("")
                    .to_string();
                let krx_orgno = r
                    .output
                    .as_ref()
                    .and_then(|v| v.get("KRX_FWDG_ORD_ORGNO").and_then(|o| o.as_str()))
                    .unwrap_or("")
                    .to_string();
                if rounded != price {
                    info!(
                        "{}: TP 지정가 발주 완료 — {}원(이론 {}원 호가단위 round), 주문번호={}",
                        self.stock_name, rounded, price, order_no
                    );
                } else {
                    info!(
                        "{}: TP 지정가 발주 완료 — {}원, 주문번호={}",
                        self.stock_name, rounded, order_no
                    );
                }
                if let Some(ref store) = self.db_store {
                    store
                        .save_order_log(
                            self.stock_code.as_str(),
                            "TP지정가",
                            "Sell",
                            quantity as i64,
                            rounded,
                            &order_no,
                            "발주",
                            "",
                        )
                        .await;
                }
                Some((order_no, krx_orgno, rounded))
            }
            Ok(r) => {
                warn!(
                    "{}: TP 지정가 발주 실패 — {} (가격 {}원)",
                    self.stock_name, r.msg1, rounded
                );
                if let Some(ref store) = self.db_store {
                    store
                        .save_order_log(
                            self.stock_code.as_str(),
                            "TP지정가",
                            "Sell",
                            quantity as i64,
                            rounded,
                            "",
                            "실패",
                            &r.msg1,
                        )
                        .await;
                }
                None
            }
            Err(e) => {
                warn!("{}: TP 지정가 발주 에러 — {e}", self.stock_name);
                if let Some(ref store) = self.db_store {
                    store
                        .save_order_log(
                            self.stock_code.as_str(),
                            "TP지정가",
                            "Sell",
                            quantity as i64,
                            rounded,
                            "",
                            "에러",
                            &e.to_string(),
                        )
                        .await;
                }
                None
            }
        }
    }

    /// TP 지정가 주문 취소 (3회 재시도)
    async fn cancel_tp_order(&self, order_no: &str, krx_orgno: &str) -> Result<(), KisError> {
        for attempt in 0..3u32 {
            let body = serde_json::json!({
                "CANO": self.client.account_no(),
                "ACNT_PRDT_CD": self.client.account_product_code(),
                "KRX_FWDG_ORD_ORGNO": krx_orgno,
                "ORGN_ODNO": order_no,
                "ORD_DVSN": "00",
                "RVSE_CNCL_DVSN_CD": "02",
                "ORD_QTY": "0",
                "ORD_UNPR": "0",
                "QTY_ALL_ORD_YN": "Y",
            });

            let resp: Result<KisResponse<serde_json::Value>, _> = self
                .client
                .execute(
                    HttpMethod::Post,
                    "/uapi/domestic-stock/v1/trading/order-rvsecncl",
                    &TransactionId::OrderCancel,
                    None,
                    Some(&body),
                )
                .await;

            match resp {
                Ok(r) if r.rt_cd == "0" => {
                    info!("{}: TP 주문 취소 완료 — {}", self.stock_name, order_no);
                    return Ok(());
                }
                Ok(r) => {
                    warn!(
                        "{}: TP 취소 실패 (시도 {}/3) — {}",
                        self.stock_name,
                        attempt + 1,
                        r.msg1
                    );
                }
                Err(e) => {
                    warn!(
                        "{}: TP 취소 에러 (시도 {}/3) — {e}",
                        self.stock_name,
                        attempt + 1
                    );
                }
            }

            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(500 * (1u64 << attempt))).await;
            }
        }
        Err(KisError::Internal(format!(
            "TP 취소 3회 실패: {}",
            order_no
        )))
    }

    /// 잔여분 시장가 매도 (부분 체결 후 잔량 청산용)
    async fn place_market_sell(&self, qty: u64) -> Result<String, KisError> {
        let body = serde_json::json!({
            "CANO": self.client.account_no(),
            "ACNT_PRDT_CD": self.client.account_product_code(),
            "PDNO": self.stock_code.as_str(),
            "ORD_DVSN": "01",
            "ORD_QTY": qty.to_string(),
            "ORD_UNPR": "0",
        });

        let resp: KisResponse<serde_json::Value> = self
            .client
            .execute(
                HttpMethod::Post,
                "/uapi/domestic-stock/v1/trading/order-cash",
                &TransactionId::OrderCashSell,
                None,
                Some(&body),
            )
            .await?;

        if resp.rt_cd == "0" {
            let order_no = resp
                .output
                .as_ref()
                .and_then(|v| v.get("ODNO").and_then(|o| o.as_str()))
                .unwrap_or("")
                .to_string();
            info!(
                "{}: 잔여 {}주 시장가 매도 완료 (주문번호={})",
                self.stock_name, qty, order_no
            );
            Ok(order_no)
        } else {
            Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1))
        }
    }

    /// 주문 체결 상세 조회 — WS 체결통보 우선 (5초), REST 폴링 fallback (5회, 2초 간격)
    ///
    /// 시장가 주문의 실제 체결가를 반드시 확인해야 함.
    /// 체결가 조회 실패 시 이론가(FVG mid_price)로 fallback되면
    /// 실제 손익과 기록 손익이 괴리됨 (2026-04-13 사고 분석).
    async fn fetch_fill_detail(&self, order_no: &str) -> Option<(i64, u64)> {
        // 1차: WS 체결통보 대기 (최대 5초)
        if let Some(ref tx) = self.trade_tx {
            let mut rx = tx.subscribe();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }

                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Ok(RealtimeData::ExecutionNotice(notice))) => {
                        if notice.order_no == order_no
                            && notice.is_filled
                            && notice.filled_price > 0
                        {
                            info!(
                                "{}: WS 체결통보 수신 — 주문 {} → {}원 {}주",
                                self.stock_name, order_no, notice.filled_price, notice.filled_qty
                            );
                            return Some((notice.filled_price, notice.filled_qty));
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => break,
                    _ => {}
                }
            }
            warn!(
                "{}: WS 체결통보 5초 내 미수신 — REST fallback",
                self.stock_name
            );
        }

        // 2차: REST 폴링 (최대 5회, 2초 간격 = 최대 10초 추가)
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            if let Some((price, qty)) = self.query_execution(order_no).await {
                return Some((price, qty));
            }
            debug!(
                "{}: REST 체결 조회 {}회차 미확인 — 주문 {}",
                self.stock_name,
                attempt + 1,
                order_no
            );
        }

        // 3차: 최종 대기 후 마지막 시도 (총 20초 후)
        warn!(
            "{}: 체결 조회 5회 실패 — 5초 추가 대기 후 최종 시도",
            self.stock_name
        );
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Some((price, qty)) = self.query_execution(order_no).await {
            info!(
                "{}: 최종 시도 체결 확인 — 주문 {} → {}원 {}주",
                self.stock_name, order_no, price, qty
            );
            return Some((price, qty));
        }

        error!(
            "{}: 체결 조회 최종 실패 — 주문 {} (실제 체결가 미확인, 이론가로 fallback 위험)",
            self.stock_name, order_no
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "system",
                "fill_query_failed",
                "error",
                &format!("체결 조회 실패 — 주문 {} (WS 5초 + REST 6회)", order_no),
                serde_json::json!({"order_no": order_no}),
            );
        }
        None
    }

    /// position_lock의 preempted 상태를 200ms 간격으로 폴링.
    /// preempted=true 감지 시 반환. position_lock이 None이면 영원히 대기(select!에서 다른 branch 양보).
    ///
    /// wait_execution_notice의 select! branch로 사용하여 선점 요청을 5초 → 200ms 이내에 감지.
    async fn wait_preempted(&self) {
        loop {
            if let Some(ref lock) = self.position_lock {
                let state = lock.read().await;
                if let PositionLockState::Pending {
                    preempted: true, ..
                } = &*state
                {
                    return;
                }
            } else {
                // position_lock 없으면 preempt 불가 — 영원히 pending으로 다른 branch에 양보
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// WebSocket 체결 통보를 지정 시간만큼 대기. 체결 시 (가격, 수량) 반환.
    /// fetch_fill_detail()과 달리 REST 폴링 없이 순수 WS 수신만 수행하여
    /// 외부 루프에서 타임아웃을 제어할 수 있다.
    async fn wait_execution_notice(
        &self,
        order_no: &str,
        timeout: std::time::Duration,
    ) -> Option<(i64, u64)> {
        let tx = self.trade_tx.as_ref()?;
        let mut rx = tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }

            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(RealtimeData::ExecutionNotice(notice)) => {
                            if notice.order_no == order_no && notice.is_filled && notice.filled_price > 0 {
                                info!("{}: WS 체결통보 수신 — 주문 {} → {}원 {}주",
                                    self.stock_name, order_no, notice.filled_price, notice.filled_qty);
                                return Some((notice.filled_price, notice.filled_qty));
                            }
                        }
                        Err(_) => return None, // channel closed
                        _ => {} // 다른 RealtimeData variant — 무시하고 계속 대기
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return None; // 타임아웃
                }
                _ = self.stop_notify.notified() => {
                    return None; // 중지 신호
                }
                _ = self.wait_preempted() => {
                    // 선점 요청 감지 — 체결 대기를 즉시 중단 (감지 지연 최대 ~200ms)
                    return None;
                }
            }
        }
    }

    /// 체결 내역 API 1회 조회 — (평균가, 체결수량) 반환
    async fn query_execution(&self, order_no: &str) -> Option<(i64, u64)> {
        self.query_execution_detailed(order_no, "direct", 1)
            .await
            .fill
    }

    async fn query_execution_detailed(
        &self,
        order_no: &str,
        phase: &'static str,
        attempt: u32,
    ) -> ExecutionQueryOutcome {
        let query = [
            ("CANO", self.client.account_no()),
            ("ACNT_PRDT_CD", self.client.account_product_code()),
            ("INQR_STRT_DT", &Local::now().format("%Y%m%d").to_string()),
            ("INQR_END_DT", &Local::now().format("%Y%m%d").to_string()),
            ("SLL_BUY_DVSN_CD", "00"),
            ("INQR_DVSN", "00"),
            ("PDNO", ""),
            ("CCLD_DVSN", "01"),
            ("ORD_GNO_BRNO", ""),
            ("ODNO", order_no),
            ("INQR_DVSN_3", "00"),
            ("INQR_DVSN_1", ""),
            ("CTX_AREA_FK100", ""),
            ("CTX_AREA_NK100", ""),
        ];

        let resp: Result<KisResponse<Vec<serde_json::Value>>, _> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/trading/inquire-daily-ccld",
                &TransactionId::InquireDailyExecution,
                Some(&query),
                None,
            )
            .await;

        match resp {
            Ok(resp) => {
                let Some(items) = resp.output else {
                    return ExecutionQueryOutcome {
                        fill: None,
                        diagnostics: ExecutionQueryDiagnostics {
                            phase: phase.to_string(),
                            attempt,
                            order_no: order_no.to_string(),
                            status: "no_output".to_string(),
                            ..ExecutionQueryDiagnostics::default()
                        },
                    };
                };
                let outcome = parse_execution_query_items(order_no, phase, attempt, &items);
                if let Some((price, qty)) = outcome.fill {
                    info!(
                        "{}: 체결 조회 — 주문 {} → 평균 {}원, {}주 체결 (phase={}, attempt={})",
                        self.stock_name, order_no, price, qty, phase, attempt
                    );
                }
                outcome
            }
            Err(e) => ExecutionQueryOutcome {
                fill: None,
                diagnostics: ExecutionQueryDiagnostics {
                    phase: phase.to_string(),
                    attempt,
                    order_no: order_no.to_string(),
                    status: "http_error".to_string(),
                    error: Some(e.to_string()),
                    ..ExecutionQueryDiagnostics::default()
                },
            },
        }
    }

    /// REST API로 현재가 조회 (fallback용)
    async fn fetch_current_price_rest(&self) -> Result<InquirePrice, KisError> {
        let query = [
            ("FID_COND_MRKT_DIV_CODE", "J"),
            ("FID_INPUT_ISCD", self.stock_code.as_str()),
        ];
        let resp: KisResponse<InquirePrice> = self
            .client
            .execute(
                HttpMethod::Get,
                "/uapi/domestic-stock/v1/quotations/inquire-price",
                &TransactionId::InquirePrice,
                Some(&query),
                None,
            )
            .await?;
        resp.into_result()
    }

    /// 분봉 조회 — canonical 5 분 시퀀스 반환 (v3 불변식 #2)
    ///
    /// precedence:
    /// - 과거 완료 5 m 버킷 (bucket_end <= now): Yahoo/Naver 백필 (interval_min=5)
    ///   우선, 없을 때만 WS 1m→5m 집계 fallback
    /// - 진행 중 현재 버킷 (bucket_start <= now < bucket_end): WS 1m→5m 집계만 사용
    ///   (Yahoo 는 아직 해당 버킷 미생성)
    ///
    /// 근거: Yahoo 5m 은 완료된 버킷에 대해 공식 OHLC (거래소 원본 근접) 제공.
    /// WS tick 집계는 tick 일부 누락 가능 (2026-04-17 실측: 라이브 high 102,740
    /// vs Yahoo 102,830). OR 계산과 FVG 탐색이 모두 이 canonical 을 사용해
    /// 백테스트/라이브 parity 를 보장.
    async fn fetch_canonical_5m_candles(&self) -> Result<Vec<MinuteCandle>, KisError> {
        let Some(ref ws) = self.ws_candles else {
            return Ok(Vec::new());
        };

        let store = ws.read().await;
        let completed = store.get(self.stock_code.as_str());
        let today = Local::now().date_naive();
        let now_time = Local::now().time();

        let Some(bars) = completed else {
            return Ok(Vec::new());
        };

        // 1. 1분봉(WS 집계)과 5분봉(Yahoo/Naver 백필) 분리.
        let mut ws_1m: Vec<MinuteCandle> = Vec::new();
        let mut backfill_5m: Vec<MinuteCandle> = Vec::new();
        for c in bars {
            let parts: Vec<&str> = c.time.split(':').collect();
            let h: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let m: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let Some(time) = NaiveTime::from_hms_opt(h, m, 0) else {
                continue;
            };
            let candle = MinuteCandle {
                date: today,
                time,
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                volume: c.volume,
            };
            match c.interval_min {
                5 => backfill_5m.push(candle),
                _ => ws_1m.push(candle), // 1 (WS/Naver) 또는 기타
            }
        }

        // 2. WS 1m → 5m 집계 (진행 중 현재 버킷은 aggregate_completed_bars 가
        //    자동으로 pop — bucket_end <= now 판정).
        let ws_5m = aggregate_completed_bars(&ws_1m, 5, now_time);

        // 3. Precedence merge.
        //    - 과거 완료 버킷: backfill_5m 우선 (있을 때), 없으면 ws_5m fallback
        //    - 진행 중 버킷: ws_5m 만 존재 (aggregate_completed_bars 가 미완성
        //      버킷 제거하므로 현재 시점엔 없을 수도)
        use std::collections::BTreeMap;
        let mut by_time: BTreeMap<NaiveTime, MinuteCandle> = BTreeMap::new();

        // 3-a. 과거 완료 버킷에 backfill 우선 주입
        for b in &backfill_5m {
            let bucket_end = b.time + chrono::Duration::minutes(5);
            if bucket_end <= now_time {
                by_time.insert(b.time, b.clone());
            }
        }

        // 3-b. WS 5m 집계. 같은 시각에 backfill 이 이미 있으면 skip,
        //      없으면 fallback 으로 삽입 (과거 버킷 + 현재 진행 버킷 모두 포함)
        for w in &ws_5m {
            by_time.entry(w.time).or_insert_with(|| w.clone());
        }

        Ok(by_time.into_values().collect())
    }

    /// 분봉 조회 — 전체 + 실시간만 분리 반환 (v3 이전 경로, canonical 로 대체)
    #[allow(dead_code)]
    async fn fetch_candles_split(
        &self,
    ) -> Result<(Vec<MinuteCandle>, Vec<MinuteCandle>), KisError> {
        let Some(ref ws) = self.ws_candles else {
            return Ok((Vec::new(), Vec::new()));
        };

        let store = ws.read().await;
        let completed = store.get(self.stock_code.as_str());
        let today = Local::now().date_naive();

        let mut all = Vec::new();
        let mut realtime_only = Vec::new();

        if let Some(bars) = completed {
            for c in bars {
                let parts: Vec<&str> = c.time.split(':').collect();
                let h: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let m: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let Some(time) = NaiveTime::from_hms_opt(h, m, 0) else {
                    continue;
                };
                let candle = MinuteCandle {
                    date: today,
                    time,
                    open: c.open,
                    high: c.high,
                    low: c.low,
                    close: c.close,
                    volume: c.volume,
                };
                all.push(candle.clone());
                if c.is_realtime {
                    realtime_only.push(candle);
                }
            }
        }

        Ok((all, realtime_only))
    }

    /// 분봉 조회 — WebSocket CandleAggregator 데이터 사용 (API 호출 없음)
    #[allow(dead_code)]
    async fn fetch_candles_cached(&self) -> Result<Vec<MinuteCandle>, KisError> {
        let Some(ref ws) = self.ws_candles else {
            return Ok(Vec::new());
        };

        let store = ws.read().await;
        let completed = store.get(self.stock_code.as_str());

        let today = Local::now().date_naive();
        let candles: Vec<MinuteCandle> = completed
            .map(|bars| {
                bars.iter()
                    .filter_map(|c| {
                        let parts: Vec<&str> = c.time.split(':').collect();
                        let h: u32 = parts.first()?.parse().ok()?;
                        let m: u32 = parts.get(1)?.parse().ok()?;
                        let time = NaiveTime::from_hms_opt(h, m, 0)?;
                        Some(MinuteCandle {
                            date: today,
                            time,
                            open: c.open,
                            high: c.high,
                            low: c.low,
                            close: c.close,
                            volume: c.volume,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(candles)
    }

    /// REST API 분봉 조회 (특정 시각부터). v3 불변식 이후 canonical 5m 경로가
    /// 주도권을 가져가면서 직접 호출은 사라졌지만, OR fallback (네이버 실패 시 KIS REST)
    /// 수단으로 보존한다.
    #[allow(dead_code)]
    async fn fetch_minute_candles_from(
        &self,
        from_hour: &str,
    ) -> Result<Vec<MinuteCandle>, KisError> {
        let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
        let mut all_items = Vec::new();
        let mut fid_input_hour = from_hour.to_string();

        // 증분 조회이므로 최대 3페이지면 충분
        for page in 0..3 {
            let query = [
                ("FID_ETC_CLS_CODE", ""),
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", self.stock_code.as_str()),
                ("FID_INPUT_HOUR_1", &fid_input_hour),
                ("FID_PW_DATA_INCU_YN", "N"),
            ];

            let resp: KisResponse<Vec<MinutePriceItem>> = self
                .client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-time-itemchartprice",
                    &TransactionId::InquireTimePrice,
                    Some(&query),
                    None,
                )
                .await?;

            if resp.rt_cd != "0" {
                if page == 0 {
                    return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
                }
                break;
            }

            let has_next = resp.has_next();
            let items = resp.output.unwrap_or_default();
            if items.is_empty() {
                break;
            }

            if let Some(last) = items.last() {
                fid_input_hour = last.stck_cntg_hour.clone();
            }
            all_items.extend(items);

            if !has_next {
                break;
            }
            // 페이지 간 충분한 간격
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }

        let mut candles: Vec<MinuteCandle> = all_items
            .iter()
            .filter_map(|item| {
                let d = chrono::NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let t = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                if t < market_open || t > market_close {
                    return None;
                }
                Some(MinuteCandle {
                    date: d,
                    time: t,
                    open: item.stck_oprc,
                    high: item.stck_hgpr,
                    low: item.stck_lwpr,
                    close: item.stck_prpr,
                    volume: item.cntg_vol as u64,
                })
            })
            .collect();

        candles.sort_by_key(|c| c.time);
        Ok(candles)
    }

    /// OR(Opening Range) 외부 수집: KIS API 분봉 → 네이버 분봉 순으로 시도.
    /// 09:00~09:15 범위의 high/low를 반환. 현재 주 경로는 WS canonical 집계이며 이 함수는
    /// 백필 fallback 스캐폴드 — 호출 사이트 없지만 복구 경로 보전을 위해 유지한다.
    #[allow(dead_code)]
    async fn fetch_or_from_external(
        &self,
        or_start: NaiveTime,
        or_end: NaiveTime,
    ) -> Option<(i64, i64)> {
        // 1차: KIS API 분봉 조회 (OHLCV 정확)
        match self.fetch_minute_candles_from("091500").await {
            Ok(candles) if !candles.is_empty() => {
                let or_candles: Vec<_> = candles
                    .iter()
                    .filter(|c| c.time >= or_start && c.time < or_end)
                    .collect();
                if !or_candles.is_empty() {
                    let h = or_candles.iter().map(|c| c.high).max().unwrap();
                    let l = or_candles.iter().map(|c| c.low).min().unwrap();
                    info!(
                        "{}: KIS API에서 OR 수집 — {}봉 사용",
                        self.stock_name,
                        or_candles.len()
                    );
                    return Some((h, l));
                }
                warn!("{}: KIS API 분봉 있으나 OR 범위 캔들 없음", self.stock_name);
            }
            Ok(_) => warn!("{}: KIS API 분봉 빈 응답", self.stock_name),
            Err(e) => warn!("{}: KIS API 분봉 조회 실패: {e}", self.stock_name),
        }

        // 2차: 네이버 금융 분봉 (종가 기반 근사값)
        info!("{}: 네이버 금융에서 OR 수집 시도", self.stock_name);
        let client = reqwest::Client::new();
        let url = format!(
            "https://fchart.stock.naver.com/siseJson.naver?symbol={}&requestType=1&startTime=20260101&endTime=20261231&timeframe=minute",
            self.stock_code.as_str()
        );

        let resp = match client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("{}: 네이버 요청 실패: {e}", self.stock_name);
                return None;
            }
        };

        let raw = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                warn!("{}: 네이버 응답 읽기 실패: {e}", self.stock_name);
                return None;
            }
        };

        let cleaned = raw.replace('\'', "\"");
        let parsed: Vec<Vec<serde_json::Value>> = match serde_json::from_str(&cleaned) {
            Ok(v) => v,
            Err(e) => {
                warn!("{}: 네이버 파싱 실패: {e}", self.stock_name);
                return None;
            }
        };

        let today = Local::now().date_naive();
        let ticks: Vec<(NaiveTime, i64)> = parsed
            .into_iter()
            .skip(1)
            .filter_map(|row| {
                if row.len() < 6 {
                    return None;
                }
                let dt_str = row[0].as_str()?.trim().trim_matches('"');
                if dt_str.len() != 12 {
                    return None;
                }
                let dt = chrono::NaiveDateTime::parse_from_str(dt_str, "%Y%m%d%H%M").ok()?;
                if dt.date() != today {
                    return None;
                }
                let close = row[4].as_i64()?;
                if close <= 0 {
                    return None;
                }
                Some((dt.time(), close))
            })
            .collect();

        let or_ticks: Vec<_> = ticks
            .iter()
            .filter(|(t, _)| *t >= or_start && *t < or_end)
            .collect();

        if or_ticks.is_empty() {
            warn!("{}: 네이버 OR 범위 틱 없음", self.stock_name);
            return None;
        }

        let h = or_ticks.iter().map(|(_, p)| *p).max().unwrap();
        let l = or_ticks.iter().map(|(_, p)| *p).min().unwrap();
        info!(
            "{}: 네이버에서 OR 수집 (종가 근사) — {}틱 사용",
            self.stock_name,
            or_ticks.len()
        );
        Some((h, l))
    }
}

/// `inquire-balance` 응답 items 에서 이 종목의 보유 스냅샷 추출 (순수 파싱).
///
/// 2026-04-15 Codex re-review 대응 — `fetch_holding_snapshot` 의 판정 규칙을 테스트
/// 가능한 순수 함수로 분리. 핵심 원칙:
/// - qty == 0 인 항목은 스킵 (다음 항목으로 진행)
/// - qty > 0 이면 보유로 확정, avg 는 파싱 가능하고 >0 이면 `Some` 아니면 `None`
/// - 같은 종목이 중복되면 먼저 발견한 "qty > 0" 항목 사용
///
/// 반환:
/// - `Some((qty, Some(avg)))`: 보유 + 평균매입가 확보
/// - `Some((qty, None))`: 보유 (qty>0) 이지만 avg 가 0/미제공/파싱 불가
/// - `None`: 응답에 해당 종목 없거나 qty=0 만 있음
fn build_restart_restored_position(
    saved: &ActivePosition,
    side: PositionSide,
    quantity: u64,
    entry_price: i64,
    stop_loss: i64,
    take_profit: i64,
    best_price: i64,
    original_sl: i64,
    reached_1r: bool,
) -> Position {
    Position {
        side,
        entry_price,
        stop_loss,
        take_profit,
        entry_time: saved.entry_time.time(),
        quantity,
        tp_order_no: None,
        tp_krx_orgno: None,
        tp_limit_price: None,
        reached_1r,
        best_price,
        original_sl,
        sl_triggered_tick: false,
        intended_entry_price: saved.trigger_price.unwrap_or(saved.entry_price),
        order_to_fill_ms: 0,
        signal_id: saved.signal_id.clone().unwrap_or_default(),
    }
}

fn resolve_restart_entry_price(
    balance_avg: Option<i64>,
    execution_fill_price: Option<i64>,
    submitted_price: i64,
) -> i64 {
    balance_avg
        .filter(|price| *price > 0)
        .or_else(|| execution_fill_price.filter(|price| *price > 0))
        .unwrap_or(submitted_price)
}

fn restore_bracket_from_saved(
    saved: &ActivePosition,
    side: PositionSide,
    restored_entry: i64,
    rr_ratio: f64,
) -> (i64, i64) {
    let risk = saved
        .original_risk
        .unwrap_or_else(|| (saved.entry_price - saved.stop_loss).abs())
        .max(1);
    match side {
        PositionSide::Long => (
            restored_entry - risk,
            restored_entry + (risk as f64 * rr_ratio) as i64,
        ),
        PositionSide::Short => (
            restored_entry + risk,
            restored_entry - (risk as f64 * rr_ratio) as i64,
        ),
    }
}

fn parse_holding_snapshot(
    items: &[serde_json::Value],
    stock_code: &str,
) -> Option<(u64, Option<i64>)> {
    for item in items {
        let code = item.get("pdno").and_then(|v| v.as_str()).unwrap_or("");
        if code != stock_code {
            continue;
        }
        let qty: u64 = item
            .get("hldg_qty")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if qty == 0 {
            continue;
        }
        let avg_opt: Option<i64> = item
            .get("pchs_avg_pric")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| f as i64)
            .filter(|&v| v > 0);
        return Some((qty, avg_opt));
    }
    None
}

/// KRX ETF 호가단위로 round-nearest (2023년 KRX 호가단위 개편 후).
/// - 5,000원 미만: 1원 단위 (round 불필요)
/// - 5,000원 이상: 5원 단위
///
/// 2026-04-17 P0: stop_flag 발동 시 자동 청산을 스킵해야 하는지 판정.
///
/// `manual_intervention_required=true` 면 청산 스킵 (= true 반환). reconcile
/// `balance_reconcile_mismatch` 등이 stop_flag 와 함께 manual 플래그를 세웠다면
/// "잔고 API 지연으로 인한 숨은 포지션 오판"일 수 있으므로 자동 청산을 막고
/// 운영자 수동 처리 경로로 보낸다. 정책을 한 곳에 두어 메인 루프와
/// `manage_position` 양쪽이 동일하게 분기되도록 강제한다.
#[inline]
pub(crate) fn should_skip_auto_close_for_manual(state: &RunnerState) -> bool {
    state.manual_intervention_required
}

/// 2026-04-17 v3 fixups H: 자기 종목이 holder 인 경우에만 lock 을 Free 로 푼다.
///
/// 청산 경로(`close_position_market`, abort_entry, 잔여 폐기 등) 에서 lock 을
/// 무조건 `Free` 로 푸는 기존 패턴을 대체한다. 다른 종목의 `Held(other)` /
/// `ManualHeld(other)` / `Pending(other)` 또는 이미 `Free` 인 경우는 건드리지
/// 않아 dual-locked invariant (manual 종목 unresolved 동안 다른 종목 진입 차단)
/// 를 유지한다.
///
/// 적용 사이트:
/// - `live_runner.rs` close_position_market 후 lock 해제
/// - manage_position TP 체결 후 lock 해제
/// - 잔여 포지션 강제 폐기 후 lock 해제
/// - abort_entry 의 lock 해제
async fn release_lock_if_self_held(lock: &Arc<RwLock<PositionLockState>>, code: &str) {
    let mut guard = lock.write().await;
    match &*guard {
        PositionLockState::Held(c) if c == code => {
            *guard = PositionLockState::Free;
        }
        PositionLockState::Pending { code: c, .. } if c == code => {
            *guard = PositionLockState::Free;
        }
        _ => {
            // ManualHeld(any) / Held(other) / Pending(other) / Free / ManualHeld(self)
            // 모두 건드리지 않는다. ManualHeld(self) 는 자기 자신이 manual 인데
            // 이 경로(정상 청산) 에 도달하지 말아야 함 (P0 가 차단). 안전 차원
            // 보존.
        }
    }
}

/// 2026-04-17 v3 F4+: 메인 루프 stop 분기 결정.
///
/// `is_stopped()` 가 true 일 때 메인 루프가 어느 분기를 타야 하는지 결정한다.
/// 분기 정책을 free function 으로 추출해 단위테스트로 회귀 감시한다 (메인 루프
/// 자체는 큰 함수의 일부라 직접 호출이 어려움).
///
/// `had_position` / `has_position` 은 호출자에게 전달되어 청산 시도 여부와
/// `auto_close_skipped` 이벤트 metadata 에 사용된다.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MainLoopStopAction {
    /// manual_intervention 활성 — 자동 청산 스킵, 포지션 보존, 러너 종료.
    SkipForManual { had_position: bool },
    /// 정상 stop — 포지션 있으면 시장가 청산 후 종료, 없으면 그냥 종료.
    CloseAndExit { has_position: bool },
}

#[inline]
pub(crate) fn classify_main_loop_stop(state: &RunnerState) -> MainLoopStopAction {
    let has_pos = state.current_position.is_some();
    if state.manual_intervention_required {
        MainLoopStopAction::SkipForManual {
            had_position: has_pos,
        }
    } else {
        MainLoopStopAction::CloseAndExit {
            has_position: has_pos,
        }
    }
}

/// 2026-04-17 v3 F3: manual_intervention 상태 전환의 공용 진입점.
///
/// 두 callsite (`LiveRunner::trigger_manual_intervention` 와
/// `StrategyManager::reconcile_once`) 의 manual 정책 invariant 를 한 곳에서
/// 강제한다. 양쪽이 미세하게 다른 코드를 들고 있으면 다음 수정에서 정책이
/// 갈라지기 쉽다.
///
/// 정책 (4단 일괄):
/// 1. RunnerState — `phase = "수동 개입 필요"`, `manual_intervention_required = true`,
///    `degraded = true`, `degraded_reason = Some(reason)`
/// 2. shared `position_lock` 이 주어지면 (v3 fixups A):
///    - `Free` / `Pending(_)` / `Held(code)` → `Held(code)` 로 고정
///    - `Held(other)` → **silent overwrite 금지**. 기존 lock 보존 + 별도
///      `manual_intervention_lock_conflict` (critical) 이벤트 기록.
///      이 종목의 manual 은 자체 `manual_intervention_required` 플래그로 차단되므로
///      shared lock 을 굳이 빼앗을 필요 없다. 다른 종목이 자기 청산으로 lock 을
///      Free 로 풀어도 이 종목은 manual 플래그로 신규 진입 차단됨.
/// 3. event_log 에 `event_type` (critical) 기록 — 호출자별 의미 차이 보존
/// 4. `stop_flag.store(true, Relaxed)` + `stop_notify.notify_waiters()`
///
/// statuses (UI 표시용) 갱신은 호출자 책임 — UI 모델은 helper 의 영역이 아니다.
///
/// 호출자별 `event_type`:
/// - `LiveRunner::trigger_manual_intervention` → `"manual_intervention_required"`
/// - `StrategyManager::reconcile_once` → `"balance_reconcile_mismatch"`
///   또는 `"balance_reconcile_inconclusive_escalated"`
pub(crate) async fn enter_manual_intervention_state(
    state: &Arc<RwLock<RunnerState>>,
    position_lock: Option<&Arc<RwLock<PositionLockState>>>,
    stop_flag: &Arc<AtomicBool>,
    stop_notify: &Arc<Notify>,
    event_logger: Option<&Arc<EventLogger>>,
    code: &str,
    event_type: &str,
    reason: String,
    metadata: serde_json::Value,
) {
    let prev_execution_state = {
        let mut s = state.write().await;
        let prev = s.execution_state;
        s.phase = "수동 개입 필요".to_string();
        s.manual_intervention_required = true;
        s.degraded = true;
        s.degraded_reason = Some(reason.clone());
        // Phase 2: execution_state 를 ManualIntervention 으로 전이 (같은 write lock).
        s.execution_state = ExecutionState::ManualIntervention;
        prev
    };
    // 2026-04-17 v3 fixups A + H: Held(other) / ManualHeld(other) 시 silent overwrite 금지.
    // ManualHeld(self) 로 전이해 close 경로가 lock 을 풀지 않도록 한다.
    let mut lock_conflict: Option<String> = None;
    if let Some(lock) = position_lock {
        let mut guard = lock.write().await;
        match &*guard {
            PositionLockState::Held(existing) if existing != code => {
                lock_conflict = Some(existing.clone());
            }
            PositionLockState::ManualHeld(existing) if existing != code => {
                lock_conflict = Some(existing.clone());
            }
            _ => {
                // Free / Pending(any) / Held(self) / ManualHeld(self) → ManualHeld(self)
                *guard = PositionLockState::ManualHeld(code.to_string());
            }
        }
    }
    if let Some(el) = event_logger {
        el.log_event(code, "position", event_type, "critical", &reason, metadata);
        if let Some(existing) = lock_conflict {
            el.log_event(
                code,
                "position",
                "manual_intervention_lock_conflict",
                "critical",
                "manual_intervention 진입 중 shared position_lock 충돌 — 기존 lock 유지",
                serde_json::json!({
                    "requested_code": code,
                    "existing_lock_holder": existing,
                }),
            );
        }
        // Phase 2: execution_state 전이 이벤트 기록 (실제 값이 바뀐 경우에만).
        if prev_execution_state != ExecutionState::ManualIntervention {
            el.log_event(
                code,
                "execution_state",
                "transition",
                "info",
                &format!(
                    "{} → manual_intervention ({})",
                    prev_execution_state.label(),
                    event_type
                ),
                serde_json::json!({
                    "from": prev_execution_state.label(),
                    "to": "manual_intervention",
                    "reason": event_type,
                }),
            );
        }
    }
    stop_flag.store(true, Ordering::Relaxed);
    stop_notify.notify_waiters();
}

/// 장중 milestone 대기 종료 시각 계산 — 2026-04-24 수정.
///
/// 기존 구현은 `NaiveTime` 만 비교해, target 시각이 오늘 이미 지난 경우 즉시
/// 통과했다. 장외(예: 22:31) 재시작 후 `wait_until(09:00)` 이 대기 없이 빠져
/// 러너가 바로 종료되는 버그(2026-04-24 발견)의 원인이었다.
///
/// 현재 정책:
/// - target 전/정각이면 오늘 target 까지 대기.
/// - target 은 지났지만 `session_end` 전이면 장중 재시작으로 보고 즉시 통과.
/// - session_end 이후면 다음날 target 까지 대기.
pub(crate) fn compute_session_wait_deadline(
    now: chrono::NaiveDateTime,
    target: NaiveTime,
    session_end: NaiveTime,
) -> chrono::NaiveDateTime {
    let today_target = now.date().and_time(target);
    if now <= today_target {
        return today_target;
    }
    if now.time() < session_end {
        return now;
    }
    today_target + chrono::Duration::days(1)
}

/// ETF 호가단위 계산 — KRX ETF 호가단위 표 반영.
/// 2,000원 미만 1원 / 2,000원 이상~50,000원 미만 5원 / 50,000원 이상 50원.
fn etf_tick_size(price: i64) -> i64 {
    if price < 2_000 {
        1
    } else if price < 50_000 {
        5
    } else {
        50
    }
}

/// ETF 호가단위 보정 — `etf_tick_size` 결과 단위로 반올림.
/// KODEX 인버스(1,500원대)는 1원, KODEX 레버리지(10만원대)는 50원 단위다.
fn round_to_etf_tick(price: i64) -> i64 {
    let tick = etf_tick_size(price);
    if tick == 1 {
        price
    } else {
        ((price + tick / 2) / tick) * tick
    }
}

/// `track_order_and_guard_repeat` 의 순수 로직 (상태 변경 없는 queue 갱신 + 중복 카운트).
///
/// 테스트 가능성을 위해 분리. async 메서드에선 `&mut queue, now, price` 만 넘겨 호출한다.
///
/// 1. 5분(300초) 초과 이력 제거
/// 2. 신규 항목 push, 최대 크기 10 유지
/// 3. 새 limit_price 와 ±0.1% tolerance 안에 들어오는 주문 수 반환
fn update_order_queue_and_count_dup(
    queue: &mut VecDeque<(NaiveTime, i64)>,
    now: NaiveTime,
    limit_price: i64,
) -> usize {
    while let Some(&(t, _)) = queue.front() {
        let elapsed = (now - t).num_seconds();
        if !(0..=300).contains(&elapsed) {
            queue.pop_front();
        } else {
            break;
        }
    }

    queue.push_back((now, limit_price));
    while queue.len() > 10 {
        queue.pop_front();
    }

    let tolerance = (limit_price as f64 * 0.001).max(1.0) as i64;
    queue
        .iter()
        .filter(|(_, p)| (p - limit_price).abs() <= tolerance)
        .count()
}

#[cfg(test)]
mod repeat_guard_tests {
    use super::update_order_queue_and_count_dup;
    use chrono::NaiveTime;
    use std::collections::VecDeque;

    fn hms(h: u32, m: u32, s: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, s).unwrap()
    }

    #[test]
    fn single_order_has_count_one() {
        let mut q = VecDeque::new();
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        assert_eq!(dup, 1);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn three_same_price_triggers_count_three() {
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        update_order_queue_and_count_dup(&mut q, hms(10, 1, 0), 94_465);
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 2, 0), 94_465);
        assert_eq!(dup, 3);
    }

    #[test]
    fn prices_within_0_1_pct_tolerance_counted_as_same() {
        // 94465 의 0.1% = 94.465 → 정수 tolerance 94 (최종 호출 limit_price 기준).
        // 마지막 호출 가격 94465 를 기준으로 나머지 두 건이 ±94 이내인지 확인.
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_400); // 94465-65, 안쪽
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 30), 94_540); // 94465+75, 안쪽
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 1, 0), 94_465);
        assert_eq!(dup, 3);
    }

    #[test]
    fn prices_outside_tolerance_not_counted() {
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        update_order_queue_and_count_dup(&mut q, hms(10, 1, 0), 95_000); // +535, 바깥
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 2, 0), 94_465);
        assert_eq!(dup, 2); // 94_465 두 번만
    }

    #[test]
    fn old_entries_over_5min_expire() {
        let mut q = VecDeque::new();
        update_order_queue_and_count_dup(&mut q, hms(10, 0, 0), 94_465);
        update_order_queue_and_count_dup(&mut q, hms(10, 2, 0), 94_465);
        // 6분 후 — 10:00 이력은 300초 초과로 제거되어야 함
        let dup = update_order_queue_and_count_dup(&mut q, hms(10, 6, 0), 94_465);
        assert_eq!(dup, 2); // 10:02 + 10:06 (10:00 은 제거됨)
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn queue_capped_at_10_entries() {
        let mut q = VecDeque::new();
        for i in 0..15 {
            // 초 단위를 달리해 다른 시각으로 삽입
            update_order_queue_and_count_dup(&mut q, hms(10, 0, i), 1_000 + (i as i64) * 10_000);
        }
        assert!(q.len() <= 10);
    }
}

#[cfg(test)]
mod stop_action_tests {
    //! 2026-04-17 P0: stop_flag 발동 시 자동 청산 vs manual_intervention 분기 정책.
    //!
    //! `should_skip_auto_close_for_manual` 가 메인 루프와 `manage_position` 양쪽의
    //! "manual 이면 자동 청산을 스킵한다" 정책을 한 곳에서 보장한다. 이 테스트는
    //! 그 정책 자체의 회귀를 감시한다.
    //!
    //! v3 F4+: `classify_main_loop_stop` 매트릭스 4건 추가 — 메인 루프 자체는
    //! 큰 함수의 일부라 직접 호출이 어려우므로 분기 결정 로직을 helper 로 추출해
    //! 단위테스트한다. 사고 핵심 경로(stop+manual+position=Some → close 미호출)
    //! 를 직접 못 박는다.
    use super::{
        ArmedWatchStats, ExecutionState, MainLoopStopAction, PreflightMetadata, RunnerState,
        classify_main_loop_stop, should_skip_auto_close_for_manual,
    };
    use crate::strategy::types::{Position, PositionSide};

    fn make_state(manual: bool, with_position: bool) -> RunnerState {
        let execution_state =
            ExecutionState::derive_from_legacy(with_position, false, manual, false);
        RunnerState {
            phase: String::new(),
            today_trades: Vec::new(),
            today_pnl: 0.0,
            current_position: if with_position {
                Some(Position {
                    side: PositionSide::Long,
                    entry_price: 100_000,
                    stop_loss: 99_000,
                    take_profit: 102_500,
                    entry_time: chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
                    quantity: 10,
                    tp_order_no: None,
                    tp_krx_orgno: None,
                    tp_limit_price: None,
                    reached_1r: false,
                    best_price: 100_000,
                    original_sl: 99_000,
                    sl_triggered_tick: false,
                    intended_entry_price: 100_000,
                    order_to_fill_ms: 0,
                    signal_id: "test-signal".to_string(),
                })
            } else {
                None
            },
            or_high: None,
            or_low: None,
            or_stages: Vec::new(),
            market_halted: false,
            degraded: false,
            manual_intervention_required: manual,
            degraded_reason: None,
            exit_pending: false,
            execution_state,
            preflight_metadata: PreflightMetadata::default(),
            last_external_gate_stale: false,
            last_flat_transition_at: None,
            last_quote_sanity_issue: None,
            armed_stats: ArmedWatchStats::default(),
            current_armed_signal_id: None,
        }
    }

    #[test]
    fn skips_auto_close_when_manual_intervention_required() {
        assert!(should_skip_auto_close_for_manual(&make_state(true, false)));
    }

    #[test]
    fn does_not_skip_auto_close_when_manual_intervention_false() {
        assert!(!should_skip_auto_close_for_manual(&make_state(
            false, false
        )));
    }

    /// v3 F4+ 핵심 사고 경로 회귀: stop+manual+position=Some 이면 메인 루프가
    /// SkipForManual(had_position=true) 를 받아 close 를 호출하지 않는다.
    #[test]
    fn main_loop_with_manual_and_position_takes_skip_branch() {
        assert_eq!(
            classify_main_loop_stop(&make_state(true, true)),
            MainLoopStopAction::SkipForManual { had_position: true },
            "이 분기가 깨지면 사고가 재발한다 (stop+manual+포지션 → 강제 청산)"
        );
    }

    #[test]
    fn main_loop_with_manual_no_position_takes_skip_branch() {
        assert_eq!(
            classify_main_loop_stop(&make_state(true, false)),
            MainLoopStopAction::SkipForManual {
                had_position: false
            },
        );
    }

    #[test]
    fn main_loop_without_manual_with_position_takes_close_branch() {
        assert_eq!(
            classify_main_loop_stop(&make_state(false, true)),
            MainLoopStopAction::CloseAndExit { has_position: true },
            "정상 stop 시 포지션 보유면 시장가 청산 경로",
        );
    }

    #[test]
    fn main_loop_without_manual_no_position_takes_close_branch() {
        assert_eq!(
            classify_main_loop_stop(&make_state(false, false)),
            MainLoopStopAction::CloseAndExit {
                has_position: false
            },
        );
    }
}

#[cfg(test)]
mod tick_tests {
    use super::round_to_etf_tick;

    #[test]
    fn test_round_to_etf_tick() {
        // 2,000원 미만은 1원 단위 그대로
        assert_eq!(round_to_etf_tick(1_524), 1_524);
        assert_eq!(round_to_etf_tick(1_519), 1_519);
        // 2,000원 이상~50,000원 미만은 5원 단위
        assert_eq!(round_to_etf_tick(4_999), 5_000);

        // 5,000원 이상~50,000원 미만도 5원 단위로 round
        assert_eq!(round_to_etf_tick(5_000), 5_000);
        assert_eq!(round_to_etf_tick(5_002), 5_000); // 5002 → 5000
        assert_eq!(round_to_etf_tick(5_003), 5_005); // 5003 → 5005 (반올림)

        // 50,000원 이상은 50원 단위로 round
        assert_eq!(round_to_etf_tick(92_794), 92_800);
        assert_eq!(round_to_etf_tick(92_792), 92_800);
        assert_eq!(round_to_etf_tick(91_500), 91_500); // 이미 정렬됨
        assert_eq!(round_to_etf_tick(92_102), 92_100); // 92102 → 92100
    }
}

/// 2026-04-15 Codex re-review #3 대응 테스트.
///
/// `parse_holding_snapshot` 의 qty>0 판정 규칙을 고정한다. avg=0/미제공 블라인드
/// 스팟이 재발하면 cancel/fill race 방어와 safe_orphan_cleanup 두 경로가 동시에
/// 뚫리므로, 이 규칙만은 회귀 테스트로 계속 감시한다.
#[cfg(test)]
mod holding_snapshot_tests {
    use super::parse_holding_snapshot;
    use serde_json::json;

    #[test]
    fn qty_gt_zero_with_valid_avg_returns_both() {
        let items = vec![json!({
            "pdno": "122630",
            "hldg_qty": "54",
            "pchs_avg_pric": "92450.5",
        })];
        let snap = parse_holding_snapshot(&items, "122630");
        assert_eq!(snap, Some((54u64, Some(92_450i64))));
    }

    #[test]
    fn qty_gt_zero_with_zero_avg_still_treated_as_holding() {
        // avg=0 인데 qty>0 인 케이스 — 모의투자나 일부 특수 상태에서 발생.
        // 반드시 Some 을 반환해야 cancel/fill race / orphan_cleanup 이 오판하지 않는다.
        let items = vec![json!({
            "pdno": "122630",
            "hldg_qty": "54",
            "pchs_avg_pric": "0",
        })];
        let snap = parse_holding_snapshot(&items, "122630");
        assert_eq!(snap, Some((54u64, None)));
    }

    #[test]
    fn qty_gt_zero_with_missing_avg_still_treated_as_holding() {
        // pchs_avg_pric 필드 자체 부재.
        let items = vec![json!({
            "pdno": "122630",
            "hldg_qty": "3",
        })];
        let snap = parse_holding_snapshot(&items, "122630");
        assert_eq!(snap, Some((3u64, None)));
    }

    #[test]
    fn qty_gt_zero_with_unparseable_avg_returns_none_avg() {
        let items = vec![json!({
            "pdno": "122630",
            "hldg_qty": "10",
            "pchs_avg_pric": "N/A",
        })];
        let snap = parse_holding_snapshot(&items, "122630");
        assert_eq!(snap, Some((10u64, None)));
    }

    #[test]
    fn qty_zero_skipped_other_items_considered() {
        // 잔고 응답에 qty=0 잔재가 먼저 나와도 실제 보유 항목을 찾아야 한다.
        let items = vec![
            json!({ "pdno": "122630", "hldg_qty": "0", "pchs_avg_pric": "91000" }),
            json!({ "pdno": "122630", "hldg_qty": "7", "pchs_avg_pric": "91500" }),
        ];
        let snap = parse_holding_snapshot(&items, "122630");
        assert_eq!(snap, Some((7u64, Some(91_500i64))));
    }

    #[test]
    fn other_stock_codes_ignored() {
        let items = vec![
            json!({ "pdno": "114800", "hldg_qty": "20", "pchs_avg_pric": "1500" }),
            json!({ "pdno": "005930", "hldg_qty": "50", "pchs_avg_pric": "70000" }),
        ];
        let snap = parse_holding_snapshot(&items, "122630");
        assert_eq!(snap, None);
    }

    #[test]
    fn empty_items_returns_none() {
        let snap = parse_holding_snapshot(&[], "122630");
        assert_eq!(snap, None);
    }
}

/// 2026-04-15 Codex re-review #4 대응 테스트.
///
/// `trigger_manual_intervention(KeepLock)` 호출 시 shared `PositionLockState` 가
/// `Held(self_code)` 로 고정되는지, state 의 manual 플래그 / stop_flag / phase 가
/// 일관되게 세팅되는지 확인한다. KisHttpClient 실제 요청 없이 검증하기 위해
/// LiveRunner 에 dummy 로 StockCode 와 shared lock 만 주입하는 테스트 빌더 사용.
#[cfg(test)]
mod manual_intervention_tests {
    use super::*;
    use crate::domain::types::{Environment, StockCode};
    use crate::infrastructure::kis_client::auth::TokenManager;
    use crate::infrastructure::kis_client::http_client::KisHttpClient;
    use crate::infrastructure::kis_client::rate_limiter::KisRateLimiter;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn dummy_http_client() -> Arc<KisHttpClient> {
        // 실제 HTTP 요청은 발생하지 않음 — LiveRunner::new 타입 충족용 dummy.
        let env = Environment::Paper;
        let tm = Arc::new(TokenManager::new(
            "test_appkey".to_string(),
            "test_appsecret".to_string(),
            env,
        ));
        let rl = Arc::new(KisRateLimiter::new(env));
        Arc::new(KisHttpClient::new(
            tm,
            rl,
            env,
            "00000000-00".to_string(),
            "01".to_string(),
        ))
    }

    fn build_runner(code: &str) -> (LiveRunner, Arc<RwLock<PositionLockState>>) {
        let stock_code = StockCode::new(code).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lock = Arc::new(RwLock::new(PositionLockState::Free));
        let runner = LiveRunner::new(
            dummy_http_client(),
            stock_code,
            format!("테스트{}", code),
            1,
            stop,
        )
        .with_position_lock(Arc::clone(&lock));
        (runner, lock)
    }

    /// 2026-04-17 v3 fixups H 후: 자기 자신 manual 진입 시 lock = ManualHeld(self).
    /// 이전엔 Held(self) 였으나 H 적용으로 ManualHeld variant 사용.
    #[tokio::test]
    async fn trigger_manual_intervention_keeps_lock_as_held_self() {
        let (runner, lock) = build_runner("122630");
        runner
            .trigger_manual_intervention(
                "test reason".to_string(),
                serde_json::json!({}),
                ManualInterventionMode::KeepLock,
            )
            .await;

        let guard = lock.read().await;
        match &*guard {
            PositionLockState::ManualHeld(c) => assert_eq!(c, "122630"),
            other => panic!("expected ManualHeld(\"122630\"), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn trigger_manual_intervention_sets_state_flags() {
        let (runner, _lock) = build_runner("122630");
        runner
            .trigger_manual_intervention(
                "state flags test".to_string(),
                serde_json::json!({}),
                ManualInterventionMode::KeepLock,
            )
            .await;

        let state = runner.state.read().await;
        assert!(state.manual_intervention_required);
        assert!(state.degraded);
        assert_eq!(state.phase, "수동 개입 필요");
        assert_eq!(state.degraded_reason.as_deref(), Some("state flags test"));
        assert!(runner.stop_flag.load(Ordering::Relaxed));
    }

    /// 2026-04-17 P0 회귀 테스트: manual_intervention 활성 + stop_flag=true 인 상태에서
    /// `manage_position` 이 호출되면 `close_position_market` 이 실행되지 않고 `Ok(None)` 만
    /// 반환해야 한다. 이번 사고에서 reconcile mismatch → stop_flag → 시장가 강제 청산
    /// 회로의 진짜 끝단인 `manage_position` 분기를 직접 검증한다.
    #[tokio::test]
    async fn manage_position_skips_close_when_manual_intervention_active() {
        let (runner, lock) = build_runner("122630");
        {
            let mut state = runner.state.write().await;
            state.manual_intervention_required = true;
        }
        runner.stop_flag.store(true, Ordering::Relaxed);

        let result = runner.manage_position().await;

        // close_position_market 이 호출됐다면 dummy http 클라이언트가 실제 KIS 요청을
        // 시도하면서 Err 가 났을 것이다. Ok(None) 이 나온다는 것은 manual 분기가
        // 정확히 작동해 청산 경로 자체가 진입조차 안 했다는 뜻이다.
        assert!(matches!(result, Ok(None)));

        // shared position_lock 도 manual 분기에서는 건드리지 않는다 (build_runner 가
        // Free 로 초기화한 그대로 유지). 다른 종목 진입을 차단하는 `Held(self)` 보호
        // 는 reconcile_once 또는 trigger_manual_intervention 호출자가 별도로 책임진다.
        let lock_state = lock.read().await;
        assert!(matches!(*lock_state, PositionLockState::Free));

        // current_position 도 보존되어 있어야 (이 테스트는 채워두지 않았으므로 None
        // 이 그대로 유지되는지만 확인) — 자동 청산 분기로 빠지지 않은 증거.
        let state = runner.state.read().await;
        assert!(state.current_position.is_none());
        assert!(state.manual_intervention_required);
    }

    /// 2026-04-17 v3 F4-T1a: `current_position=Some(...)` 매트릭스 테스트.
    /// 이전 테스트는 current_position=None 이라 "manual 분기가 close 를 막는다" 가
    /// 약한 증거였다. 이 테스트는 포지션을 명시적으로 채워서:
    ///  - manual=true → close 호출 0회 → Ok(None)
    ///  - manual=false → close 호출 시도 → dummy http Err → Err 반환
    /// 두 케이스 비교로 "manual 분기가 정확히 close 를 막는다" 강한 증거 확보.
    fn make_test_position() -> Position {
        Position {
            side: PositionSide::Long,
            entry_price: 100_000,
            stop_loss: 99_000,
            take_profit: 102_500,
            entry_time: chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            quantity: 10,
            tp_order_no: None,
            tp_krx_orgno: None,
            tp_limit_price: None,
            reached_1r: false,
            best_price: 100_000,
            original_sl: 99_000,
            sl_triggered_tick: false,
            intended_entry_price: 100_000,
            order_to_fill_ms: 0,
            signal_id: "test-signal".to_string(),
        }
    }

    #[tokio::test]
    async fn manage_position_with_filled_position_skips_close_when_manual_active() {
        let (runner, lock) = build_runner("122630");
        {
            let mut state = runner.state.write().await;
            state.current_position = Some(make_test_position());
            state.manual_intervention_required = true;
        }
        runner.stop_flag.store(true, Ordering::Relaxed);

        let result = runner.manage_position().await;

        // manual=true → close_position_market 호출 0회 → Ok(None) 반환.
        // 포지션이 Some 인 상태에서도 manual 분기가 close 를 막아내는지 검증.
        assert!(matches!(result, Ok(None)));

        // 포지션 보존 — 청산이 일어났다면 current_position 이 None 으로 비워졌을 것.
        let state = runner.state.read().await;
        assert!(
            state.current_position.is_some(),
            "manual 분기는 포지션을 보존해야 함"
        );
        assert!(state.manual_intervention_required);

        // shared lock 은 manual 분기에서는 변경되지 않음 (Free 유지)
        let lock_state = lock.read().await;
        assert!(matches!(*lock_state, PositionLockState::Free));
    }

    #[tokio::test]
    async fn manage_position_with_filled_position_attempts_close_when_manual_inactive() {
        // manual=false + stop=true + position=Some → close_position_market 시도.
        // dummy http 클라이언트라 KIS 호출이 실패하므로 manage_position 이 Err 반환.
        // 이로써 "manual=false 면 청산 경로로 들어간다" 강한 증거 (Err 발생).
        // 같은 setup 에서 위 테스트(manual=true) 는 Ok(None) — 두 케이스 비교가 핵심.
        let (runner, _lock) = build_runner("122630");
        {
            let mut state = runner.state.write().await;
            state.current_position = Some(make_test_position());
            state.manual_intervention_required = false;
        }
        runner.stop_flag.store(true, Ordering::Relaxed);

        let result = runner.manage_position().await;

        assert!(
            result.is_err(),
            "manual=false 면 close 시도 → dummy http Err 가 propagate 돼야 함"
        );
    }

    /// H 후: Pending(other) → ManualHeld(self) 로 전이 (Pending 은 보호 대상 아님).
    #[tokio::test]
    async fn trigger_manual_intervention_overwrites_prior_lock_with_self_held() {
        // 다른 종목이 Pending lock 을 잡고 있어도, 이 종목이 manual_intervention 으로
        // 진입하면 shared lock 을 self ManualHeld 로 고정해야 한다 — 상대 종목의
        // 새 진입을 끌려 들어가게 하지 않기 위함.
        let (runner, lock) = build_runner("122630");
        *lock.write().await = PositionLockState::Pending {
            code: "114800".to_string(),
            preempted: false,
        };

        runner
            .trigger_manual_intervention(
                "override test".to_string(),
                serde_json::json!({}),
                ManualInterventionMode::KeepLock,
            )
            .await;

        let guard = lock.read().await;
        match &*guard {
            PositionLockState::ManualHeld(c) => assert_eq!(c, "122630"),
            other => panic!("expected ManualHeld(\"122630\"), got {:?}", other),
        }
    }

    // ── 2026-04-17 v3 fixups B: enter_manual_intervention_state Held(other) 보호 ──

    fn fresh_runner_state() -> super::RunnerState {
        super::RunnerState {
            phase: String::new(),
            today_trades: Vec::new(),
            today_pnl: 0.0,
            current_position: None,
            or_high: None,
            or_low: None,
            or_stages: Vec::new(),
            market_halted: false,
            degraded: false,
            manual_intervention_required: false,
            degraded_reason: None,
            exit_pending: false,
            execution_state: super::ExecutionState::Flat,
            preflight_metadata: super::PreflightMetadata::default(),
            last_external_gate_stale: false,
            last_flat_transition_at: None,
            last_quote_sanity_issue: None,
            armed_stats: super::ArmedWatchStats::default(),
            current_armed_signal_id: None,
        }
    }

    /// fixups A 핵심 회귀: Held(other) 상태에서 다른 종목이 manual 진입해도
    /// shared lock 은 기존 holder(114800) 그대로 유지돼야 한다 (silent overwrite 금지).
    /// 이 분기가 깨지면 dual-locked invariant (다른 종목이 청산으로 lock=Free 풀
    /// 때 unresolved manual position 종목의 신규 진입이 다시 열릴 수 있음) 가
    /// 깨진다.
    #[tokio::test]
    async fn enter_manual_state_preserves_held_other_lock() {
        let state = Arc::new(RwLock::new(fresh_runner_state()));
        let lock = Arc::new(RwLock::new(PositionLockState::Held("114800".to_string())));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());

        super::enter_manual_intervention_state(
            &state,
            Some(&lock),
            &stop_flag,
            &stop_notify,
            None,
            "122630",
            "manual_intervention_required",
            "test conflict".to_string(),
            serde_json::json!({}),
        )
        .await;

        // lock 은 Held("114800") 그대로 유지 (overwrite 금지)
        let guard = lock.read().await;
        match &*guard {
            PositionLockState::Held(c) => assert_eq!(
                c, "114800",
                "Held(other) silent overwrite 됨 — fixups A 결함"
            ),
            other => panic!("expected Held(\"114800\"), got {:?}", other),
        }

        // 이 종목 자체의 state 는 manual 로 전이 (lock 보호와 무관)
        let s = state.read().await;
        assert!(s.manual_intervention_required);
        assert!(s.degraded);
        assert!(stop_flag.load(Ordering::Relaxed));
    }

    /// H 후: Free → ManualHeld(self) 전이.
    #[tokio::test]
    async fn enter_manual_state_takes_lock_from_free() {
        let state = Arc::new(RwLock::new(fresh_runner_state()));
        let lock = Arc::new(RwLock::new(PositionLockState::Free));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());

        super::enter_manual_intervention_state(
            &state,
            Some(&lock),
            &stop_flag,
            &stop_notify,
            None,
            "122630",
            "manual_intervention_required",
            "free → manual_held".to_string(),
            serde_json::json!({}),
        )
        .await;

        let guard = lock.read().await;
        match &*guard {
            PositionLockState::ManualHeld(c) => assert_eq!(c, "122630"),
            other => panic!("expected ManualHeld(\"122630\"), got {:?}", other),
        }
    }

    /// 2026-04-17 v3 fixups H: ManualHeld(other) 보존 검증 — fixups A 의 Held(other)
    /// 분기와 동일한 정책이 ManualHeld 에도 적용되는지. 두 종목이 동시에 manual
    /// 전환되는 시나리오 (rare).
    #[tokio::test]
    async fn enter_manual_state_preserves_manual_held_other_lock() {
        let state = Arc::new(RwLock::new(fresh_runner_state()));
        let lock = Arc::new(RwLock::new(PositionLockState::ManualHeld(
            "114800".to_string(),
        )));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());

        super::enter_manual_intervention_state(
            &state,
            Some(&lock),
            &stop_flag,
            &stop_notify,
            None,
            "122630",
            "balance_reconcile_mismatch",
            "manual collision".to_string(),
            serde_json::json!({}),
        )
        .await;

        // ManualHeld("114800") 그대로 유지 (silent overwrite 금지)
        let guard = lock.read().await;
        match &*guard {
            PositionLockState::ManualHeld(c) => assert_eq!(
                c, "114800",
                "ManualHeld(other) silent overwrite 됨 — fixups H 결함"
            ),
            other => panic!("expected ManualHeld(\"114800\"), got {:?}", other),
        }
        // 이 종목 자체 state 는 manual 로 전이
        assert!(state.read().await.manual_intervention_required);
    }

    /// 2026-04-17 v3 fixups H: `release_lock_if_self_held` 정책 회귀.
    /// 자기 종목이 holder 인 경우만 Free, 그 외 모든 상태 보존.
    #[tokio::test]
    async fn release_lock_helper_only_releases_self_holder() {
        // 1) Held(self) → Free
        let lock = Arc::new(RwLock::new(PositionLockState::Held("122630".to_string())));
        super::release_lock_if_self_held(&lock, "122630").await;
        assert!(matches!(*lock.read().await, PositionLockState::Free));

        // 2) Held(other) → 보존
        let lock = Arc::new(RwLock::new(PositionLockState::Held("114800".to_string())));
        super::release_lock_if_self_held(&lock, "122630").await;
        match &*lock.read().await {
            PositionLockState::Held(c) => assert_eq!(c, "114800"),
            other => panic!("expected Held(\"114800\") preserved, got {:?}", other),
        }

        // 3) ManualHeld(other) → 보존 (외부 #1 핵심)
        let lock = Arc::new(RwLock::new(PositionLockState::ManualHeld(
            "114800".to_string(),
        )));
        super::release_lock_if_self_held(&lock, "122630").await;
        match &*lock.read().await {
            PositionLockState::ManualHeld(c) => assert_eq!(
                c, "114800",
                "외부 #1 회귀 — manual 종목 unresolved 동안 다른 종목 청산이 lock 풀어버림"
            ),
            other => panic!("expected ManualHeld(\"114800\") preserved, got {:?}", other),
        }

        // 4) ManualHeld(self) → 보존 (정상 청산은 P0 가 차단했지만 안전 보존)
        let lock = Arc::new(RwLock::new(PositionLockState::ManualHeld(
            "122630".to_string(),
        )));
        super::release_lock_if_self_held(&lock, "122630").await;
        match &*lock.read().await {
            PositionLockState::ManualHeld(c) => assert_eq!(c, "122630"),
            other => panic!("expected ManualHeld(\"122630\") preserved, got {:?}", other),
        }

        // 5) Pending(self) → Free
        let lock = Arc::new(RwLock::new(PositionLockState::Pending {
            code: "122630".to_string(),
            preempted: false,
        }));
        super::release_lock_if_self_held(&lock, "122630").await;
        assert!(matches!(*lock.read().await, PositionLockState::Free));

        // 6) Pending(other) → 보존
        let lock = Arc::new(RwLock::new(PositionLockState::Pending {
            code: "114800".to_string(),
            preempted: false,
        }));
        super::release_lock_if_self_held(&lock, "122630").await;
        assert!(matches!(
            *lock.read().await,
            PositionLockState::Pending { .. }
        ));

        // 7) Free → no-op
        let lock = Arc::new(RwLock::new(PositionLockState::Free));
        super::release_lock_if_self_held(&lock, "122630").await;
        assert!(matches!(*lock.read().await, PositionLockState::Free));
    }

    /// 2026-04-17 v3 fixups G + J: persist helper 가 db_store=None 분기로 빠지는
    /// 케이스의 panic 회귀 감시.
    ///
    /// 정확히는 두 호출 모두 **첫 번째 early return(`db_store=None`) 에서 빠짐**.
    /// 두 번째 분기(`current_position=None`) 는 db_store 가 None 이라 도달하지
    /// 못한다. 즉 본 테스트가 검증하는 것은 단 한 가지 분기 — db_store=None 시
    /// panic 없이 즉시 return.
    ///
    /// 본격적인 호출 횟수/인자 검증 (db_store=Some(mock) 시 update_position_trailing
    /// 호출 횟수 / 행 부재 시 active_position_missing 이벤트 기록) 은 PostgresStore
    /// mock trait 추출이 필요하므로 follow-up (별도 PR) 로 분리.
    #[tokio::test]
    async fn persist_current_position_no_op_when_db_store_none() {
        let (runner, _lock) = build_runner("122630");
        runner.persist_current_position_for_manual_recovery().await;

        // current_position 을 채워도 db_store=None 분기가 먼저 잡혀 동일하게 no-op.
        {
            let mut state = runner.state.write().await;
            state.current_position = Some(make_test_position());
        }
        runner.persist_current_position_for_manual_recovery().await;
        // 두 호출 모두 panic 없이 도달 = 통과.
    }

    /// I + H: Held(self) → ManualHeld(self) 전이 — 자기 자신이 일반 보유 중일 때
    /// manual 진입하면 lock variant 만 ManualHeld 로 승격. holder 는 그대로 self.
    #[tokio::test]
    async fn enter_manual_state_self_held_keeps_self_as_holder() {
        let state = Arc::new(RwLock::new(fresh_runner_state()));
        let lock = Arc::new(RwLock::new(PositionLockState::Held("122630".to_string())));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());

        super::enter_manual_intervention_state(
            &state,
            Some(&lock),
            &stop_flag,
            &stop_notify,
            None,
            "122630",
            "manual_intervention_required",
            "self held".to_string(),
            serde_json::json!({}),
        )
        .await;

        let guard = lock.read().await;
        match &*guard {
            PositionLockState::ManualHeld(c) => assert_eq!(c, "122630"),
            other => panic!("expected ManualHeld(\"122630\"), got {:?}", other),
        }
        let s = state.read().await;
        assert!(s.manual_intervention_required);
    }

    /// H 후: Pending(other) → ManualHeld(self) 전이 — Pending 은 보호 대상이 아님.
    #[tokio::test]
    async fn enter_manual_state_takes_lock_from_pending_other() {
        let state = Arc::new(RwLock::new(fresh_runner_state()));
        let lock = Arc::new(RwLock::new(PositionLockState::Pending {
            code: "114800".to_string(),
            preempted: false,
        }));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());

        super::enter_manual_intervention_state(
            &state,
            Some(&lock),
            &stop_flag,
            &stop_notify,
            None,
            "122630",
            "manual_intervention_required",
            "pending → manual_held".to_string(),
            serde_json::json!({}),
        )
        .await;

        let guard = lock.read().await;
        match &*guard {
            PositionLockState::ManualHeld(c) => assert_eq!(c, "122630"),
            other => panic!("expected ManualHeld(\"122630\"), got {:?}", other),
        }
    }

    /// 2026-04-18 Phase 1: `exit_pending=true` 인 상태에서 `manage_position` 이
    /// TP/SL/Time 판정을 스킵하는지 검증 (불변식 4).
    ///
    /// stop_flag=false (stop 분기 미경유), manual_intervention=false (manual 분기 미경유),
    /// current_position=Some, **exit_pending=true** 인 상태로 `manage_position()` 을
    /// 호출하면 dummy http 에 도달하지 않고 `Ok(None)` 이 반환되어야 한다.
    /// 만약 get_current_price 등 외부 호출이 발생하면 dummy 가 Err 를 내어
    /// 이 테스트가 깨진다 — 그 자체가 회귀 시그널.
    #[tokio::test]
    async fn manage_position_skips_when_exit_pending_active() {
        let (runner, _lock) = build_runner("122630");
        {
            let mut state = runner.state.write().await;
            state.current_position = Some(make_test_position());
            state.exit_pending = true;
            // stop_flag / manual 은 모두 false — exit_pending 만으로 스킵됨을 확인
        }

        let result = runner.manage_position().await;

        // exit_pending 가드가 최우선에서 스킵 → Ok(None). get_current_price 가 호출됐다면
        // dummy 클라이언트가 Err 를 냈을 것이다 (Err 반환 → 테스트 실패).
        assert!(
            matches!(result, Ok(None)),
            "exit_pending 활성 시 Ok(None) 기대, 실제 {result:?}"
        );

        // 포지션은 여전히 유지 (flat 전이 금지)
        let state = runner.state.read().await;
        assert!(
            state.current_position.is_some(),
            "exit_pending 중엔 포지션 유지"
        );
        assert!(state.exit_pending, "exit_pending 플래그 보존");
    }
}

#[cfg(test)]
mod aggregate_completed_bars_tests {
    use super::aggregate_completed_bars;
    use crate::strategy::candle::MinuteCandle;
    use chrono::{NaiveDate, NaiveTime};

    fn candle(h: u32, m: u32) -> MinuteCandle {
        MinuteCandle {
            date: NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
            time: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 1,
        }
    }

    #[test]
    fn drops_last_bucket_when_it_is_still_open() {
        let candles = vec![
            candle(9, 45),
            candle(9, 46),
            candle(9, 47),
            candle(9, 48),
            candle(9, 49),
            candle(9, 50),
            candle(9, 51),
            candle(9, 52),
            candle(9, 53),
            candle(9, 54),
            candle(9, 55),
            candle(9, 56),
            candle(9, 57),
            candle(9, 58),
            candle(9, 59),
            candle(10, 0),
            candle(10, 1),
            candle(10, 2),
        ];

        let aggregated =
            aggregate_completed_bars(&candles, 5, NaiveTime::from_hms_opt(10, 3, 0).unwrap());

        let times: Vec<_> = aggregated.iter().map(|c| c.time).collect();
        assert_eq!(
            times,
            vec![
                NaiveTime::from_hms_opt(9, 45, 0).unwrap(),
                NaiveTime::from_hms_opt(9, 50, 0).unwrap(),
                NaiveTime::from_hms_opt(9, 55, 0).unwrap(),
            ]
        );
    }

    #[test]
    fn keeps_last_bucket_after_boundary_passes() {
        let candles = vec![
            candle(9, 55),
            candle(9, 56),
            candle(9, 57),
            candle(9, 58),
            candle(9, 59),
            candle(10, 0),
            candle(10, 1),
            candle(10, 2),
            candle(10, 3),
            candle(10, 4),
        ];

        let aggregated =
            aggregate_completed_bars(&candles, 5, NaiveTime::from_hms_opt(10, 5, 0).unwrap());

        let times: Vec<_> = aggregated.iter().map(|c| c.time).collect();
        assert_eq!(
            times,
            vec![
                NaiveTime::from_hms_opt(9, 55, 0).unwrap(),
                NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            ]
        );
    }
}

/// 2026-04-18 Phase 1 (execution-timing-implementation-plan):
/// `ExitVerification` / `ExitTraceMeta` / `ExitResolutionSource` 순수 로직 회귀.
///
/// 이 모듈은 API I/O 없이 판정 로직만 검증한다:
/// - `can_flat()` 이 holdings=0 확인 이후에만 true 를 돌려준다 (불변식 2).
/// - `has_remaining()` 은 `None` (조회 실패) 도 true 로 간주해 잔량 처리 분기로 보낸다 (불변식 5).
/// - `ExitTraceMeta` helper 가 `fill_price_unknown` 플래그를 정확히 세팅한다.
/// - `ExitResolutionSource::label` 이 DB 에 저장될 문자열과 일치한다.
#[cfg(test)]
mod exit_verification_tests {
    use super::{ExitResolutionSource, ExitTraceMeta, ExitVerification};

    fn make(
        holdings: Option<u64>,
        price: Option<i64>,
        src: ExitResolutionSource,
    ) -> ExitVerification {
        ExitVerification {
            filled_qty: 0,
            filled_price: price,
            holdings_after: holdings,
            resolution_source: src,
            ws_received: false,
            rest_execution_attempts: Vec::new(),
        }
    }

    #[test]
    fn can_flat_only_when_holdings_zero_confirmed() {
        let v = make(Some(0), Some(10_000), ExitResolutionSource::WsNotice);
        assert!(v.can_flat(), "holdings=0 확인 시 flat 허용");
        assert!(!v.has_remaining());
    }

    #[test]
    fn can_flat_blocks_when_holdings_remain() {
        let v = make(Some(3), Some(10_000), ExitResolutionSource::RestExecution);
        assert!(!v.can_flat(), "holdings>0 이면 flat 금지");
        assert!(v.has_remaining());
    }

    #[test]
    fn balance_lookup_failure_is_treated_as_remaining() {
        // 불변식 5: 조회 실패는 자동 추정 금지 → has_remaining=true 로 잔량 처리 분기
        let v = make(None, Some(10_000), ExitResolutionSource::RestExecution);
        assert!(!v.can_flat(), "balance 조회 실패 시 flat 차단");
        assert!(
            v.has_remaining(),
            "조회 실패는 잔량 있다고 간주해 재청산 또는 manual 로"
        );
    }

    #[test]
    fn can_flat_true_even_with_unknown_price() {
        // 체결가 미확인이어도 holdings=0 이면 flat 전이 허용 (price_unknown 경로)
        let v = make(Some(0), None, ExitResolutionSource::BalanceOnly);
        assert!(v.can_flat());
        assert!(!v.has_remaining());
    }

    #[test]
    fn tp_limit_meta_not_price_unknown() {
        let m = ExitTraceMeta::tp_limit();
        assert!(!m.fill_price_unknown);
        assert_eq!(m.exit_resolution_source, "tp_limit");
    }

    #[test]
    fn resolved_meta_records_source_label() {
        let m = ExitTraceMeta::resolved(ExitResolutionSource::WsNotice);
        assert!(!m.fill_price_unknown);
        assert_eq!(m.exit_resolution_source, "ws_notice");

        let m2 = ExitTraceMeta::resolved(ExitResolutionSource::RestExecution);
        assert_eq!(m2.exit_resolution_source, "rest_execution");
    }

    #[test]
    fn price_unknown_meta_sets_flag_and_label() {
        let m = ExitTraceMeta::price_unknown(ExitResolutionSource::BalanceOnly);
        assert!(
            m.fill_price_unknown,
            "현재가 fallback 시 fill_price_unknown 플래그 필수"
        );
        assert_eq!(m.exit_resolution_source, "balance_only");
    }

    #[test]
    fn resolution_source_label_mapping_is_stable() {
        // DB 에 저장되는 문자열이라 변경되면 사후 분석 쿼리가 깨진다 — 회귀 감시.
        assert_eq!(ExitResolutionSource::WsNotice.label(), "ws_notice");
        assert_eq!(
            ExitResolutionSource::RestExecution.label(),
            "rest_execution"
        );
        assert_eq!(ExitResolutionSource::BalanceOnly.label(), "balance_only");
        assert_eq!(ExitResolutionSource::Unresolved.label(), "unresolved");
        assert_eq!(ExitResolutionSource::TpLimit.label(), "tp_limit");
    }

    #[test]
    fn default_meta_is_benign_zero() {
        let d = ExitTraceMeta::default();
        assert!(!d.fill_price_unknown);
        assert_eq!(d.exit_resolution_source, "");
    }

    #[test]
    fn execution_query_parser_aggregates_matching_rows_by_weighted_average() {
        let items = vec![
            serde_json::json!({
                "odno": "A",
                "tot_ccld_qty": "10",
                "avg_prvs": "1000",
            }),
            serde_json::json!({
                "odno": "A",
                "tot_ccld_qty": "30",
                "avg_prvs": "1020",
            }),
            serde_json::json!({
                "odno": "B",
                "tot_ccld_qty": "99",
                "avg_prvs": "9999",
            }),
        ];

        let outcome = super::parse_execution_query_items("A", "post_balance_flat", 2, &items);
        assert_eq!(outcome.fill, Some((1015, 40)));
        assert_eq!(outcome.diagnostics.status, "filled");
        assert_eq!(outcome.diagnostics.output_rows, 3);
        assert_eq!(outcome.diagnostics.matched_rows, 2);
        assert_eq!(outcome.diagnostics.priced_qty, 40);
    }

    #[test]
    fn execution_query_parser_distinguishes_zero_qty_and_missing_order() {
        let zero_qty = vec![serde_json::json!({
            "odno": "A",
            "tot_ccld_qty": "0",
            "avg_prvs": "1000",
        })];
        let zero = super::parse_execution_query_items("A", "pre_balance", 1, &zero_qty);
        assert_eq!(zero.fill, None);
        assert_eq!(zero.diagnostics.status, "matched_zero_qty");

        let missing = super::parse_execution_query_items("B", "pre_balance", 1, &zero_qty);
        assert_eq!(missing.fill, None);
        assert_eq!(missing.diagnostics.status, "order_not_found");
    }

    #[test]
    fn rest_diagnostics_json_keeps_last_status_for_metadata() {
        let mut v = make(Some(0), None, ExitResolutionSource::BalanceOnly);
        v.rest_execution_attempts
            .push(super::ExecutionQueryDiagnostics {
                phase: "pre_balance".to_string(),
                attempt: 1,
                order_no: "0001".to_string(),
                status: "matched_zero_qty".to_string(),
                ..super::ExecutionQueryDiagnostics::default()
            });
        assert_eq!(v.rest_execution_last_status(), Some("matched_zero_qty"));
        let json = v.rest_execution_diagnostics_json();
        assert_eq!(json[0]["phase"], "pre_balance");
        assert_eq!(json[0]["status"], "matched_zero_qty");
    }
}

#[cfg(test)]
mod restart_recovery_tests {
    use super::*;
    use chrono::NaiveDate;

    fn sample_saved_position() -> ActivePosition {
        ActivePosition {
            stock_code: "122630".to_string(),
            side: "Long".to_string(),
            entry_price: 100_000,
            stop_loss: 99_000,
            take_profit: 103_000,
            quantity: 10,
            tp_order_no: String::new(),
            tp_krx_orgno: String::new(),
            entry_time: NaiveDate::from_ymd_opt(2026, 4, 19)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
            original_sl: 99_000,
            reached_1r: false,
            best_price: 100_000,
            or_high: Some(100_000),
            or_low: Some(99_000),
            or_stage: Some("15m".to_string()),
            or_source: None,
            trigger_price: Some(100_000),
            original_risk: Some(1_000),
            signal_id: Some("sig-1".to_string()),
            entry_mode: Some("marketable_limit".to_string()),
            manual_intervention_required: false,
            execution_state: "entry_pending".to_string(),
            last_exit_order_no: String::new(),
            last_exit_reason: String::new(),
            pending_entry_order_no: "12345".to_string(),
            pending_entry_krx_orgno: "54321".to_string(),
        }
    }

    #[test]
    fn build_restart_restored_position_uses_holdings_quantity() {
        let saved = sample_saved_position();
        let pos = super::build_restart_restored_position(
            &saved,
            PositionSide::Long,
            3,
            100_500,
            99_500,
            102_500,
            100_500,
            99_500,
            false,
        );
        assert_eq!(pos.quantity, 3);
        assert_eq!(pos.signal_id, "sig-1");
    }

    #[test]
    fn resolve_restart_entry_price_prefers_balance_avg_then_fill_then_submitted() {
        assert_eq!(
            super::resolve_restart_entry_price(Some(101_000), Some(100_800), 100_500),
            101_000
        );
        assert_eq!(
            super::resolve_restart_entry_price(None, Some(100_800), 100_500),
            100_800
        );
        assert_eq!(
            super::resolve_restart_entry_price(None, None, 100_500),
            100_500
        );
    }

    #[test]
    fn restore_bracket_from_saved_uses_original_risk() {
        let saved = sample_saved_position();
        let (sl, tp) = super::restore_bracket_from_saved(&saved, PositionSide::Long, 101_000, 2.0);
        assert_eq!(sl, 100_000);
        assert_eq!(tp, 103_000);
    }
}

/// 2026-04-18 Phase 2 (execution-timing-implementation-plan):
/// `ExecutionState` 전이 규칙과 `PreflightMetadata` gate 순수 로직 회귀 테스트.
#[cfg(test)]
mod execution_state_tests {
    use super::{
        ExecutionState, PreflightMetadata, QuoteSanityEvaluation, etf_tick_size,
        evaluate_spread_gate, validate_quote_sanity,
    };
    use chrono::NaiveTime;

    #[test]
    fn default_is_flat() {
        assert_eq!(ExecutionState::default(), ExecutionState::Flat);
    }

    #[test]
    fn derive_from_legacy_prioritizes_manual_over_exit_pending() {
        // manual_intervention 이 최우선 — exit_pending / position / degraded 가 모두 true 여도 manual.
        let s = ExecutionState::derive_from_legacy(true, true, true, true);
        assert_eq!(s, ExecutionState::ManualIntervention);
    }

    #[test]
    fn derive_from_legacy_exit_pending_over_open() {
        // manual 아니면 exit_pending 이 두번째 우선 — 포지션 있어도 ExitPending.
        let s = ExecutionState::derive_from_legacy(true, true, false, false);
        assert_eq!(s, ExecutionState::ExitPending);
    }

    #[test]
    fn derive_from_legacy_position_to_open() {
        let s = ExecutionState::derive_from_legacy(true, false, false, false);
        assert_eq!(s, ExecutionState::Open);
    }

    #[test]
    fn derive_from_legacy_degraded_over_flat_when_no_position() {
        let s = ExecutionState::derive_from_legacy(false, false, false, true);
        assert_eq!(s, ExecutionState::Degraded);
    }

    #[test]
    fn derive_from_legacy_default_is_flat() {
        let s = ExecutionState::derive_from_legacy(false, false, false, false);
        assert_eq!(s, ExecutionState::Flat);
    }

    #[test]
    fn can_place_entry_only_in_flat_or_signal_armed() {
        assert!(ExecutionState::Flat.can_place_entry());
        assert!(ExecutionState::SignalArmed.can_place_entry());
        assert!(!ExecutionState::Open.can_place_entry());
        assert!(!ExecutionState::EntryPending.can_place_entry());
        assert!(!ExecutionState::ExitPending.can_place_entry());
        assert!(!ExecutionState::ManualIntervention.can_place_entry());
        assert!(!ExecutionState::Degraded.can_place_entry());
    }

    #[test]
    fn can_auto_exit_matches_positionable_states() {
        assert!(ExecutionState::Open.can_auto_exit());
        assert!(ExecutionState::EntryPartial.can_auto_exit());
        assert!(ExecutionState::ExitPartial.can_auto_exit());
        assert!(!ExecutionState::Flat.can_auto_exit());
        assert!(
            !ExecutionState::ExitPending.can_auto_exit(),
            "ExitPending 은 이미 청산 중"
        );
        assert!(
            !ExecutionState::ManualIntervention.can_auto_exit(),
            "manual 은 자동 청산 금지"
        );
    }

    #[test]
    fn is_manageable_position_restricted_to_open_and_entry_partial() {
        assert!(ExecutionState::Open.is_manageable_position());
        assert!(ExecutionState::EntryPartial.is_manageable_position());
        assert!(!ExecutionState::ExitPending.is_manageable_position());
        assert!(!ExecutionState::ExitPartial.is_manageable_position());
        assert!(!ExecutionState::ManualIntervention.is_manageable_position());
        assert!(!ExecutionState::Flat.is_manageable_position());
    }

    #[test]
    fn label_mapping_is_stable() {
        assert_eq!(ExecutionState::Flat.label(), "flat");
        assert_eq!(ExecutionState::SignalArmed.label(), "signal_armed");
        assert_eq!(ExecutionState::EntryPending.label(), "entry_pending");
        assert_eq!(ExecutionState::EntryPartial.label(), "entry_partial");
        assert_eq!(ExecutionState::Open.label(), "open");
        assert_eq!(ExecutionState::ExitPending.label(), "exit_pending");
        assert_eq!(ExecutionState::ExitPartial.label(), "exit_partial");
        assert_eq!(
            ExecutionState::ManualIntervention.label(),
            "manual_intervention"
        );
        assert_eq!(ExecutionState::Degraded.label(), "degraded");
    }

    #[test]
    fn preflight_default_all_ok() {
        let pf = PreflightMetadata::default();
        assert!(
            pf.all_ok(),
            "기본값은 모든 gate 통과 — 리팩터링 중간 단계에서 기존 동작 보존"
        );
    }

    #[test]
    fn preflight_all_ok_requires_every_gate_true() {
        let mut pf = PreflightMetadata::default();
        pf.spread_ok = false;
        assert!(!pf.all_ok());

        let mut pf = PreflightMetadata::default();
        pf.nav_gate_ok = false;
        assert!(!pf.all_ok());

        let mut pf = PreflightMetadata::default();
        pf.regime_confirmed = false;
        assert!(!pf.all_ok());

        let mut pf = PreflightMetadata::default();
        pf.subscription_health_ok = false;
        assert!(!pf.all_ok());

        let mut pf = PreflightMetadata::default();
        pf.quote_issue = Some("off_market".to_string());
        assert!(
            !pf.all_ok(),
            "비정상 호가는 external spread override 와 무관하게 최종 진입을 차단해야 한다"
        );
    }

    #[test]
    fn preflight_allows_entry_only_before_cutoff_for_active_instrument() {
        let pf = PreflightMetadata {
            entry_cutoff: Some(NaiveTime::from_hms_opt(10, 30, 0).unwrap()),
            active_instrument: Some("122630".to_string()),
            ..PreflightMetadata::default()
        };

        assert!(pf.allows_entry_for("122630", NaiveTime::from_hms_opt(10, 29, 59).unwrap()));
        assert!(!pf.allows_entry_for("122630", NaiveTime::from_hms_opt(10, 30, 0).unwrap()));
        assert!(!pf.allows_entry_for("114800", NaiveTime::from_hms_opt(10, 29, 59).unwrap()));
    }

    #[test]
    fn etf_tick_size_matches_krx_table() {
        assert_eq!(etf_tick_size(1_439), 1);
        assert_eq!(etf_tick_size(4_999), 5);
        assert_eq!(etf_tick_size(49_999), 5);
        assert_eq!(etf_tick_size(50_000), 50);
        assert_eq!(etf_tick_size(101_915), 50);
    }

    #[test]
    fn spread_gate_uses_50won_tick_for_high_priced_etf() {
        let eval = evaluate_spread_gate(Some((101_950, 101_900)), true, 3);
        assert!(
            eval.spread_ok,
            "10만원대 ETF 1호가 50원은 정상 스프레드로 봐야 함"
        );
        assert_eq!(eval.spread_tick, Some(50));
        assert_eq!(eval.spread_raw, Some(50));
        assert_eq!(eval.spread_threshold_ticks, 3);
    }

    #[test]
    fn preflight_spread_in_ticks_reports_ratio() {
        let pf = PreflightMetadata {
            spread_raw: Some(100),
            spread_tick: Some(50),
            ..PreflightMetadata::default()
        };
        assert_eq!(pf.spread_in_ticks(), Some(2.0));
    }

    #[test]
    fn quote_sanity_accepts_normal_quote_near_current_price() {
        assert_eq!(
            validate_quote_sanity(1_405, 1_400, Some(1_402)),
            QuoteSanityEvaluation::Valid
        );
    }

    #[test]
    fn quote_sanity_rejects_inverted_quote() {
        assert_eq!(
            validate_quote_sanity(1_399, 1_400, Some(1_400)),
            QuoteSanityEvaluation::Invalid("inverted")
        );
    }

    #[test]
    fn quote_sanity_rejects_off_market_quote() {
        assert_eq!(
            validate_quote_sanity(20_000, 294_564_483, Some(1_400)),
            QuoteSanityEvaluation::Invalid("inverted")
        );
        assert_eq!(
            validate_quote_sanity(2_000, 1_990, Some(1_400)),
            QuoteSanityEvaluation::Invalid("off_market")
        );
    }
}

/// 2026-04-24 세션 milestone 대기 회귀 방지.
/// 장외 재시작은 다음 세션을 기다리고, 장중 재시작은 이미 지난 milestone 을
/// 즉시 통과해야 한다.
#[cfg(test)]
mod wait_deadline_tests {
    use super::compute_session_wait_deadline;
    use chrono::{NaiveDate, NaiveTime};

    fn dt(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap()
    }

    #[test]
    fn deadline_is_today_when_target_still_in_future() {
        // 01:02 → 09:00: 같은 날 09:00 을 기다려야 한다.
        let now = dt(2026, 4, 24, 1, 2);
        let target = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let session_end = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
        assert_eq!(
            compute_session_wait_deadline(now, target, session_end),
            dt(2026, 4, 24, 9, 0)
        );
    }

    #[test]
    fn deadline_rolls_to_next_day_when_target_already_passed_after_session() {
        // 22:31 → 09:00: 다음 날 09:00 까지 대기해야 한다 (핵심 회귀 케이스).
        let now = dt(2026, 4, 23, 22, 31);
        let target = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let session_end = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
        assert_eq!(
            compute_session_wait_deadline(now, target, session_end),
            dt(2026, 4, 24, 9, 0)
        );
    }

    #[test]
    fn deadline_is_immediate_when_market_open_milestone_already_passed() {
        // 13:39 핫픽스 재시작 → 09:00 장 시작 milestone 은 이미 충족된 것으로 봐야 한다.
        let now = dt(2026, 4, 24, 13, 39);
        let target = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let session_end = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
        assert_eq!(compute_session_wait_deadline(now, target, session_end), now);
    }

    #[test]
    fn deadline_waits_for_or_end_when_it_is_still_future_today() {
        // 09:01 재시작 → 09:05 5분 OR 종료는 아직 미래이므로 기다린다.
        let now = dt(2026, 4, 24, 9, 1);
        let target = NaiveTime::from_hms_opt(9, 5, 0).unwrap();
        let session_end = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
        assert_eq!(
            compute_session_wait_deadline(now, target, session_end),
            dt(2026, 4, 24, 9, 5)
        );
    }

    #[test]
    fn deadline_rolls_after_session_end() {
        // 16:00 → 09:00: 세션 종료 이후 → 다음 날 장 시작.
        let now = dt(2026, 4, 24, 16, 0);
        let target = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let session_end = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
        assert_eq!(
            compute_session_wait_deadline(now, target, session_end),
            dt(2026, 4, 25, 9, 0)
        );
    }

    #[test]
    fn deadline_now_equals_target_returns_today() {
        // 09:00 정각에 wait_until(09:00) — 즉시 통과해야 하므로 오늘 09:00 deadline.
        let now = dt(2026, 4, 24, 9, 0);
        let target = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let session_end = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
        assert_eq!(
            compute_session_wait_deadline(now, target, session_end),
            dt(2026, 4, 24, 9, 0)
        );
    }
}
