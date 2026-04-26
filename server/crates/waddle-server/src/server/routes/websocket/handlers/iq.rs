use chrono;
use jid::{BareJid, FullJid, Jid};
use std::sync::Arc;
use tracing::{debug, warn};
use url::{Host, Url};
use waddle_xmpp::{
    carbons::CARBONS_NS,
    commands::{CommandContext, CommandResult},
    disco::{
        build_disco_info_response, build_disco_info_response_with_extensions,
        build_disco_items_response, muc_room_features, parse_disco_info_query,
        parse_disco_items_query, spaces_service_features, upload_service_features, DiscoItem,
        Feature, Identity,
    },
    inbox::runtime::filter_query,
    isr::{build_isr_token_error, build_isr_token_result, is_isr_token_request, IsrToken, ISR_NS},
    mam::{
        add_stanza_id as add_mam_stanza_id, build_fin_iq, build_result_messages, is_mam_query,
        parse_mam_query, ArchivedMessage, ArchivedModeration, ArchivedRichMessage,
        ArchivedRichPayload, RichMessageId, RichText,
    },
    muc::{
        admin::{
            build_admin_result, build_admin_set_result, build_role_result, is_muc_admin_iq,
            is_role_change_query, parse_admin_query,
        },
        owner::build_config_form,
        room_actor::{ApplyAdminItems, GetAdminContext, GetSnapshot, PingSelfCheck, UpdateConfig},
        DATA_FORMS_NS,
    },
    protocol::{
        frame::{parse_frame, InboundFrame},
        ConnectionPhase, StanzaContext as ProtocolStanzaContext,
    },
    pubsub::{
        build_pubsub_error, build_pubsub_items_result, build_pubsub_publish_result,
        build_pubsub_success, is_pubsub_iq, parse_pubsub_iq, PubSubError, PubSubItem,
        PubSubRequest,
    },
    xep::xep0054::{VCard, VCardPhoto},
    xep::xep0357::{
        build_push_disable_result, build_push_enable_result, is_push_disable, is_push_enable,
        parse_push_disable, parse_push_enable,
    },
    xep::xep0363::{
        build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
        parse_upload_request, sanitize_filename, UploadError, UploadSlot,
    },
    xep::xep0430::{
        build_inbox_query_result, build_mark_read_result, is_inbox_iq, parse_inbox_query,
        parse_mark_read,
    },
    xep::{
        build_command_items, build_command_result, build_last_activity_response,
        build_moderation_result_message, build_search_response, build_server_role_form,
        build_spaces_metadata_form, build_spaces_metadata_form_for_requester,
        is_last_activity_query, is_search_request, is_time_query, is_version_query,
        parse_command_from_iq, parse_moderation_iq, parse_search_request, ChannelResult, Command,
        CommandStatus, Searchable, SpaceAffiliation, NODE_COMMANDS, NS_CHANNEL_SEARCH,
    },
    Affiliation, SpaceDetails, Stanza, StanzaErrorCondition, StanzaErrorType, XmppError,
};
use xmpp_parsers::minidom::Element;

use super::super::{
    build_iq_error_xml, build_iq_error_xml_with_addresses, build_iq_result_xml, destroy_room_actor,
    get_room_actor, iq_to_xml, is_muc_room_jid, stanza_to_xml, WebSocketState,
};
use super::presence::get_managed_channel_for_room;
use crate::auth::{NativeUserStore, Session};
use crate::db::actor::{DbExecute, DbQuery, DbQueryOne, GetDatabase};
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::{row_value, Database, Value, ValueExt};
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionError, Relation, Subject,
    SubjectType, Tuple, WriteTuple,
};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::xmpp_state::{get_xmpp_channel, list_xmpp_channels, XmppChannelRecord};
use crate::vcard::VCardStore;

/// Only called from test helpers — suppress dead_code lint for binary crate.
#[allow(dead_code)]
pub async fn handle_iq(
    frame: &str,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let mut carbons_enabled = phase.bound_jid().is_some_and(|jid| {
        state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(jid)
    });

    let iq = match parse_frame(frame) {
        Ok(InboundFrame::Stanza(stanza)) => match *stanza {
            Stanza::Iq(iq) => iq,
            _ => return vec![],
        },
        _ => return vec![],
    };

    handle_iq_with_conn_state(
        iq,
        domain,
        muc_domain,
        state,
        authenticated_session,
        phase,
        &mut carbons_enabled,
    )
    .await
}

