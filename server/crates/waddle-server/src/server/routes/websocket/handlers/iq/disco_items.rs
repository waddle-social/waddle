use super::{
    bad_request_iq_error, build_command_items, build_disco_items_response,
    build_iq_error_xml_typed, command_refs_by_boundary, debug, extension_route_disco_node,
    get_managed_channel_for_room, get_room_actor, internal_server_error_iq_error, iq_to_xml,
    item_not_found_iq_error, list_xmpp_channels, muc_occupant_disco, parse_disco_items_query,
    resolve_muc_room_archive_access, space_details_from_node, spaces_service_bare_jid, warn,
    Affiliation, BareJid, CheckPermission, CommandBoundary, ConnectionPhase, DbQuery, DbQueryOne,
    DiscoItem, FullJid, GetSnapshot, IqHandlerContext, Jid, Object, ObjectType, Permission,
    Relation, RoomArchiveAccess, RsmRequest, RsmResponse, Subject, SubjectType, WebSocketState,
    XmppChannelRecord, NODE_COMMANDS, PEP_NODE_AVATAR_DATA, PEP_NODE_AVATAR_METADATA,
};
use crate::permissions::host_grant::{HostGrantAction, PrincipalEvidence, RoomBinding, RoomGate};
use crate::permissions::ListSubjects;
use crate::server::xmpp_channels::list_federated_public_xmpp_channels;
use std::collections::HashSet;
use waddle_xmpp::xep::xep0059::{build_rsm_response_element, extract_rsm_request};
use waddle_xmpp::xep::xep0402::NS_BOOKMARKS2;
use waddle_xmpp_core::disco::NODE_WADDLE_ROOM_BOTS;
use xmpp_parsers::disco::Item;
use xmpp_parsers::rsm::{First, SetQuery, SetResult};

/// Snapshot-ask deadline for the instant-room scan: a wedged room
/// actor must not stall service discovery.
const DISCO_ITEMS_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

enum RoomCommandTarget {
    GroupDm,
    NotGroupDm,
    NotMember,
    Missing,
    Failed,
}

async fn classify_room_command_target(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: Option<&FullJid>,
) -> RoomCommandTarget {
    let Some(requester_bare) = requester.map(|jid| jid.to_bare()) else {
        return RoomCommandTarget::NotMember;
    };
    if let Some(room_actor) = get_room_actor(state, room_jid).await {
        let snapshot = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => snapshot.room,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "Failed to load room snapshot for disco#items"
                );
                return RoomCommandTarget::Failed;
            }
        };
        return if snapshot.config.group_dm {
            if snapshot.affiliation(&requester_bare) >= Affiliation::Member {
                RoomCommandTarget::GroupDm
            } else {
                let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
                    return RoomCommandTarget::NotMember;
                };
                if requester_has_durable_group_dm_membership(state, &requester_bare, &channel_id)
                    .await
                {
                    RoomCommandTarget::GroupDm
                } else {
                    RoomCommandTarget::NotMember
                }
            }
        } else {
            RoomCommandTarget::NotGroupDm
        };
    }

    match get_managed_channel_for_room(state, room_jid).await {
        Ok(Some(channel)) if channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM => {
            let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
                return RoomCommandTarget::Missing;
            };
            if requester_has_group_dm_membership_tuple(state, &requester_bare, &channel_id)
                .await
                .is_some_and(|allowed| allowed)
                || requester_has_durable_group_dm_membership(state, &requester_bare, &channel_id)
                    .await
            {
                return RoomCommandTarget::GroupDm;
            }
            match state
                .deps
                .app_state
                .permission_actor
                .ask(CheckPermission {
                    subject: Subject::user(requester_bare.to_string()),
                    permission: Permission::Member,
                    object: Object::new(ObjectType::Channel, channel_id),
                })
                .await
            {
                Err(kameo::error::SendError::HandlerError(error)) => {
                    warn!(
                        room = %room_jid,
                        error = ?error,
                        "Failed to check persisted group-DM membership for disco#items"
                    );
                    RoomCommandTarget::Failed
                }
                Ok(response) if response.allowed => RoomCommandTarget::GroupDm,
                Ok(_) => RoomCommandTarget::NotMember,
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = ?error,
                        "Failed to ask permission actor for disco#items"
                    );
                    RoomCommandTarget::Failed
                }
            }
        }
        Ok(Some(_)) => RoomCommandTarget::NotGroupDm,
        Ok(None) => RoomCommandTarget::Missing,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Failed to load persisted room for disco#items"
            );
            RoomCommandTarget::Failed
        }
    }
}

