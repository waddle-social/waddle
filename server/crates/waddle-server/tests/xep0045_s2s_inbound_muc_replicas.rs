//! XEP-0045 across S2S, inbound, with two replicas: a remote occupant of a
//! room held by a replica other than the one its server's stream landed on
//! (ADR-0020 D10, with D1 and D8 at the effect boundary).
//!
//! A request for a room another replica holds is carried there with what
//! the inbound stream proved of its sender, before anything is decided or
//! seated for it on the replica that received it. The holder decides the
//! request on the carried evidence, seats the occupant and answers through
//! its own connection manager, so the join's answer and the room's traffic
//! reach the occupant on one stream in the room's order (§7.1). A request
//! the holder cannot take is answered by the replica the stream landed on
//! with a retryable error.
//!
//! Both replicas run in this process over the same S2S snapshot
//! (`s2s_inbound_muc_support::TwoReplicas`): the peer's streams land on the
//! front replica, and the room is held by the other, to which a carrier
//! hands what the cluster relay
//! (`waddle.clustering.relay.remote_room_request.v1`) carries. The
//! cluster's room claims and the relay hop themselves are not run here.
//! No PostgreSQL, SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use std::sync::Arc;

use minidom::Element;
use s2s_inbound_muc_support::{
    body, error_condition, item, jid, local_groupchat, local_join, local_receives, local_set,
    naming, owner_form, presence_between, remote_get, remote_groupchat, remote_join, remote_leave,
    remote_set, statuses, TwoReplicas, ARRIVES, BOB, BOB_OCCUPANT, HOME_B, MUC, QUIET, ROOM,
};
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::route::{
    CarriedPrincipal, CarriedRoomOutcome, CarriedRoomRequest, RemotePrincipal, RoomPlacement,
};
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_server::test_support::RoomCarrier;
use waddle_xmpp::Stanza;

const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MAM: &str = "urn:xmpp:mam:2";
const NS_MODERATE: &str = "urn:xmpp:message-moderate:1";
const NS_RETRACT: &str = "urn:xmpp:message-retract:1";

/// A resource of bob's account that holds no seat.
const BOB_TABLET: &str = "bob@home-b.test/tablet";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

/// `home-b.test` is a trusted home whose entry requires its private CA.
fn trusting_the_private_ca(spec: &mut s2s_inbound_muc_support::Spec) {
    spec.trusted_homes = vec![HOME_B];
    spec.require_private_ca = vec![HOME_B];
}

/// A `muc#admin` item that sets `affiliation` for `account` (XEP-0045
/// §9, §10).
fn affiliation_item(affiliation: &str, account: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                .build(),
        )
        .build()
}

/// A `muc#admin` request for the list of `affiliation` (XEP-0045 §9.5).
fn affiliation_list(affiliation: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .build(),
        )
        .build()
}

/// What a stream proved of a sender whose stanza a replica carries.
#[derive(Clone, Copy)]
struct Proved {
    strength: AuthenticationStrength,
    trust_profile: Option<TrustProfileKind>,
}

/// A stream that authenticated by dialback (XEP-0220).
const DIALBACK: Proved = Proved {
    strength: AuthenticationStrength::Dialback,
    trust_profile: None,
};

/// A stream whose certificate was verified under the public Web PKI.
const PUBLIC_CA: Proved = Proved {
    strength: AuthenticationStrength::Pkix,
    trust_profile: Some(TrustProfileKind::PublicWebPki),
};

/// Hand `stanza` from `from` to the holder as the replica that verified a
/// stream that `proved` so much would carry it.
fn carry(replicas: &TwoReplicas, from: &str, proved: Proved, stanza: Stanza) {
    let carried = CarriedRoomRequest {
        principal: CarriedPrincipal {
            address: jid(from),
            strength: proved.strength,
            trust_profile: proved.trust_profile,
            generation: uuid::Uuid::nil(),
        },
        stanza,
    };
    assert_eq!(
        replicas.holder.router.carried_room_request(carried),
        CarriedRoomOutcome::Queued
    );
}

