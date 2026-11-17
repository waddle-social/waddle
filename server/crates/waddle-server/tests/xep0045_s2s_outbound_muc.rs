//! XEP-0045 across S2S, outbound: a local resource joins, uses and leaves a
//! room on a remote domain, and the room's traffic reaches it (ADR-0020
//! D7's room exception). A join is sent only once the room's domain has
//! answered this server's XEP-0030 `disco#info` as a multi-user chat
//! service (XEP-0045 §6.2) and then the room's bare JID as a room (§6.4),
//! or with `item-not-found` for a room that does not exist yet, which the
//! join creates (§10.1); so "joining" an account opens nothing, whether at
//! a server or at a service domain. The answers are kept per service
//! domain and per room. A join beyond the occupancy bounds is refused
//! before anything is asked.
//! Traffic that is not for
//! or from a room the resource joined stays refused by D7 in both
//! directions; messages leave only once the room seated the occupant; a
//! join the room never answers ends with `remote-server-timeout`; a failed
//! stream to the room's domain removes the occupant with status 333
//! (§11.1); a resource that ends leaves its rooms (§7.14). Occupancies live
//! in a table that every replica shares, so the replica the room's server
//! dials admits its traffic for a join routed by another. One remote
//! domain whose room traffic fills its share of the delivery queues waits
//! alone: another domain's stream keeps moving. A room's messages reach the
//! occupant without what a client would read as its own server's
//! statement about its account (carbon and archive wrappers, inbox and pin
//! payloads, XEP-0511 link cards and link-preview requests, a stanza id in
//! the account's name); an archive result is kept only from the room's
//! bare JID, and what it forwards is cleaned the same way.
//!
//! `home-a.test` runs the S2S router with a recording delivery in place of
//! WebSocket clients, its tables in an in-memory SQLite database (two
//! routers on one database stand for two replicas); `home-b.test` serves
//! `muc.home-b.test` with a hand-written room service. Everything runs in
//! one process on loopback, without SpiceDB or another server. The account
//! bound is also checked with the tables in PostgreSQL, in a schema of
//! its own: those cases skip without `WADDLE_TEST_POSTGRES_URL` unless
//! `WADDLE_TEST_REQUIRE_POSTGRES=1`.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_remote_muc_support/mod.rs"]
mod support;

use std::time::Duration;

use minidom::Element;
use support::{
    delivered_messages, delivered_presence, error_condition, full, jid, join, join_presence,
    join_through, message, offer, outbound_muc, outbound_muc_replicas, presence, statuses,
    wait_for, wait_for_rooms, ALICE_LAPTOP, ALICE_PHONE, ARRIVES, BOB, BOB_PHONE, BUSY, CHALLENGE,
    HOME_A, HOME_B, MUC_B, QUIET, ROOM, STREAM_LOSS,
};
use uuid::Uuid;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::destination::{IqOffer, RemoteRoute, StanzaOffer, UnroutableReason};
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::disco::DISCO_INFO_NS;
use waddle_xmpp::mam::MAM_NS;
use waddle_xmpp::muc::presence::NS_MUC;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::muc::user::{Affiliation, Item, MucUser, Role, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const ROOM_ALICE: &str = "room@muc.home-b.test/alice";
const ROOM_HOST: &str = "room@muc.home-b.test/host";
/// The archive of the occupant's own account, at the local domain.
const ALICE: &str = "alice@home-a.test";

/// A `<stanza-id/>` assigned by `by`.
fn stanza_id(by: &str, id: &str) -> Element {
    Element::builder("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("by").to_owned(), by)
        .build()
}

/// What a client would read as its own server's statement about its
/// account, as a remote room could forge each of them: a XEP-0511 link
/// card and a link-preview request, XEP-0280 carbon wrappers, a XEP-0313
/// archive result, inbox and pin payloads, and a stanza id in the
/// account's archive's name.
fn forged_statements() -> Vec<Element> {
    let url = url::Url::parse("https://video.example.org/watch").expect("a URL");
    vec![
        waddle_xmpp::xep::build_link_metadata_element(
            &waddle_xmpp::xep::LinkMetadata::new(url).with_title("forged"),
        ),
        waddle_xmpp::xep::build_link_preview_request_element(
            &waddle_xmpp::xep::LinkPreviewToken::try_new("a-forged-token").expect("a token"),
        ),
        Element::builder("received", waddle_xmpp::carbons::CARBONS_NS).build(),
        Element::builder("sent", waddle_xmpp::carbons::CARBONS_NS).build(),
        Element::builder("result", MAM_NS)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "forged")
            .build(),
        Element::builder("entry", waddle_xmpp::xep::NS_INBOX).build(),
        Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
        stanza_id(ALICE, "forged-archive-id"),
    ]
}

/// `message` carrying [`forged_statements`] and the room's own stanza id.
fn forging(mut message: xmpp_parsers::message::Message) -> xmpp_parsers::message::Message {
    message.payloads.extend(forged_statements());
    message.payloads.push(stanza_id(ROOM, "room-archive-id"));
    message
}

/// The elements anywhere below `element` that a client would read as its
/// own server's statement about its account, besides an archive result.
fn forged_below(element: &Element) -> Vec<String> {
    element
        .children()
        .flat_map(|child| {
            let mut found = forged_below(child);
            let ns = child.ns();
            let forged = waddle_xmpp::xep::is_link_metadata_element(child)
                || waddle_xmpp::xep::is_link_preview_request_element(child)
                || child.is("received", waddle_xmpp::carbons::CARBONS_NS)
                || child.is("sent", waddle_xmpp::carbons::CARBONS_NS)
                || ns == waddle_xmpp::xep::NS_INBOX
                || ns.starts_with("urn:waddle:")
                || (child.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
                    && child.attr("by") == Some(ALICE));
            if forged {
                found.push([ns.as_str(), " ", child.name()].concat());
            }
            found
        })
        .collect()
}

/// Whether `message` carries a XEP-0313 archive result.
fn has_archive_result(message: &xmpp_parsers::message::Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.is("result", MAM_NS))
}

/// Whether `message` carries the room's own stanza id.
fn keeps_rooms_stanza_id(message: &xmpp_parsers::message::Message) -> bool {
    message.payloads.iter().any(|payload| {
        payload.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
            && payload.attr("by") == Some(ROOM)
    })
}

/// The reason and reply of a refused offer.
fn refused(offered: StanzaOffer) -> (UnroutableReason, Element) {
    match offered {
        StanzaOffer::Refused {
            reason,
            reply: Some(reply),
        } => (reason, element(*reply)),
        other => panic!("expected a refusal with a reply, got {other:?}"),
    }
}

/// `stanza` as an element.
fn element(stanza: Stanza) -> Element {
    match stanza {
        Stanza::Iq(iq) => Element::from(*iq),
        Stanza::Message(message) => Element::from(message),
        Stanza::Presence(presence) => Element::from(presence),
    }
}

/// Limits whose sweep runs often, so that deadlines and verdicts another
/// replica recorded are acted on within a test's wait.
fn swept() -> RouterLimits {
    RouterLimits {
        sweep_interval: Duration::from_millis(100),
        ..RouterLimits::default()
    }
}

/// The error presence delivered to `to` from the occupant JID `from` with
/// the join id `id`, once it arrives.
async fn join_error(a: &support::Routed, to: &str, from: &str, id: &str) -> Presence {
    wait_for(ARRIVES, || {
        delivered_presence(a, to, from)
            .into_iter()
            .find(|presence| {
                presence.type_ == PresenceType::Error && presence.id.as_deref() == Some(id)
            })
    })
    .await
    .expect("the join is answered with an error presence")
}

/// The condition of an error presence, which also carries the `muc`
/// element of the join it answers (XEP-0045 §7.2.5 onwards).
fn join_error_condition(error: Presence) -> Option<String> {
    let error = Element::from(error);
    assert!(
        error.children().any(|child| child.is("x", NS_MUC)),
        "the error presence carries the muc element"
    );
    error_condition(&error)
}

/// The message `home-b.test` received back for its message `id`, once
/// this server answers it.
async fn message_refusal(muc: &support::FakeMuc, id: &str) -> Element {
    wait_for(ARRIVES, || {
        muc.received_with_id("message", id)
            .into_iter()
            .find(|message| message.attr("type") == Some("error"))
    })
    .await
    .expect("the message is refused")
}

