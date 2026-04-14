use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::time::{Duration, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use super::crypto::aes_cbc_base64_decrypt;
use super::parser::{self, DataMessage, WsMessage, parse_data_message, parse_ws_message};
use super::subscription::SubscriptionManager;
use crate::domain::error::KisError;
use crate::domain::ports::realtime::RealtimeData;
use crate::domain::types::Environment;
use crate::infrastructure::kis_client::auth::TokenManager;
use crate::infrastructure::monitoring::event_logger::EventLogger;

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
        }
    }

    /// 이벤트 로거 주입 (`ws_reconnect` 등 system 이벤트를 DB 에 기록).
    pub fn with_event_logger(mut self, logger: Arc<EventLogger>) -> Self {
        self.event_logger = Some(logger);
        self
    }

    pub fn subscription_manager(&self) -> Arc<SubscriptionManager> {
        Arc::clone(&self.subscription_manager)
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
                    if retry_count > max_retries {
                        error!("WebSocket 최대 재시도 횟수 초과: {e}");
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

    fn handle_message(&self, text: &str) {
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
                    // 체결통보 구독 응답에서 AES key/iv 추출
                    if ctrl.header.tr_id == "H0STCNI9" || ctrl.header.tr_id == "H0STCNI0" {
                        if let Some(output) = &ctrl.body.output {
                            let key = output.get("key").and_then(|v| v.as_str()).map(String::from);
                            let iv = output.get("iv").and_then(|v| v.as_str()).map(String::from);
                            if key.is_some() && iv.is_some() {
                                let aes_key = Arc::clone(&self.aes_key);
                                let aes_iv = Arc::clone(&self.aes_iv);
                                tokio::spawn(async move {
                                    *aes_key.write().await = key;
                                    *aes_iv.write().await = iv;
                                });
                                info!("체결통보 AES key/iv 수신 완료");
                            }
                        }
                    }
                } else {
                    warn!(
                        "구독 실패: {} — {} ({})",
                        ctrl.header.tr_id, ctrl.body.msg_cd, ctrl.body.msg1
                    );
                }
            }
            Some(WsMessage::Data(data)) => {
                // 체결통보: 암호화 데이터 → 복호화 후 파싱
                if data.encrypted && (data.tr_id == "H0STCNI9" || data.tr_id == "H0STCNI0") {
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
}
