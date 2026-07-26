use super::*;
use crate::admin::channels::{acquire_room_config_lock, explicit_channel_affiliations_for_jids};

/// Upper bound on the mutating room-actor asks below. The room actor
/// awaits the durable-membership source inside its affiliation
/// handlers, so a wedged (not dead) permission actor can stall the
/// room's mailbox; without a reply timeout that stall would propagate
/// into this stanza handler and hold the connection's IQ processing
/// hostage. A timeout falls into each match's catch-all `Err` arm
/// (internal-server-error reply). Magnitude matches the
/// `REAPER_ASK_TIMEOUT` precedent in `session_janitors.rs`.
const ADMIN_ROOM_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn admin_item_has_role_shape(item: &AdminItem) -> bool {
    item.role.is_some()
}

fn admin_item_has_affiliation_shape(item: &AdminItem) -> bool {
    item.affiliation.is_some()
}

fn has_mixed_admin_set_semantics(items: &[AdminItem]) -> bool {
    let has_role = items.iter().any(admin_item_has_role_shape);
    let has_affiliation = items.iter().any(admin_item_has_affiliation_shape);
    items
        .iter()
        .any(|item| admin_item_has_role_shape(item) && admin_item_has_affiliation_shape(item))
        || (has_role && has_affiliation)
}

fn has_incomplete_admin_set_item(items: &[AdminItem]) -> bool {
    if is_role_change_query(items) {
        return items
            .iter()
            .any(|item| item.nick.is_none() || item.role.is_none());
    }
    items
        .iter()
        .any(|item| item.jid.is_none() || item.affiliation.is_none())
}

fn channel_affiliation_relation(affiliation: Affiliation) -> Option<&'static str> {
    match affiliation {
        Affiliation::Owner => Some("owner"),
        Affiliation::Admin => Some("admin"),
        Affiliation::Member => Some("member"),
        Affiliation::Outcast => Some("outcast"),
        Affiliation::None => None,
    }
}

fn can_change_affiliation(
    sender_affiliation: Affiliation,
    target_current_affiliation: Affiliation,
    new_affiliation: Affiliation,
) -> bool {
    match new_affiliation {
        Affiliation::Owner | Affiliation::Admin => sender_affiliation == Affiliation::Owner,
        Affiliation::Member | Affiliation::None | Affiliation::Outcast
            if target_current_affiliation == Affiliation::Admin =>
        {
            sender_affiliation == Affiliation::Owner
        }
        Affiliation::Member | Affiliation::None | Affiliation::Outcast => {
            matches!(sender_affiliation, Affiliation::Owner | Affiliation::Admin)
        }
    }
}

pub(in crate::server::routes::websocket::handlers) async fn persist_managed_channel_affiliation(
    state: &WebSocketState,
    channel_id: &str,
    jid: &BareJid,
    affiliation: Affiliation,
) -> Result<(), String> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(jid.to_string());

    for relation in ["owner", "admin", "member", "outcast"] {
        let tuple = Tuple::new(object.clone(), Relation::new(relation), subject.clone());
        match state
            .deps
            .app_state
            .permission_actor
            .ask(DeleteTuple { tuple })
            .await
        {
            Ok(()) | Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => {
            }
            Err(error) => return Err(format!("delete affiliation tuple failed: {error}")),
        }
    }

    let Some(relation) = channel_affiliation_relation(affiliation) else {
        return Ok(());
    };
    let tuple = Tuple::new(object, Relation::new(relation), subject);
    match state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple { tuple })
        .await
    {
        Ok(())
        | Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(format!("write affiliation tuple failed: {error}")),
    }
}

/// Roll back optimistically-persisted affiliation changes after the room
/// actor rejected an admin set.
async fn rollback_admin_affiliations(
    state: &WebSocketState,
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    managed_channel_id: Option<&str>,
    durable_previous_affiliations: &[(BareJid, Affiliation)],
    actor_previous_affiliations: &[(BareJid, Affiliation)],
) {
    let Some(channel_id) = managed_channel_id else {
        return;
    };
    for (previous_jid, previous_affiliation) in durable_previous_affiliations {
        let _ = persist_managed_channel_affiliation(
            state,
            channel_id,
            previous_jid,
            *previous_affiliation,
        )
        .await;
    }
    for (previous_jid, previous_affiliation) in actor_previous_affiliations {
        let _ = room_actor
            .ask(ChangeAffiliation {
                jid: previous_jid.clone(),
                affiliation: *previous_affiliation,
            })
            .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
            .await;
    }
}