/// The bots that have posted in `room`, for anyone who may enter it
/// (XEP-0313 §5.1's archive gate: the bots are what the archive shows).
/// Uninstalled and banned bots are left out.
async fn room_bot_items(
    state: &WebSocketState,
    room: &BareJid,
    requester: Option<&BareJid>,
) -> Result<Vec<DiscoItem>, ()> {
    let channel = get_managed_channel_for_room(state, room)
        .await
        .map_err(|error| warn!(%room, %error, "Failed to load room for bot listing"))?;
    match resolve_muc_room_archive_access(state, room, requester, channel.as_ref()).await {
        RoomArchiveAccess::Allowed => {}
        RoomArchiveAccess::Denied => return Ok(Vec::new()),
        RoomArchiveAccess::Error => return Err(()),
    }
    let plugins =
        crate::server::extension_bot_rooms::list(state.deps.app_state.db_pool.global(), room)
            .await
            .map_err(|error| warn!(%room, %error, "Failed to list room bots"))?;
    if plugins.is_empty() {
        return Ok(Vec::new());
    }
    // A managed channel's bans live in its permission tuples, which outlast
    // the room actor; bots post only in managed channels.
    let outcasts = match channel.as_ref() {
        Some(channel) => state
            .deps
            .app_state
            .permission_actor
            .ask(ListSubjects {
                object: Object::new(ObjectType::Channel, channel.id.as_str()),
                relation: Relation::new("outcast"),
            })
            .await
            .map_err(|error| warn!(%room, ?error, "Failed to list room outcasts"))?,
        None => Vec::new(),
    };
    let domains = &state.deps.service_domains;
    let manager = &state.deps.protocol.extension_manager;
    Ok(plugins
        .iter()
        .filter_map(|plugin| domains.extension_bot_jid(plugin).ok())
        .filter_map(|bot| {
            let bot = Jid::from(bot.to_bare());
            crate::server::extension_bot::installed_bot(domains, manager, &bot)
                .map(|installed| (bot, installed))
        })
        .filter(|(bot, _)| {
            !outcasts.iter().any(|subject| {
                subject.subject_type == SubjectType::User
                    && subject.relation.is_none()
                    && subject.id == bot.as_str()
            })
        })
        .map(|(bot, installed)| DiscoItem::new(bot.as_str(), Some(installed.name.as_str()), None))
        .collect())
}

