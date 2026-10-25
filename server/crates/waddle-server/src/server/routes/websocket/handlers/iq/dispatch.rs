//! Implementation behind the parent module facade.

use super::apply_muc_owner_config;
use super::archive_inbox_upload;
use super::blocking;
use super::build_config_form;
use super::caps_result;
use super::chrono;
use super::commands;
use super::community_items;
use super::community_publish;
use super::community_retract;
use super::community_rsvp;
use super::conn_state;
use super::disco_info;
use super::disco_items;
use super::errors;
use super::extension_forms;
use super::extension_route_items;
use super::full_jid_forward;
use super::handle_archive_inbox_upload_iq;
use super::handle_blocking_iq;
use super::handle_caps_disco_info_result;
use super::handle_community_items;
use super::handle_community_publish;
use super::handle_community_retract;
use super::handle_disco_info_iq;
use super::handle_disco_items_iq;
#[cfg(test)]
use super::handle_iq;
use super::handle_last_activity_iq;
use super::handle_muc_admin_iq;
use super::handle_muc_owner_and_moderation_iq;
use super::handle_muc_self_ping_iq;
use super::handle_pubsub_iq;
use super::handle_push_iq;
use super::handle_roster_iq;
use super::handle_sans_io_iq;
use super::handle_spaces_items;
use super::handle_spaces_publish;
use super::handle_spaces_retract;
use super::is_pep_self_or_to;
use super::jingle_muji_gate;
#[cfg(feature = "clustering")]
use super::jingle_muji_relay;
use super::last_activity;
use super::link_preview_lookup;
use super::link_preview_player_embed;
use super::link_preview_resolver;
use super::managed_channel_permission_allowed;
use super::mark_span_error;
use super::mentions_permissions;
use super::muc_admin;
#[cfg(feature = "clustering")]
use super::muc_admin_relay;
use super::muc_occupant_disco;
use super::muc_owner_config;
use super::muc_owner_moderation;
use super::muc_self_ping;
use super::pep_addressing;
use super::permissions;
use super::persist_managed_channel_affiliation;
use super::pin_query;
use super::pubsub_admin;
use super::pubsub_dispatch;
use super::push;
use super::roster;
use super::route_full_jid_iq;
use super::sans_io;
use super::search;
use super::session_jid;
use super::spaces_bookmark_cleanup;
use super::spaces_discovery;
use super::spaces_items;
use super::spaces_publish;
use super::spaces_retract;
use super::story_attachments;
use super::vcard_private;
use super::Arc;
use super::BroadcastOutcome;
use super::DatabaseBlockingStorage;
use super::Element;
use super::HashSet;
use super::IqConnState;
use super::ResolvedPrincipal;
use super::ResponseBatch;
use super::VCardStore;
use super::CARBONS_NS;
use super::DATA_FORMS_NS;
use super::DEPLOYMENT_SERVER_ID;
use super::{
    add_stanza_id_xep0359, build_command_items, build_command_result, build_last_activity_response,
    build_moderation_result_message, build_room_metadata_form, build_search_form_response,
    build_search_response_with_rsm, build_server_role_form, build_space_node_iri,
    build_space_parent_form, build_spaces_metadata_form_for_requester_with_owners,
    is_last_activity_query, is_search_form_request, is_search_request, is_time_query,
    is_version_query, parse_command_from_iq, parse_moderation_iq, parse_search_request,
    AdHocCommandCondition, ChannelResult, Command, CommandError, CommandStatus, RsmRequest,
    RsmResponse, Searchable, SpaceAffiliation, Xep0359StanzaId, NODE_COMMANDS, NS_CHANNEL_SEARCH,
};
use super::{
    bad_format_iq_error, bad_request_iq_error, conflict_iq_error, feature_not_implemented_iq_error,
    forbidden_iq_error, internal_server_error_iq_error, item_not_found_iq_error,
    jid_malformed_iq_error, not_acceptable_iq_error, not_allowed_iq_error, not_authorized_iq_error,
    service_unavailable_iq_error,
};
use super::{
    build_admin_result, build_admin_set_result, build_role_result, is_muc_admin_iq,
    is_role_change_query, parse_admin_query, AdminItem,
};
use super::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    build_disco_items_response, community_service_features, muc_room_features,
    muc_service_features, parse_disco_info_query, parse_disco_items_query, push_service_features,
    spaces_service_features, upload_service_features, DiscoItem, Feature, Identity,
};
use super::{
    build_fin_iq, build_query_form_iq, build_result_messages, is_mam_query,
    is_mam_query_form_request, parse_mam_query, ArchivedMessage, ArchivedModeration,
    ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
};
use super::{
    build_inbox_entry_message, build_inbox_fin_iq, build_mark_read_result, is_inbox_iq,
    is_mark_read_iq, parse_inbox_query, parse_mark_read, InboxFinCounts, InboxLastMessage,
};
use super::{build_iq_error_xml_typed, build_iq_error_xml_with_payload};
use super::{
    build_iq_result_xml, element_to_xml, get_room_actor, get_room_actor_result, iq_to_xml,
    is_muc_room_jid, stanza_to_xml, WebSocketState,
};
use super::{
    build_muc_owner_config_response, build_xmpp_error_response, global_database,
    muc_owner_authorized, server_affiliation_for_requester, space_affiliation_for_requester,
    spaces_node_mutation_allowed,
};
use super::{
    build_pep_identity, build_pubsub_affiliations_result, build_pubsub_configure_form_result,
    build_pubsub_error, build_pubsub_items_result, build_pubsub_owner_subscriptions_result,
    build_pubsub_publish_result, build_pubsub_subscribe_result, build_pubsub_success,
    is_pep_request, is_pep_request_to, is_pubsub_iq, parse_pubsub_iq, pep_features, PubSubError,
    PubSubItem, PubSubRequest, SubId, SubscriptionState, PEP_NODE_AVATAR_DATA,
    PEP_NODE_AVATAR_METADATA,
};
use super::{
    build_push_disable_result, build_push_enable_result, is_push_disable, is_push_enable,
    parse_push_disable, parse_push_enable,
};
use super::{
    build_roster_push, build_roster_result, build_roster_result_empty, parse_roster_get,
    parse_roster_set, AskType, RosterItem, RosterSetResult, RosterVersion, Subscription, ROSTER_NS,
};
use super::{build_subscription_presence, SubscriptionStateMachine, SubscriptionType};
use super::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadError, UploadSlot,
};
use super::{
    command_name_by_boundary, command_refs_by_boundary, extension_command_metadata_form,
    extension_features_for_disco, extension_route_disco_node, extension_route_metadata_form,
    CommandBoundary, EXTENSION_COMMAND_FORM_TYPE, EXTENSION_ROUTE_FORM_TYPE,
};
use super::{debug, info, warn};
use super::{
    get_managed_channel_for_room, resolve_muc_room_archive_access,
    send_current_presence_from_user_to_jid, send_unavailable_presence_from_user_to_jid,
    RoomArchiveAccess,
};
use super::{get_xmpp_channel, list_xmpp_channels, XmppChannelRecord};
use super::{handle_channel_search_iq, handle_user_search_iq};
use super::{handle_command_iq, CommandTargets};
use super::{handle_extension_route_items, PubSubItemsRead};
use super::{handle_link_preview_lookup_iq, is_link_preview_lookup_iq};
use super::{handle_mentions_permissions_iq, is_mentions_permissions_iq};
use super::{handle_pin_query_iq, is_pin_query_iq};
use super::{handle_private_storage_iq, handle_vcard_iq};
use super::{local_account_exists, local_account_jid_exists, Session};
use super::{room_space_link, space_details_from_node, spaces_service_bare_jid, RoomSpaceLink};
use super::{row_value, Database, Value, ValueExt};
use super::{
    server_policy_for_managed_channel, ManagedChannelServerPolicy,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS,
};
use super::{Affiliation, SpaceDetails, Stanza, StanzaErrorCondition, StanzaErrorType, XmppError};
use super::{
    ApplyAdminItems, EnforceMembersOnly, EnforceMembersOnlyAffiliations, GetAdminContext,
    GetOccupantByJid, GetSnapshot, PingSelfCheck, UpdateConfig,
};
use super::{BareJid, FullJid, Jid};
use super::{
    CheckPermission, Object, ObjectType, Permission, PermissionError, Relation, Subject,
    SubjectType, Tuple, WriteTuple,
};
use super::{CommandContext, CommandResult};
use super::{ConnectionPhase, ProtocolStanzaContext};
use super::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterRowMutationKind,
    RosterStorageError,
};
use super::{DbExecute, DbQuery, DbQueryOne, GetDatabase};

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
