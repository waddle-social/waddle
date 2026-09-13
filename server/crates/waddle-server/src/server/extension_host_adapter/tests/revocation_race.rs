//! A grant resolved before planning must still be valid at durable admission.
use super::super::ExtensionHostAdapterError;
use super::direct_ingress::{adapter, invocation, plugin, request};
use crate::{ingress::test_support::IngressFixture, ingress_uow::ExtensionGrantRepository};
use jid::{BareJid, Jid};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

struct SenderReadGate {
    sender: BareJid,
    armed: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}

#[async_trait::async_trait]
impl BlockingStorage for SenderReadGate {
    async fn list_blocked_jids(
        &self,
        user: &BareJid,
    ) -> Result<Vec<BareJid>, BlockingStorageError> {
        Ok(self
            .list_blocked_jid_entries(user)
            .await?
            .into_iter()
            .map(|entry| entry.to_bare())
            .collect())
    }

    async fn list_blocked_jid_entries(
        &self,
        user: &BareJid,
    ) -> Result<Vec<Jid>, BlockingStorageError> {
        if user == &self.sender && self.armed.swap(false, Ordering::SeqCst) {
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("release gate open")
                .forget();
        }
        Ok(Vec::new())
    }
}

async fn revoked_during_planning(f: IngressFixture) {
    let mut adapter = adapter(&f).await;
    let gate = Arc::new(SenderReadGate {
        sender: invocation().actor_jid.to_bare(),
        armed: AtomicBool::new(true),
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    Arc::get_mut(&mut adapter.state)
        .expect("unique adapter state")
        .deps
        .protocol
        .blocking_storage = gate.clone();
    let adapter = Arc::new(adapter);
    let sending_adapter = Arc::clone(&adapter);
    let sending = tokio::spawn(async move {
        sending_adapter
            .send_message(&invocation(), request("revoked-during-planning"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.acquire())
        .await
        .expect("adapter reaches sender blocklist after resolving its grant")
        .expect("entry gate open")
        .forget();
    assert_eq!(f.count("ingress_messages").await, 0);
    let mut tx = f.uow.begin().await.expect("revocation transaction");
    assert_eq!(
        ExtensionGrantRepository::revoke_plugin(&mut tx, &plugin())
            .await
            .expect("revoke resolved grant"),
        1,
    );
    tx.commit()
        .await
        .expect("durable revocation before admission");
    gate.release.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(5), sending)
        .await
        .expect("revoked dispatch finishes")
        .expect("dispatch task completes");
    assert!(
        matches!(result, Err(ExtensionHostAdapterError::NotAuthorized)),
        "{result:?}"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    assert_eq!(
        f.count("mam_messages").await,
        0,
        "planning cannot archive before admission"
    );
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(5))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_revoked_during_planning_sqlite() {
    revoked_during_planning(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_direct_revoked_during_planning_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_revoke_race").await {
        revoked_during_planning(f).await;
    }
}

async fn committed_before_concurrent_revocation(f: IngressFixture) {
    use crate::ingress::commit::commit_race_gate::Registration;
    use std::{future::Future, task::Poll};
    use waddle_xmpp_core::xep0359::OriginId;

    let adapter = Arc::new(adapter(&f).await);
    let origin = OriginId::new(uuid::Uuid::new_v4().to_string());
    let gate = Registration::new(origin.clone());
    let sending_adapter = Arc::clone(&adapter);
    let sending = tokio::spawn(async move {
        sending_adapter
            .send_message(&invocation(), request(origin.as_str()))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered())
        .await
        .expect("Phase B has asserted the grant inside its open transaction");
    assert_eq!(f.count("ingress_messages").await, 0);

    let (sent, revoked) = {
        let revoking = async {
            let mut tx = f.uow.begin().await.expect("concurrent revocation tx");
            let revoked = ExtensionGrantRepository::revoke_plugin(&mut tx, &plugin())
                .await
                .expect("concurrent revoke");
            tx.commit().await.expect("concurrent revocation commit");
            revoked
        };
        tokio::pin!(revoking);
        // Poll the revocation operation while the admitted transaction is open.
        // SQLite serializes at BEGIN IMMEDIATE; PostgreSQL at the grant row lock.
        // The gate proves overlap without depending on a sleep or task scheduling.
        std::future::poll_fn(|cx| match revoking.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("revocation completed while admission owns its grant"),
        })
        .await;
        gate.release();
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(sending, revoking)
        })
        .await
        .expect("admitted dispatch and revocation finish")
    };
    sent.expect("dispatch task")
        .expect("admission committed before revocation");
    assert_eq!(revoked, 1);
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(f.count("pending_delivery").await, 1);
    assert_eq!(f.count("notification_candidates").await, 1);
    assert_eq!(f.count("mam_messages").await, 2);
    assert_complete_offline_obligations(&f).await;
    assert!(matches!(
        adapter
            .send_message(&invocation(), request("after-concurrent-revoke"))
            .await,
        Err(ExtensionHostAdapterError::NotAuthorized)
    ));
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("pending_delivery").await, 1);
    assert_eq!(f.count("notification_candidates").await, 1);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(5))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_commit_overlaps_revocation_sqlite() {
    committed_before_concurrent_revocation(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_direct_commit_overlaps_revocation_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_commit_revoke").await {
        committed_before_concurrent_revocation(f).await;
    }
}

async fn assert_complete_offline_obligations(f: &IngressFixture) {
    use crate::ingress_uow::{EffectIntentRepository, EffectReceiptRepository};
    use waddle_xmpp::ingress::{
        IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    };
    let key = MessageKey::from_storage(
        f.optional_text("SELECT CAST(message_key AS TEXT) FROM ingress_messages")
            .await
            .expect("canonical key")
            .parse()
            .expect("message uuid"),
    );
    let mut tx = f.uow.begin().await.expect("inspect committed obligations");
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    assert_eq!(
        intents
            .iter()
            .filter(|intent| matches!(intent, IngressEffectIntent::PendingDelivery { .. }))
            .count(),
        1
    );
    assert_eq!(
        intents
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::NotificationActivityPreview {
                    mutation: NotificationActivityMutation::NotificationCandidate {
                        outcome: NotificationCandidateOutcome::Inserted,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        intents
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::NotificationActivityPreview {
                    mutation: NotificationActivityMutation::OfflineDelivery { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("all receipts settled"));
    tx.commit().await.expect("inspection commit");
}