pub async fn handle_iq_with_conn_state(
    iq: xmpp_parsers::iq::Iq,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    carbons_enabled: &mut bool,
) -> Vec<String> {
    let spaces_domain = state.deps.service_domains.spaces.clone();
    let upload_domain = state.deps.service_domains.upload.clone();

    let id = iq.id.clone();
    let to = iq.to.as_ref().map(|jid| jid.to_string());
    let from = iq.from.as_ref().map(|jid| jid.to_string());
    let response_from = to.as_deref();
    let response_to = from.as_deref();

    if matches!(
        &iq.payload,
        xmpp_parsers::iq::IqType::Result(_) | xmpp_parsers::iq::IqType::Error(_)
    ) {
        debug!(id = %id, "Ignoring IQ result/error stanza");
        return vec![];
    }

    let payload_ns = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(e) | xmpp_parsers::iq::IqType::Set(e) => e.ns(),
        _ => String::new(),
    };
    let has_destroy = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(e) => e
            .get_child("destroy", "http://jabber.org/protocol/muc#owner")
            .is_some(),
        _ => false,
    };

    if waddle_xmpp::xep::xep0410::is_self_ping(&iq)
        && iq
            .to
            .as_ref()
            .is_some_and(|to| to.domain().as_str() == muc_domain)
    {
        return handle_muc_self_ping_iq(&iq, state, phase.bound_jid(), response_from, response_to)
            .await;
    }

    if is_isr_token_request(&iq) {
        return handle_isr_token_request_iq(&iq, state, authenticated_session, phase.bound_jid())
            .await;
    }

    if let Some(target) = iq
        .to
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    {
        if target.domain().as_str() == domain {
            return route_full_jid_iq(iq, state, phase.bound_jid(), target, response_from).await;
        }
    }

    // Sans-I/O dispatch: if the IQ namespace has a registered handler in
    // the protocol dispatcher, route through it and translate the emitted
    // OutboundEvents into outbound XML frames via `interpret()`.
    //
    // Handlers that still need async I/O (for example MAM, Jingle, disco,
    // and any other namespaces not yet registered with the dispatcher)
    // continue to fall through to the legacy string-matching branches
    // below until the two-phase async callback machinery lands.
    let carbons_toggle = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(e)
            if e.ns() == CARBONS_NS && (e.name() == "enable" || e.name() == "disable") =>
        {
            Some(e.name() == "enable")
        }
        _ => None,
    };
    if state
        .deps
        .protocol
        .dispatcher
        .has_iq_handler(payload_ns.as_str())
    {
        if payload_ns == waddle_xmpp::xep::NS_VERSION && !is_version_query(&iq) {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        }
        if payload_ns == waddle_xmpp::xep::NS_VERSION
            && iq
                .to
                .as_ref()
                .is_some_and(|target| target.to_bare().as_str() != domain)
        {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "cancel",
                "service-unavailable",
            )];
        }
        if payload_ns == waddle_xmpp::xep::NS_TIME {
            if !is_time_query(&iq) {
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "modify",
                    "bad-request",
                )];
            }
            if iq
                .to
                .as_ref()
                .is_some_and(|target| target.to_bare().as_str() != domain)
            {
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "cancel",
                    "service-unavailable",
                )];
            }
        }
        let Some(full_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        };
        if let Some(enabled) = carbons_toggle {
            *carbons_enabled = enabled;
            let _ = state
                .deps
                .protocol
                .connection_registry
                .set_carbons_enabled(full_jid, enabled);
        }
        let ctx = ProtocolStanzaContext { domain, full_jid };
        let events = state.deps.protocol.dispatcher.dispatch_iq(&iq, &ctx);
        let outcome = crate::server::routes::interpret::interpret(
            events,
            &state.deps.protocol.connection_registry,
        )
        .await;
        if outcome.close {
            warn!(
                ns = %payload_ns,
                "Sans-I/O handler requested transport close; \
                 WebSocket adapter cannot honour CloseTransport yet"
            );
        }
        return outcome.frames;
    }

    // jabber:iq:roster is now served by protocol::handlers::roster::RosterHandler
    // through the sans-I/O dispatcher short-circuit above.

    if waddle_xmpp::xep::xep0054::is_vcard_get(&iq) || waddle_xmpp::xep::xep0054::is_vcard_set(&iq)
    {
        return handle_vcard_iq(&iq, state, phase.bound_jid(), response_from, response_to).await;
    }

    if is_last_activity_query(&iq) {
        return handle_last_activity_iq(
            &iq,
            domain,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    if waddle_xmpp::xep::xep0049::is_private_storage_query(&iq) {
        return handle_private_storage_iq(
            &iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    if waddle_xmpp::xep::xep0191::is_blocking_query(&iq) {
        return handle_blocking_iq(&iq, state, phase.bound_jid(), response_from, response_to).await;
    }

    if is_push_enable(&iq) || is_push_disable(&iq) {
        return handle_push_iq(&iq, state, phase.bound_jid(), response_from, response_to).await;
    }

    if is_search_request(&iq) {
        return handle_channel_search_iq(&iq, muc_domain, state, response_from, response_to).await;
    }

    if payload_ns == "jabber:iq:search" {
        return handle_user_search_iq(&iq, domain, state, response_from, response_to).await;
    }

    // Disco info on MUC service
    if payload_ns == "http://jabber.org/protocol/disco#info" {
        let request_iq = &iq;
        let query = match parse_disco_info_query(request_iq) {
            Ok(query) => query,
            Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        };

        if to.as_deref() == Some(muc_domain) {
            let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
            let mut features = vec![
                Feature::muc(),
                Feature::replies(),
                Feature::muc_self_ping_optimization(),
                Feature::new(NS_CHANNEL_SEARCH),
            ];
            features.extend(
                state
                    .deps
                    .protocol
                    .extension_manager
                    .extension_features()
                    .into_iter()
                    .map(|ns| Feature::new(&ns)),
            );
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on a specific room
        if let Some(target) = to.as_deref() {
            let room_target = target.split('/').next().unwrap_or(target);
            if let Ok(room_jid) = room_target.parse::<BareJid>() {
                if let Some(room_actor) = get_room_actor(state, &room_jid).await {
                    let snapshot = match room_actor.ask(GetSnapshot).await {
                        Ok(snapshot) => snapshot.room,
                        Err(error) => {
                            warn!(
                                room = %room_jid,
                                error = ?error,
                                "Failed to load room snapshot for disco#info"
                            );
                            return vec![build_iq_error_xml_with_addresses(
                                &id,
                                response_from,
                                response_to,
                                "wait",
                                "internal-server-error",
                            )];
                        }
                    };
                    let identities = vec![Identity::muc_room(Some(&snapshot.config.name))];
                    let mut features = muc_room_features(
                        snapshot.config.persistent,
                        snapshot.config.members_only,
                        snapshot.config.moderated,
                        snapshot.config.forum,
                    );
                    features.extend(
                        state
                            .deps
                            .protocol
                            .extension_manager
                            .extension_features()
                            .into_iter()
                            .map(|ns| Feature::new(&ns)),
                    );
                    let extensions = room_space_metadata_extensions(state, &room_jid, domain).await;
                    let response = if extensions.is_empty() {
                        build_disco_info_response(request_iq, &identities, &features, None)
                    } else {
                        build_disco_info_response_with_extensions(
                            request_iq,
                            &identities,
                            &features,
                            None,
                            &extensions,
                        )
                    };
                    return vec![iq_to_xml(response)];
                }

                if is_muc_room_jid(state, &room_jid).await {
                    if let Ok(Some(channel)) = get_managed_channel_for_room(state, &room_jid).await
                    {
                        let identities = vec![Identity::muc_room(Some(&channel.name))];
                        let mut features = muc_room_features(
                            true,
                            true,
                            channel.channel_type == "announcement",
                            channel.channel_type == "forum",
                        );
                        features.extend(
                            state
                                .deps
                                .protocol
                                .extension_manager
                                .extension_features()
                                .into_iter()
                                .map(|ns| Feature::new(&ns)),
                        );
                        let extensions =
                            room_space_metadata_extensions(state, &room_jid, domain).await;
                        let response = if extensions.is_empty() {
                            build_disco_info_response(request_iq, &identities, &features, None)
                        } else {
                            build_disco_info_response_with_extensions(
                                request_iq,
                                &identities,
                                &features,
                                None,
                                &extensions,
                            )
                        };
                        return vec![iq_to_xml(response)];
                    }

                    let room_name = room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string());
                    let identities = vec![Identity::muc_room(Some(&room_name))];
                    let mut features = muc_room_features(false, false, false, false);
                    features.extend(
                        state
                            .deps
                            .protocol
                            .extension_manager
                            .extension_features()
                            .into_iter()
                            .map(|ns| Feature::new(&ns)),
                    );
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, None);
                    return vec![iq_to_xml(response)];
                }
            }
        }

        if to.as_deref() == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let identities = vec![Identity::automation(Some("Ad-Hoc Commands"))];
            let features = vec![
                Feature::disco_info(),
                Feature::disco_items(),
                Feature::commands(),
            ];
            let response =
                build_disco_info_response(request_iq, &identities, &features, Some(NODE_COMMANDS));
            return vec![iq_to_xml(response)];
        }

        // Disco info on spaces service
        if to.as_deref() == Some(spaces_domain.as_str()) {
            if let Some(node) = query.node.as_deref() {
                let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
                    return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                };
                let space_node = match state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_node(&spaces_jid, node)
                    .await
                {
                    Ok(Some(node)) => node,
                    Ok(None) => return vec![build_iq_error_xml(&id, "cancel", "item-not-found")],
                    Err(error) => {
                        warn!(node, error = %error, "Failed to resolve Spaces node info");
                        return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
                    }
                };

                let space = space_details_from_node(&space_node);
                let requester_affiliation =
                    space_affiliation_for_requester(state, authenticated_session.as_ref(), node)
                        .await;
                let identities = vec![Identity::pubsub_leaf(Some(&space.name))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::pubsub(),
                    Feature::pubsub_retrieve_items(),
                    Feature::spaces(),
                ];
                let metadata =
                    build_spaces_metadata_form_for_requester(&space, requester_affiliation);
                let response = build_disco_info_response_with_extensions(
                    request_iq,
                    &identities,
                    &features,
                    Some(node),
                    &[metadata],
                );
                return vec![iq_to_xml(response)];
            }

            let identities = vec![Identity::spaces_service(Some("Spaces"))];
            let features = spaces_service_features();
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on upload service (XEP-0363)
        if to.as_deref() == Some(upload_domain.as_str()) {
            let identities = vec![Identity::upload_service(Some("HTTP File Upload"))];
            let features = upload_service_features();
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on server
        let identities = vec![Identity::server(Some("Waddle"))];
        let mut features = vec![
            Feature::ping(),
            Feature::replies(),
            Feature::disco_info(),
            Feature::disco_items(),
            Feature::commands(),
            Feature::spaces(),
            Feature::last_activity(),
            Feature::software_version(),
            Feature::entity_time(),
            Feature::blocking(),
            Feature::new("jabber:iq:search"),
            Feature::new(ISR_NS),
        ];
        features.extend(
            state
                .deps
                .protocol
                .extension_manager
                .extension_features()
                .into_iter()
                .map(|ns| Feature::new(&ns)),
        );
        let response =
            match server_affiliation_for_requester(state, authenticated_session.as_ref()).await {
                Some(role) => build_disco_info_response_with_extensions(
                    request_iq,
                    &identities,
                    &features,
                    None,
                    &[build_server_role_form(role)],
                ),
                None => build_disco_info_response(request_iq, &identities, &features, None),
            };
        return vec![iq_to_xml(response)];
    }

    // Disco items - list services/rooms
    if payload_ns == "http://jabber.org/protocol/disco#items" {
        let request_iq = &iq;
        let query = match parse_disco_items_query(request_iq) {
            Ok(query) => query,
            Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        };

        if to.as_deref() == Some(muc_domain) {
            debug!("Disco items query on MUC service");
            let items = canonical_channel_disco_items(state, muc_domain, 500).await;

            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        if to.as_deref() == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs: Vec<(&str, &str)> = commands
                .iter()
                .map(|(node, name)| (node.as_str(), name.as_str()))
                .collect();
            let response = build_command_items(request_iq, &command_refs, domain);
            return vec![iq_to_xml(response)];
        }

        if to.as_deref() == Some(spaces_domain.as_str()) {
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => vec![],
                None => match spaces_service_bare_jid(&spaces_domain) {
                    Ok(spaces_jid) => match state
                        .deps
                        .protocol
                        .pubsub_storage
                        .list_nodes(&spaces_jid)
                        .await
                    {
                        Ok(nodes) => nodes
                            .into_iter()
                            .map(|node| {
                                let name = if node == "general" { "General" } else { &node };
                                DiscoItem::spaces_node(&spaces_domain, &node, Some(name))
                            })
                            .collect(),
                        Err(error) => {
                            warn!(error = %error, "Failed to list Spaces nodes");
                            vec![]
                        }
                    },
                    Err(error) => {
                        warn!(error = %error, "Invalid Spaces service JID");
                        vec![]
                    }
                },
            };

            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        debug!("Disco items query on server");
        let items = vec![
            DiscoItem::muc_service(muc_domain, Some("Chatrooms")),
            DiscoItem::upload_service(&upload_domain, Some("HTTP File Upload")),
            DiscoItem::spaces_service(&spaces_domain, Some("Spaces")),
        ];
        let response = build_disco_items_response(request_iq, &items, None);
        return vec![iq_to_xml(response)];
    }

    if payload_ns == "http://jabber.org/protocol/commands" {
        return handle_command_iq(&iq, state, authenticated_session, phase.bound_jid()).await;
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

    // MUC owner IQ (XEP-0045): instant room config submit and room destroy.
    // This is needed for clients that create a room by:
    // 1) joining via presence
    // 2) submitting an empty owner form (`jabber:x:data` type='submit')
    if payload_ns == "http://jabber.org/protocol/muc#owner" {
        let Some(target) = to.as_deref() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "modify",
                "jid-malformed",
            )];
        };

        if !is_muc_room_jid(state, &room_jid).await {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "cancel",
                "item-not-found",
            )];
        }

        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        };
        match muc_owner_authorized(state, &room_jid, sender_jid, authenticated_session.as_ref())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "auth",
                    "forbidden",
                )];
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = %error,
                    "Failed to authorize MUC owner IQ"
                );
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        }

        if has_destroy {
            if destroy_room_actor(state, &room_jid).await {
                debug!(room = %room_jid, "Destroyed MUC room via owner IQ");
                let room_jid_string = room_jid.to_string();
                return vec![build_iq_result_xml(
                    &id,
                    Some(room_jid_string.as_str()),
                    response_to,
                    None,
                )];
            }

            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "cancel",
                "item-not-found",
            )];
        }

        if matches!(&iq.payload, xmpp_parsers::iq::IqType::Get(_)) {
            match build_muc_owner_config_response(state, &room_jid, &id, response_to).await {
                Ok(response) => return vec![response],
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = %error,
                        "Failed to build MUC owner config response"
                    );
                    return vec![build_iq_error_xml_with_addresses(
                        &id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
            }
        }

        if let Err(error) =
            apply_muc_owner_config(state, &room_jid, &iq, authenticated_session.as_ref()).await
        {
            warn!(
                room = %room_jid,
                error = %error,
                "Failed to apply MUC owner config"
            );
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )];
        }

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        return vec![build_iq_result_xml(
            &id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
    }

    if let Some(request) = parse_moderation_iq(&iq) {
        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        };
        let Some(room_jid) = iq.to.as_ref().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        };
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "cancel",
                "item-not-found",
            )];
        };
        let context = match room_actor
            .ask(GetAdminContext {
                sender_jid: sender_jid.clone(),
            })
            .await
        {
            Ok(context) => context,
            Err(_) => {
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )]
            }
        };
        let is_moderator = matches!(context.affiliation, Affiliation::Owner | Affiliation::Admin)
            || matches!(context.role, waddle_xmpp::Role::Moderator);
        if !is_moderator {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "forbidden",
            )];
        }
        match state
            .deps
            .protocol
            .mam_storage
            .get_message(&request.target_id)
            .await
        {
            Ok(Some(message)) if message.to == room_jid.to_string() => {}
            Ok(Some(_)) => {
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "cancel",
                    "item-not-found",
                )]
            }
            Ok(None) => {
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "cancel",
                    "item-not-found",
                )]
            }
            Err(error) => {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to look up moderation target");
                return vec![build_iq_error_xml_with_addresses(
                    &id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        }

        let moderator_nick = context
            .nick
            .unwrap_or_else(|| sender_jid.resource().to_string());
        let moderated_by = format!("{room_jid}/{moderator_nick}");
        let stamp_time = chrono::Utc::now();
        let stamp = stamp_time.to_rfc3339();
        let mut moderation = build_moderation_result_message(
            Some(jid::Jid::from(room_jid.clone())),
            &request.target_id,
            &moderated_by,
            &stamp,
            request.reason.as_deref(),
        );
        let archive_id = uuid::Uuid::now_v7().to_string();
        add_mam_stanza_id(&mut moderation, archive_id.as_str(), &room_jid.to_string());

        if let (Some(target_id), Ok(moderator_jid)) = (
            RichMessageId::new(request.target_id.clone()),
            moderated_by.parse::<Jid>(),
        ) {
            let archived = ArchivedMessage {
                id: archive_id,
                timestamp: chrono::Utc::now(),
                from: room_jid.to_string(),
                to: room_jid.to_string(),
                body: String::new(),
                stanza_id: moderation.id.clone(),
                thread_id: None,
                reply_to_id: None,
                reply_to_jid: None,
                origin_id: None,
                message_type: "groupchat".to_string(),
                stanza_xml: None,
                rich: Some(ArchivedRichMessage {
                    payload: Some(ArchivedRichPayload::Moderation(ArchivedModeration {
                        target_id,
                        moderated_by: moderator_jid,
                        stamp: Some(stamp_time),
                        reason: request.reason.as_deref().and_then(RichText::new),
                    })),
                    reply: None,
                    references: Vec::new(),
                    mentions: Vec::new(),
                }),
            };
            if let Err(error) = state
                .deps
                .protocol
                .mam_storage
                .store_message(&room_jid.to_string(), &archived)
                .await
            {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to archive moderation event");
            }
        }

        let mut frames = Vec::new();
        if let Ok(snapshot) = room_actor.ask(GetSnapshot).await {
            for occupant in snapshot.room.occupants.values() {
                for occupant_jid in snapshot.room.get_occupant_sessions(&occupant.nick) {
                    let mut outbound = moderation.clone();
                    outbound.to = Some(jid::Jid::from(occupant_jid.clone()));
                    if occupant_jid == *sender_jid {
                        frames.push(stanza_to_xml(&Stanza::Message(outbound)));
                        continue;
                    }
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&occupant_jid, Stanza::Message(outbound));
                }
            }
        }

        frames.push(build_iq_result_xml(&id, response_from, response_to, None));
        return frames;
    }

    // MAM (Message Archive Management) query
    if is_mam_query(&iq) {
        let request_iq = &iq;
        let Some(target) = request_iq.to.as_ref().map(|jid| jid.to_string()) else {
            return vec![build_iq_error_xml(&id, "modify", "bad-request")];
        };

        let room_target = target.split('/').next().unwrap_or(target.as_str());
        let Ok(target_bare) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml(&id, "modify", "jid-malformed")];
        };

        // Determine whether this is a personal archive query (to=self) or a
        // MUC room archive query. Personal queries are allowed only when the
        // bound session identity matches the requested bare JID.
        let sender_bare = phase.bound_jid().map(|jid| jid.to_bare());

        let is_personal = sender_bare
            .as_ref()
            .is_some_and(|bare| *bare == target_bare);

        if !is_personal && !is_muc_room_jid(state, &target_bare).await {
            return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
        }

        let (query_id, query) = match parse_mam_query(request_iq) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(error = %err, target = %target_bare, "Invalid MAM query");
                return vec![build_iq_error_xml(&id, "modify", "bad-request")];
            }
        };

        let archive_jid = target_bare.to_string();
        let mut result = match state
            .deps
            .protocol
            .mam_storage
            .query_messages(archive_jid.as_str(), &query)
            .await
        {
            Ok(result) => result,
            Err(err) => {
                warn!(error = %err, target = %target_bare, "MAM query failed");
                return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
            }
        };

        result.count = state
            .deps
            .protocol
            .mam_storage
            .count_messages(archive_jid.as_str())
            .await
            .ok();

        let recipient_jid = request_iq
            .from
            .as_ref()
            .map(|jid| jid.to_string())
            .or_else(|| phase.bound_jid().map(ToString::to_string))
            .unwrap_or_else(|| "unknown@localhost".to_string());

        let mut responses: Vec<String> =
            build_result_messages(&query_id, recipient_jid.as_str(), &result.messages)
                .into_iter()
                .map(|message| stanza_to_xml(&Stanza::Message(message)))
                .collect();
        responses.push(iq_to_xml(build_fin_iq(request_iq, &result)));
        return responses;
    }

    if is_inbox_iq(&iq) {
        let request_iq = &iq;
        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml(&id, "auth", "not-authorized")];
        };

        match &request_iq.payload {
            xmpp_parsers::iq::IqType::Get(_) => {
                let query = match parse_inbox_query(request_iq) {
                    Ok(query) => query,
                    Err(error) => {
                        warn!(error = %error, "Invalid inbox query");
                        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                    }
                };
                let entries = if query.threads {
                    if let Some(room) = &query.room {
                        match state
                            .deps
                            .protocol
                            .inbox_storage
                            .list_threads(&user_jid, room)
                            .await
                        {
                            Ok(entries) => entries,
                            Err(error) => {
                                warn!(error = %error, jid = %user_jid, "Failed to list thread inbox");
                                return vec![build_iq_error_xml(
                                    &id,
                                    "wait",
                                    "internal-server-error",
                                )];
                            }
                        }
                    } else {
                        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                    }
                } else {
                    match state.deps.protocol.inbox_storage.list(&user_jid).await {
                        Ok(entries) => entries,
                        Err(error) => {
                            warn!(error = %error, jid = %user_jid, "Failed to list inbox");
                            return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                        }
                    }
                };
                let total_unread = match state
                    .deps
                    .protocol
                    .inbox_storage
                    .total_unread(&user_jid)
                    .await
                {
                    Ok(total_unread) => total_unread,
                    Err(error) => {
                        warn!(error = %error, jid = %user_jid, "Failed to count inbox unread");
                        return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                    }
                };
                let response = build_inbox_query_result(
                    request_iq,
                    &filter_query(entries, &query),
                    total_unread,
                );
                return vec![iq_to_xml(response)];
            }
            xmpp_parsers::iq::IqType::Set(_) => {
                let mark_read = match parse_mark_read(request_iq) {
                    Ok(mark_read) => mark_read,
                    Err(error) => {
                        warn!(error = %error, "Invalid inbox mark-read");
                        return vec![build_iq_error_xml(&id, "modify", "bad-request")];
                    }
                };
                if let Err(error) = state
                    .deps
                    .protocol
                    .inbox_storage
                    .mark_read(
                        &user_jid,
                        &mark_read.partner,
                        mark_read.thread_id.as_deref(),
                    )
                    .await
                {
                    warn!(error = %error, jid = %user_jid, partner = %mark_read.partner, "Failed to mark inbox read");
                    return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                }
                return vec![iq_to_xml(build_mark_read_result(request_iq))];
            }
            _ => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        }
    }

    // urn:xmpp:carbons:2 enable/disable is now served by
    // protocol::handlers::carbons::CarbonsHandler via the short-circuit above.

    // XEP-0363: HTTP File Upload slot request
    if payload_ns == "urn:xmpp:http:upload:0" {
        let request_iq = &iq;
        if is_upload_request(request_iq) {
            let Some(sender_jid) = phase.bound_jid() else {
                return vec![build_iq_error_xml(&id, "auth", "not-authorized")];
            };
            let request = match parse_upload_request(request_iq) {
                Ok(req) => req,
                Err(e) => {
                    return vec![build_upload_error(&id, &e)];
                }
            };

            // Check file size limits (default 10 MB)
            let max_size: u64 = std::env::var("WADDLE_MAX_UPLOAD_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10 * 1024 * 1024);

            if request.size > max_size {
                return vec![build_upload_error(
                    &id,
                    &UploadError::FileTooLarge { max_size },
                )];
            }

            let safe_filename = sanitize_filename(&request.filename);
            let content_type = effective_content_type(request.content_type.as_deref()).to_string();
            let slot_id = uuid::Uuid::new_v4().to_string();
            let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(15)).to_rfc3339();

            let base_url =
                std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| format!("https://{}", domain));
            let base_url = base_url.trim_end_matches('/');
            let put_url = format!("{}/api/upload/{}", base_url, slot_id);
            let get_url = format!("{}/api/files/{}/{}", base_url, slot_id, safe_filename);

            if let Err(e) = state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .clone()
                .ask(DbExecute {
                    sql: "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)".to_string(),
                    params: vec![
                        slot_id.clone().into(),
                        sender_jid.to_bare().to_string().into(),
                        safe_filename.clone().into(),
                        (request.size as i64).into(),
                        content_type.clone().into(),
                        expires_at.into(),
                    ],
                })
                .await
            {
                warn!(error = %e, "Failed to create upload slot in database");
                return vec![build_upload_error(
                    &id,
                    &UploadError::InternalError(format!("Database error: {}", e)),
                )];
            }

            debug!(
                slot_id = %slot_id,
                put_url = %put_url,
                get_url = %get_url,
                "Created upload slot via WebSocket"
            );

            let slot = UploadSlot {
                put_url,
                put_headers: vec![("Content-Type".to_string(), content_type)],
                get_url,
            };
            let response = build_upload_slot_response(request_iq, &slot);
            return vec![iq_to_xml(response)];
        }
    }

    // PubSub / PEP (XEP-0060, XEP-0163)
    if is_pubsub_iq(&iq) {
        if !phase.is_ready() {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        }

        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_with_addresses(
                &id,
                response_from,
                response_to,
                "auth",
                "not-authorized",
            )];
        };

        let target_jid = match &iq.to {
            Some(to_jid) => to_jid.to_bare(),
            None => user_jid.clone(),
        };

        let request = match parse_pubsub_iq(&iq) {
            Ok(req) => req,
            Err(e) => {
                warn!("Failed to parse PubSub request: {}", e);
                let error = build_pubsub_error(&iq, PubSubError::InvalidJid);
                return vec![iq_to_xml(error)];
            }
        };

        debug!(?request, "Handling PubSub request via WebSocket");

        match request {
            PubSubRequest::Publish { node, item } => {
                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_publish(
                        &iq,
                        state,
                        muc_domain,
                        &spaces_domain,
                        &node,
                        item,
                        authenticated_session.as_ref(),
                    )
                    .await;
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .publish_item(&target_jid, &node, &item, Some(&user_jid), true)
                    .await;

                match result {
                    Ok(publish_result) => {
                        debug!(
                            node = %node,
                            item_id = %publish_result.item_id,
                            created = publish_result.node_created,
                            "PubSub item published via WebSocket"
                        );
                        let response =
                            build_pubsub_publish_result(&iq, &node, &publish_result.item_id);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub publish failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Items {
                node,
                max_items,
                item_ids,
            } => {
                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_items(
                        &iq,
                        state,
                        &spaces_domain,
                        &node,
                        max_items,
                        &item_ids,
                    )
                    .await;
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_items(&target_jid, &node, max_items, &item_ids)
                    .await;

                match result {
                    Ok(stored_items) => {
                        let items: Vec<_> =
                            stored_items.iter().map(|si| si.to_pubsub_item()).collect();
                        debug!(
                            node = %node,
                            count = items.len(),
                            "PubSub items retrieved via WebSocket"
                        );
                        let response = build_pubsub_items_result(&iq, &node, &items);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub items retrieval failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Retract {
                node,
                item_id,
                notify: _,
            } => {
                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_retract(
                        &iq,
                        state,
                        muc_domain,
                        &spaces_domain,
                        &node,
                        &item_id,
                        authenticated_session.as_ref(),
                    )
                    .await;
                }

                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(&target_jid, &node, &item_id)
                    .await;

                match result {
                    Ok(retracted) => {
                        if retracted {
                            debug!(node = %node, item_id = %item_id, "PubSub item retracted via WebSocket");
                            let response = build_pubsub_success(&iq);
                            return vec![iq_to_xml(response)];
                        } else {
                            let error = build_pubsub_error(&iq, PubSubError::ItemNotFound);
                            return vec![iq_to_xml(error)];
                        }
                    }
                    Err(e) => {
                        warn!("PubSub retract failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::CreateNode { node } => {
                if target_jid.to_string() == spaces_domain {
                    if server_permission_allowed(
                        state,
                        authenticated_session.as_ref(),
                        Permission::CreateSpace,
                    )
                    .await
                    .unwrap_or(false)
                    {
                        let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
                            return vec![iq_to_xml(build_pubsub_error(
                                &iq,
                                PubSubError::InvalidJid,
                            ))];
                        };
                        match state
                            .deps
                            .protocol
                            .pubsub_storage
                            .get_or_create_node(&spaces_jid, &node)
                            .await
                        {
                            Ok((_, true)) => {
                                if let Err(error) = state
                                    .deps
                                    .protocol
                                    .pubsub_storage
                                    .update_node_config(
                                        &spaces_jid,
                                        &node,
                                        &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
                                    )
                                    .await
                                {
                                    warn!(node = %node, error = %error, "Failed to configure Spaces node");
                                    return vec![iq_to_xml(build_pubsub_error(
                                        &iq,
                                        PubSubError::Forbidden,
                                    ))];
                                }
                                if let Err(error) = write_space_owner_tuple(
                                    state,
                                    &node,
                                    authenticated_session.as_ref(),
                                )
                                .await
                                {
                                    warn!(node = %node, error = %error, "Failed to persist Space owner tuple");
                                    return vec![iq_to_xml(build_pubsub_error(
                                        &iq,
                                        PubSubError::Forbidden,
                                    ))];
                                }
                                let response = build_pubsub_success(&iq);
                                return vec![iq_to_xml(response)];
                            }
                            Ok((_, false)) => {
                                let error = build_pubsub_error(&iq, PubSubError::NodeExists);
                                return vec![iq_to_xml(error)];
                            }
                            Err(error) => {
                                warn!(node = %node, error = %error, "Failed to create Spaces node");
                                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                                return vec![iq_to_xml(error)];
                            }
                        }
                    } else {
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }

                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_or_create_node(&target_jid, &node)
                    .await;

                match result {
                    Ok((_, created)) => {
                        if created {
                            debug!(node = %node, "PubSub node created via WebSocket");
                        } else {
                            debug!(node = %node, "PubSub node already exists");
                        }
                        let response = build_pubsub_success(&iq);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub node creation failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::ConfigureNode { node } => {
                if target_jid.to_string() == spaces_domain {
                    if spaces_node_mutation_allowed(state, authenticated_session.as_ref(), &node)
                        .await
                        .unwrap_or(false)
                    {
                        let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
                            return vec![iq_to_xml(build_pubsub_error(
                                &iq,
                                PubSubError::InvalidJid,
                            ))];
                        };
                        match state
                            .deps
                            .protocol
                            .pubsub_storage
                            .get_node(&spaces_jid, &node)
                            .await
                        {
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                return vec![iq_to_xml(build_pubsub_error(
                                    &iq,
                                    PubSubError::NodeNotFound,
                                ))];
                            }
                            Err(error) => {
                                warn!(node = %node, error = %error, "Failed to configure Spaces node");
                                return vec![iq_to_xml(build_pubsub_error(
                                    &iq,
                                    PubSubError::NodeNotFound,
                                ))];
                            }
                        }
                        let response = build_pubsub_success(&iq);
                        return vec![iq_to_xml(response)];
                    }
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            PubSubRequest::DeleteNode { node } => {
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .delete_node(&target_jid, &node)
                    .await;

                match result {
                    Ok(deleted) => {
                        if deleted {
                            debug!(node = %node, "PubSub node deleted via WebSocket");
                            let response = build_pubsub_success(&iq);
                            return vec![iq_to_xml(response)];
                        } else {
                            let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                            return vec![iq_to_xml(error)];
                        }
                    }
                    Err(e) => {
                        warn!("PubSub node deletion failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Subscribe { node, jid } => {
                let subscription_jid = jid.to_bare();
                if subscription_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
                match state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_node(&target_jid, &node)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                    Err(e) => {
                        warn!("PubSub subscribe node lookup failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
                state.deps.protocol.pubsub_subscriptions.insert((
                    target_jid.to_string(),
                    node,
                    subscription_jid.to_string(),
                ));
                let response = build_pubsub_success(&iq);
                return vec![iq_to_xml(response)];
            }

            PubSubRequest::Unsubscribe { node, jid, .. } => {
                let subscription_jid = jid.to_bare();
                if subscription_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
                let removed = state.deps.protocol.pubsub_subscriptions.remove(&(
                    target_jid.to_string(),
                    node,
                    subscription_jid.to_string(),
                ));
                if removed.is_none() {
                    let error = build_pubsub_error(&iq, PubSubError::NotSubscribed);
                    return vec![iq_to_xml(error)];
                }
                let response = build_pubsub_success(&iq);
                return vec![iq_to_xml(response)];
            }
        }
    }

    // Unknown IQ - log a compact summary and return an error.
    let payload_ns = (!payload_ns.is_empty()).then_some(payload_ns.as_str());
    warn!(id = %id, payload_ns, "Unhandled IQ stanza");
    vec![build_iq_error_xml_with_addresses(
        &id,
        response_from,
        response_to,
        "cancel",
        "feature-not-implemented",
    )]
}

async fn handle_muc_self_ping_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };
    let Some(target) = iq
        .to
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )];
    };
    let room_jid = target.to_bare();
    let nick = target.resource().to_string();
    let Some(room_actor) = get_room_actor(state, &room_jid).await else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "cancel",
            "item-not-found",
        )];
    };
    match room_actor
        .ask(PingSelfCheck {
            nick,
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(()) => vec![build_iq_result_xml(
            &iq.id,
            response_from,
            response_to,
            None,
        )],
        Err(kameo::error::SendError::HandlerError(_)) => vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "cancel",
            "not-acceptable",
        )],
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to process MUC self-ping");
            vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )]
        }
    }
}

async fn handle_isr_token_request_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    sender_jid: Option<&FullJid>,
) -> Vec<String> {
    let (Some(session), Some(sender_jid)) = (authenticated_session.as_ref(), sender_jid) else {
        return vec![iq_to_xml(build_isr_token_error(iq, "not-authorized"))];
    };
    let token: IsrToken = state
        .deps
        .protocol
        .isr_token_store
        .create_token(session.user_id.to_string(), sender_jid.to_bare());
    vec![iq_to_xml(build_isr_token_result(iq, &token))]
}

async fn route_full_jid_iq(
    mut iq: xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    target: FullJid,
    response_from: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            None,
            "auth",
            "not-authorized",
        )];
    };
    let blocking = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    match blocking
        .is_blocked(&target.to_bare(), &sender_jid.to_bare())
        .await
    {
        Ok(true) => {
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                Some(sender_jid.as_str()),
                "cancel",
                "service-unavailable",
            )];
        }
        Ok(false) => {}
        Err(error) => {
            warn!(error = %error, target = %target, sender = %sender_jid, "Failed to check blocklist before routing IQ");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                Some(sender_jid.as_str()),
                "wait",
                "internal-server-error",
            )];
        }
    }
    iq.from = Some(Jid::from(sender_jid.clone()));
    iq.to = Some(Jid::from(target.clone()));
    match state
        .deps
        .protocol
        .connection_registry
        .send_to(&target, Stanza::Iq(iq.clone()))
        .await
    {
        waddle_xmpp::registry::SendResult::Sent => Vec::new(),
        waddle_xmpp::registry::SendResult::NotConnected
        | waddle_xmpp::registry::SendResult::ChannelClosed => {
            let sender = sender_jid.to_string();
            vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                Some(sender.as_str()),
                "cancel",
                "service-unavailable",
            )]
        }
    }
}

