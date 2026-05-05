//! Shared PubSub stanza parsing and building helpers.

use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Message, MessageType};

use crate::pubsub::{Affiliation, NodeConfig};
use crate::{CoreError, CoreResult};

/// Main PubSub namespace.
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// PubSub event namespace.
pub const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

/// PubSub owner namespace.
pub const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";

/// PubSub errors namespace.
pub const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

/// A typed PubSub item with an optional ID, publisher, and payload element.
///
/// `publisher` is meaningful for `<event>` notifications (XEP-0060 §7.1.5)
/// and `<items>` results (§6.5.4). On inbound `<publish>` requests the
/// attribute, if present, is parsed but the publish handler MUST derive the
/// authoritative publisher from the IQ `from` — never trust the item.
#[derive(Debug, Clone)]
pub struct PubSubItem {
    pub id: Option<String>,
    pub publisher: Option<BareJid>,
    pub payload: Option<Element>,
}

impl PubSubItem {
    pub fn new(id: Option<String>, payload: Option<Element>) -> Self {
        Self {
            id,
            publisher: None,
            payload,
        }
    }

    pub fn with_publisher(mut self, publisher: Option<BareJid>) -> Self {
        self.publisher = publisher;
        self
    }

    pub fn from_element(elem: &Element) -> Self {
        let publisher = elem
            .attr("publisher")
            .and_then(|raw| raw.parse::<BareJid>().ok());
        Self {
            id: elem.attr("id").map(str::to_owned),
            publisher,
            payload: elem.children().next().cloned(),
        }
    }

    pub fn to_element(&self, ns: &str) -> Element {
        let mut builder = Element::builder("item", ns);

        if let Some(ref id) = self.id {
            builder = builder.attr("id", id);
        }

        if let Some(ref publisher) = self.publisher {
            builder = builder.attr("publisher", publisher.to_string());
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
    ConfigureNode {
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
    /// XEP-0060 §8.5 `<purge node='...'/>` (owner-only).
    PurgeNode {
        node: String,
    },
    /// XEP-0060 §6.4 `<configure node='...'><x.../>` (owner-only, set form).
    ConfigureNodeSet {
        node: String,
        config: NodeConfig,
    },
    /// XEP-0060 §8.9 `<affiliations node='...'/>` get on owner namespace.
    AffiliationsGet {
        node: String,
    },
    /// XEP-0060 §8.9.4 `<affiliations node='...'><affiliation jid='...' affiliation='...'/></affiliations>` set.
    AffiliationsSet {
        node: String,
        changes: Vec<(jid::BareJid, Affiliation)>,
    },
    Unsupported {
        feature: PubSubUnsupportedFeature,
    },
}

/// PubSub features understood by the parser but not implemented by this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubSubUnsupportedFeature {
    ManageSubscriptions,
}

impl PubSubUnsupportedFeature {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManageSubscriptions => "manage-subscriptions",
        }
    }
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
    /// An unexpected backend/storage failure unrelated to the requested resource.
    /// Maps to XEP-0060 §8.1.3 `<internal-server-error/>` (error type: wait).
    InternalServerError,
    UnsupportedFeature(PubSubUnsupportedFeature),
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

    if let Some(purge) = pubsub_elem.get_child("purge", NS_PUBSUB_OWNER) {
        return Ok(PubSubRequest::PurgeNode {
            node: required_attr(purge, "node")?,
        });
    }

    if let Some(affs) = pubsub_elem.get_child("affiliations", NS_PUBSUB_OWNER) {
        let node = required_attr(affs, "node")?;
        let mut changes: Vec<(jid::BareJid, Affiliation)> = Vec::new();
        for child in affs
            .children()
            .filter(|c| c.is("affiliation", NS_PUBSUB_OWNER))
        {
            let entity_raw = required_attr(child, "jid")?;
            let entity: jid::BareJid = entity_raw
                .parse()
                .map_err(|e: jid::Error| CoreError::bad_request(Some(e.to_string())))?;
            let aff_raw = required_attr(child, "affiliation")?;
            let aff: Affiliation = aff_raw.parse().map_err(|_| {
                CoreError::bad_request(Some(format!("invalid affiliation: {aff_raw}")))
            })?;
            changes.push((entity, aff));
        }
        if changes.is_empty() {
            return Ok(PubSubRequest::AffiliationsGet { node });
        }
        return Ok(PubSubRequest::AffiliationsSet { node, changes });
    }

    if pubsub_elem
        .get_child("subscriptions", NS_PUBSUB_OWNER)
        .is_some()
    {
        return Ok(PubSubRequest::Unsupported {
            feature: PubSubUnsupportedFeature::ManageSubscriptions,
        });
    }

