//! Node names are opaque and only identify a host registration with its service.

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};
use waddle_xmpp::push::{PushSubscription, PushSubscriptionStore};

use super::test_support::{notification_item, owner, scalar_i64};
use super::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration, PushOperatorNode,
};
use crate::db::{IntoParams, Value};
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
};

struct HostState {
    service: BareJid,
    node: String,
    pending: uuid::Uuid,
    history: uuid::Uuid,
}

#[derive(Debug, PartialEq)]
struct HostSnapshot {
    registration: Vec<Value>,
    pending: Vec<Value>,
    history: Vec<Value>,
    unsettled_lineage: i64,
    unsettled_foundation: i64,
}

async fn values(
    fixture: &IngressFixture,
    sql: &str,
    params: impl IntoParams,
    columns: usize,
) -> Vec<Value> {
    let conn = fixture.db.guard().await.expect("connection");
    let mut rows = conn.query(sql, params).await.expect("query");
    let row = rows.next().await.expect("row result").expect("row");
    (0..columns)
        .map(|column| row.get_value(column).expect("column"))
        .collect()
}

async fn snapshot(
    fixture: &IngressFixture,
    push: &DatabasePushServiceStore,
    host: &HostState,
) -> HostSnapshot {
    const OUTBOX: &str = "SELECT status, queue_acceptance_may_exist, last_error, updated_at_ms, published_at_ms, next_attempt_at_ms, context_xml FROM notification_outbox WHERE job_id = ?";
    HostSnapshot {
        registration: values(fixture, "SELECT status, last_error, failure_count, next_retry_at_ms, created_at_ms, updated_at_ms, publish_options_xml FROM push_registrations WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ?",
            crate::db_params![owner().to_string(), host.service.to_string(), &host.node], 7).await,
        pending: values(fixture, OUTBOX, crate::db_params![host.pending.to_string()], 7).await,
        history: values(fixture, OUTBOX, crate::db_params![host.history.to_string()], 7).await,
        unsettled_lineage: scalar_i64(push, "SELECT COUNT(*) FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL", crate::db_params![host.pending.to_string()]).await,
        unsettled_foundation: scalar_i64(push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![host.pending.to_string()]).await,
    }
}

async fn seed_host(fixture: &IngressFixture, node: &str, service: &BareJid) -> HostState {
    let registrations =
        crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
            .await
            .expect("registration schema");
    registrations
        .register(PushSubscription {
            user_jid: owner().to_string(),
            service_jid: service.to_string(),
            node: Some(node.to_owned()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("registration");
    fixture.execute("UPDATE push_registrations SET last_error = 'original registration marker', failure_count = 2, next_retry_at_ms = 17, created_at_ms = 3, updated_at_ms = 7 WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ?",
        crate::db_params![owner().to_string(), service.to_string(), node]).await;
    let pending = uuid::Uuid::new_v4();
    let history = uuid::Uuid::new_v4();
    let payload = String::from(&notification_item("scope").payload.expect("payload"));
    for (id, uncertain, marker, updated) in [
        (pending, 1_i64, "original parked marker", 11_i64),
        (history, 0_i64, "original terminal marker", 19_i64),
    ] {
        fixture.execute("INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, queue_acceptance_may_exist, last_error, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, 'bob@example.com', 'bob@example.com', '[]', 'dm', 1, ?, 'failed', ?, ?, 1, ?)",
            crate::db_params![id.to_string(), owner().to_string(), service.to_string(), node, &payload, uncertain, marker, updated]).await;
    }
    fixture.execute("INSERT INTO notification_outbox_lineage (candidate_delivery_id, job_id, settled_at_ms) VALUES (?, ?, NULL), (?, ?, 1)",
        crate::db_params![uuid::Uuid::new_v4().to_string(), pending.to_string(), uuid::Uuid::new_v4().to_string(), history.to_string()]).await;
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: owner(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: "bob@example.com".parse().expect("conversation"),
            archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                pending.to_string(),
                owner().into(),
            ),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut tx = fixture.uow.begin().await.expect("canonical transaction");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [67; 32]).expect("digest"),
        None,
    )
    .await
    .expect("message");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    EffectDescendantRepository::attach(&mut tx, key, &intent.semantic_key(), pending)
        .await
        .expect("custody");
    tx.commit().await.expect("commit");
    HostState {
        service: service.clone(),
        node: node.to_owned(),
        pending,
        history,
    }
}

async fn configured_store(fixture: &IngressFixture, service: &BareJid) -> DatabasePushServiceStore {
    let backing = Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("backing"),
    );
    DatabasePushServiceStore::new_with_secret_key_and_pubsub(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
        service.clone(),
        backing,
    )
    .await
    .expect("configured store")
}

