//! What a stanza from an S2S stream may reach (ADR-0020 D10; gate G23):
//! only the server domain's service discovery and ping are answered for a
//! remote sender, a local resource's own pending request is completed, and
//! every other local service refuses: IQ requests and messages receive
//! `service-unavailable`, presence is ignored and no error is answered.
//! The MUC service, which D10 enables, is reached only on a replica that
//! hosts rooms for remote occupants, and only for the operations reviewed
//! for a remote principal; the Spaces PubSub service refuses until its
//! handlers are reviewed. The identity profile
//! applies to every address a stanza carries (gate G10): a refused spelling
//! is answered with `jid-malformed`, with the stanza's own id, except in
//! presence, which is dropped unanswered, and the stream stays open.
//!
//! The inventory these cases follow is recorded in
//! `server/docs/planning/s2s-routing-2047.md`. `home-a.test` runs the S2S
//! router for its account domain and several service domains;
//! `home-b.test` sends through its own connection manager, or as a
//! hand-written peer, and keeps what it receives. No PostgreSQL, SpiceDB or
//! second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    error_condition, jid, routed_and_capturing, send, wait_for, CapturingSink, RawPeer, ARRIVES,
    HOME_A, HOME_B, QUIET,
};
use waddle_server::destination::ServedService;
use waddle_server::s2s::route::{
    reach as reach_with, InboundReach, LocalRooms, LocalTarget, Reachable, Refusal, RoomOperation,
    RoomReplies, RoomRequest, RouterLimits, Seat, SeatEnd, SqlSeatStore,
};
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const BOB: &str = "bob@home-b.test/phone";
const MUC: &str = "muc.home-a.test";
const SPACES: &str = "spaces.home-a.test";
const UPLOAD: &str = "upload.home-a.test";
const PUSH: &str = "push.home-a.test";

/// What a stanza reaches on a replica that hosts no rooms for remote
/// occupants.
fn reach(target: LocalTarget, stanza: &Stanza) -> InboundReach {
    reach_with(target, stanza, Reachable::default())
}

fn get(to: &str, id: &str, payload: Element) -> Iq {
    Iq::Get {
        from: Some(jid(BOB)),
        to: Some(jid(to)),
        id: id.to_owned(),
        payload,
    }
}

fn query(namespace: &str) -> Element {
    Element::builder("query", namespace).build()
}

fn ping_payload() -> Element {
    Element::builder("ping", NS_PING).build()
}

fn chat(to: &str, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(jid(to)));
    message.from = Some(jid(BOB));
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message
}

fn available(to: &str, id: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(jid(BOB));
    presence.to = Some(jid(to));
    presence.id = Some(id.to_owned());
    presence
}

#[test]
fn the_reach_of_every_served_destination_follows_d10_and_d7() {
    let request = |payload: Element| {
        Stanza::Iq(Box::new(Iq::Get {
            from: Some(jid(BOB)),
            to: None,
            id: "r".into(),
            payload,
        }))
    };
    let set = Stanza::Iq(Box::new(Iq::Set {
        from: Some(jid(BOB)),
        to: None,
        id: "r".into(),
        payload: query("jabber:iq:roster"),
    }));
    let response = Stanza::Iq(Box::new(Iq::Result {
        from: Some(jid(BOB)),
        to: None,
        id: "r".into(),
        payload: None,
    }));
    let message = Stanza::Message(chat(HOME_A, "r"));
    let presence = Stanza::Presence(available(HOME_A, "r"));

    use InboundReach::{
        CorrelatedResponse, Refused, ServerDiscoInfo, ServerDiscoItems, ServerPing, ServerResponse,
    };
    let server = LocalTarget::ServerDomain;
    assert_eq!(
        reach(server, &request(query(DISCO_INFO_NS))),
        ServerDiscoInfo
    );
    assert_eq!(
        reach(server, &request(query(DISCO_ITEMS_NS))),
        ServerDiscoItems
    );
    assert_eq!(reach(server, &request(ping_payload())), ServerPing);
    // A response to the server domain completes only a room verification
    // this server sent; the router decides that against the correlation
    // table.
    assert_eq!(reach(server, &response), ServerResponse);
    assert_eq!(
        reach(LocalTarget::ServerResource, &response),
        Refused(Refusal::ServerRequest)
    );
    for stanza in [
        request(query("jabber:iq:version")),
        request(query("vcard-temp")),
        set.clone(),
        message.clone(),
        presence.clone(),
    ] {
        assert_eq!(reach(server, &stanza), Refused(Refusal::ServerRequest));
        assert_eq!(
            reach(LocalTarget::ServerResource, &stanza),
            Refused(Refusal::ServerRequest)
        );
    }
    assert_eq!(
        reach(LocalTarget::ServerResource, &request(ping_payload())),
        Refused(Refusal::ServerRequest)
    );

    assert_eq!(
        reach(LocalTarget::AccountResource, &response),
        CorrelatedResponse
    );
    for stanza in [
        request(ping_payload()),
        set.clone(),
        message.clone(),
        presence.clone(),
    ] {
        assert_eq!(
            reach(LocalTarget::AccountResource, &stanza),
            Refused(Refusal::AccountTraffic)
        );
    }
    for stanza in [
        request(ping_payload()),
        set.clone(),
        response.clone(),
        message.clone(),
        presence.clone(),
    ] {
        assert_eq!(
            reach(LocalTarget::AccountBare, &stanza),
            Refused(Refusal::AccountTraffic)
        );
    }

    for service in [ServedService::Muc, ServedService::Spaces] {
        for stanza in [
            request(query(DISCO_INFO_NS)),
            request(ping_payload()),
            set.clone(),
            response.clone(),
            message.clone(),
            presence.clone(),
        ] {
            assert_eq!(
                reach(LocalTarget::Service(service), &stanza),
                Refused(Refusal::AwaitingReview(service))
            );
        }
    }
    for service in [
        ServedService::Upload,
        ServedService::Community,
        ServedService::Extensions,
        ServedService::Push,
        ServedService::CallsMixer,
    ] {
        for stanza in [
            request(query(DISCO_INFO_NS)),
            request(ping_payload()),
            set.clone(),
            response.clone(),
            message.clone(),
            presence.clone(),
        ] {
            assert_eq!(
                reach(LocalTarget::Service(service), &stanza),
                Refused(Refusal::NotEnabled(service))
            );
        }
    }
}

