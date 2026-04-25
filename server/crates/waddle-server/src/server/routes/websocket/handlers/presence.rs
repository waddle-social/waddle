use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    muc::{
        room_actor::{JoinWithAffiliation, LeaveByRealJid},
        RoomConfig,
    },
    presence::subscription::{
        build_available_presence, build_subscription_presence, build_unavailable_presence,
        parse_subscription_presence, PresenceAction, SubscriptionStateMachine, SubscriptionType,
    },
    roster::{AskType, RosterItem, Subscription},
    Affiliation, Role, Stanza,
};
use xmpp_parsers::minidom::Element;

use super::super::{element_to_xml, get_or_create_room_actor, get_room_actor, WebSocketState};
use crate::auth::Session;
use crate::db::actor::GetDatabase;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::{DatabaseRosterStorage, RosterItemRow};
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::xmpp_state::{get_xmpp_channel, XmppChannelRecord};
use waddle_xmpp::protocol::ConnectionPhase;

pub async fn handle_presence(
    presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    let is_unavailable = presence.type_ == xmpp_parsers::presence::Type::Unavailable;

    // Check if this is a MUC presence (to room@muc.domain/nick)
    if let Some(to_jid) = presence
        .to
        .as_ref()
        .filter(|jid| jid.domain().as_str() == muc_domain)
    {
        let room_jid = to_jid.to_bare();
        let Some(nick) = to_jid.resource().map(|resource| resource.as_str()) else {
            warn!(room = %room_jid, "MUC presence missing occupant nick");
            return vec![];
        };

        let Some(sender_jid) = phase.bound_jid() else {
            warn!("MUC presence without authenticated session");
            return vec![];
        };

        if is_unavailable {
            return handle_muc_leave(state, &room_jid, sender_jid, nick).await;
        }

        return handle_muc_join(
            state,
            domain,
            &room_jid,
            sender_jid,
            nick,
            _authenticated_session,
        )
        .await;
    }

    let Some(sender_jid) = phase.bound_jid() else {
        warn!("Presence received without authenticated session");
        return vec![];
    };

    if is_directed_presence_update(&presence) {
        handle_directed_presence(state, sender_jid, presence).await;
        return vec![];
    }

    match parse_subscription_presence(&presence, &sender_jid.to_bare()) {
        Ok(PresenceAction::Subscription(request)) => {
            handle_subscription_presence(state, request).await;
        }
        Ok(PresenceAction::Probe { from, to, .. }) => {
            handle_presence_probe(state, from, to).await;
        }
        Ok(PresenceAction::PresenceUpdate(presence_update)) => {
            handle_regular_presence_update(state, sender_jid, presence_update).await;
        }
        Err(error) => {
            warn!(error = %error, "Invalid presence stanza");
        }
    }
    vec![]
}

fn is_directed_presence_update(presence: &xmpp_parsers::presence::Presence) -> bool {
    presence.to.is_some()
        && !matches!(
            presence.type_,
            xmpp_parsers::presence::Type::Subscribe
                | xmpp_parsers::presence::Type::Subscribed
                | xmpp_parsers::presence::Type::Unsubscribe
                | xmpp_parsers::presence::Type::Unsubscribed
                | xmpp_parsers::presence::Type::Probe
        )
}

async fn roster_storage(state: &WebSocketState) -> Option<DatabaseRosterStorage> {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => Some(DatabaseRosterStorage::new(db)),
        Err(error) => {
            warn!(error = %error, "Failed to access roster database for presence");
            None
        }
    }
}

async fn blocking_storage(state: &WebSocketState) -> Option<DatabaseBlockingStorage> {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => Some(DatabaseBlockingStorage::new(db)),
        Err(error) => {
            warn!(error = %error, "Failed to access blocking database for presence");
            None
        }
    }
}

async fn recipient_blocks_sender(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
) -> bool {
    let Some(storage) = blocking_storage(state).await else {
        return false;
    };
    match storage.is_blocked(recipient, sender).await {
        Ok(blocked) => blocked,
        Err(error) => {
            warn!(error = %error, recipient = %recipient, sender = %sender, "Failed to check blocking state");
            true
        }
    }
}

