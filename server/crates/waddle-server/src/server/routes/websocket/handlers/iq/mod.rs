use chrono;
use jid::{BareJid, FullJid, Jid};
use std::{collections::HashSet, sync::Arc};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    carbons::CARBONS_NS,
    commands::{CommandContext, CommandResult},
    disco::{
        build_disco_info_response, build_disco_info_response_with_extensions,
        build_disco_items_response, community_service_features, muc_room_features,
        muc_service_features, parse_disco_info_query, parse_disco_items_query,
        push_service_features, spaces_service_features, upload_service_features, DiscoItem,
        Feature, Identity,
    },
    isr::{build_isr_token_error, build_isr_token_result, is_isr_token_request, IsrToken, ISR_NS},
    mam::{
        build_fin_iq, build_query_form_iq, build_result_messages, is_mam_query,
        is_mam_query_form_request, parse_mam_query, ArchivedMessage, ArchivedModeration,
        ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
    },
    muc::{
        admin::{
            build_admin_result, build_admin_set_result, build_role_result, is_muc_admin_iq,
            is_role_change_query, parse_admin_query,
        },
        owner::build_config_form,
        room_actor::{
            ApplyAdminItems, GetAdminContext, GetOccupantByJid, GetSnapshot, PingSelfCheck,
            UpdateConfig,
        },
        DATA_FORMS_NS,
    },
    presence::subscription::{
        build_subscription_presence, SubscriptionStateMachine, SubscriptionType,
    },
    protocol::{ConnectionPhase, StanzaContext as ProtocolStanzaContext},
    pubsub::{
        build_pep_identity, build_pubsub_affiliations_result, build_pubsub_configure_form_result,
        build_pubsub_error, build_pubsub_items_result, build_pubsub_owner_subscriptions_result,
        build_pubsub_publish_result, build_pubsub_subscribe_result, build_pubsub_success,
        is_pep_request, is_pep_request_to, is_pubsub_iq, parse_pubsub_iq, pep_features,
        PubSubError, PubSubItem, PubSubRequest, SubId, SubscriptionState, PEP_NODE_AVATAR_DATA,
        PEP_NODE_AVATAR_METADATA,
    },
    registry::BroadcastOutcome,
    roster::{
        build_roster_push, build_roster_result, build_roster_result_empty, parse_roster_get,
        parse_roster_set, AskType, RosterItem, RosterSetResult, RosterVersion, Subscription,
        ROSTER_NS,
    },
    xep::xep0357::{
        build_push_disable_result, build_push_enable_result, is_push_disable, is_push_enable,
        parse_push_disable, parse_push_enable,
    },
    xep::xep0363::{
        build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
        parse_upload_request, sanitize_filename, UploadError, UploadSlot,
    },
    xep::xep0430::{
        build_inbox_entry_message, build_inbox_fin_iq, build_mark_read_result, is_inbox_iq,
        is_mark_read_iq, parse_inbox_query, parse_mark_read, InboxFinCounts, InboxLastMessage,
    },
    xep::{
        add_stanza_id_xep0359, build_command_items, build_command_result,
        build_last_activity_response, build_moderation_result_message, build_room_metadata_form,
        build_room_space_metadata_forms_with_description, build_search_response,
        build_server_role_form, build_spaces_metadata_form_for_requester_with_owners,
        is_last_activity_query, is_search_request, is_time_query, is_version_query,
        parse_command_from_iq, parse_moderation_iq, parse_search_request, AdHocCommandCondition,
        ChannelResult, Command, CommandError, CommandStatus, Searchable, SpaceAffiliation,
        Xep0359StanzaId, NODE_COMMANDS, NS_CHANNEL_SEARCH,
    },
    Affiliation, SpaceDetails, Stanza, StanzaErrorCondition, StanzaErrorType, XmppError,
};
use xmpp_parsers::minidom::Element;