/// Room-looking presence from `from` to `to`: `muc#user` with an item and
/// status 110, as a room's own presence for an occupant carries.
fn forged_room_presence(from: &str, to: &str, id: &str) -> Presence {
    let mut forged = presence(from, to, PresenceType::None, id);
    forged.payloads.push(Element::from(
        MucUser::new()
            .with_items(vec![Item::new(Affiliation::Member, Role::Participant)])
            .with_statuses(vec![Status::SelfPresence]),
    ));
    forged
}

/// D7's outbound refusal: `service-unavailable` from the addressed JID.
fn assert_d7_refusal(offered: StanzaOffer, from: &str) {
    let (reason, reply) = refused(offered);
    assert_eq!(reason, UnroutableReason::RefusedByPolicy);
    assert_eq!(reply.attr("type"), Some("error"));
    assert_eq!(reply.attr("from"), Some(from));
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("service-unavailable")
    );
}

#[tokio::test]
async fn a_local_resource_joins_a_remote_room_and_receives_its_presence() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // The join left with the muc element, from the bound full JID.
    let joins = muc.received_with_id("presence", "join");
    assert_eq!(joins.len(), 1);
    assert_eq!(joins[0].attr("from"), Some(ALICE_PHONE));
    assert_eq!(joins[0].attr("to"), Some(ROOM_ALICE));
    assert!(joins[0]
        .children()
        .any(|child| child.is("x", "http://jabber.org/protocol/muc")));
    assert_eq!(muc.nick_of(ROOM, ALICE_PHONE).as_deref(), Some("alice"));

    // XEP-0045 §6.2 and §6.4: before the join, the server asked the room's
    // service domain whether it is a multi-user chat service, and then the
    // room's bare JID whether it is a room, from its own domain and without
    // a node.
    let asked = muc.disco_requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].attr("to"), Some(MUC_B));
    assert_eq!(asked[1].attr("to"), Some(ROOM));
    for request in &asked {
        assert_eq!(request.attr("from"), Some(HOME_A));
        assert!(request.attr("id").is_some_and(|id| id.starts_with("room-")));
        let query = request
            .children()
            .find(|child| child.is("query", DISCO_INFO_NS))
            .expect("a disco#info query");
        assert_eq!(query.attr("node"), None);
    }

    // Other occupants' presence arrives too, and only for this resource.
    let host = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_HOST)
            .into_iter()
            .next()
    })
    .await;
    assert!(host.is_some(), "the host's presence reaches the occupant");
    assert!(delivered_presence(&a, ALICE_LAPTOP, ROOM_HOST).is_empty());
    assert_eq!(a.router.remote_rooms().await, 1);
}

#[tokio::test]
async fn groupchat_messages_reach_the_room_and_its_reflection_reaches_the_occupant() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "g1",
            "hello room",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let sent = wait_for(ARRIVES, || {
        muc.received_with_id("message", "g1").into_iter().next()
    })
    .await
    .expect("the groupchat message reaches the room");
    assert_eq!(sent.attr("from"), Some(ALICE_PHONE));
    assert_eq!(sent.attr("type"), Some("groupchat"));

    let reflected = wait_for(ARRIVES, || {
        delivered_messages(&a, ALICE_PHONE, "g1").into_iter().next()
    })
    .await
    .expect("the room's reflection reaches the occupant");
    assert_eq!(reflected.from, Some(jid(ROOM_ALICE)));
    assert_eq!(reflected.type_, MessageType::Groupchat);
}

#[tokio::test]
async fn subject_and_private_messages_from_the_room_reach_the_occupant_only() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // XEP-0045 §7.2.15: the subject, from the room's bare JID.
    let mut subject = message(ROOM, ALICE_PHONE, MessageType::Groupchat, "s1", "");
    subject.bodies.clear();
    subject.subjects.insert(
        xmpp_parsers::message::Lang(String::new()),
        "Today".to_owned(),
    );
    muc.send(subject);
    // XEP-0045 §7.5: a private message from another occupant.
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "pm1",
        "psst",
    ));
    // The same to a resource of the account that never joined.
    muc.send(message(
        ROOM_HOST,
        ALICE_LAPTOP,
        MessageType::Chat,
        "pm2",
        "psst",
    ));

    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "s1")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the subject reaches the occupant"
    );
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "pm1")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the private message reaches the occupant"
    );
    // The resource that never joined is refused as D7 refuses any account
    // traffic, and nothing reaches it.
    let refusal = wait_for(ARRIVES, || {
        muc.received_with_id("message", "pm2").into_iter().next()
    })
    .await
    .expect("the room is told");
    assert_eq!(refusal.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(delivered_messages(&a, ALICE_LAPTOP, "pm2").is_empty());
}

#[tokio::test]
async fn the_rooms_messages_lose_what_only_the_occupants_server_states() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // Groupchat from an occupant, a private message from one, and the
    // subject from the room's bare JID (XEP-0045 §7.4, §7.5, §7.2.15),
    // each carrying statements only the occupant's server makes.
    muc.send(forging(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Groupchat,
        "forged-g",
        "hello",
    )));
    muc.send(forging(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "forged-pm",
        "psst",
    )));
    let mut subject = forging(message(
        ROOM,
        ALICE_PHONE,
        MessageType::Groupchat,
        "forged-s",
        "",
    ));
    subject.bodies.clear();
    subject.subjects.insert(
        xmpp_parsers::message::Lang(String::new()),
        "Today".to_owned(),
    );
    muc.send(subject);

    for id in ["forged-g", "forged-pm", "forged-s"] {
        let delivered = wait_for(ARRIVES, || {
            delivered_messages(&a, ALICE_PHONE, id).into_iter().next()
        })
        .await
        .unwrap_or_else(|| panic!("{id} reaches the occupant"));
        let element = Element::from(delivered.clone());
        assert!(forged_below(&element).is_empty(), "{id}: {element:?}");
        assert!(!has_archive_result(&delivered), "{id}: {element:?}");
        // What the room states in its own name, and the text, are kept.
        assert!(keeps_rooms_stanza_id(&delivered), "{id}: {element:?}");
        assert!(
            !delivered.bodies.is_empty() || !delivered.subjects.is_empty(),
            "{id}: {element:?}"
        );
    }
}

#[tokio::test]
async fn leaving_sends_unavailable_and_ends_the_occupancy_when_the_room_answers() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            ROOM_ALICE,
            PresenceType::Unavailable,
            "leave",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let own = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Unavailable)
    })
    .await
    .expect("the room's answer to the leave reaches the occupant");
    assert!(statuses(&own).contains(&"110".to_owned()));
    assert!(wait_for_rooms(&a.router, 0).await, "the occupancy ends");
    assert!(muc.nick_of(ROOM, ALICE_PHONE).is_none());

    // The room is no longer one the resource occupies.
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "after",
            "anyone?",
        )),
    )
    .await;
    assert_d7_refusal(offered, ROOM);
}

#[tokio::test]
async fn a_nickname_change_keeps_the_occupancy_under_the_new_nickname() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // XEP-0045 §7.6: presence without the muc element to the new occupant
    // JID.
    let offered = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            "room@muc.home-b.test/alicia",
            PresenceType::None,
            "nick",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let changed = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| statuses(presence).contains(&"303".to_owned()))
    })
    .await;
    assert!(changed.is_some(), "the old nickname's 303 arrives");
    assert!(
        wait_for(ARRIVES, || delivered_presence(
            &a,
            ALICE_PHONE,
            "room@muc.home-b.test/alicia"
        )
        .into_iter()
        .next())
        .await
        .is_some(),
        "the new nickname's own presence arrives"
    );
    assert_eq!(muc.nick_of(ROOM, ALICE_PHONE).as_deref(), Some("alicia"));
    // Still an occupant: room traffic keeps flowing.
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "pm",
        "hi",
    ));
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "pm")
            .into_iter()
            .next())
        .await
        .is_some()
    );
    assert_eq!(a.router.remote_rooms().await, 1);
}

#[tokio::test]
async fn joining_again_keeps_a_joined_occupancy_and_its_traffic() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    // A client resynchronising joins again; the room's answer is slow.
    muc.silence_joins();
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, "rejoin")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || muc
            .received_with_id("presence", "rejoin")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the join reaches the room"
    );
    // Room traffic sent meanwhile still reaches the occupant.
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Groupchat,
        "meanwhile",
        "still here",
    ));
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "meanwhile")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the room's traffic is still admitted"
    );
    assert!(muc.received_with_id("message", "meanwhile").is_empty());
    assert_eq!(a.router.remote_rooms().await, 1);
    // The service and the room are known: joining again asks neither.
    assert_eq!(muc.disco_requests().len(), 2);
}

