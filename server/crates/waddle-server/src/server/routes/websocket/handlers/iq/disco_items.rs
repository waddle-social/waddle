use super::{
    bad_request_iq_error, build_command_items, build_disco_items_response,
    build_iq_error_xml_typed, command_refs_by_boundary, debug, extension_route_disco_node,
    get_managed_channel_for_room, get_room_actor, internal_server_error_iq_error, iq_to_xml,
    item_not_found_iq_error, list_xmpp_channels, muc_occupant_disco, parse_disco_items_query,
    resolve_muc_room_archive_access, space_details_from_node, spaces_service_bare_jid, warn,
    Affiliation, BareJid, CheckPermission, CommandBoundary, ConnectionPhase, DbQuery, DbQueryOne,
    DiscoItem, FullJid, GetSnapshot, IqHandlerContext, Jid, Object, ObjectType, Permission,
    Relation, RoomArchiveAccess, RsmRequest, RsmResponse, Subject, SubjectType, WebSocketState,
    NODE_COMMANDS, PEP_NODE_AVATAR_DATA, PEP_NODE_AVATAR_METADATA,
};
use crate::permissions::host_grant::{
    HostGrantAction, HostGrantGate, ManagedRoomGrants, PrincipalEvidence, RoomBinding, RoomGate,
};
use crate::permissions::ListSubjects;
use futures::StreamExt;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;
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
            let items = match muc_rooms_for_session(state, muc_domain, requester.as_ref()).await {
                Ok(items) => items,
                Err(error) => {
                    warn!(error = %error, "Failed to list MUC rooms");
                    return vec![build_iq_error_xml_typed(
                        id,
                        None,
                        None,
                        internal_server_error_iq_error("Failed to list MUC rooms."),
                    )];
                }
            };
            let (page, rsm_response) =
                page_disco_items(&items, rsm_request.as_ref(), usize::MAX, |item| {
                    item.jid.as_str()
                });
            return vec![paged_disco_items_response(
                request_iq,
                page,
                None,
                rsm_response,
            )];
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
                let (page, rsm_response) = page_disco_items(
                    &items,
                    rsm_request.as_ref(),
                    MAX_LISTED_SPACE_ROOMS,
                    |item| item.name.as_deref().unwrap_or_default(),
                );
                return vec![paged_disco_items_response(
                    request_iq,
                    page,
                    Some(node),
                    rsm_response,
                )];
            }
            // A node on the query was answered above with that Space's rooms.
            let items: Vec<DiscoItem> = match spaces_service_bare_jid(spaces_domain) {
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
            };

            let response = build_disco_items_response(request_iq, &items, None);
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

/// Longest a session's listing of the MUC service, or of a Space's rooms,
/// takes, the rooms read and the session's grants decided: a room not
/// decided by then is left out, and the listing is answered with the rest.
const LISTING_LIMIT: Duration = Duration::from_millis(2_500);

/// Longest the rooms a listing may name are read for, before any asker's
/// grants: the channel catalog, the live rooms and their actors. A room
/// whose actor has not answered by then is left out.
const CANDIDATES_LIMIT: Duration = Duration::from_millis(1_500);

/// Longest one build of the rooms the MUC service may list to accounts at
/// other servers takes ([`RemoteRoomCandidates`]). It is below
/// [`CANDIDATES_LIMIT`], so the asker whose request started the build is
/// answered from it.
const REMOTE_CANDIDATES_BUILD_LIMIT: Duration = Duration::from_millis(1_200);

/// How long a replica answers accounts at other servers from one build of
/// the rooms its MUC service may list.
const REMOTE_CANDIDATES_TTL: Duration = Duration::from_secs(2);

/// Managed rooms whose grants one listing reads at once.
const GRANT_READS_AT_ONCE: usize = 4;

/// Instant rooms whose actors one listing asks at once.
const SNAPSHOT_ASKS_AT_ONCE: usize = 16;

/// Live instant rooms one listing asks at most. Every catalog-backed room
/// is set aside before the bound applies, so they never take its places.
const MAX_INSTANT_ROOM_SCAN: usize = 1_000;

/// Room addresses one catalog membership query names at most.
const CATALOG_CHECK_CHUNK: usize = 500;

/// Rooms one answer to a session's `disco#items` of a Space node lists at
/// most; the rest are reached by paging (XEP-0059).
const MAX_LISTED_SPACE_ROOMS: usize = 1_024;