async fn handle_subscription_presence(
    state: &WebSocketState,
    request: waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) {
    if recipient_blocks_sender(state, &request.to, &request.from).await {
        debug!(from = %request.from, to = %request.to, "Dropping subscription presence blocked by recipient");
        return;
    }

    if let Some(storage) = roster_storage(state).await {
        if let Err(error) = update_subscription_roster_state(&storage, &request).await {
            warn!(error = %error, from = %request.from, to = %request.to, "Failed to update roster subscription state");
        }
    }

    let stanza = Stanza::Presence(build_subscription_presence(
        request.subscription_type,
        &request.from,
        &request.to,
        request.status.as_deref(),
        &request.payloads,
    ));
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(&request.to);
    if resources.is_empty() {
        state
            .deps
            .protocol
            .connection_registry
            .queue_pending_subscription_stanza(&request.to, stanza);
        return;
    }
    for resource in resources {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}

async fn handle_directed_presence(
    state: &WebSocketState,
    sender_jid: &FullJid,
    mut presence: xmpp_parsers::presence::Presence,
) {
    let Some(target) = presence.to.clone() else {
        return;
    };
    let target_bare = target.to_bare();
    if recipient_blocks_sender(state, &target_bare, &sender_jid.to_bare()).await {
        debug!(from = %sender_jid, to = %target_bare, "Dropping directed presence blocked by recipient");
        return;
    }

    presence.from = Some(Jid::from(sender_jid.clone()));
    let stanza = Stanza::Presence(presence);
    if let Ok(target_full) = target.clone().try_into_full() {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&target_full, stanza)
            .await;
        return;
    }

    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(&target_bare)
    {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}

async fn update_subscription_roster_state(
    storage: &DatabaseRosterStorage,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) -> Result<(), crate::db::roster::RosterStorageError> {
    let mut from_item = load_roster_item(storage, &request.from, &request.to).await?;
    let mut to_item = load_roster_item(storage, &request.to, &request.from).await?;
    match request.subscription_type {
        SubscriptionType::Subscribe => {
            SubscriptionStateMachine::apply_outbound_subscribe(&mut from_item);
        }
        SubscriptionType::Subscribed => {
            SubscriptionStateMachine::apply_outbound_subscribed(&mut from_item);
            SubscriptionStateMachine::apply_inbound_subscribed(&mut to_item);
        }
        SubscriptionType::Unsubscribe => {
            SubscriptionStateMachine::apply_outbound_unsubscribe(&mut from_item);
            SubscriptionStateMachine::apply_inbound_unsubscribed(&mut to_item);
        }
        SubscriptionType::Unsubscribed => {
            SubscriptionStateMachine::apply_outbound_unsubscribed(&mut from_item);
            SubscriptionStateMachine::apply_inbound_unsubscribed(&mut to_item);
        }
    }
    storage
        .set_roster_item(&request.from, &roster_item_to_row(&from_item))
        .await?;
    storage
        .set_roster_item(&request.to, &roster_item_to_row(&to_item))
        .await?;
    Ok(())
}

async fn load_roster_item(
    storage: &DatabaseRosterStorage,
    user: &BareJid,
    contact: &BareJid,
) -> Result<RosterItem, crate::db::roster::RosterStorageError> {
    Ok(match storage.get_roster_item(user, contact).await? {
        Some(row) => roster_row_to_item(row),
        None => RosterItem::new(contact.clone()),
    })
}

fn roster_row_to_item(row: RosterItemRow) -> RosterItem {
    RosterItem {
        jid: row.contact_jid.parse().expect("stored roster JID is valid"),
        name: row.name,
        subscription: parse_subscription_state(&row.subscription),
        ask: row.ask.as_deref().and_then(parse_ask_state),
        groups: row.groups,
    }
}

fn roster_item_to_row(item: &RosterItem) -> RosterItemRow {
    RosterItemRow {
        contact_jid: item.jid.to_string(),
        name: item.name.clone(),
        subscription: subscription_state_str(item.subscription).to_string(),
        ask: item.ask.map(ask_state_str).map(ToOwned::to_owned),
        groups: item.groups.clone(),
    }
}

fn parse_subscription_state(value: &str) -> Subscription {
    match value {
        "to" => Subscription::To,
        "from" => Subscription::From,
        "both" => Subscription::Both,
        "remove" => Subscription::Remove,
        _ => Subscription::None,
    }
}

fn subscription_state_str(value: Subscription) -> &'static str {
    match value {
        Subscription::None => "none",
        Subscription::To => "to",
        Subscription::From => "from",
        Subscription::Both => "both",
        Subscription::Remove => "remove",
    }
}

