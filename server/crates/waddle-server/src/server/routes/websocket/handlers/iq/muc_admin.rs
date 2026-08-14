use super::*;
use crate::admin::channels::{acquire_room_config_lock, explicit_channel_affiliations_for_jids};
use crate::server::routes::websocket::handlers::iq::errors::resource_constraint_iq_error;

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

/// Roll back optimistically-persisted channel tuples after the room actor
/// rejected an admin set. The actor batch itself is durable-first and
/// all-or-nothing now, so there is no room-memory compensation here.
async fn rollback_admin_affiliations(
    state: &WebSocketState,
    managed_channel_id: Option<&str>,
    durable_previous_affiliations: &[(BareJid, Affiliation)],
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
}

/// A snapshot queued after an `ApplyAdminItems` ask is ordered after that
/// mutation in the room actor's mailbox. It can therefore distinguish a
/// delivered-and-applied affiliation set from a failed delivery before the
/// optimistic managed-channel projection is rolled back.
fn affiliation_updates_match_room(
    room: &waddle_xmpp::muc::MucRoom,
    affiliation_updates: &[(BareJid, Affiliation)],
) -> bool {
    affiliation_updates
        .iter()
        .all(|(jid, affiliation)| room.get_affiliation(jid) == *affiliation)
}

fn role_updates_match_room(
    before: &waddle_xmpp::muc::MucRoom,
    after: &waddle_xmpp::muc::MucRoom,
    items: &[AdminItem],
) -> bool {
    items.iter().all(|item| {
        let (Some(target_nick), Some(new_role)) = (item.nick.as_ref(), item.role) else {
            return false;
        };
        let Some(before_occupant) = before.get_occupant(target_nick) else {
            return false;
        };
        match new_role {
            waddle_xmpp::Role::None => after.get_occupant(target_nick).is_none(),
            _ => after
                .get_occupant(target_nick)
                .is_some_and(|after_occupant| {
                    after_occupant.real_jid == before_occupant.real_jid
                        && after_occupant.affiliation == before_occupant.affiliation
                        && after_occupant.role == new_role
                }),
        }
    })
}

fn admin_items_match_room(
    before: &waddle_xmpp::muc::MucRoom,
    after: &waddle_xmpp::muc::MucRoom,
    items: &[AdminItem],
) -> bool {
    if is_role_change_query(items) {
        role_updates_match_room(before, after, items)
    } else {
        let affiliation_updates: Vec<(BareJid, Affiliation)> = items
            .iter()
            .filter_map(|item| item.jid.clone().zip(item.affiliation))
            .collect();
        affiliation_updates_match_room(after, &affiliation_updates)
    }
}

fn all_room_sessions(room: &waddle_xmpp::muc::MucRoom) -> Vec<FullJid> {
    room.occupants
        .values()
        .flat_map(|occupant| room.get_occupant_sessions(&occupant.nick))
        .collect()
}

fn occupants_for_bare(
    room: &waddle_xmpp::muc::MucRoom,
    target_jid: &BareJid,
) -> Vec<waddle_xmpp::muc::Occupant> {
    room.occupants
        .values()
        .filter(|occupant| occupant.real_jid.to_bare() == *target_jid)
        .cloned()
        .collect()
}

fn session_voices(
    room: &waddle_xmpp::muc::MucRoom,
    target_jid: &BareJid,
) -> Vec<(FullJid, waddle_xmpp::Voice)> {
    let moderation = room.moderation();
    occupants_for_bare(room, target_jid)
        .into_iter()
        .flat_map(|occupant| {
            let voice = occupant.role.voice(moderation);
            room.get_occupant_sessions(&occupant.nick)
                .into_iter()
                .map(move |session| (session, voice))
        })
        .collect()
}

fn changed_session_voices(
    before: &[(FullJid, waddle_xmpp::Voice)],
    after: &[(FullJid, waddle_xmpp::Voice)],
) -> Vec<(FullJid, waddle_xmpp::Voice)> {
    let before_map: std::collections::BTreeMap<FullJid, waddle_xmpp::Voice> =
        before.iter().cloned().collect();
    let after_map: std::collections::BTreeMap<FullJid, waddle_xmpp::Voice> =
        after.iter().cloned().collect();
    after_map
        .into_iter()
        .filter(|(session, voice)| before_map.get(session) != Some(voice))
        .collect()
}

