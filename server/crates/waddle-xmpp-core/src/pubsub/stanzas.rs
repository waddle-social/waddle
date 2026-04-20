//! Shared PubSub stanza parsing and building helpers.

use jid::Jid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::Message;

use crate::{CoreError, CoreResult};

/// Main PubSub namespace.
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// PubSub event namespace.
pub const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

/// PubSub owner namespace.
pub const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";

/// PubSub errors namespace.
pub const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

/// A typed PubSub item with an optional ID and payload element.
#[derive(Debug, Clone)]
pub struct PubSubItem {
    pub id: Option<String>,
    pub payload: Option<Element>,
}

impl PubSubItem {
    pub fn new(id: Option<String>, payload: Option<Element>) -> Self {
        Self { id, payload }
    }

    pub fn from_element(elem: &Element) -> Self {
        Self {
            id: elem.attr("id").map(str::to_owned),
            payload: elem.children().next().cloned(),
        }
    }

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

/// Parsed PubSub event notification.
#[derive(Debug, Clone)]
pub struct PubSubEvent {
    pub node: String,
    pub items: Vec<PubSubItem>,
}

impl PubSubEvent {
    pub fn new(node: impl Into<String>, items: Vec<PubSubItem>) -> Self {
        Self {
            node: node.into(),
            items,
        }
    }
}

/// Parsed PubSub request types.
#[derive(Debug, Clone)]
pub enum PubSubRequest {
    Publish {
        node: String,
        item: PubSubItem,
    },
    Retract {
        node: String,
        item_id: String,
        notify: bool,
    },
    Items {
        node: String,
        max_items: Option<u32>,
        item_ids: Vec<String>,
    },
    CreateNode {
        node: String,
    },
    DeleteNode {
        node: String,
    },
    Subscribe {
        node: String,
        jid: Jid,
    },
    Unsubscribe {
        node: String,
        jid: Jid,
        subid: Option<String>,
    },
}

/// PubSub-specific error conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubSubError {
    NodeNotFound,
    ItemNotFound,
    Forbidden,
    NodeExists,
    InvalidJid,
    PreconditionNotMet,
    NotSubscribed,
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

/// Check if a message is a PubSub event notification.
pub fn is_pubsub_event(message: &Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.name() == "event" && payload.ns() == NS_PUBSUB_EVENT)
}

/// Parse a PubSub event notification from a message.
pub fn parse_pubsub_event(message: &Message) -> CoreResult<PubSubEvent> {
    let event_elem = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "event" && payload.ns() == NS_PUBSUB_EVENT)
        .ok_or_else(|| CoreError::bad_request(Some("Missing PubSub event payload".to_string())))?;

    let items_elem = event_elem
        .get_child("items", NS_PUBSUB_EVENT)
        .ok_or_else(|| CoreError::bad_request(Some("Missing PubSub event items".to_string())))?;

    let node = items_elem
        .attr("node")
        .ok_or_else(|| CoreError::bad_request(Some("Missing node attribute".to_string())))?
        .to_string();

    let items = items_elem
        .children()
        .filter(|child| child.name() == "item" && child.ns() == NS_PUBSUB_EVENT)
        .map(PubSubItem::from_element)
        .collect();

    Ok(PubSubEvent::new(node, items))
}