/// The requests of a room's administration and archive a standing admits,
/// from `from`, each with an id that starts with `prefix`: the ban list
/// (§9.8), the configuration form (§10.2) and a XEP-0313 query.
fn standing_requests(from: &str, prefix: &str) -> Vec<(String, Stanza)> {
    let id = |name: &str| [prefix, "-", name].concat();
    vec![
        (
            id("list"),
            Stanza::Iq(Box::new(remote_get(
                from,
                ROOM,
                &id("list"),
                affiliation_list("outcast"),
            ))),
        ),
        (
            id("config"),
            Stanza::Iq(Box::new(remote_get(
                from,
                ROOM,
                &id("config"),
                Element::builder("query", NS_MUC_OWNER).build(),
            ))),
        ),
        (
            id("archive"),
            Stanza::Iq(Box::new(remote_set(
                from,
                ROOM,
                &id("archive"),
                Element::builder("query", NS_MAM)
                    .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), id("q"))
                    .build(),
            ))),
        ),
    ]
}

/// [`ROOM`] created on the holder by `alice@example.com/desk`, and
/// `bob@home-b.test/phone` joined to it through the front replica: the
/// replicas, alice's client and the room's own presence for bob.
async fn room_on_the_holder(
    adjust: impl FnOnce(&mut s2s_inbound_muc_support::Spec),
) -> (TwoReplicas, waddle_ws_test_support::WsXmppClient, Element) {
    let replicas = TwoReplicas::start(adjust).await;
    let mut alice = replicas.holder.room_creator("alice", "desk").await;
    let created = local_join(&mut alice, ROOM, "alice").await;
    assert_eq!(created.attr("type"), None, "{created:?}");
    replicas
        .holder
        .from_peer(remote_join(BOB, ROOM, "bob", "join-1"))
        .await;
    let own = replicas
        .holder
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the remote occupant");
    (replicas, alice, own)
}

#[tokio::test]
async fn a_join_for_a_room_another_replica_holds_is_seated_and_answered_by_that_replica() {
    let (replicas, mut alice, own) = room_on_the_holder(no_change).await;
    assert_eq!(
        item(&own),
        Some(("participant".to_owned(), "none".to_owned()))
    );
    // §7.2.3: the occupants already there precede the occupant's own
    // presence, on the one stream the holder sends on.
    let stanzas = replicas.holder.captured.stanzas();
    let position = |from: &str, own: bool| {
        stanzas.iter().position(|stanza| {
            presence_between(stanza, from, BOB)
                && statuses(stanza).contains(&"110".to_owned()) == own
        })
    };
    let existing = position("lounge@muc.example.com/alice", false);
    assert!(
        existing.is_some() && existing < position(BOB_OCCUPANT, true),
        "{stanzas:?}"
    );
    // The seat is the holder's, with the evidence of the stream that
    // carried the join to the front; the front seats nobody.
    let seat = replicas
        .holder
        .seat(ROOM, BOB)
        .expect("the holder's seat for the remote occupant");
    assert_eq!(seat.evidence.strength(), AuthenticationStrength::Pkix);
    assert_eq!(
        seat.evidence.trust_profile(),
        Some(TrustProfileKind::PrivateCa)
    );
    assert!(replicas.front_seat(ROOM, BOB).is_none());
    assert!(replicas.front_router.seats().is_empty());
    // The local occupant is told of the remote one.
    let seen = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "presence" && stanza.attr("from") == Some(BOB_OCCUPANT)
    })
    .await;
    assert!(seen.is_some(), "the local occupant was not told");
}

#[tokio::test]
async fn messages_pass_both_ways_through_the_replica_that_holds_the_room() {
    let (replicas, mut alice, _own) = room_on_the_holder(no_change).await;
    // The remote occupant's message lands on the front, is carried to the
    // holder, and is reflected from there (§7.4).
    replicas
        .holder
        .from_peer(remote_groupchat(BOB, ROOM, "remote-1", "hello from there"))
        .await;
    let received = local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("hello from there")
    })
    .await
    .expect("the remote occupant's message for the local occupant");
    assert_eq!(received.attr("from"), Some(BOB_OCCUPANT));
    let reflection = replicas
        .holder
        .at_peer(|stanza| {
            stanza.name() == "message"
                && stanza.attr("from") == Some(BOB_OCCUPANT)
                && stanza.attr("to") == Some(BOB)
                && body(stanza).as_deref() == Some("hello from there")
        })
        .await;
    assert!(reflection.is_some(), "the sender's reflection");
    // The room's traffic for the occupant leaves from the holder.
    local_groupchat(&mut alice, ROOM, "local-1", "hello from here").await;
    let is_copy = |stanza: &Element| {
        stanza.attr("to") == Some(BOB) && body(stanza).as_deref() == Some("hello from here")
    };
    replicas
        .holder
        .at_peer(is_copy)
        .await
        .expect("the remote occupant's copy");
    tokio::time::sleep(QUIET).await;
    assert_eq!(replicas.holder.count_at_peer(is_copy), 1);
}

