//! Sync service for offline-first applications.
//!
//! # Pull endpoint
//!
//! `GET /db/{name}/changes?since=<epoch>&limit=<n>`
//!
//! Clients poll `SyncService::pull()` with their last seen epoch and receive
//! all mutations since that point. The `server_epoch` in the response becomes
//! the `since` value for the next poll.
//!
//! # Push endpoint
//!
//! `POST /db/{name}/sync`
//!
//! Clients submit their local changesets. The server applies them with
//! last-write-wins (LWW) conflict resolution, comparing wall-clock timestamps.
//! Returns `{ server_epoch, applied, skipped, conflicts, id_mappings }`.
//!
//! # Protocol
//!
//! 1. On first sync, call `GET /changes?since=0` to receive all history.
//! 2. Store `response.server_epoch` locally.
//! 3. On reconnect, call with `since=stored_epoch` to receive only new changes.
//! 4. To push local changes, `POST /sync` with the changeset.
//! 5. Update local IDs for any creates using the returned `id_mappings`.
//!
//! # Limits
//!
//! Pull results are capped at `limit` events (max 10 000). If
//! `changes.len() == limit`, there may be more: poll again using the epoch of
//! the last returned event as `since`.

use std::collections::HashMap;

use grafeo_common::types::{EdgeId, NodeId};
use serde::{Deserialize, Serialize};

use crate::database::DatabaseManager;
use crate::error::ServiceError;

// ---------------------------------------------------------------------------
// Pull wire types
// ---------------------------------------------------------------------------

/// Response from `GET /db/{name}/changes`.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChangesResponse {
    /// Current server epoch. Use as `since` in your next poll.
    pub server_epoch: u64,
    /// Change events with epoch >= the requested `since` value, ordered by
    /// epoch ascending.
    pub changes: Vec<ChangeEventDto>,
}

/// A single mutation event, safe for wire serialization.
///
/// Converted from `grafeo_engine::cdc::ChangeEvent`. All IDs are raw `u64`
/// values; the `entity_type` field distinguishes nodes, edges, and triples.
///
/// For Create events the `labels` (nodes), `edge_type`, `src_id`, and
/// `dst_id` (edges) fields carry the structural metadata needed to replay the
/// create on a remote database. For RDF triple events the `triple_subject`,
/// `triple_predicate`, `triple_object`, and `triple_graph` fields carry the
/// N-Triples-encoded terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChangeEventDto {
    /// Raw entity ID (`NodeId::as_u64()`, `EdgeId::as_u64()`, or triple hash).
    pub id: u64,
    /// `"node"`, `"edge"`, or `"triple"`.
    pub entity_type: String,
    /// `"create"`, `"update"`, or `"delete"`.
    pub kind: String,
    /// MVCC epoch when this change was committed.
    pub epoch: u64,
    /// Wall-clock timestamp (milliseconds since Unix epoch).
    pub timestamp: u64,
    /// Properties before the change. Absent for creates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    /// Properties after the change. Absent for deletes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
    /// Node labels. Present only on node Create events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Edge relationship type. Present only on edge Create events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_type: Option<String>,
    /// Edge source node ID. Present only on edge Create events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_id: Option<u64>,
    /// Edge destination node ID. Present only on edge Create events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_id: Option<u64>,
    /// RDF triple subject (N-Triples encoded). Present only on triple events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triple_subject: Option<String>,
    /// RDF triple predicate (N-Triples encoded). Present only on triple events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triple_predicate: Option<String>,
    /// RDF triple object (N-Triples encoded). Present only on triple events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triple_object: Option<String>,
    /// Named graph containing the triple. `None` means the default graph.
    /// Present only on triple events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triple_graph: Option<String>,
}

// ---------------------------------------------------------------------------
// Push wire types
// ---------------------------------------------------------------------------

/// Request body for `POST /db/{name}/sync`.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SyncRequest {
    /// Opaque client identifier (device ID, user ID, etc.).
    pub client_id: String,
    /// The last server epoch the client has processed. Informational only;
    /// used for diagnostics and future incremental backup integration.
    #[serde(default)]
    pub last_seen_epoch: u64,
    /// The changes the client wants to apply to the server.
    pub changes: Vec<SyncChangeRequest>,
}