fn replay_affiliation_change_from_snapshot(
    room: &mut waddle_xmpp::muc::MucRoom,
    occupant_id_secret: &waddle_xmpp::xep::xep0421::OccupantIdSecret,
    target_jid: BareJid,
    new_affiliation: Affiliation,
    actor: &BareJid,
    reason: Option<&str>,
) -> waddle_xmpp::muc::room_actor::AdminItemsApplied {
    let voices_before = session_voices(room, &target_jid);
    if room
        .set_affiliation(target_jid.clone(), new_affiliation)
        .is_none()
    {
        return waddle_xmpp::muc::room_actor::AdminItemsApplied::default();
    }
    let affected_occupants = occupants_for_bare(room, &target_jid);
    if affected_occupants.is_empty() {
        return waddle_xmpp::muc::room_actor::AdminItemsApplied::default();
    }

    if new_affiliation == Affiliation::Outcast {
        let mut presence_updates = Vec::new();
        let mut removed_by_moderation = Vec::new();
        for occupant in &affected_occupants {
            let from_room_jid = room
                .room_jid
                .with_resource_str(&occupant.nick)
                .expect("nick was previously accepted as resource");
            let removed_sessions = room.get_occupant_sessions(&occupant.nick);
            let occupant_bare = occupant.real_jid.to_bare();
            presence_updates.extend(all_room_sessions(room).into_iter().map(|recipient| {
                let occupant_identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
                    bare_jid: &occupant_bare,
                    real_jid: Some(&occupant.real_jid),
                    secret: occupant_id_secret,
                };
                let is_self = removed_sessions.iter().any(|jid| jid == &recipient);
                let presence = waddle_xmpp::muc::build_ban_presence(
                    &from_room_jid,
                    &recipient,
                    waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                    reason,
                    Some(actor),
                    &occupant_identity,
                );
                (recipient, presence)
            }));
            removed_by_moderation.extend(removed_sessions);
        }
        for occupant in affected_occupants {
            room.remove_occupant(&occupant.nick);
        }
        return waddle_xmpp::muc::room_actor::AdminItemsApplied {
            presence_updates,
            removed_by_moderation,
            voice_changes: Vec::new(),
        };
    }

    if room.config.members_only && new_affiliation < Affiliation::Member {
        let mut presence_updates = Vec::new();
        let removed_by_moderation: Vec<FullJid> = affected_occupants
            .iter()
            .flat_map(|occupant| room.get_occupant_sessions(&occupant.nick))
            .collect();
        for occupant in &affected_occupants {
            let from_room_jid = room
                .room_jid
                .with_resource_str(&occupant.nick)
                .expect("nick was previously accepted as resource");
            let removed_sessions = room.get_occupant_sessions(&occupant.nick);
            let occupant_bare = occupant.real_jid.to_bare();
            presence_updates.extend(all_room_sessions(room).into_iter().map(|recipient| {
                let occupant_identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
                    bare_jid: &occupant_bare,
                    real_jid: Some(&occupant.real_jid),
                    secret: occupant_id_secret,
                };
                let is_self = removed_sessions.iter().any(|jid| jid == &recipient);
                let presence = waddle_xmpp::muc::build_membership_removal_presence(
                    &from_room_jid,
                    &recipient,
                    "321",
                    waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                    Some(actor),
                    &occupant_identity,
                );
                (recipient, presence)
            }));
        }
        for occupant in affected_occupants {
            room.remove_occupant(&occupant.nick);
        }
        return waddle_xmpp::muc::room_actor::AdminItemsApplied {
            presence_updates,
            removed_by_moderation,
            voice_changes: Vec::new(),
        };
    }

    let mut presence_updates = Vec::new();
    for occupant in &affected_occupants {
        let from_room_jid = room
            .room_jid
            .with_resource_str(&occupant.nick)
            .expect("nick was previously accepted as resource");
        let affected_sessions = room.get_occupant_sessions(&occupant.nick);
        let occupant_bare = occupant.real_jid.to_bare();
        presence_updates.extend(all_room_sessions(room).into_iter().map(|recipient| {
            let occupant_identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
                bare_jid: &occupant_bare,
                real_jid: Some(&occupant.real_jid),
                secret: occupant_id_secret,
            };
            let is_self = affected_sessions.iter().any(|jid| jid == &recipient);
            let presence = waddle_xmpp::muc::build_affiliation_change_presence(
                &from_room_jid,
                &recipient,
                new_affiliation,
                occupant.role,
                waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                &occupant_identity,
            );
            (recipient, presence)
        }));
    }

    waddle_xmpp::muc::room_actor::AdminItemsApplied {
        presence_updates,
        removed_by_moderation: Vec::new(),
        voice_changes: changed_session_voices(&voices_before, &session_voices(room, &target_jid)),
    }
}

