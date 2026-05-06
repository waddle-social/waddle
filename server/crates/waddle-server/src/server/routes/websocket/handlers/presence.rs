use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    muc::{
        messages::build_subject_message,
        room_actor::{JoinWithAffiliation, LeaveByRealJid},
        RoomConfig,
    },
    presence::subscription::{
        build_available_presence, build_subscription_presence, build_unavailable_presence,
        parse_subscription_presence, PresenceAction, SubscriptionStateMachine, SubscriptionType,
    },
    registry::BroadcastOutcome,
    roster::{build_roster_push, AskType, RosterItem, RosterVersion, Subscription},
    xep::NS_DELAY,
    Affiliation, Role, Stanza,
};
use xmpp_parsers::minidom::Element;

use super::super::{
    element_to_xml, get_or_create_room_actor, get_room_actor, stanza_to_xml, WebSocketState,
};
use crate::auth::Session;
use crate::db::actor::GetDatabase;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterStorageError,
};
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::xmpp_state::{get_xmpp_channel, XmppChannelRecord};
use waddle_xmpp::protocol::ConnectionPhase;

pub async fn handle_presence(
    mut presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    strip_client_authored_delay(&mut presence);
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
        Ok(PresenceAction::Probe {
            from,
            to,
            to_was_full,
        }) => {
            let to_full = if to_was_full {
                presence
                    .to
                    .as_ref()
                    .and_then(|jid| jid.clone().try_into_full().ok())
            } else {
                None
            };
            handle_presence_probe(state, from, to, to_full).await;
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

fn strip_client_authored_delay(presence: &mut xmpp_parsers::presence::Presence) {
    presence
        .payloads
        .retain(|payload| !(payload.name() == "delay" && payload.ns() == NS_DELAY));
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

#[cfg(test)]
mod delay_strip_tests {
    use super::*;

    #[test]
    fn strips_client_supplied_delay_payload() {
        let xml = "<presence xmlns='jabber:client' from='alice@example.com/web'>\
                    <delay xmlns='urn:xmpp:delay' from='evil.example' stamp='2024-06-01T09:30:00Z'/>\
                    <status>ready</status>\
                    </presence>";
        let mut presence =
            xmpp_parsers::presence::Presence::try_from(xml.parse::<Element>().expect("valid xml"))
                .expect("presence");

        strip_client_authored_delay(&mut presence);

        assert!(presence
            .payloads
            .iter()
            .all(|payload| !(payload.name() == "delay" && payload.ns() == NS_DELAY)));
    }
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
    if request.subscription_type == SubscriptionType::Unsubscribe {
        state
            .deps
            .protocol
            .connection_registry
            .remove_pending_subscribe(&request.to, &request.from);
    }
    if matches!(
        request.subscription_type,
        SubscriptionType::Subscribed | SubscriptionType::Unsubscribed
    ) {
        state
            .deps
            .protocol
            .connection_registry
            .remove_pending_subscribe(&request.from, &request.to);
    }

    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let update = match update_subscription_roster_state(state, &storage, &request).await {
        Ok(Some(update)) => update,
        Ok(None) => {
            debug!(from = %request.from, to = %request.to, kind = ?request.subscription_type, "Ignoring invalid subscription transition");
            return;
        }
        Err(error) => {
            warn!(error = %error, from = %request.from, to = %request.to, "Failed to update roster subscription state");
            return;
        }
    };

    // Roster pushes are now emitted inline by `update_subscription_roster_state`
    // (see PR #336 review on cross-user lock deadlock). Only the post-roster
    // presence side effects remain here.
    if update.auto_approve_subscribe {
        send_existing_subscription_ack(
            state,
            &request.to,
            &request.from,
            request.status.as_deref(),
            &request.payloads,
        )
        .await;
        send_current_presence_from_user_to_user(state, &request.to, &request.from).await;
        return;
    }

    if !update.forward_subscription_stanza {
        return;
    }

    let stanza = Stanza::Presence(build_subscription_presence(
        request.subscription_type,
        &request.from,
        &request.to,
        request.status.as_deref(),
        &request.payloads,
    ));
    if request.subscription_type == SubscriptionType::Subscribe {
        state
            .deps
            .protocol
            .connection_registry
            .queue_pending_subscription_stanza(&request.to, stanza.clone());
    }
    if request.subscription_type == SubscriptionType::Unsubscribed
        && update.send_unavailable_before_unsubscribed
    {
        send_unavailable_presence_from_user_to_user(state, &request.from, &request.to).await;
    }

    let resources = subscription_presence_recipients(state, &request);
    let mut delivered = 0usize;
    let mut delivered_resources = Vec::new();
    if resources.is_empty() && request.subscription_type == SubscriptionType::Subscribe {
        let _recorded = record_stanza_for_detached_available_resources(
            state,
            &request.to,
            &stanza,
            "subscription presence",
        )
        .await;
        state
            .deps
            .protocol
            .connection_registry
            .queue_pending_subscription_stanza(&request.to, stanza);
    } else {
        for resource in &resources {
            if state
                .deps
                .protocol
                .connection_registry
                .send_to(resource, stanza.clone())
                .await
                .is_sent()
            {
                delivered += 1;
                delivered_resources.push(resource.clone());
            }
        }
        delivered += record_subscription_stanza_for_detached_resources_excluding(
            state,
            &request,
            &stanza,
            &delivered_resources,
        )
        .await;
        if delivered == 0 && request.subscription_type == SubscriptionType::Subscribe {
            state
                .deps
                .protocol
                .connection_registry
                .queue_pending_subscription_stanza(&request.to, stanza);
        }
    }

    send_subscription_presence_side_effects(state, &request).await;
}

async fn send_subscription_presence_side_effects(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) {
    match request.subscription_type {
        SubscriptionType::Subscribed => {
            send_current_presence_from_user_to_user(state, &request.from, &request.to).await;
        }
        SubscriptionType::Unsubscribe => {
            send_unavailable_presence_from_user_to_user(state, &request.to, &request.from).await;
        }
        SubscriptionType::Subscribe | SubscriptionType::Unsubscribed => {}
    }
}

async fn send_existing_subscription_ack(
    state: &WebSocketState,
    contact: &BareJid,
    requester: &BareJid,
    status: Option<&str>,
    payloads: &[Element],
) {
    let stanza = Stanza::Presence(build_subscription_presence(
        SubscriptionType::Subscribed,
        contact,
        requester,
        status,
        payloads,
    ));
    let live_resources = state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(requester);
    for resource in &live_resources {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(resource, stanza.clone())
            .await;
    }
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(requester)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %requester, "Failed to list detached interested resources");
            return;
        }
    };
    for resource in detached
        .into_iter()
        .filter(|resource| !live_resources.contains(resource))
    {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&resource, stanza.clone())
                    .await;
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached subscription acknowledgement");
            }
        }
    }
}