fn parse_ask_state(value: &str) -> Option<AskType> {
    match value {
        "subscribe" => Some(AskType::Subscribe),
        _ => None,
    }
}

fn ask_state_str(value: AskType) -> &'static str {
    match value {
        AskType::Subscribe => "subscribe",
    }
}

async fn handle_presence_probe(state: &WebSocketState, from: BareJid, to: BareJid) {
    if recipient_blocks_sender(state, &to, &from).await {
        info!(requester = %from, target = %to, "Blocked presence probe");
        return;
    }
    let available = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(&to);
    if available.is_empty() {
        let unavailable = Stanza::Presence(build_unavailable_presence(&to, &from));
        for resource in state
            .deps
            .protocol
            .connection_registry
            .get_resources_for_user(&from)
        {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(&resource, unavailable.clone())
                .await;
        }
        return;
    }
    let requester_resources = state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(&from);
    for (resource, _priority) in available {
        let presence_state = state
            .deps
            .protocol
            .connection_registry
            .get_presence_state(&resource);
        let presence = Stanza::Presence(build_available_presence(
            &resource,
            &from,
            presence_state
                .as_ref()
                .and_then(|state| state.show.as_deref()),
            presence_state
                .as_ref()
                .and_then(|state| state.status.as_deref()),
            presence_state
                .as_ref()
                .map(|state| state.priority)
                .unwrap_or(0),
        ));
        for requester_resource in &requester_resources {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(requester_resource, presence.clone())
                .await;
        }
    }
}

async fn handle_regular_presence_update(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: xmpp_parsers::presence::Presence,
) {
    let available = presence.type_ != xmpp_parsers::presence::Type::Unavailable;
    let priority = presence.priority;
    if available {
        state
            .deps
            .protocol
            .connection_registry
            .clear_last_activity(&sender_jid.to_bare());
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(sender_jid, true, priority);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence_state(
                sender_jid,
                presence
                    .show
                    .as_ref()
                    .map(|show| show_name(show).to_string()),
                presence.statuses.values().next().cloned(),
                priority,
            );
        for stanza in state
            .deps
            .protocol
            .connection_registry
            .drain_pending_subscription_stanzas(&sender_jid.to_bare())
        {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(sender_jid, stanza)
                .await;
        }
    } else {
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(sender_jid, false, priority);
        state
            .deps
            .protocol
            .connection_registry
            .clear_presence_state(sender_jid);
        state
            .deps
            .protocol
            .connection_registry
            .record_last_activity(
                &sender_jid.to_bare(),
                presence.statuses.values().next().cloned(),
            );
    }
    broadcast_presence_to_subscribers(state, sender_jid, &presence, available).await;
}

async fn broadcast_presence_to_subscribers(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: &xmpp_parsers::presence::Presence,
    available: bool,
) {
    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let subscribers = match storage
        .get_presence_subscribers(&sender_jid.to_bare())
        .await
    {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(error = %error, jid = %sender_jid, "Failed to load presence subscribers");
            return;
        }
    };
    for subscriber in subscribers {
        let Ok(subscriber_bare) = subscriber.parse::<BareJid>() else {
            continue;
        };
        if recipient_blocks_sender(state, &sender_jid.to_bare(), &subscriber_bare).await {
            continue;
        }
        let stanza = if available {
            let show = presence.show.as_ref().map(show_name);
            Stanza::Presence(build_available_presence(
                sender_jid,
                &subscriber_bare,
                show,
                presence.statuses.values().next().map(String::as_str),
                presence.priority,
            ))
        } else {
            Stanza::Presence(build_unavailable_presence(
                &sender_jid.to_bare(),
                &subscriber_bare,
            ))
        };
        for resource in state
            .deps
            .protocol
            .connection_registry
            .get_resources_for_user(&subscriber_bare)
        {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(&resource, stanza.clone())
                .await;
        }
    }
}