fn replay_role_change_from_snapshot(
    room: &mut waddle_xmpp::muc::MucRoom,
    occupant_id_secret: &waddle_xmpp::xep::xep0421::OccupantIdSecret,
    target_nick: &str,
    new_role: waddle_xmpp::Role,
    actor: &BareJid,
    reason: Option<&str>,
) -> waddle_xmpp::muc::room_actor::AdminItemsApplied {
    let Some(target_occupant) = room.get_occupant(target_nick).cloned() else {
        return waddle_xmpp::muc::room_actor::AdminItemsApplied::default();
    };
    let from_room_jid = room
        .room_jid
        .with_resource_str(target_nick)
        .expect("nick was previously accepted as resource");
    let target_bare = target_occupant.real_jid.to_bare();
    let target_sessions = room.get_occupant_sessions(target_nick);
    if new_role == waddle_xmpp::Role::None {
        let mut presence_updates = Vec::new();
        for recipient in all_room_sessions(room) {
            let target_identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
                bare_jid: &target_bare,
                real_jid: Some(&target_occupant.real_jid),
                secret: occupant_id_secret,
            };
            let is_self = target_sessions.iter().any(|jid| jid == &recipient);
            let presence = waddle_xmpp::muc::build_kick_presence(
                &from_room_jid,
                &recipient,
                target_occupant.affiliation,
                waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                reason,
                Some(actor),
                &target_identity,
            );
            presence_updates.push((recipient, presence));
        }
        room.remove_occupant(target_nick);
        return waddle_xmpp::muc::room_actor::AdminItemsApplied {
            presence_updates,
            removed_by_moderation: target_sessions,
            voice_changes: Vec::new(),
        };
    }

    let before_voice = target_occupant.role.voice(room.moderation());
    if let Some(occupant) = room.occupants.get_mut(target_nick) {
        occupant.role = new_role;
    }
    let new_voice = new_role.voice(room.moderation());
    let voice_changes: Vec<_> = if before_voice != new_voice {
        target_sessions
            .iter()
            .cloned()
            .map(|session| (session, new_voice))
            .collect()
    } else {
        Vec::new()
    };
    let mut presence_updates = Vec::new();
    for recipient in all_room_sessions(room) {
        let target_identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &target_bare,
            real_jid: Some(&target_occupant.real_jid),
            secret: occupant_id_secret,
        };
        let is_self = target_sessions.iter().any(|jid| jid == &recipient);
        let presence = waddle_xmpp::muc::build_role_change_presence(
            &from_room_jid,
            &recipient,
            target_occupant.affiliation,
            new_role,
            waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
            &target_identity,
        );
        presence_updates.push((recipient, presence));
    }
    waddle_xmpp::muc::room_actor::AdminItemsApplied {
        presence_updates,
        removed_by_moderation: Vec::new(),
        voice_changes,
    }
}

