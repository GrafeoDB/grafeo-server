//! Maelstrom node testing grafeo-server's primary-replica replication model.
//!
//! Implements the Maelstrom `lin-kv` protocol with primary-replica async
//! replication: n0 is always primary, other nodes are replicas. Writes on
//! replicas are forwarded to n0, which applies and broadcasts to all replicas.
//! Reads are served locally (eventual consistency).
//!
//! ```bash
//! cargo build --release -p grafeo-maelstrom
//! maelstrom test -w lin-kv \
//!   --bin target/release/grafeo-maelstrom \
//!   --node-count 3 --time-limit 20 --rate 100 \
//!   --consistency-models read-uncommitted
//! ```

use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufWriter, Write};

// ---------------------------------------------------------------------------
// HLC (simplified, non-atomic, single-threaded)
// ---------------------------------------------------------------------------

/// Hybrid Logical Clock: upper 48 bits physical ms, lower 16 bits logical.
struct HlcCounter {
    last: u64,
}

impl HlcCounter {
    fn new() -> Self {
        Self { last: 0 }
    }

    /// Advance the clock and return a new timestamp.
    fn tick(&mut self) -> u64 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let now_packed = now_ms << 16;

        if now_packed > self.last {
            self.last = now_packed;
        } else {
            self.last += 1;
        }
        self.last
    }

    /// Merge with a received timestamp, advancing past both.
    fn update(&mut self, received: u64) {
        if received > self.last {
            self.last = received + 1;
        } else {
            self.last += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

struct StampedValue {
    value: Value,
    hlc: u64,
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

struct Node {
    id: String,
    node_ids: Vec<String>,
    store: BTreeMap<String, StampedValue>,
    hlc: HlcCounter,
    next_msg_id: u64,
    /// Tracks forwarded writes awaiting primary acknowledgment.
    pending: HashMap<u64, PendingForward>,
}

struct PendingForward {
    client_src: String,
    client_msg_id: Value,
}

/// Normalizes a Maelstrom key (integer or string) to a consistent string.
fn key_str(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Node {
    fn new() -> Self {
        Self {
            id: String::new(),
            node_ids: Vec::new(),
            store: BTreeMap::new(),
            hlc: HlcCounter::new(),
            next_msg_id: 0,
            pending: HashMap::new(),
        }
    }

    fn msg_id(&mut self) -> u64 {
        self.next_msg_id += 1;
        self.next_msg_id
    }

    fn is_primary(&self) -> bool {
        self.node_ids.first().is_some_and(|n| n == &self.id)
    }

    fn primary_id(&self) -> String {
        self.node_ids[0].clone()
    }

    fn replica_ids(&self) -> Vec<String> {
        let primary = self.primary_id();
        self.node_ids
            .iter()
            .filter(|n| **n != primary)
            .cloned()
            .collect()
    }

    /// Emit a message to the output buffer.
    fn emit(from: &str, to: &str, body: Value, out: &mut Vec<Value>) {
        out.push(json!({"src": from, "dest": to, "body": body}));
    }

    /// Send a reply to a request.
    fn reply(&mut self, dest: &str, in_reply_to: &Value, mut body: Value, out: &mut Vec<Value>) {
        let mid = self.msg_id();
        body["msg_id"] = json!(mid);
        body["in_reply_to"] = in_reply_to.clone();
        Self::emit(&self.id, dest, body, out);
    }

    /// Broadcast a replicate message to all replicas.
    fn broadcast_replicate(&mut self, key: &str, value: &Value, hlc: u64, out: &mut Vec<Value>) {
        let id = self.id.clone();
        let replicas = self.replica_ids();
        for replica in &replicas {
            let mid = self.msg_id();
            Self::emit(
                &id,
                replica,
                json!({
                    "type": "replicate",
                    "msg_id": mid,
                    "key": key,
                    "value": value,
                    "hlc": hlc,
                }),
                out,
            );
        }
    }

    /// Process one incoming message, returning zero or more outgoing messages.
    fn handle(&mut self, msg: Value) -> Vec<Value> {
        let mut out = Vec::new();

        let src = msg["src"].as_str().unwrap_or("").to_string();
        let body = &msg["body"];
        let msg_type = body["type"].as_str().unwrap_or("");
        let req_msg_id = &body["msg_id"];

        match msg_type {
            // ----- Maelstrom lifecycle -----
            "init" => {
                self.id = body["node_id"].as_str().unwrap_or("").to_string();
                self.node_ids = body["node_ids"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                self.reply(&src, req_msg_id, json!({"type": "init_ok"}), &mut out);
            }

            // ----- KV: read -----
            "read" => {
                let key = key_str(&body["key"]);
                match self.store.get(&key) {
                    Some(sv) => {
                        self.reply(
                            &src,
                            req_msg_id,
                            json!({"type": "read_ok", "value": sv.value}),
                            &mut out,
                        );
                    }
                    None => {
                        self.reply(
                            &src,
                            req_msg_id,
                            json!({"type": "error", "code": 20, "text": "key does not exist"}),
                            &mut out,
                        );
                    }
                }
            }

            // ----- KV: write -----
            "write" => {
                let key = key_str(&body["key"]);
                let value = body["value"].clone();

                if self.is_primary() {
                    let hlc = self.hlc.tick();
                    self.store.insert(key.clone(), StampedValue { value: value.clone(), hlc });
                    self.reply(&src, req_msg_id, json!({"type": "write_ok"}), &mut out);
                    self.broadcast_replicate(&key, &value, hlc, &mut out);
                } else {
                    // Forward to primary
                    let mid = self.msg_id();
                    self.pending.insert(mid, PendingForward {
                        client_src: src.clone(),
                        client_msg_id: req_msg_id.clone(),
                    });
                    let primary = self.primary_id();
                    Self::emit(
                        &self.id,
                        &primary,
                        json!({
                            "type": "fwd_write",
                            "msg_id": mid,
                            "key": body["key"],
                            "value": value,
                        }),
                        &mut out,
                    );
                }
            }

            // ----- KV: compare-and-swap -----
            "cas" => {
                let key = key_str(&body["key"]);
                let from = &body["from"];
                let to = body["to"].clone();

                if self.is_primary() {
                    match self.store.get(&key) {
                        Some(sv) if sv.value == *from => {
                            let hlc = self.hlc.tick();
                            self.store.insert(key.clone(), StampedValue { value: to.clone(), hlc });
                            self.reply(&src, req_msg_id, json!({"type": "cas_ok"}), &mut out);
                            self.broadcast_replicate(&key, &to, hlc, &mut out);
                        }
                        Some(_) => {
                            self.reply(
                                &src,
                                req_msg_id,
                                json!({"type": "error", "code": 22, "text": "precondition failed"}),
                                &mut out,
                            );
                        }
                        None => {
                            self.reply(
                                &src,
                                req_msg_id,
                                json!({"type": "error", "code": 20, "text": "key does not exist"}),
                                &mut out,
                            );
                        }
                    }
                } else {
                    // Forward CAS to primary
                    let mid = self.msg_id();
                    self.pending.insert(mid, PendingForward {
                        client_src: src.clone(),
                        client_msg_id: req_msg_id.clone(),
                    });
                    let primary = self.primary_id();
                    Self::emit(
                        &self.id,
                        &primary,
                        json!({
                            "type": "fwd_cas",
                            "msg_id": mid,
                            "key": body["key"],
                            "from": from,
                            "to": to,
                        }),
                        &mut out,
                    );
                }
            }

            // ----- Inter-node: forwarded write (primary receives) -----
            "fwd_write" => {
                let key = key_str(&body["key"]);
                let value = body["value"].clone();
                let hlc = self.hlc.tick();
                self.store.insert(key.clone(), StampedValue { value: value.clone(), hlc });
                self.reply(&src, req_msg_id, json!({"type": "fwd_write_ok"}), &mut out);
                self.broadcast_replicate(&key, &value, hlc, &mut out);
            }

            // ----- Inter-node: forwarded write ack (replica receives) -----
            "fwd_write_ok" => {
                let in_reply = body["in_reply_to"].as_u64().unwrap_or(0);
                if let Some(fwd) = self.pending.remove(&in_reply) {
                    self.reply(
                        &fwd.client_src,
                        &fwd.client_msg_id,
                        json!({"type": "write_ok"}),
                        &mut out,
                    );
                }
            }

            // ----- Inter-node: forwarded CAS (primary receives) -----
            "fwd_cas" => {
                let key = key_str(&body["key"]);
                let from = &body["from"];
                let to = body["to"].clone();

                match self.store.get(&key) {
                    Some(sv) if sv.value == *from => {
                        let hlc = self.hlc.tick();
                        self.store.insert(key.clone(), StampedValue { value: to.clone(), hlc });
                        self.reply(&src, req_msg_id, json!({"type": "fwd_cas_ok"}), &mut out);
                        self.broadcast_replicate(&key, &to, hlc, &mut out);
                    }
                    Some(_) => {
                        self.reply(
                            &src,
                            req_msg_id,
                            json!({"type": "fwd_cas_err", "code": 22, "text": "precondition failed"}),
                            &mut out,
                        );
                    }
                    None => {
                        self.reply(
                            &src,
                            req_msg_id,
                            json!({"type": "fwd_cas_err", "code": 20, "text": "key does not exist"}),
                            &mut out,
                        );
                    }
                }
            }

            // ----- Inter-node: forwarded CAS success (replica receives) -----
            "fwd_cas_ok" => {
                let in_reply = body["in_reply_to"].as_u64().unwrap_or(0);
                if let Some(fwd) = self.pending.remove(&in_reply) {
                    self.reply(
                        &fwd.client_src,
                        &fwd.client_msg_id,
                        json!({"type": "cas_ok"}),
                        &mut out,
                    );
                }
            }

            // ----- Inter-node: forwarded CAS failure (replica receives) -----
            "fwd_cas_err" => {
                let in_reply = body["in_reply_to"].as_u64().unwrap_or(0);
                if let Some(fwd) = self.pending.remove(&in_reply) {
                    self.reply(
                        &fwd.client_src,
                        &fwd.client_msg_id,
                        json!({
                            "type": "error",
                            "code": body["code"],
                            "text": body["text"],
                        }),
                        &mut out,
                    );
                }
            }

            // ----- Inter-node: replication (replica receives from primary) -----
            "replicate" => {
                let key = key_str(&body["key"]);
                let value = body["value"].clone();
                let remote_hlc = body["hlc"].as_u64().unwrap_or(0);

                // LWW: only apply if remote is newer
                let dominated = self
                    .store
                    .get(&key)
                    .is_some_and(|sv| sv.hlc >= remote_hlc);

                if !dominated {
                    self.store.insert(key, StampedValue { value, hlc: remote_hlc });
                }
                self.hlc.update(remote_hlc);
                self.reply(&src, req_msg_id, json!({"type": "replicate_ok"}), &mut out);
            }

            // Ack from replica after applying replication (ignored)
            "replicate_ok" => {}

            _ => {
                eprintln!("unknown message type: {msg_type}");
            }
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Main event loop
// ---------------------------------------------------------------------------

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let mut node = Node::new();

    for line in stdin.lock().lines() {
        let line = line.expect("failed to read stdin");
        if line.is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(&line).expect("invalid JSON on stdin");
        let responses = node.handle(msg);
        for resp in responses {
            serde_json::to_writer(&mut out, &resp).expect("failed to write JSON");
            out.write_all(b"\n").expect("failed to write newline");
        }
        out.flush().expect("failed to flush stdout");
    }
}