#[tokio::test]
async fn administration_is_decided_by_the_holder_on_the_carried_evidence() {
    // `home-b.test` is a trusted home and authenticates by certificate.
    let (replicas, mut alice, _own) = room_on_the_holder(|spec| {
        spec.trusted_homes = vec![HOME_B];
    })
    .await;
    let list = || {
        Element::builder("query", NS_MUC_ADMIN)
            .append(
                Element::builder("item", NS_MUC_ADMIN)
                    .attr(
                        minidom::rxml::xml_ncname!("affiliation").to_owned(),
                        "outcast",
                    )
                    .build(),
            )
            .build()
    };
    // The room holds no standing for the occupant yet.
    replicas
        .holder
        .from_peer(remote_get(BOB, ROOM, "list-1", list()))
        .await;
    let refused = replicas.holder.answered("list-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    let grant = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    "admin",
                )
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    "bob@home-b.test",
                )
                .build(),
        )
        .build();
    let granted = local_set(&mut alice, ROOM, "grant-1", grant).await;
    assert_eq!(granted.attr("type"), Some("result"), "{granted:?}");
    // The admin standing is honoured for the `pkix` stream of a trusted
    // home that carried the request to the front.
    replicas
        .holder
        .from_peer(remote_get(BOB, ROOM, "list-2", list()))
        .await;
    let answered = replicas.holder.answered("list-2").await;
    assert_eq!(answered.attr("type"), Some("result"), "{answered:?}");
    assert_eq!(answered.attr("from"), Some(ROOM));
}

#[tokio::test]
async fn the_holder_decides_a_carried_stanza_on_the_evidence_carried_with_it() {
    let (replicas, _alice, _own) = room_on_the_holder(no_change).await;
    let seated = replicas.holder.seat(ROOM, BOB).expect("the first seat");
    // The occupant's next stanza arrives on a stream that proved less: the
    // replica that verified it carries what that stream proved.
    let weaker = CarriedRoomRequest {
        principal: CarriedPrincipal {
            address: jid(BOB),
            strength: AuthenticationStrength::Dialback,
            trust_profile: None,
            generation: seated.snapshot,
        },
        stanza: Stanza::Presence(remote_join(BOB, ROOM, "bob", "join-2")),
    };
    assert_eq!(
        replicas.holder.router.carried_room_request(weaker),
        CarriedRoomOutcome::Queued
    );
    // The seat admitted on the certificate does not stand for it: it ends,
    // the occupant is told (status 333), and presence enters again on the
    // evidence that came with it.
    let told = replicas
        .holder
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && stanza.attr("type") == Some("unavailable")
                && statuses(stanza).contains(&"333".to_owned())
        })
        .await;
    assert!(told.is_some(), "the occupant was not told its seat ended");
    let reseated = s2s_inbound_muc_support::wait_for(ARRIVES, || {
        replicas
            .holder
            .seat(ROOM, BOB)
            .filter(|seat| seat.session != seated.session)
    })
    .await
    .expect("the seat opened on the carried evidence");
    assert_eq!(
        reseated.evidence.strength(),
        AuthenticationStrength::Dialback
    );
    assert_eq!(reseated.evidence.trust_profile(), None);
}

