//! Service Discovery: disco#items handling.
//!
//! Implements XEP-0030 disco#items for querying entity items/services.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::XmppError;

/// Service Discovery items namespace (XEP-0030).
pub const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";

/// Parsed disco#items query.
#[derive(Debug, Clone)]
pub struct DiscoItemsQuery {
    /// Target JID (from IQ 'to' attribute)
    pub target: Option<String>,
    /// Optional node being queried
    pub node: Option<String>,
}

/// Item element for disco#items response.
#[derive(Debug, Clone)]
pub struct DiscoItem {
    /// JID of the item
    pub jid: String,
    /// Optional name (human-readable)
    pub name: Option<String>,
    /// Optional node identifier
    pub node: Option<String>,
}

impl DiscoItem {
    /// Create a new disco item.
    pub fn new(jid: &str, name: Option<&str>, node: Option<&str>) -> Self {
        Self {
            jid: jid.to_string(),
            name: name.map(|s| s.to_string()),
            node: node.map(|s| s.to_string()),
        }
    }

    /// Create a MUC service item.
    pub fn muc_service(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    /// Create a MUC room item.
    pub fn muc_room(jid: &str, name: &str) -> Self {
        Self::new(jid, Some(name), None)
    }
}

/// Check if an IQ is a disco#items query.
pub fn is_disco_items_query(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            elem.name() == "query" && elem.ns() == DISCO_ITEMS_NS
        }
        _ => false,
    }
}

/// Parse a disco#items query from an IQ stanza.
pub fn parse_disco_items_query(iq: &Iq) -> Result<DiscoItemsQuery, XmppError> {
    let query_elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "query" && elem.ns() == DISCO_ITEMS_NS {
                elem
            } else {
                return Err(XmppError::bad_request(Some(
                    "Missing disco#items query element".to_string(),
                )));
            }
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "disco#items must be IQ get".to_string(),
            )))
        }
    };

    let node = query_elem.attr("node").map(|s| s.to_string());
    let target = iq.to.as_ref().map(|j| j.to_string());

    debug!(target = ?target, node = ?node, "Parsed disco#items query");

    Ok(DiscoItemsQuery { target, node })
}

/// Build a disco#items response IQ.
///
/// The response includes items available at the queried entity.
pub fn build_disco_items_response(
    original_iq: &Iq,
    items: &[DiscoItem],
    node: Option<&str>,
) -> Iq {
    let mut query_builder = Element::builder("query", DISCO_ITEMS_NS);

    // Add node attribute if present
    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }

    // Add items
    for item in items {
        let mut item_builder =
            Element::builder("item", DISCO_ITEMS_NS).attr("jid", &item.jid);

        if let Some(ref name) = item.name {
            item_builder = item_builder.attr("name", name);
        }

        if let Some(ref node) = item.node {
            item_builder = item_builder.attr("node", node);
        }

        query_builder = query_builder.append(item_builder.build());
    }

    let query = query_builder.build();

    // Build response IQ
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(query)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_disco_items_query() {
        let query_elem = Element::builder("query", DISCO_ITEMS_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        assert!(is_disco_items_query(&iq));
    }

    #[test]
    fn test_is_not_disco_items_query_wrong_ns() {
        let query_elem = Element::builder("query", "some:other:ns").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        assert!(!is_disco_items_query(&iq));
    }

    #[test]
    fn test_is_not_disco_items_query_set() {
        let query_elem = Element::builder("query", DISCO_ITEMS_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-3".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        assert!(!is_disco_items_query(&iq));
    }

    #[test]
    fn test_build_disco_items_response() {
        let query_elem = Element::builder("query", DISCO_ITEMS_NS).build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("server.example.com".parse().unwrap()),
            id: "disco-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let items = vec![DiscoItem::muc_service(
            "muc.example.com",
            Some("MUC Service"),
        )];

        let response = build_disco_items_response(&iq, &items, None);

        assert_eq!(response.id, "disco-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));
    }

    #[test]
    fn test_disco_item_constructors() {
        let muc_service = DiscoItem::muc_service("muc.example.com", Some("Chat"));
        assert_eq!(muc_service.jid, "muc.example.com");
        assert_eq!(muc_service.name, Some("Chat".to_string()));
        assert!(muc_service.node.is_none());

        let room = DiscoItem::muc_room("room@muc.example.com", "General");
        assert_eq!(room.jid, "room@muc.example.com");
        assert_eq!(room.name, Some("General".to_string()));
    }

    #[test]
    fn test_build_disco_items_with_node() {
        let query_elem = Element::builder("query", DISCO_ITEMS_NS)
            .attr("node", "test-node")
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("muc.example.com".parse().unwrap()),
            id: "disco-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let items = vec![DiscoItem::muc_room("room@muc.example.com", "Room 1")];

        let response = build_disco_items_response(&iq, &items, Some("test-node"));

        if let xmpp_parsers::iq::IqType::Result(Some(elem)) = response.payload {
            assert_eq!(elem.attr("node"), Some("test-node"));
        } else {
            panic!("Expected Result with element");
        }
    }
}