/// Why the MUC service's rooms could not be listed now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RoomListingError {
    /// The channel catalog could not be read.
    #[error("the channel catalog could not be read")]
    Catalog,
    /// Whether a room is managed by an enrolled host-grant scope cannot be
    /// told: the enrolled set has not loaded.
    #[error("the host-grant enrolled set is not known")]
    Enrollment,
    /// The rooms were not read within the listing's time.
    #[error("the rooms were not read in time")]
    Deadline,
}

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
/// ID for a Space's rooms. A page holds at most `most` items whatever the
/// request asks; the `<set/>` that places it is answered when the request
/// asked for paging, or when the list did not fit in one answer (XEP-0059
/// §1), and `None` otherwise.
fn page_disco_items<'a>(
    items: &'a [DiscoItem],
    request: Option<&RsmRequest>,
    most: usize,
    key: fn(&DiscoItem) -> &str,
) -> (&'a [DiscoItem], Option<RsmResponse>) {
    let total = items.len();
    let max = request
        .and_then(|request| request.max)
        .map_or(most, |max| (max as usize).min(most));
    let (start, end) = page_window(
        items,
        key,
        max,
        request.and_then(|request| request.before.as_deref()),
        request.and_then(|request| request.after.as_deref()),
        request
            .and_then(|request| request.index)
            .map(|index| index as usize),
    );
    let page = &items[start..end];
    if request.is_none() && page.len() == total {
        return (page, None);
    }
    let mut response = RsmResponse::new().with_count(u32::try_from(total).unwrap_or(u32::MAX));
    if let (Some(first), Some(last)) = (page.first(), page.last()) {
        response = response
            .with_first(key(first).to_owned(), u32::try_from(start).ok())
            .with_last(key(last).to_owned());
    }
    (page, Some(response))
}

/// The `disco#items` result answering `request_iq` with `page` of `node`,
/// carrying the `<set/>` that places the page when there is one.
fn paged_disco_items_response(
    request_iq: &xmpp_parsers::iq::Iq,
    page: &[DiscoItem],
    node: Option<&str>,
    rsm: Option<RsmResponse>,
) -> String {
    let mut response = build_disco_items_response(request_iq, page, node);
    if let (
        Some(rsm),
        xmpp_parsers::iq::Iq::Result {
            payload: Some(query),
            ..
        },
    ) = (rsm, &mut response)
    {
        query.append_child(build_rsm_response_element(&rsm));
    }
    iq_to_xml(response)
}

