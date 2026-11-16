//! Presence between local accounts and remote contacts (ADR-0020 D7; gate
//! G18; RFC 6121 §3 and §4).
//!
//! Only the local account's half of a subscription is this server's: the
//! contact's roster is on its own server. Every function here reads and
//! writes the account's roster item for the remote contact and never
//! creates one for an inbound request (RFC 6121 §3.1.3, security warning).
//! Inbound stanzas reach this module only through the account side
//! (`s2s_accounts`), which has decided the sender's admission first; the
//! rules and their reasons are recorded in
//! `server/docs/planning/s2s-account-traffic-2047.md`.

use jid::{BareJid, FullJid, Jid};
use tracing::warn;
use waddle_xmpp::presence::subscription::{
    build_available_presence, build_subscription_presence, SubscriptionStateMachine,
    SubscriptionType,
};
use waddle_xmpp::roster::{AskType, RosterItem, Subscription};
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::subscription::{load_existing_roster_item, roster_item_to_row, roster_storage};
use crate::db::roster::{DatabaseRosterStorage, RosterRowChange};
use crate::db::s2s_accounts::{PendingOutcome, PendingSubscriptions};
use crate::destination::{
    offer_stanza, unroutable_reply, RemoteDestination, StanzaOffer, UnservedDestination,
    REMOTE_ROUTE_OFFER_TIMEOUT,
};
use crate::s2s::route::{RemotePrincipal, MAX_ACCOUNT_REPLIES};
use crate::server::routes::websocket::handlers::account_reads;
use crate::server::routes::websocket::handlers::roster_fanout::send_roster_push_to_resources;
use crate::server::routes::websocket::handlers::unserved_route::unserved_destination;
use crate::server::routes::websocket::s2s_accounts::{
    account_exists, admits_domain, blocks, database, deliver, s2s_enabled, telemetry,
};
use crate::server::routes::websocket::transport_xml::stanza_to_xml;
use crate::server::routes::websocket::WebSocketState;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn pending_store(state: &WebSocketState) -> Option<PendingSubscriptions> {
    database(state).await.map(PendingSubscriptions::new)
}

/// Store `item` as `account`'s roster item and push it to the account's
/// interested resources (RFC 6121 §2.1.6), holding the account's roster
/// lock until the push is queued.
async fn store_and_push(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    item: &RosterItem,
) {
    match storage
        .apply_roster_change(account, RosterRowChange::Upsert(roster_item_to_row(item)))
        .await
    {
        Ok((mutation, _lock)) => {
            send_roster_push_to_resources(state, account, item, &mutation.version, None).await;
        }
        Err(error) => warn!(%error, "storing a roster item for a remote contact failed"),
    }
}

/// Deliver `stanza` to each of `resources`.
async fn deliver_to(state: &WebSocketState, resources: &[FullJid], stanza: &Stanza) {
    for resource in resources {
        let _ = deliver(state, resource, stanza).await;
    }
}

async fn available(state: &WebSocketState, account: &BareJid) -> Vec<FullJid> {
    account_reads::available_resources(state, account)
        .await
        .into_iter()
        .map(|(resource, _)| resource)
        .collect()
}