fn show_name(show: &xmpp_parsers::presence::Show) -> &'static str {
    match show {
        xmpp_parsers::presence::Show::Away => "away",
        xmpp_parsers::presence::Show::Chat => "chat",
        xmpp_parsers::presence::Show::Dnd => "dnd",
        xmpp_parsers::presence::Show::Xa => "xa",
    }
}

/// Handle MUC room join
pub async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC join request");

    let managed_channel = match get_managed_channel_for_room(state, room_jid).await {
        Ok(channel) => channel,
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to resolve managed MUC channel");
            return vec![build_muc_join_error_xml(
                room_jid,
                nick,
                sender_jid,
                "wait",
                "internal-server-error",
            )];
        }
    };
    let managed_affiliation = if let Some(channel) = managed_channel.as_ref() {
        let Some(session) = authenticated_session else {
            return vec![build_muc_join_error_xml(
                room_jid,
                nick,
                sender_jid,
                "auth",
                "not-authorized",
            )];
        };
        match resolve_managed_channel_affiliation(state, session, &channel.id).await {
            Ok(Some(Affiliation::Outcast)) => {
                return vec![build_muc_join_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    "auth",
                    "forbidden",
                )];
            }
            Ok(Some(affiliation)) => Some(affiliation),
            Ok(None) => {
                return vec![build_muc_join_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    "auth",
                    "registration-required",
                )];
            }
            Err(()) => {
                return vec![build_muc_join_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    "wait",
                    "internal-server-error",
                )];
            }
        }
    } else {
        None
    };

    let existing_room_actor = get_room_actor(state, room_jid).await;
    let (room_actor, created_instant_room) = match existing_room_actor {
        Some(actor) => (actor, false),
        None => {
            let config = managed_channel
                .as_ref()
                .map(|channel| RoomConfig {
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                    members_only: true,
                    forum: channel.channel_type == "forum",
                    ..Default::default()
                })
                .unwrap_or_else(|| RoomConfig {
                    name: room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string()),
                    members_only: false,
                    ..Default::default()
                });

            let (waddle_id, channel_id) = managed_channel
                .as_ref()
                .map(|channel| {
                    let (waddle_id, _) = parse_room_jid_context(room_jid);
                    (waddle_id, channel.id.clone())
                })
                .unwrap_or_else(|| parse_room_jid_context(room_jid));

            let Some(actor) =
                get_or_create_room_actor(state, room_jid, config, waddle_id, channel_id).await
            else {
                return vec![];
            };
            (actor, managed_channel.is_none())
        }
    };

    let effective_affiliation = if created_instant_room {
        Affiliation::Owner
    } else if let Some(affiliation) = managed_affiliation {
        affiliation
    } else {
        Affiliation::Member
    };

    let join_outcome = match room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: nick.to_string(),
            effective_affiliation,
            local_domain: domain.to_string(),
        })
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let nick_collision = matches!(
                &error,
                kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::NickAlreadyInUse(_)
                )
            );
            if nick_collision {
                warn!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    "MUC nick collision; returning conflict"
                );
                return vec![build_muc_conflict_presence_xml(room_jid, nick, sender_jid)];
            }
            warn!(room = %room_jid, nick = %nick, error = ?error, "Failed to join MUC room");
            return vec![];
        }
    };

    let occupant_count = join_outcome.occupant_count;

    info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

    let mut responses = Vec::new();

    // Replay one occupant presence per nick to the joiner. Same-bare multi-session
    // joins must not turn into duplicate room occupants on the wire.
    let mut replayed_nicks = std::collections::HashSet::new();
    for existing in join_outcome
        .existing_occupants
        .iter()
        .filter(|existing| existing.nick != nick)
        .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
    {
        responses.push(build_muc_join_presence_xml(
            room_jid,
            &existing.nick,
            sender_jid,
            affiliation_str(existing.affiliation),
            role_str(existing.role),
            &existing.jid,
            false,
        ));
    }

    // Broadcast the new occupant's presence to all existing occupants.
    // Non-blocking: a zombied/slow consumer must never stall the join path,
    // which is how "Timed out waiting for self-presence" cascades start.
    // Drop accounting is handled inside `try_send_to` (logs + metrics);
    // per-occupant outcome is discarded here because a missed join
    // presence self-heals via the next MUC presence/probe round-trip.
    if !join_outcome.is_same_bare_multi_session_join {
        for existing in &join_outcome.existing_occupants {
            let presence_stanza = create_presence_stanza(
                room_jid,
                nick,
                sender_jid,
                &existing.jid,
                join_outcome.new_occupant_affiliation,
                join_outcome.new_occupant_role,
            );
            let stanza = Stanza::Presence(presence_stanza);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&existing.jid, stanza);
        }
    }

    // Send self-presence to the joining user (with status code 110)
    responses.push(build_muc_join_presence_xml(
        room_jid,
        nick,
        sender_jid,
        affiliation_str(join_outcome.new_occupant_affiliation),
        role_str(join_outcome.new_occupant_role),
        sender_jid,
        true,
    ));

    // Send room subject
    let room_name = room_jid
        .node()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "Waddle".to_string());
    responses.push(build_muc_subject_message_xml(
        room_jid, sender_jid, &room_name,
    ));

    responses
}

