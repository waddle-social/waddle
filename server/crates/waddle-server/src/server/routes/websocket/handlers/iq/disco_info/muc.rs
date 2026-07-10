use super::*;

use waddle_xmpp::muc::roominfo::MucRoomInfo;

/// XEP-0045 §7.12 well-known disco node for reserved-nick discovery.
const NODE_ROOMUSER_ITEM: &str = "x-roomuser-item";

/// XEP-0030 routing classification for a disco#info target as seen by the
/// MUC dispatcher.
///
/// The MUC handler must only consult the `RoomRegistryActor` for targets that
/// are actually hosted on the MUC service domain. Any other target (a sibling
/// component such as `upload.<domain>`, or a JID on a non-MUC domain) is
/// `NotMuc` and the handler defers to the next handler **without** touching the
/// room registry — see #757, where a wedged `RoomRegistryActor` froze
/// disco#info for every component because the MUC handler looked rooms up
/// unconditionally.
#[derive(Debug, PartialEq)]
enum MucDiscoTarget {
    /// The MUC service JID itself (`muc.<domain>`).
    Service,
    /// A room hosted on the MUC service (`<node>@muc.<domain>`).
    Room(BareJid),
    /// Not a MUC target — the dispatcher returns `None`.
    NotMuc,
}

/// Decide how the MUC dispatcher should treat a disco#info target, without any
/// I/O. Pure so the routing decision is verifiable in isolation.
fn classify_muc_disco_target(target_to: Option<&str>, muc_domain: &str) -> MucDiscoTarget {
    if target_to == Some(muc_domain) {
        return MucDiscoTarget::Service;
    }
    let Some(target) = target_to else {
        return MucDiscoTarget::NotMuc;
    };
    let room_target = target.split('/').next().unwrap_or(target);
    let Ok(room_jid) = room_target.parse::<BareJid>() else {
        return MucDiscoTarget::NotMuc;
    };
    if room_jid.domain().as_str() != muc_domain {
        return MucDiscoTarget::NotMuc;
    }
    MucDiscoTarget::Room(room_jid)
}

fn bare_group_dm_room_target(target_to: Option<&str>, muc_domain: &str) -> Option<BareJid> {
    target_to
        .filter(|target| !target.contains('/'))
        .and_then(|target| target.parse::<BareJid>().ok())
        .filter(|room_jid| room_jid.domain().as_str() == muc_domain)
}

fn group_dm_shared_name(name: &str) -> Option<&str> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

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
                    "Failed to load room snapshot for command disco#info"
                );
                return RoomCommandTarget::Failed;
            }
        };
        return if snapshot.config.group_dm {
            if snapshot.get_affiliation(&requester_bare) >= Affiliation::Member {
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
                RoomCommandTarget::GroupDm
            } else {
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
                            "Failed to check persisted group-DM membership for command disco#info"
                        );
                        RoomCommandTarget::Failed
                    }
                    Ok(response) if response.allowed => RoomCommandTarget::GroupDm,
                    Ok(_) => RoomCommandTarget::NotMember,
                    Err(error) => {
                        warn!(
                            room = %room_jid,
                            error = ?error,
                            "Failed to ask permission actor for command disco#info"
                        );
                        RoomCommandTarget::Failed
                    }
                }
            }
        }
        Ok(Some(_)) => RoomCommandTarget::NotGroupDm,
        Ok(None) => RoomCommandTarget::Missing,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Failed to load persisted room for command disco#info"
            );
            RoomCommandTarget::Failed
        }
    }
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

/// Compose a room's disco#info extension forms with exactly one
/// `muc#roominfo` FORM_TYPE (#1259, XEP-0115 §5.4): the optional
/// XEP-0503 parent form, the single merged `muc#roominfo` form
/// (description + XEP-0500 slow-mode duration + optional space link),
/// and the Waddle room-metadata form. Pushes `Feature::spaces()` when
/// the room is space-linked.
fn room_disco_extension_forms(
    features: &mut Vec<Feature>,
    space_link: Option<RoomSpaceLink>,
    description: Option<&str>,
    room_metadata_form: Element,
) -> Vec<Element> {
    let mut extensions = Vec::new();
    let mut room_info = MucRoomInfo {
        description: description.map(str::to_string),
        ..MucRoomInfo::default()
    };
    if let Some(link) = space_link {
        features.push(Feature::spaces());
        extensions.push(link.parent_form);
        room_info.space_pubsub_iri = Some(link.pubsub_iri);
    }
    extensions.push(room_info.to_form_element());
    extensions.push(room_metadata_form);
    extensions
}

