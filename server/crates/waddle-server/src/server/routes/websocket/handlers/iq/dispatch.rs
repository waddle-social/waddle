//! Implementation behind the parent module facade.

use super::build_iq_error_xml_typed;
use super::handle_archive_inbox_upload_iq;
use super::handle_blocking_iq;
use super::handle_caps_disco_info_result;
use super::handle_disco_info_iq;
use super::handle_disco_items_iq;
use super::handle_last_activity_iq;
use super::handle_muc_admin_iq;
use super::handle_muc_owner_and_moderation_iq;
use super::handle_muc_self_ping_iq;
use super::handle_pubsub_iq;
use super::handle_push_iq;
use super::handle_roster_iq;
use super::handle_sans_io_iq;
use super::is_muc_admin_iq;

use super::route_full_jid_iq;
use super::ConnectionPhase;
use super::IqConnState;
use super::ResolvedPrincipal;
use super::ResponseBatch;
use super::Session;
use super::WebSocketState;
use super::ROSTER_NS;
use super::{
    bad_request_iq_error, feature_not_implemented_iq_error, forbidden_iq_error,
    internal_server_error_iq_error, item_not_found_iq_error,
};
use super::{debug, warn};
use super::{handle_channel_search_iq, handle_user_search_iq};
use super::{handle_command_iq, CommandTargets};
use super::{handle_link_preview_lookup_iq, is_link_preview_lookup_iq};
use super::{handle_mentions_permissions_iq, is_mentions_permissions_iq};
use super::{handle_pin_query_iq, is_pin_query_iq};
use super::{handle_private_storage_iq, handle_vcard_iq};
use super::{is_last_activity_query, is_search_request};
use super::{is_push_disable, is_push_enable};
use super::{StanzaErrorCondition, XmppError};

pub(in crate::server::routes::websocket::handlers) fn response_batch_from_inline_room_effect_frames(
    frames: crate::room_effect_outbox::drain::InlineRoomEffectDrain,
) -> ResponseBatch {
    let mut batch = ResponseBatch::default();
    for frame in frames.frames {
        batch.push_completion_frame(frame.stanza, frame.completion);
    }
    batch
}

pub(in crate::server::routes::websocket::handlers::iq) fn push_service_stanza_error(
    error: XmppError,
) -> xmpp_parsers::stanza_error::StanzaError {
    match error {
        XmppError::Stanza {
            condition: StanzaErrorCondition::BadRequest,
            ..
        } => bad_request_iq_error("Malformed Push Service request."),
        XmppError::Stanza {
            condition: StanzaErrorCondition::ItemNotFound,
            ..
        } => item_not_found_iq_error("Requested Push Service item not found."),
        XmppError::Stanza {
            condition: StanzaErrorCondition::Forbidden,
            ..
        }
        | XmppError::PermissionDenied(_) => forbidden_iq_error("Push Service request forbidden."),
        _ => internal_server_error_iq_error("Internal server error."),
    }
}

#[derive(Clone, Copy)]
pub(in crate::server::routes::websocket::handlers) struct IqHandlerContext<'a> {
    pub(in crate::server::routes::websocket::handlers) iq: &'a xmpp_parsers::iq::Iq,
    pub(in crate::server::routes::websocket::handlers) id: &'a str,
    pub(in crate::server::routes::websocket::handlers) payload_ns: &'a str,
    pub(in crate::server::routes::websocket::handlers) target_to: Option<&'a str>,
    pub(in crate::server::routes::websocket::handlers) has_destroy: bool,
    pub(in crate::server::routes::websocket::handlers) domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) muc_domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) upload_domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) spaces_domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) community_domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) extensions_domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) push_domain: &'a str,
    pub(in crate::server::routes::websocket::handlers) response_from: Option<&'a str>,
    pub(in crate::server::routes::websocket::handlers) response_to: Option<&'a str>,
}

