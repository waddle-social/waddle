//! A node without the sealing actor proves a sealed room child against the
//! seat the child was sealed for: the connection generation and the identity
//! of the occupancy.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};
use waddle_xmpp::muc::durable::{
    RoomClaimFenceContext, RoomCommittedCoordinates, RoomLifecycleId, RoomRevision,
};
use waddle_xmpp::muc::room_actor::OccupancyWatermark;
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp_core::{OccupancySeatId, OccupancySessionGeneration};

use super::{enqueue, queue_detached, settle_superseded};
use crate::ingress::commit::commit_hooks::ALLOW_CLUSTERED_ROOM_MANIFESTS;
use crate::ingress::commit::commit_submission;
use crate::ingress::test_support::IngressFixture;
use crate::ingress::xep0045_room_manifest_tests::room_submission;
use crate::ingress::{EffectReceiptKey, FrozenRoomManifest, SealedSeat};
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

/// A seat with an occupancy identity, as the room actor grants it.
fn identified(generation: OccupancySessionGeneration) -> SealedSeat {
    SealedSeat {
        generation,
        identity: Some(OccupancySeatId::mint()),
    }
}

/// A seat without an occupancy identity: what a version 2 manifest child
/// reads as, and what a row written before rows carried identities holds.
fn unidentified(generation: OccupancySessionGeneration) -> SealedSeat {
    SealedSeat {
        generation,
        identity: None,
    }
}

/// Another occupancy of the same connection: the seat it holds after it
/// left and joined again.
fn rejoined_on_the_same_connection(seat: SealedSeat) -> SealedSeat {
    identified(seat.generation)
}

