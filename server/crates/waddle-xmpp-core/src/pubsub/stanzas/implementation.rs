//! Implementation behind the parent module facade.

use super::Element;
use super::Iq;
use super::Message;
use super::{Affiliation, NodeConfigPatch, SubscriptionState};
use super::{BareJid, Jid};
use super::{CoreError, CoreResult};

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
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: Option<String>,
    /// Bare JID of the item publisher, when supplied.
    pub publisher: Option<BareJid>,
    /// Optional typed payload carried by this item.
    pub payload: Option<Element>,
}

impl PubSubItem {
    /// Create an item with the supplied identifier and payload and no publisher attribution.
    #[must_use]
    pub fn new(id: Option<String>, payload: Option<Element>) -> Self {
        Self {
            id,
            publisher: None,
            payload,
        }
    }

    /// Set the publisher and return the updated value.
    #[must_use]
    pub fn with_publisher(mut self, publisher: Option<BareJid>) -> Self {
        self.publisher = publisher;
        self
    }

    /// Parse the supported protocol fields from an XML element.
    #[must_use]
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

    /// Serialize this value as a typed XML element.
    #[must_use]
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
    /// PubSub node whose items are included in the notification.
    pub node: String,
    /// Items published to the notified node.
    pub items: Vec<PubSubItem>,
}

impl PubSubEvent {
    /// Create a publication event for the node and its supplied items.
    #[must_use]
    pub fn new(node: impl Into<String>, items: Vec<PubSubItem>) -> Self {
        Self {
            node: node.into(),
            items,
        }
    }
}

/// Parsed PubSub request types.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PubSubRequest {
    /// Publish an item to the selected PubSub node.
    Publish {
        /// Destination PubSub node for the new item.
        node: String,
        /// Item to publish.
        item: PubSubItem,
        /// Publish-options data form supplied with the publication.
        publish_options: Option<Box<Element>>,
    },
    /// Retract an item from the selected PubSub node.
    Retract {
        /// PubSub node containing the item to retract.
        node: String,
        /// Identifier of the PubSub item.
        item_id: String,
        /// Whether the operation requests subscriber notification.
        notify: bool,
    },
    /// Retrieve items from the selected PubSub node.
    Items {
        /// PubSub node from which to retrieve items.
        node: String,
        /// Maximum number of retained or returned items.
        max_items: Option<u32>,
        /// Item identifiers selected by the request.
        item_ids: Vec<String>,
    },
    /// Create a PubSub node.
    CreateNode {
        /// Requested identifier for the new PubSub node.
        node: String,
    },
    /// Request the node's configuration form.
    ConfigureNode {
        /// PubSub node whose configuration form is requested.
        node: String,
    },
    /// Delete the selected PubSub node.
    DeleteNode {
        /// PubSub node to delete, including its items and subscriptions.
        node: String,
    },
    /// Request a subscription to a PubSub node's notifications.
    Subscribe {
        /// PubSub node to subscribe to.
        node: String,
        /// JID that will receive node notifications.
        jid: Jid,
    },
    /// Request removal of a PubSub node subscription.
    Unsubscribe {
        /// PubSub node whose subscription is being removed.
        node: String,
        /// Subscriber JID whose subscription is being removed.
        jid: Jid,
        /// Service-assigned identifier distinguishing this subscription.
        subid: Option<String>,
    },
    /// XEP-0060 §8.5 `<purge node='...'/>` (owner-only).
    PurgeNode {
        /// PubSub node whose stored items are to be removed.
        node: String,
    },
    /// XEP-0060 §6.4 `<configure node='...'><x.../>` (owner-only, set form).
    ConfigureNodeSet {
        /// PubSub node to reconfigure.
        node: String,
        /// Configuration applied to this operation.
        config: NodeConfigPatch,
    },
    /// XEP-0060 §8.9 `<affiliations node='...'/>` get on owner namespace.
    AffiliationsGet {
        /// PubSub node whose affiliations are requested.
        node: String,
    },
    /// XEP-0060 §8.9.4 `<affiliations node='...'><affiliation jid='...' affiliation='...'/></affiliations>` set.
    AffiliationsSet {
        /// PubSub node whose affiliations are to be changed.
        node: String,
        /// Requested changes, applied in the order supplied.
        changes: Vec<(jid::BareJid, Affiliation)>,
    },
    /// XEP-0060 §8.8 `<subscriptions node='...'/>` get on owner namespace.
    OwnerSubscriptionsGet {
        /// PubSub node whose subscriptions are requested by its owner.
        node: String,
    },
    /// XEP-0060 §8.8 owner subscription delta update.
    OwnerSubscriptionsSet {
        /// PubSub node whose subscriptions are to be changed by its owner.
        node: String,
        /// Requested changes, applied in the order supplied.
        changes: Vec<(jid::Jid, SubscriptionState, Option<String>)>,
    },
    /// The requested operation requires an unsupported feature.
    Unsupported {
        /// Feature required by the request or advertised by the service.
        feature: PubSubUnsupportedFeature,
    },
}