#[tokio::test]
async fn a_join_the_room_refuses_is_delivered_and_forgotten() {
    let pki = support::Pki::new();
    let (a, _b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    let occupant = "busy@muc.home-b.test/alice";
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, occupant, "busy-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let error = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, occupant)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Error)
    })
    .await
    .expect("the room's refusal reaches the resource");
    assert_eq!(
        error_condition(&Element::from(error)).as_deref(),
        Some("conflict")
    );
    assert!(
        wait_for_rooms(&a.router, 0).await,
        "the refused join is forgotten"
    );
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            BUSY,
            MessageType::Groupchat,
            "busy-message",
            "hi",
        )),
    )
    .await;
    assert_d7_refusal(offered, BUSY);
}

#[tokio::test]
async fn traffic_from_a_room_the_resource_never_joined_is_refused() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;

    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Groupchat,
        "unjoined",
        "hello",
    ));
    let mut stray = presence(ROOM_HOST, ALICE_PHONE, PresenceType::None, "stray");
    stray.payloads.push(Element::from(
        xmpp_parsers::muc::user::MucUser::new().with_items(vec![
            xmpp_parsers::muc::user::Item::new(
                xmpp_parsers::muc::user::Affiliation::Owner,
                xmpp_parsers::muc::user::Role::Moderator,
            ),
        ]),
    ));
    muc.send(stray);
    let refusal = wait_for(ARRIVES, || {
        muc.received_with_id("message", "unjoined")
            .into_iter()
            .next()
    })
    .await
    .expect("the message is refused");
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    tokio::time::sleep(QUIET).await;
    // Presence is ignored without an answer (RFC 6121 §8.5.1).
    assert!(muc.received_with_id("presence", "stray").is_empty());
    assert!(a.delivery.delivered().is_empty(), "nothing reached alice");
    assert_eq!(a.router.remote_rooms().await, 0);
}

#[tokio::test]
async fn account_traffic_to_and_from_remote_jids_stays_refused() {
    let pki = support::Pki::new();
    let (a, b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // Outbound: a direct message, directed presence and a subscription
    // request to an account, and presence or messages for a room the
    // resource never joined.
    let refusals = [
        (
            Stanza::Message(message(ALICE_PHONE, BOB, MessageType::Chat, "d1", "hi")),
            BOB,
        ),
        (
            Stanza::Presence(presence(ALICE_PHONE, BOB_PHONE, PresenceType::None, "d2")),
            BOB_PHONE,
        ),
        (
            Stanza::Presence(presence(ALICE_PHONE, BOB, PresenceType::Subscribe, "d3")),
            BOB,
        ),
        (
            Stanza::Presence(presence(
                ALICE_PHONE,
                "other@muc.home-b.test/alice",
                PresenceType::None,
                "d4",
            )),
            "other@muc.home-b.test/alice",
        ),
        (
            Stanza::Message(message(
                ALICE_PHONE,
                "other@muc.home-b.test",
                MessageType::Groupchat,
                "d5",
                "hi",
            )),
            "other@muc.home-b.test",
        ),
        // A subscription to the joined room's own JID is account traffic.
        (
            Stanza::Presence(presence(ALICE_PHONE, ROOM, PresenceType::Subscribe, "d6")),
            ROOM,
        ),
        // A resource of the account that did not join the room.
        (
            Stanza::Message(message(
                ALICE_LAPTOP,
                ROOM,
                MessageType::Groupchat,
                "d7",
                "hi",
            )),
            ROOM,
        ),
    ];
    for (stanza, from) in refusals {
        assert_d7_refusal(offer(&a, stanza).await, from);
    }

    // Inbound: an account at the room's home domain is still refused, as
    // if the occupant did not exist.
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        message(BOB_PHONE, ALICE_PHONE, MessageType::Chat, "d8", "hi"),
    )
    .await;
    let refusal = message_refusal(&muc, "d8").await;
    assert_eq!(refusal.attr("from"), Some(ALICE_PHONE));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(delivered_messages(&a, ALICE_PHONE, "d8").is_empty());
    assert!(muc.received_with_id("message", "d1").is_empty());
    // None of it asked anything of the JIDs it was for or their domains:
    // only the room's join asked its service and the room.
    assert_eq!(muc.disco_requests().len(), 2);
}

#[tokio::test]
async fn a_join_to_an_account_is_refused_with_item_not_found_and_never_sent() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    // A "join" of an account's full JID is held while the account's domain
    // is asked whether it is a multi-user chat service.
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, BOB_PHONE, "fake-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    // The account's server names itself a server, not a chat service.
    let error = join_error(&a, ALICE_PHONE, BOB_PHONE, "fake-join").await;
    assert_eq!(
        join_error_condition(error).as_deref(),
        Some("item-not-found")
    );
    // Only the domain was asked: the account's bare JID never is.
    let asked = muc.disco_requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].attr("from"), Some(HOME_A));
    assert_eq!(asked[0].attr("to"), Some(HOME_B));
    assert!(muc.disco_requests_to(BOB).is_empty());
    assert!(muc.received_with_id("presence", "fake-join").is_empty());
    assert!(muc.joins(ALICE_PHONE, BOB).is_empty());
    assert_eq!(a.router.remote_rooms().await, 0);

    // The verdict is remembered for the domain: the next join to the same
    // account, and a join to another account there, are refused at once,
    // and the domain is not asked again.
    for (occupant, id) in [
        (BOB_PHONE, "fake-join-2"),
        ("carol@home-b.test/laptop", "fake-join-3"),
    ] {
        let (reason, reply) = refused(
            offer(
                &a,
                Stanza::Presence(join_presence(ALICE_PHONE, occupant, id)),
            )
            .await,
        );
        assert_eq!(reason, UnroutableReason::NotARoom);
        assert_eq!(reply.attr("type"), Some("error"));
        assert_eq!(reply.attr("id"), Some(id));
        assert_eq!(error_condition(&reply).as_deref(), Some("item-not-found"));
    }
    tokio::time::sleep(QUIET).await;
    assert_eq!(muc.disco_requests().len(), 1);
    assert!(muc.joins(ALICE_PHONE, BOB).is_empty());
    assert!(muc.joins(ALICE_PHONE, "carol@home-b.test").is_empty());
    assert_eq!(a.router.remote_rooms().await, 0);

    // The verdict is the domain's alone: the room service and the room are
    // asked on their own and a join to the room is sent.
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert_eq!(muc.disco_requests_to(ROOM).len(), 1);
    assert_eq!(muc.disco_requests_to(HOME_B).len(), 1);
}

#[tokio::test]
async fn a_service_whose_disco_is_answered_with_an_error_is_not_a_service() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    muc.disco_error(ErrorType::Cancel, DefinedCondition::ServiceUnavailable);
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, "nr-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let error = join_error(&a, ALICE_PHONE, ROOM_ALICE, "nr-join").await;
    assert_eq!(
        join_error_condition(error).as_deref(),
        Some("item-not-found")
    );
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
    assert_eq!(a.router.remote_rooms().await, 0);

    // Remembered for the service: a join to another of its rooms is
    // refused at once without asking again.
    let (reason, reply) = refused(
        offer(
            &a,
            Stanza::Presence(join_presence(
                ALICE_PHONE,
                "other@muc.home-b.test/alice",
                "nr-join-2",
            )),
        )
        .await,
    );
    assert_eq!(reason, UnroutableReason::NotARoom);
    assert_eq!(reply.attr("id"), Some("nr-join-2"));
    assert_eq!(error_condition(&reply).as_deref(), Some("item-not-found"));
    tokio::time::sleep(QUIET).await;
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert_eq!(muc.disco_requests().len(), 1);
    assert!(muc.joins(ALICE_PHONE, "other@muc.home-b.test").is_empty());
}

