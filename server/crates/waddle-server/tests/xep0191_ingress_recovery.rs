//! XEP-0191 §2 (Requirements): blocking a sender after intake must also
//! prevent delivery when maintenance recovers a lost direct route. A failed
//! blocklist read defers recovery fail-closed until policy can be evaluated.

#[path = "ingress_cases/detached_progress_support.rs"]
pub mod detached_progress_support;
pub mod ingress_support;

use std::{sync::Arc, time::Duration};

use ingress_support::IngressFixture;
use jid::{BareJid, FullJid, Jid};
use waddle_server::{
    db::blocking::DatabaseBlockingStorage,
    ingress::{Deps, IngressAuthority, IngressDecision, IngressDecisionClass, RecoveryEnvironment},
    ingress_uow::EffectReceiptRepository,
};
use waddle_xmpp::{
    ingress::{DigestContext, DigestInput, NormalizedTarget},
    registry::ConnectionRegistry,
    stream_management::InMemorySmSessionRegistry,
    xep::xep0191::{BlockingStorage, BlockingStorageError},
};

struct BlockingRecoveryEnvironment {
    connections: ConnectionRegistry,
    sm: Arc<InMemorySmSessionRegistry>,
    blocking: Arc<dyn BlockingStorage>,
}

impl RecoveryEnvironment for BlockingRecoveryEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.connections, "example.com");
        deps.sm_session_registry = Some(&self.sm);
        deps.blocking_storage = Some(&self.blocking);
        deps
    }
}

fn bind(
    authority: &IngressAuthority,
    sm: &Arc<InMemorySmSessionRegistry>,
    blocking: Arc<dyn BlockingStorage>,
) -> Arc<dyn RecoveryEnvironment> {
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(BlockingRecoveryEnvironment {
        connections: ConnectionRegistry::new(),
        sm: Arc::clone(sm),
        blocking,
    });
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    environment
}

async fn commit_route(
    fixture: &IngressFixture,
    authority: &IngressAuthority,
    resource: &FullJid,
    origin: &str,
) -> IngressDecision {
    let mut submission = fixture.submission(Some(origin), "canonical recovered direct stanza");
    submission.target = NormalizedTarget::Bare(resource.to_bare());
    submission.plan.sanitized_message.to = Some(resource.to_bare().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("bare-target digest");
    detached_progress_support::route(&mut submission, std::slice::from_ref(resource), 1);
    let decision = authority.commit(&submission).await;
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    decision
}

async fn backdate_pending(fixture: &IngressFixture) {
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => {
            "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL"
        }
        waddle_server::db::DatabaseDriver::Sqlite => {
            "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL"
        }
    };
    fixture
        .execute(
            sql,
            waddle_server::db_params![
                (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339()
            ],
        )
        .await;
}

async fn wait_for_terminal_count(fixture: &IngressFixture, expected: i64) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await
                == expected
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance finishes the expected routes");
}

async fn assert_receipts(fixture: &IngressFixture, decision: &IngressDecision, completed: bool) {
    let mut tx = fixture.uow.begin().await.expect("receipt read");
    let receipts = EffectReceiptRepository::keys(
        &mut tx,
        decision.message_key.expect("canonical message key"),
    )
    .await
    .expect("durable route receipts");
    tx.commit().await.expect("receipt read complete");
    assert_eq!(
        decision.receipts_pending.len(),
        1,
        "one RouteDirect obligation"
    );
    if completed {
        assert_eq!(receipts, decision.receipts_pending);
    } else {
        assert!(receipts.is_empty());
    }
}

async fn lost_direct_route_to_now_blocking_recipient_is_discarded(fixture: IngressFixture) {
    let sm = detached_progress_support::registry(&fixture).await;
    let resource = detached_progress_support::resources()[0].clone();
    detached_progress_support::attach(&sm, &resource).await;
    let authority = fixture.authority().await;
    let blocking = Arc::new(DatabaseBlockingStorage::new(fixture.db.clone()));
    let environment = bind(&authority, &sm, blocking.clone());
    let decision = commit_route(&fixture, &authority, &resource, "blocked-after-intake").await;
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    assert_receipts(&fixture, &decision, false).await;

    assert_eq!(
        blocking
            .add_blocks(
                &resource.to_bare(),
                &[fixture.principal.bare_jid().clone().into()],
            )
            .await
            .expect("recipient blocks the sender after commit"),
        1,
    );
    backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 1).await;
    assert_receipts(&fixture, &decision, true).await;
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    assert!(detached_progress_support::queued(&sm, &resource)
        .await
        .unacked_stanzas
        .is_empty());

    // A second lost route witnesses a completed second pass, without relying
    // on a sleep to infer that maintenance actually ran again.
    let witness = commit_route(&fixture, &authority, &resource, "blocked-second-pass").await;
    backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 2).await;
    assert_receipts(&fixture, &decision, true).await;
    assert_receipts(&fixture, &witness, true).await;
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    let session = detached_progress_support::queued(&sm, &resource).await;
    assert_eq!(session.outbound_count, 0);
    assert!(session.unacked_stanzas.is_empty());
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(environment);
    drop(authority);
    drop(sm);
    drop(blocking);
    fixture.close().await;
}