/// PubSub features understood by the parser but not implemented by this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PubSubUnsupportedFeature {
    /// The service does not support subscription administration.
    ManageSubscriptions,
    /// The addressed node does not support publish options.
    PublishOptions,
}

impl PubSubUnsupportedFeature {
    /// Borrow the stored text or canonical protocol token without allocating.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManageSubscriptions => "manage-subscriptions",
            Self::PublishOptions => "publish-options",
        }
    }
}

/// PubSub-specific error conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PubSubError {
    /// The selected PubSub node does not exist.
    NodeNotFound,
    /// The selected PubSub item does not exist.
    ItemNotFound,
    /// The requester does not have permission to perform the operation.
    Forbidden,
    /// XEP-0060 §6.1.3.2: `<not-authorized/>` + pubsub-namespaced
    /// `<presence-subscription-required/>`: the node's access model is
    /// `presence` and the requester has no presence subscription to the owner.
    PresenceSubscriptionRequired,
    /// XEP-0060 §6.1.3.3: `<not-authorized/>` + pubsub-namespaced
    /// `<not-in-roster-group/>`: the node's access model is `roster` and
    /// the requester is in none of the allowed roster groups.
    NotInRosterGroup,
    /// XEP-0060 §8.1.3 / §6.5.9: `<not-acceptable/>` + pubsub-namespaced
    /// `<unsupported-access-model/>`: the service does not support the
    /// requested access model.
    UnsupportedAccessModel,
    /// The node does not accept additional subscriptions.
    ClosedNode,
    /// The requested node already exists.
    NodeExists,
    /// The request is malformed, with optional explanatory text.
    BadRequest,
    /// The supplied address is not a valid JID (plain `<bad-request/>`).
    InvalidJid,
    /// XEP-0060 §6.1.3.1: `<bad-request/>` + pubsub-namespaced
    /// `<invalid-jid/>`: a subscriber or affiliate JID is unparsable, or its
    /// bare JID differs from the requester's.
    InvalidSubscriberJid,
    /// XEP-0060 §7.1.3.6 / §7.2.3.2: `<bad-request/>` + pubsub-namespaced
    /// `<item-required/>`: a publish or retract carried no `<item/>` (or no
    /// item id) where one is required.
    ItemRequired,
    /// XEP-0060 §7.2.3.1 / §8.4.3: `<bad-request/>` + pubsub-namespaced
    /// `<nodeid-required/>`: the request named no node.
    NodeIdRequired,
    /// XEP-0060 §8.1.2.4: `<not-acceptable/>` + pubsub-namespaced
    /// `<nodeid-required/>`: a create request without a node on a service
    /// that does not support instant nodes.
    InstantNodesUnsupported,
    /// XEP-0060 §7.1.3.4: `<bad-request/>` + pubsub-namespaced
    /// `<invalid-payload/>` extension. Used when a publish item's
    /// payload fails the per-node payload-conformance contract
    /// (e.g. the typed parser rejects the inner element).
    InvalidPayload,
    /// XEP-0060 §7.1.3.3: `<bad-request/>` + pubsub-namespaced
    /// `<payload-required/>` extension. Used when a publish that
    /// requires a payload arrives without one.
    PayloadRequired,
    /// The node does not satisfy the supplied publish-options preconditions.
    PreconditionNotMet,
    /// The requester has no matching subscription to remove.
    NotSubscribed,
    /// The subscription identifier is stale or does not match the subscriber.
    InvalidSubscriptionId,
    /// This subscription requires the identifier returned at subscribe time.
    SubscriptionIdRequired,
    /// An unexpected backend/storage failure unrelated to the requested resource.
    /// Maps to XEP-0060 §8.1.3 `<internal-server-error/>` (error type: wait).
    InternalServerError,
    /// The operation requires a PubSub feature unsupported by this service.
    UnsupportedFeature(PubSubUnsupportedFeature),
}

