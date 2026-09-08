//! Required recipient reads refuse ingress; historical bounces remain authoritative.
use super::super::{
    effects::{PlanFailure, PlanSink},
    message_plan::{build_plan_deps, finish_plan},
    Deps,
};
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressEffectCapture, IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::SmIngressStreamRepository;
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, WireHandledCount},
    pending_delivery::SmSessionId,
    protocol::StanzaDispatcher,
    registry::ConnectionRegistry,
    stream_management::{InMemorySmSessionRegistry, SmRegistryError},
    xep::xep0191::BlockingStorage,
    Stanza,
};

async fn resumable(fixture: &IngressFixture) -> IngressSubmission {
    let mut submission = fixture.submission(Some("recipient-read-retry"), "hello");
    let stream_id = SmSessionId::new("recipient-read-stream");
    let mut tx = fixture.uow.begin().await.expect("begin");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("commit stream");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(1),
        checkpoint_h: WireHandledCount::new(1),
    };
    submission
}

async fn assert_refused(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
    expected: PlanFailure,
) {
    assert_eq!(submission.plan.failure, Some(expected));
    let failure = commit_submission(&fixture.uow, submission, 1)
        .await
        .expect_err("incomplete plan");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
        "mam_messages",
        "inbox_entries",
    ] {
        assert_eq!(fixture.count(table).await, 0, "{table}");
    }
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 0 AND checkpoint_h = 0")
            .await,
        1
    );
}

async fn blocklist_failure(fixture: IngressFixture) {
    let registry = ConnectionRegistry::new();
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let dispatcher = Arc::new(dispatcher);
    let blocking: Arc<dyn BlockingStorage> = Arc::new(
        crate::db::blocking::DatabaseBlockingStorage::new(fixture.db.clone()),
    );
    let mam: Arc<dyn waddle_xmpp::mam::MamStorage> = Arc::new(
        waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let mut deps = Deps::registry_only(&registry);
    deps.message_dispatcher = Some(&dispatcher);
    deps.blocking_storage = Some(&blocking);
    deps.mam_storage = Some(&mam);
    let mut submission = resumable(&fixture).await;
    let incoming = submission.plan.sanitized_message.clone();
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    fixture
        .execute(
            "ALTER TABLE blocking_list RENAME TO unavailable_blocking_list",
            (),
        )
        .await;
    for fail in [true, false] {
        let mut machine =
            waddle_xmpp::protocol::XmppStateMachine::new("example.com", (*dispatcher).clone());
        machine.transition_to_ready(submission.sender.clone(), false);
        submission.plan = super::super::message_plan::plan_message_dispatch(
            &mut machine,
            incoming.clone(),
            &deps,
        )
        .await;
        if fail {
            assert_refused(&fixture, &submission, PlanFailure::RecipientBlocklistRead).await;
            fixture
                .execute(
                    "ALTER TABLE unavailable_blocking_list RENAME TO blocking_list",
                    (),
                )
                .await;
        } else {
            assert_eq!(submission.plan.failure, None);
            assert!(submission.plan.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { archive, .. } if archive == &recipient)));
            let decision = commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect("healthy retry");
            assert!(decision.class.advances());
            assert_eq!(
                fixture
                    .count("ingress_sm_streams WHERE handled_ordinal = 1 AND checkpoint_h = 1")
                    .await,
                1
            );
        }
    }
    drop(mam);
    drop(blocking);
    fixture.close().await;
}

