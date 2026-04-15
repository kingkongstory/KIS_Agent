use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, RwLock};
use tokio::time::{Duration, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use super::crypto::aes_cbc_base64_decrypt;
use super::parser::{
    self, ControlMessage, DataMessage, WsMessage, parse_data_message, parse_ws_message,
};
use super::subscription::SubscriptionManager;
use crate::domain::error::KisError;
use crate::domain::ports::realtime::RealtimeData;
use crate::domain::types::Environment;
use crate::infrastructure::kis_client::auth::TokenManager;
use crate::infrastructure::monitoring::event_logger::EventLogger;

/// WS 운영 상태 스냅샷 — watchdog / /monitoring/health 용.
///
/// Unix epoch 초 단위 타임스탬프는 직렬화가 간단하고 UTC 차이를 직접 계산 가능해
/// Instant 대신 선택했다. `None` 이면 아직 해당 이벤트가 발생하지 않음.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WsHealthSnapshot {
    /// 최신 text 프레임 수신 (PINGPONG 포함 모든 메시지).
    pub last_message_epoch_secs: Option<u64>,
    /// 실시간 시세 tick (Execution/Orderbook/MarketOperation) 최신 수신.
    pub last_tick_epoch_secs: Option<u64>,
    /// 체결통보(H0STCNI9/0) 최신 수신.
    pub last_notification_epoch_secs: Option<u64>,
    /// 누적 재연결 시도 횟수.
    pub retry_count: u32,
    /// 연결이 영구 종료됐는지 (max_retries 초과 등). true 면 프로세스 교체 필요.
    pub terminated: bool,
    /// AES key/iv 수립 여부 — 체결통보 복호화 가능 상태인지.
    pub notification_ready: bool,
    /// 조회 시점 epoch 초.
    pub observed_epoch_secs: u64,
}