pub(super) async fn handle_muc_admin_iq(
    iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let (mut header, payload) = iq.clone().split();
    header.from = Some(Jid::from(sender_jid.clone()));
    let iq_with_from = payload.assemble(header);
    let query = match parse_admin_query(&iq_with_from, muc_domain) {
        Ok(query) => query,
        Err(error) => return vec![build_xmpp_error_response(&iq_with_from, error)],
    };
    let room_actor = match get_room_actor_result(state, &query.room_jid).await {
        Ok(Some(room_actor)) => room_actor,
        Ok(None) => {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        }
        Err(error) => {
            warn!(room = %query.room_jid, %error, "MUC admin room lookup failed");
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    let _config_guard = if query.is_get {
        None
    } else {
        Some(acquire_room_config_lock(&query.room_jid).await)
    };
    let context = match room_actor
        .ask(GetAdminContext {
            sender_jid: sender_jid.clone(),
        })
        .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
        .await
    {
        Ok(context) => context,
        Err(_) => {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    let has_admin_affiliation =
        matches!(context.affiliation, Affiliation::Owner | Affiliation::Admin);
    let is_admin = has_admin_affiliation || matches!(context.role, waddle_xmpp::Role::Moderator);
    // XEP-0045 §9.5: in a non-anonymous room (all Waddle rooms are
    // muc_nonanonymous) any member SHOULD be able to retrieve the
    // member list — even when not currently an occupant. Every other
    // list GET and all admin sets remain admin+ (#1265 item 12).
    let member_list_get = query.is_get
        && !query.items.is_empty()
        && !query.items.iter().any(admin_item_has_role_shape)
        && query
            .items
            .iter()
            .all(|item| item.affiliation == Some(Affiliation::Member))
        && context.affiliation >= Affiliation::Member;
    if !is_admin && !member_list_get {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            forbidden_iq_error("Operation not permitted."),
        )];
    }
    if query.is_get {
        let snapshot = match room_actor
            .ask(GetSnapshot)
            .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
            .await
        {
            Ok(snapshot) => snapshot.room,
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        let to_jid = Jid::from(sender_jid.clone());
        if query.items.iter().any(admin_item_has_role_shape) {
            let role_filter = query.items.iter().find_map(|item| item.role);
            let items: Vec<(String, waddle_xmpp::Role, Affiliation, FullJid)> = snapshot
                .occupants
                .values()
                .filter(|occupant| role_filter.is_none_or(|role| occupant.role == role))
                .map(|occupant| {
                    (
                        occupant.nick.clone(),
                        occupant.role,
                        occupant.affiliation,
                        occupant.real_jid.clone(),
                    )
                })
                .collect();
            return vec![iq_to_xml(build_role_result(
                iq.id(),
                &query.room_jid,
                &to_jid,
                &items,
            ))];
        }
        if !has_admin_affiliation && !member_list_get {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
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
            iq.id(),
            &query.room_jid,
            &to_jid,
            &items,
        ))];
    }
    let room_jid = query.room_jid.clone();
    let items = query.items.clone();
    if has_mixed_admin_set_semantics(&items) {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("MUC admin set cannot mix role and affiliation semantics."),
        )];
    }
    if has_incomplete_admin_set_item(&items) {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Malformed MUC admin set item."),
        )];
    }
    if !is_role_change_query(&items) && !has_admin_affiliation {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            forbidden_iq_error("Operation not permitted."),
        )];
    }
    let affiliation_updates: Vec<(BareJid, Affiliation)> = if is_role_change_query(&items) {
        Vec::new()
    } else {
        items
            .iter()
            .filter_map(|item| item.jid.clone().zip(item.affiliation))
            .collect()
    };
    let actor_previous_affiliations = if affiliation_updates.is_empty() {
        Vec::new()
    } else {
        match room_actor
            .ask(GetSnapshot)
            .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
            .await
        {
            Ok(snapshot) => {
                let mut final_affiliations: std::collections::BTreeMap<BareJid, Affiliation> =
                    snapshot
                        .room
                        .get_all_affiliations()
                        .into_iter()
                        .map(|entry| (entry.jid, entry.affiliation))
                        .collect();
                let current_owner_count = snapshot
                    .room
                    .get_jids_by_affiliation(Affiliation::Owner)
                    .len();
                for (jid, affiliation) in &affiliation_updates {
                    if *affiliation == Affiliation::Owner
                        && context.affiliation != Affiliation::Owner
                    {
                        return vec![build_iq_error_xml_typed(
                            iq.id(),
                            response_from,
                            response_to,
                            forbidden_iq_error("Operation not permitted."),
                        )];
                    }
                    if *affiliation == Affiliation::Outcast && *jid == sender_jid.to_bare() {
                        return vec![build_iq_error_xml_typed(
                            iq.id(),
                            response_from,
                            response_to,
                            conflict_iq_error("Cannot ban yourself from a room."),
                        )];
                    }
                    let previous_affiliation = snapshot.room.get_affiliation(jid);
                    if *affiliation != Affiliation::Owner
                        && previous_affiliation == Affiliation::Owner
                        && context.affiliation == Affiliation::Admin
                    {
                        return vec![build_iq_error_xml_typed(
                            iq.id(),
                            response_from,
                            response_to,
                            not_allowed_iq_error("Admins cannot change an owner's affiliation."),
                        )];
                    }
                    if !can_change_affiliation(
                        context.affiliation,
                        previous_affiliation,
                        *affiliation,
                    ) {
                        return vec![build_iq_error_xml_typed(
                            iq.id(),
                            response_from,
                            response_to,
                            forbidden_iq_error("Operation not permitted."),
                        )];
                    }
                    if *affiliation == Affiliation::None {
                        final_affiliations.remove(jid);
                    } else {
                        final_affiliations.insert(jid.clone(), *affiliation);
                    }
                }
                if current_owner_count > 0
                    && !final_affiliations
                        .values()
                        .any(|affiliation| *affiliation == Affiliation::Owner)
                {
                    return vec![build_iq_error_xml_typed(
                        iq.id(),
                        response_from,
                        response_to,
                        conflict_iq_error("Cannot remove the last owner from a room."),
                    )];
                }
                let mut previous_affiliations = Vec::with_capacity(affiliation_updates.len());
                for (jid, _) in &affiliation_updates {
                    previous_affiliations.push((jid.clone(), snapshot.room.get_affiliation(jid)));
                }
                previous_affiliations
            }
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }
    };
    let managed_channel_id = waddle_xmpp::parse_managed_room_jid(&room_jid);
    let durable_previous_affiliations = if affiliation_updates.is_empty() {
        Vec::new()
    } else if let Some(channel_id) = managed_channel_id.as_deref() {
        let target_jids: Vec<BareJid> = affiliation_updates
            .iter()
            .map(|(jid, _)| jid.clone())
            .collect();
        match explicit_channel_affiliations_for_jids(&state.deps.app_state, channel_id, target_jids)
            .await
        {
            Ok(affiliations) => affiliations,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = %error,
                    "Failed to snapshot explicit channel affiliations before MUC admin update"
                );
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }
    } else {
        Vec::new()
    };
    if let Some(channel_id) = managed_channel_id.as_deref() {
        for (jid, affiliation) in &affiliation_updates {
            if let Err(error) =
                persist_managed_channel_affiliation(state, channel_id, jid, *affiliation).await
            {
                warn!(
                    room = %room_jid,
                    target = %jid,
                    error = %error,
                    "Failed to persist MUC admin affiliation change before actor update"
                );
                for (previous_jid, previous_affiliation) in &durable_previous_affiliations {
                    let _ = persist_managed_channel_affiliation(
                        state,
                        channel_id,
                        previous_jid,
                        *previous_affiliation,
                    )
                    .await;
                }
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }
    }
    let applied = match room_actor
        .ask(ApplyAdminItems {
            sender_jid: sender_jid.clone(),
            sender_affiliation: context.affiliation,
            sender_role: context.role,
            items,
        })
        .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
        .await
    {
        Ok(applied) => applied,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::CannotRemoveLastOwner,
        )) => {
            warn!(room = %room_jid, "MUC admin set rejected because it would remove the last owner");
            rollback_admin_affiliations(
                state,
                &room_actor,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
                &actor_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                conflict_iq_error("Cannot remove the last owner from a room."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::CannotAdminModifyOwner,
        )) => {
            warn!(room = %room_jid, "MUC admin set rejected because an admin tried to change an owner affiliation");
            rollback_admin_affiliations(
                state,
                &room_actor,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
                &actor_previous_affiliations,
            )
            .await;
            // XEP-0045 §9.2: the denial returns <not-allowed/> "along
            // with the offending item(s)".
            return vec![build_iq_error_xml_with_payload(
                iq.id(),
                response_from,
                response_to,
                waddle_xmpp::muc::admin::build_admin_items_query(&query.items),
                not_allowed_iq_error("Admins cannot change an owner's affiliation."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::CannotModifyPrivilegedRole,
        )) => {
            warn!(room = %room_jid, "MUC admin set rejected because a non-owner tried to change an owner/admin role");
            rollback_admin_affiliations(
                state,
                &room_actor,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
                &actor_previous_affiliations,
            )
            .await;
            // XEP-0045 §8.4/§9.7: the denial returns <not-allowed/>
            // "along with the offending item(s)".
            return vec![build_iq_error_xml_with_payload(
                iq.id(),
                response_from,
                response_to,
                waddle_xmpp::muc::admin::build_admin_items_query(&query.items),
                not_allowed_iq_error("Admins and moderators cannot change an owner or admin role."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::OccupantNotFound(nick),
        )) => {
            // XEP-0045 §8.2: kicking a nick that is not in the room is
            // <item-not-found/>, not <forbidden/> (#1265 item 16).
            warn!(room = %room_jid, nick = %nick, "MUC admin set targeted an absent occupant");
            rollback_admin_affiliations(
                state,
                &room_actor,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
                &actor_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                item_not_found_iq_error("No such occupant in this room."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(error)) => {
            warn!(room = %room_jid, error = %error, "MUC admin set rejected");
            rollback_admin_affiliations(
                state,
                &room_actor,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
                &actor_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
        }
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to apply MUC admin IQ");
            rollback_admin_affiliations(
                state,
                &room_actor,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
                &actor_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    // XEP-0045 §8.2/§9.1 stanza ordering (#1265 item 6): the kicked/
    // banned occupant's unavailable presence goes out first, then the
    // moderator's IQ result, then the broadcast to remaining occupants.
    // The moderator's own broadcast copy rides this connection's
    // response frames AFTER the IQ result so the order is deterministic
    // on the moderator's stream.
    let mut remaining_broadcasts = Vec::new();
    let mut self_kick_frames = Vec::new();
    let mut moderator_frames = Vec::new();
    for (recipient, presence) in applied.presence_updates {
        if applied.removed_by_moderation.contains(&recipient) {
            if recipient == *sender_jid {
                // Self-kick: the kickee IS this connection, so its
                // presence must ride the response frames BEFORE the IQ
                // result — a registry send could otherwise land after
                // the frame write and invert the §8.2 order.
                self_kick_frames.push(stanza_to_xml(&Stanza::Presence(presence)));
            } else {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&recipient, Stanza::Presence(presence))
                    .await;
            }
        } else if recipient == *sender_jid {
            moderator_frames.push(stanza_to_xml(&Stanza::Presence(presence)));
        } else {
            remaining_broadcasts.push((recipient, presence));
        }
    }
    // Membership-scoped visibility (#935): a kick (307) or ban (301)
    // ends the occupant's room membership, so their live SFU call
    // participation must end with it. Presence loss never reaches
    // this handler, and eviction failure is fire-and-forget inside
    // the SFU layer — the moderation IQ result below is never
    // blocked on LiveKit.
    for removed in &applied.removed_by_moderation {
        super::super::super::muc_call_sfu::unregister_participant_from_room(
            state, &room_jid, removed,
        );
    }
    // Role-derived media grants: a non-removal role change (voice
    // grant/revoke, moderator grant/revoke) must converge the
    // target's live SFU permission with the role the room actor just
    // applied — otherwise a voice-revoked visitor keeps publishing
    // until they renegotiate. Fire-and-forget like the eviction above.
    for (session, new_role) in &applied.role_changes {
        super::super::super::muc_call_sfu::apply_role_grants_for_room(
            state, &room_jid, session, *new_role,
        );
    }
    for (recipient, presence) in remaining_broadcasts {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&recipient, Stanza::Presence(presence))
            .await;
    }
    let mut frames = self_kick_frames;
    frames.push(iq_to_xml(build_admin_set_result(
        iq.id(),
        &room_jid,
        &Jid::from(sender_jid.clone()),
    )));
    frames.extend(moderator_frames);
    frames
}
