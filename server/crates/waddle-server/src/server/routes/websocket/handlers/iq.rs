use chrono;
use jid::{BareJid, FullJid, Jid};
use tracing::{debug, warn};
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
    mam::{build_fin_iq, build_result_messages, is_mam_query, parse_mam_query},
    muc::room_actor::GetSnapshot,
    protocol::{
        frame::{parse_frame, InboundFrame},
        ConnectionPhase, StanzaContext as ProtocolStanzaContext,
    },
    pubsub::{
        build_pubsub_error, build_pubsub_items_result, build_pubsub_publish_result,
        build_pubsub_success, is_pubsub_iq, parse_pubsub_iq, PubSubError, PubSubRequest,
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
        build_command_items, build_command_result, build_spaces_metadata_form,
        parse_command_from_iq, Command, CommandStatus, NODE_COMMANDS,
    },
    Stanza, StanzaErrorCondition, StanzaErrorType, WaddleDetails, XmppError,
};

use super::super::{
    build_iq_error_xml, build_iq_error_xml_with_addresses, build_iq_result_xml, destroy_room_actor,
    get_room_actor, iq_to_xml, is_muc_room_jid, list_room_jids, stanza_to_xml, WebSocketState,
};
use super::presence::get_managed_channel_for_room;
use crate::auth::Session;
use crate::db::actor::DbExecute;
use crate::server::routes::channels::list_channels_from_db;
use crate::server::routes::waddles::{
    get_waddle_by_id, list_all_waddles_from_db, list_user_waddles,
};

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
    let spaces_domain = format!("spaces.{domain}");
    let single_tenant = std::env::var("WADDLE_SINGLE_TENANT")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);

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

    // Disco info on MUC service
    if payload_ns == "http://jabber.org/protocol/disco#info" {
        let request_iq = &iq;
        let query = match parse_disco_info_query(request_iq) {
            Ok(query) => query,
            Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        };

        if to.as_deref() == Some(muc_domain) {
            let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
            let mut features = vec![Feature::muc(), Feature::replies()];
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
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, None);
                    return vec![iq_to_xml(response)];
                }

                if is_muc_room_jid(state, &room_jid).await {
                    if let Some(channel) = get_managed_channel_for_room(state, &room_jid).await {
                        let identities = vec![Identity::muc_room(Some(&channel.name))];
                        let mut features =
                            muc_room_features(true, false, false, channel.channel_type == "forum");
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
                let waddle = match get_waddle_by_id(
                    state.deps.app_state.db_pool.global_actor().clone(),
                    node,
                )
                .await
                {
                    Ok(Some(waddle)) => waddle,
                    Ok(None) => {
                        return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
                    }
                    Err(err) => {
                        warn!(
                            node = %node,
                            error = %err,
                            "Failed to load space node for disco#info"
                        );
                        return vec![build_iq_error_xml(&id, "wait", "internal-server-error")];
                    }
                };

                let is_member = if single_tenant {
                    true
                } else if let Some(session) = authenticated_session {
                    match list_user_waddles(
                        state.deps.app_state.db_pool.global_actor().clone(),
                        &session.user_id,
                        200,
                        0,
                    )
                    .await
                    {
                        Ok(waddles) => waddles.iter().any(|candidate| candidate.id == node),
                        Err(err) => {
                            warn!(
                                user_id = %session.user_id,
                                node = %node,
                                error = %err,
                                "Failed membership check for space node disco#info"
                            );
                            false
                        }
                    }
                } else {
                    false
                };

                if !single_tenant && !is_member && !waddle.is_public {
                    return vec![build_iq_error_xml(&id, "cancel", "item-not-found")];
                }

                let identities = vec![Identity::pubsub_leaf(Some(&waddle.name))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::pubsub(),
                    Feature::pubsub_retrieve_items(),
                    Feature::spaces(),
                ];
                let metadata = build_spaces_metadata_form(&WaddleDetails {
                    id: waddle.id.clone(),
                    name: waddle.name.clone(),
                    description: waddle.description.clone(),
                    owner_id: waddle.owner_user_id.clone(),
                    icon_url: waddle.icon_url.clone(),
                    is_public: waddle.is_public,
                    created_at: waddle.created_at.clone(),
                });
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
        let upload_domain = format!("upload.{domain}");
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

    // Disco items - list services/rooms
    if payload_ns == "http://jabber.org/protocol/disco#items" {
        let request_iq = &iq;
        let query = match parse_disco_items_query(request_iq) {
            Ok(query) => query,
            Err(_) => return vec![build_iq_error_xml(&id, "modify", "bad-request")],
        };

        if to.as_deref() == Some(muc_domain) {
            debug!("Disco items query on MUC service");
            let mut rooms = list_room_jids(state).await;
            rooms.sort_by_key(|room| room.to_string());

            let items: Vec<DiscoItem> = if rooms.is_empty() {
                let lobby_jid = format!("lobby@{muc_domain}");
                vec![DiscoItem::muc_room(&lobby_jid, "Lobby")]
            } else {
                rooms
                    .into_iter()
                    .map(|room_jid| {
                        let room_jid_string = room_jid.to_string();
                        let name = room_jid
                            .node()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| room_jid_string.clone());
                        DiscoItem::muc_room(&room_jid_string, &name)
                    })
                    .collect()
            };

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
            let global_db_actor = state.deps.app_state.db_pool.global_actor().clone();
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(node) => {
                    let can_list_channels = if single_tenant {
                        true
                    } else if let Some(session) = authenticated_session {
                        match list_user_waddles(global_db_actor.clone(), &session.user_id, 200, 0)
                            .await
                        {
                            Ok(waddles) => waddles.iter().any(|w| w.id == node),
                            Err(err) => {
                                warn!(
                                    user_id = %session.user_id,
                                    node = %node,
                                    error = %err,
                                    "Failed membership check for spaces node discovery"
                                );
                                false
                            }
                        }
                    } else {
                        false
                    };

                    if !can_list_channels {
                        vec![]
                    } else {
                        match state.deps.app_state.db_pool.get_waddle_actor(node).await {
                            Ok(waddle_actor) => {
                                match list_channels_from_db(waddle_actor, node, 200, 0).await {
                                    Ok(channels) => {
                                        channels
                                            .into_iter()
                                            .filter_map(|channel| {
                                                waddle_xmpp::managed_room_jid(
                                                    node,
                                                    &channel.id,
                                                    muc_domain,
                                                )
                                                .ok()
                                                .map(|room_jid| {
                                                    DiscoItem::muc_room(
                                                        &room_jid.to_string(),
                                                        &channel.name,
                                                    )
                                                })
                                            })
                                            .collect()
                                    }
                                    Err(err) => {
                                        warn!(
                                            node = %node,
                                            error = %err,
                                            "Failed to list channels for spaces node discovery"
                                        );
                                        vec![]
                                    }
                                }
                            }
                            Err(err) => {
                                warn!(
                                    node = %node,
                                    error = %err,
                                    "Failed to open waddle database for spaces node discovery"
                                );
                                vec![]
                            }
                        }
                    }
                }
                None => {
                    let waddles = if single_tenant {
                        match list_all_waddles_from_db(global_db_actor.clone(), 1, 0).await {
                            Ok(rows) => rows,
                            Err(err) => {
                                warn!(error = %err, "Failed to list canonical single-tenant space");
                                vec![]
                            }
                        }
                    } else if let Some(session) = authenticated_session {
                        const PAGE_SIZE: usize = 200;
                        let mut offset = 0usize;
                        let mut all = Vec::new();

                        loop {
                            match list_user_waddles(
                                global_db_actor.clone(),
                                &session.user_id,
                                PAGE_SIZE,
                                offset,
                            )
                            .await
                            {
                                Ok(page) => {
                                    let count = page.len();
                                    all.extend(page);
                                    if count < PAGE_SIZE {
                                        break;
                                    }
                                    offset += PAGE_SIZE;
                                }
                                Err(err) => {
                                    warn!(
                                        user_id = %session.user_id,
                                        error = %err,
                                        "Failed to list user spaces for discovery"
                                    );
                                    break;
                                }
                            }
                        }

                        all
                    } else {
                        vec![]
                    };

                    waddles
                        .into_iter()
                        .map(|w| DiscoItem::spaces_node(&spaces_domain, &w.id, Some(&w.name)))
                        .collect()
                }
            };

            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        debug!("Disco items query on server");
        let upload_domain = format!("upload.{domain}");
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

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        return vec![build_iq_result_xml(
            &id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
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

            PubSubRequest::Subscribe { .. } | PubSubRequest::Unsubscribe { .. } => {
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