const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MODERATE: &str = "urn:xmpp:message-moderate:1";

#[test]
fn the_muc_service_is_reached_only_where_rooms_are_hosted_and_only_for_reviewed_operations() {
    let hosting = Reachable { rooms: true };
    let muc = LocalTarget::Service(ServedService::Muc);
    let request = |payload: Element| {
        Stanza::Iq(Box::new(Iq::Get {
            from: Some(jid(BOB)),
            to: None,
            id: "r".into(),
            payload,
        }))
    };
    let set = |payload: Element| {
        Stanza::Iq(Box::new(Iq::Set {
            from: Some(jid(BOB)),
            to: None,
            id: "r".into(),
            payload,
        }))
    };
    let room = |operation| InboundReach::Room(operation);
    for (stanza, operation) in [
        (request(query(DISCO_INFO_NS)), RoomOperation::DiscoInfo),
        (request(query(DISCO_ITEMS_NS)), RoomOperation::DiscoItems),
        (request(ping_payload()), RoomOperation::Ping),
        (request(query("urn:xmpp:mam:2")), RoomOperation::Archive),
        (set(query("urn:xmpp:mam:2")), RoomOperation::Archive),
        (Stanza::Message(chat(MUC, "r")), RoomOperation::Message),
        (
            Stanza::Presence(available(MUC, "r")),
            RoomOperation::Presence,
        ),
        // Room administration: the lists and changes of `muc#admin`, the
        // configuration and destruction of `muc#owner`, and a moderator's
        // retraction (XEP-0425). Reaching the host admits nothing: it
        // decides the requester's standing on its stream's evidence.
        (request(query(NS_MUC_ADMIN)), RoomOperation::Admin),
        (set(query(NS_MUC_ADMIN)), RoomOperation::Admin),
        (request(query(NS_MUC_OWNER)), RoomOperation::Owner),
        (set(query(NS_MUC_OWNER)), RoomOperation::Owner),
        (
            set(Element::builder("moderate", NS_MODERATE).build()),
            RoomOperation::Moderation,
        ),
    ] {
        assert_eq!(reach_with(muc, &stanza, hosting), room(operation));
        // Without a room host the whole service refuses.
        assert_eq!(
            reach(muc, &stanza),
            InboundReach::Refused(Refusal::AwaitingReview(ServedService::Muc))
        );
    }
    // Registration, vCards, pins, a moderation element that is not a
    // request to moderate and every IQ response stay outside the reviewed
    // operations.
    let response = Stanza::Iq(Box::new(Iq::Result {
        from: Some(jid(BOB)),
        to: None,
        id: "r".into(),
        payload: None,
    }));
    for stanza in [
        set(query("jabber:iq:register")),
        request(query("vcard-temp")),
        request(query("urn:waddle:pin:0")),
        request(Element::builder("moderate", NS_MODERATE).build()),
        response,
    ] {
        assert_eq!(
            reach_with(muc, &stanza, hosting),
            InboundReach::Refused(Refusal::RoomRequest)
        );
    }
    // Hosting rooms opens nothing else.
    assert_eq!(
        reach_with(
            LocalTarget::Service(ServedService::Spaces),
            &request(query(DISCO_INFO_NS)),
            hosting
        ),
        InboundReach::Refused(Refusal::AwaitingReview(ServedService::Spaces))
    );
    assert_eq!(
        reach_with(LocalTarget::AccountBare, &request(ping_payload()), hosting),
        InboundReach::Refused(Refusal::AccountTraffic)
    );
}