#[derive(Debug, thiserror::Error)]
#[error("recipient blocklist storage unavailable")]
struct BlocklistUnavailable;

struct FailingBlockingStorage {
    recipient: BareJid,
    failed_read: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl BlockingStorage for FailingBlockingStorage {
    async fn list_blocked_jids(&self, _: &BareJid) -> Result<Vec<BareJid>, BlockingStorageError> {
        panic!("recovery must preserve full XEP-0191 JID entries");
    }

    async fn list_blocked_jid_entries(
        &self,
        user: &BareJid,
    ) -> Result<Vec<Jid>, BlockingStorageError> {
        if user == &self.recipient {
            self.failed_read.notify_one();
            Err(BlockingStorageError::new(BlocklistUnavailable))
        } else {
            Ok(Vec::new())
        }
    }
}

async fn blocklist_storage_failure_defers_recovery_fail_closed(fixture: IngressFixture) {
    let sm = detached_progress_support::registry(&fixture).await;
    let resource = detached_progress_support::resources()[0].clone();
    let witness_resource: FullJid = "romeo@example.com/recovery-witness"
        .parse()
        .expect("witness JID");
    detached_progress_support::attach(&sm, &resource).await;
    detached_progress_support::attach(&sm, &witness_resource).await;
    let authority = fixture.authority().await;
    let failing = Arc::new(FailingBlockingStorage {
        recipient: resource.to_bare(),
        failed_read: tokio::sync::Notify::new(),
    });
    let unavailable_environment = bind(&authority, &sm, failing.clone());
    let decision = commit_route(&fixture, &authority, &resource, "policy-unavailable").await;
    let witness = commit_route(
        &fixture,
        &authority,
        &witness_resource,
        "policy-pass-witness",
    )
    .await;
    backdate_pending(&fixture).await;
    // Recovery orders canonical rows by receipt time. Keep the failed route
    // ahead of the witness, even when both commits share a SQLite millisecond.
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres =>
            "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE message_key = ?::uuid",
        waddle_server::db::DatabaseDriver::Sqlite =>
            "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?",
    };
    fixture
        .execute(
            sql,
            waddle_server::db_params![
                (chrono::Utc::now() - chrono::Duration::seconds(130)).to_rfc3339(),
                decision
                    .message_key
                    .expect("failed route key")
                    .to_storage()
                    .to_string()
            ],
        )
        .await;
    authority.trigger_maintenance();
    tokio::time::timeout(Duration::from_secs(15), failing.failed_read.notified())
        .await
        .expect("maintenance attempts the unavailable blocklist");
    // A different recipient can settle in the same pass. This witnesses
    // maintenance progress while the original recipient remains fail-closed.
    wait_for_terminal_count(&fixture, 1).await;
    assert_receipts(&fixture, &witness, true).await;
    assert_receipts(&fixture, &decision, false).await;
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("sm_ingress_appends WHERE resource = 'juliet@example.com/a'")
            .await,
        0
    );
    assert!(detached_progress_support::queued(&sm, &resource)
        .await
        .unacked_stanzas
        .is_empty());

    let available_environment = bind(
        &authority,
        &sm,
        Arc::new(DatabaseBlockingStorage::new(fixture.db.clone())),
    );
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 2).await;
    assert_receipts(&fixture, &decision, true).await;
    assert_eq!(
        fixture
            .count("sm_ingress_appends WHERE resource = 'juliet@example.com/a'")
            .await,
        1
    );
    let session = detached_progress_support::queued(&sm, &resource).await;
    assert_eq!(session.outbound_count, 1);
    assert_eq!(session.unacked_stanzas.len(), 1);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(available_environment);
    drop(unavailable_environment);
    drop(authority);
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_lost_direct_route_to_now_blocking_recipient_is_discarded() {
    lost_direct_route_to_now_blocking_recipient_is_discarded(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_lost_direct_route_to_now_blocking_recipient_is_discarded() {
    if let Some(fixture) = IngressFixture::postgres("xep0191_blocked_recovery").await {
        lost_direct_route_to_now_blocking_recipient_is_discarded(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_blocklist_storage_failure_defers_recovery_fail_closed() {
    blocklist_storage_failure_defers_recovery_fail_closed(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_blocklist_storage_failure_defers_recovery_fail_closed() {
    if let Some(fixture) = IngressFixture::postgres("xep0191_blocklist_unavailable").await {
        blocklist_storage_failure_defers_recovery_fail_closed(fixture).await;
    }
}