#[tokio::test]
async fn a_temporary_disco_error_answers_the_join_with_remote_server_timeout_and_is_not_kept() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    // RFC 6120 §8.3.2: an error of type `wait`, or a remote-server
    // condition about the route, says nothing about the service.
    let errors = [
        (
            ErrorType::Wait,
            DefinedCondition::ResourceConstraint,
            "ov-1",
        ),
        (
            ErrorType::Cancel,
            DefinedCondition::RemoteServerNotFound,
            "ov-2",
        ),
        (
            ErrorType::Cancel,
            DefinedCondition::RemoteServerTimeout,
            "ov-3",
        ),
    ];
    for (attempt, (type_, condition, id)) in errors.into_iter().enumerate() {
        muc.disco_error(type_, condition);
        // Not remembered: each join asks again.
        let offered = offer(
            &a,
            Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, id)),
        )
        .await;
        assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
        let error = join_error(&a, ALICE_PHONE, ROOM_ALICE, id).await;
        assert_eq!(
            join_error_condition(error).as_deref(),
            Some("remote-server-timeout")
        );
        assert_eq!(muc.disco_requests_to(MUC_B).len(), attempt + 1);
    }
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
    assert!(wait_for_rooms(&a.router, 0).await);
}

#[tokio::test]
async fn an_account_cannot_reach_a_resource_whose_fake_join_was_refused() {
    let pki = support::Pki::new();
    let (a, b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, BOB_PHONE, "fake-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    join_error(&a, ALICE_PHONE, BOB_PHONE, "fake-join").await;

    // The account answers with what a room would send: a message and its
    // own presence with muc#user. Both are account traffic.
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        forged_room_presence(BOB_PHONE, ALICE_PHONE, "forged"),
    )
    .await;
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        message(BOB_PHONE, ALICE_PHONE, MessageType::Chat, "chat", "hi"),
    )
    .await;
    let refusal = message_refusal(&muc, "chat").await;
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(delivered_messages(&a, ALICE_PHONE, "chat").is_empty());
    // Presence is dropped without an answer (RFC 6121 §8.5.1); only the
    // join's error presence reached alice from that JID.
    assert!(muc.received_with_id("presence", "forged").is_empty());
    let from_bob = delivered_presence(&a, ALICE_PHONE, BOB_PHONE);
    assert_eq!(from_bob.len(), 1, "{from_bob:?}");
    assert_eq!(from_bob[0].type_, PresenceType::Error);
    assert_eq!(a.router.remote_rooms().await, 0);
}

#[tokio::test]
async fn an_account_cannot_reach_a_resource_while_its_fake_join_waits_or_after_it_times_out() {
    let pki = support::Pki::new();
    let limits = RouterLimits {
        verification_timeout: Duration::from_secs(2),
        ..swept()
    };
    let (a, b, muc) = outbound_muc(&pki, limits).await;
    // The account's server never says whether it is a multi-user chat
    // service.
    muc.silence_disco();
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, BOB_PHONE, "silent-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || muc.disco_requests_to(HOME_B).into_iter().next())
            .await
            .is_some(),
        "the account's domain is asked"
    );

    // While the join is held, nothing from the JID is admitted.
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        forged_room_presence(BOB_PHONE, ALICE_PHONE, "forged"),
    )
    .await;
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        message(BOB_PHONE, ALICE_PHONE, MessageType::Chat, "held", "hi"),
    )
    .await;
    let refusal = message_refusal(&muc, "held").await;
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );

    // No answer within the verification timeout.
    let error = join_error(&a, ALICE_PHONE, BOB_PHONE, "silent-join").await;
    assert_eq!(
        join_error_condition(error).as_deref(),
        Some("remote-server-timeout")
    );
    assert!(wait_for_rooms(&a.router, 0).await);
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        message(BOB_PHONE, ALICE_PHONE, MessageType::Chat, "after", "hi"),
    )
    .await;
    message_refusal(&muc, "after").await;
    for id in ["held", "after"] {
        assert!(delivered_messages(&a, ALICE_PHONE, id).is_empty());
    }
    let from_bob = delivered_presence(&a, ALICE_PHONE, BOB_PHONE);
    assert_eq!(from_bob.len(), 1, "only the join's error: {from_bob:?}");
    assert!(muc.joins(ALICE_PHONE, BOB).is_empty());
}

#[tokio::test]
async fn leaving_while_the_rooms_service_is_verified_sends_nothing_and_forgets_the_join() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    muc.silence_disco();
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, "held")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || muc.disco_requests_to(MUC_B).into_iter().next())
            .await
            .is_some(),
        "the room's service is asked"
    );
    assert_eq!(a.router.remote_rooms().await, 1);
    let offered = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            ROOM_ALICE,
            PresenceType::Unavailable,
            "leave",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert_eq!(a.router.remote_rooms().await, 0);
    tokio::time::sleep(QUIET).await;
    assert!(
        muc.received()
            .iter()
            .all(|stanza| stanza.name() != "presence"),
        "neither the join nor the leave reached the room"
    );
}

#[tokio::test]
async fn a_challenge_from_the_rooms_bare_jid_reaches_a_joining_occupant() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    muc.challenge_joins();
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, "join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    // The room answers the join with a challenge from its bare JID before
    // it seats the occupant.
    let challenge = wait_for(ARRIVES, || {
        delivered_messages(&a, ALICE_PHONE, CHALLENGE)
            .into_iter()
            .next()
    })
    .await
    .expect("the challenge reaches the joining occupant");
    assert_eq!(challenge.from, Some(jid(ROOM)));

    // Until it is seated, an occupant's message is not admitted and the
    // resource may not send messages to the room.
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "early",
        "psst",
    ));
    let refusal = message_refusal(&muc, "early").await;
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(delivered_messages(&a, ALICE_PHONE, "early").is_empty());
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "too-soon",
            "hello?",
        )),
    )
    .await;
    assert_d7_refusal(offered, ROOM);
    assert!(muc.received_with_id("message", "too-soon").is_empty());

    // Once seated, both directions open.
    muc.pass_challenge(ROOM, ALICE_PHONE);
    assert!(
        wait_for(ARRIVES, || delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| statuses(presence).contains(&"110".to_owned())))
        .await
        .is_some(),
        "the room seats the occupant"
    );
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "seated",
            "hello",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "late",
        "welcome",
    ));
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "late")
            .into_iter()
            .next())
        .await
        .is_some(),
        "an occupant's message reaches the seated occupant"
    );
}

#[tokio::test]
async fn a_join_the_room_never_answers_ends_with_remote_server_timeout() {
    let pki = support::Pki::new();
    let limits = RouterLimits {
        join_timeout: Duration::from_secs(1),
        ..swept()
    };
    let (a, _b, muc) = outbound_muc(&pki, limits).await;
    muc.silence_joins();
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, "slow")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || muc.joins(ALICE_PHONE, ROOM).into_iter().next())
            .await
            .is_some(),
        "the join reaches the room"
    );
    let error = join_error(&a, ALICE_PHONE, ROOM_ALICE, "slow").await;
    assert_eq!(
        join_error_condition(error).as_deref(),
        Some("remote-server-timeout")
    );
    assert!(wait_for_rooms(&a.router, 0).await, "the join is forgotten");
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "too-late",
            "hello?",
        )),
    )
    .await;
    assert_d7_refusal(offered, ROOM);
}

#[tokio::test]
async fn leaving_and_joining_again_before_the_room_answers_keeps_the_new_join() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    // The room answers the leave only once the rejoin has reached it, then
    // answers both in order.
    muc.defer_leaves();
    for stanza in [
        presence(ALICE_PHONE, ROOM_ALICE, PresenceType::Unavailable, "leave"),
        join_presence(ALICE_PHONE, ROOM_ALICE, "rejoin"),
    ] {
        let offered = offer(&a, Stanza::Presence(stanza)).await;
        assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    }
    let rejoined = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| {
                presence.type_ == PresenceType::None && presence.id.as_deref() == Some("rejoin")
            })
    })
    .await
    .expect("the room's answer to the rejoin arrives");
    assert!(statuses(&rejoined).contains(&"110".to_owned()));
    // The leave's answer came first and was delivered without ending the
    // new join.
    let left = delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
        .into_iter()
        .find(|presence| presence.type_ == PresenceType::Unavailable)
        .expect("the room's answer to the leave arrives");
    assert!(statuses(&left).contains(&"110".to_owned()));
    assert_eq!(a.router.remote_rooms().await, 1);
    assert_eq!(muc.nick_of(ROOM, ALICE_PHONE).as_deref(), Some("alice"));
    // The rejoin asked neither the service nor the room again.
    assert_eq!(muc.disco_requests().len(), 2);

    // Joined: room traffic flows both ways.
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "back",
            "back again",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "back")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the room's reflection reaches the occupant"
    );
}