async fn handle_channel_search_iq(
    iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
    state: &WebSocketState,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    if iq
        .to
        .as_ref()
        .is_some_and(|to| to.to_string() != muc_domain)
    {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "cancel",
            "item-not-found",
        )];
    }
    let Some(request) = parse_search_request(iq) else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )];
    };
    let limit = request.max.unwrap_or(50).clamp(1, 200) as usize;
    let channels =
        match list_xmpp_channels(state.deps.app_state.db_pool.global_actor().clone(), 500, 0).await
        {
            Ok(channels) => channels,
            Err(error) => {
                warn!(error = %error, "Failed to load channels for WebSocket search");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        };
    let mut results = Vec::new();
    for channel in channels {
        let Some(room_jid) = waddle_xmpp::managed_room_jid(&channel.id, muc_domain).ok() else {
            continue;
        };
        let result = ChannelResult::new(room_jid.to_string())
            .with_name(channel.name)
            .with_description(channel.description.unwrap_or_default());
        if result.matches_query(&request.query) {
            results.push(result);
            if results.len() >= limit {
                break;
            }
        }
    }
    vec![iq_to_xml(build_search_response(iq, &results))]
}

async fn handle_user_search_iq(
    iq: &xmpp_parsers::iq::Iq,
    domain: &str,
    state: &WebSocketState,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(_) => {
            let payload = Element::builder("query", "jabber:iq:search")
                .append(
                    Element::builder("instructions", "jabber:iq:search")
                        .append("Search users by username or email")
                        .build(),
                )
                .append(Element::builder("nick", "jabber:iq:search").build())
                .append(Element::builder("email", "jabber:iq:search").build())
                .build();
            vec![build_iq_result_xml(
                &iq.id,
                response_from,
                response_to,
                Some(payload),
            )]
        }
        xmpp_parsers::iq::IqType::Set(query) => {
            let term = query
                .children()
                .find(|child| matches!(child.name(), "nick" | "email" | "first" | "last"))
                .map(|child| child.text())
                .unwrap_or_default();
            let like = format!("%{}%", term.trim());
            let rows = match state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .clone()
                .ask(DbQuery {
                    sql: "SELECT username, email FROM native_users WHERE domain = ? AND (username LIKE ? OR COALESCE(email, '') LIKE ?) ORDER BY username LIMIT 50".to_string(),
                    params: vec![domain.into(), like.clone().into(), like.into()],
                })
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(error = %error, "Failed to search native users over WebSocket");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
            };
            let mut query = Element::builder("query", "jabber:iq:search");
            for row in rows {
                let username = row_value(&row, 0)
                    .and_then(ValueExt::as_string)
                    .unwrap_or_default();
                if username.is_empty() {
                    continue;
                }
                let email = row_value(&row, 1)
                    .and_then(ValueExt::as_optional_string)
                    .ok()
                    .flatten();
                let mut item = Element::builder("item", "jabber:iq:search")
                    .attr("jid", format!("{username}@{domain}"))
                    .append(
                        Element::builder("nick", "jabber:iq:search")
                            .append(username.clone())
                            .build(),
                    );
                if let Some(email) = email {
                    item = item.append(
                        Element::builder("email", "jabber:iq:search")
                            .append(email)
                            .build(),
                    );
                }
                query = query.append(item.build());
            }
            vec![build_iq_result_xml(
                &iq.id,
                response_from,
                response_to,
                Some(query.build()),
            )]
        }
        _ => vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )],
    }
}