pub async fn handle_iq_with_conn_state(
    iq: xmpp_parsers::iq::Iq,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: Option<&Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> ResponseBatch {
    let spaces_domain = state.deps.service_domains.spaces.clone();
    let community_domain = state.deps.service_domains.community.clone();
    let upload_domain = state.deps.service_domains.upload.clone();
    let extensions_domain = state.deps.service_domains.extensions.clone();
    let push_domain = state.deps.service_domains.push.clone();

    let id = iq.id().to_string();
    let to = iq.to().map(|jid| jid.to_string());
    let from = iq.from().map(|jid| jid.to_string());
    let response_from = to.as_deref();
    let response_to = from.as_deref();

    if matches!(
        &iq,
        xmpp_parsers::iq::Iq::Result { .. } | xmpp_parsers::iq::Iq::Error { .. }
    ) {
        if let Some(full) = phase.bound_jid() {
            handle_caps_disco_info_result(&iq, full, state);
        }
        debug!(id = %id, "Ignoring IQ result/error stanza");
        return ResponseBatch::default();
    }

    let payload_ns = match &iq {
        xmpp_parsers::iq::Iq::Get { payload: e, .. }
        | xmpp_parsers::iq::Iq::Set { payload: e, .. } => e.ns(),
        _ => String::new(),
    };
    let has_destroy = match &iq {
        xmpp_parsers::iq::Iq::Set { payload: e, .. } => e
            .get_child("destroy", "http://jabber.org/protocol/muc#owner")
            .is_some(),
        _ => false,
    };

    if waddle_xmpp::xep::xep0410::is_self_ping(&iq)
        && iq.to().is_some_and(|to| to.domain().as_str() == muc_domain)
    {
        return handle_muc_self_ping_iq(&iq, state, phase.bound_jid(), response_from, response_to)
            .await
            .into();
    }

    if is_link_preview_lookup_iq(&iq) {
        return handle_link_preview_lookup_iq(
            &iq,
            phase.bound_jid(),
            state,
            muc_domain,
            response_from,
            response_to,
            state.deps.occupant_id_secret.key(),
        )
        .await
        .into();
    }

    if payload_ns == ROSTER_NS {
        return handle_roster_iq(
            &iq,
            domain,
            state,
            phase.bound_jid(),
            conn_state.roster_interested,
            conn_state.registry_owner,
        )
        .await
        .into();
    }

    // Namespace-based dispatch takes priority over the generic
    // full-jid forwarder ONLY for the handful of namespaces whose
    // server-side handler must mediate the stanza before it reaches
    // the peer — e.g. XEP-0166 Jingle stamping a LiveKit join token
    // onto the Waddle transport in the rewrite path. For other
    // handler-registered namespaces (XEP-0199 ping, XEP-0030
    // disco#info, etc.) the historical contract is that IQs
    // addressed to another user's full JID are forwarded verbatim
    // and answered by that user's real client, not by our local
    // handler — preserve that.
    let payload_mediates_peer_routing = matches!(
        payload_ns.as_str(),
        s if s == waddle_xmpp::xep::xep0166::NS_JINGLE
            || s == waddle_xmpp::xep::xep0215::NS_EXT_DISCO
    );
    if !payload_mediates_peer_routing {
        if let Some(target) = iq.to().and_then(|jid| jid.clone().try_into_full().ok()) {
            if target.domain().as_str() == domain {
                return route_full_jid_iq(
                    iq,
                    state,
                    phase.bound_jid(),
                    target,
                    response_from,
                    conn_state.ordered_relay_origin.clone(),
                )
                .await
                .into();
            }
        }
    }

    let handler_ctx = IqHandlerContext {
        iq: &iq,
        id: id.as_str(),
        payload_ns: payload_ns.as_str(),
        target_to: to.as_deref(),
        has_destroy,
        domain,
        muc_domain,
        upload_domain: upload_domain.as_str(),
        spaces_domain: spaces_domain.as_str(),
        community_domain: community_domain.as_str(),
        extensions_domain: extensions_domain.as_str(),
        push_domain: push_domain.as_str(),
        response_from,
        response_to,
    };

    // `Some(_)` means a registered dispatcher handler owned this IQ;
    // it is terminal even when the frame list is empty (e.g. a Jingle
    // 1:1 stanza forwarded to the peer with no synchronous reply for
    // the sender). Only `None` — no handler claimed the namespace —
    // continues to the remaining branches below.
    if let Some(frames) =
        handle_sans_io_iq(handler_ctx, state, authenticated_session, phase, conn_state).await
    {
        return frames.into();
    }

    // jabber:iq:roster is served by handle_roster_iq above because it needs
    // durable roster storage and roster-push fanout.
    let misc_response = if waddle_xmpp::xep::xep0054::is_vcard_get(&iq)
        || waddle_xmpp::xep::xep0054::is_vcard_set(&iq)
    {
        handle_vcard_iq(&iq, state, phase.bound_jid(), response_from, response_to).await
    } else if is_last_activity_query(&iq) {
        handle_last_activity_iq(
            &iq,
            domain,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await
    } else if waddle_xmpp::xep::xep0049::is_private_storage_query(&iq) {
        handle_private_storage_iq(&iq, state, phase.bound_jid(), response_from, response_to).await
    } else if waddle_xmpp::xep::xep0191::is_blocking_query(&iq) {
        handle_blocking_iq(
            &iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
            conn_state,
        )
        .await
    } else if is_push_enable(&iq) || is_push_disable(&iq) {
        handle_push_iq(
            &iq,
            state,
            phase.bound_jid(),
            push_domain.as_str(),
            response_from,
            response_to,
        )
        .await
    } else if is_search_request(&iq) {
        handle_channel_search_iq(&iq, muc_domain, state, response_from, response_to).await
    } else if payload_ns == "jabber:iq:search" {
        handle_user_search_iq(&iq, domain, state, response_from, response_to).await
    } else {
        Vec::new()
    };
    if !misc_response.is_empty() {
        return misc_response.into();
    }

    let disco_info_response =
        handle_disco_info_iq(handler_ctx, state, phase, authenticated_session).await;
    if !disco_info_response.is_empty() {
        return disco_info_response.into();
    }

    let disco_items_response = handle_disco_items_iq(handler_ctx, state, phase).await;
    if !disco_items_response.is_empty() {
        return disco_items_response.into();
    }

    if payload_ns == "http://jabber.org/protocol/commands" {
        return handle_command_iq(
            &iq,
            state,
            CommandTargets {
                domain,
                muc_domain,
                extensions_domain: &extensions_domain,
                push_domain: &push_domain,
            },
            authenticated_session.map(ResolvedPrincipal::from_authenticated_session),
            phase.bound_jid(),
        )
        .await
        .into();
    }

    if payload_ns == "http://jabber.org/protocol/muc#admin" && is_muc_admin_iq(&iq, muc_domain) {
        return handle_muc_admin_iq(
            &iq,
            muc_domain,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    if is_pin_query_iq(&iq, muc_domain, domain) {
        return handle_pin_query_iq(&iq, state, phase.bound_jid(), response_from, response_to)
            .await
            .into();
    }

    if is_mentions_permissions_iq(&iq, muc_domain) {
        return handle_mentions_permissions_iq(
            &iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await
        .into();
    }

    let muc_owner_or_moderation =
        handle_muc_owner_and_moderation_iq(handler_ctx, state, phase, authenticated_session).await;
    if !muc_owner_or_moderation.frames.is_empty() || !muc_owner_or_moderation.completions.is_empty()
    {
        return muc_owner_or_moderation;
    }

    let archive_inbox_upload =
        handle_archive_inbox_upload_iq(&iq, &id, payload_ns.as_str(), domain, state, phase).await;
    if !archive_inbox_upload.is_empty() {
        return archive_inbox_upload.into();
    }

    let pubsub_response = handle_pubsub_iq(handler_ctx, state, phase, authenticated_session).await;
    if !pubsub_response.is_empty() {
        return pubsub_response.into();
    }

    // Unknown IQ - log a compact summary and return an error.
    let payload_ns = (!payload_ns.is_empty()).then_some(payload_ns.as_str());
    warn!(id = %id, payload_ns, "Unhandled IQ stanza");
    vec![build_iq_error_xml_typed(
        &id,
        response_from,
        response_to,
        feature_not_implemented_iq_error("Requested feature not implemented."),
    )]
    .into()
}