#[tokio::test]
async fn joins_beyond_the_bound_per_resource_are_refused_with_resource_constraint() {
    let pki = support::Pki::new();
    let limits = RouterLimits {
        rooms_per_resource: 1,
        ..RouterLimits::default()
    };
    let (a, _b, _muc) = outbound_muc(&pki, limits).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(
            ALICE_PHONE,
            "second@muc.home-b.test/alice",
            "second",
        )),
    )
    .await;
    let (reason, reply) = refused(offered);
    assert_eq!(reason, UnroutableReason::EgressBusy);
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("resource-constraint")
    );
    // Another resource has its own share.
    join(&a, ALICE_LAPTOP, "third@muc.home-b.test/alice2").await;
    assert_eq!(a.router.remote_rooms().await, 2);
}

#[tokio::test]
async fn a_join_beyond_the_bound_is_refused_before_its_service_is_asked() {
    let pki = support::Pki::new();
    let limits = RouterLimits {
        rooms_per_resource: 1,
        ..RouterLimits::default()
    };
    let (a, _b, muc) = outbound_muc(&pki, limits).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    // A join to a JID at a domain not yet asked, beyond the resource's
    // bound: refused at once, and its domain is never asked.
    let occupant = "lobby@home-b.test/alice";
    let (reason, reply) = refused(
        offer(
            &a,
            Stanza::Presence(join_presence(ALICE_PHONE, occupant, "over")),
        )
        .await,
    );
    assert_eq!(reason, UnroutableReason::EgressBusy);
    assert_eq!(reply.attr("id"), Some("over"));
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("resource-constraint")
    );
    tokio::time::sleep(QUIET).await;
    assert!(muc.disco_requests_to(HOME_B).is_empty());
    assert_eq!(muc.disco_requests().len(), 2);
    assert!(muc.joins(ALICE_PHONE, "lobby@home-b.test").is_empty());
    assert!(delivered_presence(&a, ALICE_PHONE, occupant).is_empty());
    assert_eq!(a.router.remote_rooms().await, 1);
}

#[tokio::test]
async fn a_join_to_a_room_that_does_not_exist_yet_at_a_verified_service_is_sent() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    // XEP-0045 §10.1: a join to a room the service does not have yet
    // creates it. The room answers its own `disco#info` with
    // `item-not-found` (§6.4); at a verified service that is a room the
    // join creates.
    let new_room = "newroom@muc.home-b.test";
    assert!(muc.nick_of(new_room, ALICE_PHONE).is_none());
    join(&a, ALICE_PHONE, "newroom@muc.home-b.test/alice").await;
    assert_eq!(muc.joins(ALICE_PHONE, new_room).len(), 1);
    assert_eq!(muc.nick_of(new_room, ALICE_PHONE).as_deref(), Some("alice"));
    assert_eq!(a.router.remote_rooms().await, 2);
    // One question for the service covered both rooms; each room was
    // asked once.
    let asked = muc.disco_requests();
    assert_eq!(asked.len(), 3, "{asked:?}");
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert_eq!(muc.disco_requests_to(ROOM).len(), 1);
    assert_eq!(muc.disco_requests_to(new_room).len(), 1);
}

#[tokio::test]
async fn a_mam_query_to_a_joined_room_is_answered_with_its_results() {
    let pki = support::Pki::new();
    let (a, _b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let query = Iq::Set {
        from: Some(jid(ALICE_PHONE)),
        to: Some(jid(ROOM)),
        id: "mam1".into(),
        payload: Element::builder("query", MAM_NS)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
            .build(),
    };
    let offered = a.offer_iq(ROOM, query).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    // XEP-0313: the result arrives as a message from the room.
    let result = wait_for(ARRIVES, || {
        a.delivery
            .delivered()
            .into_iter()
            .find_map(|(to, stanza)| match stanza {
                Stanza::Message(message)
                    if to == full(ALICE_PHONE)
                        && message
                            .payloads
                            .iter()
                            .any(|payload| payload.is("result", MAM_NS)) =>
                {
                    Some(message)
                }
                _ => None,
            })
    })
    .await;
    assert!(result.is_some(), "the archived message arrives");
    let fin = wait_for(ARRIVES, || a.delivery.iqs("mam1").into_iter().next())
        .await
        .expect("the query is answered");
    assert_eq!(fin.0, full(ALICE_PHONE));
    assert!(matches!(fin.1, Iq::Result { .. }));
}

#[tokio::test]
async fn an_archive_result_is_kept_only_from_the_rooms_bare_jid_and_what_it_forwards_is_cleaned() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // XEP-0313 §4.2: the room's archive answers with results from its
    // bare JID; what one forwards carries forged statements of its own.
    let mut archived = message(
        ROOM_HOST,
        ROOM,
        MessageType::Groupchat,
        "archived-1",
        "from the archive",
    );
    archived.payloads.extend(forged_statements());
    archived.payloads.push(stanza_id(ROOM, "archived-room-id"));
    let forwarded = Element::builder("forwarded", xmpp_parsers::ns::FORWARD)
        .append(Element::from(archived))
        .build();
    let result = |queryid: &str| {
        Element::builder("result", MAM_NS)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), queryid)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "archived-room-id",
            )
            .append(forwarded.clone())
            .build()
    };
    let mut from_room = message(ROOM, ALICE_PHONE, MessageType::Normal, "result-room", "");
    from_room.bodies.clear();
    from_room.payloads.push(result("q1"));
    muc.send(from_room);
    // The same from an occupant's JID: no archive answers from there.
    let mut from_occupant = message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "result-occupant",
        "not an archive",
    );
    from_occupant.payloads.push(result("q2"));
    muc.send(from_occupant);

    let kept = wait_for(ARRIVES, || {
        delivered_messages(&a, ALICE_PHONE, "result-room")
            .into_iter()
            .next()
    })
    .await
    .expect("the room's archive result reaches the occupant");
    let kept_element = Element::from(kept.clone());
    assert!(has_archive_result(&kept), "{kept_element:?}");
    // The forwarded message keeps its text and the room's stanza id, and
    // loses everything forged.
    assert!(forged_below(&kept_element).is_empty(), "{kept_element:?}");
    let inner = kept
        .payloads
        .iter()
        .find(|payload| payload.is("result", MAM_NS))
        .and_then(|result| result.get_child("forwarded", xmpp_parsers::ns::FORWARD))
        .and_then(|forwarded| forwarded.children().find(|child| child.name() == "message"))
        .expect("the result forwards the archived message");
    assert!(
        inner
            .children()
            .any(|child| child.name() == "body" && child.text() == "from the archive"),
        "{inner:?}"
    );
    assert!(
        !inner.children().any(|child| child.is("result", MAM_NS)),
        "{inner:?}"
    );
    assert!(
        inner.children().any(|child| {
            child.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
                && child.attr("by") == Some(ROOM)
        }),
        "{inner:?}"
    );

    let dropped = wait_for(ARRIVES, || {
        delivered_messages(&a, ALICE_PHONE, "result-occupant")
            .into_iter()
            .next()
    })
    .await
    .expect("the occupant's message reaches the occupant");
    assert!(
        !has_archive_result(&dropped),
        "{:?}",
        Element::from(dropped.clone())
    );
}

#[tokio::test]
async fn a_failed_stream_to_the_rooms_domain_removes_the_occupant_with_status_333() {
    let pki = support::Pki::new();
    let (a, b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // The peer shuts down: its streams end with `system-shutdown`, a
    // stream error, so stanzas in flight may have been lost.
    let _ = b.service.shutdown(Duration::from_secs(5)).await;
    let removed = wait_for(STREAM_LOSS, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Unavailable)
    })
    .await
    .expect("the occupant is told it left the room");
    let codes = statuses(&removed);
    assert!(codes.contains(&"110".to_owned()), "{codes:?}");
    assert!(codes.contains(&"333".to_owned()), "{codes:?}");
    assert!(!codes.contains(&"307".to_owned()), "{codes:?}");
    assert_eq!(a.router.remote_rooms().await, 0);
    // Only once: the second stream of the pair finds nothing left.
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .filter(|presence| presence.type_ == PresenceType::Unavailable)
            .count(),
        1
    );
}