async fn resolve_managed_channel_affiliation(
    state: &WebSocketState,
    session: &Session,
    channel_id: &str,
) -> Result<Option<Affiliation>, ()> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(&session.user_id);
    if check_channel_permission(
        state,
        object.clone(),
        subject.clone(),
        Permission::Custom("outcast".into()),
    )
    .await?
    {
        return Ok(Some(Affiliation::Outcast));
    }

    for (permission, affiliation) in [
        (Permission::Owner, Affiliation::Owner),
        (Permission::Admin, Affiliation::Admin),
        (Permission::Member, Affiliation::Member),
    ] {
        if check_channel_permission(state, object.clone(), subject.clone(), permission).await? {
            return Ok(Some(affiliation));
        }
    }

    if check_channel_permission(state, object, subject, Permission::Read).await? {
        return Ok(Some(Affiliation::Member));
    }
    Ok(None)
}

async fn check_channel_permission(
    state: &WebSocketState,
    object: Object,
    subject: Subject,
    permission: Permission,
) -> Result<bool, ()> {
    let response = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject,
            permission,
            object,
        })
        .await
        .map_err(|error| {
            warn!(error = ?error, "Permission actor failed during managed MUC join");
        })?;
    Ok(response.allowed)
}

/// Handle MUC room leave
pub async fn handle_muc_leave(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave request");

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        debug!(room = %room_jid, "Room not found for leave");
        return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
    };

    let outcome = match room_actor
        .ask(LeaveByRealJid {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave for absent occupant");
            return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
        }
        Err(error) => {
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room");
            return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
        }
    };

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`.
    if outcome.removed_last_session {
        for occupant_jid in &outcome.remaining_occupants {
            let from_jid = room_jid
                .clone()
                .with_resource_str(&outcome.nick)
                .unwrap_or_else(|_| sender_jid.clone());
            let mut presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
            presence.from = Some(jid::Jid::from(from_jid));
            presence.to = Some(jid::Jid::from(occupant_jid.clone()));
            let stanza = Stanza::Presence(presence);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(occupant_jid, stanza);
        }
    }

    vec![build_muc_self_unavailable_xml(
        room_jid,
        &outcome.nick,
        sender_jid,
    )]
}

fn build_muc_join_presence_xml(
    room_jid: &BareJid,
    nick: &str,
    to_jid: &FullJid,
    affiliation: &str,
    role: &str,
    real_jid: &FullJid,
    include_self_status: bool,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let mut user_payload = Element::builder("x", "http://jabber.org/protocol/muc#user").append(
        Element::builder("item", "http://jabber.org/protocol/muc#user")
            .attr("affiliation", affiliation)
            .attr("role", role)
            .attr("jid", real_jid.to_string())
            .build(),
    );

    if include_self_status {
        user_payload = user_payload.append(
            Element::builder("status", "http://jabber.org/protocol/muc#user")
                .attr("code", "110")
                .build(),
        );
    }

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .append(user_payload.build())
            .build(),
    )
}

/// XEP-0045 §7.2.9 conflict presence: the requested nick is already in use
/// by a different user. The joiner receives a `<presence type='error'/>` and
/// no room state changes.
fn build_muc_conflict_presence_xml(room_jid: &BareJid, nick: &str, to_jid: &FullJid) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let error_payload = Element::builder("error", waddle_xmpp::ns::JABBER_CLIENT)
        .attr("type", "cancel")
        .append(Element::builder("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas").build())
        .build();

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "error")
            .append(error_payload)
            .build(),
    )
}

fn build_muc_join_error_xml(
    room_jid: &BareJid,
    nick: &str,
    to_jid: &FullJid,
    error_type: &str,
    condition: &str,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let error_payload = Element::builder("error", waddle_xmpp::ns::JABBER_CLIENT)
        .attr("type", error_type)
        .append(Element::builder(condition, "urn:ietf:params:xml:ns:xmpp-stanzas").build())
        .build();

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "error")
            .append(error_payload)
            .build(),
    )
}

fn build_muc_subject_message_xml(room_jid: &BareJid, to_jid: &FullJid, room_name: &str) -> String {
    element_to_xml(
        Element::builder("message", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", room_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "groupchat")
            .append(
                Element::builder("subject", waddle_xmpp::ns::JABBER_CLIENT)
                    .append(format!("Welcome to {}!", room_name))
                    .build(),
            )
            .build(),
    )
}

fn build_muc_self_unavailable_xml(room_jid: &BareJid, nick: &str, sender_jid: &FullJid) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| sender_jid.clone());

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", sender_jid.to_string())
            .attr("type", "unavailable")
            .append(
                Element::builder("x", "http://jabber.org/protocol/muc#user")
                    .append(
                        Element::builder("item", "http://jabber.org/protocol/muc#user")
                            .attr("affiliation", "member")
                            .attr("role", "none")
                            .build(),
                    )
                    .append(
                        Element::builder("status", "http://jabber.org/protocol/muc#user")
                            .attr("code", "110")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
}

/// Create a presence stanza for MUC
fn create_presence_stanza(
    room_jid: &BareJid,
    nick: &str,
    real_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
) -> xmpp_parsers::presence::Presence {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| real_jid.clone());

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid::Jid::from(from_jid));
    presence.to = Some(jid::Jid::from(to_jid.clone()));
    presence.payloads.push(
        Element::builder("x", "http://jabber.org/protocol/muc#user")
            .append(
                Element::builder("item", "http://jabber.org/protocol/muc#user")
                    .attr("affiliation", affiliation_str(affiliation))
                    .attr("role", role_str(role))
                    .attr("jid", real_jid.to_string())
                    .build(),
            )
            .build(),
    );

    presence
}

/// Convert Affiliation to string
fn affiliation_str(affiliation: Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
    }
}

/// Convert Role to string
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Moderator => "moderator",
        Role::Participant => "participant",
        Role::Visitor => "visitor",
        Role::None => "none",
    }
}

/// Derive the single-space id and channel id from a room's bare JID node.
///
/// Convention: node is the channel id.
/// Falls back to ("default", "default") if the node can't be parsed.
pub fn parse_room_jid_context(room_jid: &jid::BareJid) -> (String, String) {
    if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        return ("space".to_string(), channel_id);
    }
    ("default".to_string(), "default".to_string())
}

pub async fn get_managed_channel_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<XmppChannelRecord>, String> {
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(None);
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    get_xmpp_channel(actor, &channel_id).await
}