async fn handle_muc_admin_iq(
    iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };
    let mut iq_with_from = iq.clone();
    iq_with_from.from = Some(Jid::from(sender_jid.clone()));
    let query = match parse_admin_query(&iq_with_from, muc_domain) {
        Ok(query) => query,
        Err(error) => return vec![build_xmpp_error_response(&iq_with_from, error)],
    };
    let Some(room_actor) = get_room_actor(state, &query.room_jid).await else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "cancel",
            "item-not-found",
        )];
    };
    let context = match room_actor
        .ask(GetAdminContext {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(context) => context,
        Err(_) => {
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )]
        }
    };
    let is_admin = matches!(context.affiliation, Affiliation::Owner | Affiliation::Admin)
        || matches!(context.role, waddle_xmpp::Role::Moderator);
    if !is_admin {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "forbidden",
        )];
    }
    if query.is_get {
        let snapshot = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => snapshot.room,
            Err(_) => {
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )]
            }
        };
        let to_jid = Jid::from(sender_jid.clone());
        if is_role_change_query(&query.items) {
            let role_filter = query.items.iter().find_map(|item| item.role);
            let items: Vec<(String, waddle_xmpp::Role, Option<BareJid>)> = snapshot
                .occupants
                .values()
                .filter(|occupant| role_filter.is_none_or(|role| occupant.role == role))
                .map(|occupant| {
                    (
                        occupant.nick.clone(),
                        occupant.role,
                        Some(occupant.real_jid.to_bare()),
                    )
                })
                .collect();
            return vec![iq_to_xml(build_role_result(
                &iq.id,
                &query.room_jid,
                &to_jid,
                &items,
            ))];
        }
        let affiliation_filter = query.items.iter().find_map(|item| item.affiliation);
        let items: Vec<(BareJid, Affiliation)> = if let Some(affiliation) = affiliation_filter {
            snapshot
                .get_jids_by_affiliation(affiliation)
                .into_iter()
                .map(|jid| (jid, affiliation))
                .collect()
        } else {
            snapshot
                .get_all_affiliations()
                .into_iter()
                .map(|entry| (entry.jid, entry.affiliation))
                .collect()
        };
        return vec![iq_to_xml(build_admin_result(
            &iq.id,
            &query.room_jid,
            &to_jid,
            &items,
        ))];
    }
    let room_jid = query.room_jid.clone();
    let updates = match room_actor
        .ask(ApplyAdminItems {
            sender_jid: sender_jid.clone(),
            sender_affiliation: context.affiliation,
            sender_role: context.role,
            items: query.items,
        })
        .await
    {
        Ok(updates) => updates,
        Err(kameo::error::SendError::HandlerError(error)) => {
            warn!(room = %room_jid, error = %error, "MUC admin set rejected");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "auth",
                "forbidden",
            )];
        }
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to apply MUC admin IQ");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )];
        }
    };
    for (recipient, presence) in updates {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&recipient, Stanza::Presence(presence))
            .await;
    }
    vec![iq_to_xml(build_admin_set_result(
        &iq.id,
        &room_jid,
        &Jid::from(sender_jid.clone()),
    ))]
}