#[tokio::test]
async fn a_resource_that_ends_leaves_its_remote_rooms() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    assert_eq!(a.router.remote_rooms().await, 1);

    a.router.resource_ended(&full(ALICE_PHONE)).await;
    assert_eq!(a.router.remote_rooms().await, 0);
    let left = wait_for(ARRIVES, || {
        muc.received().into_iter().find(|stanza| {
            stanza.name() == "presence" && stanza.attr("type") == Some("unavailable")
        })
    })
    .await
    .expect("the room receives the leave");
    assert_eq!(left.attr("from"), Some(ALICE_PHONE));
    assert_eq!(left.attr("to"), Some(ROOM_ALICE));
    assert!(
        wait_for(ARRIVES, || muc
            .nick_of(ROOM, ALICE_PHONE)
            .is_none()
            .then_some(()))
        .await
        .is_some(),
        "the room no longer seats the resource"
    );
}

#[tokio::test]
async fn a_join_routed_by_one_replica_is_completed_by_the_replica_the_room_dials() {
    let pki = support::Pki::new();
    let (a1, a2, _b, muc) = outbound_muc_replicas(&pki, swept()).await;
    // The first replica routes the join; the room's server dials the
    // second, which completes the service's and then the room's
    // verification, records the verdicts both share and admits the room's
    // answer. The first moves the join on to the room once it reads the
    // service's verdict, and sends it once it reads the room's.
    join_through(&a1, &a2, ALICE_PHONE, ROOM_ALICE).await;
    tokio::time::sleep(QUIET).await;
    let asked = muc.disco_requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].attr("to"), Some(MUC_B));
    assert_eq!(asked[1].attr("to"), Some(ROOM));
    let joins = muc.joins(ALICE_PHONE, ROOM);
    assert_eq!(joins.len(), 1, "the join reaches the room once: {joins:?}");
    assert!(
        a1.delivery.delivered().is_empty(),
        "{:?}",
        a1.delivery.delivered()
    );
    assert_eq!(a1.router.remote_rooms().await, 1);
    assert_eq!(a2.router.remote_rooms().await, 1);

    // Either replica routes the occupant's messages; the room's reflection
    // arrives at the second.
    let offered = offer(
        &a1,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "r1",
            "from the first replica",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let reflected = wait_for(ARRIVES, || {
        delivered_messages(&a2, ALICE_PHONE, "r1")
            .into_iter()
            .next()
    })
    .await
    .expect("the reflection is delivered by the replica the room dials");
    assert_eq!(reflected.from, Some(jid(ROOM_ALICE)));
    assert!(delivered_messages(&a1, ALICE_PHONE, "r1").is_empty());
}

#[tokio::test]
async fn a_resource_that_ends_on_another_replica_leaves_its_remote_rooms() {
    let pki = support::Pki::new();
    let (a1, a2, _b, muc) = outbound_muc_replicas(&pki, swept()).await;
    join_through(&a1, &a2, ALICE_PHONE, ROOM_ALICE).await;
    assert_eq!(a1.router.remote_rooms().await, 1);

    // The replica that did not route the join ends the resource.
    a2.router.resource_ended(&full(ALICE_PHONE)).await;
    assert_eq!(a1.router.remote_rooms().await, 0);
    assert_eq!(a2.router.remote_rooms().await, 0);
    let left = wait_for(ARRIVES, || {
        muc.received().into_iter().find(|stanza| {
            stanza.name() == "presence" && stanza.attr("type") == Some("unavailable")
        })
    })
    .await
    .expect("the room receives the leave");
    assert_eq!(left.attr("from"), Some(ALICE_PHONE));
    assert_eq!(left.attr("to"), Some(ROOM_ALICE));
    assert!(
        wait_for(ARRIVES, || muc
            .nick_of(ROOM, ALICE_PHONE)
            .is_none()
            .then_some(()))
        .await
        .is_some(),
        "the room no longer seats the resource"
    );
    // Nothing is left for either replica to route.
    let offered = offer(
        &a1,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "gone",
            "anyone?",
        )),
    )
    .await;
    assert_d7_refusal(offered, ROOM);
}

/// Offer a join from `from` to `occupant` with `id` and wait for its error
/// presence: the condition it carries.
async fn refused_join(a: &support::Routed, from: &str, occupant: &str, id: &str) -> Option<String> {
    let offered = offer(a, Stanza::Presence(join_presence(from, occupant, id))).await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    join_error_condition(join_error(a, from, occupant, id).await)
}

#[tokio::test]
async fn a_domain_naming_a_server_identity_with_the_muc_feature_is_not_a_service() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    // A server that lists the MUC feature on its own domain hosts
    // accounts there, not rooms.
    muc.answer_service_as(&[("server", "im")], &[DISCO_INFO_NS, NS_MUC]);
    assert_eq!(
        refused_join(&a, ALICE_PHONE, ROOM_ALICE, "server-im")
            .await
            .as_deref(),
        Some("item-not-found")
    );
    tokio::time::sleep(QUIET).await;
    // The room's bare JID is never asked, and nothing reaches it.
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert!(muc.disco_requests_to(ROOM).is_empty());
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
    assert_eq!(a.router.remote_rooms().await, 0);
}

#[tokio::test]
async fn a_domain_naming_a_server_identity_beside_the_conference_one_is_not_a_service() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    muc.answer_service_as(
        &[("conference", "text"), ("server", "im")],
        &[DISCO_INFO_NS, NS_MUC],
    );
    assert_eq!(
        refused_join(&a, ALICE_PHONE, ROOM_ALICE, "both")
            .await
            .as_deref(),
        Some("item-not-found")
    );
    tokio::time::sleep(QUIET).await;
    assert!(muc.disco_requests_to(ROOM).is_empty());
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
    assert_eq!(a.router.remote_rooms().await, 0);
}

#[tokio::test]
async fn a_service_naming_the_conference_identity_without_the_muc_feature_is_not_a_service() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    muc.answer_service_as(&[("conference", "text")], &[DISCO_INFO_NS]);
    assert_eq!(
        refused_join(&a, ALICE_PHONE, ROOM_ALICE, "no-feature")
            .await
            .as_deref(),
        Some("item-not-found")
    );
    tokio::time::sleep(QUIET).await;
    assert!(muc.disco_requests_to(ROOM).is_empty());
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
}

#[tokio::test]
async fn a_jid_answering_as_an_account_or_a_client_at_a_verified_service_is_not_a_room() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    let cases = [
        ("bob@muc.home-b.test", "account", "registered", "as-account"),
        ("desk@muc.home-b.test", "client", "pc", "as-client"),
        ("relay@muc.home-b.test", "gateway", "irc", "as-gateway"),
    ];
    for (jid, category, type_, _) in cases {
        muc.answer_jid_as(jid, category, type_);
    }
    for (jid, _, _, id) in cases {
        let occupant = [jid, "/alice"].concat();
        assert_eq!(
            refused_join(&a, ALICE_PHONE, &occupant, id)
                .await
                .as_deref(),
            Some("item-not-found"),
            "{jid}"
        );
        // The service was verified, then the JID itself was asked.
        assert_eq!(muc.disco_requests_to(jid).len(), 1, "{jid}");
        assert!(muc.joins(ALICE_PHONE, jid).is_empty(), "{jid}");
    }
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert_eq!(a.router.remote_rooms().await, 0);

    // Remembered per JID: a second join is refused at once without asking
    // again, while a room at the same service is joined.
    let (reason, reply) = refused(
        offer(
            &a,
            Stanza::Presence(join_presence(
                ALICE_PHONE,
                "bob@muc.home-b.test/alice",
                "as-account-2",
            )),
        )
        .await,
    );
    assert_eq!(reason, UnroutableReason::NotARoom);
    assert_eq!(error_condition(&reply).as_deref(), Some("item-not-found"));
    assert_eq!(muc.disco_requests_to("bob@muc.home-b.test").len(), 1);
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert_eq!(muc.disco_requests_to(ROOM).len(), 1);
}

#[tokio::test]
async fn a_room_refusing_its_disco_with_another_error_is_not_joined() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    // Only `item-not-found` stands for a room a join creates; any other
    // refusal leaves the JID unproven.
    muc.answer_jid_with_error(ROOM, ErrorType::Auth, DefinedCondition::Forbidden);
    assert_eq!(
        refused_join(&a, ALICE_PHONE, ROOM_ALICE, "forbidden")
            .await
            .as_deref(),
        Some("item-not-found")
    );
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
    assert_eq!(a.router.remote_rooms().await, 0);
}

