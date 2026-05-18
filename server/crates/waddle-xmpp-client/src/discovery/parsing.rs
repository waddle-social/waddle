use jid::BareJid;
use minidom::Element;

use super::types::{
    DiscoDataField, DiscoDataForm, DiscoIdentity, DiscoInfoResult, DiscoItem, DiscoveredChannel,
    DiscoveredChannelType, DiscoveredSpace, SpaceNode, UploadSlot,
};
use super::{
    BOOKMARKS_NS, DATA_FORMS_NS, DISCO_INFO_NS, DISCO_ITEMS_NS, PUBSUB_METADATA_FORM_TYPE,
    PUBSUB_NS, SPACES_NS, UPLOAD_NS,
};

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

pub fn space_from_disco_item(
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
