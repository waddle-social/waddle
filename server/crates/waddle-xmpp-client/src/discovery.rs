//! Service discovery (XEP-0030), HTTP upload (XEP-0363), inbox (XEP-0430),
//! push notifications, and XEP-0503 Spaces topology discovery.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp_core::roster::{RosterItem, ROSTER_NS};

use crate::messaging::MucAffiliation;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::client::ClientHandle;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::event::ClientEvent;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::time::Duration;

// ── Namespace constants ───────────────────────────────────────────────────────

pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";
pub const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";
pub const UPLOAD_NS: &str = "urn:xmpp:http:upload:0";
pub const INBOX_NS: &str = "erlang-solutions.com:xmpp:inbox:0";
pub const WADDLE_INBOX_NS: &str = "urn:waddle:inbox:0";
pub const PUSH_NS: &str = "urn:xmpp:push:0";
pub const CLIENT_NS: &str = "jabber:client";
pub const DATA_FORMS_NS: &str = "jabber:x:data";
pub const RSM_NS: &str = "http://jabber.org/protocol/rsm";
pub const PUBSUB_NS: &str = "http://jabber.org/protocol/pubsub";
pub const PUBSUB_METADATA_FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#meta-data";
pub const BOOKMARKS_NS: &str = "urn:xmpp:bookmarks:1";
pub const SPACES_NS: &str = "urn:xmpp:spaces:0";
pub const WADDLE_ROOM_METADATA_FORM_TYPE: &str = "urn:waddle:room:0";
pub const USER_SEARCH_NS: &str = "jabber:iq:search";
pub const MUC_ADMIN_NS: &str = "http://jabber.org/protocol/muc#admin";

// ── ID generation ────────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoFeature(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoIdentity {
    pub category: String,
    pub identity_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoInfoResult {
    pub jid: String,
    pub node: Option<String>,
    pub identities: Vec<DiscoIdentity>,
    pub features: Vec<String>,
    pub forms: Vec<DiscoDataForm>,
}

impl DiscoInfoResult {
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    pub fn has_form_value(&self, form_type: &str, field_var: &str, value: &str) -> bool {
        self.forms
            .iter()
            .filter(|form| form.form_type.as_deref() == Some(form_type))
            .flat_map(|form| &form.fields)
            .any(|field| {
                field.var == field_var
                    && field.values.iter().any(|field_value| field_value == value)
            })
    }

