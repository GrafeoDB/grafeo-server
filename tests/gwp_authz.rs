#![cfg(all(feature = "gwp", feature = "auth"))]
//! Authorization of GWP's AdminService and SearchService: the calls act as
//! their session's token, reach only the databases in its scope, and the
//! admin calls need the admin role, as the HTTP admin routes do. Every RPC of
//! the two services is listed from gwp's service descriptors, so a new one
//! fails `every_admin_and_search_rpc_is_covered` until it is added here.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use grafeo_service::auth::{Role, TokenRecord, TokenScope};
use gwp::proto;
use prost::Message;
use tokio::net::TcpListener;
use tonic::Code;
use tonic::transport::Channel;

const ROOT: &str = "root-token";
const ADMIN_A: &str = "admin-a-token";
const RW: &str = "rw-token";
const RO: &str = "ro-token";

/// Every AdminService and SearchService RPC, and whether it needs admin.
const RPCS: &[(&str, bool)] = &[
    ("AdminService/GetGraphStats", true),
    ("AdminService/WalStatus", true),
    ("AdminService/WalCheckpoint", true),
    ("AdminService/Validate", true),
    ("AdminService/CreateIndex", true),
    ("AdminService/DropIndex", true),
    ("SearchService/VectorSearch", false),
    ("SearchService/TextSearch", false),
    ("SearchService/HybridSearch", false),
];

#[test]
fn every_admin_and_search_rpc_is_covered() {
    let set = prost_types::FileDescriptorSet::decode(proto::FILE_DESCRIPTOR_SET).unwrap();
    let listed: BTreeSet<String> = set
        .file
        .iter()
        .flat_map(|f| f.service.iter())
        .filter(|s| matches!(s.name(), "AdminService" | "SearchService"))
        .flat_map(|s| {
            s.method
                .iter()
                .map(move |m| format!("{}/{}", s.name(), m.name()))
        })
        .collect();
    let covered: BTreeSet<String> = RPCS.iter().map(|(r, _)| (*r).to_string()).collect();
    assert_eq!(listed, covered);
}

fn token(plaintext: &str, name: &str, role: Role, databases: &[&str]) -> TokenRecord {
    TokenRecord {
        id: format!("tok-{name}"),
        name: name.to_string(),
        token_hash: grafeo_service::token_service::hash_token(plaintext),
        scope: TokenScope {
            role,
            databases: databases.iter().map(|d| (*d).to_string()).collect(),
        },
        created_at: "2026-01-01T00:00:00Z".to_string(),
        expires_at: None,
    }
}

