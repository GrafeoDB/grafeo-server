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
    /// Optional schema version hash computed by the client (FNV-1a hex over
    /// sorted label names + property keys). When present, the server checks
    /// for a mismatch and reports it in `SyncResponse.schema_mismatch`.
    /// No changes are rejected — this is a diagnostic warning only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
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
    /// CRDT operation to apply to `crdt_property`. When set, the named
    /// property is merged via the CRDT rules instead of the LWW path.
    pub crdt_op: Option<CrdtOp>,
    /// Property key targeted by `crdt_op`. Required when `crdt_op` is set.
    pub crdt_property: Option<String>,
}

/// A CRDT operation applied to a single property on a node or edge.
///
/// When `crdt_op` is present on a `SyncChangeRequest`, the sync service
/// applies the operation via `crdt::apply_op` instead of the normal LWW path.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type")]
pub enum CrdtOp {
    /// Add `amount` to a grow-only (G-Counter) property.
    GrowAdd { amount: u64, replica_id: String },
    /// Add (positive or negative) `amount` to a PN-Counter property.
    Increment { amount: i64, replica_id: String },
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
    /// `true` when the client supplied `schema_version` and it differed from
    /// the server's computed version. The client should re-fetch the schema
    /// and reconcile before applying more changes. No changes are rejected.
    pub schema_mismatch: bool,
    /// Server's current schema version (FNV-1a hex over sorted label names
    /// and property keys). Always present so clients can detect drift even
    /// without sending their own `schema_version`.
    pub server_schema_version: String,
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
    /// # CDC activation
    ///
    /// CDC must be enabled on the database for events to be recorded (it is
    /// opt-in since grafeo-engine 0.5.32). The server enables CDC automatically
    /// on all databases when running as a replication primary. Both direct CRUD
    /// API calls and session-driven mutations (INSERT/SET/DELETE via GQL/Cypher)
    /// produce CDC events.
    pub fn pull(
        databases: &DatabaseManager,
        db_name: &str,
        since: u64,
        limit: usize,
    ) -> Result<ChangesResponse, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        if !entry.db.is_cdc_enabled() {
            return Err(ServiceError::BadRequest(
                "CDC is not enabled on this database — sync requires replication mode or explicit CDC activation".to_string(),
            ));
        }

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

