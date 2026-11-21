use super::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    build_spaces_metadata_form_for_requester_with_owners, chrono, internal_server_error_iq_error,
    item_not_found_iq_error, space_affiliation_for_requester, space_details_from_node,
    spaces_service_bare_jid, spaces_service_features, warn, DiscoInfoRequest, DiscoInfoResponse,
    Feature, Identity, WebSocketState,
};
use crate::permissions::host_grant::ScopeKey;
use crate::server::routes::websocket::frame::ResponseFrame;
use crate::server::routes::websocket::ResolvedPrincipal;
use crate::space_identity::SpaceNode;
use jid::FullJid;
use xmpp_parsers::disco::{DiscoInfoQuery, DiscoInfoResult};
use xmpp_parsers::iq::Iq;

/// Build a discovery reply on the canonical local Space grant path.
pub(in crate::server::routes::websocket::handlers::iq) async fn local_protected_space_info(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    session: Option<&super::Session>,
) -> Option<ResponseFrame> {
    let service = state.deps.service_domains.spaces.as_str();
    if iq.to()?.as_str() != service {
        return None;
    }
    let Iq::Get { payload, .. } = iq else {
        return None;
    };
    let node = DiscoInfoQuery::try_from(payload.clone()).ok()?.node?;
    let gate = &state.deps.app_state.host_grants;
    if gate.space_enrolled(&node).await != Ok(true) {
        return None;
    }
    let requester = requester?;
    if session?.user_jid != requester.to_bare().as_str() {
        return None;
    }
    let service = spaces_service_bare_jid(service).ok()?;
    let key = ScopeKey::new(service.clone(), &node).ok()?;
    let grant = gate.space_reply_local(&key, requester, iq).await?;
    let title = state
        .deps
        .app_state
        .spaces_metadata_store
        .get_by_node(&SpaceNode::from(node.as_str()))
        .await
        .ok()
        .flatten()
        .map_or_else(|| node.clone(), |metadata| metadata.name);
    let mut info = xmpp_parsers::minidom::Element::from(DiscoInfoResult {
        node: Some(node),
        identities: vec![xmpp_parsers::disco::Identity {
            category: "pubsub".to_owned(),
            type_: "leaf".to_owned(),
            lang: None,
            name: Some(title.clone()),
        }],
        features: vec![xmpp_parsers::disco::Feature::new(
            "http://jabber.org/protocol/disco#info",
        )],
        extensions: Vec::new(),
    });
    info.append_child(waddle_xmpp::xep::build_spaces_metadata_form_for_remote(
        &title,
    ));
    let response = Iq::Result {
        from: Some(service.into()),
        to: Some(requester.clone().into()),
        id: iq.id().to_owned(),
        payload: Some(info),
    };
    ResponseFrame::protected_space_frame(super::iq_to_xml(response), grant).ok()
}

/// Guard an ordinary result against enrollment while its socket is waiting.
pub(in crate::server::routes::websocket::handlers::iq) fn guard_unmanaged_space_info(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    mut replies: Vec<String>,
) -> crate::server::routes::websocket::frame::ResponseBatch {
    let service = state.deps.service_domains.spaces.as_str();
    let Iq::Get { payload, .. } = iq else {
        return replies.into();
    };
    if iq.to().is_none_or(|to| to.as_str() != service) {
        return replies.into();
    }
    let Some(node) = DiscoInfoQuery::try_from(payload.clone())
        .ok()
        .and_then(|query| query.node)
    else {
        return replies.into();
    };
    let Some(mut reply @ Iq::Result { .. }) = replies
        .first()
        .and_then(|xml| xml.parse::<xmpp_parsers::minidom::Element>().ok())
        .and_then(|element| Iq::try_from(element).ok())
    else {
        return replies.into();
    };
    let guarded = (|| {
        let requester = requester?;
        let service = spaces_service_bare_jid(service).ok()?;
        let guard = state
            .deps
            .app_state
            .host_grants
            .unmanaged_space_disco_reply_local(&service, &node, requester, iq)?;
        if let Iq::Result { to, .. } = &mut reply {
            *to = Some(requester.clone().into());
        }
        ResponseFrame::unmanaged_space_frame(super::iq_to_xml(reply), guard).ok()
    })();
    if let Some(frame) = guarded {
        return crate::server::routes::websocket::frame::ResponseBatch::from_frames(vec![frame]);
    }
    replies[0] = super::iq_to_xml(Iq::Error {
        from: iq.to().cloned(),
        to: requester.map(|jid| jid.clone().into()),
        id: iq.id().to_owned(),
        payload: None,
        error: item_not_found_iq_error("Requested item not found."),
    });
    replies.into()
}

