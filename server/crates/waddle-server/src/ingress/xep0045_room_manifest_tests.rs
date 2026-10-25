//! XEP-0045 frozen room acceptance and crash/retry authority.
use super::commit::commit_submission;
use super::test_support::IngressFixture;
use super::{IngressStreamIdentity, IngressSubmission, RoomManifestSnapshot};
use crate::ingress_uow::{CanonicalMessageRepository, EffectIntentRepository};
use crate::server::routes::interpret::effects::room::RoomFenceRequirement;
use crate::server::routes::interpret::effects::RoomExecutionPath;
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
    NormalizedTarget,
};
use waddle_xmpp::muc::durable::AdminMutationId;
use waddle_xmpp::muc::room_actor::OccupancyWatermark;
use waddle_xmpp_core::xep0359::StanzaId;

pub(super) fn room_submission(fixture: &IngressFixture) -> IngressSubmission {
    let mut submission = fixture.submission(Some("frozen-room-manifest"), "accepted room body");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let peer: jid::FullJid = "alice@example.com/phone".parse().expect("peer");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let stamp = StanzaId::new("room-authoritative-id", room.clone().into());
    let mut canonical = submission.plan.sanitized_message.clone();
    canonical.from = Some(room.with_resource_str("romeo").expect("nick").into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut canonical, &stamp);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut canonical,
        &waddle_xmpp::xep::xep0421::OccupantId("sender-occupancy".into()),
    );
    submission.plan.room_canonical_message = Some(Box::new(canonical));
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![submission.sender.clone(), peer.clone()],
            reflection: submission.sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stamp),
        });
    submission.plan.room_execution = RoomExecutionPath::Local {
        room,
        fence: RoomFenceRequirement::Unfenced,
        snapshot_generation: 1,
        manifest_snapshot: Some(RoomManifestSnapshot {
            host_no_transport: Vec::new(),
            coordinates: None,
            occupancy_incarnation: AdminMutationId::generate(),
            occupants: vec![
                (
                    submission.sender.clone(),
                    OccupancyWatermark::from_revision(1),
                ),
                (peer, OccupancyWatermark::from_revision(2)),
            ],
        }),
    };
    submission
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_clustered_acceptance_keeps_archive_free_room_delivery_legacy() {
    let Some(mut fixture) = IngressFixture::postgres("manifest_cluster_dark").await else {
        return;
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let fence = fixture.room_fence(&room).await;
    let mut submission = room_submission(&fixture);
    let RoomExecutionPath::Local {
        fence: planned_fence,
        ..
    } = &mut submission.plan.room_execution
    else {
        panic!("local room plan");
    };
    *planned_fence = RoomFenceRequirement::Guarded(fence);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("clustered room retains legacy acceptance");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let key = decision.message_key.expect("key");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    assert!(
        envelope.room_manifest().is_none(),
        "clustered sealing is dark"
    );
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    assert!(
        !intents
            .iter()
            .any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. })),
        "archive-free reflection retains its sender-stream lifetime"
    );
    assert!(decision
        .route_progress
        .iter()
        .any(|progress| progress.current_attempt_reflection.as_ref() == Some(&submission.sender)));
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_clustered_dark_manifest_remains_authoritative_without_test_opt_in() {
    use waddle_xmpp::muc::durable::{RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};
    let Some(mut fixture) = IngressFixture::postgres("manifest_cluster_replay").await else {
        return;
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let fence = fixture.room_fence(&room).await;
    let mut submission = room_submission(&fixture);
    let coordinates = RoomCommittedCoordinates {
        lifecycle: RoomLifecycleId::generate(),
        revision: RoomRevision::initial(),
    };
    let RoomExecutionPath::Local {
        fence: planned_fence,
        manifest_snapshot: Some(snapshot),
        ..
    } = &mut submission.plan.room_execution
    else {
        panic!("room snapshot");
    };
    *planned_fence = RoomFenceRequirement::Guarded(fence);
    snapshot.coordinates = Some(coordinates);
    fixture.execute("CREATE TABLE IF NOT EXISTS clustering_muc_rooms (room_jid TEXT PRIMARY KEY, lifecycle_id TEXT, revision BIGINT)", ()).await;
    fixture
        .execute(
            "INSERT INTO clustering_muc_rooms (room_jid, lifecycle_id, revision) VALUES (?, ?, ?)",
            crate::db_params![room.to_string(), coordinates.lifecycle.to_string(), 1_i64],
        )
        .await;
    let accepted = super::commit::commit_hooks::ALLOW_CLUSTERED_ROOM_MANIFESTS
        .scope(true, commit_submission(&fixture.uow, &submission, 1))
        .await
        .expect("explicit dark acceptance");
    let key = accepted.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("inspect acceptance");
    let original = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    assert!(
        original.room_manifest().is_some(),
        "dark test actually sealed"
    );
    tx.commit().await.expect("inspect commit");

    let retried = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("persisted manifest replay survives disabled new seals");
    assert_eq!(retried.message_key, accepted.message_key);
    let mut tx = fixture.uow.begin().await.expect("inspect retry");
    let saved = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    assert_eq!(saved.room_manifest(), original.room_manifest());
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_clustered_legacy_remote_completion_releases_local_successor() {
    use crate::ingress_uow::{
        ArchiveDispatchObligation, ArchiveDispatchRepository, DispatchTarget,
    };
    use crate::server::routes::interpret::effects::delivery::{
        ExternalDeliveryEffect, PeerDeliveryKind,
    };
    use crate::server::routes::interpret::effects::{
        Effect, ExternalEffect, ImmediateSink, PlannedEffect,
    };
    use crate::server::routes::interpret::{
        Deps, FullJidDeliveryOutcome, CONTROLLED_REGISTERED_REMOTE_DELIVERY,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use waddle_xmpp::Stanza;
    let Some(mut fixture) = IngressFixture::postgres("manifest_cluster_successor").await else {
        return;
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let target: jid::FullJid = "alice@example.com/phone".parse().expect("target");
    let fence = fixture.room_fence(&room).await;
    let connections = waddle_xmpp::registry::ConnectionRegistry::new();
    let deps = Deps::new(&connections, "example.com");
    let wire = Arc::new(Mutex::new(Vec::new()));
    let mut original = None;
    let mut local_rx = None;
    for ordinal in [1_i64, 2] {
        let mut submission = room_submission(&fixture);
        submission
            .plan
            .sanitized_message
            .payloads
            .retain(|payload| !waddle_xmpp_core::xep0359::is_origin_id_element(payload));
        waddle_xmpp_core::xep0359::add_origin_id(
            &mut submission.plan.sanitized_message,
            if ordinal == 1 {
                "cluster-first"
            } else {
                "cluster-second"
            },
        );
        submission.digest_input = DigestInput::from_parsed(
            &submission.plan.sanitized_message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![room.clone()],
                stanza_lang: None,
            },
        )
        .expect("digest");
        let RoomExecutionPath::Local {
            fence: planned_fence,
            ..
        } = &mut submission.plan.room_execution
        else {
            panic!("local room");
        };
        *planned_fence = RoomFenceRequirement::Guarded(fence.clone());
        let IngressEffectIntent::RouteMucGroupchat { route_identity, .. } =
            &submission.plan.intents[0]
        else {
            panic!("room route");
        };
        let identity = route_identity.clone();
        let mut message = submission
            .plan
            .room_canonical_message
            .as_ref()
            .expect("canonical")
            .as_ref()
            .clone();
        message.to = Some(target.clone().into());
        submission.plan.plan =
            vec![
                PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RouteToPeer {
                        route_identity: Some(identity),
                        jid: target.clone(),
                        stanza: Box::new(Stanza::Message(message)),
                        kind: PeerDeliveryKind::PeerStanza,
                        call_setup: None,
                    },
                )))
                // Match PlanSink's policy for a non-sender occupant. The raw
                // PlannedEffect constructor defaults to Always, which deliberately
                // permits repeats and does not model production room planning.
                .with_suppression(crate::ingress::PlanSuppressionPolicy::SenderOnly),
            ];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("legacy accept");
        let mut tx = fixture.uow.begin().await.expect("archive order");
        let key = decision.message_key.expect("key");
        ArchiveDispatchRepository::record(
            &mut tx,
            key,
            &room,
            waddle_xmpp::mam::ArchiveOrdinal::from_storage(ordinal).expect("ordinal"),
            &[ArchiveDispatchObligation {
                receipt: decision.external_receipts[0][0].clone(),
                target: DispatchTarget::Resource(target.clone()),
            }],
        )
        .await
        .expect("archive position");
        tx.commit().await.expect("archive commit");
        let report = CONTROLLED_REGISTERED_REMOTE_DELIVERY
            .scope(
                (FullJidDeliveryOutcome::Delivered, wire.clone()),
                super::execute::execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &decision,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(2),
                ),
            )
            .await;
        assert_eq!(report.outcomes[0].1, super::ExternalOutcome::Done);
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            ordinal
        );
        if ordinal == 1 {
            original = Some(submission);
            let (local_tx, receiver) = tokio::sync::mpsc::channel(4);
            connections.register(target.clone(), local_tx);
            local_rx = Some(receiver);
        }
    }
    assert!(
        local_rx
            .as_mut()
            .expect("local receiver")
            .try_recv()
            .is_ok(),
        "the completed remote predecessor cannot block the local successor"
    );
    assert_eq!(
        wire.lock().expect("wire").len(),
        1,
        "successor used the local sink"
    );
    let retry = commit_submission(&fixture.uow, &original.expect("original"), 1)
        .await
        .expect("same-origin retry");
    let _ = CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (FullJidDeliveryOutcome::Delivered, wire.clone()),
            super::execute::execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(2),
            ),
        )
        .await;
    assert_eq!(
        wire.lock().expect("wire").len(),
        1,
        "successful remote copy does not repeat"
    );
    assert!(
        local_rx
            .as_mut()
            .expect("local receiver")
            .try_recv()
            .is_err(),
        "completed original cannot resend after moving local"
    );
    fixture.close().await;
}

