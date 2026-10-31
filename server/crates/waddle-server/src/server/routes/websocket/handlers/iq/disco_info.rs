use super::{
    bad_request_iq_error, build_disco_info_response, build_disco_info_response_with_extensions,
    build_iq_error_xml_typed, build_pep_identity, build_room_metadata_form, build_server_role_form,
    build_spaces_metadata_form_for_requester_with_owners, chrono, command_name_by_boundary,
    community_service_features, debug, extension_command_metadata_form,
    extension_features_for_disco, extension_route_disco_node, extension_route_metadata_form,
    get_managed_channel_for_room, get_room_actor, info, internal_server_error_iq_error, iq_to_xml,
    is_muc_room_jid, item_not_found_iq_error, muc_room_features, muc_service_features,
    parse_disco_info_query, pep_features, push_service_features, room_space_link,
    server_affiliation_for_requester, space_affiliation_for_requester, space_details_from_node,
    spaces_service_bare_jid, spaces_service_features, upload_service_features, warn, Affiliation,
    BareJid, CheckPermission, CommandBoundary, ConnectionPhase, DbQueryOne, Element, Feature,
    FullJid, GetSnapshot, Identity, IqHandlerContext, Object, ObjectType, Permission,
    RoomSpaceLink, Session, Subject, WebSocketState, DATA_FORMS_NS, EXTENSION_COMMAND_FORM_TYPE,
    EXTENSION_ROUTE_FORM_TYPE, NODE_COMMANDS, NS_CHANNEL_SEARCH,
};
use crate::server::routes::websocket::ResolvedPrincipal;

mod account;
mod calls_mixer;
mod community;
mod extensions;
mod muc;
mod server_info;
mod services;
mod spaces;

#[derive(Clone, Copy)]
struct DiscoInfoRequest<'a> {
    request_iq: &'a xmpp_parsers::iq::Iq,
    id: &'a str,
    node: Option<&'a str>,
    target_to: Option<&'a str>,
    requester: Option<&'a FullJid>,
    domain: &'a str,
    muc_domain: &'a str,
    upload_domain: &'a str,
    spaces_domain: &'a str,
    community_domain: &'a str,
    extensions_domain: &'a str,
    push_domain: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
}

enum DiscoInfoResponse<'a> {
    Iq(xmpp_parsers::iq::Iq),
    IqError {
        id: &'a str,
        from: Option<&'a str>,
        to: Option<&'a str>,
        error: xmpp_parsers::stanza_error::StanzaError,
    },
}

impl<'a> DiscoInfoResponse<'a> {
    fn iq(iq: xmpp_parsers::iq::Iq) -> Self {
        Self::Iq(iq)
    }

    fn error(
        id: &'a str,
        from: Option<&'a str>,
        to: Option<&'a str>,
        error: xmpp_parsers::stanza_error::StanzaError,
    ) -> Self {
        Self::IqError {
            id,
            from,
            to,
            error,
        }
    }

    fn into_xml(self) -> String {
        match self {
            Self::Iq(iq) => iq_to_xml(iq),
            Self::IqError {
                id,
                from,
                to,
                error,
            } => build_iq_error_xml_typed(id, from, to, error),
        }
    }
}

fn disco_info_xml(response: DiscoInfoResponse<'_>) -> Vec<String> {
    vec![response.into_xml()]
}

pub(super) async fn handle_disco_info_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: Option<&Session>,
) -> Vec<String> {
    if ctx.payload_ns != "http://jabber.org/protocol/disco#info" {
        return Vec::new();
    }

    // Pre-compute a flat string for the `target` log field so production
    // grep / OTLP queries can match on `target="upload.example.com"`
    // rather than the Debug-formatted `Some("upload.example.com")` an
    // `?Option<&str>` field would emit. An absent `to` renders as the
    // empty string — every disco#info IQ on the wire carries a target,
    // so this only fires for malformed payloads.
    let target = ctx.target_to.unwrap_or("");
    let query = match parse_disco_info_query(ctx.iq) {
        Ok(query) => query,
        Err(_) => {
            debug!(id = %ctx.id, target = %target, "disco#info malformed payload");
            return disco_info_xml(DiscoInfoResponse::error(
                ctx.id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            ));
        }
    };

    if let Some(reply) = phase.bound_jid().and_then(|sender| {
        super::remote_destination::remote_iq_error(
            ctx.iq,
            ctx.domain,
            &state.deps.service_domains,
            sender,
        )
    }) {
        info!(id = %ctx.id, target = %target, handler = "remote_destination", "disco#info answered");
        return disco_info_xml(DiscoInfoResponse::iq(reply));
    }

    // The per-handler "answered" debug below is the load-bearing trace
    // for #750-style silent-drops: every disco#info that reaches this
    // dispatcher ends in exactly one such line (one of the ten arms or
    // the fallback). Absence of any "answered" line for a request id is
    // the silent-drop signature. `parse_disco_info_query` already emits
    // a "Parsed disco#info query" debug per request, so we do not need
    // an extra entry log here.

    let request = DiscoInfoRequest {
        request_iq: ctx.iq,
        id: ctx.id,
        node: query.node.as_deref(),
        target_to: ctx.target_to,
        requester: phase.bound_jid(),
        domain: ctx.domain,
        muc_domain: ctx.muc_domain,
        upload_domain: ctx.upload_domain,
        spaces_domain: ctx.spaces_domain,
        community_domain: ctx.community_domain,
        extensions_domain: ctx.extensions_domain,
        push_domain: ctx.push_domain,
        response_from: ctx.response_from,
        response_to: ctx.response_to,
    };

    if let Some(response) = muc::handle_muc_disco_info(&request, state).await {
        info!(id = %ctx.id, target = %target, handler = "muc", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = calls_mixer::handle_calls_mixer_disco_info(&request) {
        info!(id = %ctx.id, target = %target, handler = "calls_mixer", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = server_info::handle_command_disco_info(&request, state).await {
        info!(id = %ctx.id, target = %target, handler = "commands", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = extensions::handle_extensions_disco_info(&request, state).await {
        info!(id = %ctx.id, target = %target, handler = "extensions", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = spaces::handle_spaces_disco_info(
        &request,
        state,
        authenticated_session.map(ResolvedPrincipal::from_authenticated_session),
    )
    .await
    {
        info!(id = %ctx.id, target = %target, handler = "spaces", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = community::handle_community_disco_info(&request) {
        info!(id = %ctx.id, target = %target, handler = "community", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = services::handle_upload_disco_info(&request) {
        info!(id = %ctx.id, target = %target, handler = "upload", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = services::handle_push_service_disco_info(&request, state, phase).await {
        info!(id = %ctx.id, target = %target, handler = "push", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = account::handle_account_disco_info(&request, state, phase).await {
        info!(id = %ctx.id, target = %target, handler = "account", "disco#info answered");
        return disco_info_xml(response);
    }

    info!(id = %ctx.id, target = %target, handler = "server_fallback", "disco#info answered");
    disco_info_xml(
        server_info::handle_server_disco_info(
            &request,
            state,
            authenticated_session.map(ResolvedPrincipal::from_authenticated_session),
        )
        .await,
    )
}