/// The current presence of each of `account`'s available resources,
/// addressed to `contact`, with each resource's stored extension payloads
/// (XEP-0115 caps among them), at most [`MAX_ACCOUNT_REPLIES`].
async fn current_presence(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> Vec<Stanza> {
    let mut presences = Vec::new();
    for resource in available(state, account)
        .await
        .into_iter()
        .take(MAX_ACCOUNT_REPLIES)
    {
        let stored = account_reads::presence_state(state, &resource).await;
        let mut presence = build_available_presence(
            &resource,
            contact,
            stored.as_ref().and_then(|stored| stored.show.as_deref()),
            stored.as_ref().and_then(|stored| stored.status.as_deref()),
            stored.as_ref().map_or(0, |stored| stored.priority),
        );
        if let Some(stored) = stored {
            presence.payloads.extend(stored.payloads);
        }
        presences.push(Stanza::Presence(presence));
    }
    presences
}

/// `unavailable` from each of `account`'s available resources to `contact`,
/// at most [`MAX_ACCOUNT_REPLIES`].
async fn unavailable_from_each(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> Vec<Stanza> {
    available(state, account)
        .await
        .into_iter()
        .take(MAX_ACCOUNT_REPLIES)
        .map(|resource| {
            let mut presence = Presence::new(PresenceType::Unavailable);
            presence.from = Some(Jid::from(resource));
            presence.to = Some(Jid::from(contact.clone()));
            Stanza::Presence(presence)
        })
        .collect()
}

fn has(item: Option<&RosterItem>, states: &[Subscription]) -> bool {
    item.is_some_and(|item| states.contains(&item.subscription))
}

// --- inbound ---------------------------------------------------------------

/// A presence stanza from a remote principal to the local account address
/// `to`, and the stanzas that answer it.
pub(crate) async fn receive_remote_presence(
    state: &WebSocketState,
    principal: &RemotePrincipal,
    to: &Jid,
    mut presence: Presence,
) -> Vec<Stanza> {
    if presence.type_ == PresenceType::Error {
        return Vec::new();
    }
    let account = to.to_bare();
    if !account_exists(state, &account).await || blocks(state, &account, principal.address()).await
    {
        return Vec::new();
    }
    let contact = principal.account().as_bare_jid().clone();
    let Some(storage) = roster_storage(state).await else {
        return Vec::new();
    };
    let item = match load_existing_roster_item(&storage, &account, &contact).await {
        Ok(item) => item,
        Err(error) => {
            warn!(%error, "reading a roster item for a remote contact failed");
            return Vec::new();
        }
    };
    match presence.type_ {
        PresenceType::None | PresenceType::Unavailable => {
            if let Ok(full) = to.clone().try_into_full() {
                // Directed presence to one resource (RFC 6121 §8.5.3.1).
                let _ = deliver(state, &full, &Stanza::Presence(presence)).await;
            } else if has(item.as_ref(), &[Subscription::To, Subscription::Both]) {
                let stanza = Stanza::Presence(presence);
                deliver_to(state, &available(state, &account).await, &stanza).await;
            }
            Vec::new()
        }
        PresenceType::Probe => answer_probe(state, &account, to, &contact, item.as_ref()).await,
        PresenceType::Subscribe
        | PresenceType::Subscribed
        | PresenceType::Unsubscribe
        | PresenceType::Unsubscribed => {
            // RFC 6121 §3.1.3: a subscription stanza names the bare JIDs.
            presence.from = Some(Jid::from(contact.clone()));
            presence.to = Some(Jid::from(account.clone()));
            receive_subscription(state, &storage, &account, &contact, item, presence).await
        }
        PresenceType::Error => Vec::new(),
    }
}

/// A probe (RFC 6121 §4.3.2), answered only for a contact the account lets
/// see its presence; anybody else, including a blocked sender, receives
/// nothing.
async fn answer_probe(
    state: &WebSocketState,
    account: &BareJid,
    to: &Jid,
    contact: &BareJid,
    item: Option<&RosterItem>,
) -> Vec<Stanza> {
    if !has(item, &[Subscription::From, Subscription::Both]) {
        return Vec::new();
    }
    let resources = available(state, account).await;
    if let Ok(full) = to.clone().try_into_full() {
        // Only the resource probed, and only the fact that it is available.
        let type_ = if resources.contains(&full) {
            PresenceType::None
        } else {
            PresenceType::Unavailable
        };
        let mut presence = Presence::new(type_);
        presence.from = Some(Jid::from(full));
        presence.to = Some(Jid::from(contact.clone()));
        return vec![Stanza::Presence(presence)];
    }
    if resources.is_empty() {
        let mut presence = Presence::new(PresenceType::Unavailable);
        presence.from = Some(Jid::from(account.clone()));
        presence.to = Some(Jid::from(contact.clone()));
        return vec![Stanza::Presence(presence)];
    }
    current_presence(state, account, contact).await
}

/// An inbound subscription stanza from `contact` to `account` (RFC 6121
/// §3.1.3, §3.1.6, §3.2.3, §3.3.3).
async fn receive_subscription(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    contact: &BareJid,
    item: Option<RosterItem>,
    presence: Presence,
) -> Vec<Stanza> {
    let stanza = Stanza::Presence(presence.clone());
    match presence.type_ {
        PresenceType::Subscribe => {
            if has(item.as_ref(), &[Subscription::From, Subscription::Both]) {
                // Already approved: answered on the account's behalf.
                return vec![subscribed(account, contact)];
            }
            if let Some(mut item) = item.filter(|item| item.approved) {
                // Pre-approved (§3.4): approved on the account's behalf.
                SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
                item.approved = false;
                store_and_push(state, storage, account, &item).await;
                let mut replies = vec![subscribed(account, contact)];
                replies.extend(current_presence(state, account, contact).await);
                replies.truncate(MAX_ACCOUNT_REPLIES);
                return replies;
            }
            let Some(pending) = pending_store(state).await else {
                return Vec::new();
            };
            match pending
                .record(
                    account,
                    contact,
                    &minidom::Element::from(presence),
                    now_ms(),
                )
                .await
            {
                Ok(PendingOutcome::Recorded) => {
                    deliver_to(state, &available(state, account).await, &stanza).await;
                }
                Ok(PendingOutcome::Bounds) => telemetry::pending_dropped(),
                Err(error) => warn!(%error, "recording a remote subscription request failed"),
            }
            Vec::new()
        }
        PresenceType::Subscribed => {
            // Only a request the account made can be approved (§3.1.6).
            let Some(mut item) = item.filter(|item| item.ask == Some(AskType::Subscribe)) else {
                return Vec::new();
            };
            SubscriptionStateMachine::apply_inbound_subscribed(&mut item);
            deliver_to(
                state,
                &account_reads::roster_interested(state, account).await,
                &stanza,
            )
            .await;
            store_and_push(state, storage, account, &item).await;
            Vec::new()
        }
        PresenceType::Unsubscribe => {
            if let Some(pending) = pending_store(state).await {
                let _ = pending.remove(account, contact).await;
            }
            let Some(mut item) = item.filter(|item| {
                matches!(item.subscription, Subscription::From | Subscription::Both)
            }) else {
                return Vec::new();
            };
            SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
            deliver_to(
                state,
                &account_reads::roster_interested(state, account).await,
                &stanza,
            )
            .await;
            store_and_push(state, storage, account, &item).await;
            unavailable_from_each(state, account, contact).await
        }
        PresenceType::Unsubscribed => {
            let Some(mut item) = item.filter(|item| {
                item.ask == Some(AskType::Subscribe)
                    || matches!(item.subscription, Subscription::To | Subscription::Both)
            }) else {
                return Vec::new();
            };
            SubscriptionStateMachine::apply_inbound_unsubscribed(&mut item);
            deliver_to(
                state,
                &account_reads::roster_interested(state, account).await,
                &stanza,
            )
            .await;
            store_and_push(state, storage, account, &item).await;
            Vec::new()
        }
        PresenceType::None
        | PresenceType::Unavailable
        | PresenceType::Probe
        | PresenceType::Error => Vec::new(),
    }
}

fn subscribed(account: &BareJid, contact: &BareJid) -> Stanza {
    Stanza::Presence(build_subscription_presence(
        SubscriptionType::Subscribed,
        account,
        contact,
        None,
        &[],
    ))
}

/// The subscription requests remote senders sent `account` that it has not
/// answered, to deliver on the first available presence of a new session
/// (RFC 6121 §3.1.3 rule 4). A request from a domain the replica no
/// longer admits is not delivered.
pub(crate) async fn pending_remote_subscriptions(
    state: &WebSocketState,
    account: &BareJid,
) -> Vec<Stanza> {
    if !s2s_enabled(state) {
        return Vec::new();
    }
    let Some(pending) = pending_store(state).await else {
        return Vec::new();
    };
    let requests = match pending.list(account).await {
        Ok(requests) => requests,
        Err(error) => {
            warn!(%error, "reading remote subscription requests failed");
            return Vec::new();
        }
    };
    requests
        .into_iter()
        .filter(|(contact, _)| admits_domain(state, contact.domain()))
        .filter_map(|(_, stanza)| Presence::try_from(stanza).ok().map(Stanza::Presence))
        .collect()
}

// --- outbound --------------------------------------------------------------

/// The remote destination of `to`, when it is at a domain this deployment
/// does not serve.
fn remote_destination(state: &WebSocketState, to: &Jid) -> Option<RemoteDestination> {
    match unserved_destination(Some(to), state.deps.auth_state.xmpp_domain.as_str(), state)? {
        UnservedDestination::Remote(remote) => Some(remote),
        UnservedDestination::Invalid(_) => None,
    }
}

/// Whether `contact` is at a domain this deployment does not serve.
pub(crate) fn is_remote_contact(state: &WebSocketState, contact: &BareJid) -> bool {
    remote_destination(state, &Jid::from(contact.clone())).is_some()
}

/// Offer presence the server sends on an account's behalf to a remote JID.
/// `true` when the route took it; nothing answers a refusal.
pub(crate) async fn offer_presence(state: &WebSocketState, presence: Presence) -> bool {
    let Some(to) = presence.to.clone() else {
        return false;
    };
    let Some(remote) = remote_destination(state, &to) else {
        return false;
    };
    let Some(home) = presence.from.as_ref().map(|from| from.domain().to_owned()) else {
        return false;
    };
    matches!(
        offer_stanza(
            state.deps.remote_route.as_ref(),
            remote,
            Stanza::Presence(presence),
            &home,
            REMOTE_ROUTE_OFFER_TIMEOUT,
        )
        .await,
        StanzaOffer::Accepted
    )
}

/// What became of an outbound subscription stanza offered to the route.
enum Routed {
    Accepted,
    Refused(Vec<String>),
}

async fn route(
    state: &WebSocketState,
    sender: &FullJid,
    remote: RemoteDestination,
    presence: Presence,
) -> Routed {
    match offer_stanza(
        state.deps.remote_route.as_ref(),
        remote,
        Stanza::Presence(presence),
        sender.domain(),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await
    {
        StanzaOffer::Accepted => Routed::Accepted,
        StanzaOffer::Unattributed => Routed::Refused(Vec::new()),
        StanzaOffer::Refused { reply, .. } => Routed::Refused(
            reply
                .map(|reply| stanza_to_xml(&reply))
                .into_iter()
                .collect(),
        ),
    }
}

/// A bound client's subscription stanza to a remote contact, with S2S
/// enabled (RFC 6121 §3.1.2, §3.1.5, §3.2.2, §3.3.2, §3.4): stamped with
/// the account's bare JID and addressed to the contact's, routed, and the
/// account's half of the subscription changed only once the route took it.
/// Returns the frames for the client: the error answering a refused one.
pub(crate) async fn route_remote_subscription(
    state: &WebSocketState,
    sender: &FullJid,
    mut presence: Presence,
    remote: RemoteDestination,
) -> Vec<String> {
    let account = sender.to_bare();
    let contact = remote.address().to_bare();
    // RFC 6121 §3.1.2: a request to a full JID is for the bare JID.
    let remote = remote_destination(state, &Jid::from(contact.clone())).unwrap_or(remote);
    presence.from = Some(Jid::from(account.clone()));
    presence.to = Some(Jid::from(contact.clone()));
    let Some(storage) = roster_storage(state).await else {
        return lookup_failed(&presence, sender);
    };
    let item = match load_existing_roster_item(&storage, &account, &contact).await {
        Ok(item) => item,
        Err(error) => {
            warn!(%error, "reading a roster item for a remote contact failed");
            return lookup_failed(&presence, sender);
        }
    };
    let pending = pending_store(state).await;
    match presence.type_ {
        PresenceType::Subscribe => {
            match route(state, sender, remote, presence).await {
                Routed::Refused(frames) => return frames,
                Routed::Accepted => {}
            }
            if !has(item.as_ref(), &[Subscription::To, Subscription::Both]) {
                let mut item = item.unwrap_or_else(|| RosterItem::new(contact.clone()));
                SubscriptionStateMachine::apply_outbound_subscribe(&mut item);
                store_and_push(state, &storage, &account, &item).await;
            }
            Vec::new()
        }
        PresenceType::Unsubscribe => {
            match route(state, sender, remote, presence).await {
                Routed::Refused(frames) => return frames,
                Routed::Accepted => {}
            }
            if let Some(mut item) = item.filter(|item| {
                item.ask == Some(AskType::Subscribe)
                    || matches!(item.subscription, Subscription::To | Subscription::Both)
            }) {
                SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);
                store_and_push(state, &storage, &account, &item).await;
            }
            Vec::new()
        }
        PresenceType::Subscribed => {
            let requested = match &pending {
                Some(pending) => pending.holds(&account, &contact).await.unwrap_or(false),
                None => false,
            };
            if !requested && !has(item.as_ref(), &[Subscription::From, Subscription::Both]) {
                // No request to approve: a pre-approval (§3.4.2), which
                // nothing leaves for.
                let mut item = item.unwrap_or_else(|| RosterItem::new(contact.clone()));
                item.approved = true;
                store_and_push(state, &storage, &account, &item).await;
                return Vec::new();
            }
            match route(state, sender, remote, presence).await {
                Routed::Refused(frames) => return frames,
                Routed::Accepted => {}
            }
            if requested {
                let mut item = item.unwrap_or_else(|| RosterItem::new(contact.clone()));
                SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
                item.approved = false;
                store_and_push(state, &storage, &account, &item).await;
                if let Some(pending) = &pending {
                    let _ = pending.remove(&account, &contact).await;
                }
                // §3.1.5: the contact receives the account's current presence.
                for stanza in current_presence(state, &account, &contact).await {
                    if let Stanza::Presence(presence) = stanza {
                        let _ = offer_presence(state, presence).await;
                    }
                }
            }
            Vec::new()
        }
        PresenceType::Unsubscribed => {
            let requested = match &pending {
                Some(pending) => pending.holds(&account, &contact).await.unwrap_or(false),
                None => false,
            };
            if has(item.as_ref(), &[Subscription::From, Subscription::Both]) {
                // §3.2.2: unavailable from every available resource first.
                for stanza in unavailable_from_each(state, &account, &contact).await {
                    if let Stanza::Presence(presence) = stanza {
                        let _ = offer_presence(state, presence).await;
                    }
                }
                match route(state, sender, remote, presence).await {
                    Routed::Refused(frames) => return frames,
                    Routed::Accepted => {}
                }
                if let Some(mut item) = item {
                    SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
                    store_and_push(state, &storage, &account, &item).await;
                }
            } else if requested {
                // Denying a request (§3.1.4).
                match route(state, sender, remote, presence).await {
                    Routed::Refused(frames) => return frames,
                    Routed::Accepted => {}
                }
            } else if let Some(mut item) = item.filter(|item| item.approved) {
                // Cancelling a pre-approval: nothing leaves (§3.2.2 rule 2).
                item.approved = false;
                store_and_push(state, &storage, &account, &item).await;
            }
            if let Some(pending) = &pending {
                let _ = pending.remove(&account, &contact).await;
            }
            Vec::new()
        }
        PresenceType::None
        | PresenceType::Unavailable
        | PresenceType::Probe
        | PresenceType::Error => Vec::new(),
    }
}

/// The retryable error answering a subscription stanza whose roster could
/// not be read (RFC 6120 §8.3).
fn lookup_failed(presence: &Presence, sender: &FullJid) -> Vec<String> {
    let mut reply = Presence::new(PresenceType::Error);
    reply.id = presence.id.clone();
    reply.from = presence.to.clone();
    reply.to = Some(Jid::from(sender.clone()));
    reply.payloads.push(minidom::Element::from(StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "en",
        "Contact lookup failed; retry later.",
    )));
    vec![stanza_to_xml(&Stanza::Presence(reply))]
}

/// RFC 6121 §2.5.2: `account` removed `item`, a remote contact, from its
/// roster. The contact is sent `unsubscribe` if the account was subscribed
/// to it or asked to be, and `unsubscribed` if it was subscribed to the
/// account; a request it sent is forgotten.
pub(crate) async fn remove_remote_contact(
    state: &WebSocketState,
    account: &BareJid,
    item: &RosterItem,
) {
    if let Some(pending) = pending_store(state).await {
        let _ = pending.remove(account, &item.jid).await;
    }
    let mut types = Vec::new();
    if item.ask == Some(AskType::Subscribe)
        || matches!(item.subscription, Subscription::To | Subscription::Both)
    {
        types.push(SubscriptionType::Unsubscribe);
    }
    if matches!(item.subscription, Subscription::From | Subscription::Both) {
        types.push(SubscriptionType::Unsubscribed);
    }
    for type_ in types {
        let presence = build_subscription_presence(type_, account, &item.jid, None, &[]);
        let _ = offer_presence(state, presence).await;
    }
}

/// RFC 6121 §4.2.2 and §4.3.1: on the first available presence of a
/// session, probe each remote contact whose presence `account` is
/// subscribed to, from the account's bare JID.
pub(crate) async fn probe_remote_contacts(state: &WebSocketState, account: &BareJid) {
    if !s2s_enabled(state) {
        return;
    }
    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let roster = match storage.get_roster(account).await {
        Ok(roster) => roster,
        Err(error) => {
            warn!(%error, "reading the roster to probe remote contacts failed");
            return;
        }
    };
    for row in roster {
        if !matches!(row.subscription.as_str(), "to" | "both") {
            continue;
        }
        let Ok(contact) = row.contact_jid.parse::<BareJid>() else {
            continue;
        };
        if !is_remote_contact(state, &contact) {
            continue;
        }
        // XEP-0191 §3.3: nothing is routed to a JID the account blocks.
        if blocks(state, account, &Jid::from(contact.clone())).await {
            continue;
        }
        let mut probe = Presence::new(PresenceType::Probe);
        probe.from = Some(Jid::from(account.clone()));
        probe.to = Some(Jid::from(contact));
        let _ = offer_presence(state, probe).await;
    }
}

/// The error answering a client's message or presence to a remote address
/// on its own blocking list (XEP-0191 §3.3), from that address; nothing is
/// routed. `None` for an error stanza, which is never answered.
pub(crate) fn outgoing_block_reply(stanza: &Stanza, sender: &FullJid) -> Option<Stanza> {
    let reply = unroutable_reply(
        stanza,
        &Jid::from(sender.clone()),
        sender.domain(),
        crate::destination::UnroutableReason::RefusedByPolicy,
    )?;
    let mut error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAcceptable,
        "en",
        "Recipient is on your blocklist.",
    );
    error.other = Some(
        minidom::Element::builder(
            "blocked",
            waddle_xmpp::protocol::handlers::errors::NS_BLOCKING_ERRORS,
        )
        .build(),
    );
    error.by = Some(Jid::from(BareJid::from_parts(None, sender.domain())));
    let error = minidom::Element::from(error);
    Some(match reply {
        Stanza::Message(mut message) => {
            message.payloads = vec![error];
            Stanza::Message(message)
        }
        Stanza::Presence(mut presence) => {
            presence.payloads = vec![error];
            Stanza::Presence(presence)
        }
        Stanza::Iq(iq) => Stanza::Iq(iq),
    })
}

/// Whether the sender's own blocking list holds `to`, for a stanza to a
/// remote address with S2S enabled.
pub(crate) async fn sender_blocks(state: &WebSocketState, sender: &FullJid, to: &Jid) -> bool {
    s2s_enabled(state) && blocks(state, &sender.to_bare(), to).await
}
