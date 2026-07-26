//! XEP-0199: XMPP Ping dedicated suite.

use minidom::Element;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::protocol::event::{OutboundEvent, StanzaContext};
use waddle_xmpp::protocol::handlers::ping::PingHandler;
use waddle_xmpp::protocol::IqHandler;
use waddle_xmpp::xep::{is_ping, NS_PING};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

fn session_jid() -> jid::FullJid {
    "alice@waddle.social/web".parse().expect("valid full jid")
}

fn stanza_ctx<'a>(jid: &'a jid::FullJid) -> StanzaContext<'a> {
    StanzaContext {
        domain: "waddle.social",
        full_jid: jid,
        media_capabilities: None,
    }
}

#[test]
fn xep0199_server_disco_advertises_ping() {
    let features = server_features();
    assert!(features.contains(&Feature::ping()));
}

#[test]
fn xep0199_only_empty_iq_get_ping_is_accepted() {
    let valid = Iq::Get {
        from: Some("alice@waddle.social/web".parse().expect("valid jid")),
        to: None,
        id: "ping-valid".to_string(),
        payload: Element::builder("ping", NS_PING).build(),
    };
    let malformed = Iq::Get {
        from: Some("alice@waddle.social/web".parse().expect("valid jid")),
        to: None,
        id: "ping-bad".to_string(),
        payload: Element::builder("ping", NS_PING)
            .append(Element::builder("extra", NS_PING).build())
            .build(),
    };

    assert!(is_ping(&valid));
    assert!(!is_ping(&malformed));
}

#[test]
fn xep0199_bare_jid_ping_replies_from_server_domain() {
    let iq = Iq::Get {
        from: Some("alice@waddle.social/web".parse().expect("valid jid")),
        to: Some("alice@waddle.social".parse().expect("valid jid")),
        id: "ping-bare".to_string(),
        payload: Element::builder("ping", NS_PING).build(),
    };
    let jid = session_jid();

    let events = PingHandler.handle(&iq, &stanza_ctx(&jid));

    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                assert_eq!(
                    reply.from().map(|j| j.to_string()),
                    Some("waddle.social".into())
                );
                assert_eq!(
                    reply.to().map(|j| j.to_string()),
                    Some("alice@waddle.social/web".into())
                );
                assert!(matches!(&**reply, Iq::Result { payload: None, .. }));
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}

#[test]
fn xep0199_full_jid_ping_keeps_full_jid_as_reply_from() {
    let iq = Iq::Get {
        from: Some("alice@waddle.social/phone".parse().expect("valid jid")),
        to: Some("alice@waddle.social/web".parse().expect("valid jid")),
        id: "ping-full".to_string(),
        payload: Element::builder("ping", NS_PING).build(),
    };
    let jid = session_jid();

    let events = PingHandler.handle(&iq, &stanza_ctx(&jid));

    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                assert_eq!(
                    reply.from().map(|j| j.to_string()),
                    Some("alice@waddle.social/web".into())
                );
                assert_eq!(
                    reply.to().map(|j| j.to_string()),
                    Some("alice@waddle.social/phone".into())
                );
                assert!(matches!(&**reply, Iq::Result { payload: None, .. }));
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}

#[test]
fn xep0199_malformed_ping_get_returns_bad_request() {
    let iq = Iq::Get {
        from: Some("alice@waddle.social/web".parse().expect("valid jid")),
        to: None,
        id: "ping-bad".to_string(),
        payload: Element::builder("ping", NS_PING)
            .append(Element::builder("extra", NS_PING).build())
            .build(),
    };
    let jid = session_jid();

    let events = PingHandler.handle(&iq, &stanza_ctx(&jid));

    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                let Iq::Error { error, .. } = &**reply else {
                    panic!("expected typed error reply")
                };
                assert_eq!(error.type_, ErrorType::Modify);
                assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}

#[test]
fn xep0199_ping_set_returns_bad_request() {
    let iq = Iq::Set {
        from: Some("alice@waddle.social/web".parse().expect("valid jid")),
        to: Some("waddle.social".parse().expect("valid jid")),
        id: "ping-set".to_string(),
        payload: Element::builder("ping", NS_PING).build(),
    };
    let jid = session_jid();

    let events = PingHandler.handle(&iq, &stanza_ctx(&jid));

    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                let Iq::Error { error, .. } = &**reply else {
                    panic!("expected typed error reply")
                };
                assert_eq!(error.type_, ErrorType::Modify);
                assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
            }
            other => panic!("expected IQ reply, got {other:?}"),
        },
        other => panic!("expected SendStanza, got {other:?}"),
    }
}
