use super::*;

pub(super) fn spaces_service_bare_jid(spaces_domain: &str) -> Result<BareJid, String> {
    spaces_domain
        .parse::<BareJid>()
        .map_err(|error| format!("invalid spaces service JID: {error}"))
}

pub(super) fn space_details_from_node(
    node: &waddle_xmpp::pubsub::PubSubNode,
) -> Option<SpaceDetails> {
    let access_model = waddle_xmpp::SpaceAccessModel::from_pubsub(node.config.access_model)?;
    let name = if node.node_name == "general" {
        "General".to_string()
    } else {
        node.node_name.clone()
    };
    Some(SpaceDetails {
        id: node.node_name.clone(),
        name,
        description: None,
        owner_id: node.owner.to_string(),
        icon_url: None,
        is_public: matches!(
            node.config.access_model,
            waddle_xmpp::pubsub::AccessModel::Open
        ),
        access_model,
        created_at: node.created_at.to_rfc3339(),
    })
}

/// A room's XEP-0503 space link, resolved for disco#info.
///
/// Carries the `urn:xmpp:spaces:0` parent form plus the space node IRI
/// destined for the `muc#roomconfig_pubsub` field of the room's single
/// `muc#roominfo` form — the roominfo form itself is composed by the
/// disco#info handler so the response never carries two forms with the
/// same FORM_TYPE (XEP-0115 §5.4, #1259).
pub(super) struct RoomSpaceLink {
    pub(super) parent_form: Element,
    pub(super) pubsub_iri: String,
}

pub(super) async fn room_space_link(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Option<RoomSpaceLink> {
    let spaces_domain = state.deps.service_domains.spaces.clone();
    let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
        return None;
    };
    let room_item_id = room_jid.to_string();
    match state
        .deps
        .protocol
        .pubsub_storage
        .find_node_for_item(&spaces_jid, &room_item_id)
        .await
    {
        Ok(Some(space_node)) => Some(RoomSpaceLink {
            parent_form: build_space_parent_form(&spaces_domain, &space_node.node_name),
            pubsub_iri: build_space_node_iri(&spaces_domain, &space_node.node_name),
        }),
        Ok(None) => None,
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to find Space node for room");
            None
        }
    }
}