    // Some clients send <configure/> under NS_PUBSUB; XEP-0060 puts it under NS_PUBSUB_OWNER.
    for ns in &[NS_PUBSUB_OWNER, NS_PUBSUB] {
        if let Some(configure) = pubsub_elem.get_child("configure", *ns) {
            let node = required_attr(configure, "node")?;
            if let Some(form) = configure.get_child("x", "jabber:x:data") {
                let config = parse_configure_form(form)?;
                return Ok(PubSubRequest::ConfigureNodeSet { node, config });
            }
            return Ok(PubSubRequest::ConfigureNode { node });
        }
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
///
/// The message type is `headline` per XEP-0060 §12.18 (default
/// `pubsub#notification_type`) and XEP-0163 §4.3 (PEP MUST be headline).
/// Headline messages are not stored offline (RFC 6121 §8.5.2.1.4), which
/// matches PubSub's catch-up-via-`<items/>` recovery model.
pub fn build_pubsub_event(from: &Jid, to: &Jid, event: &PubSubEvent) -> Message {
    let mut items_elem = Element::builder("items", NS_PUBSUB_EVENT).attr("node", &event.node);

    for item in &event.items {
        items_elem = items_elem.append(item.to_element(NS_PUBSUB_EVENT));
    }

    let event_elem = Element::builder("event", NS_PUBSUB_EVENT)
        .append(items_elem.build())
        .build();

    let mut message = Message::new_with_type(MessageType::Headline, Some(to.clone()));
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
        PubSubError::InternalServerError => {
            (ErrorType::Wait, DefinedCondition::InternalServerError)
        }
        PubSubError::UnsupportedFeature(_) => {
            (ErrorType::Cancel, DefinedCondition::FeatureNotImplemented)
        }
    };

    let mut stanza_error = StanzaError::new(error_type, defined_condition, "en", "");
    if let PubSubError::UnsupportedFeature(feature) = error {
        stanza_error.other = Some(
            Element::builder("unsupported", NS_PUBSUB_ERRORS)
                .attr("feature", feature.as_str())
                .build(),
        );
    }

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

fn parse_configure_form(form: &Element) -> CoreResult<NodeConfig> {
    let mut config = NodeConfig::default();
    for field in form.children().filter(|c| c.is("field", "jabber:x:data")) {
        let var = field.attr("var").unwrap_or("");
        let value = field
            .get_child("value", "jabber:x:data")
            .map(|v| v.text())
            .unwrap_or_default();
        match var {
            "pubsub#access_model" => {
                config.access_model = value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!("invalid pubsub#access_model: {value}")))
                })?;
            }
            "pubsub#publish_model" => {
                config.publish_model = value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!("invalid pubsub#publish_model: {value}")))
                })?;
            }
            "pubsub#max_items" => {
                config.max_items = value.parse::<u32>().map_err(|_| {
                    CoreError::bad_request(Some(format!("invalid pubsub#max_items: {value}")))
                })?;
            }
            "pubsub#persist_items" => {
                config.persist_items = match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#persist_items: {value}"
                        ))))
                    }
                };
            }
            "pubsub#deliver_payloads" => {
                config.deliver_payloads = match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#deliver_payloads: {value}"
                        ))))
                    }
                };
            }
            "pubsub#notify_retract" => {
                config.notify_retract = match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#notify_retract: {value}"
                        ))))
                    }
                };
            }
            "pubsub#notify_delete" => {
                config.notify_delete = match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#notify_delete: {value}"
                        ))))
                    }
                };
            }
            "pubsub#send_last_published_item" => {
                config.send_last_published_item = value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!(
                        "invalid pubsub#send_last_published_item: {value}"
                    )))
                })?;
            }
            _ => {} // Unknown fields ignored per XEP-0060.
        }
    }
    Ok(config)
}

