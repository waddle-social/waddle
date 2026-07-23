//! XEP-0060 Publish-Subscribe — typed service-node client helpers.
//!
//! Shared IQ builders and result parsers for the XEP-0060 operations
//! every Waddle community surface performs against a pubsub service
//! (the `community.<domain>` component) or the account's own PEP
//! service: items fetch (§6.5), publish with optional XEP-0223
//! `<publish-options/>` preconditions (§7.1 / §7.1.5), subscribe
//! (§6.1), and notify-retract (§7.2).
//!
//! The XEP-0472 feed, XEP-0501 stories, story-reads, and xCal events
//! modules layer their typed payloads (`<entry/>`, `<vcalendar/>`,
//! `<reads/>`) on top of these envelopes; this module owns only the
//! XEP-0060 pubsub wrapper shapes.

use std::str::FromStr;

use jid::{BareJid, Jid};
use minidom::Element;

use crate::bootstrap::NS_CLIENT;
use crate::pep::NS_PUBSUB;

#[cfg(test)]
mod tests;

/// XEP-0060 `<publish-options/>` FORM_TYPE (also the XEP-0060 disco
/// feature var for publish-options support; pinned equal to
/// [`crate::mds::NS_PUBSUB_PUBLISH_OPTIONS_FEATURE`] by tests).
pub const NS_PUBSUB_PUBLISH_OPTIONS: &str = "http://jabber.org/protocol/pubsub#publish-options";

const NS_DATA_FORMS: &str = "jabber:x:data";

/// Conventional JID of the Waddle community pubsub component
/// (`community.<domain>`) hosting the XEP-0472 social feed, XEP-0501
/// stories, and xCal events nodes. Mirrors
/// [`crate::extension_commands::fallback_extension_service_jid`] for
/// the extensions component.
pub fn community_service_jid(domain: &str) -> Result<BareJid, jid::Error> {
    BareJid::from_str(&format!("community.{domain}"))
}

/// XEP-0060 §4.5 node access models accepted in `pubsub#access_model`
/// publish-options preconditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubsubAccessModel {
    Open,
    Presence,
    Roster,
    Authorize,
    Whitelist,
}

impl PubsubAccessModel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Presence => "presence",
            Self::Roster => "roster",
            Self::Authorize => "authorize",
            Self::Whitelist => "whitelist",
        }
    }
}

/// XEP-0060 `pubsub#send_last_published_item` node-configuration
/// values accepted in publish-options preconditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubsubSendLastPublishedItem {
    Never,
    OnSub,
    OnSubAndPresence,
}

impl PubsubSendLastPublishedItem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnSub => "on_sub",
            Self::OnSubAndPresence => "on_sub_and_presence",
        }
    }
}

/// Typed XEP-0223/XEP-0060 §7.1.5 `<publish-options/>` precondition
/// form. Only the node-configuration fields Waddle publishes are
/// modelled; each `Some` field becomes one `jabber:x:data` submit
/// field alongside the mandatory hidden FORM_TYPE.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PubsubPublishOptions {
    pub persist_items: Option<bool>,
    pub access_model: Option<PubsubAccessModel>,
    pub send_last_published_item: Option<PubsubSendLastPublishedItem>,
    pub max_items: Option<u32>,
}