#[tokio::test]
async fn the_holder_takes_only_a_room_stanza_from_the_principal_it_names() {
    let replicas = TwoReplicas::start(no_change).await;
    let carried = |address: &str, stanza: Stanza| CarriedRoomRequest {
        principal: CarriedPrincipal {
            address: jid(address),
            strength: AuthenticationStrength::Pkix,
            trust_profile: Some(TrustProfileKind::PrivateCa),
            generation: uuid::Uuid::nil(),
        },
        stanza,
    };
    let router = &replicas.holder.router;
    // A stanza whose sender is not the principal carried with it.
    assert_eq!(
        router.carried_room_request(carried(
            "mallory@home-b.test/phone",
            Stanza::Presence(remote_join(BOB, ROOM, "bob", "c1")),
        )),
        CarriedRoomOutcome::Refused
    );
    // A stanza for the service, which every replica answers itself.
    let to_service = remote_get(
        BOB,
        MUC,
        "c2",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    );
    assert_eq!(
        router.carried_room_request(carried(BOB, Stanza::Iq(Box::new(to_service)))),
        CarriedRoomOutcome::Refused
    );
    // A request outside the operations a remote principal reaches.
    let register = remote_get(
        BOB,
        ROOM,
        "c3",
        Element::builder("query", "jabber:iq:register").build(),
    );
    assert_eq!(
        router.carried_room_request(carried(BOB, Stanza::Iq(Box::new(register)))),
        CarriedRoomOutcome::Refused
    );
    // An address that is not in canonical form names no principal.
    assert!(RemotePrincipal::carried(CarriedPrincipal {
        address: jid("Bob@home-b.test/phone"),
        strength: AuthenticationStrength::Pkix,
        trust_profile: None,
        generation: uuid::Uuid::nil(),
    })
    .is_err());
    assert!(replicas.holder.seat(ROOM, BOB).is_none());
}

/// A carrier whose holder never takes a request: the room's claim names a
/// replica that cannot be reached.
struct Unreachable;

#[async_trait::async_trait]
impl RoomCarrier for Unreachable {
    async fn place(
        &self,
        _room: &jid::BareJid,
        _principal: &RemotePrincipal,
        _stanza: &Stanza,
    ) -> RoomPlacement {
        RoomPlacement::Refused(Box::new(xmpp_parsers::stanza_error::StanzaError::new(
            xmpp_parsers::stanza_error::ErrorType::Wait,
            xmpp_parsers::stanza_error::DefinedCondition::ResourceConstraint,
            "en",
            "The room is held by another server node that could not be reached.",
        )))
    }
}

#[tokio::test]
async fn a_request_the_holder_cannot_take_is_answered_where_the_stream_landed() {
    use s2s_inbound_muc_support::{Hosted, Wiring};
    use waddle_server::s2s::route::RouterLimits;
    use waddle_server::test_support::websocket_state_running_s2s;
    // One replica whose carrier finds every room held elsewhere and out of
    // reach.
    let wiring = Wiring::new(no_change).await;
    let state = websocket_state_running_s2s(&wiring.config).await;
    let hosted = Hosted::serve_carrying(
        wiring,
        state,
        RouterLimits::default(),
        Arc::new(Unreachable),
    )
    .await;
    hosted
        .from_peer(remote_join(BOB, ROOM, "bob", "join-1"))
        .await;
    let refused = hosted.answered("join-1").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    // Nothing was seated for it here.
    assert!(hosted.seat(ROOM, BOB).is_none());
    assert!(hosted.router.seats().is_empty());
    // A stanza for the service is every replica's own.
    hosted
        .from_peer(remote_get(
            BOB,
            MUC,
            "disco-1",
            Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
        ))
        .await;
    let answered = hosted.answered("disco-1").await;
    assert_eq!(answered.attr("type"), Some("result"), "{answered:?}");
}

