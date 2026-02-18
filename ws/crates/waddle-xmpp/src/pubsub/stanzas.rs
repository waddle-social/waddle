//! PubSub stanza parsing and building.
//!
//! Handles XML parsing of PubSub IQ stanzas and building of responses.

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use crate::XmppError;

/// Main PubSub namespace (XEP-0060).
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// PubSub event namespace for notifications.
pub const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

/// PubSub owner namespace for node management.
pub const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";

/// PubSub errors namespace.
pub const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

/// A PubSub item with optional ID and payload.
#[derive(Debug, Clone)]
pub struct PubSubItem {
    /// Optional item ID. If None, server generates one.
    pub id: Option<String>,
    /// The item payload (any XML element).
    pub payload: Option<Element>,
}

impl PubSubItem {
    /// Create a new PubSubItem with an ID and payload.
    pub fn new(id: Option<String>, payload: Option<Element>) -> Self {
        Self { id, payload }
    }

    /// Create a PubSubItem from a minidom Element.
    pub fn from_element(elem: &Element) -> Self {
        let id = elem.attr("id").map(String::from);
        let payload = elem.children().next().cloned();
        Self { id, payload }
    }

    /// Build an item element for inclusion in responses.
    pub fn to_element(&self, ns: &str) -> Element {
        let mut builder = Element::builder("item", ns);

        if let Some(ref id) = self.id {
            builder = builder.attr("id", id);
        }

        if let Some(ref payload) = self.payload {
            builder = builder.append(payload.clone());
        }

        builder.build()
    }
}

/// Parsed PubSub request types.
#[derive(Debug, Clone)]
pub enum PubSubRequest {
    /// Publish an item to a node.
    Publish {
        /// Node name to publish to.
        node: String,
        /// The item to publish.
        item: PubSubItem,
    },
    /// Retract (delete) an item from a node.
    Retract {
        /// Node name.
        node: String,
        /// Item ID to retract.
        item_id: String,
        /// Whether to notify subscribers of retraction.
        notify: bool,
    },
    /// Retrieve items from a node.
    Items {
        /// Node name.
        node: String,
        /// Maximum number of items to return.
        max_items: Option<u32>,
        /// Specific item IDs to retrieve.
        item_ids: Vec<String>,
    },
    /// Create a new node.
    CreateNode {
        /// Node name.
        node: String,
    },
    /// Delete a node.
    DeleteNode {
        /// Node name.
        node: String,
    },
    /// Subscribe to a node.
    Subscribe {
        /// Node name.
        node: String,
        /// JID to subscribe.
        jid: String,
    },
    /// Unsubscribe from a node.
    Unsubscribe {
        /// Node name.
        node: String,
        /// JID to unsubscribe.
        jid: String,
        /// Optional subscription ID.
        subid: Option<String>,
    },
}

/// Check if an IQ is a PubSub request.
pub fn is_pubsub_iq(iq: &Iq) -> bool {
    match &iq.payload {
        IqType::Get(elem) | IqType::Set(elem) => {
            elem.name() == "pubsub" && (elem.ns() == NS_PUBSUB || elem.ns() == NS_PUBSUB_OWNER)
        }
        _ => false,
    }
}

