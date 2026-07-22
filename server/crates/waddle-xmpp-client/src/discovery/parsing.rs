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
    // XEP-0068 §4.3 (Incorrectly Specified FORM_TYPE): "If the FORM_TYPE
    // field is not hidden in a form with type='form' or type='result',
    // it MUST be ignored as a context indicator." Two conditions —
    // BOTH required:
    //
    //   1. The wrapping `<x/>` element MUST be `type='form'` or
    //      `type='result'`. A `submit` form is a user-supplied payload,
    //      not a context-bearing advertisement; a `cancel` form
    //      carries no fields by definition; an absent `type=` is
    //      malformed per XEP-0004 §3.1.
    //
    //   2. The FORM_TYPE field itself MUST carry `type='hidden'`.
    //
    // Downstream callers (e.g. push_vapid::parse_push_vapid_from_disco_info)
    // pick the form they trust by `form_type` match — so leaking a
    // FORM_TYPE from a `submit`/`cancel`/unknown form would let a
    // malicious or misconfigured server smuggle an attacker-claimed
    // context indicator into the typed result.
    let form_kind = form.attr("type");
    let form_type = if matches!(form_kind, Some("form") | Some("result")) {
        form.children()
            .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
            .find(|field| {
                field.attr("var") == Some("FORM_TYPE") && field.attr("type") == Some("hidden")
            })
            .and_then(|field| {
                field
                    .children()
                    .find(|child| child.name() == "value" && child.ns() == DATA_FORMS_NS)
                    .map(Element::text)
            })
    } else {
        None
    };
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

/// Resolve the MUC and Spaces service JIDs from a server's
/// `disco#items` items list, hydrated with each item's `disco#info`
/// where available. The result preserves the supplied fallbacks for
/// any component that the server does not explicitly advertise
/// (most small deployments only advertise the MUC component).
///
/// MUC services are identified by the XEP-0045 feature
/// `http://jabber.org/protocol/muc` (or the `conference` disco
/// category as a legacy fallback). Spaces services are identified
/// strictly by the `urn:xmpp:spaces:0` feature — a generic pubsub
/// identity is not enough because Waddle's extensions component is
/// also pubsub but unrelated to spaces (this is the same shape the
/// chat web client uses; see `chat/src/lib/xmpp/discovery.ts`).
///
/// The first matching service wins for each component; subsequent
/// matches are ignored.
///
/// Gated to the native non-wasm build (where `discovery::ext` is
/// compiled) plus the test build (which exercises this helper
/// directly). The wasm browser client performs the equivalent
/// disco walk in TypeScript (`chat/src/lib/xmpp/discovery.ts`),
/// so the function would otherwise be dead in the wasm crate's
/// dependency closure and trip the rustc `dead_code` warning.
#[cfg(any(all(feature = "native", not(target_arch = "wasm32")), test,))]
pub fn resolve_component_services(
    items: &[(jid::BareJid, Option<DiscoInfoResult>)],
    fallback_muc: jid::BareJid,
    fallback_spaces: jid::BareJid,
) -> super::types::DiscoveredComponentServices {
    use super::{MUC_NS, SPACES_NS};

    let mut muc: Option<jid::BareJid> = None;
    let mut spaces: Option<jid::BareJid> = None;
    for (jid, info_opt) in items {
        let Some(info) = info_opt else {
            continue;
        };
        if muc.is_none()
            && (info.has_feature(MUC_NS)
                || info
                    .identities
                    .iter()
                    .any(|identity| identity.category == "conference"))
        {
            muc = Some(jid.clone());
        }
        if spaces.is_none() && info.has_feature(SPACES_NS) {
            spaces = Some(jid.clone());
        }
    }
    super::types::DiscoveredComponentServices {
        muc: muc.unwrap_or(fallback_muc),
        spaces: spaces.unwrap_or(fallback_spaces),
    }
}

pub fn space_from_disco_item(
    spaces_jid: &BareJid,
    item: DiscoItem,
    info: &DiscoInfoResult,
) -> Option<DiscoveredSpace> {
    if item.jid != spaces_jid.to_string() {
        return None;
    }
    let id = SpaceNode::new(item.node?)?;
    if info.node.as_deref() != Some(id.as_str()) {
        return None;
    }
    if !info.has_form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#type", SPACES_NS) {
        return None;
    }
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
            // XEP-0402 §2.2: an absent autojoin attr means `false`
            // (web parity — `bookmark.autojoin ?? false`). The user's
            // own PEP bookmark can still override this later in
            // `discover_topology`.
            let autojoin = matches!(conference.attr("autojoin"), Some("true") | Some("1"));
            Some((id, room_jid, name, autojoin))
        })
        .enumerate()
        .map(
            |(position, (id, room_jid, name, autojoin))| DiscoveredChannel {
                id,
                room_jid,
                name,
                description: None,
                channel_type: DiscoveredChannelType::Text,
                position: position as i32,
                space_id: space_id.clone(),
                autojoin,
                bookmark_name: None,
                is_group_dm: false,
            },
        )
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
