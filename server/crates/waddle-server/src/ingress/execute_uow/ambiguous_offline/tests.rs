use super::*;
use crate::ingress::{commit::commit_submission, receipt_key, test_support::IngressFixture};
use crate::ingress_uow::{EffectReceiptRepository, SendClaim};
use crate::server::routes::interpret::{
    effects::{
        delivery::ExternalDeliveryEffect, Effect, ExternalEffect, ImmediateSink, PlannedEffect,
    },
    DeliveryExecutionContext,
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    ingress::EffectMessageIdentity,
    ownership::NodeIdentity,
    pending_delivery::{storage::PendingDeliveryStorage, QuotaPolicy},
    registry::ConnectionRegistry,
    xep::xep0334::{add_hint, Hint},
};

async fn run_handoff(
    fixture: IngressFixture,
    hint: Option<Hint>,
    settled_first: bool,
    siblings: bool,
    initial_reservation: bool,
) {
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            fixture.db.clone(),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage"),
    );
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let resource: FullJid = "juliet@example.com/phone".parse().expect("resource");
    let mut resources = vec![resource.clone()];
    if siblings {
        resources.push("juliet@example.com/laptop".parse().expect("sibling"));
    }
    let intent = IngressEffectIntent::RouteDirect {
        prepared: None,
        recipient: resource.to_bare(),
        fanout: resources.clone(),
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let canonical_intent = intent
        .with_encoded_v1(IngressEffectIntent::decode_v1)
        .expect("encode")
        .expect("canonical authority");
    let mut submission = fixture.submission(None, "recovered after pod crash");
    if let Some(hint) = hint {
        add_hint(&mut submission.plan.sanitized_message, hint);
    }
    let archive = IngressEffectIntent::ArchiveAuthoritative {
        ordinal: None,
        archive: resource.to_bare(),
        by: resource.to_bare(),
        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
            "fallback-source",
            resource.to_bare().into(),
        ),
        archived_at: chrono::Utc::now(),
    };
    submission.plan.intents = vec![intent.clone()];
    if hint.is_none() {
        use waddle_xmpp::mam::MamStorage;
        let IngressEffectIntent::ArchiveAuthoritative { stanza_id, .. } = &archive else {
            panic!("archive fixture")
        };
        let mut message = waddle_xmpp::mam::ArchivedMessage::for_test(
            submission.sender.clone().into(),
            resource.to_bare().into(),
        );
        message.id = stanza_id.id.clone();
        message.stanza_id = Some(stanza_id.clone());
        waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("real handoff archive")
            .store_message(&resource.to_bare(), &message)
            .await
            .expect("committed archive position");
        submission.plan.intents.push(archive);
    }
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(EffectMessageIdentity::capture_ordinal(1)),
            call_setup: None,
            bare: resource.to_bare(),
            resources: resources.clone(),
            stanza: Box::new(waddle_xmpp::Stanza::Message(
                submission.plan.sanitized_message.clone(),
            )),
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("canonical");
    let key = decision.message_key.expect("key");
    let progress = RouteProgress::from_intent(&intent, None, vec![])
        .expect("progress")
        .expect("direct");
    let obligation = SendObligation {
        message: key,
        receipt: receipt_key(&intent).expect("receipt"),
        recipient: resource.clone(),
    };
    let mut tx = fixture.uow.begin().await.expect("claim");
    let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
        &mut tx,
        &obligation,
        &NodeIdentity::local(),
        Duration::from_secs(5),
    )
    .await
    .expect("claim") else {
        panic!("fresh lease")
    };
    if !initial_reservation {
        assert!(SendAttemptRepository::start(&mut tx, &lease)
            .await
            .expect("start"));
    }
    let sibling_lease = if siblings {
        let sibling = SendObligation {
            recipient: resources[1].clone(),
            ..obligation.clone()
        };
        let SendClaim::Acquired(other) = SendAttemptRepository::claim(
            &mut tx,
            &sibling,
            &NodeIdentity::local(),
            Duration::from_secs(5),
        )
        .await
        .expect("sibling claim") else {
            panic!("fresh sibling")
        };
        if !initial_reservation {
            assert!(SendAttemptRepository::start(&mut tx, &other)
                .await
                .expect("sibling start"));
        }
        Some(other)
    } else {
        None
    };
    tx.commit().await.expect("commit start");
    assert!(handoff(&fixture.uow, &deps, key, &progress, &resource)
        .await
        .expect("unexpired check")
        .is_none());
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    if settled_first && siblings {
        let mut tx = fixture
            .uow
            .begin()
            .await
            .expect("concurrent resource settlement");
        crate::ingress_uow::DeliveryProgressRepository::record(
            &mut tx,
            key,
            &progress.receipt,
            std::slice::from_ref(&resource),
        )
        .await
        .expect("resource proof");
        tx.commit().await.expect("resource proof commit");
        assert!(handoff(&fixture.uow, &deps, key, &progress, &resource)
            .await
            .expect("resource proof first")
            .is_none());
        assert_eq!(
            fixture.count("pending_delivery").await,
            0,
            "stale expired marker cannot replace proven resource acceptance"
        );
        fixture.close().await;
        return;
    }
    if settled_first {
        let mut tx = fixture
            .uow
            .begin()
            .await
            .expect("concurrent policy settlement");
        EffectReceiptRepository::record_receipt(
            &mut tx,
            key,
            progress.receipt.kind,
            &progress.receipt.semantic_identity_hash,
        )
        .await
        .expect("settled route");
        tx.commit().await.expect("settlement commit");
        assert_eq!(
            handoff(&fixture.uow, &deps, key, &progress, &resource)
                .await
                .expect("proof first"),
            Some(vec![canonical_intent])
        );
        assert_eq!(
            fixture.count("pending_delivery").await,
            0,
            "a policy-settled route must not recreate delivery from its expired marker"
        );
        fixture.close().await;
        return;
    }
    if !initial_reservation {
        // Crash after reclaim committed but before its start. The persisted
        // lineage must still authorize bounded fallback after this lease expires.
        let mut reclaimed_tx = fixture.uow.begin().await.expect("reclaim");
        assert!(matches!(
            SendAttemptRepository::claim(
                &mut reclaimed_tx,
                &obligation,
                &NodeIdentity::local(),
                Duration::from_secs(5)
            )
            .await
            .expect("reclaim"),
            SendClaim::Acquired(_)
        ));
        reclaimed_tx.commit().await.expect("reclaim commit");
        fixture
            .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
            .await;
    }
    if siblings {
        // A newly reclaimed sibling may not be displaced while its sink
        // authority is still current, either before or after start.
        let sibling = SendObligation {
            recipient: resources[1].clone(),
            ..obligation.clone()
        };
        let mut tx = fixture.uow.begin().await.expect("active sibling");
        let SendClaim::Acquired(active) = SendAttemptRepository::claim(
            &mut tx,
            &sibling,
            &NodeIdentity::local(),
            Duration::from_secs(5),
        )
        .await
        .expect("active sibling claim") else {
            panic!("expired sibling")
        };
        tx.commit().await.expect("active sibling commit");
        assert!(handoff(&fixture.uow, &deps, key, &progress, &resource)
            .await
            .expect("leased sibling")
            .is_none());
        let mut tx = fixture.uow.begin().await.expect("active sibling start");
        assert!(SendAttemptRepository::start(&mut tx, &active)
            .await
            .expect("active sibling start"));
        tx.commit().await.expect("active sibling start commit");
        assert!(handoff(&fixture.uow, &deps, key, &progress, &resource)
            .await
            .expect("started sibling")
            .is_none());
        fixture
            .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
            .await;
        assert_eq!(
            handoff(&fixture.uow, &deps, key, &progress, &resource)
                .await
                .expect("first sibling handoff"),
            Some(vec![canonical_intent.clone()])
        );
        let mut tx = fixture
            .uow
            .begin()
            .await
            .expect("retired current sibling token");
        assert!(!SendAttemptRepository::complete(&mut tx, &active)
            .await
            .expect("current sibling token retired"));
        assert!(
            !SendAttemptRepository::release_proven_not_enqueued(&mut tx, &active)
                .await
                .expect("current sibling release fenced")
        );
        tx.commit().await.expect("retirement check");
    } else {
        let report = crate::ingress::execute::execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert!(
            report.receipt_failures.is_empty(),
            "offline handoff must settle without fabricating socket acceptance"
        );
    }
    let rows = storage
        .list_unoutboxed_archived(10)
        .await
        .expect("notification backlog");
    if hint.is_none() {
        assert_eq!(
            rows.len(),
            1,
            "existing notification janitor must discover recovered delivery"
        );
        assert!(matches!(&rows[0].payload, PendingPayload::Archived(_)));
        assert_eq!(rows[0].id, pending_id(key, &progress.receipt));
    } else {
        assert!(
            rows.is_empty(),
            "restricted hints must not invent archive-backed push"
        );
    }
    assert_eq!(
        fixture.count("pending_delivery").await,
        if hint == Some(Hint::NoStore) { 0 } else { 1 }
    );
    let mut tx = fixture.uow.begin().await.expect("proof");
    let completed =
        crate::ingress_uow::DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
            .await
            .expect("resource proof");
    assert!(
        completed.is_empty(),
        "recipient-wide offline custody is not individual socket delivery evidence"
    );
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        obligation.receipt.kind,
        &obligation.receipt.semantic_identity_hash
    )
    .await
    .expect("settlement"));
    assert!(!SendAttemptRepository::complete(&mut tx, &lease)
        .await
        .expect("old token revoked"));
    assert!(
        !SendAttemptRepository::release_proven_not_enqueued(&mut tx, &lease)
            .await
            .expect("old release revoked")
    );
    if let Some(other) = sibling_lease {
        assert!(!SendAttemptRepository::complete(&mut tx, &other)
            .await
            .expect("old sibling token revoked"));
        assert!(
            !SendAttemptRepository::release_proven_not_enqueued(&mut tx, &other)
                .await
                .expect("old sibling cannot delete")
        );
    }
    tx.commit().await.expect("proof commit");
    fixture.execute("DELETE FROM pending_delivery", ()).await;
    assert_eq!(
        handoff(
            &fixture.uow,
            &deps,
            key,
            &progress,
            resources.last().expect("last sibling")
        )
        .await
        .expect("consumed retry"),
        Some(vec![canonical_intent])
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "a consumed fallback is never recreated"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_expired_start_hands_off_to_pending_notification_recovery() {
    run_handoff(IngressFixture::sqlite().await, None, false, false, false).await;
}

#[tokio::test]
async fn sqlite_ambiguous_fallback_respects_no_store() {
    run_handoff(
        IngressFixture::sqlite().await,
        Some(Hint::NoStore),
        false,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn sqlite_ambiguous_fallback_preserves_transient_storage_policy() {
    run_handoff(
        IngressFixture::sqlite().await,
        Some(Hint::NoPermanentStore),
        false,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn postgres_expired_start_hands_off_to_pending_notification_recovery() {
    if let Some(fixture) = IngressFixture::postgres("ambiguous_offline").await {
        run_handoff(fixture, None, false, false, false).await;
    }
}

#[tokio::test]
async fn sqlite_settled_route_with_expired_start_cannot_recreate_pending_delivery() {
    run_handoff(IngressFixture::sqlite().await, None, true, false, false).await;
}

#[tokio::test]
async fn sqlite_expired_siblings_share_archived_pending_custody() {
    run_handoff(IngressFixture::sqlite().await, None, false, true, false).await;
}

#[tokio::test]
async fn postgres_expired_siblings_share_archived_pending_custody() {
    if let Some(fixture) = IngressFixture::postgres("ambiguous_siblings").await {
        run_handoff(fixture, None, false, true, false).await;
    }
}

#[tokio::test]
async fn sqlite_transient_sibling_handoff_survives_consumption() {
    run_handoff(
        IngressFixture::sqlite().await,
        Some(Hint::NoPermanentStore),
        false,
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn postgres_transient_sibling_handoff_survives_consumption() {
    if let Some(fixture) = IngressFixture::postgres("transient_sibling_handoff").await {
        run_handoff(fixture, Some(Hint::NoPermanentStore), false, true, false).await;
    }
}

#[tokio::test]
async fn sqlite_expired_initial_reservation_hands_off_offline() {
    run_handoff(IngressFixture::sqlite().await, None, false, false, true).await;
}

#[tokio::test]
async fn postgres_expired_initial_reservation_hands_off_offline() {
    if let Some(fixture) = IngressFixture::postgres("initial_reservation_handoff").await {
        run_handoff(fixture, None, false, false, true).await;
    }
}

#[tokio::test]
async fn sqlite_accepted_resource_with_expired_marker_cannot_handoff() {
    run_handoff(IngressFixture::sqlite().await, None, true, true, false).await;
}

#[tokio::test]
async fn postgres_accepted_resource_with_expired_marker_cannot_handoff() {
    if let Some(fixture) = IngressFixture::postgres("resource_proof_before_handoff").await {
        run_handoff(fixture, None, true, true, false).await;
    }
}
