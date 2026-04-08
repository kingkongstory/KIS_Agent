use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tracing::{debug, warn};

use super::app_state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/ws/realtime", get(ws_handler))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.realtime_tx.subscribe();

    // 실시간 데이터 → 클라이언트 전송 태스크
    let send_task = tokio::spawn(async move {
        while let Ok(data) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&data)
                && sender.send(Message::Text(json.into())).await.is_err()
            {
                break;
            }
        }
    });

    // 클라이언트 메시지 수신 (ping/pong, 종료 처리)
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Close(_)) => break,
            Ok(Message::Text(text)) => {
                debug!("WS 클라이언트 메시지: {text}");
            }
            Err(e) => {
                warn!("WS 수신 오류: {e}");
                break;
            }
            _ => {}
        }
    }

    send_task.abort();
}