pub(super) async fn send_current_presence_from_user_to_user(
    state: &WebSocketState,
    from: &BareJid,
    to: &BareJid,
) {
    for resource in available_live_and_detached_resources_for_user(state, from).await {
        let presence_state = presence_state_for_available_resource(state, &resource).await;
        let stanza = Stanza::Presence(build_available_presence(
            &resource,
            to,
            presence_state
                .as_ref()
                .and_then(|state| state.show.as_ref())
                .map(show_name),
            presence_state
                .as_ref()
                .and_then(|state| state.status.as_deref()),
            presence_state
                .as_ref()
                .map(|state| state.priority)
                .unwrap_or(0),
        ));
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            to,
            &stanza,
            "current presence",
        )
        .await;
    }
}

pub(super) async fn send_unavailable_presence_from_user_to_user(
    state: &WebSocketState,
    from: &BareJid,
    to: &BareJid,
) {
    for resource in available_live_and_detached_resources_for_user(state, from).await {
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(Jid::from(resource));
        presence.to = Some(Jid::from(to.clone()));
        let stanza = Stanza::Presence(presence);
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            to,
            &stanza,
            "unavailable presence",
        )
        .await;
    }
}

pub async fn broadcast_unavailable_for_expired_detached_session(
    state: &WebSocketState,
    from: &FullJid,
) {
    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let from_bare = from.to_bare();
    let subscribers = match storage.get_presence_subscribers(&from_bare).await {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(error = %error, from = %from, "Failed to load presence subscribers for expired detached session");
            return;
        }
    };

    for subscriber in subscribers {
        let Ok(subscriber) = subscriber.parse::<BareJid>() else {
            warn!(
                subscriber,
                "Skipping invalid stored presence subscriber JID"
            );
            continue;
        };
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(Jid::from(from.clone()));
        presence.to = Some(Jid::from(subscriber.clone()));
        let stanza = Stanza::Presence(presence);
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            &subscriber,
            &stanza,
            "expired detached unavailable presence",
        )
        .await;
    }

    let mut presence =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
    presence.from = Some(Jid::from(from.clone()));
    presence.to = Some(Jid::from(from_bare.clone()));
    let stanza = Stanza::Presence(presence);
    send_stanza_to_available_user_resources_and_detached_available(
        state,
        &from_bare,
        &stanza,
        "expired detached unavailable presence to siblings",
    )
    .await;
}

