//! Phase C records each successful resource before settling the frozen batch.
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    stream_management::{DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry},
};

fn capture_delivery(deps: &Deps<'_>, effect: ExternalDeliveryEffect) -> Vec<PlannedEffect> {
    let sink = crate::server::routes::interpret::effects::PlanSink::new();
    let mut planned = deps.clone();
    planned.effects = &sink;
    crate::server::routes::interpret::effects::delivery::record(&planned, effect);
    sink.take().0
}

async fn detached_receipts(fixture: IngressFixture, missing_second: bool, live_second: bool) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("phone");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("laptop");
    for resource in [&first, &second] {
        if resource == &second && (missing_second || live_second) {
            continue;
        }
        sm.store_session(DetachedSession {
            stream_id: resource.to_string(),
            user_id: resource.to_bare().to_string(),
            jid: resource.clone(),
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
        .expect("detached session");
    }
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    if live_second {
        crate::server::routes::websocket::tests::register_test_connection(&state, &second, tx)
            .await;
    }
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.sm_session_registry = Some(&sm);
    let mut submission = fixture.submission(Some("detached-proof"), "detached fanout");
    let identity = EffectMessageIdentity::capture_ordinal(1);
    let intent = IngressEffectIntent::RouteDirect {
        recipient: first.to_bare(),
        fanout: vec![first.clone(), second.clone()],
        route_identity: identity.clone(),
    };
    let receipt = crate::ingress::durable::receipt_key(&intent).expect("route receipt");
    submission.plan.intents = vec![intent];
    submission.plan.plan = capture_delivery(
        &deps,
        ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity.clone()),
            call_setup: None,
            bare: first.to_bare(),
            resources: vec![first.clone(), second.clone()],
            stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
        },
    );
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let key = decision.message_key.expect("canonical key");
    let mut tx = fixture.uow.begin().await.expect("receipt transaction");
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("receipt lookup"),
        !missing_second
    );
    let progress = crate::ingress_uow::DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("resource progress");
    assert!(progress.contains(&first));
    assert_eq!(progress.contains(&second), !missing_second);
    tx.commit().await.expect("read receipt");
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("terminalization"),
        !missing_second
    );
    assert_eq!(
        sm.peek_session(&first.to_string())
            .await
            .expect("peek")
            .expect("first session")
            .unacked_stanzas
            .len(),
        1
    );
    if missing_second {
        let replay = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("alias replay commit");
        assert_eq!(replay.message_key, Some(key));
        let replay_report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &replay,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert!(replay_report.receipt_failures.is_empty());
        assert_eq!(
            sm.peek_session(&first.to_string())
                .await
                .expect("peek after alias replay")
                .expect("first session retained")
                .unacked_stanzas
                .len(),
            1,
            "aggregate pending receipt must not repeat the completed resource"
        );
        assert!(
            !terminalize_if_complete(&fixture.uow, key)
                .await
                .expect("partial batch remains pending"),
            "missing resources remain unresolved until a retry can reach them"
        );
        let resumed = sm
            .take_session(&first.to_string())
            .await
            .expect("resume completed resource")
            .expect("completed resource's replay carrier");
        assert_eq!(resumed.unacked_stanzas.len(), 1);
        let (resumed_tx, mut resumed_rx) = tokio::sync::mpsc::channel(8);
        crate::server::routes::websocket::tests::register_test_connection(
            &state, &first, resumed_tx,
        )
        .await;
        // Replan the now-live resource with its original capture identity. The
        // frozen aggregate still includes the unavailable laptop, so absence
        // of its receipt is not evidence that phone needs another delivery.
        submission.plan.plan = capture_delivery(
            &deps,
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(identity),
                jid: first.clone(),
                stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
                kind: crate::server::routes::interpret::effects::delivery::PeerDeliveryKind::PeerStanza,
                call_setup: None,
            },
        );
        let replay = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("live resource alias replay commit");
        assert_eq!(replay.message_key, Some(key));
        assert!(
            replay.external.is_empty(),
            "aggregate cannot authorize a live subset retry"
        );
        let replay_report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &replay,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert!(replay_report.receipt_failures.is_empty());
        assert!(
            matches!(
                resumed_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "a resumed target of a partial aggregate must not receive another live delivery"
        );
        assert!(!terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("aggregate remains incomplete after resume"));
    }
    if live_second {
        assert!(
            rx.try_recv().is_ok(),
            "detached target resumed and accepted the live retry"
        );
    } else if !missing_second {
        assert_eq!(
            sm.peek_session(&second.to_string())
                .await
                .expect("peek")
                .expect("second session")
                .unacked_stanzas
                .len(),
            1
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_detached_multi_resource_complete_receipts_and_terminalizes() {
    detached_receipts(IngressFixture::sqlite().await, false, false).await;
}
#[tokio::test]
async fn postgres_detached_multi_resource_complete_receipts_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("detached_complete").await {
        detached_receipts(fixture, false, false).await;
    }
}
#[tokio::test]
async fn sqlite_detached_partial_delivery_keeps_receipt_pending() {
    detached_receipts(IngressFixture::sqlite().await, true, false).await;
}
#[tokio::test]
async fn postgres_detached_partial_delivery_keeps_receipt_pending() {
    if let Some(fixture) = IngressFixture::postgres("detached_partial").await {
        detached_receipts(fixture, true, false).await;
    }
}
#[tokio::test]
async fn sqlite_detached_resumed_resource_live_retry_completes_receipt() {
    detached_receipts(IngressFixture::sqlite().await, false, true).await;
}
#[tokio::test]
async fn postgres_detached_resumed_resource_live_retry_completes_receipt() {
    if let Some(fixture) = IngressFixture::postgres("detached_live_retry").await {
        detached_receipts(fixture, false, true).await;
    }
}
