//! XEP-0045 across S2S, inbound: the router's part of hosting rooms for
//! occupants at other servers (ADR-0020 D10), with a scripted room host.
//!
//! These cases hold the transport decisions the router takes whatever a
//! room decides: a request for a room whose holder does not answer is
//! answered without holding back any other room or remote domain; a join
//! replaces a seat a stopped replica left, and is refused where the room's
//! stanzas for it could not leave; a seat ends once when a stream of its
//! pair is lost, when the room no longer seats its occupant, when the host
//! refuses the occupant, when this replica takes it over from one that
//! stopped (status 333, §11.1) and when this replica stops (status 332,
//! §11.2); presence for an earlier occupancy neither ends nor reaches a
//! later seat; an occupant that enters again is answered before the room's
//! traffic reaches it (§7.1); the sender's own copy of its message reaches
//! it once (§7.4); one account cannot spend its domain's request rate; and
//! chat states are dropped before a seat ends.
//!
//! `example.com` runs the production router and connection manager with a
//! scripted host (`s2s_inbound_muc_support/scripted.rs`); `home-b.test` and
//! `home-c.test` are connection managers that keep what they receive. The
//! seat table is an in-memory SQLite database. No WebSocket state,
//! PostgreSQL, SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;
#[allow(dead_code)]
#[path = "s2s_inbound_muc_support/scripted.rs"]
mod scripted;

use std::time::Duration;

use minidom::Element;
use s2s_inbound_muc_support::routing::wait_for_async;
use s2s_inbound_muc_support::{
    body, error_condition, jid, presence_between, remote_get, remote_groupchat, remote_join,
    statuses, wait_for, ARRIVES, BOB, BOB_OCCUPANT, DOMAIN, HOME_B, MUC, QUIET, ROOM,
};
use scripted::{bare, full, own_unavailable, Scripted, ScriptedRooms, CAROL, GREETING};
use waddle_server::destination::RemoteRoute;
use waddle_server::permissions::host_grant::StreamEvidence;
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::route::{
    CarriedPrincipal, CarriedRoomOutcome, CarriedRoomRequest, RouterLimits, Seat, SeatEnd,
    SeatInsert, SeatLimits, SeatStore,
};
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const NS_CHATSTATES: &str = "http://jabber.org/protocol/chatstates";
const ALICE_OCCUPANT: &str = "lounge@muc.example.com/alice";
/// A room held by a replica that does not answer.
const SILENT_ROOM: &str = "attic@muc.example.com";
/// A second account of `home-b.test`.
const DAVE: &str = "dave@home-b.test/tablet";

/// A replica name no router records as alive.
const STOPPED: &str = "a-replica-that-stopped";

fn ping() -> Element {
    Element::builder("ping", "urn:xmpp:ping").build()
}

/// Bounds no case here reaches.
fn wide() -> SeatLimits {
    SeatLimits {
        per_room: 64,
        per_peer: 64,
        per_account: 64,
        total: 64,
    }
}

/// A seat row of `owner` for `occupant` in `room`, written at
/// `updated_at_ms`.
fn row(owner: &str, room: &str, occupant: &str, updated_at_ms: i64) -> Seat {
    Seat {
        room: bare(room),
        occupant: full(occupant),
        nick: "bob".to_owned(),
        session: OccupancySessionGeneration::mint(),
        evidence: StreamEvidence::for_test(
            AuthenticationStrength::Pkix,
            Some(TrustProfileKind::PrivateCa),
        ),
        snapshot: uuid::Uuid::nil(),
        owner: owner.to_owned(),
        lost_at_ms: None,
        updated_at_ms,
    }
}

/// `bob@home-b.test/phone` joined to [`ROOM`], and its seat.
async fn with_bob(hosted: &Scripted) -> Seat {
    hosted
        .home_b
        .send(remote_join(BOB, ROOM, "bob", "join-1"))
        .await;
    let seat = hosted.seated(ROOM, BOB).await;
    hosted
        .home_b
        .received(is_own_presence)
        .await
        .expect("the room's own presence for the occupant");
    seat
}

