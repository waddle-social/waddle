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

pub(super) async fn room_space_metadata_extensions(
    state: &WebSocketState,
    room_jid: &BareJid,
    description: Option<&str>,
) -> Vec<Element> {
    let spaces_domain = state.deps.service_domains.spaces.clone();
    let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
        return vec![];
    };
    let room_item_id = room_jid.to_string();
    match state
        .deps
        .protocol
        .pubsub_storage
        .find_node_for_item(&spaces_jid, &room_item_id)
        .await
    {
        Ok(Some(space_node)) => build_room_space_metadata_forms_with_description(
            &spaces_domain,
            &space_node.node_name,
            description,
        ),
        Ok(None) => vec![],
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to find Space node for room");
            vec![]
        }
    }
}
