use jid::BareJid;
use minidom::Element;
use waddle_xmpp_core::roster::RosterItem;

use crate::messaging::MucAffiliation;

use super::ids::next_id;
use super::types::{
    MucAdminAffiliationItem, RosterResult, SpaceNode, UserSearchForm, UserSearchItem,
    UserSearchQuery, UserSearchResult, WaddleInboxConversation, WaddleInboxMarkRead,
    WaddleInboxQuery, WaddleInboxResult,
};
use super::{
    CLIENT_NS, DATA_FORMS_NS, DISCO_INFO_NS, DISCO_ITEMS_NS, MUC_ADMIN_NS, PUBSUB_NS, PUSH_NS,
    ROSTER_NS, UPLOAD_NS, USER_SEARCH_NS, WADDLE_INBOX_NS,
};

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

pub fn build_pubsub_items_iq(to: &BareJid, node: &SpaceNode) -> Element {
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

pub fn build_enable_push_iq(push_service_jid: &str, node: &str, token: &str) -> Element {
    let id = format!("push-enable-{}", next_id());
    let mut enable = Element::builder("enable", PUSH_NS)
        .attr("jid", push_service_jid)
        .attr("node", node);
    if !token.is_empty() {
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
        enable = enable.append(form);
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", id)
        .append(enable.build())
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