/// Parse a PubSub IQ stanza into a structured request.
pub fn parse_pubsub_iq(iq: &Iq) -> Result<PubSubRequest, XmppError> {
    let pubsub_elem = match &iq.payload {
        IqType::Get(elem) | IqType::Set(elem) => {
            if elem.name() == "pubsub" && (elem.ns() == NS_PUBSUB || elem.ns() == NS_PUBSUB_OWNER) {
                elem
            } else {
                return Err(XmppError::bad_request(Some(
                    "Expected pubsub element".to_string(),
                )));
            }
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "PubSub IQ must be get or set".to_string(),
            )));
        }
    };

    // Check for each PubSub child element type
    if let Some(publish) = pubsub_elem.get_child("publish", NS_PUBSUB) {
        let node = publish
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        let item = publish
            .get_child("item", NS_PUBSUB)
            .map(PubSubItem::from_element)
            .unwrap_or_else(|| PubSubItem::new(None, None));

        return Ok(PubSubRequest::Publish { node, item });
    }

    if let Some(retract) = pubsub_elem.get_child("retract", NS_PUBSUB) {
        let node = retract
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        let notify = retract
            .attr("notify")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let item_id = retract
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id"))
            .ok_or_else(|| XmppError::bad_request(Some("Missing item id".to_string())))?
            .to_string();

        return Ok(PubSubRequest::Retract {
            node,
            item_id,
            notify,
        });
    }

    if let Some(items) = pubsub_elem.get_child("items", NS_PUBSUB) {
        let node = items
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        let max_items = items.attr("max_items").and_then(|s| s.parse().ok());

        let item_ids: Vec<String> = items
            .children()
            .filter(|c| c.name() == "item")
            .filter_map(|c| c.attr("id").map(String::from))
            .collect();

        return Ok(PubSubRequest::Items {
            node,
            max_items,
            item_ids,
        });
    }

    if let Some(create) = pubsub_elem.get_child("create", NS_PUBSUB) {
        let node = create
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        return Ok(PubSubRequest::CreateNode { node });
    }

    if let Some(delete) = pubsub_elem.get_child("delete", NS_PUBSUB_OWNER) {
        let node = delete
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        return Ok(PubSubRequest::DeleteNode { node });
    }

    if let Some(subscribe) = pubsub_elem.get_child("subscribe", NS_PUBSUB) {
        let node = subscribe
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        let jid = subscribe
            .attr("jid")
            .ok_or_else(|| XmppError::bad_request(Some("Missing jid attribute".to_string())))?
            .to_string();

        return Ok(PubSubRequest::Subscribe { node, jid });
    }

    if let Some(unsubscribe) = pubsub_elem.get_child("unsubscribe", NS_PUBSUB) {
        let node = unsubscribe
            .attr("node")
            .ok_or_else(|| XmppError::bad_request(Some("Missing node attribute".to_string())))?
            .to_string();

        let jid = unsubscribe
            .attr("jid")
            .ok_or_else(|| XmppError::bad_request(Some("Missing jid attribute".to_string())))?
            .to_string();

        let subid = unsubscribe.attr("subid").map(String::from);

        return Ok(PubSubRequest::Unsubscribe { node, jid, subid });
    }

    Err(XmppError::bad_request(Some(
        "Unsupported PubSub operation".to_string(),
    )))
}

/// Build a PubSub event notification message.
///
/// Used to notify subscribers of published items.
pub fn build_pubsub_event(from: &str, to: &str, node: &str, items: &[PubSubItem]) -> Element {
    let mut items_elem = Element::builder("items", NS_PUBSUB_EVENT).attr("node", node);

    for item in items {
        items_elem = items_elem.append(item.to_element(NS_PUBSUB_EVENT));
    }

    Element::builder("message", "jabber:client")
        .attr("from", from)
        .attr("to", to)
        .append(
            Element::builder("event", NS_PUBSUB_EVENT)
                .append(items_elem.build())
                .build(),
        )
        .build()
}