/// The room's own available presence for bob.
fn is_own_presence(stanza: &Element) -> bool {
    presence_between(stanza, BOB_OCCUPANT, BOB)
        && stanza.attr("type").is_none()
        && statuses(stanza).contains(&"110".to_owned())
}

/// The room's or the service's unavailable presence telling bob that his
/// occupancy ended.
fn is_own_departure(stanza: &Element) -> bool {
    presence_between(stanza, BOB_OCCUPANT, BOB) && stanza.attr("type") == Some("unavailable")
}

/// Whether the seat table holds a row for `occupant` in `room`.
async fn has_row(hosted: &Scripted, room: &str, occupant: &str) -> bool {
    hosted
        .store
        .get(&bare(room), &full(occupant))
        .await
        .expect("the seat table is read")
        .is_some()
}

/// Wait until the seat table holds no row for `occupant` in `room`.
async fn row_removed(hosted: &Scripted, room: &str, occupant: &str) -> bool {
    wait_for_async(ARRIVES, || async {
        (!has_row(hosted, room, occupant).await).then_some(())
    })
    .await
    .is_some()
}

/// Wait until the router has told the host of an end of `occupant`'s seat,
/// let more arrive, and return every end it told of.
async fn ends_of(hosted: &Scripted, occupant: &str) -> Vec<SeatEnd> {
    wait_for(ARRIVES, || {
        (!hosted.host.ends_of(occupant).is_empty()).then_some(())
    })
    .await
    .expect("the router ends the seat");
    tokio::time::sleep(QUIET).await;
    hosted.host.ends_of(occupant)
}

/// A room message for bob from another occupant that carries `payload`
/// and, when given, a text.
fn from_room(text: Option<&str>, payload: Option<Element>) -> Stanza {
    let mut message = Message::new_with_type(MessageType::Groupchat, Some(jid(BOB)));
    message.from = Some(jid(ALICE_OCCUPANT));
    if let Some(text) = text {
        message
            .bodies
            .insert(xmpp_parsers::message::Lang(String::new()), text.to_owned());
    }
    message.payloads.extend(payload);
    Stanza::Message(message)
}