async fn available_live_and_detached_resources_for_user(
    state: &WebSocketState,
    user: &BareJid,
) -> Vec<FullJid> {
    let mut resources: Vec<FullJid> = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(user)
        .into_iter()
        .map(|(jid, _)| jid)
        .collect();
    match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_resources_for_user(user)
        .await
    {
        Ok(detached) => {
            for resource in detached {
                if !resources.contains(&resource) {
                    resources.push(resource);
                }
            }
        }
        Err(error) => {
            warn!(error = %error, user = %user, "Failed to list detached available resources");
        }
    }
    resources
}

async fn presence_state_for_available_resource(
    state: &WebSocketState,
    resource: &FullJid,
) -> Option<PresenceStateSnapshot> {
    if let Some(state) = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(resource)
    {
        return Some(PresenceStateSnapshot {
            show: state.show.as_deref().and_then(show_from_name),
            status: state.status,
            priority: state.priority,
        });
    }
    match state
        .deps
        .protocol
        .sm_session_registry
        .detached_presence_state(resource)
        .await
    {
        Ok(Some((show, status, priority))) => Some(PresenceStateSnapshot {
            show,
            status,
            priority,
        }),
        Ok(None) => None,
        Err(error) => {
            warn!(error = %error, resource = %resource, "Failed to read detached presence state");
            None
        }
    }
}

struct PresenceStateSnapshot {
    show: Option<xmpp_parsers::presence::Show>,
    status: Option<String>,
    priority: i8,
}

async fn send_stanza_to_available_user_resources_and_detached_available(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
) {
    let live_resources: Vec<FullJid> = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(user)
        .into_iter()
        .map(|(jid, _)| jid)
        .collect();
    let mut delivered_resources = Vec::new();
    for resource in &live_resources {
        if state
            .deps
            .protocol
            .connection_registry
            .send_to(resource, stanza.clone())
            .await
            .is_sent()
        {
            delivered_resources.push(resource.clone());
        }
    }
    let recorded_resources = record_stanza_for_detached_available_resources_excluding(
        state,
        user,
        stanza,
        context,
        &delivered_resources,
    )
    .await;
    delivered_resources.extend(recorded_resources);
    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(user)
        .into_iter()
        .map(|(jid, _)| jid)
        .filter(|resource| !delivered_resources.contains(resource))
    {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}

async fn record_stanza_for_detached_available_resources(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
) -> Vec<FullJid> {
    record_stanza_for_detached_available_resources_excluding(state, user, stanza, context, &[])
        .await
}

async fn record_stanza_for_detached_available_resources_excluding(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
    excluded_resources: &[FullJid],
) -> Vec<FullJid> {
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_resources_for_user(user)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %user, context, "Failed to list detached available resources");
            return Vec::new();
        }
    };

    let mut recorded = Vec::new();
    for resource in detached
        .into_iter()
        .filter(|resource| !excluded_resources.contains(resource))
    {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_available_resource(&resource, stanza)
            .await
        {
            Ok(true) => recorded.push(resource.clone()),
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&resource, stanza.clone())
                    .await;
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, context, "Failed to record detached available stanza");
            }
        }
    }
    recorded
}

async fn record_subscription_stanza_for_detached_resources_excluding(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    stanza: &Stanza,
    excluded_resources: &[FullJid],
) -> usize {
    if request.subscription_type == SubscriptionType::Subscribe {
        return record_stanza_for_detached_available_resources_excluding(
            state,
            &request.to,
            stanza,
            "subscription presence",
            excluded_resources,
        )
        .await
        .len();
    }

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(&request.to)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %request.to, "Failed to list detached interested resources");
            return 0;
        }
    };

    let mut recorded = 0;
    for resource in detached
        .into_iter()
        .filter(|resource| !excluded_resources.contains(resource))
    {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, stanza)
            .await
        {
            Ok(true) => recorded += 1,
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&resource, stanza.clone())
                    .await;
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached subscription stanza");
            }
        }
    }
    recorded
}