pub(super) async fn handle_muc_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    // XEP-0045 §6.6 (#1265 item 10): disco addressed to an occupant JID
    // (`room@service/nick`) is rejected — <bad-request/> for
    // non-occupants (MUST), <feature-not-implemented/> for occupants
    // (pass-through unsupported) — instead of leaking room info by
    // silently stripping the resource.
    if let Some(error) = super::super::muc_occupant_disco::muc_occupant_disco_error(
        state,
        req.target_to,
        req.muc_domain,
        req.requester,
    )
    .await
    {
        return Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            error,
        ));
    }

    // XEP-0045 §7.12 (#1265 item 9): reserved-nick discovery. Waddle
    // locks nicknames to identity, so a user's "reserved" nick is the
    // nick they currently occupy the room under; with no occupancy
    // there is no registered nickname and the query element is empty
    // per XEP-0030.
    if req.node == Some(NODE_ROOMUSER_ITEM) {
        let MucDiscoTarget::Room(room_jid) =
            classify_muc_disco_target(req.target_to, req.muc_domain)
        else {
            return None;
        };
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            // A dormant channel-backed room exists but has no occupants,
            // so the requester has no reserved nick: empty query per
            // XEP-0030. A room with neither actor nor channel record
            // does not exist (#1260).
            if let Ok(Some(_)) = get_managed_channel_for_room(state, &room_jid).await {
                let response =
                    build_disco_info_response(req.request_iq, &[], &[], Some(NODE_ROOMUSER_ITEM));
                return Some(DiscoInfoResponse::iq(response));
            }
            return Some(DiscoInfoResponse::error(
                req.id,
                req.response_from,
                req.response_to,
                item_not_found_iq_error("Requested item not found."),
            ));
        };
        let reserved_nick = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => req.requester.and_then(|requester| {
                snapshot
                    .room
                    .find_nick_by_real_jid(requester)
                    .map(str::to_string)
            }),
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "Failed to load room snapshot for reserved-nick disco"
                );
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    internal_server_error_iq_error("Internal server error."),
                ));
            }
        };
        let identities: Vec<Identity> = reserved_nick
            .as_deref()
            .map(|nick| vec![Identity::muc_room(Some(nick))])
            .unwrap_or_default();
        let response =
            build_disco_info_response(req.request_iq, &identities, &[], Some(NODE_ROOMUSER_ITEM));
        return Some(DiscoInfoResponse::iq(response));
    }

    if matches!(
        req.node,
        Some(NODE_COMMANDS) | Some(waddle_xmpp::admin::NS_GROUP_DM_RENAME)
    ) {
        let room_jid = bare_group_dm_room_target(req.target_to, req.muc_domain)?;
        match classify_room_command_target(state, &room_jid, req.requester).await {
            RoomCommandTarget::GroupDm => {}
            RoomCommandTarget::NotGroupDm
            | RoomCommandTarget::NotMember
            | RoomCommandTarget::Missing => {
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    item_not_found_iq_error("Requested item not found."),
                ));
            }
            RoomCommandTarget::Failed => {
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    internal_server_error_iq_error("Internal server error."),
                ));
            }
        }

        if req.node == Some(NODE_COMMANDS) {
            let identities = vec![Identity::command_list(Some("Group DM Commands"))];
            let features = vec![
                Feature::disco_info(),
                Feature::disco_items(),
                Feature::commands(),
            ];
            let response = build_disco_info_response(
                req.request_iq,
                &identities,
                &features,
                Some(NODE_COMMANDS),
            );
            return Some(DiscoInfoResponse::iq(response));
        }

        let commands = state.deps.protocol.command_registry.list_commands().await;
        if let Some(name) = command_name_by_boundary(
            &commands,
            waddle_xmpp::admin::NS_GROUP_DM_RENAME,
            CommandBoundary::MucRoom,
        ) {
            let identities = vec![Identity::automation(Some(name))];
            let features = vec![
                Feature::disco_info(),
                Feature::commands(),
                Feature::new(DATA_FORMS_NS),
            ];
            let response = build_disco_info_response(
                req.request_iq,
                &identities,
                &features,
                Some(waddle_xmpp::admin::NS_GROUP_DM_RENAME),
            );
            return Some(DiscoInfoResponse::iq(response));
        }
        return Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            item_not_found_iq_error("Requested item not found."),
        ));
    }

    // #757: only consult the RoomRegistryActor for targets actually on the
    // MUC service domain. A sibling component (`upload.<domain>`, …) or a JID
    // on any non-MUC domain is `NotMuc` and we defer to the next handler
    // without any room-registry I/O — otherwise a wedged actor freezes
    // disco#info for every component, not just MUC.
    let room_jid = match classify_muc_disco_target(req.target_to, req.muc_domain) {
        MucDiscoTarget::Service => {
            let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
            let mut features = muc_service_features();
            features.push(Feature::replies());
            features.push(Feature::new(NS_CHANNEL_SEARCH));
            features.extend(extension_features_for_disco(state));
            let response = build_disco_info_response(req.request_iq, &identities, &features, None);
            return Some(DiscoInfoResponse::iq(response));
        }
        MucDiscoTarget::NotMuc => return None,
        MucDiscoTarget::Room(room_jid) => room_jid,
    };

    if let Some(room_actor) = get_room_actor(state, &room_jid).await {
        let snapshot = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => snapshot.room,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "Failed to load room snapshot for disco#info"
                );
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    internal_server_error_iq_error("Internal server error."),
                ));
            }
        };
        let managed_channel = get_managed_channel_for_room(state, &room_jid)
            .await
            .ok()
            .flatten();
        let channel_type = managed_channel
            .as_ref()
            .map(|channel| channel.channel_type.as_str())
            .unwrap_or(if snapshot.config.forum {
                "forum"
            } else {
                "text"
            });
        let description = managed_channel
            .as_ref()
            .and_then(|channel| channel.description.as_deref())
            .or(snapshot.config.description.as_deref());
        let room_name = if snapshot.config.group_dm {
            group_dm_shared_name(&snapshot.config.name)
        } else {
            Some(snapshot.config.name.as_str())
        };
        let identities = vec![Identity::muc_room(room_name)];
        let mut features = muc_room_features(
            snapshot.config.persistent,
            snapshot.config.members_only,
            snapshot.config.public_room,
            snapshot.config.moderated || channel_type == "announcement",
            snapshot.config.forum || channel_type == "forum",
        );
        if snapshot.config.group_dm {
            features.push(Feature::new(waddle_xmpp::admin::NS_GROUP_DM_FEATURE));
        }
        features.extend(extension_features_for_disco(state));
        let space_link = room_space_link(state, &room_jid).await;
        let extensions = room_disco_extension_forms(
            &mut features,
            space_link,
            description,
            build_room_metadata_form(channel_type, snapshot.config.pin_permission.as_form_value()),
        );
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            None,
            &extensions,
        );
        return Some(DiscoInfoResponse::iq(response));
    }

    if !is_muc_room_jid(state, &room_jid).await {
        return None;
    }

    if let Ok(Some(channel)) = get_managed_channel_for_room(state, &room_jid).await {
        let room_name = if channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
            group_dm_shared_name(&channel.name)
        } else {
            Some(channel.name.as_str())
        };
        let identities = vec![Identity::muc_room(room_name)];
        let mut features = muc_room_features(
            true,
            channel.members_only,
            channel.public_room,
            channel.channel_type == "announcement",
            channel.channel_type == "forum",
        );
        if channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
            features.push(Feature::new(waddle_xmpp::admin::NS_GROUP_DM_FEATURE));
        }
        features.extend(extension_features_for_disco(state));
        let space_link = room_space_link(state, &room_jid).await;
        // #422: read the persisted pin policy from the channel record so
        // dormant rooms advertise the truth, not the default.
        let extensions = room_disco_extension_forms(
            &mut features,
            space_link,
            channel.description.as_deref(),
            build_room_metadata_form(
                &channel.channel_type,
                channel.pin_permission.as_form_value(),
            ),
        );
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            None,
            &extensions,
        );
        return Some(DiscoInfoResponse::iq(response));
    }

    // #1260 / XEP-0045 §6.4: a localpart with no live actor and no
    // persisted channel record is a room that does not exist — return
    // <item-not-found/> instead of fabricating an open-room response.
    Some(DiscoInfoResponse::error(
        req.id,
        req.response_from,
        req.response_to,
        item_not_found_iq_error("Requested item not found."),
    ))
}

