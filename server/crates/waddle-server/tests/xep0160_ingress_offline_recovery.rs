//! XEP-0160 §3: lost offline storage recovers; a refused queue stays refused.
pub mod ingress_support;
use ingress_support::IngressFixture;
use std::{sync::Arc, time::Duration};
use waddle_server::{
    ingress::{
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            Effect, PlanSuppressionPolicy,
        },
        Deps, ExternalEffect, IngressAuthority, IngressDecisionClass, IngressSubmission,
        PlannedEffect, RecoveryEnvironment,
    },
    pending_delivery::DatabasePendingDeliveryStorage,
};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, PendingDeliveryMutation},
    pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
    registry::ConnectionRegistry,
};
struct OfflineEnvironment {
    connections: Arc<ConnectionRegistry>,
    storage: Arc<dyn PendingDeliveryStorage>,
}
impl RecoveryEnvironment for OfflineEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.connections, "example.com");
        deps.pending_delivery_storage = Some(&self.storage);
        deps
    }
}
async fn environment(
    f: &IngressFixture,
    connections: Arc<ConnectionRegistry>,
    quota: QuotaPolicy,
) -> Arc<OfflineEnvironment> {
    Arc::new(OfflineEnvironment {
        connections,
        storage: Arc::new(
            DatabasePendingDeliveryStorage::open(Some(f.db.database_url()), quota)
                .await
                .expect("pending storage"),
        ),
    })
}
fn bind(authority: &IngressAuthority, environment: &Arc<OfflineEnvironment>) {
    let erased: Arc<dyn RecoveryEnvironment> = environment.clone();
    authority.bind_recovery_environment(Arc::downgrade(&erased));
}
fn offline_plan(f: &IngressFixture, origin: &str) -> (IngressSubmission, PendingRowId) {
    let mut submission = f.submission(Some(origin), "canonical offline payload");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: "juliet@example.com".parse().expect("recipient"),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(submission.plan.sanitized_message.clone())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let id = row.id.clone();
    submission
        .plan
        .intents
        .push(IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
            },
        });
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Suppressed,
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            },
        )))
        .with_suppression(PlanSuppressionPolicy::SenderOnly),
    );
    (submission, id)
}
async fn recover(f: &IngressFixture, authority: &IngressAuthority, terminal_count: i64) {
    let sql = match f.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
        waddle_server::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
    };
    f.execute(
        sql,
        waddle_server::db_params![
            (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339()
        ],
    )
    .await;
    authority.trigger_maintenance();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if f.count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await
                == terminal_count
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance settles the lost offline obligation");
}
async fn lost_offline_row_is_materialized_once(f: IngressFixture) {
    let env = environment(
        &f,
        Arc::new(ConnectionRegistry::new()),
        QuotaPolicy::Unlimited,
    )
    .await;
    let authority = f.authority().await;
    bind(&authority, &env);
    let (submission, id) = offline_plan(&f, "offline-recovery");
    assert_eq!(
        authority.commit(&submission).await.class,
        IngressDecisionClass::Accepted
    );
    // Phase C is deliberately lost; maintenance alone must materialize the row.
    assert_eq!(f.count("pending_delivery").await, 0);
    recover(&f, &authority, 1).await;
    let recipient = "juliet@example.com".parse().expect("recipient");
    let rows = env.storage.list(&recipient).await.expect("recovered rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    let PendingPayload::Transient(message) = &rows[0].payload else {
        panic!("canonical transient payload");
    };
    let expected: minidom::Element = submission.plan.sanitized_message.clone().into();
    assert_eq!(minidom::Element::from(message.as_ref().clone()), expected);
    assert_eq!(f.count("ingress_effect_receipts").await, 1);
    // Another lost obligation witnesses a completed second maintenance pass.
    let (witness, witness_id) = offline_plan(&f, "offline-second-pass");
    assert_eq!(
        authority.commit(&witness).await.class,
        IngressDecisionClass::Accepted
    );
    recover(&f, &authority, 2).await;
    let after = env
        .storage
        .list(&recipient)
        .await
        .expect("second pass rows");
    assert_eq!(after.len(), 2);
    assert_eq!(after.iter().filter(|row| row.id == id).count(), 1);
    assert_eq!(after.iter().filter(|row| row.id == witness_id).count(), 1);
    let original = after.iter().find(|row| row.id == id).expect("original row");
    assert_eq!(original.original_receipt_at, rows[0].original_receipt_at);
    let PendingPayload::Transient(message) = &original.payload else {
        panic!("transient payload retained");
    };
    assert_eq!(minidom::Element::from(message.as_ref().clone()), expected);
    assert_eq!(f.count("ingress_effect_receipts").await, 2);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(authority);
    drop(env);
    f.close().await;
}
#[tokio::test]
async fn sqlite_lost_offline_row_is_materialized_once() {
    lost_offline_row_is_materialized_once(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_lost_offline_row_is_materialized_once() {
    if let Some(f) = IngressFixture::postgres("xep0160_lost_offline").await {
        lost_offline_row_is_materialized_once(f).await;
    }
}

async fn quota_refusal_bounces_once_and_is_not_requeued(f: IngressFixture) {
    let connections = Arc::new(ConnectionRegistry::new());
    let env = environment(
        &f,
        connections.clone(),
        QuotaPolicy::CountCap { max_rows: 0 },
    )
    .await;
    let authority = f.authority().await;
    bind(&authority, &env);
    let (submission, refused_id) = offline_plan(&f, "offline-quota-refused");
    let (sender, mut frames) = tokio::sync::mpsc::channel(8);
    connections.register_with_carbons(submission.sender.clone(), sender, false);
    assert_eq!(
        authority.commit(&submission).await.class,
        IngressDecisionClass::Accepted
    );
    assert_eq!(f.count("pending_delivery").await, 0);
    assert!(frames.try_recv().is_err(), "Phase B never bounces");
    // Lose Phase C. Maintenance reaches the real quota refusal and receipts it.
    recover(&f, &authority, 1).await;
    let frame = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .expect("quota bounce arrives")
        .expect("sender frame");
    let waddle_xmpp::Stanza::Message(bounce) = frame.stanza else {
        panic!("message error");
    };
    assert_eq!(bounce.type_, xmpp_parsers::message::MessageType::Error);
    assert_eq!(bounce.to, Some(submission.sender.clone().into()));
    assert_eq!(bounce.from, submission.plan.sanitized_message.to);
    assert_eq!(bounce.bodies, submission.plan.sanitized_message.bodies);
    let errors: Vec<_> = bounce
        .payloads
        .iter()
        .filter_map(|payload| {
            xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).ok()
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].type_,
        xmpp_parsers::stanza_error::ErrorType::Cancel
    );
    assert_eq!(
        errors[0].defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable
    );
    assert!(frames.try_recv().is_err());
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("ingress_effect_receipts").await, 1);

    // Capacity returns; a new lost row proves the next pass can now store data.
    let available = environment(&f, connections, QuotaPolicy::Unlimited).await;
    bind(&authority, &available);
    let (witness, witness_id) = offline_plan(&f, "quota-capacity-returned");
    assert_eq!(
        authority.commit(&witness).await.class,
        IngressDecisionClass::Accepted
    );
    recover(&f, &authority, 2).await;
    let rows = available
        .storage
        .list(&"juliet@example.com".parse().expect("recipient"))
        .await
        .expect("pending rows after capacity returns");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, witness_id);
    assert!(
        rows.iter().all(|row| row.id != refused_id),
        "a refused obligation never becomes a queued message"
    );
    assert_eq!(f.count("ingress_effect_receipts").await, 2);
    assert!(
        frames.try_recv().is_err(),
        "a settled refusal never bounces again"
    );
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(authority);
    drop(available);
    drop(env);
    f.close().await;
}
#[tokio::test]
async fn sqlite_quota_refusal_bounces_once_and_is_not_requeued() {
    quota_refusal_bounces_once_and_is_not_requeued(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_quota_refusal_bounces_once_and_is_not_requeued() {
    if let Some(f) = IngressFixture::postgres("xep0160_quota_refusal").await {
        quota_refusal_bounces_once_and_is_not_requeued(f).await;
    }
}
