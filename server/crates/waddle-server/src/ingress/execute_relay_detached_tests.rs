//! A frozen remote route retains its receipt identity when execution falls back locally.
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    stream_management::{
        DetachedSession, InMemorySmSessionRegistry, SmIngressAppendKey, SmIngressReceiptKind,
        SmSessionRegistry,
    },
};

async fn relay_fallback_receipt_failure(fixture: IngressFixture) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    sm.store_session(DetachedSession {
        stream_id: recipient.to_string(),
        user_id: recipient.to_bare().to_string(),
        jid: recipient.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    })
    .await
    .expect("store detached session");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.sm_session_registry = Some(&sm);
    let mut submission = fixture.submission(Some("relay-detached-retry"), "remote planned DM");
    let identity = EffectMessageIdentity::capture_ordinal(7);
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: identity.clone(),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("recorded route receipt");
    submission.plan.intents = vec![intent];
    // Phase A selected the remote relay. By execution the resource is local and
    // detached: no remote bridge claims it, so RelayFullJid uses peer fallback.
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
            route_identity: Some(identity),
            origin: None,
            target: recipient.clone(),
            stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
            call_setup: None,
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit frozen remote route");
    let message_key = decision.message_key.expect("canonical message key");
    assert_eq!(decision.external_receipts[0], vec![receipt.clone()]);
    assert!(
        decision.arm_owned_receipts.is_empty(),
        "relay settles generically"
    );
    let hash = hex::encode(receipt.semantic_identity_hash);
    // This SQL represents stored receipt bytes, not XML or protocol payloads.
    match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => fixture.execute(&format!("CREATE TRIGGER fail_relay_receipt BEFORE INSERT ON ingress_effect_receipts WHEN NEW.semantic_identity_hash = X'{hash}' BEGIN SELECT RAISE(FAIL, 'injected relay receipt failure'); END"), ()).await,
        crate::db::DatabaseDriver::Postgres => {
            fixture.execute(&format!("CREATE FUNCTION fail_relay_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.semantic_identity_hash = decode('{hash}', 'hex') THEN RAISE EXCEPTION 'injected relay receipt failure'; END IF; RETURN NEW; END $$"), ()).await;
            fixture.execute("CREATE TRIGGER fail_relay_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_relay_receipt()", ()).await;
        }
    }
    let failed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(failed.receipt_failures.len(), 1);
    assert_eq!(failed.receipt_failures[0].0, receipt);
    assert_eq!(queue_len(&sm, &recipient).await, 1);
    let append_key = SmIngressAppendKey {
        message_key,
        kind: SmIngressReceiptKind::from_storage(receipt.kind.to_storage()),
        semantic_identity_hash: receipt.semantic_identity_hash,
        resource: recipient.clone(),
    };
    assert!(
        crate::sm_persistence::ingress_append::get(&fixture.db, &append_key)
            .await
            .expect("ledger lookup")
            .is_some(),
        "fallback must preserve the exact recorded receipt identity"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    assert!(!terminalize_if_complete(&fixture.uow, message_key)
        .await
        .expect("pending receipt"));
    let drop_trigger = match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "DROP TRIGGER fail_relay_receipt",
        crate::db::DatabaseDriver::Postgres => {
            "DROP TRIGGER fail_relay_receipt ON ingress_effect_receipts"
        }
    };
    fixture.execute(drop_trigger, ()).await;
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry frozen route");
    assert_eq!(retry.message_key, Some(message_key));
    assert_eq!(retry.external_receipts[0], vec![receipt]);
    let completed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(completed.receipt_failures.is_empty());
    assert_eq!(completed.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        queue_len(&sm, &recipient).await,
        1,
        "receipt retry allocates no additional queue entry"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(&fixture.uow, message_key)
        .await
        .expect("terminal receipt"));
    fixture.close().await;
}

async fn queue_len(sm: &InMemorySmSessionRegistry, recipient: &jid::FullJid) -> usize {
    sm.peek_session(&recipient.to_string())
        .await
        .expect("peek session")
        .expect("detached session")
        .unacked_stanzas
        .len()
}

#[tokio::test]
async fn sqlite_remote_planned_relay_detached_receipt_failure_does_not_reappend() {
    relay_fallback_receipt_failure(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_remote_planned_relay_detached_receipt_failure_does_not_reappend() {
    if let Some(fixture) = IngressFixture::postgres("relay_detached_receipt_retry").await {
        relay_fallback_receipt_failure(fixture).await;
    }
}