#[tokio::test]
async fn a_stored_owners_standing_counts_only_on_a_stream_that_honours_it() {
    let (replicas, mut alice, _own) = room_on_the_holder(trusting_the_private_ca).await;
    // The account is made an owner while it is seated on a certificate
    // stream of its trusted home, verified under the home's private CA.
    let granted = local_set(
        &mut alice,
        ROOM,
        "grant-1",
        affiliation_item("owner", "bob@home-b.test"),
    )
    .await;
    assert_eq!(granted.attr("type"), Some("result"), "{granted:?}");
    replicas
        .holder
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(_, affiliation)| affiliation == "owner")
        })
        .await
        .expect("the occupant is told of its standing");
    // Something for the archive to hold.
    local_groupchat(&mut alice, ROOM, "seed-1", "something to read").await;
    replicas
        .holder
        .at_peer(|stanza| body(stanza).as_deref() == Some("something to read"))
        .await
        .expect("the occupant's copy of the archived message");
    // The room admits members only, so the archive is read on a standing.
    let closed = local_set(
        &mut alice,
        ROOM,
        "members-only-1",
        owner_form(&[("muc#roomconfig_membersonly", "1")]),
    )
    .await;
    assert_eq!(closed.attr("type"), Some("result"), "{closed:?}");

    // On that stream the stored standing is honoured, for a resource of
    // the account that holds no seat as for one that does.
    for (id, request) in standing_requests(BOB_TABLET, "trusted") {
        let element = match request {
            Stanza::Iq(iq) => Element::from(*iq),
            Stanza::Message(message) => Element::from(message),
            Stanza::Presence(presence) => Element::from(presence),
        };
        replicas.holder.from_peer(element).await;
        let answered = replicas.holder.answered(&id).await;
        assert_eq!(answered.attr("type"), Some("result"), "{id}: {answered:?}");
    }
    // ADR-0020 D8 at the request: the same requests of the same account
    // are refused when the stream that carried them proved less. A
    // dialback stream carries no owner or admin standing, and neither
    // does a certificate of the public Web PKI where the home's entry
    // requires its private CA. The stored entry then contributes nothing.
    for (prefix, proved) in [("dialback", DIALBACK), ("public-ca", PUBLIC_CA)] {
        for (id, request) in standing_requests(BOB_TABLET, prefix) {
            carry(&replicas, BOB_TABLET, proved, request);
            let refused = replicas.holder.answered(&id).await;
            assert_eq!(refused.attr("type"), Some("error"), "{id}: {refused:?}");
            assert_eq!(
                error_condition(&refused).as_deref(),
                Some("forbidden"),
                "{id}"
            );
        }
        // XEP-0425: no moderation on such a stream either.
        let id = [prefix, "-moderate"].concat();
        let mut moderate = naming("moderate", NS_MODERATE, "some-archived-message");
        moderate.append_child(Element::builder("retract", NS_RETRACT).build());
        carry(
            &replicas,
            BOB_TABLET,
            proved,
            Stanza::Iq(Box::new(remote_set(BOB_TABLET, ROOM, &id, moderate))),
        );
        let refused = replicas.holder.answered(&id).await;
        assert_eq!(
            error_condition(&refused).as_deref(),
            Some("forbidden"),
            "{id}"
        );
        // Nor is a seat taken on it: the room would count the stored
        // entry, which is not honoured, so the join is refused.
        let id = [prefix, "-join"].concat();
        carry(
            &replicas,
            BOB_TABLET,
            proved,
            Stanza::Presence(remote_join(BOB_TABLET, ROOM, "bob", &id)),
        );
        let refused = replicas.holder.answered(&id).await;
        assert_eq!(refused.attr("type"), Some("error"), "{id}: {refused:?}");
        assert_eq!(
            error_condition(&refused).as_deref(),
            Some("forbidden"),
            "{id}"
        );
        assert!(replicas.holder.seat(ROOM, BOB_TABLET).is_none(), "{id}");
    }
    // The seat taken on the certificate stream stands throughout.
    assert!(replicas.holder.seat(ROOM, BOB).is_some());
}