#[tokio::test]
async fn pre_h_acceptance_seals_reflection_and_all_original_children() {
    let fixture = IngressFixture::sqlite().await;
    let submission = room_submission(&fixture);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("accept");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let key = decision.message_key.expect("key");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    let manifest = envelope.room_manifest().expect("complete pre-h manifest");
    assert_eq!(manifest.message_key(), key);
    assert_eq!(manifest.stanza_id.by, manifest.room);
    assert_eq!(
        manifest
            .children
            .iter()
            .filter(|child| child.target.as_ref() == Some(&submission.sender))
            .count(),
        1,
        "reflection is one ordinary child"
    );
    assert_eq!(
        manifest.recorded_intents(),
        EffectIntentRepository::load(&mut tx, key)
            .await
            .expect("frozen intents")
    );
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[tokio::test]
async fn crash_after_manifest_commit_cannot_add_retry_targets_or_replace_keys() {
    let fixture = IngressFixture::sqlite().await;
    let submission = room_submission(&fixture);
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("accept");
    let key = first.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let original = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    tx.commit().await.expect("inspect commit");
    let mut retry = submission.clone();
    retry.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: "newcomer@example.com".parse().expect("new account"),
        fanout: vec!["newcomer@example.com/later".parse().expect("new resource")],
        prepared: None,
        route_identity: EffectMessageIdentity::capture_ordinal(99),
    });
    let retried = commit_submission(&fixture.uow, &retry, 1)
        .await
        .expect("retry after crash");
    assert_eq!(retried.message_key, first.message_key);
    let mut tx = fixture.uow.begin().await.expect("inspect retry");
    let saved = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    assert_eq!(saved.room_manifest(), original.room_manifest());
    let recorded = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded");
    assert!(!recorded.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, .. } if recipient.to_string() == "newcomer@example.com")), "no discovery after committed h");
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[tokio::test]
async fn crash_mid_manifest_rolls_back_canonical_authority_and_every_child() {
    let fixture = IngressFixture::sqlite().await;
    let mut submission = room_submission(&fixture);
    let stream = waddle_xmpp::pending_delivery::SmSessionId::new("manifest-stream");
    let mut tx = fixture.uow.begin().await.expect("mint stream");
    let id = crate::ingress_uow::SmIngressStreamRepository::mint(&mut tx, &stream)
        .await
        .expect("stream");
    tx.commit().await.expect("mint commit");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id: stream,
        sm_ingress_id: id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::local(),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: waddle_xmpp::ingress::WireHandledCount::from_storage(1),
        checkpoint_h: waddle_xmpp::ingress::WireHandledCount::from_storage(1),
    };
    let result = super::commit::commit_hooks::FAIL_AFTER_MANIFEST
        .scope(true, commit_submission(&fixture.uow, &submission, 1))
        .await;
    assert!(result.is_err(), "crash checkpoint was reached");
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert_eq!(
        fixture.count("ingress_sm_refs").await,
        0,
        "h acquired no committed responsibility"
    );
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("fresh acceptance after rollback");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let envelope =
        CanonicalMessageRepository::load_envelope(&mut tx, decision.message_key.expect("key"))
            .await
            .expect("envelope")
            .expect("recorded");
    assert!(envelope.room_manifest().is_some());
    assert_eq!(
        crate::ingress_uow::SmIngressStreamRepository::load_stream_checkpoint(&mut tx, id)
            .await
            .expect("h"),
        Some(waddle_xmpp::ingress::WireHandledCount::from_storage(1))
    );
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[tokio::test]
async fn admission_rejects_stale_durable_revision_but_replay_preserves_accepted_children() {
    use waddle_xmpp::muc::durable::{RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};
    let fixture = IngressFixture::sqlite().await;
    let mut submission = room_submission(&fixture);
    let coordinates = RoomCommittedCoordinates {
        lifecycle: RoomLifecycleId::generate(),
        revision: RoomRevision::initial(),
    };
    let RoomExecutionPath::Local {
        manifest_snapshot: Some(snapshot),
        ..
    } = &mut submission.plan.room_execution
    else {
        panic!("snapshot");
    };
    snapshot.coordinates = Some(coordinates);
    fixture.execute("CREATE TABLE IF NOT EXISTS clustering_muc_rooms (room_jid TEXT PRIMARY KEY, lifecycle_id TEXT, revision BIGINT)", ()).await;
    fixture
        .execute(
            "INSERT INTO clustering_muc_rooms (room_jid, lifecycle_id, revision) VALUES (?, ?, ?)",
            crate::db_params![
                "room@muc.example.com",
                coordinates.lifecycle.to_string(),
                2_i64
            ],
        )
        .await;
    assert!(
        commit_submission(&fixture.uow, &submission, 1)
            .await
            .is_err(),
        "stale snapshot must not accept a partial manifest"
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    fixture
        .execute("UPDATE clustering_muc_rooms SET revision = 1", ())
        .await;
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("current revision");
    fixture
        .execute("UPDATE clustering_muc_rooms SET revision = 2", ())
        .await;
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("accepted children survive later revisions");
    assert_eq!(replay.message_key, first.message_key);
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let envelope =
        CanonicalMessageRepository::load_envelope(&mut tx, first.message_key.expect("key"))
            .await
            .expect("envelope")
            .expect("recorded");
    assert_eq!(
        envelope.room_manifest().expect("manifest").coordinates,
        Some(coordinates)
    );
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[tokio::test]
async fn legacy_acceptance_cannot_acquire_a_new_occupancy_manifest_on_retry() {
    let fixture = IngressFixture::sqlite().await;
    let mut submission = room_submission(&fixture);
    commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("seed old acceptance");
    let stored = fixture
        .optional_text("SELECT CAST(envelope AS TEXT) FROM ingress_messages LIMIT 1")
        .await
        .expect("stored envelope");
    let mut legacy: serde_json::Value = serde_json::from_str(&stored).expect("storage adapter");
    legacy
        .as_object_mut()
        .expect("envelope")
        .remove("room_manifest");
    fixture
        .execute(
            "UPDATE ingress_messages SET envelope = ?, envelope_version = 1",
            crate::db_params![serde_json::to_vec(&legacy).expect("old codec")],
        )
        .await;
    let RoomExecutionPath::Local {
        manifest_snapshot: Some(snapshot),
        ..
    } = &mut submission.plan.room_execution
    else {
        panic!("snapshot");
    };
    snapshot.occupancy_incarnation = AdminMutationId::generate();
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("legacy retry");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let envelope =
        CanonicalMessageRepository::load_envelope(&mut tx, decision.message_key.expect("key"))
            .await
            .expect("envelope")
            .expect("recorded");
    assert!(
        envelope.room_manifest().is_none(),
        "today's occupancy cannot fabricate historical proof"
    );
    tx.commit().await.expect("inspect commit");
    fixture.close().await;
}

#[tokio::test]
async fn a_sealed_child_key_cannot_authorize_replacing_the_canonical_room_payload() {
    let fixture = IngressFixture::sqlite().await;
    let submission = room_submission(&fixture);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("accept");
    let key = decision.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let original = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("recorded");
    let mut replacement = original.message().clone();
    replacement
        .bodies
        .insert(Default::default(), "different accepted content".into());
    assert!(CanonicalMessageRepository::record_room_canonical_envelope(
        &mut tx,
        key,
        &crate::ingress_substrate::MessageEnvelope::new(replacement)
    )
    .await
    .is_err());
    tx.commit().await.expect("inspection commit");
    let mut tx = fixture.uow.begin().await.expect("inspect unchanged");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("envelope"),
        Some(original)
    );
    tx.commit().await.expect("inspection commit");
    fixture.close().await;
}