/// One page of a listing for an account at another server: at most `most`
/// of `items`, which are sorted by `key`, whatever the request asks. The
/// `<set/>` that places the page is told when the request asked for
/// paging, or when the listing did not fit in one answer: a responder that
/// returns a limited result set says so (XEP-0059 §1, XEP-0045 §6.3). Its
/// index is of the listing as built for this asker, and so is its count
/// when the listing is `complete`; a listing built from a bounded read
/// that may have left items out tells no count, and always carries the
/// `<set/>`.
pub(crate) fn page_listing(
    items: Vec<Item>,
    rsm: Option<&SetQuery>,
    most: usize,
    complete: bool,
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
    let partial = end - start < total || !complete;
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
        count: complete.then_some(total),
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

impl<'a> RoomListingFor<'a> {
    fn is_remote(self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    /// The subject whose grants decide a managed room, and the evidence
    /// they are decided on; `None` for a session without a bound account,
    /// which is listed no managed room.
    fn subject(self) -> Option<(&'a BareJid, PrincipalEvidence)> {
        match self {
            Self::Session(requester) => {
                requester.map(|requester| (requester, PrincipalEvidence::NoStream))
            }
            Self::Remote { account, evidence } => Some((account, *evidence)),
        }
    }
}

/// A room the MUC service may list, as the channel catalog or the room's
/// actor describes it, before any asker's grants are read: it is public
/// (not hidden) and no group conversation.
#[derive(Clone, Debug)]
pub(crate) struct ListedRoom {
    jid: BareJid,
    name: String,
    /// The channel the room's address names, if it names one: the key a
    /// host-grant scope's enrollment of the room is read by.
    channel: Option<String>,
    /// Whether the room has a catalog row. A room an enrolled scope manages
    /// is listed only from its row.
    catalogued: bool,
    /// Whether accounts at other servers may reach the room (`federated`).
    federated: bool,
}

/// What finds a room's canonical grants.
enum GrantKey {
    /// The channel the enrolled set names the room by.
    Channel(String),
    /// The room's address.
    Room(BareJid),
}

impl GrantKey {
    fn binding(&self) -> RoomBinding<'_> {
        match self {
            Self::Channel(channel) => RoomBinding::Channel(channel),
            Self::Room(room) => RoomBinding::Room(room),
        }
    }
}

/// The rooms of one listing for one asker, before its answer is built:
/// those no enrolled scope manages, and the managed ones whose grants
/// allowed the asker `view`, with the grants they were decided on.
struct ViewDecisions<T> {
    /// Rooms no enrolled scope manages, listed as their own rules say.
    unmanaged: Vec<T>,
    /// Managed rooms whose canonical grants allowed `view` when read.
    allowed: Vec<(T, ManagedRoomGrants)>,
    /// Whom `allowed` was decided for, on what evidence.
    subject: Option<(BareJid, PrincipalEvidence)>,
}

impl<T> ViewDecisions<T> {
    fn unmanaged(unmanaged: Vec<T>) -> Self {
        Self {
            unmanaged,
            allowed: Vec::new(),
            subject: None,
        }
    }

    /// The rooms listed, asked just before the answer is built: every
    /// unmanaged room, and each managed room whose grants still allow
    /// `view` ([`HostGrantGate::still_allows`]), the decision itself while
    /// its fence ticket is current and a decision read now otherwise. A
    /// managed room that is not confirmed by `deadline` is left out.
    async fn still_viewable(self, gate: &HostGrantGate, deadline: Instant) -> Vec<T> {
        let Self {
            mut unmanaged,
            allowed,
            subject,
        } = self;
        let Some((subject, evidence)) = subject else {
            return unmanaged;
        };
        let (subject, evidence) = (&subject, &evidence);
        let mut confirmed = futures::stream::iter(allowed)
            .map(|(room, grants)| async move {
                let still = gate
                    .still_allows(&grants, subject, evidence, HostGrantAction::View)
                    .await;
                (room, still)
            })
            .buffer_unordered(GRANT_READS_AT_ONCE);
        while let Ok(Some((room, still))) =
            tokio::time::timeout_at(deadline, confirmed.next()).await
        {
            if still {
                unmanaged.push(room);
            }
        }
        unmanaged
    }
}

/// Decide each of `rooms` for `subject`, whose request reached the surface
/// with `evidence`: a room no enrolled scope manages (any longer) is kept
/// as unmanaged, and a managed one only where its canonical grants allow
/// `view`, with those grants. At most [`GRANT_READS_AT_ONCE`] rooms' grants
/// are read at once, all by `deadline`; a room not decided by then, or
/// whose grants cannot be read, is left out.
async fn decide_views<T>(
    gate: &HostGrantGate,
    rooms: Vec<(T, GrantKey)>,
    subject: &BareJid,
    evidence: PrincipalEvidence,
    deadline: Instant,
) -> ViewDecisions<T> {
    let mut decided = ViewDecisions {
        unmanaged: Vec::new(),
        allowed: Vec::new(),
        subject: Some((subject.clone(), evidence)),
    };
    let mut decisions = futures::stream::iter(rooms)
        .map(|(room, key)| async move {
            let grants = gate.room(key.binding()).await;
            (room, grants)
        })
        .buffer_unordered(GRANT_READS_AT_ONCE);
    while let Ok(Some((room, grants))) = tokio::time::timeout_at(deadline, decisions.next()).await {
        match grants {
            RoomGate::Unmanaged => decided.unmanaged.push(room),
            RoomGate::Managed(grants)
                if grants.allows_as(subject, &evidence, HostGrantAction::View) =>
            {
                decided.allowed.push((room, grants));
            }
            RoomGate::Managed(_) | RoomGate::Unavailable => {}
        }
    }
    decided
}

/// The rooms the MUC service may list (XEP-0045 §6.3) before any asker's
/// grants are read, sorted by room JID: the public catalog rows that are
/// no group conversation, and the live instant rooms whose actors describe
/// them so by `deadline`.
///
/// Every catalog row is read, public or not, within
/// [`MUC_DISCO_ITEMS_FETCH_BOUND`], so the instant-room scan sets
/// catalog-backed rooms aside in memory; only when the read filled the
/// bound are the scan's candidates checked against the catalog.
async fn room_candidates(
    state: &WebSocketState,
    muc_domain: &str,
    deadline: Instant,
) -> Result<Vec<ListedRoom>, RoomListingError> {
    let db = state.deps.app_state.db_pool.global_actor().clone();
    let read = tokio::time::timeout_at(
        deadline,
        list_xmpp_channels(db, MUC_DISCO_ITEMS_FETCH_BOUND, 0),
    )
    .await;
    let channels = match read {
        Ok(Ok(channels)) => channels,
        Ok(Err(error)) => {
            warn!(error = %error, "Failed to list canonical channels for MUC discovery");
            return Err(RoomListingError::Catalog);
        }
        Err(_) => return Err(RoomListingError::Deadline),
    };
    let channel_backed = ChannelBackedRooms {
        jids: channels
            .iter()
            .filter_map(|channel| waddle_xmpp::managed_room_jid(&channel.id, muc_domain).ok())
            .collect(),
        // When the fetch filled the bound, rows beyond it exist and the set
        // is incomplete: membership misses fall back to a catalog check.
        truncated: channels.len() >= MUC_DISCO_ITEMS_FETCH_BOUND,
    };
    let mut rooms: Vec<ListedRoom> = channels
        .into_iter()
        .filter(|channel| {
            channel.public_room && channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM
        })
        .filter_map(|channel| {
            let jid = waddle_xmpp::managed_room_jid(&channel.id, muc_domain).ok()?;
            Some(ListedRoom {
                jid,
                name: channel.name,
                channel: Some(channel.id),
                catalogued: true,
                federated: channel.federated,
            })
        })
        .collect();
    // XEP-0045 §6.3 (#1265 item 11): public live instant rooms are part of
    // the service's item list too, not only channel-backed persistent
    // rooms.
    rooms.extend(instant_room_candidates(state, muc_domain, &channel_backed, deadline).await);
    rooms.sort_by(|a, b| a.jid.as_str().cmp(b.jid.as_str()));
    rooms.dedup_by(|a, b| a.jid == b.jid);
    Ok(rooms)
}

/// Of `candidates`, the rooms the MUC service lists to `audience`, before
/// the answer is built ([`ViewDecisions::still_viewable`]).
///
/// For an account at another server a room must be open to other servers
/// (its `federated` setting), read from the catalog for a catalog-backed
/// room and from the room's actor for an instant one; the room's Space, if
/// it has one, adds and removes nothing (ADR-0020 D4). Whether a room is
/// managed is read now, from the replica's enrolled set: a room an enrolled
/// host-grant scope manages is listed only from its catalog row, and only
/// to a subject whose canonical grants allow it `view`, on the evidence of
/// the stream that carried the request for an account at another server.
/// `Err` while the enrolled set has not loaded.
async fn rooms_listed_to(
    state: &WebSocketState,
    candidates: &[ListedRoom],
    audience: RoomListingFor<'_>,
    deadline: Instant,
) -> Result<ViewDecisions<ListedRoom>, RoomListingError> {
    let reachable: Vec<&ListedRoom> = candidates
        .iter()
        .filter(|room| !audience.is_remote() || room.federated)
        .collect();
    let channels: Vec<&str> = reachable
        .iter()
        .filter_map(|room| room.channel.as_deref())
        .collect();
    let gate = &state.deps.app_state.host_grants;
    let enrolled = match tokio::time::timeout_at(deadline, gate.enrolled_channels(&channels)).await
    {
        Ok(Ok(enrolled)) => enrolled,
        Ok(Err(_)) => return Err(RoomListingError::Enrollment),
        Err(_) => return Err(RoomListingError::Deadline),
    };
    let mut unmanaged = Vec::new();
    let mut managed = Vec::new();
    for room in reachable {
        match room
            .channel
            .as_ref()
            .filter(|channel| enrolled.contains(channel.as_str()))
        {
            None => unmanaged.push(room.clone()),
            // An instant room at the address of a managed room is listed
            // to nobody.
            Some(_) if !room.catalogued => {}
            Some(channel) => managed.push((room.clone(), GrantKey::Channel(channel.clone()))),
        }
    }
    let mut decided = match audience.subject() {
        Some((subject, evidence)) => decide_views(gate, managed, subject, evidence, deadline).await,
        None => ViewDecisions::unmanaged(Vec::new()),
    };
    decided.unmanaged.extend(unmanaged);
    Ok(decided)
}

/// XEP-0045 §6.3 for a session: the rooms of the MUC service listed to
/// it, sorted by room JID, read and decided within [`LISTING_LIMIT`].
async fn muc_rooms_for_session(
    state: &WebSocketState,
    muc_domain: &str,
    requester: Option<&BareJid>,
) -> Result<Vec<DiscoItem>, RoomListingError> {
    let started = Instant::now();
    let deadline = started + LISTING_LIMIT;
    let candidates = room_candidates(state, muc_domain, started + CANDIDATES_LIMIT).await?;
    let decided = rooms_listed_to(
        state,
        &candidates,
        RoomListingFor::Session(requester),
        deadline,
    )
    .await?;
    let mut rooms = decided
        .still_viewable(&state.deps.app_state.host_grants, deadline)
        .await;
    rooms.sort_by(|a, b| a.jid.as_str().cmp(b.jid.as_str()));
    Ok(rooms
        .into_iter()
        .map(|room| DiscoItem::muc_room(room.jid.as_str(), &room.name))
        .collect())
}

/// XEP-0045 §6.3 for an account at another server: the rooms of the MUC
/// service listed to it ([`rooms_listed_to`]), sorted by room JID, from the
/// replica's recent build of the rooms it may list (`candidates`) and the
/// account's grants decided now, all by `deadline`. `Err` when the listing
/// cannot be built now.
pub(crate) async fn muc_rooms_for_remote(
    state: &Arc<WebSocketState>,
    candidates: &Arc<RemoteRoomCandidates>,
    account: &BareJid,
    evidence: &PrincipalEvidence,
    deadline: Instant,
) -> Result<Vec<Item>, RoomListingError> {
    let read_by = deadline.min(Instant::now() + CANDIDATES_LIMIT);
    let rooms = candidates.rooms(state, read_by).await?;
    let decided = rooms_listed_to(
        state,
        &rooms,
        RoomListingFor::Remote { account, evidence },
        deadline,
    )
    .await?;
    let mut rooms = decided
        .still_viewable(&state.deps.app_state.host_grants, deadline)
        .await;
    rooms.sort_by(|a, b| a.jid.as_str().cmp(b.jid.as_str()));
    Ok(rooms
        .into_iter()
        .map(|room| Item {
            jid: Jid::from(room.jid),
            node: None,
            name: Some(room.name),
        })
        .collect())
}

/// One build of the rooms [`RemoteRoomCandidates`] holds.
type CandidatesBuild = Result<Arc<[ListedRoom]>, RoomListingError>;

/// The rooms a replica's MUC service may list to accounts at other servers
/// ([`room_candidates`]), built at most once at a time and reused for
/// [`REMOTE_CANDIDATES_TTL`]: what the catalog and the rooms' actors say of
/// them, and nothing that depends on the asker. Whether a room is managed,
/// and the asker's grants in it, are read for every request.
///
/// A build runs on a task of its own, within
/// [`REMOTE_CANDIDATES_BUILD_LIMIT`]: an asker whose time runs out stops
/// waiting for it, and the build goes on for the askers after it. A room
/// whose catalog row or configuration changed is listed as it was for at
/// most the reuse time.
#[derive(Default)]
pub(crate) struct RemoteRoomCandidates {
    held: tokio::sync::Mutex<HeldCandidates>,
}

#[derive(Default)]
struct HeldCandidates {
    /// The last build that succeeded, and when it finished.
    built: Option<(Instant, Arc<[ListedRoom]>)>,
    /// The build running now, which every asker waits for.
    building: Option<watch::Receiver<Option<CandidatesBuild>>>,
}

impl RemoteRoomCandidates {
    /// The rooms of a build that finished within [`REMOTE_CANDIDATES_TTL`],
    /// or of the build running now, or else of one started now, waited for
    /// until `deadline`.
    async fn rooms(
        self: &Arc<Self>,
        state: &Arc<WebSocketState>,
        deadline: Instant,
    ) -> CandidatesBuild {
        let mut building = {
            let Ok(mut held) = tokio::time::timeout_at(deadline, self.held.lock()).await else {
                return Err(RoomListingError::Deadline);
            };
            if let Some((built_at, rooms)) = &held.built {
                if built_at.elapsed() < REMOTE_CANDIDATES_TTL {
                    return Ok(Arc::clone(rooms));
                }
            }
            match held
                .building
                .as_ref()
                .filter(|building| building.has_changed().is_ok())
            {
                Some(building) => building.clone(),
                None => {
                    let building = self.build(state);
                    held.building = Some(building.clone());
                    building
                }
            }
        };
        let built =
            match tokio::time::timeout_at(deadline, building.wait_for(Option::is_some)).await {
                Ok(Ok(built)) => (*built).clone(),
                Ok(Err(_)) | Err(_) => None,
            };
        built.unwrap_or(Err(RoomListingError::Deadline))
    }

    /// Start a build on a task of its own; the receiver is told its result.
    fn build(
        self: &Arc<Self>,
        state: &Arc<WebSocketState>,
    ) -> watch::Receiver<Option<CandidatesBuild>> {
        let (told, building) = watch::channel(None);
        let candidates = Arc::clone(self);
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let muc_domain = state.deps.service_domains.muc.clone();
            let deadline = Instant::now() + REMOTE_CANDIDATES_BUILD_LIMIT;
            let built: CandidatesBuild = room_candidates(&state, &muc_domain, deadline)
                .await
                .map(Arc::from);
            {
                let mut held = candidates.held.lock().await;
                held.building = None;
                if let Ok(rooms) = &built {
                    held.built = Some((Instant::now(), Arc::clone(rooms)));
                }
            }
            let _ = told.send(Some(built));
        });
        building
    }
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
/// the `disco#items` of its node, for a session that may read the Space,
/// sorted by item ID. Each is listed as XEP-0060 §5.5 lists a published
/// item, with the service's address and the item ID (the room's JID) as
/// its name, so a client verifies that a room it was told of is part of the
/// Space.
///
/// Only bookmarks of rooms are listed: an item without a payload, or whose
/// payload is no XEP-0402 `conference`, is not a room. A room of this
/// deployment that an enrolled host-grant scope manages is listed only
/// where its canonical grants allow the session `view`, read at most
/// [`GRANT_READS_AT_ONCE`] at a time within [`LISTING_LIMIT`] and confirmed
/// just before the answer is built; one not decided in that time is left
/// out. Every other bookmark is listed as the Space's items show it. Unlike
/// a read of the items, the listing writes nothing.
async fn space_children_for_session(
    state: &WebSocketState,
    spaces_domain: &str,
    node: &str,
    requester: Option<&BareJid>,
) -> Result<Vec<DiscoItem>, SpaceListingRefusal> {
    let Some(requester) = requester else {
        return Err(SpaceListingRefusal::NotFound);
    };
    let deadline = Instant::now() + LISTING_LIMIT;
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
    let mut elsewhere = Vec::new();
    let mut here = Vec::new();
    for item in &stored {
        let item = item.to_pubsub_item();
        let Some(id) = item.id else {
            continue;
        };
        if !item
            .payload
            .as_ref()
            .is_some_and(|payload| payload.is("conference", NS_BOOKMARKS2))
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
            here.push((id, GrantKey::Room(room)));
        } else {
            elsewhere.push(id);
        }
    }
    let gate = &state.deps.app_state.host_grants;
    let mut decided =
        decide_views(gate, here, requester, PrincipalEvidence::NoStream, deadline).await;
    decided.unmanaged.extend(elsewhere);
    let mut ids = decided.still_viewable(gate, deadline).await;
    ids.sort();
    ids.dedup();
    Ok(ids
        .iter()
        .map(|id| DiscoItem::new(spaces_domain, Some(id), None))
        .collect())
}