#[tokio::test]
async fn a_join_replaces_the_seat_a_stopped_replica_left_whatever_the_bounds() {
    // The account may hold one seat, and the room one remote occupant: the
    // row the stopped replica left is the one the join takes over, not one
    // that counts against it.
    let limits = RouterLimits {
        seats_per_account: 1,
        seats_per_room: 1,
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-stale-row", limits).await;
    let left = row(STOPPED, ROOM, BOB, 1_000);
    assert_eq!(
        hosted.store.insert(&left, &wide()).await.expect("insert"),
        SeatInsert::Inserted
    );
    let seat = with_bob(&hosted).await;
    assert_ne!(seat.session, left.session);
    assert_ne!(seat.owner, STOPPED);
    let stored = hosted
        .store
        .get(&seat.room, &seat.occupant)
        .await
        .expect("read")
        .expect("the seat's row");
    assert_eq!(stored.owner, seat.owner);
    assert_eq!(stored.session, seat.session);
    assert_eq!(hosted.store.count().await.expect("count"), 1);
    assert_eq!(hosted.host.seats_under(ROOM, BOB), Some(seat.session));
}

#[tokio::test]
async fn a_room_whose_holder_does_not_answer_holds_back_no_other_room_or_domain() {
    let limits = RouterLimits {
        // One worker hands every room's requests to the host: a request
        // that waited on it for a holder would hold back all of them.
        room_request_workers: 1,
        room_placement_timeout: Duration::from_secs(4),
        room_placement_backoff: Duration::from_secs(60),
        ..RouterLimits::default()
    };
    let hosted = Scripted::start_with("inbound-router-placement", limits, |host| {
        host.carry();
        host.silence(SILENT_ROOM);
    })
    .await;
    let seat = with_bob(&hosted).await;
    // A join for a room whose holder never answers.
    hosted
        .home_b
        .send(remote_join(BOB, SILENT_ROOM, "bob", "join-silent"))
        .await;
    wait_for(ARRIVES, || (hosted.host.placed() >= 2).then_some(()))
        .await
        .expect("the holder of the silent room is asked");
    // While it is asked, another domain's join and the same domain's
    // request for another room are placed, decided and answered.
    hosted
        .home_c
        .send(remote_join(CAROL, ROOM, "carol", "join-c"))
        .await;
    hosted.seated(ROOM, CAROL).await;
    hosted
        .home_b
        .send(remote_get(BOB, ROOM, "ping-1", ping()))
        .await;
    let answered = hosted.home_b.answered("ping-1").await;
    assert_eq!(answered.attr("type"), Some("result"), "{answered:?}");
    assert!(
        hosted.home_b.captured.with_id("join-silent").is_empty(),
        "the silent room's holder was not given its time"
    );
    // The request is answered once the holder's time is up.
    let refused = hosted.home_b.answered("join-silent").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    // No seat was lost and none was opened; the stream still carries.
    assert_eq!(
        hosted.seat(ROOM, BOB).map(|held| held.session),
        Some(seat.session)
    );
    assert!(hosted.seat(ROOM, CAROL).is_some());
    assert!(hosted.seat(SILENT_ROOM, BOB).is_none());
    assert!(hosted.host.ends_of(BOB).is_empty());
    assert!(hosted.host.ends_of(CAROL).is_empty());
    // For a while the room's requests are answered without asking again.
    let asked = hosted.host.placed();
    hosted
        .home_b
        .send(remote_join(BOB, SILENT_ROOM, "bob", "join-silent-2"))
        .await;
    let refused = hosted.home_b.answered("join-silent-2").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    assert_eq!(hosted.host.placed(), asked);
    assert_eq!(hosted.router.requests_being_placed(), 0);
}

#[tokio::test]
async fn the_senders_own_copy_of_its_message_reaches_it_once() {
    let hosted = Scripted::start("inbound-router-reflection", RouterLimits::default()).await;
    with_bob(&hosted).await;
    // The scripted room hands the sender's copy to its seat where it fans
    // the message out and gives it again as its answer, as the room host
    // does (XEP-0045 §7.4).
    hosted
        .home_b
        .send(remote_groupchat(BOB, ROOM, "m-1", "hello"))
        .await;
    let is_reflection = |stanza: &Element| {
        stanza.name() == "message"
            && stanza.attr("from") == Some(BOB_OCCUPANT)
            && stanza.attr("to") == Some(BOB)
            && body(stanza).as_deref() == Some("hello")
    };
    hosted
        .home_b
        .received(is_reflection)
        .await
        .expect("the sender's own copy");
    tokio::time::sleep(QUIET).await;
    assert_eq!(hosted.home_b.count(is_reflection), 1);
}

#[tokio::test]
async fn a_burst_that_finds_no_place_ends_the_seat_as_undeliverable() {
    let limits = RouterLimits {
        seat_traffic_per_peer: 1,
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-burst", limits).await;
    with_bob(&hosted).await;
    let presence = || {
        let mut presence = Presence::new(PresenceType::None);
        presence.from = Some(jid(ALICE_OCCUPANT));
        presence.to = Some(jid(BOB));
        Stanza::Presence(presence)
    };
    // With one place for the remote domain a burst cannot be queued.
    for _ in 0..8 {
        let _ = hosted.router.offer_to_seat(&full(BOB), presence());
    }
    // The seat ends for that reason and no other, once.
    assert_eq!(ends_of(&hosted, BOB).await, vec![SeatEnd::Undeliverable]);
    assert!(hosted.unseated(ROOM, BOB).await);
    assert!(row_removed(&hosted, ROOM, BOB).await);
}

#[tokio::test]
async fn seats_are_checked_against_their_rooms_with_one_ask_for_each_room() {
    let limits = RouterLimits {
        sweep_interval: Duration::from_millis(50),
        seat_reconcile_interval: Duration::from_millis(200),
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-reconcile", limits).await;
    with_bob(&hosted).await;
    hosted
        .home_c
        .send(remote_join(CAROL, ROOM, "carol", "join-c"))
        .await;
    hosted.seated(ROOM, CAROL).await;
    // The room removed bob and its presence did not pass the router.
    hosted.host.forget(ROOM, BOB);
    assert!(hosted.unseated(ROOM, BOB).await);
    assert!(row_removed(&hosted, ROOM, BOB).await);
    // The occupant is told that the service removed it (§11.1).
    let told = hosted
        .home_b
        .received(is_own_departure)
        .await
        .expect("the removed occupant's own presence");
    assert!(statuses(&told).contains(&"333".to_owned()), "{told:?}");
    // The occupant the room still seats keeps her seat.
    assert!(hosted.seat(ROOM, CAROL).is_some());
    assert!(has_row(&hosted, ROOM, CAROL).await);
    // The room was asked for its occupants; no seat was asked about.
    assert!(hosted.host.room_asks() >= 1);
    assert_eq!(hosted.host.seat_asks(), 0);
    // The room did not end the seat through the router: it had no
    // occupant to remove.
    assert!(hosted.host.ends_of(BOB).is_empty());
}

#[tokio::test]
async fn a_seat_whose_stream_another_replica_lost_is_ended_once() {
    let limits = RouterLimits {
        sweep_interval: Duration::from_millis(50),
        room_request_workers: 1,
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-lost", limits).await;
    with_bob(&hosted).await;
    // The room's worker waits in the host, so the end of the seat queues
    // behind it while the sweep reads the marked row again and again.
    hosted.host.hold().await;
    hosted
        .home_b
        .send(remote_get(BOB, ROOM, "ping-held", ping()))
        .await;
    wait_for(ARRIVES, || {
        (!hosted.host.handled_with("ping-held").is_empty()).then_some(())
    })
    .await
    .expect("the held request reached the host");
    let marked = hosted
        .store
        .mark_lost(MUC, HOME_B, "another-replica", 2_000)
        .await
        .expect("mark");
    assert_eq!(marked, 1);
    assert!(hosted.unseated(ROOM, BOB).await);
    tokio::time::sleep(Duration::from_millis(500)).await;
    hosted.host.release();
    assert_eq!(ends_of(&hosted, BOB).await, vec![SeatEnd::StreamLost]);
    assert!(row_removed(&hosted, ROOM, BOB).await);
}

#[tokio::test]
async fn the_seats_a_stopped_replica_left_are_removed() {
    let limits = RouterLimits {
        sweep_interval: Duration::from_millis(50),
        prune_after: Duration::from_millis(300),
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-orphans", limits).await;
    let seat = with_bob(&hosted).await;
    let left = row(STOPPED, ROOM, DAVE, 1_000);
    assert_eq!(
        hosted.store.insert(&left, &wide()).await.expect("insert"),
        SeatInsert::Inserted
    );
    assert!(row_removed(&hosted, ROOM, DAVE).await);
    // The seat of this replica, which records that it is alive, stays.
    assert!(has_row(&hosted, ROOM, BOB).await);
    assert_eq!(
        hosted.seat(ROOM, BOB).map(|held| held.session),
        Some(seat.session)
    );
}

#[tokio::test]
async fn a_row_of_this_replica_that_names_no_seat_it_holds_is_removed() {
    let limits = RouterLimits {
        sweep_interval: Duration::from_millis(50),
        seat_reconcile_interval: Duration::from_millis(200),
        seat_owner_grace: Duration::from_millis(100),
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-leaked", limits).await;
    let seat = with_bob(&hosted).await;
    // What a seat leaves when the removal of its row failed: a row of
    // this replica for an occupant it does not seat.
    let leaked = row(&seat.owner, ROOM, DAVE, 1_000);
    assert_eq!(
        hosted.store.insert(&leaked, &wide()).await.expect("insert"),
        SeatInsert::Inserted
    );
    assert!(row_removed(&hosted, ROOM, DAVE).await);
    // The row of the seat it holds stays, and so does the seat.
    tokio::time::sleep(QUIET).await;
    assert!(has_row(&hosted, ROOM, BOB).await);
    assert!(hosted.seat(ROOM, BOB).is_some());
    assert!(hosted.host.ends_of(BOB).is_empty());
}

#[tokio::test]
async fn a_stopping_replica_tells_its_remote_occupants_with_status_332() {
    let hosted = Scripted::start("inbound-router-shutdown", RouterLimits::default()).await;
    with_bob(&hosted).await;
    hosted.router.shutdown(Duration::from_secs(5)).await;
    // XEP-0045 §11.2: the occupant's own unavailable presence says that
    // the service is shutting down.
    let told = hosted
        .home_b
        .received(is_own_departure)
        .await
        .expect("the occupant's own presence");
    let codes = statuses(&told);
    assert!(
        codes.contains(&"110".to_owned()) && codes.contains(&"332".to_owned()),
        "{codes:?}"
    );
    assert!(hosted.router.seats().is_empty());
    assert_eq!(hosted.store.count().await.expect("count"), 0);
}

#[tokio::test]
async fn presence_for_an_earlier_occupancy_neither_ends_nor_reaches_the_seat() {
    let hosted = Scripted::start("inbound-router-stale-presence", RouterLimits::default()).await;
    let seat = with_bob(&hosted).await;
    // The room tells the occupant that an occupancy ended while it seats
    // the occupant under the seat's generation: what a room effect that
    // is delivered again after the occupant entered again amounts to.
    hosted
        .router
        .offer_to_seat(&full(BOB), own_unavailable(&seat))
        .expect("the room's presence is taken for the seat");
    wait_for(ARRIVES, || (hosted.host.seat_asks() >= 1).then_some(()))
        .await
        .expect("the room is asked whether it seats the occupancy");
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        hosted.seat(ROOM, BOB).map(|held| held.session),
        Some(seat.session)
    );
    assert_eq!(hosted.home_b.count(is_own_departure), 0);
    // Once the room no longer seats that occupancy, the same presence is
    // sent and ends the seat.
    hosted.host.forget(ROOM, BOB);
    hosted
        .router
        .offer_to_seat(&full(BOB), own_unavailable(&seat))
        .expect("the room's presence is taken for the seat");
    let told = hosted
        .home_b
        .received(is_own_departure)
        .await
        .expect("the occupant's own presence");
    assert!(statuses(&told).contains(&"110".to_owned()), "{told:?}");
    assert!(hosted.unseated(ROOM, BOB).await);
    assert!(row_removed(&hosted, ROOM, BOB).await);
    // The room decided the end; the router asked it to remove nobody.
    assert!(hosted.host.ends_of(BOB).is_empty());
}

#[tokio::test]
async fn an_occupant_that_enters_again_is_answered_before_the_rooms_traffic() {
    let hosted = Scripted::start_with(
        "inbound-router-rejoin",
        RouterLimits::default(),
        ScriptedRooms::greet,
    )
    .await;
    let first = with_bob(&hosted).await;
    let is_greeting = |stanza: &Element| body(stanza).as_deref() == Some(GREETING);
    hosted
        .home_b
        .received(is_greeting)
        .await
        .expect("the room's message for the occupant that entered");
    let before = hosted.home_b.captured.stanzas().len();
    // XEP-0045 §7.2: presence with the `muc` element asks to enter.
    hosted
        .home_b
        .send(remote_join(BOB, ROOM, "bob", "join-2"))
        .await;
    let second = wait_for(ARRIVES, || {
        hosted
            .seat(ROOM, BOB)
            .filter(|held| held.session != first.session)
    })
    .await
    .expect("the seat opened for the second join");
    // The host was asked twice: with the seat the occupant held, and then
    // with the one opened for the join.
    let asked = hosted.host.handled_with("join-2");
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(!asked[0].joining && asked[0].session == Some(first.session));
    assert!(asked[1].joining && asked[1].session == Some(second.session));
    assert_eq!(hosted.host.seats_under(ROOM, BOB), Some(second.session));
    // §7.1 and §7.2.3: the occupant's own presence precedes what the room
    // sent it while the join was answered.
    wait_for(ARRIVES, || {
        let after: Vec<Element> = hosted
            .home_b
            .captured
            .stanzas()
            .into_iter()
            .skip(before)
            .collect();
        after.iter().any(is_greeting).then_some(())
    })
    .await
    .expect("the room's message for the occupant that entered again");
    let after: Vec<Element> = hosted
        .home_b
        .captured
        .stanzas()
        .into_iter()
        .skip(before)
        .collect();
    let own = after.iter().position(is_own_presence);
    let greeted = after.iter().position(is_greeting);
    assert!(own.is_some() && own < greeted, "{own:?} {greeted:?}");
    // The seat the occupant gave up left no row and no occupant behind.
    assert_eq!(hosted.store.count().await.expect("count"), 1);
    assert!(hosted.host.ends_of(BOB).is_empty());
}

#[tokio::test]
async fn one_account_cannot_spend_its_domains_request_rate() {
    let limits = RouterLimits {
        room_request_rate_per_account: 1,
        room_request_burst_per_account: 2,
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-account-rate", limits).await;
    let ids: Vec<String> = (0..8).map(|index| format!("ping-{index}")).collect();
    for id in &ids {
        hosted.home_b.send(remote_get(BOB, MUC, id, ping())).await;
    }
    // Another account of the same domain is answered at once: the first
    // account's stanzas beyond its share spent none of the domain's rate
    // and held the stream for none of them.
    hosted
        .home_b
        .send(remote_get(DAVE, MUC, "ping-dave", ping()))
        .await;
    let answered = hosted.home_b.answered("ping-dave").await;
    assert_eq!(answered.attr("type"), Some("result"), "{answered:?}");
    tokio::time::sleep(QUIET).await;
    let of_type = |type_: &str| {
        ids.iter()
            .filter(|id| {
                hosted
                    .home_b
                    .captured
                    .with_id(id)
                    .iter()
                    .any(|stanza| stanza.attr("type") == Some(type_))
            })
            .count()
    };
    // Two at once, and at most one more for each second that passed.
    let taken = of_type("result");
    assert!((2..=4).contains(&taken), "{taken}");
    assert_eq!(hosted.host.handled().len(), taken + 1);
    // What was not taken is answered with `resource-constraint`.
    assert_eq!(of_type("error"), ids.len() - taken);
    let refused = hosted
        .home_b
        .received(|stanza| stanza.attr("type") == Some("error"))
        .await
        .expect("a refusal");
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
}

#[tokio::test]
async fn chat_states_beyond_their_senders_allowance_are_dropped_and_the_seat_stays() {
    let limits = RouterLimits {
        // Four at once, and one more each second.
        seat_ephemeral_rate_per_sender: 1,
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-chat-states", limits).await;
    with_bob(&hosted).await;
    let composing = || {
        from_room(
            None,
            Some(Element::builder("composing", NS_CHATSTATES).build()),
        )
    };
    for _ in 0..16 {
        hosted
            .router
            .offer_to_seat(&full(BOB), composing())
            .expect("a room message is taken for the seat");
    }
    // A message with a text is not a chat state: it is always queued.
    hosted
        .router
        .offer_to_seat(&full(BOB), from_room(Some("after the burst"), None))
        .expect("a room message is taken for the seat");
    hosted
        .home_b
        .received(|stanza| body(stanza).as_deref() == Some("after the burst"))
        .await
        .expect("the message that followed the chat states");
    tokio::time::sleep(QUIET).await;
    let states = hosted.home_b.count(|stanza| {
        stanza
            .children()
            .any(|child| child.is("composing", NS_CHATSTATES))
    });
    assert!((4..=5).contains(&states), "{states}");
    assert!(hosted.seat(ROOM, BOB).is_some());
    assert!(hosted.host.ends_of(BOB).is_empty());
}

#[tokio::test]
async fn a_join_is_refused_where_no_queue_for_its_domain_can_be_opened() {
    let limits = RouterLimits {
        peer_queues: 1,
        reply_queues: 1,
        ..RouterLimits::default()
    };
    let hosted = Scripted::start("inbound-router-no-queue", limits).await;
    // The one queue is opened for the first domain that joins, which
    // keeps it while this replica seats its occupant.
    hosted
        .home_c
        .send(remote_join(CAROL, ROOM, "carol", "join-c"))
        .await;
    hosted.seated(ROOM, CAROL).await;
    hosted
        .home_b
        .send(remote_join(BOB, ROOM, "bob", "join-b"))
        .await;
    tokio::time::sleep(QUIET).await;
    // The join never reached the room: no seat was opened for it, so no
    // occupant was seated that the room's first stanza would have ended.
    assert!(hosted.host.handled_with("join-b").is_empty());
    assert!(hosted.seat(ROOM, BOB).is_none());
    assert!(!has_row(&hosted, ROOM, BOB).await);
    assert!(hosted.seat(ROOM, CAROL).is_some());
}

#[tokio::test]
async fn a_seat_taken_over_from_a_stopped_replica_ends_with_status_333() {
    let hosted = Scripted::start("inbound-router-adopt", RouterLimits::default()).await;
    let (room, bob) = (bare(ROOM), full(BOB));
    let left = row(STOPPED, ROOM, BOB, 1_000);
    assert_eq!(
        hosted.store.insert(&left, &wide()).await.expect("insert"),
        SeatInsert::Inserted
    );
    // The room's claim moved here: its recorded occupant is seated again
    // on the seat taken over, with the evidence that seat recorded.
    assert!(RemoteRoute::adopt_seat(&hosted.router, &room, &bob).await);
    let seat = hosted.seat(ROOM, BOB).expect("the seat taken over");
    assert_eq!(seat.session, left.session);
    assert_eq!(seat.evidence, left.evidence);
    hosted.host.seat(&seat);
    // What the stopped replica had queued for the occupant is lost: the
    // occupancy does not continue.
    RemoteRoute::end_adopted_seats(&hosted.router, &room, &[bob]).await;
    assert_eq!(ends_of(&hosted, BOB).await, vec![SeatEnd::HolderStopped]);
    let told = hosted
        .home_b
        .received(is_own_departure)
        .await
        .expect("the occupant's own presence");
    let codes = statuses(&told);
    assert!(
        codes.contains(&"110".to_owned()) && codes.contains(&"333".to_owned()),
        "{codes:?}"
    );
    assert!(hosted.unseated(ROOM, BOB).await);
    assert!(row_removed(&hosted, ROOM, BOB).await);
    assert_eq!(hosted.host.seats_under(ROOM, BOB), None);
}

#[tokio::test]
async fn a_seated_occupant_the_host_refuses_is_removed_from_its_room() {
    let hosted = Scripted::start("inbound-router-refused", RouterLimits::default()).await;
    let seat = with_bob(&hosted).await;
    hosted.host.refuse_next(BOB);
    hosted
        .home_b
        .send(remote_get(BOB, ROOM, "refused-1", ping()))
        .await;
    let refused = hosted.home_b.answered("refused-1").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    // The seat ends, and the room is told to remove the occupant it
    // would otherwise keep with nothing able to reach it.
    assert_eq!(ends_of(&hosted, BOB).await, vec![SeatEnd::Refused]);
    assert!(hosted.unseated(ROOM, BOB).await);
    assert!(row_removed(&hosted, ROOM, BOB).await);
    assert_eq!(hosted.host.seats_under(ROOM, BOB), None);
    let told = hosted
        .home_b
        .received(is_own_departure)
        .await
        .expect("the removed occupant's own presence");
    assert!(statuses(&told).contains(&"333".to_owned()), "{told:?}");
    assert_ne!(hosted.host.seats_under(ROOM, BOB), Some(seat.session));
}

#[tokio::test]
async fn a_carried_request_is_taken_only_for_a_room_of_the_muc_service() {
    let hosted = Scripted::start("inbound-router-carried", RouterLimits::default()).await;
    let carried = |room: &str| CarriedRoomRequest {
        principal: CarriedPrincipal {
            address: jid(BOB),
            strength: AuthenticationStrength::Pkix,
            trust_profile: Some(TrustProfileKind::PrivateCa),
            generation: uuid::Uuid::nil(),
        },
        stanza: Stanza::Presence(remote_join(BOB, room, "bob", "carried-1")),
    };
    // An address with a localpart at the account domain, which this
    // deployment serves, names no room.
    let account_domain = ["lounge@", DOMAIN].concat();
    assert_eq!(
        hosted.router.carried_room_request(carried(&account_domain)),
        CarriedRoomOutcome::Refused
    );
    assert_eq!(
        hosted.router.carried_room_request(carried(ROOM)),
        CarriedRoomOutcome::Queued
    );
    hosted.seated(ROOM, BOB).await;
    assert_eq!(hosted.host.handled_with("carried-1").len(), 1);
}