impl WsHealthSnapshot {
    /// 마지막 tick 수신 후 경과 초 (없으면 None).
    pub fn tick_age_secs(&self) -> Option<u64> {
        self.last_tick_epoch_secs
            .map(|t| self.observed_epoch_secs.saturating_sub(t))
    }
    /// 마지막 메시지 수신 후 경과 초 (없으면 None).
    pub fn message_age_secs(&self) -> Option<u64> {
        self.last_message_epoch_secs
            .map(|t| self.observed_epoch_secs.saturating_sub(t))
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// KIS WebSocket 클라이언트
pub struct KisWebSocketClient {
    token_manager: Arc<TokenManager>,
    environment: Environment,
    subscription_manager: Arc<SubscriptionManager>,
    data_tx: broadcast::Sender<RealtimeData>,
    /// 체결통보 AES 복호화 키 (구독 응답에서 수신)
    aes_key: Arc<RwLock<Option<String>>>,
    /// 체결통보 AES 복호화 IV (구독 응답에서 수신)
    aes_iv: Arc<RwLock<Option<String>>>,
    /// 운영 이벤트 로거 (ws_reconnect 기록용). 주입 안 하면 로깅 생략.
    event_logger: Option<Arc<EventLogger>>,
    /// 2026-04-16 watchdog: 최신 text 프레임 수신 시각 (epoch 초).
    last_message_epoch_secs: Arc<RwLock<Option<u64>>>,
    /// 실시간 시세 tick 수신 시각 — WS 데이터 프레임만 카운트.
    last_tick_epoch_secs: Arc<RwLock<Option<u64>>>,
    /// 체결통보(H0STCNI9/0) 수신 시각.
    last_notification_epoch_secs: Arc<RwLock<Option<u64>>>,
    /// 누적 재연결 시도 카운터.
    retry_count: Arc<AtomicU32>,
    /// 연결이 영구 종료됐는지 (`run` 이 max_retries 초과로 break 했을 때 true).
    terminated: Arc<AtomicBool>,
}

impl KisWebSocketClient {
    pub fn new(
        token_manager: Arc<TokenManager>,
        environment: Environment,
        data_tx: broadcast::Sender<RealtimeData>,
    ) -> Self {
        Self {
            token_manager,
            environment,
            subscription_manager: Arc::new(SubscriptionManager::new()),
            data_tx,
            aes_key: Arc::new(RwLock::new(None)),
            aes_iv: Arc::new(RwLock::new(None)),
            event_logger: None,
            last_message_epoch_secs: Arc::new(RwLock::new(None)),
            last_tick_epoch_secs: Arc::new(RwLock::new(None)),
            last_notification_epoch_secs: Arc::new(RwLock::new(None)),
            retry_count: Arc::new(AtomicU32::new(0)),
            terminated: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 운영 상태 스냅샷. 2026-04-16 watchdog / `/api/v1/monitoring/health` 에서 사용.
    pub async fn health_snapshot(&self) -> WsHealthSnapshot {
        let last_message = *self.last_message_epoch_secs.read().await;
        let last_tick = *self.last_tick_epoch_secs.read().await;
        let last_notification = *self.last_notification_epoch_secs.read().await;
        WsHealthSnapshot {
            last_message_epoch_secs: last_message,
            last_tick_epoch_secs: last_tick,
            last_notification_epoch_secs: last_notification,
            retry_count: self.retry_count.load(Ordering::Relaxed),
            terminated: self.terminated.load(Ordering::Relaxed),
            notification_ready: self.is_notification_ready().await,
            observed_epoch_secs: now_epoch_secs(),
        }
    }

    /// WS 가 영구 종료됐는지. watchdog 가 프로세스 교체 여부를 판단할 때 사용.
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Relaxed)
    }

    /// 이벤트 로거 주입 (`ws_reconnect` 등 system 이벤트를 DB 에 기록).
    pub fn with_event_logger(mut self, logger: Arc<EventLogger>) -> Self {
        self.event_logger = Some(logger);
        self
    }

    pub fn subscription_manager(&self) -> Arc<SubscriptionManager> {
        Arc::clone(&self.subscription_manager)
    }

    /// 체결통보(H0STCNI9/H0STCNI0) 복호화 준비 완료 여부.
    ///
    /// AES key/iv 가 구독 응답으로 수신·저장되어 실제 체결 데이터를 복호화할 수 있는 상태일 때 true.
    /// 2026-04-16 P0 — 이 값이 false 이면 거래 시작 자체를 차단한다. WS 체결통보 없이
    /// 지정가 매수의 체결 확인을 REST 폴링에만 의존하면 cancel/fill race 가 재발할 수 있음.
    pub async fn is_notification_ready(&self) -> bool {
        let key = self.aes_key.read().await;
        let iv = self.aes_iv.read().await;
        key.is_some() && iv.is_some()
    }

    /// WebSocket 연결 시작 (백그라운드 태스크로 실행)
    pub async fn run(self: Arc<Self>) {
        let max_retries = 10;
        let mut retry_count = 0;

        loop {
            match self.connect_and_listen().await {
                Ok(()) => {
                    info!("WebSocket 정상 종료");
                    break;
                }
                Err(e) => {
                    retry_count += 1;
                    self.retry_count.store(retry_count, Ordering::Relaxed);
                    if retry_count > max_retries {
                        error!("WebSocket 최대 재시도 횟수 초과: {e}");
                        self.terminated.store(true, Ordering::Relaxed);
                        if let Some(ref el) = self.event_logger {
                            el.log_event(
                                "", "system", "ws_reconnect", "critical",
                                &format!("최대 재시도 횟수 초과 — 종료: {e}"),
                                serde_json::json!({
                                    "retry_count": retry_count,
                                    "max_retries": max_retries,
                                    "terminated": true,
                                }),
                            );
                        }
                        break;
                    }

                    let delay = exponential_backoff(retry_count);
                    warn!(
                        "WebSocket 연결 실패 (시도 {retry_count}/{max_retries}): {e}, {delay:?} 후 재연결"
                    );
                    if let Some(ref el) = self.event_logger {
                        el.log_event(
                            "", "system", "ws_reconnect", "warn",
                            &format!("WS 재연결 (시도 {retry_count}/{max_retries}): {e}"),
                            serde_json::json!({
                                "retry_count": retry_count,
                                "max_retries": max_retries,
                                "delay_ms": delay.as_millis() as u64,
                                "error": e.to_string(),
                            }),
                        );
                    }
                    sleep(delay).await;
                }
            }
        }
    }

    async fn connect_and_listen(&self) -> Result<(), KisError> {
        let approval_key = self.token_manager.get_approval_key().await?;
        let url = self.environment.ws_url();

        info!("WebSocket 연결 시도: {url}");

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| KisError::WebSocketError(format!("연결 실패: {e}")))?;

        info!("WebSocket 연결 성공");

        let (mut write, mut read) = ws_stream.split();

        // 기존 구독 복원
        let restore_msgs = self.subscription_manager.restore_messages(&approval_key);
        let expected_subs = restore_msgs.len();
        for msg in restore_msgs {
            write
                .send(Message::Text(msg.into()))
                .await
                .map_err(|e| KisError::WebSocketError(e.to_string()))?;
        }

        // 구독 응답 확인 (최대 5초 대기)
        if expected_subs > 0 {
            let mut confirmed = 0usize;
            let mut failed = 0usize;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

            while confirmed + failed < expected_subs {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }

                match tokio::time::timeout(remaining, read.next()).await {
                    Ok(Some(Ok(Message::Text(text)))) => {
                        if text.contains("PINGPONG") {
                            continue;
                        }
                        match parse_ws_message(&text) {
                            Some(WsMessage::Control(ctrl)) => {
                                // 2026-04-15 Codex review #1 대응: 초기 구독 확인 loop 와
                                // 메인 `handle_message` 양쪽이 동일한 AES key/iv 추출 경로를
                                // 사용해야 한다. 구독 응답이 초기 loop 에서 소비되면서
                                // 체결통보(H0STCNI9) 의 AES key 가 영구 미설정되는 critical
                                // 버그(gate 항상 타임아웃)를 `handle_control_message` 공통
                                // 헬퍼로 차단.
                                self.handle_control_message(&ctrl);
                                if ctrl.body.rt_cd == "0" {
                                    confirmed += 1;
                                    info!(
                                        "구독 확인 ({confirmed}/{expected_subs}): {} {}",
                                        ctrl.header.tr_id, ctrl.header.tr_key
                                    );
                                } else {
                                    failed += 1;
                                    warn!(
                                        "구독 실패: {} {} — {}",
                                        ctrl.header.tr_id, ctrl.header.tr_key, ctrl.body.msg1
                                    );
                                }
                            }
                            Some(WsMessage::Data(data)) => {
                                // 확인 대기 중 수신된 데이터는 정상 처리
                                if let Some(realtime) = parse_data_message(&data) {
                                    let _ = self.data_tx.send(realtime);
                                }
                            }
                            None => {}
                        }
                    }
                    Ok(Some(Ok(Message::Ping(data)))) => {
                        let _ = write.send(Message::Pong(data)).await;
                    }
                    Ok(Some(Err(e))) => {
                        warn!("구독 확인 중 WebSocket 오류 (무시): {e}");
                        break;
                    }
                    Ok(None) => break,
                    _ => break, // 타임아웃
                }
            }

            if failed > 0 {
                warn!("구독 결과: {confirmed}건 성공, {failed}건 실패 (총 {expected_subs}건)");
            } else if confirmed < expected_subs {
                warn!("구독 확인 타임아웃: {confirmed}/{expected_subs}건만 확인");
            } else {
                info!("전체 구독 확인 완료: {confirmed}건");
            }
        }

        // 메시지 수신 루프
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    self.handle_message(&text);
                }
                Ok(Message::Ping(data)) => {
                    let _ = write.send(Message::Pong(data)).await;
                }
                Ok(Message::Close(_)) => {
                    info!("WebSocket 서버 종료 요청");
                    break;
                }
                Err(e) => {
                    return Err(KisError::WebSocketError(format!("수신 오류: {e}")));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Control 프레임 처리 공통 경로.
    ///
    /// 구독 응답 확인 loop 와 `handle_message` 양쪽에서 동일하게 호출되어,
    /// H0STCNI9/H0STCNI0 의 AES key/iv 추출이 어느 경로에서 수신되어도 일관되게 수행된다.
    /// 2026-04-15 Codex review #1 대응 — 초기 loop 가 구독 응답을 먼저 소비하면서
    /// AES key 저장이 skip 되던 critical 버그를 차단.
    fn handle_control_message(&self, ctrl: &ControlMessage) {
        if ctrl.body.rt_cd != "0" {
            return;
        }
        if ctrl.header.tr_id != "H0STCNI9" && ctrl.header.tr_id != "H0STCNI0" {
            return;
        }
        let Some(output) = &ctrl.body.output else { return };
        let key = output.get("key").and_then(|v| v.as_str()).map(String::from);
        let iv = output.get("iv").and_then(|v| v.as_str()).map(String::from);
        if key.is_none() || iv.is_none() {
            return;
        }
        let aes_key = Arc::clone(&self.aes_key);
        let aes_iv = Arc::clone(&self.aes_iv);
        tokio::spawn(async move {
            *aes_key.write().await = key;
            *aes_iv.write().await = iv;
        });
        info!("체결통보 AES key/iv 수신 완료");
    }

    fn handle_message(&self, text: &str) {
        // 2026-04-16 watchdog: 모든 text 프레임 수신 기록 (PINGPONG 포함).
        // 장중 N초 이상 메시지가 없으면 stale 로 간주해 fatal exit.
        let now = now_epoch_secs();
        {
            let slot = Arc::clone(&self.last_message_epoch_secs);
            tokio::spawn(async move {
                *slot.write().await = Some(now);
            });
        }

        // PINGPONG 메시지는 무시 (KIS 서버 헬스체크)
        if text.contains("PINGPONG") {
            debug!("KIS PINGPONG 수신");
            return;
        }

        match parse_ws_message(text) {
            Some(WsMessage::Control(ctrl)) => {
                if ctrl.body.rt_cd == "0" {
                    info!(
                        "구독 확인 — {} {} ({})",
                        ctrl.header.tr_id, ctrl.header.tr_key, ctrl.body.msg1
                    );
                    // 체결통보 AES key/iv 추출은 공통 경로에서 수행 (초기 loop 와 일관).
                    self.handle_control_message(&ctrl);
                } else {
                    warn!(
                        "구독 실패: {} — {} ({})",
                        ctrl.header.tr_id, ctrl.body.msg_cd, ctrl.body.msg1
                    );
                }
            }
            Some(WsMessage::Data(data)) => {
                // 데이터 프레임 수신 — tick 시계 갱신.
                let tick_slot = Arc::clone(&self.last_tick_epoch_secs);
                tokio::spawn(async move {
                    *tick_slot.write().await = Some(now);
                });

                // 체결통보: 암호화 데이터 → 복호화 후 파싱
                if data.encrypted && (data.tr_id == "H0STCNI9" || data.tr_id == "H0STCNI0") {
                    // 체결통보 수신 시각 기록 (복호화 성공 여부와 무관 — 경로 존재 확인)
                    let notif_slot = Arc::clone(&self.last_notification_epoch_secs);
                    tokio::spawn(async move {
                        *notif_slot.write().await = Some(now);
                    });

                    let aes_key = Arc::clone(&self.aes_key);
                    let aes_iv = Arc::clone(&self.aes_iv);
                    let tx = self.data_tx.clone();
                    let raw = data.data_parts.join("^"); // 원본 복원
                    tokio::spawn(async move {
                        let key_guard = aes_key.read().await;
                        let iv_guard = aes_iv.read().await;
                        if let (Some(key), Some(iv)) = (key_guard.as_ref(), iv_guard.as_ref()) {
                            match aes_cbc_base64_decrypt(key, iv, &raw) {
                                Ok(decrypted) => {
                                    let parts: Vec<String> = decrypted.split('^').map(String::from).collect();
                                    let dec_data = DataMessage {
                                        encrypted: false,
                                        tr_id: "H0STCNI9".to_string(),
                                        count: 1,
                                        data_parts: parts,
                                    };
                                    if let Some(realtime) = parser::parse_execution_notice(&dec_data) {
                                        let _ = tx.send(realtime);
                                    }
                                }
                                Err(e) => warn!("체결통보 복호화 실패: {e}"),
                            }
                        } else {
                            warn!("체결통보 수신했으나 AES key/iv 미설정");
                        }
                    });
                } else if let Some(realtime) = parse_data_message(&data) {
                    let _ = self.data_tx.send(realtime);
                }
            }
            None => {
                warn!("알 수 없는 WebSocket 메시지: {}", &text[..text.len().min(100)]);
            }
        }
    }
}

/// 지수 백오프 (1초 ~ 60초)
fn exponential_backoff(attempt: u32) -> Duration {
    let secs = (1u64 << attempt.min(6)).min(60);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        assert_eq!(exponential_backoff(1), Duration::from_secs(2));
        assert_eq!(exponential_backoff(2), Duration::from_secs(4));
        assert_eq!(exponential_backoff(3), Duration::from_secs(8));
        assert_eq!(exponential_backoff(6), Duration::from_secs(60));
        assert_eq!(exponential_backoff(10), Duration::from_secs(60)); // 최대 60초
    }

