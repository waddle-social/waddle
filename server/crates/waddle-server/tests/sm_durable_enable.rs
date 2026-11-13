//! Authoritative XEP-0198 enablement over single-node SQLite.
//!
//! The session row commits before a reply can be built, the reply carries a
//! resume token that is not the storage key, and an operation whose outcome
//! was resolved as abandoned can never commit. A commit whose outcome is
//! genuinely unknown needs a PostgreSQL fault fixture and is not covered.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use jid::FullJid;
use uuid::Uuid;
use waddle_server::db::Database;
use waddle_server::sm_authority::{
    DurableSmStore, EnableError, EnableOperation, EnableRejection, EnableRequest, EnableResolution,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::stream_management::durable::{
    DurableSmState, ExposureBounds, OutboundWindow, SmResumeToken,
};

fn bounds() -> ExposureBounds {
    ExposureBounds::new(4, 4096).expect("test bounds")
}

fn principal(context: u128, epoch: u64, jid: &str) -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        jid.parse().expect("bare jid"),
        AuthContextId::new(Uuid::from_u128(context)),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::new(epoch),
    )
}

fn binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + seconds, 0)
        .single()
        .expect("valid time")
}

fn alice_phone() -> FullJid {
    "alice@example.test/phone".parse().expect("full jid")
}

async fn store(name: &str) -> DurableSmStore {
    let db = Database::in_memory(name).await.expect("in-memory database");
    DurableSmStore::open(db, bounds(), false)
        .await
        .expect("open durable SM store")
}

#[tokio::test]
async fn enable_commits_a_live_generation_one_session_before_the_reply_exists() {
    let store = store("sm-durable-enable").await;
    let owner = binding();
    let operation = EnableOperation::mint();
    let session = operation.session();
    let alice = principal(1, 7, "alice@example.test");
    let request = EnableRequest {
        binding: &owner,
        principal: &alice,
        full_jid: &alice_phone(),
        max_resume: Duration::from_secs(300),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        claim: None,
        now: at(0),
    };

    // Nothing is stored until the enable runs, and the reply type is only
    // reachable from its witness.
    assert_eq!(store.session(session).await.expect("read"), None);
    let commit = store
        .enable(&operation, request)
        .await
        .expect("enable commits");

    let stored = store
        .session(session)
        .await
        .expect("read")
        .expect("session row committed");
    assert_eq!(stored.state, DurableSmState::Live);
    assert_eq!(stored.generation.value(), 1);
    assert_eq!(stored.binding, owner);
    assert_eq!(stored.principal, alice);
    assert_eq!(stored.window, OutboundWindow::EMPTY);
    assert_eq!(&stored.fence(), commit.fence());

    let reply = commit.enabled_reply();
    assert!(reply.resume);
    assert_eq!(reply.max, Some(300));
    // The reply id is the resume token, not the storage key.
    assert_ne!(reply.id, session.as_uuid().to_string());
    assert!(SmResumeToken::from_wire(&reply.id).is_ok());

    // Repeating the same operation returns the same session and token.
    let repeated = store
        .enable(&operation, request)
        .await
        .expect("idempotent enable");
    assert_eq!(repeated.fence(), commit.fence());
    assert_eq!(repeated.enabled_reply().id, reply.id);
}

#[tokio::test]
async fn enable_rejects_a_principal_that_is_not_the_bound_resource() {
    let store = store("sm-durable-enable-mismatch").await;
    let operation = EnableOperation::mint();
    let session = operation.session();
    let mallory = principal(9, 1, "mallory@example.test");
    let rejected = store
        .enable(
            &operation,
            EnableRequest {
                binding: &binding(),
                principal: &mallory,
                full_jid: &alice_phone(),
                max_resume: Duration::from_secs(300),
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                claim: None,
                now: at(0),
            },
        )
        .await;
    assert!(matches!(
        rejected,
        Err(EnableError::Rejected(EnableRejection::ResourceMismatch))
    ));
    assert_eq!(store.session(session).await.expect("read"), None);
}

#[tokio::test]
async fn an_abandoned_enable_operation_can_never_commit() {
    let store = store("sm-durable-enable-abandon").await;
    let owner = binding();
    let operation = EnableOperation::mint();
    let alice = principal(1, 7, "alice@example.test");
    let request = EnableRequest {
        binding: &owner,
        principal: &alice,
        full_jid: &alice_phone(),
        max_resume: Duration::from_secs(300),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        claim: None,
        now: at(0),
    };

    // Resolving an operation that never committed bars it.
    let resolution = store
        .resolve_enable(&operation, &owner, at(1))
        .await
        .expect("resolution is known");
    assert!(matches!(resolution, EnableResolution::NotCommitted));
    let rejected = store.enable(&operation, request).await;
    assert!(matches!(
        rejected,
        Err(EnableError::Rejected(EnableRejection::Abandoned))
    ));
    assert_eq!(
        store.session(operation.session()).await.expect("read"),
        None
    );

    // Resolving an operation that did commit reports the commit and leaves
    // the session in place.
    let committed = EnableOperation::mint();
    let witness = store
        .enable(&committed, request)
        .await
        .expect("enable commits");
    match store
        .resolve_enable(&committed, &owner, at(2))
        .await
        .expect("resolution is known")
    {
        EnableResolution::Committed(resolved) => {
            assert_eq!(resolved.fence(), witness.fence());
            assert_eq!(resolved.enabled_reply().id, witness.enabled_reply().id);
        }
        other => panic!("expected the committed enable, got {other:?}"),
    }
}