/// Build a `<subscribe/>` result IQ that carries `subscription` (XEP-0060 §6.1.6).
pub fn build_pubsub_subscribe_result(
    original_iq: &Iq,
    node: &str,
    subscriber: &jid::Jid,
    subid: &crate::pubsub::SubId,
) -> Iq {
    let subscription = Element::builder("subscription", NS_PUBSUB)
        .attr("node", node)
        .attr("jid", subscriber.to_string())
        .attr("subid", subid.to_string())
        .attr("subscription", "subscribed")
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(subscription)
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build an `<affiliations/>` result IQ for `<affiliations node='...'/>` get.
pub fn build_pubsub_affiliations_result(
    original_iq: &Iq,
    node: &str,
    rows: &[(jid::BareJid, Affiliation)],
) -> Iq {
    let mut affs = Element::builder("affiliations", NS_PUBSUB_OWNER).attr("node", node);
    for (entity, aff) in rows {
        affs = affs.append(
            Element::builder("affiliation", NS_PUBSUB_OWNER)
                .attr("jid", entity.to_string())
                .attr("affiliation", aff.to_string())
                .build(),
        );
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(affs.build())
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build the result for a `<configure/>` get carrying current node config
/// as a `<x type='form'/>` data form (XEP-0060 §6.4).
pub fn build_pubsub_configure_form_result(original_iq: &Iq, node: &str, config: &NodeConfig) -> Iq {
    fn field(var: &str, value: &str) -> Element {
        Element::builder("field", "jabber:x:data")
            .attr("var", var)
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(value)
                    .build(),
            )
            .build()
    }
    /// Build a `type='hidden'` field per XEP-0004 §3.2.
    fn hidden_field(var: &str, value: &str) -> Element {
        Element::builder("field", "jabber:x:data")
            .attr("var", var)
            .attr("type", "hidden")
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(value)
                    .build(),
            )
            .build()
    }
    let form = Element::builder("x", "jabber:x:data")
        .attr("type", "form")
        .append(hidden_field(
            "FORM_TYPE",
            "http://jabber.org/protocol/pubsub#node_config",
        ))
        .append(field(
            "pubsub#access_model",
            &config.access_model.to_string(),
        ))
        .append(field(
            "pubsub#publish_model",
            &config.publish_model.to_string(),
        ))
        .append(field("pubsub#max_items", &config.max_items.to_string()))
        .append(field(
            "pubsub#persist_items",
            if config.persist_items { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#deliver_payloads",
            if config.deliver_payloads { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#notify_retract",
            if config.notify_retract { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#notify_delete",
            if config.notify_delete { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#send_last_published_item",
            &config.send_last_published_item.to_string(),
        ))
        .build();
    let configure = Element::builder("configure", NS_PUBSUB_OWNER)
        .attr("node", node)
        .append(form)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(configure)
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
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
    fn parse_configure_request() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='user@example.com' id='cfg1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <configure node='space'/>
            </pubsub>
        </iq>"#;

        let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
        let request = parse_pubsub_iq(&iq).expect("should parse");

        match request {
            PubSubRequest::ConfigureNode { node } => assert_eq!(node, "space"),
            other => panic!("Expected configure request, got {other:?}"),
        }
    }

    #[test]
    fn parse_owner_subscriptions_as_unsupported_manage_subscriptions() {
        let xml = r#"<iq xmlns='jabber:client' type='get' from='owner@example.com' id='subs1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub#owner'>
                <subscriptions node='space'/>
            </pubsub>
        </iq>"#;

        let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
        let request = parse_pubsub_iq(&iq).expect("should parse unsupported feature");

        match request {
            PubSubRequest::Unsupported { feature } => {
                assert_eq!(feature, PubSubUnsupportedFeature::ManageSubscriptions);
            }
            other => panic!("Expected unsupported feature request, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_feature_error_includes_pubsub_condition() {
        let xml = r#"<iq xmlns='jabber:client' type='get' from='owner@example.com' id='subs1'>
            <pubsub xmlns='http://jabber.org/protocol/pubsub#owner'>
                <subscriptions node='space'/>
            </pubsub>
        </iq>"#;
        let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
        let response = build_pubsub_error(
            &iq,
            PubSubError::UnsupportedFeature(PubSubUnsupportedFeature::ManageSubscriptions),
        );
        let IqType::Error(error) = response.payload else {
            panic!("expected error response");
        };

        assert_eq!(
            error.defined_condition,
            xmpp_parsers::stanza_error::DefinedCondition::FeatureNotImplemented
        );
        let unsupported = error.other.expect("pubsub unsupported condition");
        assert_eq!(unsupported.name(), "unsupported");
        assert_eq!(unsupported.ns(), NS_PUBSUB_ERRORS);
        assert_eq!(unsupported.attr("feature"), Some("manage-subscriptions"));
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
        assert_eq!(
            message.type_,
            MessageType::Headline,
            "XEP-0060 §12.18 default and XEP-0163 §4.3 require headline"
        );

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
        assert!(parsed.publisher.is_none());
    }

    #[test]
    fn pubsub_item_publisher_attribute_round_trips() {
        let publisher: BareJid = "alice@example.com".parse().expect("valid bare");
        let item = PubSubItem::new(Some("e1".to_string()), None).with_publisher(Some(publisher));

        let elem = item.to_element(NS_PUBSUB_EVENT);
        assert_eq!(elem.attr("publisher"), Some("alice@example.com"));

        let parsed = PubSubItem::from_element(&elem);
        assert_eq!(
            parsed.publisher.as_ref().map(BareJid::to_string).as_deref(),
            Some("alice@example.com")
        );
    }

    #[test]
    fn pubsub_item_omits_publisher_when_unset() {
        let item = PubSubItem::new(Some("e2".to_string()), None);
        let elem = item.to_element(NS_PUBSUB_EVENT);
        assert!(elem.attr("publisher").is_none());
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