impl PubsubPublishOptions {
    /// Build the `<publish-options xmlns='…pubsub'/>` element carrying
    /// the submit form.
    pub fn to_element(&self) -> Element {
        let mut form = Element::builder("x", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(hidden_field("FORM_TYPE", NS_PUBSUB_PUBLISH_OPTIONS));
        if let Some(persist_items) = self.persist_items {
            form = form.append(text_single_field(
                "pubsub#persist_items",
                if persist_items { "true" } else { "false" },
            ));
        }
        if let Some(access_model) = self.access_model {
            form = form.append(text_single_field(
                "pubsub#access_model",
                access_model.as_str(),
            ));
        }
        if let Some(send_last) = self.send_last_published_item {
            form = form.append(text_single_field(
                "pubsub#send_last_published_item",
                send_last.as_str(),
            ));
        }
        if let Some(max_items) = self.max_items {
            form = form.append(text_single_field(
                "pubsub#max_items",
                max_items.to_string().as_str(),
            ));
        }
        Element::builder("publish-options", NS_PUBSUB)
            .append(form.build())
            .build()
    }
}

fn hidden_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

fn text_single_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

fn wrap_pubsub_iq(
    iq_id: &str,
    iq_type: &str,
    service: Option<&BareJid>,
    pubsub: Element,
) -> Element {
    let mut builder = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), iq_type)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id);
    if let Some(service) = service {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            service.to_string(),
        );
    }
    builder.append(pubsub).build()
}

/// XEP-0060 §6.5.2 items GET for `node`, optionally bounded by §6.5.7
/// `max_items`. `service: None` routes to the account's own PEP
/// service (XEP-0163); `Some(service)` addresses a pubsub component
/// such as `community.<domain>`.
pub fn build_pubsub_items_iq(
    iq_id: &str,
    service: Option<&BareJid>,
    node: &str,
    max_items: Option<u32>,
) -> Element {
    let mut items = Element::builder("items", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    let max_items_value;
    if let Some(max) = max_items {
        max_items_value = max.to_string();
        items = items.attr(
            minidom::rxml::xml_ncname!("max_items").to_owned(),
            max_items_value.as_str(),
        );
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(items.build())
        .build();
    wrap_pubsub_iq(iq_id, "get", service, pubsub)
}

/// XEP-0060 §7.1 publish SET for one `<item/>` on `node`.
/// `item_id: None` lets the service generate the ItemID (§7.1.1);
/// `publish_options` appends a XEP-0223/§7.1.5 precondition form after
/// the `<publish/>` element.
pub fn build_pubsub_publish_iq(
    iq_id: &str,
    service: Option<&BareJid>,
    node: &str,
    item_id: Option<&str>,
    payload: Element,
    publish_options: Option<&PubsubPublishOptions>,
) -> Element {
    let mut item = Element::builder("item", NS_PUBSUB);
    if let Some(item_id) = item_id {
        item = item.attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id);
    }
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .append(item.append(payload).build())
        .build();
    let mut pubsub = Element::builder("pubsub", NS_PUBSUB).append(publish);
    if let Some(options) = publish_options {
        pubsub = pubsub.append(options.to_element());
    }
    wrap_pubsub_iq(iq_id, "set", service, pubsub.build())
}

/// XEP-0060 §6.1 subscribe SET: subscribe `subscriber` (the account's
/// bare JID per §6.1.1) to `node` on `service`.
pub fn build_pubsub_subscribe_iq(
    iq_id: &str,
    service: &BareJid,
    node: &str,
    subscriber: &BareJid,
) -> Element {
    let subscribe = Element::builder("subscribe", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            subscriber.to_string(),
        )
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(subscribe)
        .build();
    wrap_pubsub_iq(iq_id, "set", Some(service), pubsub)
}

/// XEP-0060 §6.2 unsubscribe SET: remove `subscriber`'s subscription
/// to `node` on `service`, scoped to `subid` when the service handed
/// one out at subscribe time (§6.2.3.1 requires it when a JID holds
/// multiple subscriptions to the node).
pub fn build_pubsub_unsubscribe_iq(
    iq_id: &str,
    service: &BareJid,
    node: &str,
    subscriber: &BareJid,
    subid: Option<&str>,
) -> Element {
    let mut unsubscribe = Element::builder("unsubscribe", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            subscriber.to_string(),
        );
    if let Some(subid) = subid {
        unsubscribe = unsubscribe.attr(minidom::rxml::xml_ncname!("subid").to_owned(), subid);
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(unsubscribe.build())
        .build();
    wrap_pubsub_iq(iq_id, "set", Some(service), pubsub)
}

/// XEP-0060 §6.1.6 subscription states reported on a subscribe result
/// (or §8.8.4 notification).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubsubSubscriptionState {
    Subscribed,
    Pending,
    Unconfigured,
    None,
}

impl PubsubSubscriptionState {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "subscribed" => Some(Self::Subscribed),
            "pending" => Some(Self::Pending),
            "unconfigured" => Some(Self::Unconfigured),
            "none" => Some(Self::None),
            _ => Option::None,
        }
    }
}