async fn handle_last_activity_iq(
    iq: &xmpp_parsers::iq::Iq,
    domain: &str,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };

    let Some(target) = &iq.to else {
        let response = build_last_activity_response(
            iq,
            state
                .deps
                .protocol
                .connection_registry
                .server_uptime_seconds(),
            None,
        );
        return vec![iq_to_xml(response)];
    };

    if target.node().is_none() && target.domain().as_str() == domain {
        let response = build_last_activity_response(
            iq,
            state
                .deps
                .protocol
                .connection_registry
                .server_uptime_seconds(),
            None,
        );
        return vec![iq_to_xml(response)];
    }

    if target.node().is_some() && target.resource().is_none() && target.domain().as_str() == domain
    {
        let target_bare = target.to_bare();
        let global_db = match state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone()
            .ask(GetDatabase)
            .await
        {
            Ok(db) => db,
            Err(error) => {
                warn!(error = %error, "Failed to access database for last-activity block check");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        };
        let blocking_storage = DatabaseBlockingStorage::new(global_db);
        match blocking_storage
            .is_blocked(&target_bare, &sender_jid.to_bare())
            .await
        {
            Ok(true) => {
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "cancel",
                    "service-unavailable",
                )];
            }
            Ok(false) => {}
            Err(error) => {
                warn!(error = %error, target = %target_bare, "Failed to check last-activity block state");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        }

        if !state
            .deps
            .protocol
            .connection_registry
            .get_available_resources_for_user(&target_bare)
            .is_empty()
        {
            return vec![iq_to_xml(build_last_activity_response(iq, 0, None))];
        }

        if let Some(last_activity) = state
            .deps
            .protocol
            .connection_registry
            .get_last_activity(&target_bare)
        {
            let seconds = chrono::Utc::now()
                .signed_duration_since(last_activity.timestamp)
                .num_seconds()
                .max(0) as u64;
            let response =
                build_last_activity_response(iq, seconds, last_activity.status.as_deref());
            return vec![iq_to_xml(response)];
        }

        let Some(node) = target_bare.node() else {
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "cancel",
                "service-unavailable",
            )];
        };
        let native_user_store =
            NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());
        match native_user_store.user_exists(node.as_str(), domain).await {
            Ok(false) => {
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "cancel",
                    "service-unavailable",
                )];
            }
            Ok(true) => {
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "auth",
                    "forbidden",
                )];
            }
            Err(error) => {
                warn!(error = %error, target = %target_bare, "Failed to check local user for last-activity query");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        }
    }

    vec![build_iq_error_xml_with_addresses(
        &iq.id,
        response_from,
        response_to,
        "cancel",
        "service-unavailable",
    )]
}