/// Live rooms created ad hoc through the MUC protocol (no catalog row)
/// that are public and not group DMs, as their actors describe them by
/// `deadline`. Catalog-backed rooms are listed from the catalog, never from
/// their actors: a candidate the catalog check cannot clear is left out.
async fn instant_room_candidates(
    state: &WebSocketState,
    muc_domain: &str,
    channel_backed: &ChannelBackedRooms,
    deadline: Instant,
) -> Vec<ListedRoom> {
    let registry = waddle_xmpp::muc::RoomRegistry::wrap(state.deps.protocol.room_registry.clone());
    let room_jids = match tokio::time::timeout_at(deadline, registry.list_rooms()).await {
        Ok(Ok(room_jids)) => room_jids,
        Ok(Err(error)) => {
            warn!(error = %error, "Failed to list live MUC rooms for disco#items");
            return Vec::new();
        }
        Err(_) => return Vec::new(),
    };
    // Order of operations: (1) the free in-memory filter, and the catalog
    // check when the catalog read was bounded, set every catalog-backed
    // room aside, so they never take the scan's places; (2) sort for a
    // deterministic selection when the bound bites; (3) bound the snapshot
    // asks.
    let mut candidates: Vec<BareJid> = room_jids
        .into_iter()
        .filter(|room_jid| {
            room_jid.domain().as_str() == muc_domain && !channel_backed.jids.contains(room_jid)
        })
        .collect();
    candidates.sort_unstable_by(|a, b| a.as_str().cmp(b.as_str()));
    channel_backed
        .drop_unlisted_channel_rooms(state, &mut candidates, deadline)
        .await;
    candidates.truncate(MAX_INSTANT_ROOM_SCAN);
    // The actors are asked a few at a time, all by `deadline`: a wedged
    // actor costs its own room's place in the listing and no more.
    let mut asks = futures::stream::iter(candidates)
        .map(|room_jid| instant_room(state, room_jid))
        .buffer_unordered(SNAPSHOT_ASKS_AT_ONCE);
    let mut rooms = Vec::new();
    while let Ok(Some(room)) = tokio::time::timeout_at(deadline, asks.next()).await {
        rooms.extend(room);
    }
    rooms
}

