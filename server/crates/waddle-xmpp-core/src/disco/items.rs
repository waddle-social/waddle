//! Service Discovery: disco#items handling.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::CoreError;

/// Service Discovery items namespace (XEP-0030).
pub const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";

/// Parsed disco#items query.
#[derive(Debug, Clone)]
pub struct DiscoItemsQuery {
    pub target: Option<String>,
    pub node: Option<String>,
}

/// Item element for disco#items response.
#[derive(Debug, Clone)]
pub struct DiscoItem {
    pub jid: String,
    pub name: Option<String>,
    pub node: Option<String>,
}

impl DiscoItem {
    pub fn new(jid: &str, name: Option<&str>, node: Option<&str>) -> Self {
        Self {
            jid: jid.to_string(),
            name: name.map(str::to_string),
            node: node.map(str::to_string),
        }
    }

    pub fn muc_service(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    /// XEP-0272 SFU mixer JID (`calls.<server-domain>`).
    /// disco#items on the server returns this so a strict client
    /// can subsequently disco#info the mixer and discover Muji
    /// support before sending a session-initiate.
    pub fn calls_mixer(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    pub fn muc_room(jid: &str, name: &str) -> Self {
        Self::new(jid, Some(name), None)
    }

    pub fn upload_service(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    pub fn pubsub_service(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    pub fn spaces_service(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    /// Community pubsub service that hosts the XEP-0472 feed and
    /// XEP-0501 stories nodes (`community.<domain>`).
    pub fn community_service(domain: &str, name: Option<&str>) -> Self {
        Self::new(domain, name, None)
    }

    pub fn spaces_node(jid: &str, node: &str, name: Option<&str>) -> Self {
        Self {
            jid: jid.to_string(),
            name: name.map(str::to_string),
            node: Some(node.to_string()),
        }
    }

    pub fn pubsub_node(jid: &str, node: &str) -> Self {
        Self::new(jid, None, Some(node))
    }

    pub fn command(jid: &str, node: &str, name: &str) -> Self {
        Self::new(jid, Some(name), Some(node))
    }
}

/// Check if an IQ is a disco#items query.
pub fn is_disco_items_query(iq: &Iq) -> bool {
    match iq {
        Iq::Get { payload, .. } => payload.name() == "query" && payload.ns() == DISCO_ITEMS_NS,
        _ => false,
    }
}

/// Parse a disco#items query from an IQ stanza.
pub fn parse_disco_items_query(iq: &Iq) -> Result<DiscoItemsQuery, CoreError> {
    let query_elem = match iq {
        Iq::Get { payload, .. } => {
            if payload.name() == "query" && payload.ns() == DISCO_ITEMS_NS {
                payload
            } else {
                return Err(CoreError::bad_request(Some(
                    "Missing disco#items query element".to_string(),
                )));
            }
        }
        _ => {
            return Err(CoreError::bad_request(Some(
                "disco#items must be IQ get".to_string(),
            )))
        }
    };

    let node = query_elem.attr("node").map(str::to_string);
    let target = iq.to().map(|j| j.to_string());

    debug!(target = ?target, node = ?node, "Parsed disco#items query");

    Ok(DiscoItemsQuery { target, node })
}

/// Build a disco#items response IQ.
pub fn build_disco_items_response(original_iq: &Iq, items: &[DiscoItem], node: Option<&str>) -> Iq {
    let mut query_builder = Element::builder("query", DISCO_ITEMS_NS);

    if let Some(n) = node {
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), n);
    }

    for item in items {
        let mut item_builder = Element::builder("item", DISCO_ITEMS_NS)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), &item.jid);

        if let Some(ref name) = item.name {
            item_builder = item_builder.attr(minidom::rxml::xml_ncname!("name").to_owned(), name);
        }

        if let Some(ref node) = item.node {
            item_builder = item_builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
        }

        query_builder = query_builder.append(item_builder.build());
    }

    let query = query_builder.build();

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(query),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_disco_items_query() {
        let query_elem = Element::builder("query", DISCO_ITEMS_NS).build();
        let iq = Iq::Get {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: query_elem,
        };

        assert!(is_disco_items_query(&iq));
    }

    #[test]
    fn test_is_not_disco_items_query_wrong_ns() {
        let query_elem = Element::builder("query", "some:other:ns").build();
        let iq = Iq::Get {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: query_elem,
        };

        assert!(!is_disco_items_query(&iq));
    }

    #[test]
    fn test_build_disco_items_response() {
        let query_elem = Element::builder("query", DISCO_ITEMS_NS).build();
        let iq = Iq::Get {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("server.example.com".parse().unwrap()),
            id: "disco-1".to_string(),
            payload: query_elem,
        };

        let items = vec![DiscoItem::muc_service(
            "muc.example.com",
            Some("MUC Service"),
        )];

        let response = build_disco_items_response(&iq, &items, None);

        assert_eq!(response.id(), "disco-1");
        assert!(matches!(
            response,
            Iq::Result {
                payload: Some(_),
                ..
            }
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
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test-node")
            .build();
        let iq = Iq::Get {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("muc.example.com".parse().unwrap()),
            id: "disco-2".to_string(),
            payload: query_elem,
        };

        let items = vec![DiscoItem::muc_room("room@muc.example.com", "Room 1")];

        let response = build_disco_items_response(&iq, &items, Some("test-node"));

        if let Iq::Result {
            payload: Some(elem),
            ..
        } = response
        {
            assert_eq!(elem.attr("node"), Some("test-node"));
        } else {
            panic!("Expected Result with element");
        }
    }
}
