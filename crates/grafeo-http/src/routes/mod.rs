//! HTTP API route handlers.

pub mod admin;
pub mod backup;
pub mod batch;
pub mod database;
pub mod graph_store;
pub mod query;
#[cfg(feature = "replication")]
pub mod replication;
pub mod search;
pub mod sparql_protocol;
#[cfg(feature = "sync")]
pub mod sync;
pub mod system;
pub mod transaction;
pub mod websocket;