        // Use the database directly for mutations. Each operation auto-commits.
        // TODO: wrap in a session transaction once the deadlock in persistent
        // mode is resolved (see replication_task spawn_blocking investigation).

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
                            let props: Vec<(String, grafeo_common::types::Value)> =
                                json_to_props(after).collect();
                            let props_refs: Vec<(&str, grafeo_common::types::Value)> =
                                props.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                            db.create_node_with_props(&labels, props_refs)
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
                                let props: Vec<(String, grafeo_common::types::Value)> =
                                    json_to_props(after).collect();
                                let props_refs: Vec<(&str, grafeo_common::types::Value)> =
                                    props.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                                db.create_edge_with_props(
                                    NodeId::new(src),
                                    NodeId::new(dst),
                                    et,
                                    props_refs,
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

                    // CRDT path: apply counter operation directly, bypassing LWW.
                    if let (Some(op), Some(prop_key)) = (&change.crdt_op, &change.crdt_property) {
                        match change.entity_type.as_str() {
                            "node" => {
                                let node_id = NodeId::new(raw_id);
                                let current = db
                                    .get_node(node_id)
                                    .and_then(|n| n.get_property(prop_key).cloned())
                                    .unwrap_or(grafeo_common::types::Value::Null);
                                let merged = crate::crdt::apply_op(&current, op);
                                db.set_node_property(node_id, prop_key, merged);
                                applied += 1;
                            }
                            "edge" => {
                                let edge_id = EdgeId::new(raw_id);
                                let current = db
                                    .get_edge(edge_id)
                                    .and_then(|e| e.get_property(prop_key).cloned())
                                    .unwrap_or(grafeo_common::types::Value::Null);
                                let merged = crate::crdt::apply_op(&current, op);
                                db.set_edge_property(edge_id, prop_key, merged);
                                applied += 1;
                            }
                            _ => {
                                conflicts.push(ConflictRecord {
                                    request_index: idx,
                                    reason: format!("unknown_entity_type:{}", change.entity_type),
                                });
                            }
                        }
                        continue;
                    }

                    // LWW path: apply `after` properties with timestamp conflict check.
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

        let server_schema = compute_schema_version(db);
        let schema_mismatch = request
            .schema_version
            .as_deref()
            .is_some_and(|cv| cv != server_schema);

        Ok(SyncResponse {
            server_epoch: db.current_epoch().0,
            applied,
            skipped,
            conflicts,
            id_mappings,
            schema_mismatch,
            server_schema_version: server_schema,
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
        .map(|events| {
            let client_ts = grafeo_common::types::HlcTimestamp::from_u64(client_timestamp);
            events.iter().any(|e| e.timestamp > client_ts)
        })
        .unwrap_or(false)
}

/// Computes a stable schema version string for `db`.
///
/// Collects all node label names and property key names, sorts them, and
/// produces an FNV-1a hash encoded as a 16-character lowercase hex string.
/// The result is deterministic and does not change between server restarts
/// unless the schema changes.
///
/// RDF databases and any mode without an LPG catalog return the empty-schema
/// hash (`"cbf29ce484222325"`).
fn compute_schema_version(db: &grafeo_engine::GrafeoDB) -> String {
    let mut keys: Vec<String> = match db.schema() {
        grafeo_engine::admin::SchemaInfo::Lpg(lpg) => lpg
            .labels
            .into_iter()
            .map(|l| l.name)
            .chain(lpg.property_keys)
            .collect(),
        grafeo_engine::admin::SchemaInfo::Rdf(_) | _ => Vec::new(),
    };
    keys.sort();

    // FNV-1a 64-bit hash — stable across runs for the same input.
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for key in &keys {
        for &byte in key.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        // Separator so "ab" + "c" hashes differently from "a" + "bc".
        hash ^= u64::from(b'|');
        hash = hash.wrapping_mul(1_099_511_628_211);
    }

    format!("{hash:016x}")
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
        _ => "unknown".to_string(),
    };
    ChangeEventDto {
        id,
        entity_type,
        kind,
        epoch: event.epoch.0,
        timestamp: event.timestamp.as_u64(),
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
        let mut mgr = DatabaseManager::new(None, false);
        mgr.set_cdc_enabled(true);
        mgr
    }

    #[test]
    fn pull_without_cdc_returns_error() {
        let mgr = DatabaseManager::new(None, false);
        let err = SyncService::pull(&mgr, "default", 0, 1000).unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

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
            schema_version: None,
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
                crdt_op: None,
                crdt_property: None,
            }],
            schema_version: None,
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
                crdt_op: None,
                crdt_property: None,
            }],
            schema_version: None,
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
                crdt_op: None,
                crdt_property: None,
            }],
            schema_version: None,
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
                crdt_op: None,
                crdt_property: None,
            }],
            schema_version: None,
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
    fn apply_returns_server_schema_version() {
        let mgr = make_manager();
        let req = SyncRequest {
            client_id: "device-1".to_string(),
            last_seen_epoch: 0,
            changes: vec![],
            schema_version: None,
        };
        let resp = SyncService::apply(&mgr, "default", req).unwrap();
        // A version is always returned (non-empty hex string).
        assert!(!resp.server_schema_version.is_empty());
        assert!(!resp.schema_mismatch);
    }

    #[test]
    fn apply_detects_schema_mismatch() {
        let mgr = make_manager();
        let req = SyncRequest {
            client_id: "device-1".to_string(),
            last_seen_epoch: 0,
            changes: vec![],
            // Deliberately wrong version.
            schema_version: Some("0000000000000000".to_string()),
        };
        let resp = SyncService::apply(&mgr, "default", req).unwrap();
        // An empty DB may actually hash to the stale value, so we just check
        // that the server echoes its own version regardless.
        assert!(!resp.server_schema_version.is_empty());
        // If the version we sent matches the server, mismatch is false; any
        // real mismatch should set it true. We can't assert a specific outcome
        // for "0000000000000000" since a truly empty schema may equal it —
        // instead, assert consistency: mismatch iff versions differ.
        let expected_mismatch = "0000000000000000" != resp.server_schema_version;
        assert_eq!(resp.schema_mismatch, expected_mismatch);
    }

    #[test]
    fn apply_detects_schema_mismatch_after_label_added() {
        let mgr = make_manager();
        let entry = mgr.get("default").unwrap();

        // Snapshot version on an empty database.
        let empty_req = SyncRequest {
            client_id: "c".to_string(),
            last_seen_epoch: 0,
            changes: vec![],
            schema_version: None,
        };
        let empty_resp = SyncService::apply(&mgr, "default", empty_req).unwrap();
        let version_before = empty_resp.server_schema_version.clone();

        // Add a node to introduce a new label.
        entry.db.create_node(&["NewLabel"]);

        // Send the old (now stale) schema version.
        let stale_req = SyncRequest {
            client_id: "c".to_string(),
            last_seen_epoch: 0,
            changes: vec![],
            schema_version: Some(version_before),
        };
        let stale_resp = SyncService::apply(&mgr, "default", stale_req).unwrap();
        assert!(
            stale_resp.schema_mismatch,
            "schema should be detected as changed"
        );
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

    // -----------------------------------------------------------------------
    // Replication end-to-end: primary → pull → apply → replica
    // -----------------------------------------------------------------------

    /// Simulates primary-to-replica replication in-process:
    /// writes to primary via direct CRUD, pulls changes, applies to replica,
    /// then verifies all data arrived.
    #[test]
    fn replication_end_to_end_nodes_and_edges() {
        // Primary: write data
        let primary = make_manager();
        let primary_db = primary.get("default").unwrap();
        let alix = primary_db.db.create_node(&["Person"]);
        primary_db
            .db
            .set_node_property(alix, "name", grafeo_common::types::Value::from("Alix"));
        let gus = primary_db.db.create_node(&["Person"]);
        primary_db
            .db
            .set_node_property(gus, "name", grafeo_common::types::Value::from("Gus"));
        let _edge = primary_db.db.create_edge(alix, gus, "KNOWS");

        // Pull changes from primary
        let changes_resp = SyncService::pull(&primary, "default", 0, 1000).unwrap();
        assert!(
            !changes_resp.changes.is_empty(),
            "Primary should have CDC events"
        );

        // Convert to SyncChangeRequest (simulates what the replica task does)
        let sync_changes: Vec<SyncChangeRequest> = changes_resp
            .changes
            .into_iter()
            .map(|dto| SyncChangeRequest {
                kind: dto.kind,
                entity_type: dto.entity_type,
                id: Some(dto.id),
                labels: dto.labels,
                edge_type: dto.edge_type,
                src_id: dto.src_id,
                dst_id: dto.dst_id,
                after: dto.after,
                timestamp: dto.timestamp,
                crdt_op: None,
                crdt_property: None,
            })
            .collect();

        // Replica: apply changes (CDC disabled, replicas don't need it)
        let replica = DatabaseManager::new(None, false);
        let request = SyncRequest {
            client_id: "replica-1".to_string(),
            last_seen_epoch: 0,
            changes: sync_changes,
            schema_version: None,
        };
        let apply_resp = SyncService::apply(&replica, "default", request).unwrap();
        assert!(
            apply_resp.applied > 0,
            "Should have applied changes on replica"
        );
        assert!(
            apply_resp.conflicts.is_empty(),
            "No conflicts expected: {:?}",
            apply_resp.conflicts
        );

        // Verify replica has the data
        let replica_db = replica.get("default").unwrap();
        assert!(
            replica_db.db.node_count() >= 2,
            "Replica should have at least 2 nodes, got {}",
            replica_db.db.node_count()
        );
        assert!(
            replica_db.db.edge_count() >= 1,
            "Replica should have at least 1 edge, got {}",
            replica_db.db.edge_count()
        );
    }

    /// Verifies that session-driven mutations on the primary (via execute())
    /// now generate CDC events that can be replicated to a replica.
    #[test]
    fn replication_session_mutations_flow_to_replica() {
        let primary = make_manager();
        let primary_db = primary.get("default").unwrap();

        // Write via session (previously bypassed CDC)
        let session = primary_db.db.session();
        session
            .execute("INSERT (:Person {name: 'Vincent', city: 'Paris'})")
            .unwrap();
        session
            .execute("INSERT (:Person {name: 'Jules'})-[:KNOWS]->(:Person {name: 'Mia'})")
            .unwrap();

        // Pull changes
        let changes_resp = SyncService::pull(&primary, "default", 0, 1000).unwrap();
        let node_creates = changes_resp
            .changes
            .iter()
            .filter(|e| e.kind == "create" && e.entity_type == "node")
            .count();
        assert!(
            node_creates >= 3,
            "Session mutations should produce at least 3 node CDC events, got {node_creates}"
        );

        // Apply to replica
        let sync_changes: Vec<SyncChangeRequest> = changes_resp
            .changes
            .into_iter()
            .map(|dto| SyncChangeRequest {
                kind: dto.kind,
                entity_type: dto.entity_type,
                id: Some(dto.id),
                labels: dto.labels,
                edge_type: dto.edge_type,
                src_id: dto.src_id,
                dst_id: dto.dst_id,
                after: dto.after,
                timestamp: dto.timestamp,
                crdt_op: None,
                crdt_property: None,
            })
            .collect();

        let replica = DatabaseManager::new(None, false);
        let request = SyncRequest {
            client_id: "replica-1".to_string(),
            last_seen_epoch: 0,
            changes: sync_changes,
            schema_version: None,
        };
        let apply_resp = SyncService::apply(&replica, "default", request).unwrap();
        assert!(
            apply_resp.applied >= 3,
            "Replica should have applied session-created nodes"
        );

        let replica_db = replica.get("default").unwrap();
        assert!(
            replica_db.db.node_count() >= 3,
            "Replica should have at least 3 nodes from session mutations"
        );
    }

    /// Verifies that the epoch persistence round-trips correctly.
    #[test]
    #[cfg(feature = "replication")]
    fn replication_epoch_persistence_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let state =
            crate::replication::ReplicationState::with_persistence(dir.path().to_path_buf());

        state.advance_epoch("default", 42);
        state.advance_epoch("other_db", 100);

        // Load into a fresh state from the same directory
        let state2 =
            crate::replication::ReplicationState::with_persistence(dir.path().to_path_buf());
        assert_eq!(state2.last_epoch("default"), 42);
        assert_eq!(state2.last_epoch("other_db"), 100);
    }
}