    pub fn form_value(&self, form_type: &str, field_var: &str) -> Option<&str> {
        self.forms
            .iter()
            .filter(|form| form.form_type.as_deref() == Some(form_type))
            .flat_map(|form| &form.fields)
            .find(|field| field.var == field_var)
            .and_then(|field| field.values.first())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoDataForm {
    pub form_type: Option<String>,
    pub fields: Vec<DiscoDataField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoDataField {
    pub var: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoItem {
    pub jid: String,
    pub name: Option<String>,
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadSlot {
    pub put_url: String,
    pub get_url: String,
    pub put_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxEntry {
    pub jid: String,
    pub unread_count: u32,
    pub last_message_body: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpaceNode(String);

impl SpaceNode {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredSpace {
    pub id: SpaceNode,
    pub service_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveredChannelType {
    Text,
    Announcement,
    Forum,
}

impl DiscoveredChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Announcement => "announcement",
            Self::Forum => "forum",
        }
    }

    fn from_metadata(value: &str) -> Option<Self> {
        match value.trim() {
            "text" => Some(Self::Text),
            "announcement" => Some(Self::Announcement),
            "forum" => Some(Self::Forum),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredChannel {
    pub id: String,
    pub room_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
    pub channel_type: DiscoveredChannelType,
    pub position: i32,
    pub space_id: SpaceNode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredTopology {
    pub spaces: Vec<DiscoveredSpace>,
    pub channels: Vec<DiscoveredChannel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleInboxQuery {
    pub since: Option<u64>,
    pub only_unread: bool,
    pub room: Option<String>,
    pub threads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleInboxMarkRead {
    pub partner: String,
    pub thread: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleInboxConversation {
    pub partner: String,
    pub kind: String,
    pub last_stanza_id: Option<String>,
    pub last_updated: Option<u64>,
    pub unread: u32,
    pub preview: Option<String>,
    pub thread: Option<String>,
    pub thread_title: Option<String>,
    pub reply_count: Option<u32>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleInboxResult {
    pub total_unread: Option<u32>,
    pub conversations: Vec<WaddleInboxConversation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterResult {
    pub ver: Option<String>,
    pub items: Vec<RosterItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserSearchQuery {
    pub nick: Option<String>,
    pub email: Option<String>,
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSearchForm {
    pub instructions: Option<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSearchItem {
    pub jid: String,
    pub nick: Option<String>,
    pub email: Option<String>,
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSearchResult {
    pub items: Vec<UserSearchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MucAdminAffiliationItem {
    pub jid: Option<String>,
    pub nick: Option<String>,
    pub affiliation: Option<MucAffiliation>,
    pub reason: Option<String>,
}

// ── Parse helpers ─────────────────────────────────────────────────────────────

/// Parse a disco#info result IQ into a [`DiscoInfoResult`].
pub fn parse_disco_info_result(iq: &Element, queried_jid: &str) -> Option<DiscoInfoResult> {
    let query = iq.get_child("query", DISCO_INFO_NS)?;
    let node = query.attr("node").map(str::to_string);

    let identities = query
        .children()
        .filter(|c| c.name() == "identity" && c.ns() == DISCO_INFO_NS)
        .map(|c| DiscoIdentity {
            category: c.attr("category").unwrap_or("").to_string(),
            identity_type: c.attr("type").unwrap_or("").to_string(),
            name: c.attr("name").map(str::to_string),
        })
        .collect();

    let features = query
        .children()
        .filter(|c| c.name() == "feature" && c.ns() == DISCO_INFO_NS)
        .filter_map(|c| c.attr("var").map(str::to_string))
        .collect();
    let forms = query
        .children()
        .filter(|c| c.name() == "x" && c.ns() == DATA_FORMS_NS)
        .filter_map(parse_disco_data_form)
        .collect();

    Some(DiscoInfoResult {
        jid: queried_jid.to_string(),
        node,
        identities,
        features,
        forms,
    })
}

fn parse_disco_data_form(form: &Element) -> Option<DiscoDataForm> {
    let fields: Vec<DiscoDataField> = form
        .children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .filter_map(|field| {
            let var = field.attr("var")?.to_string();
            let values = field
                .children()
                .filter(|child| child.name() == "value" && child.ns() == DATA_FORMS_NS)
                .map(Element::text)
                .collect();
            Some(DiscoDataField { var, values })
        })
        .collect();
    if fields.is_empty() {
        return None;
    }
    let form_type = fields
        .iter()
        .find(|field| field.var == "FORM_TYPE")
        .and_then(|field| field.values.first())
        .cloned();
    Some(DiscoDataForm { form_type, fields })
}

/// Parse a disco#items result IQ into a list of [`DiscoItem`]s.
pub fn parse_disco_items_result(iq: &Element) -> Option<Vec<DiscoItem>> {
    let query = iq.get_child("query", DISCO_ITEMS_NS)?;

    let items = query
        .children()
        .filter(|c| c.name() == "item" && c.ns() == DISCO_ITEMS_NS)
        .filter_map(|c| {
            let jid = c.attr("jid")?.to_string();
            Some(DiscoItem {
                jid,
                name: c.attr("name").map(str::to_string),
                node: c.attr("node").map(str::to_string),
            })
        })
        .collect();

    Some(items)
}

pub fn parse_spaces_from_disco_items(
    spaces_jid: &BareJid,
    items: Vec<DiscoItem>,
) -> Vec<DiscoveredSpace> {
    items
        .into_iter()
        .filter(|item| item.jid == spaces_jid.to_string())
        .filter_map(|item| {
            let id = SpaceNode::new(item.node?)?;
            Some(DiscoveredSpace {
                name: item.name.unwrap_or_else(|| id.as_str().to_string()),
                id,
                service_jid: spaces_jid.clone(),
                description: None,
            })
        })
        .collect()
}

fn space_from_disco_item(
    spaces_jid: &BareJid,
    item: DiscoItem,
    info: &DiscoInfoResult,
) -> Option<DiscoveredSpace> {
    if item.jid != spaces_jid.to_string()
        || !info.has_form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#type", SPACES_NS)
    {
        return None;
    }
    let id = SpaceNode::new(item.node?)?;
    let name = info
        .form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#title")
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .or(item.name)
        .unwrap_or_else(|| id.as_str().to_string());
    let description = info
        .form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#description")
        .filter(|description| !description.trim().is_empty())
        .map(str::to_string);
    Some(DiscoveredSpace {
        id,
        service_jid: spaces_jid.clone(),
        name,
        description,
    })
}

pub fn parse_space_channels_result(
    iq: &Element,
    space_id: &SpaceNode,
) -> Option<Vec<DiscoveredChannel>> {
    let pubsub = iq.get_child("pubsub", PUBSUB_NS)?;
    let items = pubsub.get_child("items", PUBSUB_NS)?;
    if items.attr("node") != Some(space_id.as_str()) {
        return None;
    }

    let channels = items
        .children()
        .filter(|child| child.name() == "item" && child.ns() == PUBSUB_NS)
        .filter_map(|item| {
            let room_jid: BareJid = item.attr("id")?.parse().ok()?;
            let conference = item.get_child("conference", BOOKMARKS_NS)?;
            let id = format!("{}::{}", space_id.as_str(), room_jid);
            let name = conference
                .attr("name")
                .filter(|name| !name.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    room_jid
                        .node()
                        .map(|node| node.as_str().to_string())
                        .unwrap_or_else(|| id.clone())
                });
            Some((id, room_jid, name))
        })
        .enumerate()
        .map(|(position, (id, room_jid, name))| DiscoveredChannel {
            id,
            room_jid,
            name,
            description: None,
            channel_type: DiscoveredChannelType::Text,
            position: position as i32,
            space_id: space_id.clone(),
        })
        .collect();

    Some(channels)
}

/// Parse an HTTP upload slot result IQ into an [`UploadSlot`].
pub fn parse_upload_slot(iq: &Element) -> Option<UploadSlot> {
    let slot = iq.get_child("slot", UPLOAD_NS)?;
    let put_el = slot.get_child("put", UPLOAD_NS)?;
    let get_el = slot.get_child("get", UPLOAD_NS)?;

    let put_url = put_el.attr("url")?.to_string();
    let get_url = get_el.attr("url")?.to_string();

    let put_headers = put_el
        .children()
        .filter(|c| c.name() == "header" && c.ns() == UPLOAD_NS)
        .filter_map(|c| {
            let name = c.attr("name")?.to_string();
            let value = c.text();
            Some((name, value))
        })
        .collect();

    Some(UploadSlot {
        put_url,
        get_url,
        put_headers,
    })
}

/// Parse an inbox result element (XEP-0430 / erlang-solutions inbox).
///
/// Accepts either a bare `<result xmlns='...inbox:0'>` element or a wrapping
/// stanza (e.g. `<message>`) that contains one as a child.  Returns `None` for
/// plain stanzas that carry no inbox result.
pub fn parse_inbox_result(element: &Element) -> Option<InboxEntry> {
    let (result_el, jid) = if element.name() == "result" && element.ns() == INBOX_NS {
        let jid = element
            .attr("from")
            .or_else(|| element.attr("to"))
            .map(str::to_string)
            .unwrap_or_default();
        (element, jid)
    } else {
        let result_el = element.get_child("result", INBOX_NS)?;
        let jid = element
            .attr("from")
            .or_else(|| element.attr("to"))
            .map(str::to_string)
            .unwrap_or_default();
        (result_el, jid)
    };

    let unread_count = result_el
        .get_child("unread", INBOX_NS)
        .and_then(|e| e.text().parse::<u32>().ok())
        .unwrap_or(0);

    let last_message_body = extract_forwarded_body(result_el);

    Some(InboxEntry {
        jid,
        unread_count,
        last_message_body,
        timestamp: None,
    })
}

fn extract_forwarded_body(result_el: &Element) -> Option<String> {
    const FORWARD_NS: &str = "urn:xmpp:forward:0";
    let forwarded = result_el.get_child("forwarded", FORWARD_NS)?;
    let message = forwarded.get_child("message", CLIENT_NS)?;
    let body = message.get_child("body", CLIENT_NS)?;
    Some(body.text())
}

// ── IQ builders ──────────────────────────────────────────────────────────────

pub fn build_disco_info_iq(to: &str, node: Option<&str>) -> Element {
    let id = format!("disco-info-{}", next_id());
    let mut query_builder = Element::builder("query", DISCO_INFO_NS);
    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to)
        .attr("id", id)
        .append(query_builder.build())
        .build()
}

pub fn build_disco_items_iq(to: &str, node: Option<&str>) -> Element {
    let id = format!("disco-items-{}", next_id());
    let mut query_builder = Element::builder("query", DISCO_ITEMS_NS);
    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to)
        .attr("id", id)
        .append(query_builder.build())
        .build()
}

fn build_pubsub_items_iq(to: &BareJid, node: &SpaceNode) -> Element {
    let id = format!("pubsub-items-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to.to_string())
        .attr("id", id)
        .append(
            Element::builder("pubsub", PUBSUB_NS)
                .append(
                    Element::builder("items", PUBSUB_NS)
                        .attr("node", node.as_str())
                        .build(),
                )
                .build(),
        )
        .build()
}

pub fn build_upload_slot_iq(
    service_jid: &str,
    filename: &str,
    size: u64,
    content_type: &str,
) -> Element {
    let id = format!("upload-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", service_jid)
        .attr("id", id)
        .append(
            Element::builder("request", UPLOAD_NS)
                .attr("filename", filename)
                .attr("size", size.to_string())
                .attr("content-type", content_type)
                .build(),
        )
        .build()
}

pub fn build_inbox_iq(bare_jid: &str, query_id: &str, max: u32) -> Element {
    let id = format!("inbox-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("to", bare_jid)
        .attr("id", id)
        .append(
            Element::builder("inbox", INBOX_NS)
                .attr("queryid", query_id)
                .append(
                    Element::builder("max", RSM_NS)
                        .append(max.to_string())
                        .build(),
                )
                .build(),
        )
        .build()
}

pub fn build_enable_push_iq(push_service_jid: &str, node: &str, token: &str) -> Element {
    let id = format!("push-enable-{}", next_id());
    let form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "FORM_TYPE")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append("http://jabber.org/protocol/pubsub#publish-options")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "secret")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(token)
                        .build(),
                )
                .build(),
        )
        .build();
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", id)
        .append(
            Element::builder("enable", PUSH_NS)
                .attr("jid", push_service_jid)
                .attr("node", node)
                .append(form)
                .build(),
        )
        .build()
}

pub fn build_disable_push_iq(push_service_jid: &str, node: &str) -> Element {
    let id = format!("push-disable-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", id)
        .append(
            Element::builder("disable", PUSH_NS)
                .attr("jid", push_service_jid)
                .attr("node", node)
                .build(),
        )
        .build()
}

pub fn build_waddle_inbox_query_iq(to: &str, query: &WaddleInboxQuery) -> Element {
    let id = format!("waddle-inbox-{}", next_id());
    let mut builder = Element::builder("query", WADDLE_INBOX_NS);
    if let Some(since) = query.since {
        builder = builder.attr("since", since.to_string());
    }
    if query.only_unread {
        builder = builder.attr("only-unread", "true");
    }
    if let Some(room) = query.room.as_deref() {
        builder = builder.attr("room", room);
    }
    if query.threads {
        builder = builder.attr("threads", "true");
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to)
        .attr("id", id)
        .append(builder.build())
        .build()
}

pub fn build_waddle_inbox_mark_read_iq(to: &str, mark_read: &WaddleInboxMarkRead) -> Element {
    let id = format!("waddle-mark-read-{}", next_id());
    let mut builder =
        Element::builder("mark-read", WADDLE_INBOX_NS).attr("partner", mark_read.partner.as_str());
    if let Some(thread) = mark_read.thread.as_deref() {
        builder = builder.attr("thread", thread);
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("to", to)
        .attr("id", id)
        .append(builder.build())
        .build()
}

pub fn parse_waddle_inbox_result(iq: &Element) -> Option<WaddleInboxResult> {
    let query = iq.get_child("query", WADDLE_INBOX_NS)?;
    let total_unread = query
        .attr("total-unread")
        .and_then(|value| value.parse().ok());
    let conversations = query
        .children()
        .filter(|child| child.name() == "conversation" && child.ns() == WADDLE_INBOX_NS)
        .filter_map(|conversation| {
            let partner = conversation.attr("partner")?.to_string();
            let kind = conversation.attr("kind").unwrap_or("direct").to_string();
            let unread = conversation
                .attr("unread")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            Some(WaddleInboxConversation {
                partner,
                kind,
                last_stanza_id: conversation.attr("last-stanza-id").map(str::to_string),
                last_updated: conversation
                    .attr("last-updated")
                    .and_then(|value| value.parse().ok()),
                unread,
                preview: conversation
                    .get_child("preview", WADDLE_INBOX_NS)
                    .map(|child| child.text()),
                thread: conversation.attr("thread").map(str::to_string),
                thread_title: conversation.attr("thread-title").map(str::to_string),
                reply_count: conversation
                    .attr("reply-count")
                    .and_then(|value| value.parse().ok()),
                author: conversation.attr("author").map(str::to_string),
            })
        })
        .collect();
    Some(WaddleInboxResult {
        total_unread,
        conversations,
    })
}

pub fn build_roster_get_iq(to: Option<&str>, ver: Option<&str>) -> Element {
    let id = format!("roster-{}", next_id());
    let mut query = Element::builder("query", ROSTER_NS);
    if let Some(ver) = ver {
        query = query.attr("ver", ver);
    }
    let mut iq = Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("id", id)
        .append(query.build());
    if let Some(to) = to {
        iq = iq.attr("to", to);
    }
    iq.build()
}

pub fn parse_roster_result(iq: &Element) -> Option<RosterResult> {
    let query = iq.get_child("query", ROSTER_NS)?;
    let items = query
        .children()
        .filter(|child| child.name() == "item" && child.ns() == ROSTER_NS)
        .map(RosterItem::from_element)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(RosterResult {
        ver: query.attr("ver").map(str::to_string),
        items,
    })
}

pub fn build_user_search_form_iq(to: &str) -> Element {
    let id = format!("user-search-form-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to)
        .attr("id", id)
        .append(Element::builder("query", USER_SEARCH_NS).build())
        .build()
}

pub fn build_user_search_iq(to: &str, query: &UserSearchQuery) -> Element {
    let id = format!("user-search-{}", next_id());
    let mut search = Element::builder("query", USER_SEARCH_NS);
    for (name, value) in [
        ("nick", query.nick.as_deref()),
        ("email", query.email.as_deref()),
        ("first", query.first.as_deref()),
        ("last", query.last.as_deref()),
    ] {
        if let Some(value) = value {
            search = search.append(Element::builder(name, USER_SEARCH_NS).append(value).build());
        }
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("to", to)
        .attr("id", id)
        .append(search.build())
        .build()
}

pub fn parse_user_search_form(iq: &Element) -> Option<UserSearchForm> {
    let query = iq.get_child("query", USER_SEARCH_NS)?;
    let instructions = query
        .get_child("instructions", USER_SEARCH_NS)
        .map(|child| child.text());
    let fields = query
        .children()
        .filter(|child| child.ns() == USER_SEARCH_NS && child.name() != "instructions")
        .map(|child| child.name().to_string())
        .collect();
    Some(UserSearchForm {
        instructions,
        fields,
    })
}

pub fn parse_user_search_result(iq: &Element) -> Option<UserSearchResult> {
    let query = iq.get_child("query", USER_SEARCH_NS)?;
    let items = query
        .children()
        .filter(|child| child.name() == "item" && child.ns() == USER_SEARCH_NS)
        .filter_map(|item| {
            let jid = item.attr("jid")?.to_string();
            Some(UserSearchItem {
                jid,
                nick: item
                    .get_child("nick", USER_SEARCH_NS)
                    .map(|child| child.text()),
                email: item
                    .get_child("email", USER_SEARCH_NS)
                    .map(|child| child.text()),
                first: item
                    .get_child("first", USER_SEARCH_NS)
                    .map(|child| child.text()),
                last: item
                    .get_child("last", USER_SEARCH_NS)
                    .map(|child| child.text()),
            })
        })
        .collect();
    Some(UserSearchResult { items })
}

pub fn build_muc_admin_affiliation_list_iq(room_jid: &str, affiliation: MucAffiliation) -> Element {
    let id = format!("muc-admin-list-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", room_jid)
        .attr("id", id)
        .append(
            Element::builder("query", MUC_ADMIN_NS)
                .append(
                    Element::builder("item", MUC_ADMIN_NS)
                        .attr("affiliation", affiliation.as_str())
                        .build(),
                )
                .build(),
        )
        .build()
}

pub fn build_muc_admin_affiliation_set_iq(
    room_jid: &str,
    items: &[MucAdminAffiliationItem],
) -> Element {
    let id = format!("muc-admin-set-{}", next_id());
    let mut query = Element::builder("query", MUC_ADMIN_NS);
    for item in items {
        let mut item_builder = Element::builder("item", MUC_ADMIN_NS);
        if let Some(jid) = item.jid.as_deref() {
            item_builder = item_builder.attr("jid", jid);
        }
        if let Some(nick) = item.nick.as_deref() {
            item_builder = item_builder.attr("nick", nick);
        }
        if let Some(affiliation) = item.affiliation {
            item_builder = item_builder.attr("affiliation", affiliation.as_str());
        }
        if let Some(reason) = item.reason.as_deref() {
            item_builder = item_builder.append(
                Element::builder("reason", MUC_ADMIN_NS)
                    .append(reason)
                    .build(),
            );
        }
        query = query.append(item_builder.build());
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("to", room_jid)
        .attr("id", id)
        .append(query.build())
        .build()
}

pub fn parse_muc_admin_affiliation_query(
    element: &Element,
) -> Option<Vec<MucAdminAffiliationItem>> {
    let query = element.get_child("query", MUC_ADMIN_NS)?;
    Some(
        query
            .children()
            .filter(|child| child.name() == "item" && child.ns() == MUC_ADMIN_NS)
            .map(|item| MucAdminAffiliationItem {
                jid: item.attr("jid").map(str::to_string),
                nick: item.attr("nick").map(str::to_string),
                affiliation: item.attr("affiliation").and_then(MucAffiliation::from_attr),
                reason: item
                    .get_child("reason", MUC_ADMIN_NS)
                    .map(|child| child.text()),
            })
            .collect(),
    )
}

// ── Private helpers ───────────────────────────────────────────────────────────

#[cfg(feature = "native")]
fn parse_error() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some("response could not be parsed".to_string()),
    })
}

// ── Extension trait ──────────────────────────────────────────────────────────

#[allow(async_fn_in_trait)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait DiscoveryExt {
    /// Query `disco#info` for a JID, optionally scoped to a node.
    async fn discover_info(&self, jid: &str, node: Option<&str>) -> ClientResult<DiscoInfoResult>;

    /// Query `disco#items` for a JID.
    async fn discover_items(&self, jid: &str, node: Option<&str>) -> ClientResult<Vec<DiscoItem>>;

    /// Discover the HTTP upload service under `server_domain`.
    ///
    /// Queries `disco#items` on the domain and then `disco#info` on each item
    /// until one advertises `urn:xmpp:http:upload:0`.  Returns its JID, or
    /// `None` if no matching component is found.
    async fn discover_upload_service(&self, server_domain: &str) -> ClientResult<Option<String>>;

    /// Request an HTTP upload slot from `service_jid` (XEP-0363).
    async fn request_upload_slot(
        &self,
        service_jid: &str,
        filename: &str,
        size: u64,
        content_type: &str,
    ) -> ClientResult<UploadSlot>;

    /// Fetch inbox entries (XEP-0430 / erlang-solutions inbox extension).
    ///
    /// Sends the inbox IQ and collects streamed `UnhandledStanza` results until
    /// the final `<fin>` IQ result arrives or the 30-second timeout fires.
    async fn fetch_inbox(&self, max: u32) -> ClientResult<Vec<InboxEntry>>;

    /// Enable push notifications via a push service (XEP-0357).
    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        token: &str,
    ) -> ClientResult<()>;

    /// Disable push notifications for a previously-registered push node.
    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
    ) -> ClientResult<()>;

    /// Discover XEP-0503 spaces from the spaces service.
    async fn discover_spaces(&self, spaces_jid: &BareJid) -> ClientResult<Vec<DiscoveredSpace>>;

    /// Discover bookmark-backed MUC channels within one XEP-0503 space.
    async fn discover_space_channels(
        &self,
        spaces_jid: &BareJid,
        space_id: &SpaceNode,
    ) -> ClientResult<Vec<DiscoveredChannel>>;

    /// Discover the native spaces + channels topology.
    async fn discover_topology(&self, spaces_jid: &BareJid) -> ClientResult<DiscoveredTopology>;
}

// ── Implementation ────────────────────────────────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl DiscoveryExt for ClientHandle {
    async fn discover_info(&self, jid: &str, node: Option<&str>) -> ClientResult<DiscoInfoResult> {
        let iq = build_disco_info_iq(jid, node);
        let result = self.send_iq(iq).await?;
        parse_disco_info_result(&result, jid).ok_or_else(parse_error)
    }

    async fn discover_items(&self, jid: &str, node: Option<&str>) -> ClientResult<Vec<DiscoItem>> {
        let iq = build_disco_items_iq(jid, node);
        let result = self.send_iq(iq).await?;
        parse_disco_items_result(&result).ok_or_else(parse_error)
    }

    async fn discover_upload_service(&self, server_domain: &str) -> ClientResult<Option<String>> {
        let items = self.discover_items(server_domain, None).await?;
        for item in items {
            if let Ok(info) = self.discover_info(&item.jid, None).await {
                if info.has_feature(UPLOAD_NS) {
                    return Ok(Some(item.jid));
                }
            }
        }
        Ok(None)
    }

    async fn request_upload_slot(
        &self,
        service_jid: &str,
        filename: &str,
        size: u64,
        content_type: &str,
    ) -> ClientResult<UploadSlot> {
        let iq = build_upload_slot_iq(service_jid, filename, size, content_type);
        let result = self.send_iq(iq).await?;
        parse_upload_slot(&result).ok_or_else(parse_error)
    }

    async fn fetch_inbox(&self, max: u32) -> ClientResult<Vec<InboxEntry>> {
        // Subscribe before sending so we don't miss early stanzas.
        let mut events = self.events();

        let query_id = format!("inbox-q-{}", next_id());
        let snapshot = self.snapshot();
        let binding = snapshot.binding.ok_or(ClientError::Disconnected)?;
        let bare_jid = {
            let full = binding.jid.to_string();
            if let Some(idx) = full.rfind('/') {
                full[..idx].to_string()
            } else {
                full
            }
        };
        let iq = build_inbox_iq(&bare_jid, &query_id, max);

        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<ClientResult<()>>();
        let handle = self.clone();
        tokio::spawn(async move {
            let res = handle.send_iq(iq).await.map(|_| ());
            let _ = done_tx.send(res);
        });

        let mut entries = Vec::new();
        let sleep = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                result = &mut done_rx => {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(_) => return Err(ClientError::Disconnected),
                    }
                    break;
                }
                event = events.recv() => {
                    if let Ok(ClientEvent::UnhandledStanza(el)) = event {
                        if let Some(entry) = parse_inbox_result(&el) {
                            entries.push(entry);
                        }
                    }
                }
                _ = &mut sleep => {
                    break;
                }
            }
        }

        Ok(entries)
    }

    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        token: &str,
    ) -> ClientResult<()> {
        let iq = build_enable_push_iq(push_service_jid, node, token);
        self.send_iq(iq).await.map(|_| ())
    }

    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
    ) -> ClientResult<()> {
        let iq = build_disable_push_iq(push_service_jid, node);
        self.send_iq(iq).await.map(|_| ())
    }

    async fn discover_spaces(&self, spaces_jid: &BareJid) -> ClientResult<Vec<DiscoveredSpace>> {
        let items = self.discover_items(&spaces_jid.to_string(), None).await?;
        let mut spaces = Vec::new();
        for item in items {
            let Some(node) = item.node.as_deref() else {
                continue;
            };
            let Ok(info) = self
                .discover_info(&spaces_jid.to_string(), Some(node))
                .await
            else {
                continue;
            };
            if let Some(space) = space_from_disco_item(spaces_jid, item, &info) {
                spaces.push(space);
            }
        }
        Ok(spaces)
    }

    async fn discover_space_channels(
        &self,
        spaces_jid: &BareJid,
        space_id: &SpaceNode,
    ) -> ClientResult<Vec<DiscoveredChannel>> {
        let iq = build_pubsub_items_iq(spaces_jid, space_id);
        let result = self.send_iq(iq).await?;
        let mut channels =
            parse_space_channels_result(&result, space_id).ok_or_else(parse_error)?;
        for channel in &mut channels {
            if let Ok(info) = self
                .discover_info(&channel.room_jid.to_string(), None)
                .await
            {
                if let Some(channel_type) = info
                    .form_value(WADDLE_ROOM_METADATA_FORM_TYPE, "waddle#channel_type")
                    .and_then(DiscoveredChannelType::from_metadata)
                {
                    channel.channel_type = channel_type;
                }
                channel.description = info
                    .form_value(
                        "http://jabber.org/protocol/muc#roominfo",
                        "muc#roominfo_description",
                    )
                    .filter(|description| !description.trim().is_empty())
                    .map(str::to_string);
            }
        }
        Ok(channels)
    }

    async fn discover_topology(&self, spaces_jid: &BareJid) -> ClientResult<DiscoveredTopology> {
        let spaces = self.discover_spaces(spaces_jid).await?;
        let mut channels = Vec::new();
        for space in &spaces {
            channels.extend(self.discover_space_channels(spaces_jid, &space.id).await?);
        }
        Ok(DiscoveredTopology { spaces, channels })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_disco_info_result_extracts_features() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .append(
                        Element::builder("feature", DISCO_INFO_NS)
                            .attr("var", UPLOAD_NS)
                            .build(),
                    )
                    .append(
                        Element::builder("feature", DISCO_INFO_NS)
                            .attr("var", "jabber:iq:version")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let result = parse_disco_info_result(&iq, "upload.example.com").unwrap();
        assert_eq!(result.jid, "upload.example.com");
        assert!(result.has_feature(UPLOAD_NS));
        assert!(result.has_feature("jabber:iq:version"));
        assert!(!result.has_feature("urn:xmpp:nonexistent"));
    }

    #[test]
    fn parse_disco_info_result_extracts_data_form_metadata() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "result")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "FORM_TYPE")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append(PUBSUB_METADATA_FORM_TYPE)
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "pubsub#type")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append(SPACES_NS)
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "pubsub#title")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("Engineering")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "result")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "FORM_TYPE")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("http://jabber.org/protocol/muc#roominfo")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "muc#roominfo_description")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("Project discussion")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();

        let result = parse_disco_info_result(&iq, "spaces.example.com").unwrap();

        assert!(result.has_form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#type", SPACES_NS));
        assert_eq!(
            result.form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#title"),
            Some("Engineering")
        );
        assert_eq!(
            result.form_value(
                "http://jabber.org/protocol/muc#roominfo",
                "muc#roominfo_description"
            ),
            Some("Project discussion")
        );
    }

    #[test]
    fn parse_disco_info_result_extracts_identities() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .append(
                        Element::builder("identity", DISCO_INFO_NS)
                            .attr("category", "store")
                            .attr("type", "file")
                            .attr("name", "HTTP File Upload")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let result = parse_disco_info_result(&iq, "upload.example.com").unwrap();
        assert_eq!(result.identities.len(), 1);
        let id = &result.identities[0];
        assert_eq!(id.category, "store");
        assert_eq!(id.identity_type, "file");
        assert_eq!(id.name.as_deref(), Some("HTTP File Upload"));
    }

    #[test]
    fn parse_disco_items_result_extracts_items() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_ITEMS_NS)
                    .append(
                        Element::builder("item", DISCO_ITEMS_NS)
                            .attr("jid", "upload.example.com")
                            .attr("name", "Upload Service")
                            .build(),
                    )
                    .append(
                        Element::builder("item", DISCO_ITEMS_NS)
                            .attr("jid", "muc.example.com")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let items = parse_disco_items_result(&iq).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].jid, "upload.example.com");
        assert_eq!(items[0].name.as_deref(), Some("Upload Service"));
        assert_eq!(items[1].jid, "muc.example.com");
        assert!(items[1].name.is_none());
    }

    #[test]
    fn root_service_items_are_not_spaces() {
        let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
        let items = vec![
            DiscoItem {
                jid: "muc.example.com".to_string(),
                name: Some("Chatrooms".to_string()),
                node: None,
            },
            DiscoItem {
                jid: "spaces.example.com".to_string(),
                name: Some("Spaces".to_string()),
                node: None,
            },
            DiscoItem {
                jid: "extensions.example.com".to_string(),
                name: Some("Extensions".to_string()),
                node: None,
            },
        ];

        assert!(parse_spaces_from_disco_items(&spaces_jid, items).is_empty());
    }

    #[test]
    fn spaces_service_items_parse_node_backed_spaces() {
        let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
        let items = vec![DiscoItem {
            jid: "spaces.example.com".to_string(),
            name: Some("General".to_string()),
            node: Some("general".to_string()),
        }];

        let spaces = parse_spaces_from_disco_items(&spaces_jid, items);

        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].id.as_str(), "general");
        assert_eq!(spaces[0].service_jid, spaces_jid);
        assert_eq!(spaces[0].name, "General");
    }

    #[test]
    fn space_from_disco_item_requires_spaces_metadata_type() {
        let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
        let item = DiscoItem {
            jid: "spaces.example.com".to_string(),
            name: Some("Ignored Node Name".to_string()),
            node: Some("engineering".to_string()),
        };
        let space_info = DiscoInfoResult {
            jid: "spaces.example.com".to_string(),
            node: Some("engineering".to_string()),
            identities: vec![],
            features: vec![],
            forms: vec![DiscoDataForm {
                form_type: Some(PUBSUB_METADATA_FORM_TYPE.to_string()),
                fields: vec![
                    DiscoDataField {
                        var: "FORM_TYPE".to_string(),
                        values: vec![PUBSUB_METADATA_FORM_TYPE.to_string()],
                    },
                    DiscoDataField {
                        var: "pubsub#type".to_string(),
                        values: vec![SPACES_NS.to_string()],
                    },
                    DiscoDataField {
                        var: "pubsub#title".to_string(),
                        values: vec!["Engineering".to_string()],
                    },
                    DiscoDataField {
                        var: "pubsub#description".to_string(),
                        values: vec!["Build systems".to_string()],
                    },
                ],
            }],
        };
        let other_info = DiscoInfoResult {
            forms: vec![],
            ..space_info.clone()
        };

        let space = space_from_disco_item(&spaces_jid, item.clone(), &space_info).unwrap();

        assert_eq!(space.id.as_str(), "engineering");
        assert_eq!(space.name, "Engineering");
        assert_eq!(space.description.as_deref(), Some("Build systems"));
        assert!(space_from_disco_item(&spaces_jid, item, &other_info).is_none());
    }

    #[test]
    fn pubsub_items_parse_bookmark_channels_and_ignore_non_conference_payloads() {
        let space_id = SpaceNode::new("general").expect("space node");
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("pubsub", PUBSUB_NS)
                    .append(
                        Element::builder("items", PUBSUB_NS)
                            .attr("node", "general")
                            .append(
                                Element::builder("item", PUBSUB_NS)
                                    .attr("id", "urn:xmpp:spaces:avatar:metadata:0")
                                    .append(
                                        Element::builder("metadata", "urn:xmpp:avatar:metadata")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("item", PUBSUB_NS)
                                    .attr("id", "chat@muc.example.com")
                                    .append(
                                        Element::builder("conference", BOOKMARKS_NS)
                                            .attr("name", "Chat")
                                            .attr("autojoin", "true")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("item", PUBSUB_NS)
                                    .attr("id", "not-a-room")
                                    .append(
                                        Element::builder("note", "urn:example:note")
                                            .append("ignore me")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();

        let channels = parse_space_channels_result(&iq, &space_id).expect("channels");

        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, "general::chat@muc.example.com");
        assert_eq!(
            channels[0].room_jid,
            "chat@muc.example.com".parse::<BareJid>().expect("room jid")
        );
        assert_eq!(channels[0].name, "Chat");
        assert_eq!(channels[0].channel_type, DiscoveredChannelType::Text);
        assert_eq!(channels[0].position, 0);
        assert_eq!(channels[0].space_id.as_str(), "general");
    }

    #[test]
    fn discovered_channel_type_parses_waddle_metadata_values() {
        assert_eq!(
            DiscoveredChannelType::from_metadata("text"),
            Some(DiscoveredChannelType::Text)
        );
        assert_eq!(
            DiscoveredChannelType::from_metadata("announcement"),
            Some(DiscoveredChannelType::Announcement)
        );
        assert_eq!(
            DiscoveredChannelType::from_metadata("forum"),
            Some(DiscoveredChannelType::Forum)
        );
        assert_eq!(DiscoveredChannelType::from_metadata("unknown"), None);
    }

    #[test]
    fn parse_upload_slot_extracts_urls_and_headers() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("slot", UPLOAD_NS)
                    .append(
                        Element::builder("put", UPLOAD_NS)
                            .attr("url", "https://example.com/upload/file.jpg")
                            .append(
                                Element::builder("header", UPLOAD_NS)
                                    .attr("name", "Authorization")
                                    .append("Bearer token123")
                                    .build(),
                            )
                            .append(
                                Element::builder("header", UPLOAD_NS)
                                    .attr("name", "Cookie")
                                    .append("session=abc")
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("get", UPLOAD_NS)
                            .attr("url", "https://cdn.example.com/file.jpg")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let slot = parse_upload_slot(&iq).unwrap();
        assert_eq!(slot.put_url, "https://example.com/upload/file.jpg");
        assert_eq!(slot.get_url, "https://cdn.example.com/file.jpg");
        assert_eq!(slot.put_headers.len(), 2);
        assert_eq!(
            slot.put_headers[0],
            ("Authorization".to_string(), "Bearer token123".to_string())
        );
        assert_eq!(
            slot.put_headers[1],
            ("Cookie".to_string(), "session=abc".to_string())
        );
    }

    #[test]
    fn parse_upload_slot_no_headers_ok() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("slot", UPLOAD_NS)
                    .append(
                        Element::builder("put", UPLOAD_NS)
                            .attr("url", "https://example.com/upload/file.jpg")
                            .build(),
                    )
                    .append(
                        Element::builder("get", UPLOAD_NS)
                            .attr("url", "https://cdn.example.com/file.jpg")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let slot = parse_upload_slot(&iq).unwrap();
        assert_eq!(slot.put_url, "https://example.com/upload/file.jpg");
        assert_eq!(slot.get_url, "https://cdn.example.com/file.jpg");
        assert!(slot.put_headers.is_empty());
    }

    #[test]
    fn disco_info_result_has_feature_check() {
        let result = DiscoInfoResult {
            jid: "example.com".to_string(),
            node: None,
            identities: vec![],
            features: vec![UPLOAD_NS.to_string(), "jabber:iq:ping".to_string()],
            forms: vec![],
        };
        assert!(result.has_feature(UPLOAD_NS));
        assert!(result.has_feature("jabber:iq:ping"));
        assert!(!result.has_feature("urn:xmpp:nonexistent"));
    }

    #[test]
    fn build_and_parse_waddle_inbox_round_trip() {
        let iq = build_waddle_inbox_query_iq(
            "me@example.com",
            &WaddleInboxQuery {
                since: Some(1700000),
                only_unread: true,
                room: Some("room@muc.example.com".to_string()),
                threads: true,
            },
        );
        let query = iq.get_child("query", WADDLE_INBOX_NS).expect("inbox query");
        assert_eq!(query.attr("since"), Some("1700000"));
        assert_eq!(query.attr("only-unread"), Some("true"));
        assert_eq!(query.attr("room"), Some("room@muc.example.com"));
        assert_eq!(query.attr("threads"), Some("true"));

        let result = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", WADDLE_INBOX_NS)
                    .attr("total-unread", "3")
                    .append(
                        Element::builder("conversation", WADDLE_INBOX_NS)
                            .attr("partner", "alice@example.com")
                            .attr("kind", "direct")
                            .attr("last-stanza-id", "sid-1")
                            .attr("last-updated", "1700001")
                            .attr("unread", "2")
                            .append(
                                Element::builder("preview", WADDLE_INBOX_NS)
                                    .append("hi there")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();
        let parsed = parse_waddle_inbox_result(&result).expect("parse inbox result");
        assert_eq!(parsed.total_unread, Some(3));
        assert_eq!(parsed.conversations.len(), 1);
        assert_eq!(parsed.conversations[0].partner, "alice@example.com");
        assert_eq!(parsed.conversations[0].preview.as_deref(), Some("hi there"));
    }

    #[test]
    fn build_waddle_inbox_mark_read_supports_thread() {
        let iq = build_waddle_inbox_mark_read_iq(
            "me@example.com",
            &WaddleInboxMarkRead {
                partner: "room@muc.example.com".to_string(),
                thread: Some("thread-42".to_string()),
            },
        );
        let mark_read = iq
            .get_child("mark-read", WADDLE_INBOX_NS)
            .expect("mark-read");
        assert_eq!(mark_read.attr("partner"), Some("room@muc.example.com"));
        assert_eq!(mark_read.attr("thread"), Some("thread-42"));
    }

    #[test]
    fn build_and_parse_roster_result() {
        let get = build_roster_get_iq(None, Some("ver-1"));
        let query = get.get_child("query", ROSTER_NS).expect("roster query");
        assert_eq!(query.attr("ver"), Some("ver-1"));

        let result = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", ROSTER_NS)
                    .attr("ver", "ver-2")
                    .append(
                        Element::builder("item", ROSTER_NS)
                            .attr("jid", "alice@example.com")
                            .attr("name", "Alice")
                            .attr("subscription", "both")
                            .append(
                                Element::builder("group", ROSTER_NS)
                                    .append("Friends")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();
        let parsed = parse_roster_result(&result).expect("parse roster");
        assert_eq!(parsed.ver.as_deref(), Some("ver-2"));
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].jid.to_string(), "alice@example.com");
        assert_eq!(parsed.items[0].name.as_deref(), Some("Alice"));
    }

    #[test]
    fn build_and_parse_user_search_queries() {
        let form_iq = build_user_search_form_iq("localhost");
        assert!(form_iq.get_child("query", USER_SEARCH_NS).is_some());

        let search_iq = build_user_search_iq(
            "localhost",
            &UserSearchQuery {
                nick: Some("admin".to_string()),
                email: None,
                first: None,
                last: None,
            },
        );
        assert_eq!(
            search_iq
                .get_child("query", USER_SEARCH_NS)
                .and_then(|query| query.get_child("nick", USER_SEARCH_NS))
                .map(|child| child.text()),
            Some("admin".to_string())
        );

        let form_result = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", USER_SEARCH_NS)
                    .append(
                        Element::builder("instructions", USER_SEARCH_NS)
                            .append("Search users")
                            .build(),
                    )
                    .append(Element::builder("nick", USER_SEARCH_NS).build())
                    .append(Element::builder("email", USER_SEARCH_NS).build())
                    .build(),
            )
            .build();
        let parsed_form = parse_user_search_form(&form_result).expect("parse form");
        assert_eq!(parsed_form.instructions.as_deref(), Some("Search users"));
        assert_eq!(parsed_form.fields, vec!["nick", "email"]);

        let result = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", USER_SEARCH_NS)
                    .append(
                        Element::builder("item", USER_SEARCH_NS)
                            .attr("jid", "admin@localhost")
                            .append(
                                Element::builder("nick", USER_SEARCH_NS)
                                    .append("admin")
                                    .build(),
                            )
                            .append(
                                Element::builder("email", USER_SEARCH_NS)
                                    .append("admin@localhost")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();
        let parsed = parse_user_search_result(&result).expect("parse search result");
        assert_eq!(parsed.items[0].jid, "admin@localhost");
        assert_eq!(parsed.items[0].nick.as_deref(), Some("admin"));
    }

    #[test]
    fn build_and_parse_muc_admin_affiliation_queries() {
        let list =
            build_muc_admin_affiliation_list_iq("room@muc.example.com", MucAffiliation::Member);
        assert_eq!(list.attr("type"), Some("get"));
        assert_eq!(
            list.get_child("query", MUC_ADMIN_NS)
                .and_then(|query| query.get_child("item", MUC_ADMIN_NS))
                .and_then(|item| item.attr("affiliation")),
            Some("member")
        );

        let set = build_muc_admin_affiliation_set_iq(
            "room@muc.example.com",
            &[MucAdminAffiliationItem {
                jid: Some("alice@example.com".to_string()),
                nick: None,
                affiliation: Some(MucAffiliation::Admin),
                reason: Some("promoted".to_string()),
            }],
        );
        let parsed = parse_muc_admin_affiliation_query(&set).expect("parse admin set");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].jid.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed[0].affiliation, Some(MucAffiliation::Admin));
        assert_eq!(parsed[0].reason.as_deref(), Some("promoted"));
    }

    #[test]
    fn parse_inbox_result_returns_none_for_plain_message() {
        let message = Element::builder("message", CLIENT_NS)
            .attr("from", "alice@example.com")
            .attr("to", "me@example.com")
            .append(Element::builder("body", CLIENT_NS).append("Hello!").build())
            .build();

        assert!(parse_inbox_result(&message).is_none());
    }
}