async fn retirement_preserves_other_service_custody(fixture: IngressFixture) {
    crate::notification_outbox::NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    let service: BareJid = "push.example.com".parse().expect("service");
    let foreign_service: BareJid = "other-push.example.com".parse().expect("foreign service");
    let push = configured_store(&fixture, &service).await;

    // Even a same-owner/same-node host tuple at another service is unrelated.
    let node = push
        .ensure_node(&owner(), "configured-node")
        .await
        .expect("node");
    push.upsert_device(
        &owner(),
        PushDeviceRegistration::new("device", node.node(), PushDevicePlatform::Web, "test")
            .with_provider_endpoint(Some("https://push.example.com/opaque".to_owned())),
    )
    .await
    .expect("device");
    let local = seed_host(&fixture, node.node(), &service).await;
    let foreign = seed_host(&fixture, node.node(), &foreign_service).await;
    // Populate every foreign inventory dimension under the identical owner/node.
    let foreign_known = uuid::Uuid::new_v4();
    let foreign_due = uuid::Uuid::new_v4();
    for (id, status, uncertain) in [
        (foreign_known, "failed", 0_i64),
        (foreign_due, "queued", 1_i64),
    ] {
        fixture.execute("INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, queue_acceptance_may_exist, attempt_count, created_at_ms, updated_at_ms) SELECT ?, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, ?, ?, 5, created_at_ms, updated_at_ms FROM notification_outbox WHERE job_id = ?",
            crate::db_params![id.to_string(), status, uncertain, foreign.pending.to_string()]).await;
    }
    fixture.execute("INSERT INTO notification_outbox_lineage (candidate_delivery_id, job_id, settled_at_ms) SELECT candidate_delivery_id, ?, NULL FROM notification_outbox_lineage WHERE job_id = ?",
        crate::db_params![foreign_known.to_string(), foreign.pending.to_string()]).await;
    let local_before = snapshot(&fixture, &push, &local).await;
    let foreign_before = snapshot(&fixture, &push, &foreign).await;
    assert_eq!(foreign_before.unsettled_lineage, 1);
    assert_eq!(foreign_before.unsettled_foundation, 1);
    let target = PushOperatorNode::parse(node.node()).expect("target");
    let inspected = push
        .operator_inventory(&owner(), &target)
        .await
        .expect("local inventory");
    assert_eq!(inspected.outbox_retained, 2);
    assert_eq!(inspected.outbox_parked, 1);
    assert_eq!(inspected.outbox_at_budget, 0);
    assert_eq!(inspected.outbox_unresolved_failed, 0);
    push.disable_nodes_for_owner(&owner(), Some(node.node()))
        .await
        .expect("retire exact local node");
    assert_eq!(snapshot(&fixture, &push, &foreign).await, foreign_before);
    let local_after = snapshot(&fixture, &push, &local).await;
    assert_eq!(
        local_after.registration[0],
        Value::Text("disabled".to_owned())
    );
    assert_eq!(local_after.pending[1], Value::Integer(0));
    assert_eq!(
        local_after.history, local_before.history,
        "settled terminal diagnosis and retention tail survive retirement"
    );
    assert_eq!(local_after.unsettled_lineage, 0);
    assert_eq!(local_after.unsettled_foundation, 0);
    let inspected = push
        .operator_inventory(&owner(), &target)
        .await
        .expect("retired local inventory");
    assert_eq!(inspected.outbox_retained, 2);
    assert_eq!(inspected.outbox_parked, 0);
    assert_eq!(inspected.outbox_at_budget, 0);
    assert_eq!(inspected.outbox_unresolved_failed, 0);
    let device = push
        .get_device_for_owner_on_node(&owner(), node.node(), "device")
        .await
        .expect("device lookup")
        .expect("device");
    assert_eq!(device.provider_endpoint(), None);

    // A local orphan can be explicitly retired without a local provider node.
    let absent = format!("absent-{}", uuid::Uuid::new_v4());
    let orphan = seed_host(&fixture, &absent, &service).await;
    let foreign_orphan = seed_host(&fixture, &absent, &foreign_service).await;
    fixture
        .execute(
            "UPDATE notification_outbox SET queue_acceptance_may_exist = 0 WHERE job_id = ?",
            crate::db_params![orphan.pending.to_string()],
        )
        .await;
    let foreign_orphan_before = snapshot(&fixture, &push, &foreign_orphan).await;
    let absent_target = PushOperatorNode::parse(&absent).expect("absent target");
    let inspected = push
        .operator_inventory(&owner(), &absent_target)
        .await
        .expect("orphan inventory");
    assert_eq!(inspected.outbox_retained, 2);
    assert_eq!(inspected.outbox_parked, 0);
    assert_eq!(inspected.outbox_unresolved_failed, 1);
    push.disable_nodes_for_owner(&owner(), Some(&absent))
        .await
        .expect("retire local orphan");
    let orphan_after = snapshot(&fixture, &push, &orphan).await;
    assert_eq!(
        orphan_after.registration[0],
        Value::Text("disabled".to_owned())
    );
    assert_eq!(orphan_after.unsettled_lineage, 0);
    assert_eq!(orphan_after.unsettled_foundation, 0);
    let inspected = push
        .operator_inventory(&owner(), &absent_target)
        .await
        .expect("retired orphan inventory");
    assert_eq!(inspected.outbox_parked, 0);
    assert_eq!(inspected.outbox_unresolved_failed, 0);
    assert_eq!(
        snapshot(&fixture, &push, &foreign_orphan).await,
        foreign_orphan_before
    );

    // A standalone store has no authority to infer any host-service boundary.
    let standalone = DatabasePushServiceStore::new(fixture.db.clone())
        .await
        .expect("standalone store");
    let standalone_node = standalone
        .ensure_node(&owner(), "unconfigured-node")
        .await
        .expect("standalone node");
    let standalone_local = seed_host(&fixture, standalone_node.node(), &service).await;
    let standalone_foreign = seed_host(&fixture, standalone_node.node(), &foreign_service).await;
    let standalone_local_before = snapshot(&fixture, &standalone, &standalone_local).await;
    let standalone_foreign_before = snapshot(&fixture, &standalone, &standalone_foreign).await;
    let accepted = standalone
        .enqueue_notification_publish_job_from_user_server(
            standalone_node.node(),
            &notification_item("standalone-retirement"),
            &owner(),
        )
        .await
        .expect("provider enqueue");
    standalone
        .disable_nodes_for_owner(&owner(), Some(standalone_node.node()))
        .await
        .expect("standalone provider retirement");
    assert_eq!(
        snapshot(&fixture, &standalone, &standalone_local).await,
        standalone_local_before
    );
    assert_eq!(
        snapshot(&fixture, &standalone, &standalone_foreign).await,
        standalone_foreign_before
    );
    let job = standalone
        .load_publish_job(&accepted.job_id().to_string())
        .await
        .expect("provider lookup")
        .expect("retained provider history");
    assert_eq!(job.status(), "failed");
    assert_eq!(scalar_i64(&standalone, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND terminal_disposition = 'registration-revoked'", crate::db_params![accepted.job_id().to_string()]).await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_retirement_preserves_other_service_custody() {
    retirement_preserves_other_service_custody(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_retirement_preserves_other_service_custody() {
    if let Some(fixture) = IngressFixture::postgres("retirement_service_scope").await {
        retirement_preserves_other_service_custody(fixture).await;
    }
}
