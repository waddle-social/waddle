use jid::BareJid;
use minidom::Element;
use waddle_xmpp_core::roster::RosterItem;

use crate::messaging::{MucAffiliation, MucRole};

use super::ids::next_id;
use super::types::{
    MucAdminAffiliationItem, RosterResult, SpaceNode, UserSearchForm, UserSearchItem,
    UserSearchQuery, UserSearchResult, WaddleInboxMarkRead,
};
use super::{
    CLIENT_NS, DISCO_INFO_NS, DISCO_ITEMS_NS, MUC_ADMIN_NS, PUBSUB_NS, ROSTER_NS, UPLOAD_NS,
    USER_SEARCH_NS, WADDLE_INBOX_NS,
};

// ── IQ builders ──────────────────────────────────────────────────────────────

pub fn build_disco_info_iq(to: &str, node: Option<&str>) -> Element {
    let id = format!("disco-info-{}", next_id());
    let mut query_builder = Element::builder("query", DISCO_INFO_NS);
    if let Some(n) = node {
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), n);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(query_builder.build())
        .build()
}

pub fn build_disco_items_iq(to: &str, node: Option<&str>) -> Element {
    let id = format!("disco-items-{}", next_id());
    let mut query_builder = Element::builder("query", DISCO_ITEMS_NS);
    if let Some(n) = node {
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), n);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(query_builder.build())
        .build()
}

pub fn build_pubsub_items_iq(to: &BareJid, node: &SpaceNode) -> Element {
    let id = format!("pubsub-items-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("pubsub", PUBSUB_NS)
                .append(
                    Element::builder("items", PUBSUB_NS)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), service_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("request", UPLOAD_NS)
                .attr(minidom::rxml::xml_ncname!("filename").to_owned(), filename)
                .attr(
                    minidom::rxml::xml_ncname!("size").to_owned(),
                    size.to_string(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("content-type").to_owned(),
                    content_type,
                )
                .build(),
        )
        .build()
}

pub fn build_waddle_inbox_mark_read_iq(to: &str, mark_read: &WaddleInboxMarkRead) -> Element {
    let id = format!("waddle-mark-read-{}", next_id());
    let mut builder = Element::builder("mark-read", WADDLE_INBOX_NS).attr(
        minidom::rxml::xml_ncname!("partner").to_owned(),
        mark_read.partner.as_str(),
    );
    if let Some(thread) = mark_read.thread.as_deref() {
        builder = builder.attr(minidom::rxml::xml_ncname!("thread").to_owned(), thread);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(builder.build())
        .build()
}

pub fn build_roster_get_iq(to: Option<&str>, ver: Option<&str>) -> Element {
    let id = format!("roster-{}", next_id());
    let mut query = Element::builder("query", ROSTER_NS);
    if let Some(ver) = ver {
        query = query.attr(minidom::rxml::xml_ncname!("ver").to_owned(), ver);
    }
    let mut iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(query.build());
    if let Some(to) = to {
        iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("query", MUC_ADMIN_NS)
                .append(
                    Element::builder("item", MUC_ADMIN_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            affiliation.as_str(),
                        )
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
            item_builder = item_builder.attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid);
        }
        if let Some(nick) = item.nick.as_deref() {
            item_builder = item_builder.attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick);
        }
        if let Some(affiliation) = item.affiliation {
            item_builder = item_builder.attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                affiliation.as_str(),
            );
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(query.build())
        .build()
}

/// XEP-0045 §8.2 "Kicking an Occupant" (and general role changes):
/// `<iq type='set'><query xmlns='muc#admin'><item nick='…'
/// role='…'><reason/></item></query></iq>`. A kick is `role='none'`
/// addressed by nick — distinct from a §9.1 ban, which sets
/// `affiliation='outcast'` by bare JID via
/// [`build_muc_admin_affiliation_set_iq`].
pub fn build_muc_admin_role_set_iq(
    room_jid: &str,
    nick: &str,
    role: MucRole,
    reason: Option<&str>,
) -> Element {
    let id = format!("muc-admin-role-{}", next_id());
    let mut item = Element::builder("item", MUC_ADMIN_NS)
        .attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick)
        .attr(minidom::rxml::xml_ncname!("role").to_owned(), role.as_str());
    if let Some(reason) = reason.filter(|reason| !reason.is_empty()) {
        item = item.append(
            Element::builder("reason", MUC_ADMIN_NS)
                .append(reason)
                .build(),
        );
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("query", MUC_ADMIN_NS)
                .append(item.build())
                .build(),
        )
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