mod archive_inbox_upload;
mod blocking;
mod caps_result;
mod commands;
mod conn_state;
mod disco_info;
mod disco_items;
pub(crate) mod errors;
mod extension_forms;
mod jingle_muji_gate;
mod last_activity;
mod link_preview_lookup;
mod link_preview_player_embed;
mod link_preview_resolver;
mod mentions_permissions;
mod misc;
mod muc_admin;
mod muc_owner_config;
mod muc_owner_moderation;
mod permissions;
mod pin_query;
mod pubsub_admin;
mod pubsub_dispatch;
mod pubsub_helpers;
mod push;
mod roster;
mod sans_io;
mod search;
mod session_misc;
#[cfg(test)]
mod test_helpers;
mod vcard_private;

use archive_inbox_upload::handle_archive_inbox_upload_iq;
use blocking::handle_blocking_iq;
use caps_result::handle_caps_disco_info_result;
use commands::handle_command_iq;
pub use conn_state::IqConnState;
use disco_info::handle_disco_info_iq;
use disco_items::handle_disco_items_iq;
use extension_forms::{
    command_name_by_boundary, command_refs_by_boundary, extension_command_metadata_form,
    extension_features_for_disco, extension_route_disco_node, extension_route_metadata_form,
    CommandBoundary, EXTENSION_COMMAND_FORM_TYPE, EXTENSION_ROUTE_FORM_TYPE,
};
use last_activity::handle_last_activity_iq;
use link_preview_lookup::{handle_link_preview_lookup_iq, is_link_preview_lookup_iq};
use mentions_permissions::{handle_mentions_permissions_iq, is_mentions_permissions_iq};
use muc_owner_config::apply_muc_owner_config;
use muc_owner_moderation::handle_muc_owner_and_moderation_iq;
pub(crate) use permissions::managed_channel_permission_allowed;
use permissions::{
    build_muc_owner_config_response, build_xmpp_error_response, global_database,
    muc_owner_authorized, server_affiliation_for_requester, space_affiliation_for_requester,
    spaces_node_mutation_allowed,
};
use pin_query::{handle_pin_query_iq, is_pin_query_iq};
use pubsub_dispatch::handle_pubsub_iq;
use pubsub_helpers::{
    canonical_channel_disco_items, handle_community_items, handle_community_publish,
    handle_community_retract, handle_extension_route_items, handle_spaces_items,
    handle_spaces_publish, handle_spaces_retract, is_pep_self_or_to,
    room_space_metadata_extensions, space_details_from_node, spaces_service_bare_jid,
    PubSubItemsRead,
};
#[cfg(test)]
pub use test_helpers::handle_iq;
// Re-exported via `pub(super)` so submodules that wildcard-import the
// IQ-handler module's namespace (`use super::*;`) pick up the typed
// IQ-error constructors without each having to re-import the
// `errors` submodule directly.
pub(super) use errors::{
    bad_format_iq_error, bad_request_iq_error, feature_not_implemented_iq_error,
    forbidden_iq_error, internal_server_error_iq_error, item_not_found_iq_error,
    jid_malformed_iq_error, not_acceptable_iq_error, not_authorized_iq_error,
    service_unavailable_iq_error,
};

fn push_service_stanza_error(error: XmppError) -> xmpp_parsers::stanza_error::StanzaError {
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
use misc::handle_misc_iq;
use muc_admin::handle_muc_admin_iq;
use push::handle_push_iq;
use roster::handle_roster_iq;
use sans_io::handle_sans_io_iq;
use search::{handle_channel_search_iq, handle_user_search_iq};
use session_misc::{handle_isr_token_request_iq, handle_muc_self_ping_iq, route_full_jid_iq};
use vcard_private::{handle_private_storage_iq, handle_vcard_iq};

// `build_iq_error_xml_typed` is re-exported via `pub(super) use` so
// submodules wildcard-importing this module's namespace see it.
pub(super) use super::super::build_iq_error_xml_typed;

use super::super::{
    build_iq_result_xml, destroy_room_actor, element_to_xml, get_room_actor, iq_to_xml,
    is_muc_room_jid, stanza_to_xml, WebSocketState,
};
use super::presence::{
    get_managed_channel_for_room, send_current_presence_from_user_to_jid,
    send_unavailable_presence_from_user_to_jid,
};
use crate::auth::{NativeUserStore, Session};
use crate::db::actor::{DbExecute, DbQuery, DbQueryOne, GetDatabase};
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterRowMutationKind,
    RosterStorageError,
};
use crate::db::{row_value, Database, Value, ValueExt};
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionError, Relation, Subject,
    SubjectType, Tuple, WriteTuple,
};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::managed_channel_policy::{
    server_policy_for_managed_channel, ManagedChannelServerPolicy,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS,
};
use crate::server::xmpp_state::{get_xmpp_channel, list_xmpp_channels, XmppChannelRecord};
use crate::vcard::VCardStore;

