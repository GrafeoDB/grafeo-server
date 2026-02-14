//! WebSocket endpoint for interactive query execution.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

use grafeo_service::error::ServiceError;
use grafeo_service::query::QueryService;

use crate::state::AppState;

use super::helpers::{convert_json_params, query_result_to_response};
use super::types::{QueryRequest, WsClientMessage, WsServerMessage};

/// WebSocket upgrade handler.
///
/// Authentication is handled by the middleware stack before this handler
/// runs — the `/ws` route is inside the authenticated router, so the
/// HTTP upgrade request must carry valid credentials.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    while let Some(msg) = receiver.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(data)) => {
                let _ = sender.send(Message::Pong(data)).await;
                continue;
            }
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!("WebSocket receive error: {e}");
                break;
            }
        };

        let client_msg: WsClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let err = WsServerMessage::Error {
                    id: None,
                    error: "bad_request".to_string(),
                    detail: Some(format!("invalid message: {e}")),
                };
                if send_json(&mut sender, &err).await.is_err() {
                    break;
                }
                continue;
            }
        };

        let reply = match client_msg {
            WsClientMessage::Ping => WsServerMessage::Pong,
            WsClientMessage::Query { id, request } => process_query(&state, id, request).await,
        };

        if send_json(&mut sender, &reply).await.is_err() {
            break;
        }
    }

    tracing::debug!("WebSocket connection closed");
}

/// Sends a JSON-serialized message over the WebSocket.
async fn send_json<S>(sender: &mut S, msg: &WsServerMessage) -> Result<(), ()>
where
    S: SinkExt<Message, Error = axum::Error> + Unpin,
{
    let text = serde_json::to_string(msg).expect("WsServerMessage is always serializable");
    sender
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// Executes a query and returns a `WsServerMessage`.
async fn process_query(state: &AppState, id: Option<String>, req: QueryRequest) -> WsServerMessage {
    let db_name = grafeo_service::resolve_db_name(req.database.as_deref());
    let params = match convert_json_params(req.params.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            return WsServerMessage::Error {
                id,
                error: "bad_request".to_string(),
                detail: Some(e.to_string()),
            };
        }
    };
    let timeout = state.effective_timeout(req.timeout_ms);

    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        db_name,
        &req.query,
        req.language.as_deref(),
        params,
        timeout,
    )
    .await;

    match result {
        Ok(qr) => WsServerMessage::Result {
            id,
            response: query_result_to_response(&qr),
        },
        Err(e) => {
            let (error, detail) = match &e {
                ServiceError::BadRequest(msg) => ("bad_request".to_string(), Some(msg.clone())),
                ServiceError::Timeout => ("timeout".to_string(), None),
                ServiceError::NotFound(msg) => ("not_found".to_string(), Some(msg.clone())),
                _ => ("internal_error".to_string(), Some(e.to_string())),
            };
            WsServerMessage::Error { id, error, detail }
        }
    }
}