/// Seal a room message for the sender and `occupants`. Each occupant's child
/// is sealed for the seat given for it, or for none, as a manifest stored
/// before seats were recorded is read.
async fn seal(fixture: &mut IngressFixture, occupants: &[(FullJid, Option<SealedSeat>)]) -> Sealed {
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

/// Record `occupant`'s seat in the room's durable occupancy, as the owner's
/// actor does when it grants a seat.
async fn seat(fixture: &IngressFixture, sealed: &Sealed, occupant: &FullJid, held: SealedSeat) {
    let mut tx = fixture.uow.begin().await.expect("occupancy write");
    RoomOccupantRepository::record_seat(
        &mut tx,
        &RoomOccupantRecord {
            room: sealed.room.clone(),
            occupant: occupant.clone(),
            nick: occupant.node().expect("occupant node").to_string(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
            generation: held.generation,
            seat: held.identity,
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

/// The append context of the sealed route, as recovery presents it.
fn append_context(sealed: &Sealed) -> SmIngressAppendContext {
    SmIngressAppendContext {
        message_key: sealed.key,
        receipt: sealed.receipt.clone(),
        received_at: None,
        archive_positions: Vec::new(),
        dispatch_stream: None,
        authority: crate::ingress::append_authority::AppendAuthority::Verified,
    }
}

/// Move the room's claim away from the incarnation that sealed the manifest.
async fn move_claim(fixture: &IngressFixture, sealed: &Sealed) {
    fixture
        .execute(
            "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = ?",
            crate::db_params![format!("room_actor:{}", sealed.room)],
        )
        .await;
}

/// Whether `target`'s child is settled as departed from durable rows, on a
/// node that does not run the sealing actor.
async fn settles_departed(fixture: &IngressFixture, sealed: &Sealed, target: &FullJid) -> bool {
    settle_superseded(
        &fixture.uow,
        &sealed.manifest,
        None,
        &append_context(sealed),
        target,
    )
    .await
    .expect("retirement check")
    .is_some()
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
    let sealed_seat = identified(OccupancySessionGeneration::mint());
    let sealed_generation = sealed_seat.generation;
    let left_generation = OccupancySessionGeneration::mint();
    let sealed = seal(
        &mut fixture,
        &[
            (seated.clone(), Some(sealed_seat)),
            (rejoined.clone(), Some(identified(left_generation))),
            (unrecorded.clone(), None),
        ],
    )
    .await;

    // The frozen occupant is still seated: its own connection receives the
    // copy, another connection bound to the same full JID does not.
    seat(&fixture, &sealed, &seated, sealed_seat).await;
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
    seat(&fixture, &sealed, &rejoined, identified(replacement)).await;
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
    seat(&fixture, &sealed, &unrecorded, identified(current)).await;
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
    let sealed_seat = identified(OccupancySessionGeneration::mint());
    let sealed = seal(
        &mut fixture,
        &[
            (seated.clone(), Some(sealed_seat)),
            (
                rejoined.clone(),
                Some(identified(OccupancySessionGeneration::mint())),
            ),
            (unrecorded.clone(), None),
            (
                absent.clone(),
                Some(identified(OccupancySessionGeneration::mint())),
            ),
        ],
    )
    .await;
    seat(&fixture, &sealed, &seated, sealed_seat).await;
    seat(
        &fixture,
        &sealed,
        &rejoined,
        identified(OccupancySessionGeneration::mint()),
    )
    .await;
    seat(
        &fixture,
        &sealed,
        &unrecorded,
        identified(OccupancySessionGeneration::mint()),
    )
    .await;
    let context = append_context(&sealed);

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
    move_claim(&fixture, &sealed).await;
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

/// A connection that leaves the room and joins it again holds both seats
/// under one generation. A copy sealed for the earlier occupancy is not
/// written to the later one, and is settled as departed once the sealing
/// claim has moved; a copy sealed for the occupancy that still holds the
/// seat is written once and stays owed until its delivery is recorded.
#[tokio::test]
async fn xep0045_durable_proof_does_not_write_an_earlier_occupancy_copy_to_a_same_connection_rejoin(
) {
    let Some(mut fixture) = IngressFixture::postgres("manifest_seat_identity_enqueue").await else {
        return;
    };
    let rejoined: FullJid = "alice@example.com/phone".parse().expect("rejoined");
    let current: FullJid = "bob@example.com/phone".parse().expect("current");
    let earlier = identified(OccupancySessionGeneration::mint());
    let held = identified(OccupancySessionGeneration::mint());
    let sealed = seal(
        &mut fixture,
        &[
            (rejoined.clone(), Some(earlier)),
            (current.clone(), Some(held)),
        ],
    )
    .await;
    // After sealing, the connection left and joined again; the sealing
    // actor has ended, so only the recorded seats remain to prove from.
    let later = rejoined_on_the_same_connection(earlier);
    seat(&fixture, &sealed, &rejoined, later).await;
    seat(&fixture, &sealed, &current, held).await;

    assert_eq!(
        offer(&fixture, &sealed, &rejoined, earlier.generation).await,
        (BroadcastOutcome::NotConnected, false),
        "the later occupancy of the same connection does not receive the earlier one's copy"
    );
    assert_eq!(
        offer(&fixture, &sealed, &current, held.generation).await,
        (BroadcastOutcome::Delivered, true),
        "the occupancy the copy was sealed for receives it"
    );

    move_claim(&fixture, &sealed).await;
    assert!(
        settles_departed(&fixture, &sealed, &rejoined).await,
        "the earlier occupancy has left"
    );
    assert!(
        !settles_departed(&fixture, &sealed, &current).await,
        "the sealed occupancy still holds its seat and is not retired"
    );
    assert_eq!(settled(&fixture, &sealed).await, vec![rejoined]);
    fixture.close().await;
}

/// A stream-management session detached from `target`'s connection
/// `generation`, which the occupancy authority holds as current.
async fn detach(
    fixture: &IngressFixture,
    sm: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    target: &FullJid,
    generation: OccupancySessionGeneration,
) {
    use waddle_xmpp::stream_management::SmSessionRegistry as _;
    crate::occupancy_authority::publish(&fixture.db, target, generation)
        .await
        .expect("current generation");
    sm.store_session(waddle_xmpp::stream_management::DetachedSession {
        promotion_custody: waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
        stream_id: target.to_string(),
        user_id: target.to_bare().to_string(),
        jid: target.clone(),
        occupancy_session: generation,
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

/// The stanzas queued on `target`'s detached session.
async fn queued(
    sm: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    target: &FullJid,
) -> usize {
    use waddle_xmpp::stream_management::SmSessionRegistry as _;
    sm.peek_session(&target.to_string())
        .await
        .expect("peek session")
        .expect("detached session")
        .unacked_stanzas
        .len()
}

/// The detached-append branch of the seat rule. Without the sealing actor a
/// copy is appended to the target's detached session only while the room's
/// durable occupancy seats the occupancy the child was sealed for: not when
/// another connection holds the seat, and not when the same connection left
/// and joined again. The append is keyed, so offering the copy again
/// queues nothing more.
#[tokio::test]
async fn xep0045_durable_proof_appends_to_a_detached_session_only_for_the_sealed_occupancy() {
    let Some(mut fixture) = IngressFixture::postgres("manifest_seat_identity_detached").await
    else {
        return;
    };
    let rejoined: FullJid = "alice@example.com/phone".parse().expect("rejoined");
    let current: FullJid = "bob@example.com/phone".parse().expect("current");
    let replaced: FullJid = "carol@example.com/phone".parse().expect("replaced");
    let earlier = identified(OccupancySessionGeneration::mint());
    let held = identified(OccupancySessionGeneration::mint());
    let displaced = identified(OccupancySessionGeneration::mint());
    let sealed = seal(
        &mut fixture,
        &[
            (rejoined.clone(), Some(earlier)),
            (current.clone(), Some(held)),
            (replaced.clone(), Some(displaced)),
        ],
    )
    .await;
    let replacement = identified(OccupancySessionGeneration::mint());
    seat(
        &fixture,
        &sealed,
        &rejoined,
        rejoined_on_the_same_connection(earlier),
    )
    .await;
    seat(&fixture, &sealed, &current, held).await;
    seat(&fixture, &sealed, &replaced, replacement).await;

    let sm = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new().with_persistence(
            Arc::new(
                crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
                    .await
                    .expect("SM persistence"),
            ),
        ),
    );
    detach(&fixture, &sm, &rejoined, earlier.generation).await;
    detach(&fixture, &sm, &current, held.generation).await;
    detach(&fixture, &sm, &replaced, replacement.generation).await;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = crate::server::routes::interpret::Deps::new(
        &state.deps.protocol.connection_registry,
        "example.com",
    );
    deps.sm_session_registry = Some(&sm);
    let context = append_context(&sealed);
    let copy = waddle_xmpp::Stanza::Message(xmpp_parsers::message::Message::new(None::<jid::Jid>));
    let append = |target: FullJid| {
        let (deps, context, copy) = (&deps, &context, &copy);
        let (manifest, uow) = (&sealed.manifest, &fixture.uow);
        async move {
            queue_detached(manifest, uow, deps, context, &target, copy)
                .await
                .expect("durable occupancy proof")
        }
    };
    use crate::server::routes::interpret::FullJidDeliveryOutcome;

    assert_eq!(
        append(rejoined.clone()).await,
        FullJidDeliveryOutcome::Unavailable,
        "the later occupancy of the same connection does not receive the earlier one's copy"
    );
    assert_eq!(queued(&sm, &rejoined).await, 0);
    assert_eq!(
        append(replaced.clone()).await,
        FullJidDeliveryOutcome::Unavailable,
        "a seat held by another connection does not receive the copy"
    );
    assert_eq!(queued(&sm, &replaced).await, 0);
    for attempt in ["first", "repeated"] {
        assert_eq!(
            append(current.clone()).await,
            FullJidDeliveryOutcome::QueuedDetached,
            "the {attempt} offer to the sealed occupancy is accepted"
        );
        assert_eq!(
            queued(&sm, &current).await,
            1,
            "the copy is queued once after the {attempt} offer"
        );
    }

    move_claim(&fixture, &sealed).await;
    assert!(settles_departed(&fixture, &sealed, &rejoined).await);
    assert!(settles_departed(&fixture, &sealed, &replaced).await);
    assert!(!settles_departed(&fixture, &sealed, &current).await);
    let mut departed = settled(&fixture, &sealed).await;
    departed.sort_by_key(ToString::to_string);
    assert_eq!(departed, vec![rejoined, replaced]);
    fixture.close().await;
}

/// A version 2 manifest child records the seat's generation and no
/// identity (`stored_child_version_tests` covers that reading). It stands
/// only at a row that has no identity either: a seat recorded before rows
/// carried identities and not joined again since. Any join admitted since
/// writes an identity, so the same connection's later occupancy does not
/// receive the copy.
#[tokio::test]
async fn xep0045_child_sealed_without_a_seat_identity_stands_only_at_a_row_without_one() {
    let Some(mut fixture) = IngressFixture::postgres("manifest_seat_identity_legacy").await else {
        return;
    };
    let unchanged: FullJid = "alice@example.com/phone".parse().expect("unchanged");
    let rejoined: FullJid = "bob@example.com/phone".parse().expect("rejoined");
    let earlier_seat = unidentified(OccupancySessionGeneration::mint());
    let left_seat = unidentified(OccupancySessionGeneration::mint());
    let sealed = seal(
        &mut fixture,
        &[
            (unchanged.clone(), Some(earlier_seat)),
            (rejoined.clone(), Some(left_seat)),
        ],
    )
    .await;
    seat(&fixture, &sealed, &unchanged, earlier_seat).await;
    seat(
        &fixture,
        &sealed,
        &rejoined,
        rejoined_on_the_same_connection(left_seat),
    )
    .await;

    assert_eq!(
        offer(&fixture, &sealed, &unchanged, earlier_seat.generation).await,
        (BroadcastOutcome::Delivered, true),
        "a seat that predates identities is still the frozen occupant's"
    );
    assert_eq!(
        offer(&fixture, &sealed, &rejoined, left_seat.generation).await,
        (BroadcastOutcome::NotConnected, false),
        "a seat joined since carries an identity the child does not name"
    );
    move_claim(&fixture, &sealed).await;
    assert!(!settles_departed(&fixture, &sealed, &unchanged).await);
    assert!(settles_departed(&fixture, &sealed, &rejoined).await);
    assert_eq!(settled(&fixture, &sealed).await, vec![rejoined]);
    fixture.close().await;
}