/// The live instant room `room_jid` as its actor describes it, when it is
/// public and no group conversation; `None` otherwise and when its actor
/// does not answer.
async fn instant_room(state: &WebSocketState, room_jid: BareJid) -> Option<ListedRoom> {
    let room_actor = get_room_actor(state, &room_jid).await?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .reply_timeout(DISCO_ITEMS_SNAPSHOT_TIMEOUT)
        .await
        .ok()?;
    let config = &snapshot.room.config;
    if !config.public_room || config.group_dm {
        return None;
    }
    let name = if config.name.trim().is_empty() {
        room_jid
            .node()
            .map(|node| node.to_string())
            .unwrap_or_else(|| room_jid.to_string())
    } else {
        config.name.clone()
    };
    Some(ListedRoom {
        channel: waddle_xmpp::parse_managed_room_jid(&room_jid),
        jid: room_jid,
        name,
        catalogued: false,
        federated: config.federated,
    })
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
    /// already happened; a truncated catalog fetch checks every candidate
    /// whose address names a channel, [`CATALOG_CHECK_CHUNK`] to a query,
    /// by `deadline`. A candidate the check cannot clear (a query failed or
    /// ran out of time) is dropped when its address names a channel: such
    /// a room is listed only from its catalog row, never from its actor's
    /// configuration.
    async fn drop_unlisted_channel_rooms(
        &self,
        state: &WebSocketState,
        candidates: &mut Vec<BareJid>,
        deadline: Instant,
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
        let mut backed: HashSet<BareJid> = HashSet::new();
        for chunk in channel_ids.chunks(CATALOG_CHECK_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let rows = state.deps.app_state.db_pool.global_actor().ask(DbQuery {
                sql: format!("SELECT id FROM channels WHERE id IN ({placeholders})"),
                params: chunk
                    .iter()
                    .map(|(_, channel_id)| channel_id.clone().into())
                    .collect(),
            });
            let known: HashSet<String> = match tokio::time::timeout_at(deadline, rows).await {
                Ok(Ok(rows)) => rows
                    .into_iter()
                    .filter_map(|row| match row.into_iter().next() {
                        Some(crate::db::Value::Text(id)) => Some(id),
                        _ => None,
                    })
                    .collect(),
                Ok(Err(error)) => {
                    warn!(error = %error, "Failed batched channel membership check for disco#items");
                    candidates
                        .retain(|room_jid| waddle_xmpp::parse_managed_room_jid(room_jid).is_none());
                    return;
                }
                Err(_) => {
                    candidates
                        .retain(|room_jid| waddle_xmpp::parse_managed_room_jid(room_jid).is_none());
                    return;
                }
            };
            backed.extend(
                chunk
                    .iter()
                    .filter(|(_, channel_id)| known.contains(channel_id))
                    .map(|(room_jid, _)| room_jid.clone()),
            );
        }
        candidates.retain(|room_jid| !backed.contains(room_jid));
    }
}