#[tokio::test]
async fn a_temporary_error_from_the_room_answers_the_join_with_remote_server_timeout() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    // The service answers; the room says it cannot answer for now.
    muc.answer_jid_with_error(ROOM, ErrorType::Wait, DefinedCondition::ResourceConstraint);
    assert_eq!(
        refused_join(&a, ALICE_PHONE, ROOM_ALICE, "wait")
            .await
            .as_deref(),
        Some("remote-server-timeout")
    );
    assert!(muc.joins(ALICE_PHONE, ROOM).is_empty());
    assert!(wait_for_rooms(&a.router, 0).await);
}

#[tokio::test]
async fn verdicts_are_asked_again_once_they_expire() {
    let pki = support::Pki::new();
    let limits = RouterLimits {
        room_verdict_ttl: Duration::from_secs(1),
        not_room_verdict_ttl: Duration::from_secs(1),
        ..RouterLimits::default()
    };
    let (a, _b, muc) = outbound_muc(&pki, limits).await;
    // "Not a service", for the account's domain.
    assert_eq!(
        refused_join(&a, ALICE_PHONE, BOB_PHONE, "first")
            .await
            .as_deref(),
        Some("item-not-found")
    );
    // "A service" and "a room", for the room.
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    let left = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            ROOM_ALICE,
            PresenceType::Unavailable,
            "leave",
        )),
    )
    .await;
    assert!(matches!(left, StanzaOffer::Accepted), "{left:?}");
    assert!(wait_for_rooms(&a.router, 0).await, "the leave is answered");
    assert_eq!(muc.disco_requests_to(HOME_B).len(), 1);
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 1);
    assert_eq!(muc.disco_requests_to(ROOM).len(), 1);

    // Past both TTLs, each is asked again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        refused_join(&a, ALICE_PHONE, BOB_PHONE, "second")
            .await
            .as_deref(),
        Some("item-not-found")
    );
    assert_eq!(muc.disco_requests_to(HOME_B).len(), 2);
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, ROOM_ALICE, "again")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| presence.id.as_deref() == Some("again")))
        .await
        .is_some(),
        "the room seats the occupant again"
    );
    assert_eq!(muc.disco_requests_to(MUC_B).len(), 2);
    assert_eq!(muc.disco_requests_to(ROOM).len(), 2);
}

#[tokio::test]
async fn a_room_with_more_occupants_than_its_queue_holds_delivers_every_presence_and_its_own_last()
{
    let pki = support::Pki::new();
    // Queues far smaller than the room's burst, so that the stream from the
    // room's service has to wait for them to drain.
    let limits = RouterLimits {
        room_deliveries: 16,
        room_deliveries_per_room: 4,
        room_deliveries_per_peer: 4,
        ..RouterLimits::default()
    };
    let (a, _b, muc) = outbound_muc(&pki, limits).await;
    const GUESTS: usize = 200;
    muc.crowd(ROOM, GUESTS);
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let room = jid(ROOM).to_bare();
    let from_room: Vec<Presence> = a
        .delivery
        .delivered()
        .into_iter()
        .filter_map(|(recipient, stanza)| match stanza {
            Stanza::Presence(presence)
                if recipient == full(ALICE_PHONE)
                    && presence
                        .from
                        .as_ref()
                        .is_some_and(|from| from.to_bare() == room) =>
            {
                Some(presence)
            }
            _ => None,
        })
        .collect();
    // The host, every guest in the room's order, and the occupant's own
    // presence (XEP-0045 §7.2.3), nothing dropped.
    assert_eq!(from_room.len(), GUESTS + 2, "{}", from_room.len());
    assert_eq!(from_room[0].from, Some(jid(ROOM_HOST)));
    for (index, presence) in from_room[1..=GUESTS].iter().enumerate() {
        let expected = [ROOM, "/", &support::guest(index)].concat();
        assert_eq!(presence.from, Some(jid(&expected)));
        assert!(!statuses(presence).contains(&"110".to_owned()));
    }
    let own = &from_room[GUESTS + 1];
    assert_eq!(own.from, Some(jid(ROOM_ALICE)));
    assert!(statuses(own).contains(&"110".to_owned()));

    // The join completed: a message, which leaves only once joined, does.
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "crowded",
            "hello everyone",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert_eq!(a.router.remote_rooms().await, 1);
}

/// A XEP-0199 ping from `from` to the server `home-a.test`.
fn server_ping(from: &str, id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(HOME_A)),
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

/// Whether `home-b.test` has received the result of its ping `id`.
fn ping_answered(muc: &support::FakeMuc, id: &str) -> bool {
    muc.received_with_id("iq", id)
        .iter()
        .any(|iq| iq.attr("type") == Some("result"))
}

#[test]
fn one_remote_domain_holds_at_most_a_quarter_of_a_delivery_workers_queue() {
    let limits = RouterLimits::default();
    let per_worker = limits.room_deliveries / limits.room_delivery_workers;
    assert!(
        limits.room_deliveries_per_peer * 4 <= per_worker,
        "{} of {per_worker}",
        limits.room_deliveries_per_peer
    );
}

#[tokio::test]
async fn a_remote_domain_at_its_share_of_the_delivery_queue_waits_alone() {
    let pki = support::Pki::new();
    // One worker, so that every room shares its queue, and a share for each
    // remote domain of a quarter of it.
    let limits = RouterLimits {
        room_delivery_workers: 1,
        room_deliveries: 8,
        room_deliveries_per_room: 8,
        room_deliveries_per_peer: 2,
        room_delivery_timeout: Duration::from_secs(60),
        ..RouterLimits::default()
    };
    let (a, _b, muc) = outbound_muc(&pki, limits).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // The worker is held on the first of the room's messages, two more fill
    // `muc.home-b.test`'s share, and the fourth makes its stream wait, so
    // the ping behind it is not read.
    a.delivery.hold().await;
    const HELD: usize = 4;
    for index in 0..HELD {
        muc.send(message(
            ROOM_HOST,
            ALICE_PHONE,
            MessageType::Groupchat,
            &["held-", &index.to_string()].concat(),
            "while the occupant's session is slow",
        ));
    }
    muc.send(server_ping(ROOM_HOST, "behind-the-room"));

    // `home-b.test` has its own share of the same queue: its message is
    // queued at once and its stream reads on to the ping behind it.
    muc.send(message(
        BOB_PHONE,
        ALICE_PHONE,
        MessageType::Chat,
        "from-bob",
        "not room traffic",
    ));
    muc.send(server_ping(BOB_PHONE, "behind-bob"));
    assert!(
        wait_for(ARRIVES, || ping_answered(&muc, "behind-bob").then_some(()))
            .await
            .is_some(),
        "the other domain's stream waited for the room's share"
    );
    assert!(
        !ping_answered(&muc, "behind-the-room"),
        "the room's stream did not wait at its share"
    );

    // Once the session takes deliveries again, the room's stream reads on
    // and every message arrives in the room's order.
    a.delivery.release();
    assert!(
        wait_for(ARRIVES, || ping_answered(&muc, "behind-the-room")
            .then_some(()))
        .await
        .is_some(),
        "the room's stream did not read on"
    );
    let delivered: Vec<String> = a
        .delivery
        .delivered()
        .into_iter()
        .filter_map(|(recipient, stanza)| match stanza {
            Stanza::Message(message) if recipient == full(ALICE_PHONE) => message.id.map(|id| id.0),
            _ => None,
        })
        .filter(|id| id.starts_with("held-"))
        .collect();
    let expected: Vec<String> = (0..HELD)
        .map(|index| ["held-", &index.to_string()].concat())
        .collect();
    assert_eq!(delivered, expected);
    // Bob's message is not room traffic and is refused (D7).
    let refusal = message_refusal(&muc, "from-bob").await;
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
}

/// Limits allowing one account two remote rooms.
fn two_rooms_per_account() -> RouterLimits {
    RouterLimits {
        rooms_per_account: 2,
        ..RouterLimits::default()
    }
}

/// A schema of its own in the test PostgreSQL database, migrated, or `None`
/// when no test database is configured.
struct PostgresTables {
    admin: sqlx::PgPool,
    schema: String,
    database: Database,
}