async fn requester_has_group_dm_membership_tuple(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> Option<bool> {
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(requester_bare.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .ok()
        .map(|response| response.allowed)
}

async fn requester_has_durable_group_dm_membership(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> bool {
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbQueryOne {
            sql: r#"
                SELECT 1 FROM permission_tuples
                WHERE object_type = 'channel'
                  AND object_id = ?
                  AND relation = 'member'
                  AND subject_type = 'user'
                  AND subject_id = ?
                  AND subject_relation IS NULL
                LIMIT 1
            "#
            .to_string(),
            params: vec![channel_id.into(), requester_bare.to_string().into()],
        })
        .await
        .is_ok_and(|row| row.is_some())
}

pub(super) async fn handle_disco_items_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let target_to = ctx.target_to;
    let domain = ctx.domain;
    let muc_domain = ctx.muc_domain;
    let upload_domain = ctx.upload_domain;
    let spaces_domain = ctx.spaces_domain;
    let community_domain = ctx.community_domain;
    let extensions_domain = ctx.extensions_domain;
    let push_domain = ctx.push_domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // Disco items - list services/rooms
    if payload_ns == "http://jabber.org/protocol/disco#items" {
        let request_iq = &iq;
        let query = match parse_disco_items_query(request_iq) {
            Ok(query) => query,
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
        };

        // XEP-0045 §6.6 (#1265 item 10): disco to an occupant JID.
        if let Some(error) = muc_occupant_disco::muc_occupant_disco_error(
            state,
            target_to,
            muc_domain,
            phase.bound_jid(),
        )
        .await
        {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                error,
            )];
        }

        if target_to == Some(muc_domain) {
            debug!("Disco items query on MUC service");
            // XEP-0030 §3.2 (#1265 item 11): the MUC service hosts no
            // disco#items nodes; an unknown node is <item-not-found/>,
            // never the room list.
            if query.node.is_some() {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Unknown disco#items node."),
                )];
            }
            let rsm_request = match rsm_request_from_iq(request_iq) {
                Ok(rsm_request) => rsm_request,
                Err(_) => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        bad_request_iq_error("Malformed RSM <set/> element."),
                    )];
                }
            };
            let requester = phase.bound_jid().map(|jid| jid.to_bare());
            let Ok(items) = muc_service_rooms(
                state,
                muc_domain,
                RoomListingFor::Session(requester.as_ref()),
            )
            .await
            else {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    internal_server_error_iq_error("Failed to list MUC rooms."),
                )];
            };
            let Some(rsm_request) = rsm_request else {
                let response = build_disco_items_response(request_iq, &items, None);
                return vec![iq_to_xml(response)];
            };
            let (page, rsm_response) =
                page_disco_items(&items, &rsm_request, |item| item.jid.as_str());
            let mut response = build_disco_items_response(request_iq, page, None);
            if let xmpp_parsers::iq::Iq::Result {
                payload: Some(query),
                ..
            } = &mut response
            {
                query.append_child(build_rsm_response_element(&rsm_response));
            }
            return vec![iq_to_xml(response)];
        }

        if query.node.as_deref() == Some(NODE_WADDLE_ROOM_BOTS) {
            if let Some(room_jid) = target_to
                .and_then(|target| target.parse::<BareJid>().ok())
                .filter(|room_jid| room_jid.domain().as_str() == muc_domain)
            {
                let requester = phase.bound_jid().map(|jid| jid.to_bare());
                let Ok(items) = room_bot_items(state, &room_jid, requester.as_ref()).await else {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                };
                let response =
                    build_disco_items_response(request_iq, &items, Some(NODE_WADDLE_ROOM_BOTS));
                return vec![iq_to_xml(response)];
            }
        }

        if query.node.as_deref() == Some(NODE_COMMANDS) {
            if let Some(room_jid) = target_to
                .filter(|target| !target.contains('/'))
                .and_then(|target| target.parse::<BareJid>().ok())
                .filter(|room_jid| room_jid.domain().as_str() == muc_domain)
            {
                match classify_room_command_target(state, &room_jid, phase.bound_jid()).await {
                    RoomCommandTarget::GroupDm => {
                        let commands = state.deps.protocol.command_registry.list_commands().await;
                        let command_refs =
                            command_refs_by_boundary(&commands, CommandBoundary::MucRoom);
                        let response =
                            build_command_items(request_iq, &command_refs, target_to.unwrap());
                        return vec![iq_to_xml(response)];
                    }
                    RoomCommandTarget::NotGroupDm | RoomCommandTarget::NotMember => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            item_not_found_iq_error("Requested item not found."),
                        )];
                    }
                    RoomCommandTarget::Missing => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            item_not_found_iq_error("Requested room not found."),
                        )];
                    }
                    RoomCommandTarget::Failed => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            internal_server_error_iq_error("Internal server error."),
                        )];
                    }
                }
            }
        }

        if target_to == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, CommandBoundary::Server);
            let response = build_command_items(request_iq, &command_refs, domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(extensions_domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, CommandBoundary::Extensions);
            let response = build_command_items(request_iq, &command_refs, extensions_domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(push_domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, CommandBoundary::PushService);
            let response = build_command_items(request_iq, &command_refs, push_domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(extensions_domain) {
            if let Some(node) = query.node.as_deref() {
                let known_route_node = state
                    .deps
                    .protocol
                    .extension_manager
                    .route_descriptors()
                    .iter()
                    .any(|route| extension_route_disco_node(route) == node);
                if !known_route_node {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        item_not_found_iq_error("Requested item not found."),
                    )];
                }
                let response = build_disco_items_response(request_iq, &[], Some(node));
                return vec![iq_to_xml(response)];
            }
            let items = state
                .deps
                .protocol
                .extension_manager
                .route_descriptors()
                .iter()
                .map(|route| {
                    let node = extension_route_disco_node(route);
                    DiscoItem::new(
                        extensions_domain,
                        Some(route.label.as_str()),
                        Some(node.as_str()),
                    )
                })
                .collect::<Vec<_>>();
            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(community_domain) {
            // The community pubsub service hosts exactly two
            // well-known nodes: the XEP-0472 feed and the XEP-0501
            // stories node. Surface both so clients can enumerate
            // before subscribing.
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => vec![],
                None => vec![
                    DiscoItem::new(
                        community_domain,
                        Some("Community Feed"),
                        Some(waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED),
                    ),
                    DiscoItem::new(
                        community_domain,
                        Some("Community Stories"),
                        Some(waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES),
                    ),
                    DiscoItem::new(
                        community_domain,
                        Some("Community Events"),
                        Some(waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS),
                    ),
                ],
            };
            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(spaces_domain) {
            if let Some(node) = query.node.as_deref() {
                let requester = phase.bound_jid().map(|jid| jid.to_bare());
                let items = match space_children_for_session(
                    state,
                    spaces_domain,
                    node,
                    requester.as_ref(),
                )
                .await
                {
                    Ok(items) => items,
                    Err(refusal) => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            refusal.error(),
                        )];
                    }
                };
                let rsm_request = match rsm_request_from_iq(request_iq) {
                    Ok(rsm_request) => rsm_request,
                    Err(_) => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            bad_request_iq_error("Malformed RSM <set/> element."),
                        )];
                    }
                };
                let Some(rsm_request) = rsm_request else {
                    let response = build_disco_items_response(request_iq, &items, Some(node));
                    return vec![iq_to_xml(response)];
                };
                let (page, rsm_response) = page_disco_items(&items, &rsm_request, |item| {
                    item.name.as_deref().unwrap_or_default()
                });
                let mut response = build_disco_items_response(request_iq, page, Some(node));
                if let xmpp_parsers::iq::Iq::Result {
                    payload: Some(query),
                    ..
                } = &mut response
                {
                    query.append_child(build_rsm_response_element(&rsm_response));
                }
                return vec![iq_to_xml(response)];
            }
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => vec![],
                None => match spaces_service_bare_jid(spaces_domain) {
                    Ok(spaces_jid) => match state
                        .deps
                        .protocol
                        .pubsub_storage
                        .list_nodes(&spaces_jid)
                        .await
                    {
                        Ok(nodes) => {
                            let requester = phase.bound_jid().map(|jid| jid.to_bare());
                            let mut items = Vec::new();
                            for node in nodes {
                                let Ok(Some(space_node)) = state
                                    .deps
                                    .protocol
                                    .pubsub_storage
                                    .get_node(&spaces_jid, &node)
                                    .await
                                else {
                                    continue;
                                };
                                let Some(space) = space_details_from_node(&space_node) else {
                                    continue;
                                };
                                // The Space node of an enrolled host-grant
                                // scope is not served through PubSub, so it
                                // is not listed either.
                                if state.deps.app_state.host_grants.space_enrolled(&node).await
                                    != Ok(false)
                                {
                                    continue;
                                }
                                let visible = match requester.as_ref() {
                                    Some(requester) => crate::pubsub_authz::can_subscribe(
                                        &state.deps.protocol.pubsub_storage,
                                        &spaces_jid,
                                        &node,
                                        requester,
                                        false,
                                    )
                                    .await
                                    .unwrap_or(false),
                                    None => matches!(
                                        space_node.config.access_model,
                                        waddle_xmpp::pubsub::AccessModel::Open
                                    ),
                                };
                                if visible {
                                    items.push(DiscoItem::spaces_node(
                                        spaces_domain,
                                        &node,
                                        Some(&space.name),
                                    ));
                                }
                            }
                            items
                        }
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

        if target_to == Some(push_domain) {
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => Vec::new(),
                None => {
                    let Some(owner_bare_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
                        let response =
                            build_disco_items_response(request_iq, &[], query.node.as_deref());
                        return vec![iq_to_xml(response)];
                    };
                    match state
                        .deps
                        .protocol
                        .push_service
                        .list_node_names_for_owner(&owner_bare_jid)
                        .await
                    {
                        Ok(nodes) => nodes
                            .into_iter()
                            .map(|node| DiscoItem::pubsub_node(push_domain, &node))
                            .collect(),
                        Err(error) => {
                            warn!(error = %error, owner = %owner_bare_jid, "Failed to list owner Push Service nodes");
                            Vec::new()
                        }
                    }
                }
            };

            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        // An extension bot is a leaf entity with no items; any other
        // address on the extensions domain does not exist (XEP-0030 §4.1).
        if let Some(target) = target_to
            .filter(|target| *target != extensions_domain)
            .and_then(|target| target.parse::<jid::Jid>().ok())
            .filter(|target| state.deps.service_domains.is_extensions_address(target))
        {
            let is_bot = crate::server::extension_bot::installed_bot(
                &state.deps.service_domains,
                &state.deps.protocol.extension_manager,
                &target,
            )
            .is_some();
            if is_bot && query.node.is_none() {
                let response = build_disco_items_response(request_iq, &[], None);
                return vec![iq_to_xml(response)];
            }
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        }

        if let Some(target) = target_to {
            if query.node.is_none() {
                if let Ok(target_bare) = target.parse::<BareJid>() {
                    if target_bare.node().is_some() {
                        let items = match state
                            .deps
                            .protocol
                            .pubsub_storage
                            .list_nodes(&target_bare)
                            .await
                        {
                            Ok(nodes) => nodes
                                .into_iter()
                                .filter(|node| {
                                    node == PEP_NODE_AVATAR_DATA || node == PEP_NODE_AVATAR_METADATA
                                })
                                .map(|node| DiscoItem::pubsub_node(&target_bare.to_string(), &node))
                                .collect::<Vec<_>>(),
                            Err(error) => {
                                warn!(target = %target_bare, error = %error, "Failed to list PEP nodes");
                                vec![]
                            }
                        };
                        let response = build_disco_items_response(request_iq, &items, None);
                        return vec![iq_to_xml(response)];
                    }
                }
            }
        }

        // XEP-0030 §4.1: every node an entity hosts is answered above. An
        // unknown node (on a room, an account or the server) does not exist;
        // it is never the server's own service list.
        if query.node.is_some() {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                item_not_found_iq_error("Unknown disco#items node."),
            )];
        }

        debug!("Disco items query on server");
        let calls_mixer_domain = format!("calls.{domain}");
        let items = vec![
            DiscoItem::muc_service(muc_domain, Some("Chatrooms")),
            DiscoItem::upload_service(upload_domain, Some("HTTP File Upload")),
            DiscoItem::spaces_service(spaces_domain, Some("Spaces")),
            DiscoItem::community_service(community_domain, Some("Community")),
            DiscoItem::pubsub_service(extensions_domain, Some("Extensions")),
            DiscoItem::pubsub_service(push_domain, Some("Push Service")),
            DiscoItem::calls_mixer(&calls_mixer_domain, Some("Group Call Mixer")),
        ];
        let response = build_disco_items_response(request_iq, &items, None);
        return vec![iq_to_xml(response)];
    }
    Vec::new()
}

