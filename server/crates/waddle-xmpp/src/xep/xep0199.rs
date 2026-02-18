//! XEP-0199: XMPP Ping
//!
//! Provides helpers for detecting ping IQs and building responses.

use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0199 Ping.
pub const NS_PING: &str = "urn:xmpp:ping";

/// Check if an IQ stanza is a ping request (XEP-0199).
pub fn is_ping(iq: &Iq) -> bool {
    match &iq.payload {
        IqType::Get(elem) => elem.name() == "ping" && elem.ns() == NS_PING,
        _ => false,
    }
}

/// Build an empty result IQ for a ping request.
pub fn build_ping_result(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

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

        let result = build_ping_result(&iq);

        assert_eq!(result.id, "ping-3");
        assert_eq!(result.from, iq.to);
        assert_eq!(result.to, iq.from);
        assert!(matches!(result.payload, IqType::Result(None)));
    }
}