fn subscription_presence_recipients(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) -> Vec<FullJid> {
    match request.subscription_type {
        SubscriptionType::Subscribe => state
            .deps
            .protocol
            .connection_registry
            .get_available_resources_for_user(&request.to)
            .into_iter()
            .map(|(jid, _)| jid)
            .collect(),
        SubscriptionType::Subscribed
        | SubscriptionType::Unsubscribe
        | SubscriptionType::Unsubscribed => state
            .deps
            .protocol
            .connection_registry
            .get_roster_interested_resources_for_user(&request.to),
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
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) -> Result<Option<SubscriptionRosterUpdate>, crate::db::roster::RosterStorageError> {
    let existing_from_item = load_existing_roster_item(storage, &request.from, &request.to).await?;
    let mut store_from_item = true;
    let mut send_unavailable_before_unsubscribed = false;
    let mut auto_approve_subscribe = false;
    let mut forward_subscription_stanza = true;
    let mut from_item = existing_from_item
        .clone()
        .unwrap_or_else(|| RosterItem::new(request.to.clone()));
    let mut to_item = load_existing_roster_item(storage, &request.to, &request.from).await?;

    match request.subscription_type {
        SubscriptionType::Subscribe => {
            if matches!(
                from_item.subscription,
                Subscription::To | Subscription::Both
            ) {
                send_existing_subscription_ack(
                    state,
                    &request.to,
                    &request.from,
                    request.status.as_deref(),
                    &request.payloads,
                )
                .await;
                return Ok(None);
            }
            if to_item.as_ref().is_some_and(|item| {
                item.approved
                    || matches!(item.subscription, Subscription::From | Subscription::Both)
            }) {
                SubscriptionStateMachine::apply_inbound_subscribed(&mut from_item);
                if let Some(to_item) = to_item.as_mut() {
                    SubscriptionStateMachine::apply_outbound_subscribed(to_item);
                    to_item.approved = false;
                }
                auto_approve_subscribe = true;
            } else {
                SubscriptionStateMachine::apply_outbound_subscribe(&mut from_item);
                to_item = None;
            }
        }
        SubscriptionType::Subscribed => {
            if let Some(to_item) = to_item
                .as_mut()
                .filter(|item| item.ask == Some(AskType::Subscribe))
            {
                SubscriptionStateMachine::apply_outbound_subscribed(&mut from_item);
                from_item.approved = false;
                SubscriptionStateMachine::apply_inbound_subscribed(to_item);
            } else {
                if from_item.subscription == Subscription::Remove {
                    from_item.subscription = Subscription::None;
                }
                from_item.ask = None;
                from_item.approved = true;
                to_item = None;
                forward_subscription_stanza = false;
            }
        }
        SubscriptionType::Unsubscribe => {
            if !matches!(
                from_item.subscription,
                Subscription::To | Subscription::Both
            ) && from_item.ask != Some(AskType::Subscribe)
            {
                return Ok(None);
            }
            SubscriptionStateMachine::apply_outbound_unsubscribe(&mut from_item);
            if let Some(to_item) = to_item.as_mut() {
                SubscriptionStateMachine::apply_outbound_unsubscribed(to_item);
            }
        }
        SubscriptionType::Unsubscribed => {
            let valid_recipient = to_item.as_ref().is_some_and(|item| {
                item.ask == Some(AskType::Subscribe)
                    || matches!(item.subscription, Subscription::To | Subscription::Both)
            });
            let valid_sender = matches!(
                from_item.subscription,
                Subscription::From | Subscription::Both
            );
            let cancel_preapproval = from_item.approved;
            send_unavailable_before_unsubscribed = valid_sender;
            if !valid_recipient && !valid_sender && !cancel_preapproval {
                return Ok(None);
            }
            if valid_sender {
                SubscriptionStateMachine::apply_outbound_unsubscribed(&mut from_item);
            } else if cancel_preapproval {
                from_item.approved = false;
            } else if existing_from_item.is_none() {
                store_from_item = false;
            }
            if let Some(to_item) = to_item.as_mut().filter(|_| valid_recipient) {
                SubscriptionStateMachine::apply_inbound_unsubscribed(to_item);
            }
        }
    }
    // Mutate the from-user's roster, fan out the from-push, drop the lock
    // before touching the to-user's roster. Holding both per-user mutation
    // locks simultaneously could deadlock under concurrent flows that touch
    // the same user pair in opposite roles (PR #336 review). XEP-0237 §2.6
    // ordering only applies *within* a single user's push stream, so
    // emitting from-pushes before to-pushes need not be atomic across users.
    if store_from_item {
        let (mutation, _lock) = storage
            .apply_roster_change(
                &request.from,
                RosterRowChange::Upsert(roster_item_to_row(&from_item)),
            )
            .await?;
        send_roster_push_to_resources(state, &request.from, &from_item, &mutation.version).await;
        // _lock drops at end of this block; from-user's lock released before
        // we acquire to-user's lock below.
    }
    if let Some(to_item) = to_item {
        let (mutation, _lock) = storage
            .apply_roster_change(
                &request.to,
                RosterRowChange::Upsert(roster_item_to_row(&to_item)),
            )
            .await?;
        send_roster_push_to_resources(state, &request.to, &to_item, &mutation.version).await;
    }

    Ok(Some(SubscriptionRosterUpdate {
        send_unavailable_before_unsubscribed,
        auto_approve_subscribe,
        forward_subscription_stanza,
    }))
}

struct SubscriptionRosterUpdate {
    send_unavailable_before_unsubscribed: bool,
    auto_approve_subscribe: bool,
    forward_subscription_stanza: bool,
}

async fn send_roster_push_to_resources(
    state: &WebSocketState,
    user_jid: &BareJid,
    item: &RosterItem,
    version: &RosterVersion,
) {
    let live_resources = state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(user_jid);
    let mut delivered_resources = Vec::new();
    for resource in &live_resources {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            resource,
            item,
            Some(version),
        );
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, Stanza::Iq(push))
            == BroadcastOutcome::Delivered
        {
            delivered_resources.push(resource.clone());
        }
    }
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(user_jid)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %user_jid, "Failed to list detached roster-interested resources");
            return;
        }
    };
    let detached: Vec<_> = detached
        .into_iter()
        .filter(|resource| !delivered_resources.contains(resource))
        .collect();
    for resource in detached {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            &resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(push);
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza)
            .await
        {
            Ok(true) => delivered_resources.push(resource.clone()),
            Ok(false) => {
                let is_interested = state
                    .deps
                    .protocol
                    .connection_registry
                    .is_roster_interested(&resource);
                if is_interested
                    && state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&resource, stanza)
                        == BroadcastOutcome::Delivered
                {
                    delivered_resources.push(resource.clone());
                }
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached roster push");
            }
        }
    }
    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(user_jid)
        .into_iter()
        .filter(|resource| !delivered_resources.contains(resource))
    {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            &resource,
            item,
            Some(version),
        );
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, Stanza::Iq(push));
    }
}