/// A room host that answers every IQ request with an empty result and
/// records what reached it.
#[derive(Default)]
struct RecordingRooms {
    reached: std::sync::Mutex<Vec<RoomOperation>>,
}

#[async_trait::async_trait]
impl LocalRooms for RecordingRooms {
    async fn handle(&self, request: RoomRequest) -> RoomReplies {
        if let Ok(mut reached) = self.reached.lock() {
            reached.push(request.operation);
        }
        let Stanza::Iq(iq) = &request.stanza else {
            return RoomReplies::none();
        };
        RoomReplies::of(vec![Stanza::Iq(Box::new(Iq::Result {
            from: iq.to().cloned(),
            to: iq.from().cloned(),
            id: iq.id().to_owned(),
            payload: None,
        }))])
    }

    async fn unseat(&self, _seat: &Seat, _end: SeatEnd) -> bool {
        false
    }

    async fn seats(&self, _seat: &Seat) -> Option<bool> {
        Some(true)
    }
}

#[tokio::test]
async fn a_replica_that_hosts_rooms_lets_reviewed_muc_operations_reach_the_host() {
    let (a, b, captured, _pki) = deployments(&[MUC]).await;
    let rooms = std::sync::Arc::new(RecordingRooms::default());
    assert!(a.router.attach_rooms(
        rooms.clone(),
        std::sync::Arc::new(SqlSeatStore::new(a.database.clone())),
    ));
    let room = ["room@", MUC].concat();
    send(&b, HOME_B, MUC, get(MUC, "h1", query(DISCO_INFO_NS))).await;
    send(&b, HOME_B, MUC, get(&room, "h2", ping_payload())).await;
    for id in ["h1", "h2"] {
        let reply = wait_for(ARRIVES, || captured.with_id(id).into_iter().next())
            .await
            .expect("the room host's answer");
        assert_eq!(reply.attr("type"), Some("result"), "{id}");
        assert_eq!(reply.attr("to"), Some(BOB), "{id}");
    }
    // An administration request reaches the host, which decides it.
    send(&b, HOME_B, MUC, get(&room, "h5", query(NS_MUC_ADMIN))).await;
    let reply = wait_for(ARRIVES, || captured.with_id("h5").into_iter().next())
        .await
        .expect("the room host's answer");
    assert_eq!(reply.attr("type"), Some("result"));
    // What is outside the reviewed operations never reaches the host.
    let register = Iq::Get {
        from: Some(jid(BOB)),
        to: Some(jid(&room)),
        id: "h3".into(),
        payload: query("jabber:iq:register"),
    };
    send(&b, HOME_B, MUC, register).await;
    refused_with(&captured, "h3", "service-unavailable").await;
    let reached = rooms.reached.lock().expect("recorded operations").clone();
    assert_eq!(
        reached,
        vec![
            RoomOperation::DiscoInfo,
            RoomOperation::Ping,
            RoomOperation::Admin
        ]
    );
    // The server domain lists the service it now lets remote askers reach.
    send(&b, HOME_B, HOME_A, get(HOME_A, "h4", query(DISCO_ITEMS_NS))).await;
    let items = wait_for(ARRIVES, || captured.with_id("h4").into_iter().next())
        .await
        .expect("the server's items");
    let listed: Vec<String> = items
        .children()
        .flat_map(Element::children)
        .filter_map(|item| item.attr("jid").map(str::to_owned))
        .collect();
    assert_eq!(listed, vec![MUC.to_owned()]);
}

/// `home-a.test` serving its account domain and the given service
/// domains over S2S.
async fn deployments(
    services: &'static [&'static str],
) -> (
    support::Routed,
    support::Deployment,
    std::sync::Arc<CapturingSink>,
    support::Pki,
) {
    let pki = support::Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |spec| {
        spec.local.extend(services);
    })
    .await;
    (a, b, captured, pki)
}

