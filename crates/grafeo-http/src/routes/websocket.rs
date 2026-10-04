//! WebSocket endpoint for interactive query execution.
//!
//! The `/ws` endpoint supports two modes of operation:
//!
//! - **Query mode** (`type: "query"`): execute any supported query language and
//!   receive the result in a single round-trip.
//! - **Subscription mode** (`type: "subscribe"`, requires `push-changefeed`):
//!   stream the change events of a named database from epoch `since` on:
//!   its history first, then live events, with no gap and no repeat between
//!   them (the SSE stream's handover). History and live events are the same
//!   `change` messages. A subscribe to a missing database, or one without
//!   CDC, is answered with an `error` whose `id` is the `sub_id`.
//!
//! Multiple subscriptions may be active on the same connection simultaneously.
//! Each subscription is identified by a client-assigned `sub_id`; subscribing
//! again with a `sub_id` in use replaces that subscription. Two `error`
//! messages whose `id` is the `sub_id` end a subscription, and the connection
//! stays open:
//!
//! - `error` is `"lagged"`: the subscription fell too far behind and lost
//!   events. `detail` is `{"skipped": n, "since": e}`: subscribe again with
//!   `since = e` (the first epoch not delivered in full; inclusive, so epoch
//!   0 is resumed too).
//! - `error` is `"closed"`: the database's change feed stopped (the database
//!   was dropped or restored, or its CDC turned off), or a history read
//!   failed. `detail` says which ("change feed closed" for a stopped feed).

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

use grafeo_engine::auth::Identity;
use grafeo_service::error::ServiceError;
use grafeo_service::query::QueryService;

use crate::encode::{convert_json_params, query_result_to_response};
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;
use crate::types::{QueryRequest, WsClientMessage, WsServerMessage};