/// Parse a PubSub IQ stanza into a structured request.
pub fn parse_pubsub_iq(iq: &Iq) -> CoreResult<PubSubRequest> {
    let pubsub_elem = match &iq.payload {
        IqType::Get(elem) | IqType::Set(elem)
            if elem.name() == "pubsub"
                && (elem.ns() == NS_PUBSUB || elem.ns() == NS_PUBSUB_OWNER) =>
        {
            elem
        }
        IqType::Get(_) | IqType::Set(_) => {
            return Err(CoreError::bad_request(Some(
                "Expected pubsub element".to_string(),
            )));
        }
        _ => {
            return Err(CoreError::bad_request(Some(
                "PubSub IQ must be get or set".to_string(),
            )));
        }
    };

    if let Some(publish) = pubsub_elem.get_child("publish", NS_PUBSUB) {
        let node = required_attr(publish, "node")?;
        let item = publish
            .get_child("item", NS_PUBSUB)
            .map(PubSubItem::from_element)
            .unwrap_or_else(|| PubSubItem::new(None, None));
        return Ok(PubSubRequest::Publish { node, item });
    }

    if let Some(retract) = pubsub_elem.get_child("retract", NS_PUBSUB) {
        let node = required_attr(retract, "node")?;
        let notify = retract
            .attr("notify")
            .map(|value| value == "true" || value == "1")
            .unwrap_or(false);
        let item_id = retract
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id"))
            .ok_or_else(|| CoreError::bad_request(Some("Missing item id".to_string())))?
            .to_string();

        return Ok(PubSubRequest::Retract {
            node,
            item_id,
            notify,
        });
    }

    if let Some(items) = pubsub_elem.get_child("items", NS_PUBSUB) {
        let node = required_attr(items, "node")?;
        let max_items = items.attr("max_items").and_then(|value| value.parse().ok());
        let item_ids = items
            .children()
            .filter(|child| child.name() == "item")
            .filter_map(|child| child.attr("id").map(str::to_owned))
            .collect();

        return Ok(PubSubRequest::Items {
            node,
            max_items,
            item_ids,
        });
    }

    if let Some(create) = pubsub_elem.get_child("create", NS_PUBSUB) {
        return Ok(PubSubRequest::CreateNode {
            node: required_attr(create, "node")?,
        });
    }

    if let Some(delete) = pubsub_elem.get_child("delete", NS_PUBSUB_OWNER) {
        return Ok(PubSubRequest::DeleteNode {
            node: required_attr(delete, "node")?,
        });
    }

    if let Some(subscribe) = pubsub_elem.get_child("subscribe", NS_PUBSUB) {
        let node = required_attr(subscribe, "node")?;
        let jid = required_attr(subscribe, "jid")?.parse().map_err(|error| {
            CoreError::bad_request(Some(format!("Invalid jid attribute: {error}")))
        })?;
        return Ok(PubSubRequest::Subscribe { node, jid });
    }

    if let Some(unsubscribe) = pubsub_elem.get_child("unsubscribe", NS_PUBSUB) {
        let node = required_attr(unsubscribe, "node")?;
        let jid = required_attr(unsubscribe, "jid")?
            .parse()
            .map_err(|error| {
                CoreError::bad_request(Some(format!("Invalid jid attribute: {error}")))
            })?;
        let subid = unsubscribe.attr("subid").map(str::to_owned);

        return Ok(PubSubRequest::Unsubscribe { node, jid, subid });
    }

    Err(CoreError::bad_request(Some(
        "Unsupported PubSub operation".to_string(),
    )))
}