    /// 2026-04-15 Codex review #1 / re-review 대응 테스트.
    ///
    /// 구독 확인 loop 와 메인 `handle_message` 가 동일한 `handle_control_message`
    /// 공통 경로를 쓰는지, 그 경로에서 H0STCNI9 응답의 AES key/iv 가 저장되는지를
    /// 고정한다. 과거 초기 loop 가 응답을 먼저 소비하면서 key 저장이 누락돼 health
    /// gate 가 항상 타임아웃되던 critical 버그의 회귀 방지.
    fn new_test_client() -> Arc<KisWebSocketClient> {
        let tm = Arc::new(TokenManager::new(
            "test_appkey".to_string(),
            "test_appsecret".to_string(),
            crate::domain::types::Environment::Paper,
        ));
        let (tx, _rx) = broadcast::channel(16);
        Arc::new(KisWebSocketClient::new(
            tm,
            crate::domain::types::Environment::Paper,
            tx,
        ))
    }

    fn control_msg(tr_id: &str, rt_cd: &str, output: Option<serde_json::Value>) -> ControlMessage {
        ControlMessage {
            header: super::parser::ControlHeader {
                tr_id: tr_id.to_string(),
                tr_key: "ignored".to_string(),
                encrypt: String::new(),
            },
            body: super::parser::ControlBody {
                rt_cd: rt_cd.to_string(),
                msg_cd: String::new(),
                msg1: String::new(),
                output,
            },
        }
    }