/// Practical fetch bound for the MUC room list. RSM paging (#1265
/// item 11) means clients are no longer silently truncated at the old
/// hard 500 cap; this bound only protects the server from an unbounded
/// DB read.
const MUC_DISCO_ITEMS_FETCH_BOUND: usize = 10_000;

/// Catalog rows read for one listing of the MUC service to an account at
/// another server: only rows of rooms that are listed and open to other
/// servers are read, and a room beyond the bound is not listed to them.
const REMOTE_LISTING_FETCH_BOUND: usize = 4_096;

/// Extract an optional XEP-0059 `<set/>` from a disco#items query.
fn rsm_request_from_iq(iq: &xmpp_parsers::iq::Iq) -> Result<Option<RsmRequest>, ()> {
    let xmpp_parsers::iq::Iq::Get { payload, .. } = iq else {
        return Ok(None);
    };
    match extract_rsm_request(payload) {
        None => Ok(None),
        Some(Ok(request)) => Ok(Some(request)),
        Some(Err(_)) => Err(()),
    }
}

/// The bounds `start..end` of one XEP-0059 page of at most `max` items
/// over `items`, which are sorted by `key`.
///
/// Supports `after`, `index`, and both `<before/>` forms (a key for
/// backward paging, empty for the last page). An `after` or `before`
/// anchor that has vanished between pages (a room destroyed
/// mid-pagination) resolves to its ordered insertion point, so the client
/// continues with no items skipped, instead of getting a silent empty page
/// or a hard <item-not-found/> restart; an anchor is never looked up, so
/// a page tells nothing of an item the asker is not listed.
fn page_window<T>(
    items: &[T],
    key: impl Fn(&T) -> &str,
    max: usize,
    before: Option<&str>,
    after: Option<&str>,
    index: Option<usize>,
) -> (usize, usize) {
    let total = items.len();
    match (before, after, index) {
        (Some(""), _, _) => (total.saturating_sub(max), total),
        (Some(before), _, _) => {
            let end = items.partition_point(|item| key(item) < before);
            (end.saturating_sub(max), end)
        }
        (None, Some(after), _) => {
            let start = items.partition_point(|item| key(item) <= after);
            (start, start.saturating_add(max).min(total))
        }
        (None, None, Some(index)) => {
            let start = index.min(total);
            (start, start.saturating_add(max).min(total))
        }
        (None, None, None) => (0, max.min(total)),
    }
}