impl From<CoreError> for PubSubError {
    fn from(_: CoreError) -> Self {
        Self::BadRequest
    }
}

/// Check if an IQ is a PubSub request.
#[must_use]
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
#[must_use]
pub fn is_pubsub_event(message: &Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.name() == "event" && payload.ns() == NS_PUBSUB_EVENT)
}

/// Parse a PubSub event notification from a message.
///
/// # Errors
///
/// Returns `CoreError::BadRequest` when the event, items, or required node attribute is missing.
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
///
/// # Errors
///
/// Returns the XEP-0060 error condition the request earns: `NodeIdRequired`
/// for a missing `node`, `ItemRequired` for a publish without `<item/>` or a
/// retract without an item id, `InstantNodesUnsupported` for a `<create/>`
/// without a node, `InvalidSubscriberJid` for an unparsable subscriber JID, and
/// `BadRequest` for any other malformed request.
pub fn parse_pubsub_iq(iq: &Iq) -> Result<PubSubRequest, PubSubError> {
    let pubsub_elem = match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. }
            if payload.name() == "pubsub"
                && (payload.ns() == NS_PUBSUB || payload.ns() == NS_PUBSUB_OWNER) =>
        {
            payload
        }
        Iq::Get { .. } | Iq::Set { .. } => {
            return Err(PubSubError::BadRequest);
        }
        _ => {
            return Err(PubSubError::BadRequest);
        }
    };

    if let Some(publish) = pubsub_elem.get_child("publish", NS_PUBSUB) {
        let node = node_attr(publish)?;
        // §7.1.3.6: every Waddle node persists items, so a publish without
        // an `<item/>` is rejected.
        let item = publish
            .get_child("item", NS_PUBSUB)
            .map(PubSubItem::from_element)
            .ok_or(PubSubError::ItemRequired)?;
        // XEP-0060 §7.1.5: <publish-options/> MUST contain a data form.
        let publish_options = match pubsub_elem.get_child("publish-options", NS_PUBSUB) {
            None => None,
            Some(options) => Some(Box::new(
                options
                    .get_child("x", "jabber:x:data")
                    .cloned()
                    .ok_or(PubSubError::BadRequest)?,
            )),
        };
        return Ok(PubSubRequest::Publish {
            node,
            item,
            publish_options,
        });
    }

    if let Some(retract) = pubsub_elem.get_child("retract", NS_PUBSUB) {
        let node = node_attr(retract)?;
        let notify = retract
            .attr("notify")
            .map(|value| value == "true" || value == "1")
            .unwrap_or(false);
        let item_id = retract
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id"))
            .ok_or(PubSubError::ItemRequired)?
            .to_string();

        return Ok(PubSubRequest::Retract {
            node,
            item_id,
            notify,
        });
    }

    if let Some(items) = pubsub_elem.get_child("items", NS_PUBSUB) {
        let node = node_attr(items)?;
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
            node: create
                .attr("node")
                .map(str::to_owned)
                .ok_or(PubSubError::InstantNodesUnsupported)?,
        });
    }

    if let Some(delete) = pubsub_elem.get_child("delete", NS_PUBSUB_OWNER) {
        return Ok(PubSubRequest::DeleteNode {
            node: node_attr(delete)?,
        });
    }

    if let Some(purge) = pubsub_elem.get_child("purge", NS_PUBSUB_OWNER) {
        return Ok(PubSubRequest::PurgeNode {
            node: node_attr(purge)?,
        });
    }

    if let Some(affs) = pubsub_elem.get_child("affiliations", NS_PUBSUB_OWNER) {
        let node = node_attr(affs)?;
        let mut changes: Vec<(jid::BareJid, Affiliation)> = Vec::new();
        for child in affs
            .children()
            .filter(|c| c.is("affiliation", NS_PUBSUB_OWNER))
        {
            let entity_raw = required_attr(child, "jid")?;
            let entity: jid::BareJid = entity_raw
                .parse()
                .map_err(|_: jid::Error| PubSubError::InvalidSubscriberJid)?;
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

    if let Some(subscriptions) = pubsub_elem.get_child("subscriptions", NS_PUBSUB_OWNER) {
        let node = node_attr(subscriptions)?;
        let mut changes = Vec::new();
        for child in subscriptions
            .children()
            .filter(|c| c.is("subscription", NS_PUBSUB_OWNER))
        {
            let jid_raw = required_attr(child, "jid")?;
            let jid: jid::Jid = jid_raw
                .parse()
                .map_err(|_: jid::Error| PubSubError::InvalidSubscriberJid)?;
            let state_raw = required_attr(child, "subscription")?;
            let state: SubscriptionState = state_raw.parse().map_err(|_| {
                CoreError::bad_request(Some(format!("invalid subscription: {state_raw}")))
            })?;
            changes.push((jid, state, child.attr("subid").map(str::to_owned)));
        }
        if changes.is_empty() {
            return Ok(PubSubRequest::OwnerSubscriptionsGet { node });
        }
        return Ok(PubSubRequest::OwnerSubscriptionsSet { node, changes });
    }

    // Some clients send <configure/> under NS_PUBSUB; XEP-0060 puts it under NS_PUBSUB_OWNER.
    for ns in &[NS_PUBSUB_OWNER, NS_PUBSUB] {
        if let Some(configure) = pubsub_elem.get_child("configure", *ns) {
            let node = node_attr(configure)?;
            if let Some(form) = configure.get_child("x", "jabber:x:data") {
                let config = parse_configure_form(form)?;
                return Ok(PubSubRequest::ConfigureNodeSet { node, config });
            }
            return Ok(PubSubRequest::ConfigureNode { node });
        }
    }

    if let Some(subscribe) = pubsub_elem.get_child("subscribe", NS_PUBSUB) {
        let node = node_attr(subscribe)?;
        let jid = required_attr(subscribe, "jid")?
            .parse()
            .map_err(|_: jid::Error| PubSubError::InvalidSubscriberJid)?;
        return Ok(PubSubRequest::Subscribe { node, jid });
    }

    if let Some(unsubscribe) = pubsub_elem.get_child("unsubscribe", NS_PUBSUB) {
        let node = node_attr(unsubscribe)?;
        let jid = required_attr(unsubscribe, "jid")?
            .parse()
            .map_err(|_: jid::Error| PubSubError::InvalidSubscriberJid)?;
        let subid = unsubscribe.attr("subid").map(str::to_owned);

        return Ok(PubSubRequest::Unsubscribe { node, jid, subid });
    }

    Err(PubSubError::BadRequest)
}

/// The mandatory `node` attribute; its absence is `<nodeid-required/>`.
fn node_attr(element: &Element) -> Result<String, PubSubError> {
    element
        .attr("node")
        .map(str::to_owned)
        .ok_or(PubSubError::NodeIdRequired)
}

pub(in crate::pubsub::stanzas) fn required_attr(
    element: &Element,
    attr: &str,
) -> CoreResult<String> {
    element
        .attr(attr)
        .map(str::to_owned)
        .ok_or_else(|| CoreError::bad_request(Some(format!("Missing {attr} attribute"))))
}

pub(in crate::pubsub::stanzas) fn parse_configure_form(
    form: &Element,
) -> CoreResult<NodeConfigPatch> {
    parse_node_config_fields(form, false)
}

/// FORM_TYPE of the XEP-0060 §7.1.5 publish-options data form.
pub const PUBLISH_OPTIONS_FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#publish-options";

/// Parse the data form of a XEP-0060 §7.1.5 `<publish-options/>` element into
/// node-configuration preconditions.
///
/// The form MUST be a `submit` form whose `FORM_TYPE` is
/// [`PUBLISH_OPTIONS_FORM_TYPE`], and a service that supports publish options
/// MUST reject the publication on an unknown field; both surface as
/// `<bad-request/>`.
///
/// # Errors
///
/// Returns a bad-request error for a wrong form type, a wrong `FORM_TYPE`, an
/// unknown field or an invalid field value.
pub fn parse_publish_options_form(form: &Element) -> CoreResult<NodeConfigPatch> {
    if !form.is("x", "jabber:x:data") || form.attr("type") != Some("submit") {
        return Err(CoreError::bad_request(Some(
            "publish-options must contain a jabber:x:data form of type submit".to_string(),
        )));
    }
    let form_type = form
        .children()
        .find(|field| field.is("field", "jabber:x:data") && field.attr("var") == Some("FORM_TYPE"))
        .and_then(|field| field.get_child("value", "jabber:x:data"))
        .map(|value| value.text());
    if form_type.as_deref() != Some(PUBLISH_OPTIONS_FORM_TYPE) {
        return Err(CoreError::bad_request(Some(format!(
            "publish-options FORM_TYPE must be {PUBLISH_OPTIONS_FORM_TYPE}"
        ))));
    }
    parse_node_config_fields(form, true)
}

fn parse_node_config_fields(form: &Element, reject_unknown: bool) -> CoreResult<NodeConfigPatch> {
    let mut config = NodeConfigPatch::default();
    for field in form.children().filter(|c| c.is("field", "jabber:x:data")) {
        let var = field.attr("var").unwrap_or("");
        if var == "pubsub#roster_groups_allowed" {
            // list-multi (XEP-0004 §3.3): every <value/> is one group.
            let mut groups: Vec<String> = Vec::new();
            for value in field.children().filter(|c| c.is("value", "jabber:x:data")) {
                let group = value.text();
                if group.is_empty() {
                    return Err(CoreError::bad_request(Some(
                        "empty pubsub#roster_groups_allowed value".to_owned(),
                    )));
                }
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
            config.roster_groups_allowed = Some(groups);
            continue;
        }
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
            "pubsub#type" => {
                config.node_type = Some(value.parse().map_err(|_| {
                    CoreError::bad_request(Some(format!("invalid pubsub#type: {value}")))
                })?);
            }
            // Purging on publisher departure is not supported: only the
            // value the node already has (false) may be submitted.
            "pubsub#purge_offline" => match value.as_str() {
                "0" | "false" => {}
                "1" | "true" => {
                    return Err(CoreError::bad_request(Some(
                        "pubsub#purge_offline is not supported".to_owned(),
                    )))
                }
                _ => {
                    return Err(CoreError::bad_request(Some(format!(
                        "invalid pubsub#purge_offline: {value}"
                    ))))
                }
            },
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
            "pubsub#item_expire" => {
                // XEP-0060 §16.4.4 + the integer-or-max datatype: seconds,
                // or `max` for no limit other than a server imposed one.
                config.item_expire = Some(if value.eq_ignore_ascii_case("max") {
                    None
                } else {
                    match value.parse::<u32>() {
                        Ok(secs) if secs > 0 => Some(secs),
                        _ => {
                            return Err(CoreError::bad_request(Some(format!(
                                "invalid pubsub#item_expire: {value}"
                            ))))
                        }
                    }
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
            "FORM_TYPE" => {}
            // XEP-0060 §8.2.4 ignores unknown configure-form fields; §7.1.5
            // requires publish-options to reject them.
            _ if reject_unknown => {
                return Err(CoreError::bad_request(Some(format!(
                    "unknown publish-options field: {var}"
                ))));
            }
            _ => {}
        }
    }
    Ok(config)
}