async fn load_existing_roster_item(
    storage: &DatabaseRosterStorage,
    user: &BareJid,
    contact: &BareJid,
) -> Result<Option<RosterItem>, RosterStorageError> {
    match storage.get_roster_item(user, contact).await {
        Ok(Some(row)) => match roster_row_to_item(row) {
            Ok(item) => Ok(Some(item)),
            Err(error) => {
                warn!(user = %user, contact = %contact, error = %error, "Failed to convert roster row");
                Ok(None)
            }
        },
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Debug, thiserror::Error)]
enum RosterConvertError {
    #[error("invalid stored roster jid '{jid}': {error}")]
    Jid { jid: String, error: String },
    #[error("invalid stored roster subscription: {0}")]
    Subscription(String),
    #[error("invalid stored roster ask: {0}")]
    Ask(String),
}

fn roster_row_to_item(row: RosterItemRow) -> Result<RosterItem, RosterConvertError> {
    let jid = row
        .contact_jid
        .parse::<BareJid>()
        .map_err(|error| RosterConvertError::Jid {
            jid: row.contact_jid.clone(),
            error: error.to_string(),
        })?;
    let subscription = row
        .subscription
        .parse::<Subscription>()
        .map_err(|error| RosterConvertError::Subscription(error.to_string()))?;
    let ask = row
        .ask
        .as_deref()
        .map(str::parse::<AskType>)
        .transpose()
        .map_err(|error| RosterConvertError::Ask(error.to_string()))?;

    Ok(RosterItem {
        jid,
        name: row.name,
        subscription,
        ask,
        approved: row.approved,
        groups: row.groups,
    })
}