async fn refused_with(captured: &CapturingSink, id: &str, condition: &str) -> Element {
    let reply = wait_for(ARRIVES, || captured.with_id(id).into_iter().next())
        .await
        .expect("the stanza is answered");
    assert_eq!(reply.attr("type"), Some("error"), "{id}");
    assert_eq!(reply.attr("to"), Some(BOB), "{id}");
    assert_eq!(error_condition(&reply).as_deref(), Some(condition), "{id}");
    reply
}

#[tokio::test]
async fn server_domain_requests_other_than_discovery_and_ping_are_refused() {
    let (_a, b, captured, _pki) = deployments(&[]).await;
    send(
        &b,
        HOME_B,
        HOME_A,
        get(HOME_A, "v1", query("jabber:iq:version")),
    )
    .await;
    refused_with(&captured, "v1", "service-unavailable").await;
    send(&b, HOME_B, HOME_A, get(HOME_A, "v2", query("vcard-temp"))).await;
    refused_with(&captured, "v2", "service-unavailable").await;
    send(&b, HOME_B, HOME_A, chat(HOME_A, "v3")).await;
    refused_with(&captured, "v3", "service-unavailable").await;
    send(
        &b,
        HOME_B,
        HOME_A,
        get("home-a.test/resource", "v4", ping_payload()),
    )
    .await;
    refused_with(&captured, "v4", "service-unavailable").await;
    send(&b, HOME_B, HOME_A, available(HOME_A, "v5")).await;
    send(&b, HOME_B, HOME_A, get(HOME_A, "sentinel", ping_payload())).await;
    assert!(
        wait_for(ARRIVES, || captured.with_id("sentinel").into_iter().next())
            .await
            .is_some()
    );
    tokio::time::sleep(QUIET).await;
    assert!(captured.with_id("v5").is_empty(), "presence is ignored");
}

#[tokio::test]
async fn services_not_enabled_for_remote_senders_refuse_them() {
    let (_a, b, captured, _pki) = deployments(&[UPLOAD, PUSH]).await;
    for (domain, id) in [(UPLOAD, "u"), (PUSH, "p")] {
        let info = [id, "-info"].concat();
        send(&b, HOME_B, domain, get(domain, &info, query(DISCO_INFO_NS))).await;
        refused_with(&captured, &info, "service-unavailable").await;
        let message = [id, "-message"].concat();
        send(&b, HOME_B, domain, chat(domain, &message)).await;
        refused_with(&captured, &message, "service-unavailable").await;
        let presence = [id, "-presence"].concat();
        send(&b, HOME_B, domain, available(domain, &presence)).await;
        tokio::time::sleep(QUIET).await;
        assert!(captured.with_id(&presence).is_empty());
    }
}

#[tokio::test]
async fn muc_without_a_room_host_and_spaces_refuse_remote_senders() {
    let (_a, b, captured, _pki) = deployments(&[MUC, SPACES]).await;
    let (muc, spaces) = (MUC, SPACES);
    send(&b, HOME_B, muc, get(muc, "m1", query(DISCO_INFO_NS))).await;
    refused_with(&captured, "m1", "service-unavailable").await;
    let room = ["room@", muc].concat();
    send(&b, HOME_B, muc, get(&room, "m2", query(DISCO_INFO_NS))).await;
    refused_with(&captured, "m2", "service-unavailable").await;
    let groupchat = {
        let mut message = Message::new_with_type(MessageType::Groupchat, Some(jid(&room)));
        message.from = Some(jid(BOB));
        message.id = Some(xmpp_parsers::message::Id("m3".into()));
        message
    };
    send(&b, HOME_B, muc, groupchat).await;
    refused_with(&captured, "m3", "service-unavailable").await;
    let join = [room.as_str(), "/bob"].concat();
    send(&b, HOME_B, muc, available(&join, "m4")).await;
    send(&b, HOME_B, spaces, get(spaces, "s1", query(DISCO_ITEMS_NS))).await;
    refused_with(&captured, "s1", "service-unavailable").await;
    let publish = Iq::Set {
        from: Some(jid(BOB)),
        to: Some(jid(spaces)),
        id: "s2".into(),
        payload: Element::builder("pubsub", "http://jabber.org/protocol/pubsub").build(),
    };
    send(&b, HOME_B, spaces, publish).await;
    refused_with(&captured, "s2", "service-unavailable").await;
    tokio::time::sleep(QUIET).await;
    assert!(captured.with_id("m4").is_empty(), "a join is ignored");
}

