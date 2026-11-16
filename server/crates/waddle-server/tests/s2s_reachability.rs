//! What a stanza from an S2S stream may reach (ADR-0020 D10; gate G23):
//! only the server domain's service discovery and ping are answered for a
//! remote sender, a local resource's own pending request is completed, and
//! every other local service refuses: IQ requests and messages receive
//! `service-unavailable`, presence is ignored and no error is answered.
//! MUC and the Spaces PubSub service, which D10 enables, refuse until their
//! handlers are reviewed for remote principals. The identity profile
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
use waddle_server::s2s::route::{reach, InboundReach, LocalTarget, Refusal, RouterLimits};
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
        CorrelatedResponse, Refused, ServerDiscoInfo, ServerDiscoItems, ServerPing,
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
    for stanza in [
        request(query("jabber:iq:version")),
        request(query("vcard-temp")),
        set.clone(),
        response.clone(),
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
async fn muc_and_spaces_refuse_remote_senders_until_their_handlers_are_reviewed() {
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
    assert!(captured.with_id("j6").is_empty(), "presence is not answered");

    // The stream is still open.
    let (error, closed, _) = peer.read_until_closed(QUIET).await;
    assert_eq!(error, None);
    assert!(!closed);
}