#[derive(Clone, Copy)]
pub(super) struct IqHandlerContext<'a> {
    pub(super) iq: &'a xmpp_parsers::iq::Iq,
    pub(super) id: &'a str,
    pub(super) payload_ns: &'a str,
    pub(super) target_to: Option<&'a str>,
    pub(super) has_destroy: bool,
    pub(super) domain: &'a str,
    pub(super) muc_domain: &'a str,
    pub(super) upload_domain: &'a str,
    pub(super) spaces_domain: &'a str,
    pub(super) community_domain: &'a str,
    pub(super) extensions_domain: &'a str,
    pub(super) push_domain: &'a str,
    pub(super) response_from: Option<&'a str>,
    pub(super) response_to: Option<&'a str>,
}

pub async fn handle_iq_with_conn_state(
    iq: xmpp_parsers::iq::Iq,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Vec<String> {
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
        return vec![];
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
            .await;
    }

    if is_isr_token_request(&iq) {
        return handle_isr_token_request_iq(&iq, state, authenticated_session, phase.bound_jid())
            .await;
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
        .await;
    }

    if payload_ns == ROSTER_NS {
        return handle_roster_iq(
            &iq,
            domain,
            state,
            phase.bound_jid(),
            conn_state.roster_interested,
        )
        .await;
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
                return route_full_jid_iq(iq, state, phase.bound_jid(), target, response_from)
                    .await;
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

    let sans_io_response =
        handle_sans_io_iq(handler_ctx, state, authenticated_session, phase, conn_state).await;
    if !sans_io_response.is_empty() {
        return sans_io_response;
    }

    let misc_response = handle_misc_iq(handler_ctx, state, phase, conn_state).await;
    if !misc_response.is_empty() {
        return misc_response;
    }

    let disco_info_response =
        handle_disco_info_iq(handler_ctx, state, phase, authenticated_session).await;
    if !disco_info_response.is_empty() {
        return disco_info_response;
    }

    let disco_items_response = handle_disco_items_iq(handler_ctx, state, phase).await;
    if !disco_items_response.is_empty() {
        return disco_items_response;
    }

    if payload_ns == "http://jabber.org/protocol/commands" {
        return handle_command_iq(
            &iq,
            state,
            domain,
            &extensions_domain,
            &push_domain,
            authenticated_session,
            phase.bound_jid(),
        )
        .await;
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

    if is_pin_query_iq(&iq, muc_domain) {
        return handle_pin_query_iq(&iq, state, phase.bound_jid(), response_from, response_to)
            .await;
    }

    if is_mentions_permissions_iq(&iq, muc_domain) {
        return handle_mentions_permissions_iq(
            &iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    let muc_owner_or_moderation =
        handle_muc_owner_and_moderation_iq(handler_ctx, state, phase, authenticated_session).await;
    if !muc_owner_or_moderation.is_empty() {
        return muc_owner_or_moderation;
    }

    let archive_inbox_upload =
        handle_archive_inbox_upload_iq(&iq, &id, payload_ns.as_str(), domain, state, phase).await;
    if !archive_inbox_upload.is_empty() {
        return archive_inbox_upload;
    }

    let pubsub_response = handle_pubsub_iq(handler_ctx, state, phase, authenticated_session).await;
    if !pubsub_response.is_empty() {
        return pubsub_response;
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
}

#[cfg(test)]
mod tests;
