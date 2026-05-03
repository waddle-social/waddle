//! XEP-0199: XMPP Ping
//!
//! Provides helpers for validating ping IQs and building responses.

use jid::{BareJid, Jid};
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Namespace for XEP-0199 Ping.
pub const NS_PING: &str = "urn:xmpp:ping";

fn response_from(original_iq: &Iq, domain: &str, session_bare: &BareJid) -> Option<Jid> {
    match original_iq.to.as_ref() {
        None => Some(domain.parse().expect("server domain should parse as a JID")),
        Some(to) if to.try_as_full().is_err() && to.to_bare() == *session_bare => {
            Some(domain.parse().expect("server domain should parse as a JID"))
        }
        Some(to) => Some(to.clone()),
    }
}

fn ping_payload_is_valid(elem: &minidom::Element) -> bool {
    elem.name() == "ping"
        && elem.ns() == NS_PING
        && elem.attrs().next().is_none()
        && elem.children().next().is_none()
        && elem.texts().next().is_none()
}

/// Check if an IQ stanza is a conformant ping request (XEP-0199).
pub fn is_ping(iq: &Iq) -> bool {
    match &iq.payload {
        IqType::Get(elem) => ping_payload_is_valid(elem),
        _ => false,
    }
}

/// Build an empty result IQ for a ping request.
pub fn build_ping_result(original_iq: &Iq, domain: &str, session_bare: &BareJid) -> Iq {
    Iq {
        from: response_from(original_iq, domain, session_bare),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(None),
    }
}

/// Build a typed `bad-request` IQ error for a malformed ping request.
pub fn build_ping_bad_request(original_iq: &Iq, domain: &str, session_bare: &BareJid) -> Iq {
    Iq {
        from: response_from(original_iq, domain, session_bare),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(StanzaError::new(
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "en",
            "",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    fn session_bare() -> BareJid {
        "alice@example.com".parse().expect("valid bare JID")
    }

    #[test]
    fn test_is_ping() {
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "ping-1".to_string(),
            payload: IqType::Get(ping_elem),
        };

        assert!(is_ping(&iq));
    }

    #[test]
    fn test_is_ping_false_for_ping_with_child() {
        let ping_elem = Element::builder("ping", NS_PING)
            .append(Element::builder("extra", NS_PING).build())
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "ping-child".to_string(),
            payload: IqType::Get(ping_elem),
        };

        assert!(!is_ping(&iq));
    }

    #[test]
    fn test_is_ping_false_for_ping_with_attribute() {
        let ping_elem = Element::builder("ping", NS_PING)
            .attr("id", "unexpected")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "ping-attr".to_string(),
            payload: IqType::Get(ping_elem),
        };

        assert!(!is_ping(&iq));
    }

    #[test]
    fn test_is_ping_false_for_other_payloads() {
        let other_elem = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "ping-2".to_string(),
            payload: IqType::Get(other_elem),
        };

        assert!(!is_ping(&iq));
    }

    #[test]
    fn test_build_ping_result_swaps_to_from() {
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("muc.example.com".parse().unwrap()),
            id: "ping-3".to_string(),
            payload: IqType::Get(ping_elem),
        };

        let result = build_ping_result(&iq, "example.com", &session_bare());

        assert_eq!(result.id, "ping-3");
        assert_eq!(result.from, iq.to);
        assert_eq!(result.to, iq.from);
        assert!(matches!(result.payload, IqType::Result(None)));
    }

    #[test]
    fn test_build_ping_result_uses_server_domain_for_absent_to() {
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: Some("alice@example.com/web".parse().unwrap()),
            to: None,
            id: "ping-none".to_string(),
            payload: IqType::Get(ping_elem),
        };
        let result = build_ping_result(&iq, "example.com", &session_bare());
        assert_eq!(
            result.from.as_ref().map(ToString::to_string),
            Some("example.com".into())
        );
        assert_eq!(result.to, iq.from);
        assert_eq!(result.id, "ping-none");
    }

    #[test]
    fn test_build_ping_result_uses_server_domain_for_session_bare_target() {
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: Some("alice@example.com/web".parse().unwrap()),
            to: Some("alice@example.com".parse().unwrap()),
            id: "ping-bare".to_string(),
            payload: IqType::Get(ping_elem),
        };
        let result = build_ping_result(&iq, "example.com", &session_bare());
        assert_eq!(
            result.from.as_ref().map(ToString::to_string),
            Some("example.com".into())
        );
        assert_eq!(result.to, iq.from);
    }

    #[test]
    fn test_build_ping_result_keeps_full_jid_target_as_from() {
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: Some("alice@example.com/phone".parse().unwrap()),
            to: Some("alice@example.com/web".parse().unwrap()),
            id: "ping-full".to_string(),
            payload: IqType::Get(ping_elem),
        };
        let result = build_ping_result(&iq, "example.com", &session_bare());
        assert_eq!(
            result.from.as_ref().map(ToString::to_string),
            Some("alice@example.com/web".into())
        );
        assert_eq!(result.to, iq.from);
    }

    #[test]
    fn test_build_ping_bad_request_uses_typed_error() {
        let ping_elem = Element::builder("ping", NS_PING)
            .append(Element::builder("extra", NS_PING).build())
            .build();
        let iq = Iq {
            from: Some("alice@example.com/web".parse().unwrap()),
            to: None,
            id: "ping-bad".to_string(),
            payload: IqType::Get(ping_elem),
        };
        let result = build_ping_bad_request(&iq, "example.com", &session_bare());
        let IqType::Error(error) = result.payload else {
            panic!("expected typed IQ error")
        };
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
        assert_eq!(
            result.from.as_ref().map(ToString::to_string),
            Some("example.com".into())
        );
        assert_eq!(result.to, iq.from);
    }

    #[test]
    fn test_is_ping_false_for_set() {
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "ping-set".to_string(),
            payload: IqType::Set(ping_elem),
        };
        assert!(!is_ping(&iq));
    }

    #[test]
    fn test_is_ping_false_for_result() {
        let iq = Iq {
            from: None,
            to: None,
            id: "ping-res".to_string(),
            payload: IqType::Result(None),
        };
        assert!(!is_ping(&iq));
    }

    #[test]
    fn test_ns_ping_constant() {
        assert_eq!(NS_PING, "urn:xmpp:ping");
    }
}