/// XEP-0059 paging over a disco#items list sorted by `key` (a stable UID
/// ordering per XEP-0059 §2.2): the room JID for the MUC service, the item
/// ID for a Space's children.
fn page_disco_items<'a>(
    items: &'a [DiscoItem],
    request: &RsmRequest,
    key: fn(&DiscoItem) -> &str,
) -> (&'a [DiscoItem], RsmResponse) {
    let total = items.len();
    let max = request.max.map(|max| max as usize).unwrap_or(total);
    let (start, end) = page_window(
        items,
        key,
        max,
        request.before.as_deref(),
        request.after.as_deref(),
        request.index.map(|index| index as usize),
    );
    let page = &items[start..end];
    let mut response = RsmResponse::new().with_count(u32::try_from(total).unwrap_or(u32::MAX));
    if let (Some(first), Some(last)) = (page.first(), page.last()) {
        response = response
            .with_first(key(first).to_owned(), u32::try_from(start).ok())
            .with_last(key(last).to_owned());
    }
    (page, response)
}

/// One page of a listing for an account at another server: at most `most`
/// of `items`, which are sorted by `key`, whatever the request asks. The
/// `<set/>` that places the page is told when the request asked for
/// paging, or when the listing did not fit in one answer: a responder that
/// returns a limited result set says so (XEP-0059 §1, XEP-0045 §6.3). Its
/// count and index are of the listing as built for this asker.
pub(crate) fn page_listing(
    items: Vec<Item>,
    rsm: Option<&SetQuery>,
    most: usize,
    key: fn(&Item) -> &str,
) -> (Vec<Item>, Option<SetResult>) {
    let total = items.len();
    let max = rsm.and_then(|rsm| rsm.max).unwrap_or(most).min(most);
    let (start, end) = page_window(
        &items,
        key,
        max,
        rsm.and_then(|rsm| rsm.before.as_deref()),
        rsm.and_then(|rsm| rsm.after.as_deref()),
        rsm.and_then(|rsm| rsm.index),
    );
    let partial = end - start < total;
    let page: Vec<Item> = items.into_iter().skip(start).take(end - start).collect();
    if rsm.is_none() && !partial {
        return (page, None);
    }
    let set = SetResult {
        first: page.first().map(|first| First {
            index: Some(start),
            item: key(first).to_owned(),
        }),
        last: page.last().map(|last| key(last).to_owned()),
        count: Some(total),
    };
    (page, Some(set))
}

/// Whom a listing of the MUC service's rooms is built for.
#[derive(Clone, Copy)]
enum RoomListingFor<'a> {
    /// A session of this deployment, by its bound account when it has one.
    Session(Option<&'a BareJid>),
    /// An account at another server, decided on the evidence of the stream
    /// that carried its request.
    Remote {
        account: &'a BareJid,
        evidence: &'a PrincipalEvidence,
    },
}