/// Build a PubSub items result IQ.
pub fn build_pubsub_items_result(original_iq: &Iq, node: &str, items: &[PubSubItem]) -> Iq {
    let mut items_elem = Element::builder("items", NS_PUBSUB).attr("node", node);

    for item in items {
        items_elem = items_elem.append(item.to_element(NS_PUBSUB));
    }

    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(items_elem.build())
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build a PubSub publish result IQ.
pub fn build_pubsub_publish_result(original_iq: &Iq, node: &str, item_id: &str) -> Iq {
    let item_elem = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .build();

    let publish_elem = Element::builder("publish", NS_PUBSUB)
        .attr("node", node)
        .append(item_elem)
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish_elem)
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// PubSub-specific error conditions.
#[derive(Debug, Clone)]
pub enum PubSubError {
    /// The node does not exist.
    NodeNotFound,
    /// The item does not exist.
    ItemNotFound,
    /// The requesting entity does not have permission.
    Forbidden,
    /// The node already exists.
    NodeExists,
    /// Invalid JID format.
    InvalidJid,
    /// Precondition failed (e.g., wrong access model).
    PreconditionNotMet,
    /// Not subscribed to the node.
    NotSubscribed,
}

/// Build a PubSub error IQ response.
pub fn build_pubsub_error(original_iq: &Iq, error: PubSubError) -> Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let (error_type, defined_condition) = match error {
        PubSubError::NodeNotFound => (ErrorType::Cancel, DefinedCondition::ItemNotFound),
        PubSubError::ItemNotFound => (ErrorType::Cancel, DefinedCondition::ItemNotFound),
        PubSubError::Forbidden => (ErrorType::Auth, DefinedCondition::Forbidden),
        PubSubError::NodeExists => (ErrorType::Cancel, DefinedCondition::Conflict),
        PubSubError::InvalidJid => (ErrorType::Modify, DefinedCondition::BadRequest),
        PubSubError::PreconditionNotMet => (ErrorType::Cancel, DefinedCondition::Conflict),
        PubSubError::NotSubscribed => (ErrorType::Cancel, DefinedCondition::UnexpectedRequest),
    };

    let stanza_error = StanzaError::new(error_type, defined_condition, "en", "");

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(stanza_error),
    }
}

/// Build an empty result IQ for simple success responses.
pub fn build_pubsub_success(original_iq: &Iq) -> Iq {
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

    #[test]
    fn test_parse_publish_request() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='user@example.com' to='user@example.com' id='pub1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <publish node='urn:xmpp:bookmarks:1'>
                    <item id='test@conference.example.org'>
                        <conference xmlns='urn:xmpp:bookmarks:1' autojoin='true'>
                            <nick>TestNick</nick>
                        </conference>
                    </item>
                </publish>
            </pubsub>
        </iq>"#;

        let elem: Element = xml.parse().expect("valid XML");
        let iq = Iq::try_from(elem).expect("valid IQ");

        let request = parse_pubsub_iq(&iq).expect("should parse");

        if let PubSubRequest::Publish { node, item } = request {
            assert_eq!(node, "urn:xmpp:bookmarks:1");
            assert_eq!(item.id, Some("test@conference.example.org".to_string()));
            assert!(item.payload.is_some());
        } else {
            panic!("Expected Publish request");
        }
    }

    #[test]
    fn test_parse_items_request() {
        let xml = r#"<iq xmlns='jabber:client' type='get' from='user@example.com' to='user@example.com' id='items1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <items node='urn:xmpp:bookmarks:1'/>
            </pubsub>
        </iq>"#;

        let elem: Element = xml.parse().expect("valid XML");
        let iq = Iq::try_from(elem).expect("valid IQ");

        let request = parse_pubsub_iq(&iq).expect("should parse");

        if let PubSubRequest::Items {
            node,
            max_items,
            item_ids,
        } = request
        {
            assert_eq!(node, "urn:xmpp:bookmarks:1");
            assert!(max_items.is_none());
            assert!(item_ids.is_empty());
        } else {
            panic!("Expected Items request");
        }
    }

    #[test]
    fn test_is_pubsub_iq() {
        let xml = r#"<iq xmlns='jabber:client' type='get' id='test1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <items node='test'/>
            </pubsub>
        </iq>"#;

        let elem: Element = xml.parse().expect("valid XML");
        let iq = Iq::try_from(elem).expect("valid IQ");

        assert!(is_pubsub_iq(&iq));
    }

    #[test]
    fn test_pubsub_item_round_trip() {
        let payload = Element::builder("test", "test:ns")
            .attr("foo", "bar")
            .build();

        let item = PubSubItem::new(Some("item-1".to_string()), Some(payload));
        let elem = item.to_element(NS_PUBSUB);

        let parsed = PubSubItem::from_element(&elem);

        assert_eq!(parsed.id, Some("item-1".to_string()));
        assert!(parsed.payload.is_some());
    }
}