#[tokio::test]
async fn an_error_is_never_answered_with_an_error() {
    let (_a, b, captured, _pki) = deployments(&[UPLOAD]).await;
    let upload = UPLOAD;
    let error = Iq::Error {
        from: Some(jid(BOB)),
        to: Some(jid(upload)),
        id: "x1".into(),
        payload: None,
        error: StanzaError::new(ErrorType::Cancel, DefinedCondition::BadRequest, "en", "No."),
    };
    send(&b, HOME_B, upload, error).await;
    let mut message = chat(HOME_A, "x2");
    message.type_ = MessageType::Error;
    send(&b, HOME_B, HOME_A, message).await;
    send(&b, HOME_B, HOME_A, get(HOME_A, "sentinel", ping_payload())).await;
    assert!(
        wait_for(ARRIVES, || captured.with_id("sentinel").into_iter().next())
            .await
            .is_some()
    );
    tokio::time::sleep(QUIET).await;
    assert!(captured.with_id("x1").is_empty());
    assert!(captured.with_id("x2").is_empty());
}

#[tokio::test]
async fn a_refused_spelling_is_answered_with_jid_malformed_and_the_stream_stays_open() {
    let (a, _b, captured, pki) = deployments(&[]).await;
    let mut peer = RawPeer::establish(&pki, a.deployment.address, HOME_B, HOME_A).await;
    // An uppercase localpart in `from`, then in `to`.
    let cases = [
        (
            "Bob@home-b.test/phone",
            HOME_A,
            "j1",
            "bob@home-b.test/phone",
        ),
        (BOB, "Alice@home-a.test", "j2", BOB),
    ];
    for (from, to, id, answered) in cases {
        let iq = Element::builder("iq", "jabber:server")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
            .append(ping_payload())
            .build();
        let bytes = peer.encode(&iq);
        peer.write(&bytes).await.expect("written");
        let reply = wait_for(ARRIVES, || captured.with_id(id).into_iter().next())
            .await
            .expect("answered");
        assert_eq!(reply.attr("to"), Some(answered));
        assert_eq!(error_condition(&reply).as_deref(), Some("jid-malformed"));
    }
    // A nested address in a refused spelling is refused the same way.
    let item = Element::builder("item", "jabber:iq:roster")
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "Carol@example.test",
        )
        .build();
    let nested = Element::builder("iq", "jabber:server")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "j3")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), BOB)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), HOME_A)
        .append(
            Element::builder("query", "jabber:iq:roster")
                .append(item)
                .build(),
        )
        .build();
    let bytes = peer.encode(&nested);
    peer.write(&bytes).await.expect("written");
    let reply = wait_for(ARRIVES, || captured.with_id("j3").into_iter().next())
        .await
        .expect("answered");
    assert_eq!(error_condition(&reply).as_deref(), Some("jid-malformed"));

    // A message in a refused spelling is answered with the message's own
    // id, so its sender can tell which message failed.
    let message = Element::builder("message", "jabber:server")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "j5")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "Bob@home-b.test/phone",
        )
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), HOME_A)
        .append(Element::builder("body", "jabber:server").append("hi"))
        .build();
    let bytes = peer.encode(&message);
    peer.write(&bytes).await.expect("written");
    let reply = wait_for(ARRIVES, || captured.with_id("j5").into_iter().next())
        .await
        .expect("answered with the message's id");
    assert_eq!(reply.name(), "message");
    assert_eq!(reply.attr("type"), Some("error"));
    assert_eq!(reply.attr("to"), Some(BOB));
    assert_eq!(error_condition(&reply).as_deref(), Some("jid-malformed"));

    // A stanza that is not valid for its kind is a bad request.
    let empty = Element::builder("iq", "jabber:server")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "j4")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), BOB)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), HOME_A)
        .build();
    let bytes = peer.encode(&empty);
    peer.write(&bytes).await.expect("written");
    let reply = wait_for(ARRIVES, || captured.with_id("j4").into_iter().next())
        .await
        .expect("answered");
    assert_eq!(error_condition(&reply).as_deref(), Some("bad-request"));

    // Presence in a refused spelling is dropped unanswered: an error
    // presence to a room would reach it as the occupant's answer and
    // remove the occupant.
    let presence = Element::builder("presence", "jabber:server")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "j6")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "Bob@home-b.test/phone",
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "alice@home-a.test/phone",
        )
        .build();
    let bytes = peer.encode(&presence);
    peer.write(&bytes).await.expect("written");
    tokio::time::sleep(QUIET).await;
    assert!(
        captured.with_id("j6").is_empty(),
        "presence is not answered"
    );

    // The stream is still open.
    let (error, closed, _) = peer.read_until_closed(QUIET).await;
    assert_eq!(error, None);
    assert!(!closed);
}