pub(super) async fn handle_spaces_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.spaces_domain) {
        return None;
    }

    if let Some(node) = req.node {
        // Enrolled Spaces require the protected reply path. The legacy
        // metadata path describes an unavailable enrollment as absent.
        if state.deps.app_state.host_grants.space_enrolled(node).await != Ok(false) {
            return Some(DiscoInfoResponse::error(
                req.id,
                None,
                None,
                item_not_found_iq_error("Requested item not found."),
            ));
        }
        let Ok(spaces_jid) = spaces_service_bare_jid(req.spaces_domain) else {
            return Some(DiscoInfoResponse::error(
                req.id,
                None,
                None,
                internal_server_error_iq_error("Internal server error."),
            ));
        };
        let space_node = match state
            .deps
            .protocol
            .pubsub_storage
            .get_node(&spaces_jid, node)
            .await
        {
            Ok(Some(node)) => node,
            Ok(None) => {
                return Some(DiscoInfoResponse::error(
                    req.id,
                    None,
                    None,
                    item_not_found_iq_error("Requested item not found."),
                ));
            }
            Err(error) => {
                warn!(node, error = %error, "Failed to resolve Spaces node info");
                return Some(DiscoInfoResponse::error(
                    req.id,
                    None,
                    None,
                    item_not_found_iq_error("Requested item not found."),
                ));
            }
        };

        let Some(mut space) = space_details_from_node(&space_node) else {
            warn!(
                node,
                access_model = %space_node.config.access_model,
                "Spaces node has unsupported access model"
            );
            return Some(DiscoInfoResponse::error(
                req.id,
                None,
                None,
                internal_server_error_iq_error("Internal server error."),
            ));
        };
        if let Ok(Some(metadata)) = state
            .deps
            .app_state
            .spaces_metadata_store
            .get_by_node(&SpaceNode::from(node))
            .await
        {
            space.name = metadata.name;
            space.description = metadata.description;
            space.icon_url = metadata.icon_url;
            if let Some(created_at) =
                chrono::DateTime::<chrono::Utc>::from_timestamp(metadata.created_at, 0)
            {
                space.created_at = created_at.to_rfc3339();
            }
        }
        let requester_affiliation = space_affiliation_for_requester(state, principal, node).await;
        let owner_jids = state
            .deps
            .protocol
            .pubsub_storage
            .list_node_affiliations(&spaces_jid, node)
            .await
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|(jid, affiliation)| {
                        (affiliation == waddle_xmpp::pubsub::Affiliation::Owner)
                            .then(|| jid.to_string())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let identities = vec![Identity::pubsub_leaf(Some(&space.name))];
        let features = vec![
            Feature::disco_info(),
            Feature::pubsub(),
            Feature::pubsub_retrieve_items(),
            Feature::spaces(),
        ];
        let metadata = build_spaces_metadata_form_for_requester_with_owners(
            &space,
            requester_affiliation,
            &owner_jids,
        );
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            Some(node),
            &[metadata],
        );
        return Some(DiscoInfoResponse::iq(response));
    }

    let identities = vec![Identity::spaces_service(Some("Spaces"))];
    let features = spaces_service_features();
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
