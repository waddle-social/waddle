//! XEP-0357: recovery preserves the durable offline notification obligation.
pub mod ingress_support;

use ingress_support::IngressFixture;
use std::{sync::Arc, time::Duration};
use waddle_server::{
    ingress::{
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            Effect, ExternalEffect,
        },
        Deps,
    },
    ingress::{IngressDecisionClass, IngressSubmission, PlannedEffect, RecoveryEnvironment},
    notification_outbox::{NotificationCandidate, NotificationOutboxStore},
    pending_delivery::DatabasePendingDeliveryStorage,
};
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
        PendingDeliveryMutation,
    },
    pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
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
    let mut submission = fixture.submission(Some(origin), "canonical offline body");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new(origin, recipient.clone().into());
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let candidate = NotificationCandidate::direct_message(
        recipient.clone(),
        submission.sender.clone().into(),
        stamp.clone(),
        false,
    )
    .expect("direct candidate");
    submission.plan.intents.extend([
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: recipient,
                archive_stanza_id: stamp,
            },
        },
    ]);
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Prepared(Box::new(candidate)),
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
    submission
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

async fn lost_offline_notification_candidate_recovers_once(fixture: IngressFixture) {
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
    let submission = offline_plan(&fixture, "push-recovery-original");
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
    let canonical_receipt_at = backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 1).await;
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    let identity_filter = "notification_candidates WHERE recipient_bare_jid = 'juliet@example.com' AND sender_jid = 'romeo@example.com/phone' AND conversation_jid = 'romeo@example.com' AND stanza_id_by = 'juliet@example.com' AND stanza_id = 'push-recovery-original' AND class = 'dm' AND reason = 'offline_dm'";
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
    lost_offline_notification_candidate_recovers_once(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_lost_offline_notification_candidate_recovers_once() {
    if let Some(fixture) = IngressFixture::postgres("push_recovery").await {
        lost_offline_notification_candidate_recovers_once(fixture).await;
    }
}