/// Build a PubSub event notification message.
pub fn build_pubsub_event(from: &Jid, to: &Jid, event: &PubSubEvent) -> Message {
    let mut items_elem = Element::builder("items", NS_PUBSUB_EVENT).attr("node", &event.node);

    for item in &event.items {
        items_elem = items_elem.append(item.to_element(NS_PUBSUB_EVENT));
    }

    let event_elem = Element::builder("event", NS_PUBSUB_EVENT)
        .append(items_elem.build())
        .build();

    let mut message = Message::new(Some(to.clone()));
    message.from = Some(from.clone());
    message.payloads.push(event_elem);
    message
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

/// Build a PubSub error IQ response.
pub fn build_pubsub_error(original_iq: &Iq, error: PubSubError) -> Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let (error_type, defined_condition) = match error {
        PubSubError::NodeNotFound | PubSubError::ItemNotFound => {
            (ErrorType::Cancel, DefinedCondition::ItemNotFound)
        }
        PubSubError::Forbidden => (ErrorType::Auth, DefinedCondition::Forbidden),
        PubSubError::NodeExists | PubSubError::PreconditionNotMet => {
            (ErrorType::Cancel, DefinedCondition::Conflict)
        }
        PubSubError::InvalidJid => (ErrorType::Modify, DefinedCondition::BadRequest),
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

/// Build an empty result IQ for successful requests without a payload.
pub fn build_pubsub_success(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(None),
    }
}

fn required_attr(element: &Element, attr: &str) -> CoreResult<String> {
    element
        .attr(attr)
        .map(str::to_owned)
        .ok_or_else(|| CoreError::bad_request(Some(format!("Missing {attr} attribute"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::IqType;

    #[test]
    fn parse_publish_request() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='user@example.com' to='user@example.com' id='pub1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <publish node='urn:xmpp:bookmarks:1'>
                    <item id='test@conference.example.org'>
                        <conference xmlns='urn:xmpp:bookmarks:1' autojoin='true'/>
                    </item>
                </publish>
            </pubsub>
        </iq>"#;

        let elem: Element = xml.parse().expect("valid XML");
        let iq = Iq::try_from(elem).expect("valid IQ");
        let request = parse_pubsub_iq(&iq).expect("should parse");

        match request {
            PubSubRequest::Publish { node, item } => {
                assert_eq!(node, "urn:xmpp:bookmarks:1");
                assert_eq!(item.id.as_deref(), Some("test@conference.example.org"));
                assert!(item.payload.is_some());
            }
            other => panic!("Expected publish request, got {other:?}"),
        }
    }

    #[test]
    fn parse_subscribe_request_uses_typed_jid() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='romeo@example.com' id='sub1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <subscribe node='urn:xmpp:nick' jid='romeo@example.com'/>
            </pubsub>
        </iq>"#;

        let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
        let request = parse_pubsub_iq(&iq).expect("should parse");

        match request {
            PubSubRequest::Subscribe { node, jid } => {
                assert_eq!(node, "urn:xmpp:nick");
                assert_eq!(jid.to_string(), "romeo@example.com");
            }
            other => panic!("Expected subscribe request, got {other:?}"),
        }
    }

    #[test]
    fn build_and_parse_pubsub_event_message() {
        let from: Jid = "juliet@example.com".parse().expect("valid jid");
        let to: Jid = "romeo@example.com/balcony".parse().expect("valid jid");
        let payload = Element::builder("nick", "http://jabber.org/protocol/nick")
            .append("Juliet")
            .build();
        let event = PubSubEvent::new(
            "http://jabber.org/protocol/nick",
            vec![PubSubItem::new(Some("latest".to_string()), Some(payload))],
        );

        let message = build_pubsub_event(&from, &to, &event);

        assert!(is_pubsub_event(&message));

        let parsed = parse_pubsub_event(&message).expect("event should parse");
        assert_eq!(parsed.node, "http://jabber.org/protocol/nick");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].id.as_deref(), Some("latest"));
    }

    #[test]
    fn pubsub_item_round_trips() {
        let payload = Element::builder("test", "test:ns")
            .attr("foo", "bar")
            .build();
        let item = PubSubItem::new(Some("item-1".to_string()), Some(payload));
        let elem = item.to_element(NS_PUBSUB);
        let parsed = PubSubItem::from_element(&elem);

        assert_eq!(parsed.id.as_deref(), Some("item-1"));
        assert!(parsed.payload.is_some());
    }

    #[test]
    fn is_pubsub_iq_detects_pubsub_requests() {
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
    fn build_pubsub_success_preserves_iq_routing() {
        let iq = Iq {
            from: Some("romeo@example.com".parse().expect("valid jid")),
            to: Some("juliet@example.com".parse().expect("valid jid")),
            id: "ok-1".to_string(),
            payload: IqType::Get(Element::builder("ping", "urn:xmpp:ping").build()),
        };

        let response = build_pubsub_success(&iq);
        assert_eq!(response.id, "ok-1");
        assert_eq!(response.from, iq.to);
        assert_eq!(response.to, iq.from);
    }
}