fn build_xmpp_error_response(request_iq: &xmpp_parsers::iq::Iq, err: XmppError) -> String {
    match err {
        XmppError::Stanza {
            condition,
            error_type,
            text,
        } => waddle_xmpp::generate_iq_error(
            &request_iq.id,
            request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .as_deref(),
            request_iq.to.as_ref().map(|jid| jid.to_string()).as_deref(),
            condition,
            error_type,
            text.as_deref(),
        ),
        other => waddle_xmpp::generate_iq_error(
            &request_iq.id,
            request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .as_deref(),
            request_iq.to.as_ref().map(|jid| jid.to_string()).as_deref(),
            StanzaErrorCondition::InternalServerError,
            StanzaErrorType::Wait,
            Some(&other.to_string()),
        ),
    }
}

async fn global_database(state: &WebSocketState) -> Result<Database, String> {
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
        .map_err(|error| error.to_string())
}

async fn handle_vcard_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };

    let db = match global_database(state).await {
        Ok(db) => Arc::new(db),
        Err(error) => {
            warn!(error = %error, "Failed to access database for vCard IQ");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )];
        }
    };
    let store = VCardStore::new(Arc::clone(&db));

    if waddle_xmpp::xep::xep0054::is_vcard_get(iq) {
        let target_jid = iq
            .to
            .as_ref()
            .map(|jid| jid.to_bare())
            .unwrap_or_else(|| sender_jid.to_bare());
        let response = match store.get(&target_jid).await {
            Ok(Some(xml)) => match xml.parse::<Element>() {
                Ok(vcard) => xmpp_parsers::iq::Iq {
                    from: iq.to.clone(),
                    to: iq.from.clone(),
                    id: iq.id.clone(),
                    payload: xmpp_parsers::iq::IqType::Result(Some(vcard)),
                },
                Err(error) => {
                    warn!(target = %target_jid, error = %error, "Stored vCard XML is invalid");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
            },
            Ok(None) => match avatar_vcard_from_user_profile(Arc::clone(&db), &target_jid).await {
                Ok(Some(vcard)) => waddle_xmpp::xep::xep0054::build_vcard_response(iq, &vcard),
                Ok(None) => {
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "cancel",
                        "item-not-found",
                    )];
                }
                Err(error) => {
                    warn!(target = %target_jid, error = %error, "Failed to load avatar vCard fallback");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
            },
            Err(error) => {
                warn!(target = %target_jid, error = %error, "Failed to load vCard");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        };
        return vec![iq_to_xml(response)];
    }

    if let Some(to) = &iq.to {
        if to.to_bare() != sender_jid.to_bare() {
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "auth",
                "forbidden",
            )];
        }
    }

    let xmpp_parsers::iq::IqType::Set(vcard) = &iq.payload else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )];
    };

    if let Err(error) = store.set(&sender_jid.to_bare(), &String::from(vcard)).await {
        warn!(jid = %sender_jid, error = %error, "Failed to store vCard");
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "wait",
            "internal-server-error",
        )];
    }

    vec![iq_to_xml(waddle_xmpp::xep::xep0054::build_vcard_success(
        iq,
    ))]
}

async fn handle_private_storage_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };
    let bare_jid = sender_jid.to_bare();

    if let Some(key) = waddle_xmpp::xep::xep0049::parse_private_storage_get(iq) {
        if waddle_xmpp::xep::xep0048::is_legacy_bookmarks_namespace(&key.namespace) {
            let stored_items = state
                .deps
                .protocol
                .pubsub_storage
                .get_items(&bare_jid, waddle_xmpp::xep::xep0402::PEP_NODE, None, &[])
                .await
                .unwrap_or_default();
            let native_bookmarks: Vec<waddle_xmpp::xep::xep0402::Bookmark> = stored_items
                .iter()
                .filter_map(|item| {
                    let xml = item.payload_xml.as_ref()?;
                    let elem: Element = xml.parse().ok()?;
                    waddle_xmpp::xep::xep0402::parse_bookmark(&item.id, &elem).ok()
                })
                .collect();
            let legacy: Vec<waddle_xmpp::xep::xep0048::LegacyBookmark> = native_bookmarks
                .iter()
                .map(waddle_xmpp::xep::xep0048::from_native_bookmark)
                .collect();
            let bookmarks = waddle_xmpp::xep::xep0048::build_legacy_bookmarks_element(&legacy);
            let response = waddle_xmpp::xep::xep0049::build_private_storage_result(
                iq,
                Some(&String::from(&bookmarks)),
                &key,
            );
            return vec![iq_to_xml(response)];
        }

        let row = match state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone()
            .ask(DbQueryOne {
                sql: "SELECT xml_content FROM private_xml_storage WHERE jid = ? AND namespace = ?"
                    .to_string(),
                params: vec![
                    Value::from(bare_jid.to_string()),
                    Value::from(key.namespace.clone()),
                ],
            })
            .await
        {
            Ok(row) => row,
            Err(error) => {
                warn!(jid = %bare_jid, namespace = %key.namespace, error = %error, "Failed to load private XML");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        };
        let stored = match row {
            Some(values) => match row_value(&values, 0).and_then(|value| value.as_string()) {
                Ok(value) => Some(value),
                Err(error) => {
                    warn!(jid = %bare_jid, namespace = %key.namespace, error = %error, "Failed to decode private XML row");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
            },
            None => None,
        };
        let response =
            waddle_xmpp::xep::xep0049::build_private_storage_result(iq, stored.as_deref(), &key);
        return vec![iq_to_xml(response)];
    }

    if let Some((key, xml_content)) = waddle_xmpp::xep::xep0049::parse_private_storage_set(iq) {
        if waddle_xmpp::xep::xep0048::is_legacy_bookmarks_namespace(&key.namespace) {
            if let Ok(elem) = xml_content.parse::<Element>() {
                for legacy in waddle_xmpp::xep::xep0048::parse_legacy_bookmarks(&elem) {
                    if let Some(native) = waddle_xmpp::xep::xep0048::to_native_bookmark(&legacy) {
                        let bookmark_elem =
                            waddle_xmpp::xep::xep0402::build_bookmark_element(&native);
                        let item =
                            PubSubItem::new(Some(native.jid.to_string()), Some(bookmark_elem));
                        let _ = state
                            .deps
                            .protocol
                            .pubsub_storage
                            .publish_item(
                                &bare_jid,
                                waddle_xmpp::xep::xep0402::PEP_NODE,
                                &item,
                                Some(&bare_jid),
                                true,
                            )
                            .await;
                    }
                }
            }
            return vec![iq_to_xml(
                waddle_xmpp::xep::xep0049::build_private_storage_success(iq),
            )];
        }

        if let Err(error) = state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone()
            .ask(DbExecute {
                sql: "INSERT OR REPLACE INTO private_xml_storage (jid, namespace, xml_content, updated_at) VALUES (?, ?, ?, datetime('now'))".to_string(),
                params: vec![
                    Value::from(bare_jid.to_string()),
                    Value::from(key.namespace),
                    Value::from(xml_content),
                ],
            })
            .await
        {
            warn!(jid = %bare_jid, error = %error, "Failed to store private XML");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )];
        }
        return vec![iq_to_xml(
            waddle_xmpp::xep::xep0049::build_private_storage_success(iq),
        )];
    }

    vec![build_iq_error_xml_with_addresses(
        &iq.id,
        response_from,
        response_to,
        "modify",
        "bad-request",
    )]
}

async fn avatar_vcard_from_user_profile(
    db: Arc<Database>,
    target_jid: &BareJid,
) -> Result<Option<VCard>, String> {
    let Some(localpart) = target_jid.node().map(|node| node.to_string()) else {
        return Ok(None);
    };

    let conn = db.guard().await.map_err(|error| error.to_string())?;
    let mut rows = conn
        .query(
            "SELECT avatar_url FROM users WHERE xmpp_localpart = ? LIMIT 1",
            [localpart.as_str()],
        )
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let avatar_url: Option<String> = row.get(0).map_err(|error| error.to_string())?;
    let Some(avatar_url) = avatar_url.filter(|url| url.starts_with("https://")) else {
        return Ok(None);
    };

    let vcard = VCard {
        photo: Some(VCardPhoto::External { url: avatar_url }),
        ..Default::default()
    };

    Ok(Some(vcard))
}

#[cfg(test)]
mod vcard_fallback_tests {
    use super::*;
    use crate::db::MigrationRunner;
    use waddle_xmpp::xep::xep0054::{VCardPhoto, NS_VCARD};

    async fn test_db(name: &str) -> Arc<Database> {
        let db = Arc::new(Database::in_memory(name).await.expect("database"));
        MigrationRunner::global()
            .run(&db)
            .await
            .expect("migrations");
        db
    }

    #[tokio::test]
    async fn vcard_get_fallback_returns_photo_extval_without_stored_vcard() {
        let db = test_db("vcard-profile-fallback").await;
        let conn = db.guard().await.expect("connection");
        conn.execute(
            "INSERT INTO users (id, username, xmpp_localpart, avatar_url, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                Value::from("user-rawkode"),
                Value::from("rawkode"),
                Value::from("rawkode"),
                Value::from("https://cdn.example.com/rawkode.png"),
                Value::from("2026-04-25T00:00:00Z"),
                Value::from("2026-04-25T00:00:00Z"),
            ],
        )
        .await
        .expect("insert user");

        let target_jid: BareJid = "rawkode@example.com".parse().expect("jid");
        let vcard = avatar_vcard_from_user_profile(Arc::clone(&db), &target_jid)
            .await
            .expect("fallback")
            .expect("profile avatar");
        assert!(matches!(
            vcard.photo,
            Some(VCardPhoto::External { ref url }) if url == "https://cdn.example.com/rawkode.png"
        ));