/// A GWP server with auth, databases `a` and `b`, and the tokens above.
async fn spawn() -> Channel {
    let dir = tempfile::tempdir().unwrap();
    let store =
        grafeo_service::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
    for record in [
        token(ADMIN_A, "admin-a", Role::Admin, &["a"]),
        token(RW, "rw", Role::ReadWrite, &[]),
        token(RO, "ro", Role::ReadOnly, &[]),
    ] {
        store.insert(record).unwrap();
    }
    let provider = grafeo_service::auth::AuthProvider::with_token_store(
        Some(ROOT.to_string()),
        None,
        None,
        std::sync::Arc::new(store),
    )
    .unwrap();
    let service = grafeo_service::ServiceState::new_in_memory_with_auth_provider(300, provider);
    for name in ["a", "b"] {
        let req = serde_json::from_value(serde_json::json!({ "name": name })).unwrap();
        service.databases().create(&req).unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);
    let auth_provider = service.auth().cloned();
    let backend = grafeo_gwp::GrafeoBackend::new(service);
    let options = grafeo_gwp::GwpOptions {
        auth_provider,
        ..Default::default()
    };
    tokio::spawn(async move {
        grafeo_gwp::serve(backend, addr, options).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

async fn session(channel: &Channel, bearer: &str) -> String {
    proto::session_service_client::SessionServiceClient::new(channel.clone())
        .handshake(proto::HandshakeRequest {
            protocol_version: 1,
            credentials: Some(proto::AuthCredentials {
                method: Some(proto::auth_credentials::Method::BearerToken(
                    bearer.to_string(),
                )),
            }),
            client_info: std::collections::HashMap::new(),
        })
        .await
        .unwrap()
        .into_inner()
        .session_id
}

/// Calls `rpc` on database `db` in session `sid`; `Ok` for any answer that
/// is not an error status.
async fn call(channel: &Channel, rpc: &str, sid: &str, db: &str) -> Result<(), tonic::Status> {
    let mut admin = proto::admin_service_client::AdminServiceClient::new(channel.clone());
    let mut search = proto::search_service_client::SearchServiceClient::new(channel.clone());
    let graph = db.to_string();
    let session_id = sid.to_string();
    let index = || {
        Some(proto::create_index_request::Index::PropertyIndex(
            proto::PropertyIndexDef {
                property: "p".to_string(),
            },
        ))
    };
    match rpc {
        "AdminService/GetGraphStats" => admin
            .get_graph_stats(proto::GetGraphStatsRequest { graph, session_id })
            .await
            .map(drop),
        "AdminService/WalStatus" => admin
            .wal_status(proto::WalStatusRequest { graph, session_id })
            .await
            .map(drop),
        "AdminService/WalCheckpoint" => admin
            .wal_checkpoint(proto::WalCheckpointRequest { graph, session_id })
            .await
            .map(drop),
        "AdminService/Validate" => admin
            .validate(proto::ValidateRequest { graph, session_id })
            .await
            .map(drop),
        "AdminService/CreateIndex" => admin
            .create_index(proto::CreateIndexRequest {
                graph,
                index: index(),
                session_id,
            })
            .await
            .map(drop),
        "AdminService/DropIndex" => admin
            .drop_index(proto::DropIndexRequest {
                graph,
                index: Some(proto::drop_index_request::Index::PropertyIndex(
                    proto::PropertyIndexDef {
                        property: "p".to_string(),
                    },
                )),
                session_id,
            })
            .await
            .map(drop),
        "SearchService/VectorSearch" => search
            .vector_search(proto::VectorSearchRequest {
                graph,
                label: "L".to_string(),
                property: "e".to_string(),
                query_vector: vec![1.0],
                k: 1,
                session_id,
                ..Default::default()
            })
            .await
            .map(drop),
        "SearchService/TextSearch" => search
            .text_search(proto::TextSearchRequest {
                graph,
                label: "L".to_string(),
                property: "t".to_string(),
                query: "x".to_string(),
                k: 1,
                session_id,
            })
            .await
            .map(drop),
        "SearchService/HybridSearch" => search
            .hybrid_search(proto::HybridSearchRequest {
                graph,
                label: "L".to_string(),
                text_property: "t".to_string(),
                vector_property: "e".to_string(),
                query_text: "x".to_string(),
                k: 1,
                session_id,
                ..Default::default()
            })
            .await
            .map(drop),
        other => panic!("no call for {other}"),
    }
}

fn denied(result: &Result<(), tonic::Status>) -> bool {
    matches!(result, Err(s) if s.code() == Code::PermissionDenied)
}

#[tokio::test]
async fn a_scoped_session_reaches_no_other_database() {
    let channel = spawn().await;
    let sid = session(&channel, ADMIN_A).await;
    for (rpc, _) in RPCS {
        let other = call(&channel, rpc, &sid, "b").await;
        assert!(denied(&other), "{rpc} on b: {other:?}");
        let own = call(&channel, rpc, &sid, "a").await;
        assert!(!denied(&own), "{rpc} on a: {own:?}");
    }
}

#[tokio::test]
async fn admin_calls_need_the_admin_role() {
    let channel = spawn().await;
    for bearer in [RW, RO] {
        let sid = session(&channel, bearer).await;
        for (rpc, needs_admin) in RPCS {
            let result = call(&channel, rpc, &sid, "a").await;
            assert_eq!(denied(&result), *needs_admin, "{bearer}: {rpc}: {result:?}");
        }
    }
}

#[tokio::test]
async fn an_unscoped_admin_reaches_every_database() {
    let channel = spawn().await;
    let sid = session(&channel, ROOT).await;
    for (rpc, _) in RPCS {
        for db in ["a", "b"] {
            let result = call(&channel, rpc, &sid, db).await;
            assert!(!denied(&result), "{rpc} on {db}: {result:?}");
        }
    }
}

#[tokio::test]
async fn a_call_without_a_session_is_refused() {
    let channel = spawn().await;
    for (rpc, _) in RPCS {
        let result = call(&channel, rpc, "", "a").await;
        assert!(
            matches!(&result, Err(s) if s.code() == Code::Unauthenticated),
            "{rpc}: {result:?}"
        );
    }
}

#[tokio::test]
async fn schema_create_and_drop_need_the_write_role() {
    let channel = spawn().await;
    let mut catalog = proto::catalog_service_client::CatalogServiceClient::new(channel.clone());

    // A read-only token must not create or drop schemas.
    let ro = session(&channel, RO).await;
    let err = catalog
        .create_schema(proto::CreateSchemaRequest {
            name: "s1".into(),
            if_not_exists: false,
            session_id: ro.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err}");
    let err = catalog
        .drop_schema(proto::DropSchemaRequest {
            name: "s1".into(),
            if_exists: true,
            session_id: ro.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err}");

    // A read-write token may create and drop them.
    let rw = session(&channel, RW).await;
    catalog
        .create_schema(proto::CreateSchemaRequest {
            name: "s1".into(),
            if_not_exists: true,
            session_id: rw.clone(),
        })
        .await
        .unwrap();
    catalog
        .drop_schema(proto::DropSchemaRequest {
            name: "s1".into(),
            if_exists: true,
            session_id: rw.clone(),
        })
        .await
        .unwrap();

    // A token scoped to another database may not: the session works in
    // `default`, outside ADMIN_A's scope.
    let admin_a = session(&channel, ADMIN_A).await;
    let err = catalog
        .create_schema(proto::CreateSchemaRequest {
            name: "s2".into(),
            if_not_exists: false,
            session_id: admin_a,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err}");
}
