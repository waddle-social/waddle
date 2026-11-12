//! A node without the sealing actor proves a sealed room child against the
//! seat the child was sealed with.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};
use waddle_xmpp::muc::durable::{
    RoomClaimFenceContext, RoomCommittedCoordinates, RoomLifecycleId, RoomRevision,
};
use waddle_xmpp::muc::room_actor::OccupancyWatermark;
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp_core::OccupancySessionGeneration;

use super::{enqueue, settle_superseded};
use crate::ingress::commit::commit_hooks::ALLOW_CLUSTERED_ROOM_MANIFESTS;
use crate::ingress::commit::commit_submission;
use crate::ingress::test_support::IngressFixture;
use crate::ingress::xep0045_room_manifest_tests::room_submission;
use crate::ingress::{EffectReceiptKey, FrozenRoomManifest};
use crate::ingress_uow::{
    CanonicalMessageRepository, DeliveryProgressRepository, RoomOccupantRecord,
    RoomOccupantRepository,
};
use crate::server::routes::interpret::effects::room::RoomFenceRequirement;
use crate::server::routes::interpret::effects::RoomExecutionPath;
use crate::server::routes::interpret::SmIngressAppendContext;

/// A room message sealed under the room's claim on a clustered database.
struct Sealed {
    room: BareJid,
    fence: RoomClaimFenceContext,
    key: MessageKey,
    receipt: EffectReceiptKey,
    manifest: FrozenRoomManifest,
}

/// Seal a room message for the sender and `occupants`. Each occupant's seat
/// is sealed with the generation given for it, or with none, as a manifest
/// stored before seats were recorded is read.
async fn seal(
    fixture: &mut IngressFixture,
    occupants: &[(FullJid, Option<OccupancySessionGeneration>)],
) -> Sealed {
    let room: BareJid = "room@muc.example.com".parse().expect("room");
    let fence = fixture.room_fence(&room).await;
    let mut submission = room_submission(fixture);
    let sender = submission.sender.clone();
    let mut route = None;
    for intent in &mut submission.plan.intents {
        if let IngressEffectIntent::RouteMucGroupchat {
            occupants: recorded,
            ..
        } = intent
        {
            *recorded = std::iter::once(sender.clone())
                .chain(occupants.iter().map(|(jid, _)| jid.clone()))
                .collect();
            route = Some(intent.clone());
        }
    }
    let route = route.expect("room route");
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
    *planned_fence = RoomFenceRequirement::Guarded(fence.clone());
    snapshot.coordinates = Some(coordinates);
    snapshot.occupants = std::iter::once(sender)
        .chain(occupants.iter().map(|(jid, _)| jid.clone()))
        .enumerate()
        .map(|(index, jid)| (jid, OccupancyWatermark::from_revision(index as u64 + 1)))
        .collect();
    snapshot.seats = occupants
        .iter()
        .filter_map(|(jid, seat)| Some((jid.clone(), (*seat)?)))
        .collect();
    fixture.execute("CREATE TABLE IF NOT EXISTS clustering_muc_rooms (room_jid TEXT PRIMARY KEY, lifecycle_id TEXT, revision BIGINT)", ()).await;
    fixture
        .execute(
            "INSERT INTO clustering_muc_rooms (room_jid, lifecycle_id, revision) VALUES (?, ?, ?)",
            crate::db_params![room.to_string(), coordinates.lifecycle.to_string(), 1_i64],
        )
        .await;
    let decision = ALLOW_CLUSTERED_ROOM_MANIFESTS
        .scope(true, commit_submission(&fixture.uow, &submission, 1))
        .await
        .expect("sealed acceptance");
    let key = decision.message_key.expect("message key");
    let mut tx = fixture.uow.begin().await.expect("read the seal");
    let manifest = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("canonical row")
        .room_manifest()
        .expect("sealed manifest")
        .clone();
    tx.commit().await.expect("read commit");
    for (jid, seat) in occupants {
        let receipt = crate::ingress::receipt_key(&route).expect("route receipt");
        assert_eq!(
            manifest
                .child(&receipt, Some(jid))
                .expect("occupant child")
                .seat,
            *seat,
            "the stored child carries the seat it was sealed with"
        );
    }
    Sealed {
        room,
        fence,
        key,
        receipt: crate::ingress::receipt_key(&route).expect("route receipt"),
        manifest,
    }
}