        let stored = VCardStore::new(db)
            .get(&target_jid)
            .await
            .expect("stored lookup");
        assert_eq!(stored, None, "GET fallback must not persist a vCard");
    }

    #[tokio::test]
    async fn vcard_get_fallback_builds_typed_result_not_internal_server_error() {
        let db = test_db("vcard-profile-response").await;
        let conn = db.guard().await.expect("connection");
        conn.execute(
            "INSERT INTO users (id, username, xmpp_localpart, avatar_url, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                Value::from("user-icepuma"),
                Value::from("icepuma"),
                Value::from("icepuma"),
                Value::from("https://cdn.example.com/icepuma.jpg"),
                Value::from("2026-04-25T00:00:00Z"),
                Value::from("2026-04-25T00:00:00Z"),
            ],
        )
        .await
        .expect("insert user");

        let target_jid: BareJid = "icepuma@example.com".parse().expect("jid");
        let vcard = avatar_vcard_from_user_profile(db, &target_jid)
            .await
            .expect("fallback")
            .expect("profile avatar");
        let iq = xmpp_parsers::iq::Iq {
            from: Some("rawkode@example.com/web".parse::<Jid>().expect("from")),
            to: Some("icepuma@example.com".parse::<Jid>().expect("to")),
            id: "vcard-get".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(Element::builder("vCard", NS_VCARD).build()),
        };
        let response = iq_to_xml(waddle_xmpp::xep::xep0054::build_vcard_response(&iq, &vcard));

        assert!(response.contains("type=\"result\"") || response.contains("type='result'"));
        assert!(response.contains("<EXTVAL>https://cdn.example.com/icepuma.jpg</EXTVAL>"));
        assert!(!response.contains("internal-server-error"));
    }
}

async fn handle_blocking_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };
    let user_bare = sender_jid.to_bare();
    let db = match global_database(state).await {
        Ok(db) => db,
        Err(error) => {
            warn!(error = %error, "Failed to access database for blocking IQ");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )];
        }
    };
    let storage = DatabaseBlockingStorage::new(db);
    let request = match waddle_xmpp::xep::xep0191::parse_blocking_request(iq) {
        Ok(request) => request,
        Err(error) => {
            warn!(error = %error, "Invalid blocking IQ");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        }
    };

    match request {
        waddle_xmpp::xep::xep0191::BlockingRequest::GetBlocklist => {
            match storage.get_blocklist(&user_bare).await {
                Ok(blocked) => vec![iq_to_xml(
                    waddle_xmpp::xep::xep0191::build_blocklist_response(iq, &blocked),
                )],
                Err(error) => {
                    warn!(jid = %user_bare, error = %error, "Failed to load blocklist");
                    vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )]
                }
            }
        }
        waddle_xmpp::xep::xep0191::BlockingRequest::Block(jids) => {
            if let Err(error) = storage.add_blocks(&user_bare, &jids).await {
                warn!(jid = %user_bare, error = %error, "Failed to add blocks");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
            send_blocking_pushes(state, &user_bare, true, &jids).await;
            vec![iq_to_xml(
                waddle_xmpp::xep::xep0191::build_blocking_success(iq),
            )]
        }
        waddle_xmpp::xep::xep0191::BlockingRequest::Unblock(jids) => {
            let unblocked_jids = if jids.is_empty() {
                let current = match storage.get_blocklist(&user_bare).await {
                    Ok(current) => current,
                    Err(error) => {
                        warn!(jid = %user_bare, error = %error, "Failed to load blocklist before unblock-all");
                        return vec![build_iq_error_xml_with_addresses(
                            &iq.id,
                            response_from,
                            response_to,
                            "wait",
                            "internal-server-error",
                        )];
                    }
                };
                if let Err(error) = storage.remove_all_blocks(&user_bare).await {
                    warn!(jid = %user_bare, error = %error, "Failed to remove all blocks");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
                current
            } else {
                if let Err(error) = storage.remove_blocks(&user_bare, &jids).await {
                    warn!(jid = %user_bare, error = %error, "Failed to remove blocks");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
                jids
            };
            send_blocking_pushes(state, &user_bare, false, &unblocked_jids).await;
            vec![iq_to_xml(
                waddle_xmpp::xep::xep0191::build_blocking_success(iq),
            )]
        }
    }
}

async fn send_blocking_pushes(
    state: &WebSocketState,
    user_bare: &BareJid,
    blocked: bool,
    jids: &[String],
) {
    for resource_jid in state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(user_bare)
    {
        let push = if blocked {
            waddle_xmpp::xep::xep0191::build_block_push(&resource_jid.clone().into(), jids)
        } else {
            waddle_xmpp::xep::xep0191::build_unblock_push(&resource_jid.clone().into(), jids)
        };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Iq(push))
            .await;
    }
}

async fn handle_push_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "auth",
            "not-authorized",
        )];
    };
    let bare_jid = sender_jid.to_bare().to_string();

    if is_push_enable(iq) {
        let Some(enable) = parse_push_enable(iq) else {
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        };
        let endpoint = enable
            .options
            .iter()
            .find(|(key, _)| key == "endpoint")
            .map(|(_, value)| value.clone());
        let p256dh = enable
            .options
            .iter()
            .find(|(key, _)| key == "p256dh")
            .map(|(_, value)| value.clone());
        let auth_key = enable
            .options
            .iter()
            .find(|(key, _)| key == "auth")
            .map(|(_, value)| value.clone());

        if !endpoint.as_deref().is_some_and(valid_push_endpoint)
            || p256dh.is_none()
            || auth_key.is_none()
        {
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "modify",
                "bad-request",
            )];
        }

        let subscription = waddle_xmpp::push::PushSubscription {
            user_jid: bare_jid.clone(),
            service_jid: enable.jid,
            node: enable.node,
            endpoint,
            p256dh,
            auth_key,
        };
        if let Err(error) = state.deps.protocol.push_store.register(subscription).await {
            warn!(user = %bare_jid, error = %error, "Failed to register push subscription");
            return vec![build_iq_error_xml_with_addresses(
                &iq.id,
                response_from,
                response_to,
                "wait",
                "internal-server-error",
            )];
        }
        return vec![iq_to_xml(build_push_enable_result(iq))];
    }

    let Some(disable) = parse_push_disable(iq) else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )];
    };
    if let Err(error) = state
        .deps
        .protocol
        .push_store
        .remove(&bare_jid, &disable.jid, disable.node.as_deref())
        .await
    {
        warn!(user = %bare_jid, error = %error, "Failed to remove push subscription");
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "wait",
            "internal-server-error",
        )];
    }
    vec![iq_to_xml(build_push_disable_result(iq))]
}

fn valid_push_endpoint(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(addr)) => {
            !(addr.is_private()
                || addr.is_loopback()
                || addr.is_link_local()
                || addr.is_multicast()
                || addr.is_broadcast()
                || addr.is_unspecified())
        }
        Some(Host::Ipv6(addr)) => {
            !(addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_unique_local()
                || addr.is_unicast_link_local()
                || addr.is_multicast())
        }
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            matches!(
                host.as_str(),
                "updates.push.services.mozilla.com"
                    | "fcm.googleapis.com"
                    | "android.googleapis.com"
                    | "web.push.apple.com"
            ) || host.ends_with(".notify.windows.com")
        }
        None => false,
    }
}

async fn permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    object: Object,
    permission: Permission,
) -> Result<bool, String> {
    let Some(session) = session else {
        return Ok(false);
    };
    let response = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&session.user_id),
            permission,
            object,
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    Ok(response.allowed)
}

async fn server_permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    permission: Permission,
) -> Result<bool, String> {
    permission_allowed(
        state,
        session,
        Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        permission,
    )
    .await
}

async fn server_affiliation_for_requester(
    state: &WebSocketState,
    session: Option<&Session>,
) -> Option<SpaceAffiliation> {
    if server_permission_allowed(state, session, Permission::Owner)
        .await
        .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Owner);
    }
    if server_permission_allowed(state, session, Permission::Admin)
        .await
        .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Publisher);
    }
    if server_permission_allowed(state, session, Permission::Member)
        .await
        .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Member);
    }
    None
}

async fn space_affiliation_for_requester(
    state: &WebSocketState,
    session: Option<&Session>,
    node: &str,
) -> Option<SpaceAffiliation> {
    if server_permission_allowed(state, session, Permission::Owner)
        .await
        .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Owner);
    }
    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Owner,
    )
    .await
    .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Owner);
    }
    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Admin,
    )
    .await
    .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Publisher);
    }
    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Member,
    )
    .await
    .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Member);
    }
    None
}

async fn write_tuple_if_absent(state: &WebSocketState, tuple: Tuple) -> Result<(), String> {
    match state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple { tuple })
        .await
    {
        Ok(()) => Ok(()),
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(format!("permission actor failed writing tuple: {error}")),
    }
}

async fn spaces_node_mutation_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    node: &str,
) -> Result<bool, String> {
    if server_permission_allowed(state, session, Permission::CreateSpace).await? {
        return Ok(true);
    }
    permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Owner,
    )
    .await
}

async fn write_space_owner_tuple(
    state: &WebSocketState,
    node: &str,
    session: Option<&Session>,
) -> Result<(), String> {
    let Some(session) = session else {
        return Ok(());
    };
    write_tuple_if_absent(
        state,
        Tuple::new(
            Object::new(ObjectType::Space, node),
            Relation::new("owner"),
            Subject::user(&session.user_id),
        ),
    )
    .await
}

/// Write `channel:<channel_id>#parent → space:<space_node>#` so that all members
/// of the Space gain access to the channel via the permission arrow.
/// Per XEP-0503 §4, a room bookmarked inside a Space node is considered part of
/// that Space; this tuple propagates Space membership into channel access checks.
async fn write_channel_parent_tuple(
    state: &WebSocketState,
    channel_id: &str,
    space_node: &str,
) -> Result<(), String> {
    write_tuple_if_absent(
        state,
        Tuple::new(
            Object::new(ObjectType::Channel, channel_id),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, space_node, ""),
        ),
    )
    .await
}

