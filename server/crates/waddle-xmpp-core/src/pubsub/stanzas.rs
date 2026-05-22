//! Shared PubSub stanza parsing and building helpers.

use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

use crate::pubsub::{Affiliation, NodeConfigPatch};
use crate::{CoreError, CoreResult};

mod builders;
#[cfg(test)]
mod tests;

pub use builders::{
    build_pubsub_affiliations_result, build_pubsub_configure_form_result, build_pubsub_error,
    build_pubsub_event, build_pubsub_items_result, build_pubsub_publish_result,
    build_pubsub_subscribe_result, build_pubsub_success,
};

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
            builder = builder.attr(minidom::rxml::xml_ncname!("id").to_owned(), id);
        }

        if let Some(ref publisher) = self.publisher {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("publisher").to_owned(),
                publisher.to_string(),
            );
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
        publish_options: Option<Box<Element>>,
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
        config: NodeConfigPatch,
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
    ClosedNode,
    NodeExists,
    BadRequest,
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
    match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => {
            payload.name() == "pubsub"
                && (payload.ns() == NS_PUBSUB || payload.ns() == NS_PUBSUB_OWNER)
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
    let pubsub_elem = match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. }
            if payload.name() == "pubsub"
                && (payload.ns() == NS_PUBSUB || payload.ns() == NS_PUBSUB_OWNER) =>
        {
            payload
        }
        Iq::Get { .. } | Iq::Set { .. } => {
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
        let publish_options = pubsub_elem
            .get_child("publish-options", NS_PUBSUB)
            .and_then(|options| options.get_child("x", "jabber:x:data"))
            .cloned()
            .map(Box::new);
        return Ok(PubSubRequest::Publish {
            node,
            item,
            publish_options,
        });
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

fn required_attr(element: &Element, attr: &str) -> CoreResult<String> {
    element
        .attr(attr)
        .map(str::to_owned)
        .ok_or_else(|| CoreError::bad_request(Some(format!("Missing {attr} attribute"))))
}

fn parse_configure_form(form: &Element) -> CoreResult<NodeConfigPatch> {
    let mut config = NodeConfigPatch::default();
    for field in form.children().filter(|c| c.is("field", "jabber:x:data")) {
        let var = field.attr("var").unwrap_or("");
        let value = field
            .get_child("value", "jabber:x:data")
            .map(|v| v.text())
            .unwrap_or_default();
        match var {
            "pubsub#access_model" => {
                config.access_model = Some(value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!("invalid pubsub#access_model: {value}")))
                })?);
            }
            "pubsub#publish_model" => {
                config.publish_model = Some(value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!("invalid pubsub#publish_model: {value}")))
                })?);
            }
            "pubsub#max_items" => {
                // XEP-0060 §16.4.3 + XEP-0490 §3 publish-options use the
                // literal token "max" to mean "no upper bound". Accept it
                // alongside the numeric form so MDS-style publish-options
                // round-trip without a precondition-not-met error.
                config.max_items = Some(if value.eq_ignore_ascii_case("max") {
                    u32::MAX
                } else {
                    value.parse::<u32>().map_err(|_| {
                        CoreError::bad_request(Some(format!("invalid pubsub#max_items: {value}")))
                    })?
                });
            }
            "pubsub#persist_items" => {
                config.persist_items = Some(match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#persist_items: {value}"
                        ))))
                    }
                });
            }
            "pubsub#deliver_payloads" => {
                config.deliver_payloads = Some(match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#deliver_payloads: {value}"
                        ))))
                    }
                });
            }
            "pubsub#notify_retract" => {
                config.notify_retract = Some(match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#notify_retract: {value}"
                        ))))
                    }
                });
            }
            "pubsub#notify_delete" => {
                config.notify_delete = Some(match value.as_str() {
                    "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(CoreError::bad_request(Some(format!(
                            "invalid pubsub#notify_delete: {value}"
                        ))))
                    }
                });
            }
            "pubsub#send_last_published_item" => {
                config.send_last_published_item = Some(value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!(
                        "invalid pubsub#send_last_published_item: {value}"
                    )))
                })?);
            }
            _ => {} // Unknown fields ignored per XEP-0060.
        }
    }
    Ok(config)
}
