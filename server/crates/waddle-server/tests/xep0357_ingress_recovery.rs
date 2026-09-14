//! XEP-0357: recovery preserves the durable offline notification obligation.
#[path = "ingress_support/extension_recovery.rs"]
mod extension_recovery_support;
pub mod ingress_support;

use ingress_support::IngressFixture;
use std::{sync::Arc, time::Duration};
use waddle_server::{
    ingress::Deps,
    ingress::{IngressDecisionClass, IngressSubmission, RecoveryEnvironment},
    notification_outbox::NotificationOutboxStore,
    pending_delivery::DatabasePendingDeliveryStorage,
};
use waddle_xmpp::{
    pending_delivery::{storage::PendingDeliveryStorage, QuotaPolicy},
    registry::ConnectionRegistry,
};

struct OfflineRecoveryEnvironment {
    connections: ConnectionRegistry,
    pending: Arc<dyn PendingDeliveryStorage>,
}

impl RecoveryEnvironment for OfflineRecoveryEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.connections, "example.com");
        deps.pending_delivery_storage = Some(&self.pending);
        deps
    }
}

fn offline_plan(fixture: &IngressFixture, origin: &str) -> IngressSubmission {
    extension_recovery_support::offline_submission(
        fixture.submission(Some(origin), "canonical offline body"),
        origin,
    )
}

async fn backdate_pending(fixture: &IngressFixture) -> chrono::DateTime<chrono::Utc> {
    let received_at = chrono::DateTime::from_timestamp_millis(1_700_000_000_123)
        .expect("canonical receipt timestamp");
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres =>
            "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
        waddle_server::db::DatabaseDriver::Sqlite =>
            "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
    };
    fixture
        .execute(sql, waddle_server::db_params![received_at.to_rfc3339()])
        .await;
    received_at
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
    .expect("maintenance recovers the recorded notification obligations");
}

async fn lost_offline_notification_candidate_recovers_once(
    fixture: IngressFixture,
    extension: bool,
) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("notification schema");
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(OfflineRecoveryEnvironment {
        connections: ConnectionRegistry::new(),
        pending: Arc::new(
            DatabasePendingDeliveryStorage::open(
                Some(fixture.db.database_url()),
                QuotaPolicy::Unlimited,
            )
            .await
            .expect("pending storage"),
        ),
    });
    let authority = fixture.authority().await;
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let submission = if extension {
        extension_recovery_support::offline_submission(
            extension_recovery_support::extension_submission(
                &fixture,
                "push-recovery-original",
                "canonical offline body",
            )
            .await,
            "push-recovery-original",
        )
    } else {
        offline_plan(&fixture, "push-recovery-original")
    };
    let decision = authority.commit(&submission).await;
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(fixture.count("ingress_effect_intents").await, 3);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        0
    );
    // Deliberately discard Phase C: only the public maintenance executor can enqueue this notification.
    drop(decision);
    if extension {
        extension_recovery_support::revoke_after_commit(&fixture, &submission).await;
        assert_eq!(
            authority.commit(&submission).await.class,
            IngressDecisionClass::PrincipalMissing
        );
    }
    // No push policy store exists in the recovery environment: only the frozen
    // Inserted obligation can authorize materializing this candidate.
    let canonical_receipt_at = backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 1).await;
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    let socket_identity_filter = "notification_candidates WHERE recipient_bare_jid = 'juliet@example.com' AND sender_jid = 'romeo@example.com/phone' AND conversation_jid = 'romeo@example.com' AND stanza_id_by = 'juliet@example.com' AND stanza_id = 'push-recovery-original' AND class = 'dm' AND reason = 'offline_dm'";
    let extension_identity_filter = "notification_candidates WHERE recipient_bare_jid = 'juliet@example.com' AND sender_jid = 'romeo@example.com/extension-host' AND conversation_jid = 'romeo@example.com' AND stanza_id_by = 'juliet@example.com' AND stanza_id = 'push-recovery-original' AND class = 'dm' AND reason = 'offline_dm'";
    let identity_filter = if extension {
        extension_identity_filter
    } else {
        socket_identity_filter
    };
    assert_eq!(
        fixture.count(identity_filter).await,
        1,
        "the recorded recipient and stanza identity reach the XEP-0357 outbox"
    );
    let created_at = fixture.optional_text("SELECT CAST(created_at_ms AS TEXT) FROM notification_candidates WHERE stanza_id = 'push-recovery-original'").await;

    assert_eq!(
        created_at,
        Some(canonical_receipt_at.timestamp_millis().to_string()),
        "recovered candidates retain canonical receipt ordering"
    );

    // A separate lost obligation proves the next pass completed, rather than relying on a sleep.
    let sentinel = offline_plan(&fixture, "push-recovery-second-pass");
    assert_eq!(
        authority.commit(&sentinel).await.class,
        IngressDecisionClass::Accepted
    );
    backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 2).await;
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        2
    );
    assert_eq!(
        fixture.count(identity_filter).await,
        1,
        "a second pass creates no duplicate original candidate"
    );
    assert_eq!(fixture.optional_text("SELECT CAST(created_at_ms AS TEXT) FROM notification_candidates WHERE stanza_id = 'push-recovery-original'").await, created_at, "a second pass does not replace the original candidate");
    assert_eq!(fixture.count("pending_delivery").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 6);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(authority);
    drop(environment);
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_lost_offline_notification_candidate_recovers_once() {
    lost_offline_notification_candidate_recovers_once(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_lost_offline_notification_candidate_recovers_once() {
    if let Some(fixture) = IngressFixture::postgres("push_recovery").await {
        lost_offline_notification_candidate_recovers_once(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_extension_frozen_notification_recovers_after_revocation() {
    lost_offline_notification_candidate_recovers_once(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_extension_frozen_notification_recovers_after_revocation() {
    if let Some(fixture) = IngressFixture::postgres("extension_push_recovery").await {
        lost_offline_notification_candidate_recovers_once(fixture, true).await;
    }
}