#[cfg(test)]
mod tests {
    use super::{classify_muc_disco_target, MucDiscoTarget};

    const MUC_DOMAIN: &str = "muc.test.local";

    #[test]
    fn bare_muc_service_domain_is_service() {
        assert_eq!(
            classify_muc_disco_target(Some(MUC_DOMAIN), MUC_DOMAIN),
            MucDiscoTarget::Service
        );
    }

    #[test]
    fn room_on_muc_domain_is_room() {
        let target = "general@muc.test.local";
        assert_eq!(
            classify_muc_disco_target(Some(target), MUC_DOMAIN),
            MucDiscoTarget::Room(target.parse().expect("valid room jid"))
        );
    }

    #[test]
    fn room_with_resource_strips_to_bare_room() {
        assert_eq!(
            classify_muc_disco_target(Some("general@muc.test.local/alice"), MUC_DOMAIN),
            MucDiscoTarget::Room("general@muc.test.local".parse().expect("valid room jid"))
        );
    }

    // #757: a sibling component domain must NOT be routed through the MUC
    // handler — otherwise its disco#info is coupled to RoomRegistryActor health.
    #[test]
    fn sibling_component_domain_is_not_muc() {
        assert_eq!(
            classify_muc_disco_target(Some("upload.test.local"), MUC_DOMAIN),
            MucDiscoTarget::NotMuc
        );
    }

    // #757: a node JID hosted on a non-MUC domain must not reach the room
    // registry either — the MUC handler only owns the MUC service domain.
    #[test]
    fn node_jid_on_other_domain_is_not_muc() {
        assert_eq!(
            classify_muc_disco_target(Some("room@chat.test.local"), MUC_DOMAIN),
            MucDiscoTarget::NotMuc
        );
    }

    #[test]
    fn absent_target_is_not_muc() {
        assert_eq!(
            classify_muc_disco_target(None, MUC_DOMAIN),
            MucDiscoTarget::NotMuc
        );
    }
}