/// WebSocket upgrade handler.
///
/// Authentication is handled by the middleware stack before this handler
/// runs, the `/ws` route is inside the authenticated router, so the
/// HTTP upgrade request must carry valid credentials.
///
/// When CORS origins are configured, the `Origin` header is validated
/// before upgrade. Browsers do not enforce CORS on WebSocket connections,
/// so this server-side check prevents cross-site WebSocket hijacking.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    auth: AuthContext,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, crate::error::ApiError> {
    // Validate Origin header when CORS origins are configured.
    // No Origin header is allowed (non-browser clients don't send one).
    let cors_origins = state.cors_origins();
    if !cors_origins.is_empty()
        && let Some(origin) = headers.get(axum::http::header::ORIGIN)
    {
        let origin_str = origin.to_str().unwrap_or("");
        let allowed = cors_origins.iter().any(|o| o == "*" || o == origin_str);
        if !allowed {
            return Err(crate::error::ApiError::forbidden(
                "WebSocket origin not allowed".to_string(),
            ));
        }
    }

    let identity = auth.identity(state.service().is_query_read_only());
    let db_scope = auth
        .0
        .as_ref()
        .map(|info| info.scope.databases.clone())
        .unwrap_or_default();
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, identity, db_scope)))
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    identity: Identity,
    db_scope: Vec<String>,
) {
    let (mut sender, mut receiver) = socket.split();

    #[cfg(feature = "push-changefeed")]
    {
        handle_with_subscriptions(&mut sender, &mut receiver, state, identity, db_scope).await;
    }

    #[cfg(not(feature = "push-changefeed"))]
    {
        // Simple sequential version, used when push-changefeed is not enabled.
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
                WsClientMessage::Query { id, request } => {
                    process_query(&state, id, request, &identity, &db_scope).await
                }
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
async fn handle_with_subscriptions<S, R>(
    sender: &mut S,
    receiver: &mut R,
    state: AppState,
    identity: Identity,
    db_scope: Vec<String>,
) where
    S: SinkExt<Message, Error = axum::Error> + Unpin,
    R: StreamExt<Item = Result<Message, axum::Error>> + Unpin,
{
    use std::collections::HashMap;

    use tokio::sync::mpsc;

    use crate::routes::change_stream::{StreamItem, change_stream, history_page};
    use crate::types::WsServerMessage;

    // Channel that collects events from all active subscription tasks.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<(String, StreamItem)>();

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
                        process_query(&state, id, request, &identity, &db_scope).await
                    }
                    WsClientMessage::Subscribe { sub_id, db, since } => {
                        // Check database scope before subscribing.
                        if !db_scope.is_empty()
                            && !db_scope.iter().any(|d| d == &db)
                        {
                            WsServerMessage::Error {
                                id: None,
                                error: "forbidden".to_string(),
                                detail: Some(format!(
                                    "not authorized for database '{db}'"
                                )),
                            }
                        } else {
                            // The first history page is read before the
                            // reply, so a missing database, or one without
                            // CDC, is an error rather than a subscription
                            // that ends at once.
                            match history_page(&state, &db, since).await {
                                Ok(first) => {
                                    let handle = tokio::spawn(forward_subscription(
                                        sub_id.clone(),
                                        change_stream(state.clone(), db, first, since),
                                        event_tx.clone(),
                                    ));
                                    // A sub_id in use: the new subscription
                                    // replaces the old.
                                    if let Some(previous) = sub_tasks.insert(sub_id.clone(), handle)
                                    {
                                        previous.abort();
                                    }
                                    WsServerMessage::Subscribed { sub_id }
                                }
                                Err(e) => error_message(Some(sub_id), &e.0),
                            }
                        }
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
                if let Some((sub_id, item)) = event {
                    let msg = subscription_message(sub_id, item);
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

/// Forwards the change stream of subscription `sub_id` to `tx` until it
/// ends (after a lag notice or an error) or the connection goes away. The
/// connection stays open either way.
#[cfg(feature = "push-changefeed")]
async fn forward_subscription(
    sub_id: String,
    stream: impl futures_util::Stream<Item = crate::routes::change_stream::StreamItem>,
    tx: tokio::sync::mpsc::UnboundedSender<(String, crate::routes::change_stream::StreamItem)>,
) {
    let mut stream = std::pin::pin!(stream);
    while let Some(item) = stream.next().await {
        if tx.send((sub_id.clone(), item)).is_err() {
            return;
        }
    }
}

/// The message a change stream item of subscription `sub_id` becomes on the
/// socket.
#[cfg(feature = "push-changefeed")]
fn subscription_message(
    sub_id: String,
    item: crate::routes::change_stream::StreamItem,
) -> WsServerMessage {
    use crate::routes::change_stream::StreamItem;

    match item {
        StreamItem::Change(event) => WsServerMessage::Change { sub_id, event },
        StreamItem::Lagged(notice) => WsServerMessage::Error {
            id: Some(sub_id),
            error: "lagged".to_string(),
            detail: Some(
                serde_json::to_string(&notice).expect("a lag notice is always serializable"),
            ),
        },
        StreamItem::Error(message) => WsServerMessage::Error {
            id: Some(sub_id),
            error: "closed".to_string(),
            detail: Some(message),
        },
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
async fn process_query(
    state: &AppState,
    id: Option<String>,
    req: QueryRequest,
    identity: &Identity,
    db_scope: &[String],
) -> WsServerMessage {
    let db_name = grafeo_service::resolve_db_name(req.database.as_deref());

    // Check database scope before executing.
    if !db_scope.is_empty() && !db_scope.iter().any(|d| d == db_name) {
        return WsServerMessage::Error {
            id,
            error: "forbidden".to_string(),
            detail: Some(format!("not authorized for database '{db_name}'")),
        };
    }
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
        Some(identity.clone()),
    )
    .await;

    match result {
        Ok(qr) => WsServerMessage::Result {
            id,
            response: query_result_to_response(&qr),
        },
        Err(e) => error_message(id, &e),
    }
}

/// The error message for a failed query or subscribe. An internal error's
/// text stays in the log, as in the HTTP and SSE error mapping.
fn error_message(id: Option<String>, e: &ServiceError) -> WsServerMessage {
    let (error, detail) = match e {
        ServiceError::BadRequest(msg) => ("bad_request".to_string(), Some(msg.clone())),
        ServiceError::Timeout => ("timeout".to_string(), None),
        ServiceError::NotFound(msg) => ("not_found".to_string(), Some(msg.clone())),
        ServiceError::Internal(_) => {
            tracing::warn!(error = %e, "WebSocket request failed");
            (
                "internal_error".to_string(),
                Some("internal error".to_string()),
            )
        }
        _ => ("internal_error".to_string(), Some(e.to_string())),
    };
    WsServerMessage::Error { id, error, detail }
}

#[cfg(all(test, feature = "push-changefeed"))]
mod tests {
    use super::*;
    use crate::routes::change_stream::{FEED_CLOSED, StreamItem};

    #[test]
    fn an_internal_query_error_hides_its_detail() {
        let message = error_message(
            Some("q1".to_string()),
            &ServiceError::Internal("disk at /var/x full".to_string()),
        );
        let json = serde_json::to_value(message).unwrap();
        assert_eq!(json["error"], "internal_error");
        assert_eq!(json["detail"], "internal error");
        let bad = serde_json::to_value(error_message(
            None,
            &ServiceError::BadRequest("nope".into()),
        ))
        .unwrap();
        assert_eq!(bad["detail"], "nope");
    }

    #[tokio::test]
    async fn a_lagging_subscription_ends_with_a_lagged_error() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let notice = grafeo_service::changefeed::LaggedNotice {
            skipped: 7,
            since: 3,
        };
        forward_subscription(
            "s1".to_string(),
            futures_util::stream::iter([StreamItem::Lagged(notice)]),
            tx,
        )
        .await;

        let (sub_id, item) = rx.recv().await.expect("a lag message");
        let message = serde_json::to_value(subscription_message(sub_id, item)).unwrap();
        assert_eq!(
            message,
            serde_json::json!({
                "type": "error",
                "id": "s1",
                "error": "lagged",
                "detail": "{\"skipped\":7,\"since\":3}",
            })
        );
        assert!(rx.recv().await.is_none(), "the subscription has ended");
    }

    #[tokio::test]
    async fn a_closed_feed_ends_the_subscription_with_a_closed_error() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        forward_subscription(
            "s1".to_string(),
            futures_util::stream::iter([StreamItem::Error(FEED_CLOSED.to_string())]),
            tx,
        )
        .await;

        let (sub_id, item) = rx.recv().await.expect("a closed message");
        let message = serde_json::to_value(subscription_message(sub_id, item)).unwrap();
        assert_eq!(
            message,
            serde_json::json!({
                "type": "error",
                "id": "s1",
                "error": "closed",
                "detail": "change feed closed",
            })
        );
        assert!(rx.recv().await.is_none(), "the subscription has ended");
    }
}