impl RoomListingFor<'_> {
    fn is_remote(self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

/// XEP-0045 §6.3: the rooms of the MUC service listed to `audience`,
/// sorted by room JID.
///
/// A room is listed when it is public (not hidden) and no group
/// conversation. For an account at another server it must also be open to
/// other servers (its `federated` setting), read from the catalog for a
/// catalog-backed room and from the room's actor for an instant one; the
/// room's Space, if it has one, adds and removes nothing (ADR-0020 D4). A
/// room an enrolled host-grant scope manages is listed only to an account
/// whose canonical grants allow it `view`, on the evidence of the stream
/// that carried the request for an account at another server. A room whose
/// grants cannot be read now is left out.
async fn muc_service_rooms(
    state: &WebSocketState,
    muc_domain: &str,
    audience: RoomListingFor<'_>,
) -> Result<Vec<DiscoItem>, String> {
    let (mut items, channel_backed) =
        canonical_channel_disco_items(state, muc_domain, audience).await?;
    // XEP-0045 §6.3 (#1265 item 11): public live instant rooms are part of
    // the service's item list too, not only channel-backed persistent
    // rooms.
    items.extend(
        live_public_instant_room_items(state, muc_domain, &channel_backed, audience.is_remote())
            .await,
    );
    items.sort_by(|a, b| a.jid.cmp(&b.jid));
    items.dedup_by(|a, b| a.jid == b.jid);
    Ok(items)
}

/// XEP-0045 §6.3 for an account at another server: the rooms
/// [`muc_service_rooms`] lists to it, sorted by room JID. `Err` when the
/// listing cannot be built now.
pub(crate) async fn muc_rooms_for_remote(
    state: &WebSocketState,
    account: &BareJid,
    evidence: &PrincipalEvidence,
) -> Result<Vec<Item>, ()> {
    let muc_domain = state.deps.service_domains.muc.clone();
    let items = muc_service_rooms(
        state,
        &muc_domain,
        RoomListingFor::Remote { account, evidence },
    )
    .await
    .map_err(|_| ())?;
    Ok(items
        .into_iter()
        .filter_map(|item| {
            Some(Item {
                jid: item.jid.parse::<Jid>().ok()?,
                node: None,
                name: item.name,
            })
        })
        .collect())
}

/// Whether the room of the channel `channel_id`, which an enrolled
/// host-grant scope manages, is listed to `audience`: its canonical grants
/// allow `view`, read by the channel, the key the enrolled check used. A
/// session without a bound account, and a room whose grants cannot be read
/// now, is not listed.
async fn managed_room_listed(
    state: &WebSocketState,
    channel_id: &str,
    audience: RoomListingFor<'_>,
) -> bool {
    let (subject, evidence) = match audience {
        RoomListingFor::Session(Some(requester)) => (requester, PrincipalEvidence::NoStream),
        RoomListingFor::Session(None) => return false,
        RoomListingFor::Remote { account, evidence } => (account, *evidence),
    };
    matches!(
        state
            .deps
            .app_state
            .host_grants
            .allows_as(
                RoomBinding::Channel(channel_id),
                subject,
                &evidence,
                HostGrantAction::View,
            )
            .await,
        RoomGate::Managed(true)
    )
}

/// Why a Space's children are not listed to a session.
enum SpaceListingRefusal {
    /// The node is not a Space this session may read: one answer for a node
    /// that does not exist, one it may not read, and the Space of an
    /// enrolled host-grant scope.
    NotFound,
    /// What the session may read cannot be told now.
    Unavailable,
}

impl SpaceListingRefusal {
    fn error(&self) -> xmpp_parsers::stanza_error::StanzaError {
        match self {
            Self::NotFound => item_not_found_iq_error("Requested item not found."),
            Self::Unavailable => internal_server_error_iq_error("Internal server error."),
        }
    }
}

/// XEP-0503, "Advertising space membership": the rooms a Space lists, as
/// the `disco#items` of its node, for a session that may read the Space.
/// Each is listed as XEP-0060 §5.5 lists a published item, with the
/// service's address and the item ID (the room's JID) as its name, so a
/// client verifies that a room it was told of is part of the Space.
///
/// Only bookmarks of rooms are listed. A room of this deployment that an
/// enrolled host-grant scope manages is listed only where its canonical
/// grants allow the session `view`; every other bookmark is listed as the
/// Space's items show it. Unlike a read of the items, the listing writes
/// nothing.
async fn space_children_for_session(
    state: &WebSocketState,
    spaces_domain: &str,
    node: &str,
    requester: Option<&BareJid>,
) -> Result<Vec<DiscoItem>, SpaceListingRefusal> {
    let Some(requester) = requester else {
        return Err(SpaceListingRefusal::NotFound);
    };
    let spaces_jid =
        spaces_service_bare_jid(spaces_domain).map_err(|_| SpaceListingRefusal::Unavailable)?;
    // The Space node of an enrolled host-grant scope is served to nobody.
    match state.deps.app_state.host_grants.space_enrolled(node).await {
        Ok(false) => {}
        Ok(true) => return Err(SpaceListingRefusal::NotFound),
        Err(_) => return Err(SpaceListingRefusal::Unavailable),
    }
    let storage = &state.deps.protocol.pubsub_storage;
    match crate::pubsub_authz::can_subscribe(storage, &spaces_jid, node, requester, false).await {
        Ok(true) => {}
        Ok(false) => return Err(SpaceListingRefusal::NotFound),
        Err(error) => {
            warn!(node, requester = %requester, error = %error, "Failed to authorize a Space's listing");
            return Err(SpaceListingRefusal::Unavailable);
        }
    }
    let stored = storage
        .get_items(&spaces_jid, node, None, &[])
        .await
        .map_err(|error| {
            warn!(node, error = %error, "Failed to read a Space's items for its listing");
            SpaceListingRefusal::Unavailable
        })?;
    let muc_domain = state.deps.service_domains.muc.as_str();
    let mut items = Vec::new();
    for item in &stored {
        let item = item.to_pubsub_item();
        let Some(id) = item.id.as_deref() else {
            continue;
        };
        if item
            .payload
            .as_ref()
            .is_some_and(|payload| !payload.is("conference", NS_BOOKMARKS2))
        {
            continue;
        }
        let Some(room) = id
            .parse::<BareJid>()
            .ok()
            .filter(|room| room.node().is_some())
        else {
            continue;
        };
        if room.domain().as_str() == muc_domain {
            let gate = state
                .deps
                .app_state
                .host_grants
                .allows_as(
                    RoomBinding::Room(&room),
                    requester,
                    &PrincipalEvidence::NoStream,
                    HostGrantAction::View,
                )
                .await;
            if !matches!(gate, RoomGate::Unmanaged | RoomGate::Managed(true)) {
                continue;
            }
        }
        items.push(DiscoItem::new(spaces_domain, Some(id), None));
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items.dedup_by(|a, b| a.name == b.name);
    Ok(items)
}

/// Live rooms created ad hoc through the MUC protocol (no managed
/// channel record) that are public and not group DMs, and, for an account
/// at another server (`remote`), open to other servers. Channel-backed
/// rooms are already listed from the database.
async fn live_public_instant_room_items(
    state: &WebSocketState,
    muc_domain: &str,
    channel_backed: &ChannelBackedRooms,
    remote: bool,
) -> Vec<DiscoItem> {
    let room_jids =
        match waddle_xmpp::muc::RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
            .list_rooms()
            .await
        {
            Ok(room_jids) => room_jids,
            Err(error) => {
                warn!(error = %error, "Failed to list live MUC rooms for disco#items");
                return Vec::new();
            }
        };
    // Bound the per-request work: this runs on the client's
    // connect-time topology discovery, so it must stay cheap. Every
    // catalog-backed room (public or private) is skipped in memory —
    // no per-room DB query — so only true instant rooms, normally zero
    // to a handful, cost one snapshot ask each, capped hard.
    const MAX_INSTANT_ROOM_SCAN: usize = 1_000;
    // Order of operations: (1) the free in-memory filter removes every
    // catalog-listed room so managed rooms never consume cap slots;
    // (2) sort for a deterministic selection when the cap bites;
    // (3) cap so the batched DB probe's IN-clause and the snapshot
    // asks stay bounded.
    let mut candidates: Vec<BareJid> = room_jids
        .into_iter()
        .filter(|room_jid| {
            room_jid.domain().as_str() == muc_domain && !channel_backed.jids.contains(room_jid)
        })
        .collect();
    candidates.sort_unstable();
    candidates.truncate(MAX_INSTANT_ROOM_SCAN);
    channel_backed
        .drop_unlisted_channel_rooms(state, &mut candidates)
        .await;
    // A room an enrolled host-grant scope manages is listed only from its
    // channel row, under its grants; the instant-room scan lists none, and
    // if enrollment cannot be read it lists no instant room at all.
    let channel_ids: Vec<String> = candidates
        .iter()
        .filter_map(waddle_xmpp::parse_managed_room_jid)
        .collect();
    let channel_refs: Vec<&str> = channel_ids.iter().map(String::as_str).collect();
    let Ok(enrolled) = state
        .deps
        .app_state
        .host_grants
        .enrolled_channels(&channel_refs)
        .await
    else {
        return Vec::new();
    };
    candidates.retain(|room_jid| {
        waddle_xmpp::parse_managed_room_jid(room_jid)
            .is_none_or(|channel_id| !enrolled.contains(&channel_id))
    });
    let mut items = Vec::new();
    for room_jid in candidates {
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            continue;
        };
        let Ok(snapshot) = room_actor
            .ask(GetSnapshot)
            .reply_timeout(DISCO_ITEMS_SNAPSHOT_TIMEOUT)
            .await
        else {
            continue;
        };
        let config = &snapshot.room.config;
        if !config.public_room || config.group_dm || (remote && !config.federated) {
            continue;
        }
        let name = if config.name.trim().is_empty() {
            room_jid
                .node()
                .map(|node| node.to_string())
                .unwrap_or_else(|| room_jid.to_string())
        } else {
            config.name.clone()
        };
        items.push(DiscoItem::muc_room(&room_jid.to_string(), &name));
    }
    items
}

fn channels_to_disco_items(channels: Vec<XmppChannelRecord>, muc_domain: &str) -> Vec<DiscoItem> {
    channels
        .into_iter()
        .filter(|channel| channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM)
        .filter(|channel| channel.public_room)
        .filter_map(|channel| {
            waddle_xmpp::managed_room_jid(&channel.id, muc_domain)
                .ok()
                .map(|room_jid| DiscoItem::muc_room(&room_jid.to_string(), &channel.name))
        })
        .collect()
}

/// The channel disco items listed to `audience` PLUS the JID set of the
/// catalog-backed rooms this request read, so the instant-room scan can
/// classify channel-backed rooms in memory without per-room DB reads.
///
/// A session's request reads every catalog row (public or not), within
/// [`MUC_DISCO_ITEMS_FETCH_BOUND`]. A request of an account at another
/// server reads only the rows of rooms that are public and open to other
/// servers, within [`REMOTE_LISTING_FETCH_BOUND`]; the set is then
/// incomplete, and the instant-room scan checks its candidates against the
/// catalog.
async fn canonical_channel_disco_items(
    state: &WebSocketState,
    muc_domain: &str,
    audience: RoomListingFor<'_>,
) -> Result<(Vec<DiscoItem>, ChannelBackedRooms), String> {
    let db = state.deps.app_state.db_pool.global_actor().clone();
    let (fetched, complete) = if audience.is_remote() {
        let channels = list_federated_public_xmpp_channels(db, REMOTE_LISTING_FETCH_BOUND).await;
        (channels, false)
    } else {
        let channels = list_xmpp_channels(db, MUC_DISCO_ITEMS_FETCH_BOUND, 0).await;
        let complete = channels
            .as_ref()
            .is_ok_and(|channels| channels.len() < MUC_DISCO_ITEMS_FETCH_BOUND);
        (channels, complete)
    };
    let mut channels = fetched.map_err(|error| {
        warn!(error = %error, "Failed to list canonical channels for MUC discovery");
        error
    })?;
    if audience.is_remote() {
        // The query reads only these; a row is checked again here so the
        // listing never rests on the query alone.
        for channel in &mut channels {
            channel.public_room &= channel.federated;
        }
    }
    // A room an enrolled host-grant scope manages is members-only and is
    // listed only to an account its canonical grants allow `view`.
    let listed: Vec<&str> = channels
        .iter()
        .filter(|channel| channel.public_room)
        .map(|channel| channel.id.as_str())
        .collect();
    let enrolled = state
        .deps
        .app_state
        .host_grants
        .enrolled_channels(&listed)
        .await
        .map_err(|error| error.to_string())?;
    for channel in &mut channels {
        if enrolled.contains(&channel.id)
            && !managed_room_listed(state, &channel.id, audience).await
        {
            channel.public_room = false;
        }
    }
    let channel_backed = ChannelBackedRooms {
        jids: channels
            .iter()
            .filter_map(|channel| waddle_xmpp::managed_room_jid(&channel.id, muc_domain).ok())
            .collect(),
        // When the fetch filled the bound, or read only some rows, rows
        // beyond it exist and the set is incomplete — membership misses
        // must fall back to a per-room catalog check.
        truncated: !complete,
    };
    Ok((
        channels_to_disco_items(channels, muc_domain),
        channel_backed,
    ))
}

/// The catalog-backed room JIDs known to this request, plus whether the
/// fetch hit its bound (in which case the set may be incomplete).
struct ChannelBackedRooms {
    jids: HashSet<BareJid>,
    truncated: bool,
}

impl ChannelBackedRooms {
    /// Drop catalog-backed rooms that the bounded fetch missed. No-op
    /// for the common (untruncated) case — the in-memory set filtering
    /// already happened; a truncated catalog fetch issues ONE batched
    /// membership query for the capped candidate list instead of a
    /// per-room lookup.
    async fn drop_unlisted_channel_rooms(
        &self,
        state: &WebSocketState,
        candidates: &mut Vec<BareJid>,
    ) {
        if !self.truncated || candidates.is_empty() {
            return;
        }
        let channel_ids: Vec<(BareJid, String)> = candidates
            .iter()
            .filter_map(|room_jid| {
                waddle_xmpp::parse_managed_room_jid(room_jid)
                    .map(|channel_id| (room_jid.clone(), channel_id))
            })
            .collect();
        if channel_ids.is_empty() {
            return;
        }
        let placeholders = vec!["?"; channel_ids.len()].join(", ");
        let rows = state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbQuery {
                sql: format!("SELECT id FROM channels WHERE id IN ({placeholders})"),
                params: channel_ids
                    .iter()
                    .map(|(_, channel_id)| channel_id.clone().into())
                    .collect(),
            })
            .await;
        let known: HashSet<String> = match rows {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| match row.into_iter().next() {
                    Some(crate::db::Value::Text(id)) => Some(id),
                    _ => None,
                })
                .collect(),
            Err(error) => {
                warn!(error = %error, "Failed batched channel membership check for disco#items");
                // Fail closed for listing purposes: treat lookup failure
                // as "unknown", keeping candidates (their snapshot config
                // still gates what gets listed).
                return;
            }
        };
        let backed: HashSet<&BareJid> = channel_ids
            .iter()
            .filter(|(_, channel_id)| known.contains(channel_id))
            .map(|(room_jid, _)| room_jid)
            .collect();
        candidates.retain(|room_jid| !backed.contains(room_jid));
    }
}
