//! XEP-0203: recovery keeps the original offline delivery timestamp.
pub mod ingress_support;

use ingress_support::IngressFixture;
use std::{sync::Arc, time::Duration};
use waddle_server::{
    ingress::{
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            Effect,
        },
        Deps, ExternalEffect, IngressDecisionClass, IngressSubmission, PlannedEffect,
        RecoveryEnvironment,
    },
    ingress_uow::CanonicalMessageRepository,
    pending_delivery::DatabasePendingDeliveryStorage,
};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, PendingDeliveryMutation},
    pending_delivery::{
        flush::{build_replay_stanza, MaterializedPayload, ReplayReason},
        storage::PendingDeliveryStorage,
        PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
    registry::ConnectionRegistry,
    xep::NS_DELAY,
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
fn offline_submission(fixture: &IngressFixture, origin: &str) -> IngressSubmission {
    let mut submission = fixture.submission(Some(origin), "original delayed body");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: "juliet@example.com".parse().expect("recipient"),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(submission.plan.sanitized_message.clone())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    submission
        .plan
        .intents
        .push(IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
            },
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Suppressed,
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
    submission
}
async fn backdate_pending(fixture: &IngressFixture) {
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
        waddle_server::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
    };
    // The existing XEP-0203 serializer emits whole-second UTC stamps.
    fixture
        .execute(
            sql,
            waddle_server::db_params![(chrono::Utc::now() - chrono::Duration::seconds(120))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)],
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
    .expect("maintenance completes pending delivery");
}
async fn recovered_offline_flush_preserves_original_delay(fixture: IngressFixture) {
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(
        DatabasePendingDeliveryStorage::open(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage"),
    );
    let authority = fixture.authority().await;
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(OfflineRecoveryEnvironment {
        connections: ConnectionRegistry::new(),
        pending: pending.clone(),
    });
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let submission = offline_submission(&fixture, "delay-lost-execution");
    let decision = authority.commit(&submission).await;
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    let key = decision.message_key.expect("canonical key");
    let recipient = "juliet@example.com".parse().expect("recipient");
    assert!(pending
        .list(&recipient)
        .await
        .expect("pending before recovery")
        .is_empty());
    // Phase C is deliberately lost. Only maintenance may materialize the row.
    backdate_pending(&fixture).await;
    let mut tx = fixture.uow.begin().await.expect("timestamp transaction");
    let original_receipt_at = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("canonical timestamp");
    tx.commit().await.expect("timestamp read committed");
    let maintenance_started_at = chrono::Utc::now();
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 1).await;
    let rows = pending.list(&recipient).await.expect("recovered rows");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.original_receipt_at, original_receipt_at);
    assert!(row.original_receipt_at < maintenance_started_at - chrono::Duration::seconds(60));
    let replay = build_replay_stanza(
        MaterializedPayload::from_transient(row).expect("canonical inline payload"),
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let delays: Vec<_> = replay
        .payloads
        .iter()
        .filter(|p| p.is("delay", NS_DELAY))
        .collect();
    assert_eq!(delays.len(), 1, "exactly one XEP-0203 delay");
    assert_eq!(delays[0].attr("from"), Some("example.com"));
    let stamp = chrono::DateTime::parse_from_rfc3339(delays[0].attr("stamp").expect("delay stamp"))
        .expect("XEP-0082 timestamp")
        .with_timezone(&chrono::Utc);
    assert_eq!(
        stamp, original_receipt_at,
        "delay uses receipt time, not maintenance time"
    );
    let mut without_delay = replay.clone();
    without_delay.payloads.retain(|p| !p.is("delay", NS_DELAY));
    assert_eq!(
        minidom::Element::from(without_delay),
        minidom::Element::from(submission.plan.sanitized_message)
    );
    // A second lost row is an observable completion witness for another pass.
    let witness = offline_submission(&fixture, "delay-second-pass");
    assert_eq!(
        authority.commit(&witness).await.class,
        IngressDecisionClass::Accepted
    );
    backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 2).await;
    let after = pending
        .list(&recipient)
        .await
        .expect("rows after second pass");
    assert_eq!(after.len(), 2, "only the witness adds a pending row");
    let originals: Vec<_> = after
        .iter()
        .filter(|candidate| candidate.id == row.id)
        .collect();
    assert_eq!(originals.len(), 1, "original row is not duplicated");
    assert_eq!(originals[0].original_receipt_at, original_receipt_at);
    let replay_after = build_replay_stanza(
        MaterializedPayload::from_transient(originals[0]).expect("original payload retained"),
        "example.com",
        originals[0].original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    assert_eq!(
        minidom::Element::from(replay_after),
        minidom::Element::from(replay)
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(environment);
    drop(authority);
    drop(pending);
    fixture.close().await;
}
#[tokio::test]
async fn sqlite_recovered_offline_flush_preserves_original_delay() {
    recovered_offline_flush_preserves_original_delay(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_recovered_offline_flush_preserves_original_delay() {
    if let Some(fixture) = IngressFixture::postgres("delay_recovery").await {
        recovered_offline_flush_preserves_original_delay(fixture).await;
    }
}
