//! WebSocket endpoint for interactive query execution.
//!
//! The `/ws` endpoint supports two modes of operation:
//!
//! - **Query mode** (`type: "query"`): execute any supported query language and
//!   receive the result in a single round-trip.
//! - **Subscription mode** (`type: "subscribe"`, requires `push-changefeed`):
//!   stream live change events for a named database.
//!
//! Multiple subscriptions may be active on the same connection simultaneously.
//! Each subscription is identified by a client-assigned `sub_id`.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

use grafeo_service::error::ServiceError;
use grafeo_service::query::QueryService;

use crate::encode::{convert_json_params, query_result_to_response};
use crate::state::AppState;
use crate::types::{QueryRequest, WsClientMessage, WsServerMessage};

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

    #[cfg(feature = "push-changefeed")]
    {
        handle_with_subscriptions(&mut sender, &mut receiver, state).await;
    }

    #[cfg(not(feature = "push-changefeed"))]
    {
        // Simple sequential version — used when push-changefeed is not enabled.
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
    }

    tracing::debug!("WebSocket connection closed");
}

// ---------------------------------------------------------------------------
// Subscription-capable handler (requires push-changefeed)
// ---------------------------------------------------------------------------

#[cfg(feature = "push-changefeed")]
async fn handle_with_subscriptions<S, R>(sender: &mut S, receiver: &mut R, state: AppState)
where
    S: SinkExt<Message, Error = axum::Error> + Unpin,
    R: StreamExt<Item = Result<Message, axum::Error>> + Unpin,
{
    use std::collections::HashMap;

    use tokio::sync::broadcast::error::RecvError;
    use tokio::sync::mpsc;

    use crate::types::WsServerMessage;

    // Channel that collects events from all active subscription tasks.
    let (event_tx, mut event_rx) =
        mpsc::unbounded_channel::<(String, grafeo_service::sync::ChangeEventDto)>();

    // Active subscription tasks, keyed by sub_id.
    let mut sub_tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

    loop {
        tokio::select! {
            biased;

            // Prioritise incoming WebSocket messages.
            msg = receiver.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                        continue;
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
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
                        if send_json(sender, &err).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };

                let reply: WsServerMessage = match client_msg {
                    WsClientMessage::Ping => WsServerMessage::Pong,
                    WsClientMessage::Query { id, request } => {
                        process_query(&state, id, request).await
                    }
                    WsClientMessage::Subscribe { sub_id, db, since } => {
                        let rx = state.change_hub().subscribe(&db, since, state.service().clone());
                        let tx = event_tx.clone();
                        let sid = sub_id.clone();
                        let handle = tokio::spawn(async move {
                            let mut rx = rx;
                            loop {
                                match rx.recv().await {
                                    Ok(event) => {
                                        if tx.send((sid.clone(), event)).is_err() {
                                            break;
                                        }
                                    }
                                    Err(RecvError::Lagged(n)) => {
                                        tracing::debug!(
                                            "WebSocket changefeed sub '{sid}' lagged by {n} events"
                                        );
                                        // Continue — client will receive the next available event.
                                    }
                                    Err(RecvError::Closed) => break,
                                }
                            }
                        });
                        sub_tasks.insert(sub_id.clone(), handle);
                        WsServerMessage::Subscribed { sub_id }
                    }
                    WsClientMessage::Unsubscribe { sub_id } => {
                        if let Some(handle) = sub_tasks.remove(&sub_id) {
                            handle.abort();
                        }
                        WsServerMessage::Unsubscribed { sub_id }
                    }
                };

                if send_json(sender, &reply).await.is_err() {
                    break;
                }
            }

            // Forward change events from active subscriptions.
            event = event_rx.recv() => {
                if let Some((sub_id, change_event)) = event {
                    let msg = WsServerMessage::Change { sub_id, event: Box::new(change_event) };
                    if send_json(sender, &msg).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // Clean up all subscription tasks when the connection closes.
    for (_, handle) in sub_tasks {
        handle.abort();
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

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
        state.service().is_query_read_only(),
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
