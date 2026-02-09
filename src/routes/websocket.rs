//! WebSocket endpoint for interactive query execution.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

use crate::error::ApiError;
use crate::metrics::determine_language;
use crate::state::AppState;

use super::helpers::{effective_timeout, record_metrics, resolve_db_name, run_with_timeout};
use super::query::run_query;
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
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = match state.databases().get(&db_name) {
        Some(e) => e,
        None => {
            return WsServerMessage::Error {
                id,
                error: "not_found".to_string(),
                detail: Some(format!("database '{db_name}' not found")),
            };
        }
    };

    let timeout = effective_timeout(state, req.timeout_ms);
    let lang = determine_language(req.language.as_deref());

    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(state, lang, &result);

    match result {
        Ok(response) => WsServerMessage::Result { id, response },
        Err(e) => {
            let (error, detail) = match &e {
                ApiError::BadRequest(msg) => ("bad_request".to_string(), Some(msg.clone())),
                ApiError::Timeout => ("timeout".to_string(), None),
                ApiError::NotFound(msg) => ("not_found".to_string(), Some(msg.clone())),
                _ => ("internal_error".to_string(), Some(e.to_string())),
            };
            WsServerMessage::Error { id, error, detail }
        }
    }
}