    #[tokio::test]
    async fn handle_control_message_captures_aes_key_iv_for_h0stcni9() {
        let client = new_test_client();
        assert!(!client.is_notification_ready().await);

        client.handle_control_message(&control_msg(
            "H0STCNI9",
            "0",
            Some(serde_json::json!({
                "key": "1234567890abcdef",
                "iv":  "abcdef1234567890",
            })),
        ));

        // handle_control_message 는 tokio::spawn 으로 저장하므로 완료 대기.
        // yield_now 를 반복 호출해 spawned task 가 실행되도록 한다.
        for _ in 0..20 {
            if client.is_notification_ready().await {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(client.is_notification_ready().await, "AES key/iv should be captured");
    }

    #[tokio::test]
    async fn handle_control_message_ignores_non_notification_tr_ids() {
        let client = new_test_client();

        client.handle_control_message(&control_msg(
            "H0STCNT0", // 체결가 구독 (AES 대상 아님)
            "0",
            Some(serde_json::json!({
                "key": "shouldnt_be_saved",
                "iv":  "shouldnt_be_saved",
            })),
        ));

        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!client.is_notification_ready().await);
    }

    #[tokio::test]
    async fn handle_control_message_ignores_failed_subscriptions() {
        let client = new_test_client();

        // rt_cd != "0" — 구독 실패. 응답에 key/iv 가 있어도 저장되면 안 됨.
        client.handle_control_message(&control_msg(
            "H0STCNI9",
            "9",
            Some(serde_json::json!({
                "key": "shouldnt_be_saved",
                "iv":  "shouldnt_be_saved",
            })),
        ));

        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!client.is_notification_ready().await);
    }

    #[tokio::test]
    async fn handle_control_message_ignores_missing_key_or_iv() {
        let client = new_test_client();

        // key 만 있고 iv 없음 — 둘 다 있어야 저장.
        client.handle_control_message(&control_msg(
            "H0STCNI9",
            "0",
            Some(serde_json::json!({
                "key": "only_key_present",
            })),
        ));

        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!client.is_notification_ready().await);
    }
}