/// A single change the client wants to apply.
///
/// The `id` field holds the server-assigned entity ID and is required for
/// `"update"` and `"delete"` operations. For `"create"` operations it must
/// be omitted (or `null`); the server assigns a new ID and returns the
/// mapping in `SyncResponse.id_mappings`.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SyncChangeRequest {
    /// `"create"`, `"update"`, or `"delete"`.
    pub kind: String,
    /// `"node"` or `"edge"`.
    pub entity_type: String,
    /// Server entity ID. Required for update/delete; absent for create.
    pub id: Option<u64>,
    /// Wall-clock timestamp from the client (ms since Unix epoch).
    /// Used for LWW conflict detection on update and delete.
    #[serde(default)]
    pub timestamp: u64,
    /// Node labels. Required for node creates.
    pub labels: Option<Vec<String>>,
    /// Edge relationship type. Required for edge creates.
    pub edge_type: Option<String>,
    /// Edge source node ID. Required for edge creates.
    pub src_id: Option<u64>,
    /// Edge destination node ID. Required for edge creates.
    pub dst_id: Option<u64>,
    /// Properties to write. Required for creates; property-delta for updates.
    pub after: Option<serde_json::Value>,
}

/// Response from `POST /db/{name}/sync`.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SyncResponse {
    /// Current server epoch after all changes were applied.
    pub server_epoch: u64,
    /// Number of changes that were applied successfully.
    pub applied: usize,
    /// Number of changes that were skipped due to LWW conflicts.
    pub skipped: usize,
    /// Conflicts that were detected. Skipped changes appear here.
    pub conflicts: Vec<ConflictRecord>,
    /// Server-assigned IDs for each `"create"` change in the request, in
    /// the order they appeared.
    pub id_mappings: Vec<IdMapping>,
}

/// A conflict detected during sync apply.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConflictRecord {
    /// Zero-based index of the change in the request's `changes` array.
    pub request_index: usize,
    /// Human-readable reason for the conflict.
    pub reason: String,
}

/// Maps a client create request to the server-assigned entity ID.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IdMapping {
    /// Zero-based index of the create change in the request's `changes` array.
    pub request_index: usize,
    /// Server-assigned entity ID.
    pub server_id: u64,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Pull-based changefeed and push-based apply service.
///
/// Stateless — all state is borrowed from the `DatabaseManager`.
pub struct SyncService;

impl SyncService {
    /// Returns change events for `db_name` where `epoch >= since`.
    ///
    /// `since` is inclusive. Pass `0` for the full history. To poll for new
    /// changes after a previous sync, pass the `server_epoch` returned by
    /// that response. Because `since` is inclusive, the event at exactly
    /// `since` will be repeated on the next poll — clients should track the
    /// maximum epoch they have applied and skip duplicates.
    ///
    /// Results are ordered by epoch ascending and capped at `limit` (caller
    /// should clamp to <= 10 000).
    ///
    /// # Note on CDC scope
    ///
    /// CDC events are recorded by the direct CRUD API (`create_node`,
    /// `set_node_property`, etc.). Mutations executed via GQL sessions are not
    /// yet tracked in the CDC log — that integration is planned for a future
    /// phase. This endpoint is most useful for applications using grafeo as an
    /// embedded database via the Python, Node.js, or Dart bindings.
    pub fn pull(
        databases: &DatabaseManager,
        db_name: &str,
        since: u64,
        limit: usize,
    ) -> Result<ChangesResponse, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let server_epoch = entry.db.current_epoch().0;
        let since_id = grafeo_common::types::EpochId(since);
        let until_id = grafeo_common::types::EpochId(server_epoch);

        let raw = entry
            .db
            .changes_between(since_id, until_id)
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        let changes = raw.into_iter().take(limit).map(to_dto).collect();