/// Parsed XEP-0060 §6.1.6 `<subscription/>` from a subscribe result.
/// `subid` MUST be threaded back into unsubscribe requests when
/// present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubSubscription {
    pub node: String,
    pub jid: Option<Jid>,
    pub subid: Option<String>,
    pub state: PubsubSubscriptionState,
}

/// Parse a XEP-0060 §6.1.6 subscribe result. Returns `None` when the
/// IQ carries no `<subscription/>` or its `subscription` state is
/// missing/unknown — the caller surfaces both as a typed protocol
/// error.
pub fn parse_pubsub_subscribe_result(iq: &Element) -> Option<PubsubSubscription> {
    let subscription = iq
        .get_child("pubsub", NS_PUBSUB)?
        .get_child("subscription", NS_PUBSUB)?;
    let state = PubsubSubscriptionState::parse(subscription.attr("subscription")?)?;
    Some(PubsubSubscription {
        node: subscription.attr("node")?.to_string(),
        jid: subscription
            .attr("jid")
            .and_then(|jid| Jid::from_str(jid).ok()),
        subid: subscription.attr("subid").map(str::to_string),
        state,
    })
}

/// XEP-0060 §7.2 retract SET removing `item_id` from `node`, always
/// with `notify='true'` so subscribers receive the §7.2.2.1 retract
/// notification (the community surfaces rely on it for live removal).
pub fn build_pubsub_retract_iq(
    iq_id: &str,
    service: Option<&BareJid>,
    node: &str,
    item_id: &str,
) -> Element {
    let retract = Element::builder("retract", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(minidom::rxml::xml_ncname!("notify").to_owned(), "true")
        .append(
            Element::builder("item", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                .build(),
        )
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(retract)
        .build();
    wrap_pubsub_iq(iq_id, "set", service, pubsub)
}

/// One `<item/>` from a XEP-0060 §6.5 items result: its ItemID plus
/// every payload child, preserved as typed elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubResultItem {
    pub id: String,
    pub payloads: Vec<Element>,
}

impl PubsubResultItem {
    /// First payload child matching `name`/`ns` — the idiom every
    /// surface uses to pick its typed payload (`<entry/>`,
    /// `<vcalendar/>`, `<reads/>`) out of the item.
    pub fn payload(&self, name: &str, ns: &str) -> Option<&Element> {
        self.payloads.iter().find(|payload| payload.is(name, ns))
    }
}

/// Parse a XEP-0060 §6.5 items result IQ into typed items. Tolerant by
/// design: a missing `<pubsub/>`/`<items/>` wrapper yields an empty
/// list, and items without an ItemID are skipped (every Waddle node
/// persists items under explicit ids).
pub fn parse_pubsub_items_result(iq: &Element) -> Vec<PubsubResultItem> {
    let Some(items) = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter(|child| child.is("item", NS_PUBSUB))
        .filter_map(|item| {
            let id = item.attr("id")?.to_owned();
            Some(PubsubResultItem {
                id,
                payloads: item.children().cloned().collect(),
            })
        })
        .collect()
}

/// Parse the ItemID out of a XEP-0060 §7.1.2 publish success result
/// (`<pubsub><publish node='…'><item id='…'/></publish></pubsub>`).
/// Returns `None` when the service omitted the echo (legal when the
/// publisher supplied the ItemID itself).
pub fn parse_pubsub_publish_result_item_id(iq: &Element) -> Option<String> {
    iq.get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .and_then(|item| item.attr("id"))
        .map(ToOwned::to_owned)
}