fn roster_item_to_row(item: &RosterItem) -> RosterItemRow {
    RosterItemRow {
        contact_jid: item.jid.to_string(),
        name: item.name.clone(),
        subscription: subscription_state_str(item.subscription).to_string(),
        ask: item.ask.map(ask_state_str).map(ToOwned::to_owned),
        approved: item.approved,
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

fn ask_state_str(value: AskType) -> &'static str {
    match value {
        AskType::Subscribe => "subscribe",
    }
}

async fn handle_presence_probe(
    state: &WebSocketState,
    from: BareJid,
    to: BareJid,
    to_full: Option<FullJid>,
) {
    if recipient_blocks_sender(state, &to, &from).await {
        info!(requester = %from, target = %to, "Blocked presence probe");
        return;
    }
    if !presence_probe_authorized(state, &from, &to).await {
        info!(requester = %from, target = %to, "Unauthorized presence probe");
        send_unsubscribed_probe_response(state, &to, &from).await;
        return;
    }
    let mut available = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(&to);
    let mut detached_available = match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_presence_states_for_user(&to)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, target = %to, "Failed to list detached resources for presence probe");
            Vec::new()
        }
    };
    if let Some(to_full) = &to_full {
        available.retain(|(resource, _)| resource == to_full);
        detached_available.retain(|(resource, _, _, _)| resource == to_full);
    }
    detached_available.retain(|(resource, _, _, _)| {
        !available
            .iter()
            .any(|(live_resource, _)| live_resource == resource)
    });
    if available.is_empty() && detached_available.is_empty() {
        let unavailable = Stanza::Presence(if let Some(to_full) = &to_full {
            let mut presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
            presence.from = Some(Jid::from(to_full.clone()));
            presence.to = Some(Jid::from(from.clone()));
            presence
        } else {
            build_unavailable_presence(&to, &from)
        });
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
    for (resource, show, status, priority) in detached_available {
        let presence = Stanza::Presence(build_available_presence(
            &resource,
            &from,
            show.as_ref().map(show_name),
            status.as_deref(),
            priority,
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

async fn send_unsubscribed_probe_response(state: &WebSocketState, from: &BareJid, to: &BareJid) {
    let stanza = Stanza::Presence(build_subscription_presence(
        SubscriptionType::Unsubscribed,
        from,
        to,
        None,
        &[],
    ));
    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(to)
    {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}

async fn presence_probe_authorized(state: &WebSocketState, from: &BareJid, to: &BareJid) -> bool {
    if from == to {
        return true;
    }
    let Some(storage) = roster_storage(state).await else {
        return false;
    };
    match storage.get_roster_item(from, to).await {
        Ok(Some(row)) => SubscriptionStateMachine::should_receive_presence(
            parse_subscription_state(&row.subscription),
        ),
        Ok(None) => false,
        Err(error) => {
            warn!(error = %error, requester = %from, target = %to, "Failed to authorize presence probe");
            false
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
        // XEP-0160 §3 step 5 (locked Q7a/Q7d): on the first non-negative-
        // priority presence of a fresh session, drain pending_delivery
        // for the recipient. `claim_offline_flush` ensures this fires at
        // most once per session even across priority transitions.
        if priority >= 0 {
            maybe_flush_pending_delivery(state, sender_jid).await;
        }
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
            .pending_subscription_stanzas(&sender_jid.to_bare())
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

/// XEP-0160 §3 step 5 + locked Q7a / Q7c / Q7d: on the recovering
/// session's first non-negative-priority presence, drain
/// `pending_delivery` for the user's bare JID and push each row to
/// this resource.
///
/// `ConnectionEntry::claim_offline_flush()` is a CAS that returns
/// `true` exactly once per fresh session — repeated presence updates
/// (priority transitions, status text changes) do not re-flush an
/// already-drained queue.
async fn maybe_flush_pending_delivery(state: &WebSocketState, sender_jid: &FullJid) {
    let entry = match state
        .deps
        .protocol
        .connection_registry
        .get_entry(sender_jid)
    {
        Some(entry) => entry,
        None => return,
    };
    if !entry.claim_offline_flush() {
        return;
    }
    let recipient_bare = sender_jid.to_bare();
    let resolver = crate::pending_delivery::MamArchiveResolver {
        mam_storage: std::sync::Arc::clone(&state.deps.protocol.mam_storage),
    };
    // Locked Q7b SM-ack lifecycle (issue #209): when the recovering
    // connection has an active XEP-0198 session, key claims by its
    // stream id so a subsequent `<a h>` from the same session deletes
    // exactly its acked rows. Without SM, the flush function falls
    // back to delete-on-push (no ack will ever fire).
    let sm_session_id = entry.sm_stream_id();
    let outcome = crate::pending_delivery::flush_for_resource(
        &state.deps.protocol.pending_delivery_storage,
        &state.deps.protocol.connection_registry,
        state.deps.auth_state.xmpp_domain.as_str(),
        &recipient_bare,
        sender_jid,
        sm_session_id.as_ref(),
        &resolver,
    )
    .await;
    if outcome.claimed > 0 {
        debug!(
            jid = %sender_jid,
            claimed = outcome.claimed,
            pushed = outcome.pushed,
            unresolved = outcome.unresolved,
            "XEP-0160 pending_delivery flush completed"
        );
    }
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
        if recipient_blocks_sender(state, &subscriber_bare, &sender_jid.to_bare()).await {
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
            let mut unavailable = presence.clone();
            unavailable.from = Some(Jid::from(sender_jid.clone()));
            unavailable.to = Some(Jid::from(subscriber_bare.clone()));
            Stanza::Presence(unavailable)
        };
        let mut delivered_resources = Vec::new();
        for resource in state
            .deps
            .protocol
            .connection_registry
            .get_available_resources_for_user(&subscriber_bare)
            .into_iter()
            .map(|(jid, _)| jid)
        {
            if state
                .deps
                .protocol
                .connection_registry
                .send_to(&resource, stanza.clone())
                .await
                .is_sent()
            {
                delivered_resources.push(resource);
            }
        }
        record_stanza_for_detached_available_resources_excluding(
            state,
            &subscriber_bare,
            &stanza,
            "presence broadcast",
            &delivered_resources,
        )
        .await;
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

fn show_from_name(value: &str) -> Option<xmpp_parsers::presence::Show> {
    match value {
        "away" => Some(xmpp_parsers::presence::Show::Away),
        "chat" => Some(xmpp_parsers::presence::Show::Chat),
        "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
        "xa" => Some(xmpp_parsers::presence::Show::Xa),
        _ => None,
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
            if managed_channel.is_none()
                && !server_permission_allowed(
                    state,
                    authenticated_session.as_ref(),
                    Permission::CreateMuc,
                )
                .await
                .unwrap_or(false)
            {
                return vec![build_muc_join_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    "cancel",
                    "not-allowed",
                )];
            }

            let config = managed_channel
                .as_ref()
                .map(|channel| RoomConfig {
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                    members_only: true,
                    moderated: channel.channel_type == "announcement",
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
        responses.push(build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &state.deps.occupant_id_secret,
            room_jid,
            nick: &existing.nick,
            to_jid: sender_jid,
            affiliation: existing.affiliation,
            role: existing.role,
            real_jid: &existing.jid,
            include_self_status: false,
        }));
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
                state,
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
    responses.push(build_muc_join_presence_xml(MucJoinPresence {
        occupant_id_secret: &state.deps.occupant_id_secret,
        room_jid,
        nick,
        to_jid: sender_jid,
        affiliation: join_outcome.new_occupant_affiliation,
        role: join_outcome.new_occupant_role,
        real_jid: sender_jid,
        include_self_status: true,
    }));

    // XEP-0045 §7.2.15 historical room subject. The typed builder
    // produces the conformant envelope: nick-form `from` + `<delay/>`
    // + XEP-0421 `<occupant-id/>` when a setter is known, or bare-from
    // empty `<subject/>` for a never-set room (matching the established
    // resolution of XEP-0421 §3 vs §7.2.15 on never-set rooms).
    let subject_msg = build_subject_message(
        room_jid,
        sender_jid,
        join_outcome.subject_state.as_ref(),
        &state.deps.occupant_id_secret,
    );
    responses.push(stanza_to_xml(&Stanza::Message(subject_msg)));

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

    if matches!(channel_id, "chat" | "announcements") {
        let server = Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID);
        if check_channel_permission(state, server.clone(), subject.clone(), Permission::Owner)
            .await?
        {
            return Ok(Some(Affiliation::Owner));
        }
        if check_channel_permission(state, server, subject.clone(), Permission::Member).await? {
            return Ok(Some(Affiliation::Member));
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
        return vec![build_muc_self_unavailable_xml(
            state, room_jid, nick, sender_jid,
        )];
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
            return vec![build_muc_self_unavailable_xml(
                state, room_jid, nick, sender_jid,
            )];
        }
        Err(error) => {
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room");
            return vec![build_muc_self_unavailable_xml(
                state, room_jid, nick, sender_jid,
            )];
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
            let sender_bare = sender_jid.to_bare();
            let presence = waddle_xmpp::muc::build_leave_presence(
                &from_jid,
                occupant_jid,
                Affiliation::Member,
                false,
                &waddle_xmpp::xep::xep0421::OccupantIdentity {
                    bare_jid: &sender_bare,
                    real_jid: Some(sender_jid),
                    secret: &state.deps.occupant_id_secret,
                },
            );
            let stanza = Stanza::Presence(presence);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(occupant_jid, stanza);
        }
    }

    vec![build_muc_self_unavailable_xml(
        state,
        room_jid,
        &outcome.nick,
        sender_jid,
    )]
}

struct MucJoinPresence<'a> {
    occupant_id_secret: &'a waddle_xmpp::xep::xep0421::OccupantIdSecret,
    room_jid: &'a BareJid,
    nick: &'a str,
    to_jid: &'a FullJid,
    affiliation: Affiliation,
    role: Role,
    real_jid: &'a FullJid,
    include_self_status: bool,
}

fn build_muc_join_presence_xml(params: MucJoinPresence<'_>) -> String {
    let presence = build_muc_join_presence_stanza(params);
    stanza_to_xml(&Stanza::Presence(presence))
}

fn build_muc_join_presence_stanza(params: MucJoinPresence<'_>) -> xmpp_parsers::presence::Presence {
    let from_jid = params
        .room_jid
        .clone()
        .with_resource_str(params.nick)
        .unwrap_or_else(|_| params.to_jid.clone());
    let real_bare = params.real_jid.to_bare();
    waddle_xmpp::muc::build_occupant_presence(
        &from_jid,
        params.to_jid,
        params.affiliation,
        params.role,
        params.include_self_status,
        &waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &real_bare,
            real_jid: Some(params.real_jid),
            secret: params.occupant_id_secret,
        },
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

fn build_muc_self_unavailable_xml(
    state: &WebSocketState,
    room_jid: &BareJid,
    nick: &str,
    sender_jid: &FullJid,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| sender_jid.clone());

    let sender_bare = sender_jid.to_bare();
    let presence = waddle_xmpp::muc::build_leave_presence(
        &from_jid,
        sender_jid,
        Affiliation::Member,
        true,
        &waddle_xmpp::xep::xep0421::OccupantIdentity {
            bare_jid: &sender_bare,
            real_jid: Some(sender_jid),
            secret: &state.deps.occupant_id_secret,
        },
    );
    stanza_to_xml(&Stanza::Presence(presence))
}

async fn server_permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    permission: Permission,
) -> Result<bool, ()> {
    let Some(session) = session else {
        return Ok(false);
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&session.user_id),
            permission,
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        })
        .await
        .map(|response| response.allowed)
        .map_err(|error| {
            warn!(error = %error, "Failed to authorize MUC creation");
        })
}

/// Create a presence stanza for MUC
fn create_presence_stanza(
    state: &WebSocketState,
    room_jid: &BareJid,
    nick: &str,
    real_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
) -> xmpp_parsers::presence::Presence {
    build_muc_join_presence_stanza(MucJoinPresence {
        occupant_id_secret: &state.deps.occupant_id_secret,
        room_jid,
        nick,
        to_jid,
        affiliation,
        role,
        real_jid,
        include_self_status: false,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn muc_join_presence_includes_owner_and_moderator_hats() {
        let secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"join-presence-handler-test-secret".to_vec(),
        )
        .expect("test secret meets length floor");
        let room_jid: BareJid = "chat@muc.example.com".parse().unwrap();
        let to_jid: FullJid = "alice@example.com/web".parse().unwrap();
        let real_jid: FullJid = "bob@example.com/mobile".parse().unwrap();

        let xml = build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &secret,
            room_jid: &room_jid,
            nick: "bob",
            to_jid: &to_jid,
            affiliation: Affiliation::Owner,
            role: Role::Moderator,
            real_jid: &real_jid,
            include_self_status: false,
        });

        assert!(
            xml.contains("xmlns=\"urn:xmpp:hats:0\"") || xml.contains("xmlns='urn:xmpp:hats:0'")
        );
        assert!(
            xml.contains("uri=\"urn:xmpp:hats:owner\"")
                || xml.contains("uri='urn:xmpp:hats:owner'")
        );
        assert!(
            xml.contains("uri=\"urn:xmpp:hats:moderator\"")
                || xml.contains("uri='urn:xmpp:hats:moderator'")
        );
        assert!(
            xml.contains("<occupant-id") && xml.contains("urn:xmpp:occupant-id:0"),
            "typed join presence builder must stamp XEP-0421 occupant-id: {xml}"
        );
    }
}