        Ok(ChangesResponse {
            server_epoch,
            changes,
        })
    }

    /// Applies a client changeset to `db_name` using LWW conflict resolution.
    ///
    /// # Conflict resolution
    ///
    /// For `update` and `delete` operations the server checks its CDC log for
    /// the target entity. If the server has a recorded change with a
    /// wall-clock `timestamp` strictly greater than the client's
    /// `change.timestamp`, the server's version is considered newer and the
    /// client change is skipped (recorded in `conflicts`).
    ///
    /// `create` operations are never conflicted: the server assigns a fresh ID
    /// and returns the mapping in `id_mappings`.
    ///
    /// # Idempotency
    ///
    /// Replaying the same request twice may apply duplicate property writes
    /// for updates, but because the second replay's timestamps are identical
    /// to the first, the LWW check will be a no-op and the result will be
    /// the same state.
    pub fn apply(
        databases: &DatabaseManager,
        db_name: &str,
        request: SyncRequest,
    ) -> Result<SyncResponse, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let db = &entry.db;
        let mut applied = 0usize;
        let mut skipped = 0usize;
        let mut conflicts: Vec<ConflictRecord> = Vec::new();
        let mut id_mappings: Vec<IdMapping> = Vec::new();

        for (idx, change) in request.changes.iter().enumerate() {
            match change.kind.as_str() {
                "create" => match change.entity_type.as_str() {
                    "node" => {
                        let labels: Vec<&str> = change
                            .labels
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .map(String::as_str)
                            .collect();
                        let new_id = if let Some(after) = &change.after {
                            let props = json_to_props(after);
                            db.create_node_with_props(&labels, props)
                        } else {
                            db.create_node(&labels)
                        };
                        id_mappings.push(IdMapping {
                            request_index: idx,
                            server_id: new_id.as_u64(),
                        });
                        applied += 1;
                    }
                    "edge" => match (change.src_id, change.dst_id, &change.edge_type) {
                        (Some(src), Some(dst), Some(et)) => {
                            let new_id = if let Some(after) = &change.after {
                                let props = json_to_props(after);
                                db.create_edge_with_props(
                                    NodeId::new(src),
                                    NodeId::new(dst),
                                    et,
                                    props,
                                )
                            } else {
                                db.create_edge(NodeId::new(src), NodeId::new(dst), et)
                            };
                            id_mappings.push(IdMapping {
                                request_index: idx,
                                server_id: new_id.as_u64(),
                            });
                            applied += 1;
                        }
                        _ => {
                            conflicts.push(ConflictRecord {
                                request_index: idx,
                                reason: "edge_create_missing_src_dst_or_type".to_string(),
                            });
                        }
                    },
                    _ => {
                        conflicts.push(ConflictRecord {
                            request_index: idx,
                            reason: format!("unknown_entity_type:{}", change.entity_type),
                        });
                    }
                },

                "update" => {
                    let raw_id = match change.id {
                        Some(id) => id,
                        None => {
                            conflicts.push(ConflictRecord {
                                request_index: idx,
                                reason: "update_missing_id".to_string(),
                            });
                            continue;
                        }
                    };

                    let after = match &change.after {
                        Some(v) => v,
                        None => {
                            conflicts.push(ConflictRecord {
                                request_index: idx,
                                reason: "update_missing_after".to_string(),
                            });
                            continue;
                        }
                    };

                    match change.entity_type.as_str() {
                        "node" => {
                            let node_id = NodeId::new(raw_id);
                            if server_is_newer(
                                db,
                                grafeo_engine::cdc::EntityId::Node(node_id),
                                change.timestamp,
                            ) {
                                conflicts.push(ConflictRecord {
                                    request_index: idx,
                                    reason: "server_newer".to_string(),
                                });
                                skipped += 1;
                            } else {
                                for (key, val) in json_to_props(after) {
                                    db.set_node_property(node_id, &key, val);
                                }
                                applied += 1;
                            }
                        }
                        "edge" => {
                            let edge_id = EdgeId::new(raw_id);
                            if server_is_newer(
                                db,
                                grafeo_engine::cdc::EntityId::Edge(edge_id),
                                change.timestamp,
                            ) {
                                conflicts.push(ConflictRecord {
                                    request_index: idx,
                                    reason: "server_newer".to_string(),
                                });
                                skipped += 1;
                            } else {
                                for (key, val) in json_to_props(after) {
                                    db.set_edge_property(edge_id, &key, val);
                                }
                                applied += 1;
                            }
                        }
                        _ => {
                            conflicts.push(ConflictRecord {
                                request_index: idx,
                                reason: format!("unknown_entity_type:{}", change.entity_type),
                            });
                        }
                    }
                }

                "delete" => {
                    let raw_id = match change.id {
                        Some(id) => id,
                        None => {
                            conflicts.push(ConflictRecord {
                                request_index: idx,
                                reason: "delete_missing_id".to_string(),
                            });
                            continue;
                        }
                    };

                    match change.entity_type.as_str() {
                        "node" => {
                            let node_id = NodeId::new(raw_id);
                            if server_is_newer(
                                db,
                                grafeo_engine::cdc::EntityId::Node(node_id),
                                change.timestamp,
                            ) {
                                conflicts.push(ConflictRecord {
                                    request_index: idx,
                                    reason: "server_newer".to_string(),
                                });
                                skipped += 1;
                            } else {
                                db.delete_node(node_id);
                                applied += 1;
                            }
                        }
                        "edge" => {
                            let edge_id = EdgeId::new(raw_id);
                            if server_is_newer(
                                db,
                                grafeo_engine::cdc::EntityId::Edge(edge_id),
                                change.timestamp,
                            ) {
                                conflicts.push(ConflictRecord {
                                    request_index: idx,
                                    reason: "server_newer".to_string(),
                                });
                                skipped += 1;
                            } else {
                                db.delete_edge(edge_id);
                                applied += 1;
                            }
                        }
                        _ => {
                            conflicts.push(ConflictRecord {
                                request_index: idx,
                                reason: format!("unknown_entity_type:{}", change.entity_type),
                            });
                        }
                    }
                }

                _ => {
                    conflicts.push(ConflictRecord {
                        request_index: idx,
                        reason: format!("unknown_kind:{}", change.kind),
                    });
                }
            }
        }

        Ok(SyncResponse {
            server_epoch: db.current_epoch().0,
            applied,
            skipped,
            conflicts,
            id_mappings,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the server has a CDC event for `entity_id` with a
/// `timestamp` strictly greater than `client_timestamp`.
///
/// When there is no CDC history for the entity (e.g., it was created via a
/// GQL session, which does not record to the CDC log), the function returns
/// `false` — the client change is applied unconditionally.
fn server_is_newer(
    db: &grafeo_engine::GrafeoDB,
    entity_id: grafeo_engine::cdc::EntityId,
    client_timestamp: u64,
) -> bool {
    db.history(entity_id)
        .map(|events| events.iter().any(|e| e.timestamp > client_timestamp))
        .unwrap_or(false)
}

fn to_dto(event: grafeo_engine::cdc::ChangeEvent) -> ChangeEventDto {
    let (id, entity_type) = match event.entity_id {
        grafeo_engine::cdc::EntityId::Node(n) => (n.as_u64(), "node".to_string()),
        grafeo_engine::cdc::EntityId::Edge(e) => (e.as_u64(), "edge".to_string()),
        grafeo_engine::cdc::EntityId::Triple(h) => (h, "triple".to_string()),
    };
    let kind = match event.kind {
        grafeo_engine::cdc::ChangeKind::Create => "create".to_string(),
        grafeo_engine::cdc::ChangeKind::Update => "update".to_string(),
        grafeo_engine::cdc::ChangeKind::Delete => "delete".to_string(),
    };
    ChangeEventDto {
        id,
        entity_type,
        kind,
        epoch: event.epoch.0,
        timestamp: event.timestamp,
        before: event.before.map(props_to_json),
        after: event.after.map(props_to_json),
        labels: event.labels,
        edge_type: event.edge_type,
        src_id: event.src_id,
        dst_id: event.dst_id,
        triple_subject: event.triple_subject,
        triple_predicate: event.triple_predicate,
        triple_object: event.triple_object,
        triple_graph: event.triple_graph,
    }
}

fn props_to_json(props: HashMap<String, grafeo_common::Value>) -> serde_json::Value {
    serde_json::to_value(props)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::default()))
}