#[tokio::test]
async fn an_occupant_made_admin_on_a_certificate_keeps_no_standing_on_a_dialback_stream() {
    let (replicas, mut alice, _own) = room_on_the_holder(|spec| {
        spec.trusted_homes = vec![HOME_B];
    })
    .await;
    let granted = local_set(
        &mut alice,
        ROOM,
        "grant-1",
        affiliation_item("admin", "bob@home-b.test"),
    )
    .await;
    assert_eq!(granted.attr("type"), Some("result"), "{granted:?}");
    replicas
        .holder
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(_, affiliation)| affiliation == "admin")
        })
        .await
        .expect("the occupant is told of its standing");
    // The occupant's presence arrives on a dialback stream: the seat taken
    // on the certificate ends, and the presence enters again on what the
    // dialback stream proved, which carries no admin standing. The room
    // would count the stored entry, so the join is refused (ADR-0020 D8
    // where the seat is taken).
    carry(
        &replicas,
        BOB,
        DIALBACK,
        Stanza::Presence(remote_join(BOB, ROOM, "bob", "join-dialback")),
    );
    let told = replicas
        .holder
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && stanza.attr("type") == Some("unavailable")
                && statuses(stanza).contains(&"333".to_owned())
        })
        .await;
    assert!(told.is_some(), "the occupant was not told its seat ended");
    let refused = replicas.holder.answered("join-dialback").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    assert!(replicas.holder.unseated(ROOM, BOB).await);
    // Those who stay see it removed by the service (§11.1).
    let seen = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "presence"
            && stanza.attr("from") == Some(BOB_OCCUPANT)
            && stanza.attr("type") == Some("unavailable")
    })
    .await
    .expect("the occupant's removal for the local occupant");
    assert!(statuses(&seen).contains(&"333".to_owned()), "{seen:?}");
    // A subject change on that stream is no moderator's: the sender holds
    // no seat, and nothing reaches the room (§8.1).
    let mut subject = xmpp_parsers::message::Message::new_with_type(
        xmpp_parsers::message::MessageType::Groupchat,
        Some(jid(ROOM)),
    );
    subject.from = Some(jid(BOB));
    subject.id = Some(xmpp_parsers::message::Id("subject-dialback".to_owned()));
    subject.subjects.insert(
        xmpp_parsers::message::Lang(String::new()),
        "taken over".to_owned(),
    );
    carry(&replicas, BOB, DIALBACK, Stanza::Message(subject));
    let refused = replicas.holder.answered("subject-dialback").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    let changed = local_receives(&mut alice, QUIET, |stanza| {
        stanza
            .children()
            .any(|child| child.name() == "subject" && child.text() == "taken over")
    })
    .await;
    assert!(changed.is_none(), "{changed:?}");
}

#[tokio::test]
async fn an_owner_or_admin_standing_is_not_issued_while_a_seat_would_not_honour_it() {
    let replicas = TwoReplicas::start(|spec| {
        spec.trusted_homes = vec![HOME_B];
    })
    .await;
    let mut alice = replicas.holder.room_creator("alice", "desk").await;
    let created = local_join(&mut alice, ROOM, "alice").await;
    assert_eq!(created.attr("type"), None, "{created:?}");
    // An account of a trusted home enters over a dialback stream: as a
    // participant, which any admitted domain's account may be.
    carry(
        &replicas,
        BOB,
        DIALBACK,
        Stanza::Presence(remote_join(BOB, ROOM, "bob", "join-1")),
    );
    let own = replicas
        .holder
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the occupant");
    assert_eq!(
        item(&own),
        Some(("participant".to_owned(), "none".to_owned()))
    );
    let seat = replicas.holder.seat(ROOM, BOB).expect("the seat");
    assert_eq!(seat.evidence.strength(), AuthenticationStrength::Dialback);
    // The room makes every occupant of an owner or admin a moderator
    // (XEP-0045 §5.1.3). That seat's stream carries no such role, so the
    // standing is not issued while the seat is held (ADR-0020 D8), though
    // the account's home is trusted.
    for (id, affiliation) in [("grant-admin", "admin"), ("grant-owner", "owner")] {
        let answer = local_set(
            &mut alice,
            ROOM,
            id,
            affiliation_item(affiliation, "bob@home-b.test"),
        )
        .await;
        assert_eq!(answer.attr("type"), Some("error"), "{id}: {answer:?}");
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("not-allowed"),
            "{id}"
        );
    }
    // Nor is the role itself granted to the occupant of that seat.
    let role = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
                .build(),
        )
        .build();
    let answer = local_set(&mut alice, ROOM, "role-1", role).await;
    assert_eq!(answer.attr("type"), Some("error"), "{answer:?}");
    assert_eq!(error_condition(&answer).as_deref(), Some("not-allowed"));
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        replicas.holder.count_at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(role, _)| role == "moderator")
        }),
        0,
        "the occupant was made a moderator on a dialback stream"
    );
    // A membership derives no moderator role and is issued.
    let answer = local_set(
        &mut alice,
        ROOM,
        "grant-member",
        affiliation_item("member", "bob@home-b.test"),
    )
    .await;
    assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
    // Once the occupant has left, no seat stands against the standing.
    carry(
        &replicas,
        BOB,
        DIALBACK,
        Stanza::Presence(remote_leave(BOB, ROOM, "bob", "leave-1")),
    );
    assert!(replicas.holder.unseated(ROOM, BOB).await);
    let answer = local_set(
        &mut alice,
        ROOM,
        "grant-admin-2",
        affiliation_item("admin", "bob@home-b.test"),
    )
    .await;
    assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
}