async fn detached_inventory_failure(fixture: IngressFixture) {
    let registry = ConnectionRegistry::new();
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    let mut deps = Deps::registry_only(&registry);
    deps.sm_session_registry = Some(&sm);
    let mut submission = resumable(&fixture).await;
    let incoming = submission.plan.sanitized_message.clone();
    let target = "juliet@example.com/phone".parse().expect("target");
    for fail in [true, false] {
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new();
        let planned =
            build_plan_deps(&deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
        if fail {
            // The concrete registry only fails on private RwLock poisoning.
            // Feed its exact error through the production result boundary.
            assert!(super::plan::detached_inventory(
                &planned,
                Err(SmRegistryError::Internal("Lock poisoned".into()))
            )
            .is_empty());
        }
        super::plan::deliver_full(&planned, &target, &Stanza::Message(incoming.clone()), None)
            .await;
        submission.plan = finish_plan(
            &sink,
            &capture,
            incoming.clone(),
            Some(submission.sender.clone()),
        );
        if fail {
            assert_refused(&fixture, &submission, PlanFailure::DetachedInventoryRead).await;
        } else {
            assert_eq!(submission.plan.failure, None);
            assert!(commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect("healthy retry")
                .class
                .advances());
            assert_eq!(
                fixture
                    .count("ingress_sm_streams WHERE handled_ordinal = 1 AND checkpoint_h = 1")
                    .await,
                1
            );
        }
    }
    fixture.close().await;
}

async fn nonexistent_rejection(fixture: IngressFixture) {
    use super::super::effects::{ExternalEffect, ImmediateSink, PlanRejection, PolicyDeniedReason};
    use crate::ingress::execute::{execute_effects, terminalize_if_complete};
    use std::time::Duration;
    let registry = ConnectionRegistry::new();
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("database pool");
    // Reuse a standalone authority so the shared pool is not re-enrolled with a
    // second deployment lineage over the fixture's own attestation.
    let standalone = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
            Arc::new(pool),
            Arc::clone(&standalone.deps.protocol.ingress),
        )
        .await;
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let dispatcher = Arc::new(dispatcher);
    let mam: Arc<dyn waddle_xmpp::mam::MamStorage> = Arc::new(
        waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.message_dispatcher = Some(&dispatcher);
    deps.mam_storage = Some(&mam);
    let mut submission = fixture.submission(Some("nonexistent-origin"), "hello");
    let incoming = submission.plan.sanitized_message.clone();
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let planned = build_plan_deps(&deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    super::route_to_connection(
        &planned,
        incoming.to.clone().expect("target"),
        Box::new(Stanza::Message(incoming.clone())),
        0,
        None,
    )
    .await;
    submission.plan = finish_plan(&sink, &capture, incoming, Some(submission.sender.clone()));
    // The bounce is authority, not a side effect: a recorded semantic rejection
    // plus a receipt-capable reply obligation, never an accepted canonical row.
    assert!(matches!(
        submission.plan.rejection,
        Some(PlanRejection::PolicyDenied(
            PolicyDeniedReason::StanzaError(_)
        ))
    ));
    assert!(matches!(
        submission.plan.intents.as_slice(),
        [IngressEffectIntent::ErrorReply { .. }]
    ));
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    let [ExternalEffect::Frame(_)] = decision.external.as_slice() else {
        panic!(
            "only the recorded error may execute: {:?}",
            decision.external
        );
    };
    let mut report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    // Confirm the exact prepared frame through the writer-completion seam.
    assert_eq!(report.frame_obligations.len(), 1);
    report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("frame receipt");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(
        terminalize_if_complete(&fixture.uow, decision.message_key.expect("key"))
            .await
            .expect("terminalize")
    );
    // The recorded denial is the durable decision; no recipient obligation was
    // ever accepted, so later account creation cannot resurrect one.
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 0);
    assert_eq!(fixture.count("inbox_entries").await, 0);
    assert_eq!(fixture.count("ingress_deliveries").await, 0);
    drop(mam);
    drop(state);
    drop(standalone);
    fixture.close().await;
}

#[tokio::test]
async fn headless_blocklist_failure_sqlite() {
    blocklist_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn headless_blocklist_failure_postgres() {
    if let Some(fixture) = IngressFixture::postgres("headless_blocklist").await {
        blocklist_failure(fixture).await;
    }
}
#[tokio::test]
async fn detached_inventory_failure_sqlite() {
    detached_inventory_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_inventory_failure_postgres() {
    if let Some(fixture) = IngressFixture::postgres("detached_inventory").await {
        detached_inventory_failure(fixture).await;
    }
}
#[tokio::test]
async fn nonexistent_bounce_recorded_as_rejection_sqlite() {
    nonexistent_rejection(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn nonexistent_bounce_recorded_as_rejection_postgres() {
    if let Some(fixture) = IngressFixture::postgres("nonexistent_bounce").await {
        nonexistent_rejection(fixture).await;
    }
}