impl PostgresTables {
    async fn open() -> Option<Self> {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping a PostgreSQL case: no test database is configured");
            return None;
        };
        let schema = ["s2s_remote_rooms_", &Uuid::new_v4().simple().to_string()].concat();
        let admin = sqlx::PgPool::connect(&url)
            .await
            .expect("an administrative pool");
        sqlx::query(&["CREATE SCHEMA ", &schema].concat())
            .execute(&admin)
            .await
            .expect("a schema of the case's own");
        let mut scoped = url::Url::parse(&url).expect("a PostgreSQL URL");
        let retained: Vec<(String, String)> = scoped
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        scoped
            .query_pairs_mut()
            .clear()
            .extend_pairs(retained)
            .append_pair("options", &["-c search_path=", &schema].concat());
        let database = Database::from_config(
            "s2s-remote-rooms-postgres",
            &DatabaseConfig::new(DatabaseDriver::Postgres, scoped.to_string()),
        )
        .await
        .expect("the case's database");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("the deployment migrations");
        Some(Self {
            admin,
            schema,
            database,
        })
    }

    async fn remove(self) {
        drop(self.database);
        sqlx::query(&["DROP SCHEMA ", &self.schema, " CASCADE"].concat())
            .execute(&self.admin)
            .await
            .expect("drop the case's schema");
    }
}

#[tokio::test]
async fn joins_beyond_the_bound_per_account_are_refused_whichever_resource_sends_them() {
    let pki = support::Pki::new();
    let (a, _b, _muc) = outbound_muc(&pki, two_rooms_per_account()).await;
    the_account_bound_holds_for_every_resource(&a).await;
}

#[tokio::test]
async fn joins_beyond_the_bound_per_account_are_refused_whichever_resource_sends_them_on_postgresql(
) {
    let Some(tables) = PostgresTables::open().await else {
        return;
    };
    let pki = support::Pki::new();
    let (a, b, muc) =
        support::outbound_muc_on(&pki, two_rooms_per_account(), tables.database.clone()).await;
    the_account_bound_holds_for_every_resource(&a).await;
    drop((a, b, muc));
    tables.remove().await;
}

#[tokio::test]
async fn concurrent_joins_of_one_account_hold_its_bound_exactly_on_postgresql() {
    let Some(tables) = PostgresTables::open().await else {
        return;
    };
    let pki = support::Pki::new();
    let (a, b, muc) =
        support::outbound_muc_on(&pki, two_rooms_per_account(), tables.database.clone()).await;
    // Four joins of one account from two resources at once, each to a room
    // never asked before: the account lock lets exactly two of them in.
    let joins = [
        (ALICE_PHONE, "one@muc.home-b.test/alice", "race-1"),
        (ALICE_LAPTOP, "two@muc.home-b.test/alice2", "race-2"),
        (ALICE_PHONE, "three@muc.home-b.test/alice", "race-3"),
        (ALICE_LAPTOP, "four@muc.home-b.test/alice2", "race-4"),
    ];
    let offer_join = |(from, occupant, id): (&str, &str, &str)| {
        offer(&a, Stanza::Presence(join_presence(from, occupant, id)))
    };
    let (first, second, third, fourth) = tokio::join!(
        offer_join(joins[0]),
        offer_join(joins[1]),
        offer_join(joins[2]),
        offer_join(joins[3]),
    );
    let mut accepted = 0;
    for offered in [first, second, third, fourth] {
        match offered {
            StanzaOffer::Accepted => accepted += 1,
            other => {
                let (reason, reply) = refused(other);
                assert_eq!(reason, UnroutableReason::EgressBusy);
                assert_eq!(
                    error_condition(&reply).as_deref(),
                    Some("resource-constraint")
                );
            }
        }
    }
    assert_eq!(accepted, 2);
    assert!(
        wait_for_rooms(&a.router, 2).await,
        "the two accepted joins are recorded"
    );
    assert_eq!(a.router.remote_rooms().await, 2);
    drop((a, b, muc));
    tables.remove().await;
}

/// With two rooms an account may occupy, a third join from either resource
/// is refused, another account is not affected and leaving frees a place.
async fn the_account_bound_holds_for_every_resource(a: &support::Routed) {
    join(a, ALICE_PHONE, ROOM_ALICE).await;
    join(a, ALICE_LAPTOP, "second@muc.home-b.test/alice2").await;
    // Each resource is within its own bound; the account is at its own.
    for (from, occupant, id) in [
        (ALICE_PHONE, "third@muc.home-b.test/alice", "over-phone"),
        (ALICE_LAPTOP, "third@muc.home-b.test/alice2", "over-laptop"),
    ] {
        let (reason, reply) =
            refused(offer(a, Stanza::Presence(join_presence(from, occupant, id))).await);
        assert_eq!(reason, UnroutableReason::EgressBusy);
        assert_eq!(reply.attr("id"), Some(id));
        assert_eq!(
            error_condition(&reply).as_deref(),
            Some("resource-constraint")
        );
    }
    assert_eq!(a.router.remote_rooms().await, 2);
    // Another account has its own bound.
    join(a, "carol@home-a.test/desk", "third@muc.home-b.test/carol").await;
    assert_eq!(a.router.remote_rooms().await, 3);
    // Leaving a room frees a place for the account.
    let offered = offer(
        a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            ROOM_ALICE,
            PresenceType::Unavailable,
            "leave",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(wait_for_rooms(&a.router, 2).await, "the leave is answered");
    join(a, ALICE_LAPTOP, "third@muc.home-b.test/alice2").await;
    assert_eq!(a.router.remote_rooms().await, 3);
}

/// Limits under which a replica that stops recording that it is alive is
/// taken for stopped after one second.
fn stopping() -> RouterLimits {
    RouterLimits {
        prune_after: Duration::from_secs(1),
        ..swept()
    }
}

#[tokio::test]
async fn occupancies_a_stopped_replica_left_are_ended_by_another_replica() {
    let pki = support::Pki::new();
    let (a1, a2, _b, muc) = outbound_muc_replicas(&pki, stopping()).await;
    join_through(&a1, &a2, ALICE_PHONE, ROOM_ALICE).await;
    // The replica that routed the join stops, and with it its sweep and
    // its liveness record.
    assert!(a1.router.shutdown(Duration::from_secs(5)).await);

    // The other replica ends the occupancy: the room receives the
    // resource's leave and the resource, whose session may live on, its
    // own unavailable presence with status 333.
    let removed = wait_for(ARRIVES, || {
        delivered_presence(&a2, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Unavailable)
    })
    .await
    .expect("the occupant is told it left the room");
    let codes = statuses(&removed);
    assert!(codes.contains(&"110".to_owned()), "{codes:?}");
    assert!(codes.contains(&"333".to_owned()), "{codes:?}");
    let left = wait_for(ARRIVES, || {
        muc.received().into_iter().find(|stanza| {
            stanza.name() == "presence" && stanza.attr("type") == Some("unavailable")
        })
    })
    .await
    .expect("the room receives the leave");
    assert_eq!(left.attr("from"), Some(ALICE_PHONE));
    assert_eq!(left.attr("to"), Some(ROOM_ALICE));
    assert!(wait_for_rooms(&a2.router, 0).await);
}

#[tokio::test]
async fn a_session_resumed_on_another_replica_keeps_its_rooms_when_the_first_stops() {
    let pki = support::Pki::new();
    let (a1, a2, _b, muc) = outbound_muc_replicas(&pki, stopping()).await;
    join_through(&a1, &a2, ALICE_PHONE, ROOM_ALICE).await;
    // The session resumes on the second replica, which adopts its rooms;
    // then the first stops.
    a2.router.resource_resumed(&full(ALICE_PHONE)).await;
    assert!(a1.router.shutdown(Duration::from_secs(5)).await);
    // Well past the time after which the first replica's own occupancies
    // would be taken for a stopped replica's.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(a2.router.remote_rooms().await, 1);
    assert!(
        delivered_presence(&a2, ALICE_PHONE, ROOM_ALICE)
            .iter()
            .all(|presence| presence.type_ != PresenceType::Unavailable),
        "the occupant was not removed"
    );
    assert!(
        muc.received().iter().all(|stanza| {
            stanza.name() != "presence" || stanza.attr("type") != Some("unavailable")
        }),
        "no leave reached the room"
    );
    assert_eq!(muc.nick_of(ROOM, ALICE_PHONE).as_deref(), Some("alice"));
    // The second replica routes the occupant's messages.
    let offered = offer(
        &a2,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "resumed",
            "still here",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a2, ALICE_PHONE, "resumed")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the room's reflection arrives"
    );
}