fn recover_committed_admin_effects(
    room: &waddle_xmpp::muc::MucRoom,
    items: &[AdminItem],
    sender_jid: &FullJid,
    occupant_id_secret: &waddle_xmpp::xep::xep0421::OccupantIdSecret,
) -> waddle_xmpp::muc::room_actor::AdminItemsApplied {
    let mut replay_room = room.clone();
    let actor = sender_jid.to_bare();
    let mut recovered = waddle_xmpp::muc::room_actor::AdminItemsApplied::default();
    for item in items {
        let applied = if let (Some(target_jid), Some(new_affiliation)) =
            (item.jid.clone(), item.affiliation)
        {
            replay_affiliation_change_from_snapshot(
                &mut replay_room,
                occupant_id_secret,
                target_jid,
                new_affiliation,
                &actor,
                item.reason.as_deref(),
            )
        } else if let (Some(target_nick), Some(new_role)) = (item.nick.as_deref(), item.role) {
            replay_role_change_from_snapshot(
                &mut replay_room,
                occupant_id_secret,
                target_nick,
                new_role,
                &actor,
                item.reason.as_deref(),
            )
        } else {
            continue;
        };
        recovered.presence_updates.extend(applied.presence_updates);
        recovered
            .removed_by_moderation
            .extend(applied.removed_by_moderation);
        recovered.voice_changes.extend(applied.voice_changes);
    }
    recovered
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
    let pre_apply_snapshot = if is_role_change_query(&items) || !affiliation_updates.is_empty() {
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
                Some(snapshot)
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
    } else {
        None
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
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
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
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
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
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
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
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                item_not_found_iq_error("No such occupant in this room."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::NotOwner,
        )) => {
            warn!(room = %room_jid, "MUC admin set hit a deposed room actor");
            rollback_admin_affiliations(
                state,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            let _ = state
                .deps
                .protocol
                .room_registry
                .ask(
                    waddle_xmpp::muc::room_registry_actor::DemoteRoomIfExactActor {
                        room_jid: room_jid.clone(),
                        actor_ref: room_actor.clone(),
                    },
                )
                .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                resource_constraint_iq_error("This room is temporarily unavailable; please retry."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::OwnershipUnavailable,
        )) => {
            warn!(room = %room_jid, "MUC admin ownership verification is temporarily unavailable");
            rollback_admin_affiliations(
                state,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                resource_constraint_iq_error(
                    "This room's ownership cannot be verified right now; please retry.",
                ),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::PersistFailed,
        )) => {
            warn!(room = %room_jid, "MUC admin durable commit failed before apply");
            rollback_admin_affiliations(
                state,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::InviteRollbackPending,
        )) => {
            // This is an explicit wait state, not a permission denial. The
            // actor did not apply the mutation, so restoring the optimistic
            // managed-channel projection is safe.
            rollback_admin_affiliations(
                state,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                resource_constraint_iq_error(
                    "This room's invitation state is being reconciled; please retry.",
                ),
            )];
        }
        Err(kameo::error::SendError::HandlerError(error)) => {
            warn!(room = %room_jid, error = %error, "MUC admin set rejected");
            rollback_admin_affiliations(
                state,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
        }
        Err(
            error @ (kameo::error::SendError::ActorNotRunning(_)
            | kameo::error::SendError::MailboxFull(_)
            | kameo::error::SendError::Timeout(Some(_))),
        ) => {
            // Kameo returns the original message for these variants, proving
            // it never reached the actor. The optimistic tuple write can be
            // safely restored without an actor-state reconciliation.
            rollback_admin_affiliations(
                state,
                managed_channel_id.as_deref(),
                &durable_previous_affiliations,
            )
            .await;
            warn!(room = %room_jid, error = ?error, "MUC admin mutation was not delivered to the actor");
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                resource_constraint_iq_error("This room is temporarily unavailable; please retry."),
            )];
        }
        Err(error) => {
            // A non-handler failure is ambiguous: `ApplyAdminItems` may
            // already have durably committed and assigned `self.room` before
            // its reply was delayed by durable-recipient rehydration. Query a
            // mailbox-ordered snapshot before restoring the optimistic
            // managed-channel tuples. If the snapshot proves the affiliation
            // batch committed, reconstruct the caller-owned outward effects so
            // bans, membership removals, and voice changes are not dropped.
            let mut rollback_required = false;
            let recovered_applied = match room_actor
                .ask(GetSnapshot)
                .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
                .await
            {
                Ok(snapshot)
                    if pre_apply_snapshot
                        .as_ref()
                        .is_some_and(|snapshot_before_apply| {
                            admin_items_match_room(
                                &snapshot_before_apply.room,
                                &snapshot.room,
                                &query.items,
                            )
                        }) =>
                {
                    pre_apply_snapshot.as_ref().map(|snapshot_before_apply| {
                        recover_committed_admin_effects(
                            &snapshot_before_apply.room,
                            &query.items,
                            sender_jid,
                            &state.deps.occupant_id_secret,
                        )
                    })
                }
                Ok(_) => {
                    rollback_required = true;
                    None
                }
                Err(snapshot_error) => {
                    warn!(
                        room = %room_jid,
                        error = ?snapshot_error,
                        "Could not reconcile ambiguous MUC admin actor failure"
                    );
                    None
                }
            };
            if let Some(applied) = recovered_applied {
                warn!(room = %room_jid, error = ?error, "MUC admin actor reply was ambiguous but the committed affiliation batch was reconciled");
                applied
            } else {
                if rollback_required {
                    rollback_admin_affiliations(
                        state,
                        managed_channel_id.as_deref(),
                        &durable_previous_affiliations,
                    )
                    .await;
                }
                warn!(room = %room_jid, error = ?error, rollback_required, "MUC admin actor result was ambiguous");
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    resource_constraint_iq_error(
                        "This room's update outcome is being reconciled; please retry.",
                    ),
                )];
            }
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
    // Voice-derived media grants: any non-removal change that alters
    // an occupant's XEP-0045 voice — an explicit `<item role='…'/>`
    // or an affiliation change that re-derives their role — must
    // converge their live SFU permission, otherwise a de-voiced
    // occupant keeps publishing until they renegotiate.
    // Fire-and-forget like the eviction above.
    for (session, new_voice) in &applied.voice_changes {
        super::super::super::muc_call_sfu::apply_voice_grants_for_room(
            state, &room_jid, session, *new_voice,
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

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::muc::{Occupant, RoomConfig};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    fn test_secret() -> OccupantIdSecret {
        OccupantIdSecret::new(b"occupant-id-secret-at-least-32-bytes".to_vec())
            .expect("test secret meets minimum length")
    }

    fn seat(
        room: &mut waddle_xmpp::muc::MucRoom,
        nick: &str,
        jid: &str,
        affiliation: Affiliation,
        role: waddle_xmpp::Role,
    ) {
        let real_jid: FullJid = jid.parse().expect("occupant full jid");
        room.set_affiliation(real_jid.to_bare(), affiliation);
        room.add_occupant(Occupant {
            real_jid,
            nick: nick.to_string(),
            role,
            affiliation,
            is_remote: false,
            home_server: None,
        });
    }

    #[test]
    fn affiliation_reconciliation_requires_every_update_in_the_actor_snapshot() {
        let room_jid: BareJid = "admin-reconciliation@muc.example.com"
            .parse()
            .expect("room JID");
        let target: BareJid = "target@example.com".parse().expect("target JID");
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle".to_string(),
            "channel".to_string(),
            waddle_xmpp::muc::RoomConfig::default(),
        );
        let updates = vec![(target.clone(), Affiliation::Member)];

        assert!(
            !affiliation_updates_match_room(&room, &updates),
            "an unchanged actor snapshot proves the optimistic tuple must be restored"
        );

        room.set_affiliation(target, Affiliation::Member);
        assert!(
            affiliation_updates_match_room(&room, &updates),
            "a mailbox-ordered snapshot preserves a projection that the actor committed"
        );
    }

    #[test]
    fn committed_ban_recovery_replays_presence_and_removal_side_effects() {
        let room_jid: BareJid = "admin-ban-recovery@muc.example.com"
            .parse()
            .expect("room JID");
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle".to_string(),
            "channel".to_string(),
            RoomConfig {
                members_only: false,
                ..RoomConfig::default()
            },
        );
        seat(
            &mut room,
            "admin",
            "admin@example.com/web",
            Affiliation::Owner,
            waddle_xmpp::Role::Moderator,
        );
        seat(
            &mut room,
            "target",
            "target@example.com/web",
            Affiliation::Member,
            waddle_xmpp::Role::Participant,
        );

        let applied = recover_committed_admin_effects(
            &room,
            &[AdminItem {
                jid: Some("target@example.com".parse().expect("target bare jid")),
                nick: None,
                affiliation: Some(Affiliation::Outcast),
                role: None,
                reason: Some("cleanup".to_string()),
            }],
            &"admin@example.com/web".parse().expect("admin full jid"),
            &test_secret(),
        );
        let target_full_jid: FullJid = "target@example.com/web".parse().expect("target full jid");

        assert_eq!(
            applied.removed_by_moderation,
            vec![target_full_jid.clone()],
            "a reconciled ban must still evict the target session"
        );
        assert_eq!(
            applied.presence_updates.len(),
            2,
            "the ban must still notify both the target and the moderator"
        );
        let self_ban = applied
            .presence_updates
            .iter()
            .find(|(recipient, _)| recipient == &target_full_jid)
            .expect("self ban presence");
        let self_ban_xml = stanza_to_xml(&Stanza::Presence(self_ban.1.clone()));
        assert!(
            self_ban_xml.contains("code='301'") && self_ban_xml.contains("code='110'"),
            "the banned occupant must still receive 301 + 110 self-presence: {self_ban_xml}"
        );
    }

    #[test]
    fn committed_members_only_removal_recovery_replays_status_321_presence() {
        let room_jid: BareJid = "admin-members-only-recovery@muc.example.com"
            .parse()
            .expect("room JID");
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle".to_string(),
            "channel".to_string(),
            RoomConfig {
                members_only: true,
                ..RoomConfig::default()
            },
        );
        seat(
            &mut room,
            "admin",
            "admin@example.com/web",
            Affiliation::Owner,
            waddle_xmpp::Role::Moderator,
        );
        seat(
            &mut room,
            "member",
            "member@example.com/web",
            Affiliation::Member,
            waddle_xmpp::Role::Participant,
        );

        let applied = recover_committed_admin_effects(
            &room,
            &[AdminItem {
                jid: Some("member@example.com".parse().expect("member bare jid")),
                nick: None,
                affiliation: Some(Affiliation::None),
                role: None,
                reason: None,
            }],
            &"admin@example.com/web".parse().expect("admin full jid"),
            &test_secret(),
        );
        let member_full_jid: FullJid = "member@example.com/web".parse().expect("member full jid");

        assert_eq!(
            applied.removed_by_moderation,
            vec![member_full_jid.clone()],
            "losing membership in a members-only room must still evict the session"
        );
        let self_removal = applied
            .presence_updates
            .iter()
            .find(|(recipient, _)| recipient == &member_full_jid)
            .expect("self removal presence");
        let self_removal_xml = stanza_to_xml(&Stanza::Presence(self_removal.1.clone()));
        assert!(
            self_removal_xml.contains("code='321'"),
            "members-only removal must still replay status 321: {self_removal_xml}"
        );
    }

    #[test]
    fn committed_role_reconciliation_requires_post_snapshot_to_match_exact_role_intent() {
        let room_jid: BareJid = "admin-role-reconciliation@muc.example.com"
            .parse()
            .expect("room JID");
        let mut before = waddle_xmpp::muc::MucRoom::new(
            room_jid.clone(),
            "waddle".to_string(),
            "channel".to_string(),
            RoomConfig::default(),
        );
        seat(
            &mut before,
            "target",
            "target@example.com/web",
            Affiliation::Member,
            waddle_xmpp::Role::Participant,
        );

        let mut after = before.clone();
        assert!(
            !admin_items_match_room(
                &before,
                &after,
                &[AdminItem {
                    jid: None,
                    nick: Some("target".to_string()),
                    affiliation: None,
                    role: Some(waddle_xmpp::Role::Moderator),
                    reason: None,
                }],
            ),
            "an unchanged snapshot must not prove a role mutation committed"
        );

        after
            .occupants
            .get_mut("target")
            .expect("target occupant")
            .role = waddle_xmpp::Role::Moderator;
        assert!(
            admin_items_match_room(
                &before,
                &after,
                &[AdminItem {
                    jid: None,
                    nick: Some("target".to_string()),
                    affiliation: None,
                    role: Some(waddle_xmpp::Role::Moderator),
                    reason: None,
                }],
            ),
            "a mailbox-ordered snapshot must prove a committed devoice/promote role change"
        );

        let mut kicked = before.clone();
        kicked.remove_occupant("target");
        assert!(
            admin_items_match_room(
                &before,
                &kicked,
                &[AdminItem {
                    jid: None,
                    nick: Some("target".to_string()),
                    affiliation: None,
                    role: Some(waddle_xmpp::Role::None),
                    reason: Some("cleanup".to_string()),
                }],
            ),
            "a kicked nick disappearing from the post-apply snapshot proves the role-none commit"
        );
    }

    #[test]
    fn committed_kick_recovery_replays_presence_and_removal_side_effects() {
        let room_jid: BareJid = "admin-kick-recovery@muc.example.com"
            .parse()
            .expect("room JID");
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle".to_string(),
            "channel".to_string(),
            RoomConfig {
                members_only: false,
                ..RoomConfig::default()
            },
        );
        seat(
            &mut room,
            "admin",
            "admin@example.com/web",
            Affiliation::Owner,
            waddle_xmpp::Role::Moderator,
        );
        seat(
            &mut room,
            "target",
            "target@example.com/web",
            Affiliation::Member,
            waddle_xmpp::Role::Participant,
        );

        let applied = recover_committed_admin_effects(
            &room,
            &[AdminItem {
                jid: None,
                nick: Some("target".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::None),
                reason: Some("cleanup".to_string()),
            }],
            &"admin@example.com/web".parse().expect("admin full jid"),
            &test_secret(),
        );
        let target_full_jid: FullJid = "target@example.com/web".parse().expect("target full jid");

        assert_eq!(
            applied.removed_by_moderation,
            vec![target_full_jid.clone()],
            "a reconciled kick must still evict the target session"
        );
        let self_removal = applied
            .presence_updates
            .iter()
            .find(|(recipient, _)| recipient == &target_full_jid)
            .expect("self removal presence");
        let self_removal_xml = stanza_to_xml(&Stanza::Presence(self_removal.1.clone()));
        assert!(
            self_removal_xml.contains("code='307'") && self_removal_xml.contains("code='110'"),
            "kick recovery must still replay the 307 self-presence: {self_removal_xml}"
        );
    }

    #[test]
    fn committed_affiliation_voice_change_recovery_replays_voice_convergence() {
        let room_jid: BareJid = "admin-voice-recovery@muc.example.com"
            .parse()
            .expect("room JID");
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle".to_string(),
            "channel".to_string(),
            RoomConfig {
                moderated: true,
                members_only: false,
                ..RoomConfig::default()
            },
        );
        seat(
            &mut room,
            "owner",
            "owner@example.com/web",
            Affiliation::Owner,
            waddle_xmpp::Role::Moderator,
        );
        seat(
            &mut room,
            "mallory",
            "mallory@example.com/web",
            Affiliation::Admin,
            waddle_xmpp::Role::Moderator,
        );

        let applied = recover_committed_admin_effects(
            &room,
            &[AdminItem {
                jid: Some("mallory@example.com".parse().expect("mallory bare jid")),
                nick: None,
                affiliation: Some(Affiliation::None),
                role: None,
                reason: None,
            }],
            &"owner@example.com/web".parse().expect("owner full jid"),
            &test_secret(),
        );

        assert!(
            applied.removed_by_moderation.is_empty(),
            "an open-room demotion should not evict the occupant"
        );
        assert_eq!(
            applied.voice_changes,
            vec![(
                "mallory@example.com/web".parse().expect("mallory full jid"),
                waddle_xmpp::Voice::Muted,
            )],
            "a reconciled demotion must still re-converge the occupant's SFU voice grant"
        );
    }

    #[test]
    fn committed_role_voice_change_recovery_replays_voice_convergence() {
        let room_jid: BareJid = "admin-role-voice-recovery@muc.example.com"
            .parse()
            .expect("room JID");
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle".to_string(),
            "channel".to_string(),
            RoomConfig {
                moderated: true,
                members_only: false,
                ..RoomConfig::default()
            },
        );
        seat(
            &mut room,
            "owner",
            "owner@example.com/web",
            Affiliation::Owner,
            waddle_xmpp::Role::Moderator,
        );
        seat(
            &mut room,
            "mallory",
            "mallory@example.com/web",
            Affiliation::Member,
            waddle_xmpp::Role::Participant,
        );

        let applied = recover_committed_admin_effects(
            &room,
            &[AdminItem {
                jid: None,
                nick: Some("mallory".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Visitor),
                reason: None,
            }],
            &"owner@example.com/web".parse().expect("owner full jid"),
            &test_secret(),
        );

        assert!(
            applied.removed_by_moderation.is_empty(),
            "a devoice is not a removal"
        );
        assert_eq!(
            applied.voice_changes,
            vec![(
                "mallory@example.com/web".parse().expect("mallory full jid"),
                waddle_xmpp::Voice::Muted,
            )],
            "a reconciled role demotion must still converge the occupant's SFU voice grant"
        );
    }
}
