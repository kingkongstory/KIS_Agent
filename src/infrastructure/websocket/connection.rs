use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

use super::parser::{WsMessage, parse_data_message, parse_ws_message};
use super::subscription::SubscriptionManager;
use crate::domain::error::KisError;
use crate::domain::ports::realtime::RealtimeData;
use crate::domain::types::Environment;
use crate::infrastructure::kis_client::auth::TokenManager;

/// KIS WebSocket 클라이언트
pub struct KisWebSocketClient {
    token_manager: Arc<TokenManager>,
    environment: Environment,
    subscription_manager: Arc<SubscriptionManager>,
    data_tx: broadcast::Sender<RealtimeData>,
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
        }
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
                        break;
                    }

                    let delay = exponential_backoff(retry_count);
                    warn!(
                        "WebSocket 연결 실패 (시도 {retry_count}/{max_retries}): {e}, {delay:?} 후 재연결"
                    );
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
        for msg in restore_msgs {
            write
                .send(Message::Text(msg.into()))
                .await
                .map_err(|e| KisError::WebSocketError(e.to_string()))?;
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
        match parse_ws_message(text) {
            Some(WsMessage::Control(ctrl)) => {
                if ctrl.body.rt_cd == "0" {
                    info!(
                        "구독 {} — {} ({})",
                        ctrl.header.tr_id, ctrl.header.tr_key, ctrl.body.msg1
                    );
                } else {
                    warn!(
                        "구독 실패: {} — {} ({})",
                        ctrl.header.tr_id, ctrl.body.msg_cd, ctrl.body.msg1
                    );
                }
            }
            Some(WsMessage::Data(data)) => {
                if let Some(realtime) = parse_data_message(&data) {
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