/// Converts a JSON object of properties back into typed grafeo `Value` pairs.
///
/// The JSON must use grafeo's serde representation for `Value` (externally
/// tagged enum, e.g. `{"String": "Alix"}`, `{"Int64": 42}`). Unknown or
/// malformed keys are silently skipped.
fn json_to_props(json: &serde_json::Value) -> impl Iterator<Item = (String, grafeo_common::Value)> {
    let map: HashMap<String, grafeo_common::Value> =
        serde_json::from_value(json.clone()).unwrap_or_default();
    map.into_iter()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseManager;

    fn make_manager() -> DatabaseManager {
        DatabaseManager::new(None, false)
    }

    // CDC is populated by the direct CRUD API (create_node, set_node_property,
    // etc.), not by GQL session-based mutations. All direct API calls record
    // at the current epoch (typically 0 for a fresh in-memory DB since no
    // transaction commits advance the epoch counter).

    #[test]
    fn pull_empty_database_returns_no_changes() {
        let mgr = make_manager();
        let resp = SyncService::pull(&mgr, "default", 0, 1000).unwrap();
        assert!(resp.changes.is_empty());
    }

    #[test]
    fn pull_unknown_database_returns_not_found() {
        let mgr = make_manager();
        let err = SyncService::pull(&mgr, "nonexistent", 0, 1000).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn pull_returns_direct_api_mutations() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();

        // Direct API calls record to the shared CDC log.
        entry.db.create_node(&["Thing"]);
        entry.db.create_node(&["Thing"]);
        entry.db.create_node(&["Thing"]);

        let resp = SyncService::pull(&mgr, "default", 0, 1000).unwrap();
        assert_eq!(resp.changes.len(), 3);
        assert!(resp.changes.iter().all(|e| e.kind == "create"));
        assert!(resp.changes.iter().all(|e| e.entity_type == "node"));
    }

    #[test]
    fn pull_respects_limit() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        for _ in 0..5 {
            entry.db.create_node(&["Thing"]);
        }
        let resp = SyncService::pull(&mgr, "default", 0, 3).unwrap();
        assert_eq!(resp.changes.len(), 3);
    }

    #[test]
    fn pull_since_filters_by_epoch() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();

        // Record events at epoch 0 (direct API, fresh DB).
        entry.db.create_node(&["A"]);
        entry.db.create_node(&["B"]);

        let full = SyncService::pull(&mgr, "default", 0, 1000).unwrap();
        assert_eq!(full.changes.len(), 2);

        // Since server_epoch (inclusive) re-returns events at that epoch.
        // Clients handle duplicates by tracking max applied epoch themselves.
        // Passing server_epoch+1 skips all seen events.
        let next = full.server_epoch.saturating_add(1);
        let empty = SyncService::pull(&mgr, "default", next, 1000).unwrap();
        assert!(empty.changes.is_empty());
    }

    #[test]
    fn change_event_dto_serializes_cleanly() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        entry.db.create_node(&["Person"]);

        let resp = SyncService::pull(&mgr, "default", 0, 1000).unwrap();
        assert_eq!(resp.changes.len(), 1);

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"entity_type\":\"node\""));
        assert!(json.contains("\"kind\":\"create\""));
        // server_epoch is serialized as a number.
        assert!(json.contains("\"server_epoch\":"));
        // Labels appear on create events.
        assert!(json.contains("\"labels\":[\"Person\"]"));
    }

    #[test]
    fn apply_unknown_database_returns_not_found() {
        let mgr = make_manager();
        let req = SyncRequest {
            client_id: "test".to_string(),
            last_seen_epoch: 0,
            changes: vec![],
        };
        let err = SyncService::apply(&mgr, "nonexistent", req).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn apply_create_node_assigns_server_id() {
        let mgr = make_manager();
        let req = SyncRequest {
            client_id: "device-1".to_string(),
            last_seen_epoch: 0,
            changes: vec![SyncChangeRequest {
                kind: "create".to_string(),
                entity_type: "node".to_string(),
                id: None,
                timestamp: 0,
                labels: Some(vec!["Person".to_string()]),
                edge_type: None,
                src_id: None,
                dst_id: None,
                after: None,
            }],
        };
        let resp = SyncService::apply(&mgr, "default", req).unwrap();
        assert_eq!(resp.applied, 1);
        assert_eq!(resp.id_mappings.len(), 1);
        assert_eq!(resp.id_mappings[0].request_index, 0);
        // Valid IDs are any value except u64::MAX (NodeId::INVALID).
        assert_ne!(resp.id_mappings[0].server_id, u64::MAX);
        assert!(resp.conflicts.is_empty());
    }

    #[test]
    fn apply_update_node_applies_properties() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        let node = entry.db.create_node(&["Person"]);

        let req = SyncRequest {
            client_id: "device-1".to_string(),
            last_seen_epoch: 0,
            changes: vec![SyncChangeRequest {
                kind: "update".to_string(),
                entity_type: "node".to_string(),
                id: Some(node.as_u64()),
                timestamp: u64::MAX, // very new — no server conflict
                labels: None,
                edge_type: None,
                src_id: None,
                dst_id: None,
                after: Some(serde_json::json!({ "name": { "String": "Alix" } })),
            }],
        };
        let resp = SyncService::apply(&mgr, "default", req).unwrap();
        assert_eq!(resp.applied, 1);
        assert!(resp.conflicts.is_empty());

        // Verify property was written.
        let val = entry.db.get_node(node).unwrap();
        assert!(
            val.properties
                .contains_key(&grafeo_common::types::PropertyKey::new("name"))
        );
    }

    #[test]
    fn apply_delete_node_removes_entity() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        let node = entry.db.create_node(&["Thing"]);

        let req = SyncRequest {
            client_id: "device-1".to_string(),
            last_seen_epoch: 0,
            changes: vec![SyncChangeRequest {
                kind: "delete".to_string(),
                entity_type: "node".to_string(),
                id: Some(node.as_u64()),
                timestamp: u64::MAX,
                labels: None,
                edge_type: None,
                src_id: None,
                dst_id: None,
                after: None,
            }],
        };
        let resp = SyncService::apply(&mgr, "default", req).unwrap();
        assert_eq!(resp.applied, 1);
        assert!(resp.conflicts.is_empty());
        assert!(entry.db.get_node(node).is_none());
    }

    #[test]
    fn apply_lww_conflict_skips_stale_update() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        let node = entry.db.create_node(&["Person"]);
        // Write a property — this records a CDC event with a recent timestamp.
        entry
            .db
            .set_node_property(node, "name", grafeo_common::types::Value::from("Gus"));

        // Client sends an update with timestamp 0 (very old).
        let req = SyncRequest {
            client_id: "device-1".to_string(),
            last_seen_epoch: 0,
            changes: vec![SyncChangeRequest {
                kind: "update".to_string(),
                entity_type: "node".to_string(),
                id: Some(node.as_u64()),
                timestamp: 0, // older than server's CDC event
                labels: None,
                edge_type: None,
                src_id: None,
                dst_id: None,
                after: Some(serde_json::json!({ "name": { "String": "Stale" } })),
            }],
        };
        let resp = SyncService::apply(&mgr, "default", req).unwrap();
        assert_eq!(resp.skipped, 1);
        assert_eq!(resp.conflicts.len(), 1);
        assert_eq!(resp.conflicts[0].reason, "server_newer");
        // Property should still be the server's value.
        let node_data = entry.db.get_node(node).unwrap();
        let name_key = grafeo_common::types::PropertyKey::new("name");
        assert_eq!(
            node_data.properties.get(&name_key),
            Some(&grafeo_common::types::Value::from("Gus"))
        );
    }

    #[test]
    fn pull_creates_carry_labels() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        entry.db.create_node(&["Company", "Startup"]);

        let resp = SyncService::pull(&mgr, "default", 0, 1000).unwrap();
        let event = &resp.changes[0];
        let labels = event.labels.as_ref().unwrap();
        assert!(labels.contains(&"Company".to_string()));
        assert!(labels.contains(&"Startup".to_string()));
    }

    #[test]
    fn pull_edge_creates_carry_src_dst_type() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();
        let alix = entry.db.create_node(&["Person"]);
        let gus = entry.db.create_node(&["Person"]);
        entry.db.create_edge(alix, gus, "KNOWS");

        let resp = SyncService::pull(&mgr, "default", 0, 1000).unwrap();
        let edge_event = resp
            .changes
            .iter()
            .find(|e| e.entity_type == "edge")
            .unwrap();
        assert_eq!(edge_event.edge_type.as_deref(), Some("KNOWS"));
        assert_eq!(edge_event.src_id, Some(alix.as_u64()));
        assert_eq!(edge_event.dst_id, Some(gus.as_u64()));
    }
}