async fn muc_owner_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    _session: Option<&Session>,
) -> Result<bool, String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?;
    if matches!(
        snapshot.room.get_affiliation(&sender_jid.to_bare()),
        Affiliation::Owner
    ) {
        return Ok(true);
    }

    Ok(false)
}

async fn build_muc_owner_config_response(
    state: &WebSocketState,
    room_jid: &BareJid,
    id: &str,
    response_to: Option<&str>,
) -> Result<String, String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?;
    let form = build_config_form(&snapshot.room);
    let query = Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
        .append(form)
        .build();
    let room_jid_string = room_jid.to_string();
    Ok(build_iq_result_xml(
        id,
        Some(room_jid_string.as_str()),
        response_to,
        Some(query),
    ))
}

fn spaces_service_bare_jid(spaces_domain: &str) -> Result<BareJid, String> {
    spaces_domain
        .parse::<BareJid>()
        .map_err(|error| format!("invalid spaces service JID: {error}"))
}

fn space_details_from_node(node: &waddle_xmpp::pubsub::PubSubNode) -> SpaceDetails {
    let name = if node.node_name == "general" {
        "General".to_string()
    } else {
        node.node_name.clone()
    };
    SpaceDetails {
        id: node.node_name.clone(),
        name,
        description: None,
        owner_id: node.owner.to_string(),
        icon_url: None,
        is_public: true,
        created_at: node.created_at.to_rfc3339(),
    }
}

fn channels_to_disco_items(channels: Vec<XmppChannelRecord>, muc_domain: &str) -> Vec<DiscoItem> {
    channels
        .into_iter()
        .filter_map(|channel| {
            waddle_xmpp::managed_room_jid(&channel.id, muc_domain)
                .ok()
                .map(|room_jid| DiscoItem::muc_room(&room_jid.to_string(), &channel.name))
        })
        .collect()
}

async fn canonical_channel_disco_items(
    state: &WebSocketState,
    muc_domain: &str,
    limit: usize,
) -> Vec<DiscoItem> {
    match list_xmpp_channels(
        state.deps.app_state.db_pool.global_actor().clone(),
        limit,
        0,
    )
    .await
    {
        Ok(channels) => channels_to_disco_items(channels, muc_domain),
        Err(error) => {
            warn!(error = %error, "Failed to list canonical channels for MUC discovery");
            Vec::new()
        }
    }
}

async fn handle_spaces_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    spaces_domain: &str,
    node: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Vec<String> {
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to retrieve Spaces node");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, max_items, item_ids)
        .await
    {
        Ok(stored_items) => {
            let items: Vec<_> = stored_items
                .iter()
                .map(|item| item.to_pubsub_item())
                .collect();
            vec![iq_to_xml(build_pubsub_items_result(iq, node, &items))]
        }
        Err(error) => {
            warn!(node, error = %error, "Failed to retrieve Spaces items");
            vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))]
        }
    }
}

async fn handle_spaces_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item: PubSubItem,
    session: Option<&Session>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, session, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to resolve Spaces node for publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }

    let Some(item_id) = item.id.as_deref() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let Some(payload) = item.payload.as_ref() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let bookmark = match waddle_xmpp::xep::xep0402::parse_bookmark(item_id, payload) {
        Ok(bookmark) => bookmark,
        Err(error) => {
            warn!(item_id, error = %error, "Invalid XEP-0402 Spaces item");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
        }
    };
    if bookmark.jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&bookmark.jid) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
    match get_xmpp_channel(db_actor, &channel_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))],
        Err(error) => {
            warn!(channel_id = %channel_id, error = %error, "Failed to look up channel for Spaces bookmark");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    }

    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            if let Err(error) = write_channel_parent_tuple(state, &channel_id, node).await {
                warn!(
                    channel_id = %channel_id,
                    node,
                    error = %error,
                    "Published Spaces item but failed to sync channel parent tuple; \
                     retracting to keep PubSub and permission graph consistent"
                );
                // Compensating retract: remove the just-published bookmark so
                // the server does not end up in a state where the item is
                // advertised in PubSub but the channel is not accessible via
                // Space membership (XEP-0503 §4).
                if let Err(retract_err) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(&spaces_jid, node, &result.item_id)
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        item_id = %result.item_id,
                        error = %retract_err,
                        "Compensating retract also failed; manual cleanup may be required"
                    );
                }
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InternalServerError,
                ))];
            }
            vec![iq_to_xml(build_pubsub_publish_result(
                iq,
                node,
                &result.item_id,
            ))]
        }
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to publish Spaces item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

async fn handle_spaces_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item_id: &str,
    session: Option<&Session>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, session, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }

    let Ok(room_jid) = item_id.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    if room_jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&spaces_jid, node, item_id)
        .await
    {
        Ok(true) => vec![iq_to_xml(build_pubsub_success(iq))],
        Ok(false) => vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))],
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to retract Spaces item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

async fn room_space_metadata_extensions(
    state: &WebSocketState,
    room_jid: &BareJid,
    _domain: &str,
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
        Ok(Some(space_node)) => {
            vec![build_spaces_metadata_form(&space_details_from_node(
                &space_node,
            ))]
        }
        Ok(None) => vec![],
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to find Space node for room");
            vec![]
        }
    }
}

fn data_form_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .find(|field| field.attr("var") == Some(var))
        .and_then(|field| field.get_child("value", DATA_FORMS_NS))
        .map(|value| value.texts().collect())
}

fn data_form_bool(form: &Element, var: &str) -> Option<bool> {
    data_form_value(form, var).and_then(|value| match value.as_str() {
        "1" | "true" | "True" | "TRUE" => Some(true),
        "0" | "false" | "False" | "FALSE" => Some(false),
        _ => None,
    })
}

async fn apply_muc_owner_config(
    state: &WebSocketState,
    room_jid: &BareJid,
    iq: &xmpp_parsers::iq::Iq,
    session: Option<&Session>,
) -> Result<(), String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?
        .room
        .config;

    if let xmpp_parsers::iq::IqType::Set(query) = &iq.payload {
        if let Some(form) = query.get_child("x", DATA_FORMS_NS) {
            if let Some(name) =
                data_form_value(form, "muc#roomconfig_roomname").filter(|value| !value.is_empty())
            {
                config.name = name;
            }
            config.description = data_form_value(form, "muc#roomconfig_roomdesc")
                .filter(|value| !value.is_empty())
                .or(config.description);
            if let Some(members_only) = data_form_bool(form, "muc#roomconfig_membersonly") {
                config.members_only = members_only;
            }
            if let Some(moderated) = data_form_bool(form, "muc#roomconfig_moderatedroom") {
                config.moderated = moderated;
            }
            if let Some(enable_logging) = data_form_bool(form, "muc#roomconfig_enablelogging") {
                config.enable_logging = enable_logging;
            }
            if let Some(forum) = data_form_bool(form, "muc#roomconfig_forum") {
                config.forum = forum;
            }
        }
    }

    // Waddle rooms are persistent, non-anonymous collaboration surfaces.
    config.persistent = true;

    room_actor
        .ask(UpdateConfig {
            config: config.clone(),
        })
        .await
        .map_err(|error| format!("config update failed: {error:?}"))?;

    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(());
    };
    let now = chrono::Utc::now().to_rfc3339();
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
                VALUES (?, ?, ?, ?, 0, 0, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    channel_type = excluded.channel_type,
                    updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![
                channel_id.clone().into(),
                config.name.into(),
                config.description.into(),
                (if config.forum { "forum" } else { "text" }).into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
        .map_err(|error| format!("channel upsert failed: {error}"))?;

    // Write channel#owner → session user so the creator can always rejoin the
    // managed room after a server restart (before a Space bookmark is published).
    // XEP-0045 §10 requires the room creator to be an owner; without this tuple
    // the channel becomes unjoinable after restart.
    match session {
        Some(session) => {
            write_tuple_if_absent(
                state,
                Tuple::new(
                    Object::new(ObjectType::Channel, &channel_id),
                    Relation::new("owner"),
                    Subject::user(&session.user_id),
                ),
            )
            .await
            .map_err(|error| format!("channel owner tuple failed: {error}"))?;
        }
        None => {
            warn!(
                channel_id = %channel_id,
                "apply_muc_owner_config called without a session; \
                 channel owner tuple not written — room may be inaccessible after server restart"
            );
        }
    }

    Ok(())
}

async fn handle_command_iq(
    request_iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    bound_jid: Option<&FullJid>,
) -> Vec<String> {
    let sender_jid: Jid = match bound_jid.cloned().map(Jid::from) {
        Some(jid) => jid,
        None => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::not_authorized(Some("Authenticated session required".to_string())),
            )];
        }
    };

    let command = match parse_command_from_iq(request_iq) {
        Ok(command) => command,
        Err(err) => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::bad_request(Some(format!("Invalid command request: {err}"))),
            )];
        }
    };

    let node = command.node.clone();
    let session_id = command.session_id.clone();
    let ctx = CommandContext {
        from: sender_jid,
        authenticated_user_id: authenticated_session
            .as_ref()
            .map(|session| session.user_id.clone()),
        iq: request_iq.clone(),
        command,
    };

    let result = state.deps.protocol.command_registry.dispatch(ctx).await;
    let response_command = match result {
        CommandResult::Executing {
            form,
            session_id,
            notes,
        } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Executing);
            command.session_id = Some(session_id);
            command.form = Some(form);
            command.notes = notes;
            command
        }
        CommandResult::Completed { form, notes } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Completed);
            command.session_id = session_id;
            command.form = form;
            command.notes = notes;
            command
        }
        CommandResult::Canceled { notes } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Canceled);
            command.session_id = session_id;
            command.notes = notes;
            command
        }
        CommandResult::Error(err) => return vec![build_xmpp_error_response(request_iq, err)],
    };

    vec![iq_to_xml(build_command_result(
        request_iq,
        &response_command,
    ))]
}