/// Record `occupant`'s seat in the room's durable occupancy for the
/// connection `generation`, as the owner's actor does when it grants a seat.
async fn seat(
    fixture: &IngressFixture,
    sealed: &Sealed,
    occupant: &FullJid,
    generation: OccupancySessionGeneration,
) {
    let mut tx = fixture.uow.begin().await.expect("occupancy write");
    RoomOccupantRepository::record_seat(
        &mut tx,
        &RoomOccupantRecord {
            room: sealed.room.clone(),
            occupant: occupant.clone(),
            nick: occupant.node().expect("occupant node").to_string(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
            generation,
            join_request: None,
            presence: None,
            room_epoch: sealed.fence.epoch,
        },
    )
    .await
    .expect("record seat");
    tx.commit().await.expect("occupancy commit");
}

/// Offer `target`'s copy to the connection `endpoint` on a node that does not
/// run the sealing actor. Returns the outcome and whether the socket was
/// written.
async fn offer(
    fixture: &IngressFixture,
    sealed: &Sealed,
    target: &FullJid,
    endpoint: OccupancySessionGeneration,
) -> (BroadcastOutcome, bool) {
    let written = Arc::new(AtomicUsize::new(0));
    let socket = Arc::clone(&written);
    let outcome = enqueue(
        &sealed.manifest,
        &fixture.uow,
        None,
        &sealed.receipt,
        target,
        Some(endpoint),
        move || {
            socket.fetch_add(1, Ordering::SeqCst);
            BroadcastOutcome::Delivered
        },
    )
    .await
    .expect("durable occupancy proof");
    (outcome, written.load(Ordering::SeqCst) == 1)
}

/// The resources whose copy of the sealed route is recorded as settled.
async fn settled(fixture: &IngressFixture, sealed: &Sealed) -> Vec<FullJid> {
    let mut tx = fixture.uow.begin().await.expect("read progress");
    let settled = DeliveryProgressRepository::load(&mut tx, sealed.key, &sealed.receipt)
        .await
        .expect("delivery progress");
    tx.commit().await.expect("read commit");
    settled
}

#[tokio::test]
async fn xep0045_durable_proof_writes_only_to_the_seat_the_child_was_sealed_with() {
    let Some(mut fixture) = IngressFixture::postgres("manifest_sealed_seat_enqueue").await else {
        return;
    };
    let seated: FullJid = "alice@example.com/phone".parse().expect("seated");
    let rejoined: FullJid = "bob@example.com/phone".parse().expect("rejoined");
    let unrecorded: FullJid = "carol@example.com/phone".parse().expect("unrecorded");
    let sealed_generation = OccupancySessionGeneration::mint();
    let left_generation = OccupancySessionGeneration::mint();
    let sealed = seal(
        &mut fixture,
        &[
            (seated.clone(), Some(sealed_generation)),
            (rejoined.clone(), Some(left_generation)),
            (unrecorded.clone(), None),
        ],
    )
    .await;

    // The frozen occupant is still seated: its own connection receives the
    // copy, another connection bound to the same full JID does not.
    seat(&fixture, &sealed, &seated, sealed_generation).await;
    assert_eq!(
        offer(&fixture, &sealed, &seated, sealed_generation).await,
        (BroadcastOutcome::Delivered, true)
    );
    assert_eq!(
        offer(
            &fixture,
            &sealed,
            &seated,
            OccupancySessionGeneration::mint()
        )
        .await,
        (BroadcastOutcome::NotConnected, false),
        "a connection that does not hold the seat receives nothing"
    );

    // The frozen occupant left and the full JID joined again from another
    // connection, which holds the seat now. The copy was not frozen for it.
    let replacement = OccupancySessionGeneration::mint();
    seat(&fixture, &sealed, &rejoined, replacement).await;
    assert_eq!(
        offer(&fixture, &sealed, &rejoined, replacement).await,
        (BroadcastOutcome::NotConnected, false),
        "the occupancy that replaced the frozen one receives nothing"
    );
    assert_eq!(
        offer(&fixture, &sealed, &rejoined, left_generation).await,
        (BroadcastOutcome::NotConnected, false),
        "the connection that left holds no seat"
    );

    // A child that records no seat names no occupant the row could be
    // compared with, so the connection that holds the seat is not proven
    // to be the frozen occupant.
    let current = OccupancySessionGeneration::mint();
    seat(&fixture, &sealed, &unrecorded, current).await;
    assert_eq!(
        offer(&fixture, &sealed, &unrecorded, current).await,
        (BroadcastOutcome::NotConnected, false),
        "a child without a recorded seat is not delivered from durable rows"
    );
    fixture.close().await;
}

#[tokio::test]
async fn xep0045_child_whose_sealed_seat_is_gone_is_departed_once_the_claim_has_left() {
    let Some(mut fixture) = IngressFixture::postgres("manifest_sealed_seat_retire").await else {
        return;
    };
    let seated: FullJid = "alice@example.com/phone".parse().expect("seated");
    let rejoined: FullJid = "bob@example.com/phone".parse().expect("rejoined");
    let unrecorded: FullJid = "carol@example.com/phone".parse().expect("unrecorded");
    let absent: FullJid = "dave@example.com/phone".parse().expect("absent");
    let sealed_generation = OccupancySessionGeneration::mint();
    let sealed = seal(
        &mut fixture,
        &[
            (seated.clone(), Some(sealed_generation)),
            (rejoined.clone(), Some(OccupancySessionGeneration::mint())),
            (unrecorded.clone(), None),
            (absent.clone(), Some(OccupancySessionGeneration::mint())),
        ],
    )
    .await;
    seat(&fixture, &sealed, &seated, sealed_generation).await;
    seat(
        &fixture,
        &sealed,
        &rejoined,
        OccupancySessionGeneration::mint(),
    )
    .await;
    seat(
        &fixture,
        &sealed,
        &unrecorded,
        OccupancySessionGeneration::mint(),
    )
    .await;
    let context = SmIngressAppendContext {
        message_key: sealed.key,
        receipt: sealed.receipt.clone(),
        received_at: None,
        archive_positions: Vec::new(),
        dispatch_stream: None,
        authority: crate::ingress::append_authority::AppendAuthority::Verified,
    };

    // While the claim the manifest was sealed under stands, the owner's
    // actor decides: nothing is settled from durable rows.
    for target in [&seated, &rejoined, &unrecorded, &absent] {
        assert!(
            settle_superseded(&fixture.uow, &sealed.manifest, None, &context, target)
                .await
                .expect("retirement check under the sealing claim")
                .is_none(),
            "{target} stays owed while the sealing claim stands"
        );
    }
    assert!(settled(&fixture, &sealed).await.is_empty());

    // The claim leaves the incarnation that sealed the manifest.
    fixture
        .execute(
            "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = ?",
            crate::db_params![format!("room_actor:{}", sealed.room)],
        )
        .await;
    assert!(
        settle_superseded(&fixture.uow, &sealed.manifest, None, &context, &seated)
            .await
            .expect("retirement check")
            .is_none(),
        "the frozen occupant still holds its seat and stays owed"
    );
    for (target, why) in [
        (&rejoined, "its seat is held by another connection"),
        (&unrecorded, "its child records no seat"),
        (&absent, "it has no seat"),
    ] {
        assert!(
            settle_superseded(&fixture.uow, &sealed.manifest, None, &context, target)
                .await
                .expect("retirement check")
                .is_some(),
            "{target} is settled as departed: {why}"
        );
    }
    let mut departed = settled(&fixture, &sealed).await;
    departed.sort_by_key(ToString::to_string);
    assert_eq!(departed, vec![rejoined, unrecorded, absent]);
    fixture.close().await;
}
