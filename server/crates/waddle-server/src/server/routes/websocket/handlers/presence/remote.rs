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
//!
//! The read-modify-write of an item runs under the account's lock
//! (`s2s_accounts::lock_account`), inbound and outbound alike, and an
//! inbound stanza's work applies nothing once the router has abandoned it.
//! A reply that announces a change (`subscribed`, current or unavailable
//! presence) is sent only once the change is stored.

use std::sync::Arc;

use jid::{BareJid, FullJid, Jid};
use tracing::{warn, Instrument};
use waddle_xmpp::presence::subscription::{
    build_available_presence, build_subscription_presence, SubscriptionStateMachine,
    SubscriptionType,
};
use waddle_xmpp::roster::{AskType, RosterItem, Subscription};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::roster::RosterVersion;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::subscription::{load_existing_roster_item, roster_item_to_row, roster_storage};
use crate::db::roster::{DatabaseRosterStorage, RosterRowChange};
use crate::db::s2s_accounts::{PendingOutcome, PendingSubscriptions};
use crate::destination::{
    offer_stanza, unroutable_reply, RemoteDestination, RemoteRoute, ServedDomains, StanzaOffer,
    UnservedDestination, REMOTE_ROUTE_OFFER_TIMEOUT,
};
use crate::s2s::route::{AccountWork, RemotePrincipal, MAX_ACCOUNT_REPLIES};
use crate::server::routes::websocket::handlers::account_reads;
use crate::server::routes::websocket::handlers::roster_fanout::send_roster_push_to_resources;
use crate::server::routes::websocket::handlers::unserved_route::unserved_destination;
use crate::server::routes::websocket::s2s_accounts::{
    account_exists, admits_domain, blocklist, blocks, database, deliver, lock_account,
    reaches_full_jid, s2s_enabled, telemetry, AccountLock,
};
use crate::server::routes::websocket::transport_xml::stanza_to_xml;
use crate::server::routes::websocket::WebSocketState;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn pending_store(state: &WebSocketState) -> Option<PendingSubscriptions> {
    database(state).await.map(PendingSubscriptions::new)
}

/// Store `item` as `account`'s roster item. The version of the change when
/// it was stored; `None` when it was not, and nothing may announce it.
async fn store(
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    item: &RosterItem,
) -> Option<RosterVersion> {
    match storage
        .apply_roster_change(account, RosterRowChange::Upsert(roster_item_to_row(item)))
        .await
    {
        Ok((mutation, _lock)) => Some(mutation.version),
        Err(error) => {
            warn!(%error, "storing a roster item for a remote contact failed");
            None
        }
    }
}

/// Store `item` as `account`'s roster item and push it to the account's
/// interested resources (RFC 6121 §2.1.6). `false` when it was not stored;
/// nothing was pushed then.
async fn store_and_push(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    item: &RosterItem,
) -> bool {
    let Some(version) = store(storage, account, item).await else {
        return false;
    };
    send_roster_push_to_resources(state, account, item, &version, None).await;
    true
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
    for (resource, stored) in account_reads::available_resources_with_presence(state, account)
        .await
        .into_iter()
        .take(MAX_ACCOUNT_REPLIES)
    {
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

/// `account`'s roster item for `contact`: `Some(None)` when it has none,
/// `None` when the roster cannot be read.
async fn read_item(
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    contact: &BareJid,
) -> Option<Option<RosterItem>> {
    match load_existing_roster_item(storage, account, contact).await {
        Ok(item) => Some(item),
        Err(error) => {
            warn!(%error, "reading a roster item for a remote contact failed");
            None
        }
    }
}

// --- inbound ---------------------------------------------------------------

/// A presence stanza from a remote principal to the local account address
/// `to`, and the stanzas that answer it.
pub(crate) async fn receive_remote_presence(
    state: &WebSocketState,
    principal: &RemotePrincipal,
    to: &Jid,
    mut presence: Presence,
    work: &AccountWork,
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
    match presence.type_ {
        PresenceType::None | PresenceType::Unavailable => {
            if let Ok(full) = to.clone().try_into_full() {
                // Directed presence to one resource (RFC 6121 §8.5.3.1); a
                // detached session holds it only from a contact.
                if reaches_full_jid(state, &account, &contact, &full).await && !work.abandoned() {
                    let _ = deliver(state, &full, &Stanza::Presence(presence)).await;
                }
                return Vec::new();
            }
            let Some(item) = read_item(&storage, &account, &contact).await else {
                return Vec::new();
            };
            if has(item.as_ref(), &[Subscription::To, Subscription::Both]) && !work.abandoned() {
                let stanza = Stanza::Presence(presence);
                deliver_to(state, &available(state, &account).await, &stanza).await;
            }
            Vec::new()
        }
        PresenceType::Probe => {
            let Some(item) = read_item(&storage, &account, &contact).await else {
                return Vec::new();
            };
            if work.abandoned() {
                return Vec::new();
            }
            answer_probe(state, &account, to, &contact, item.as_ref()).await
        }
        PresenceType::Subscribe
        | PresenceType::Subscribed
        | PresenceType::Unsubscribe
        | PresenceType::Unsubscribed => {
            // RFC 6121 §3.1.3: a subscription stanza names the bare JIDs.
            presence.from = Some(Jid::from(contact.clone()));
            presence.to = Some(Jid::from(account.clone()));
            let _lock = lock_account(&account).await;
            if work.abandoned() {
                return Vec::new();
            }
            let Some(item) = read_item(&storage, &account, &contact).await else {
                return Vec::new();
            };
            receive_subscription(state, &storage, &account, &contact, item, presence, work).await
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
/// §3.1.3, §3.1.6, §3.2.3, §3.3.3), decided under the account's lock.
async fn receive_subscription(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    contact: &BareJid,
    item: Option<RosterItem>,
    presence: Presence,
    work: &AccountWork,
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
                if work.abandoned() || !store_and_push(state, storage, account, &item).await {
                    return Vec::new();
                }
                let mut replies = vec![subscribed(account, contact)];
                replies.extend(current_presence(state, account, contact).await);
                replies.truncate(MAX_ACCOUNT_REPLIES);
                return replies;
            }
            let Some(pending) = pending_store(state).await else {
                return Vec::new();
            };
            if work.abandoned() {
                return Vec::new();
            }
            match pending
                .record(
                    account,
                    contact,
                    &minidom::Element::from(presence),
                    now_ms(),
                )
                .await
            {
                Ok(PendingOutcome::Recorded) if !work.abandoned() => {
                    deliver_to(state, &available(state, account).await, &stanza).await;
                }
                Ok(PendingOutcome::Recorded) => {}
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
            announce(state, storage, account, &item, &stanza, work).await;
            Vec::new()
        }
        PresenceType::Unsubscribe => {
            if work.abandoned() {
                return Vec::new();
            }
            if let Some(pending) = pending_store(state).await {
                let _ = pending.remove(account, contact).await;
            }
            let Some(mut item) = item.filter(|item| {
                matches!(item.subscription, Subscription::From | Subscription::Both)
            }) else {
                return Vec::new();
            };
            SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
            if !announce(state, storage, account, &item, &stanza, work).await {
                return Vec::new();
            }
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
            announce(state, storage, account, &item, &stanza, work).await;
            Vec::new()
        }
        PresenceType::None
        | PresenceType::Unavailable
        | PresenceType::Probe
        | PresenceType::Error => Vec::new(),
    }
}

/// Store the changed `item`, then deliver the inbound subscription `stanza`
/// that changed it to the account's interested resources and push the item
/// (RFC 6121 §3.1.6, §3.2.3, §3.3.3: the stanza first, then the roster
/// push). Nothing is stored, delivered or pushed once `work` is abandoned,
/// and nothing is delivered or pushed when the item could not be stored.
async fn announce(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    account: &BareJid,
    item: &RosterItem,
    stanza: &Stanza,
    work: &AccountWork,
) -> bool {
    if work.abandoned() {
        return false;
    }
    let Some(version) = store(storage, account, item).await else {
        return false;
    };
    deliver_to(
        state,
        &account_reads::roster_interested(state, account).await,
        stanza,
    )
    .await;
    send_roster_push_to_resources(state, account, item, &version, None).await;
    true
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
/// longer admits, or from a sender the account blocks (XEP-0191 §3.3), is
/// not delivered; when the blocking list cannot be read, none is.
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
    let requests = match pending.list(account, now_ms()).await {
        Ok(requests) => requests,
        Err(error) => {
            warn!(%error, "reading remote subscription requests failed");
            return Vec::new();
        }
    };
    if requests.is_empty() {
        return Vec::new();
    }
    let Some(blocked) = blocklist(state, account).await else {
        return Vec::new();
    };
    requests
        .into_iter()
        .filter(|(contact, _)| {
            admits_domain(state, contact.domain())
                && !blocked.contains_jid(&Jid::from(contact.clone()))
        })
        .filter_map(|(_, stanza)| Presence::try_from(stanza).ok().map(Stanza::Presence))
        .collect()
}

/// Forget the recorded requests of every sender `blocked` now covers: an
/// account that blocks a sender is not presented its request again
/// (XEP-0191 §3.3).
pub(crate) async fn forget_blocked_requests(
    state: &WebSocketState,
    account: &BareJid,
    blocked: &[Jid],
) {
    if blocked.is_empty() {
        return;
    }
    let Some(pending) = pending_store(state).await else {
        return;
    };
    let requests = match pending.list(account, now_ms()).await {
        Ok(requests) => requests,
        Err(error) => {
            warn!(%error, "reading remote subscription requests to forget blocked senders failed");
            return;
        }
    };
    let blocked = waddle_xmpp::protocol::Blocklist::new(blocked.iter().cloned());
    for (contact, _) in requests {
        if blocked.contains_jid(&Jid::from(contact.clone())) {
            if let Err(error) = pending.remove(account, &contact).await {
                warn!(%error, "forgetting a blocked sender's subscription request failed");
            }
        }
    }
}

/// Forget the request `contact` sent `account`, as the account denies it
/// (RFC 6121 §3.1.4, §3.2.2), whatever then becomes of the `unsubscribed`
/// that denies it: refused as blocked (XEP-0191 §3.3), refused by the
/// route, or routed. `true` when one was recorded.
pub(crate) async fn forget_request(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> bool {
    let Some(pending) = pending_store(state).await else {
        return false;
    };
    match pending.remove(account, contact).await {
        Ok(removed) => removed,
        Err(error) => {
            warn!(%error, "forgetting a remote subscription request failed");
            false
        }
    }
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
    offer_presence_to(state.deps.remote_route.as_ref(), remote, presence).await
}

/// Offer `presence`, from a local account, to `remote` on `route`.
async fn offer_presence_to(
    route: &dyn RemoteRoute,
    remote: RemoteDestination,
    presence: Presence,
) -> bool {
    let Some(home) = presence.from.as_ref().map(|from| from.domain().to_owned()) else {
        return false;
    };
    matches!(
        offer_stanza(
            route,
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
/// account's half of the subscription changed once the route took it. An
/// `unsubscribed` takes effect locally whatever the route does: the account
/// withdraws a subscription or denies a request on its own authority.
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
    let _lock = lock_account(&account).await;
    let Some(storage) = roster_storage(state).await else {
        return lookup_failed(&presence, sender);
    };
    let Some(item) = read_item(&storage, &account, &contact).await else {
        return lookup_failed(&presence, sender);
    };
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
            // Whether the contact asked decides between an approval and a
            // pre-approval; when that cannot be read, neither is made.
            let Some(pending) = pending_store(state).await else {
                return lookup_failed(&presence, sender);
            };
            let requested = match pending.holds(&account, &contact, now_ms()).await {
                Ok(requested) => requested,
                Err(error) => {
                    warn!(%error, "reading a remote subscription request failed");
                    return lookup_failed(&presence, sender);
                }
            };
            if !requested && !has(item.as_ref(), &[Subscription::From, Subscription::Both]) {
                // No request to approve: a pre-approval (§3.4.2), which
                // nothing leaves for.
                let mut item = item.unwrap_or_else(|| RosterItem::new(contact.clone()));
                item.approved = true;
                if !store_and_push(state, &storage, &account, &item).await {
                    return lookup_failed(&presence, sender);
                }
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
                if !store_and_push(state, &storage, &account, &item).await {
                    return Vec::new();
                }
                let _ = pending.remove(&account, &contact).await;
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
            // The denial of a recorded request is the account's own: the
            // record is forgotten whatever becomes of the stanza.
            let requested = forget_request(state, &account, &contact).await;
            if has(item.as_ref(), &[Subscription::From, Subscription::Both]) {
                // §3.2.2: unavailable from every available resource first.
                for stanza in unavailable_from_each(state, &account, &contact).await {
                    if let Stanza::Presence(presence) = stanza {
                        let _ = offer_presence(state, presence).await;
                    }
                }
                let routed = route(state, sender, remote, presence).await;
                // The contact no longer sees the account's presence, whether
                // or not its server could be told.
                if let Some(mut item) = item {
                    SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
                    store_and_push(state, &storage, &account, &item).await;
                }
                return match routed {
                    Routed::Refused(frames) => frames,
                    Routed::Accepted => Vec::new(),
                };
            }
            if requested {
                // Denying a request (§3.1.4).
                return match route(state, sender, remote, presence).await {
                    Routed::Refused(frames) => frames,
                    Routed::Accepted => Vec::new(),
                };
            }
            if let Some(mut item) = item.filter(|item| item.approved) {
                // Cancelling a pre-approval: nothing leaves (§3.2.2 rule 2).
                item.approved = false;
                store_and_push(state, &storage, &account, &item).await;
            }
            Vec::new()
        }
        PresenceType::None
        | PresenceType::Unavailable
        | PresenceType::Probe
        | PresenceType::Error => Vec::new(),
    }
}

/// The retryable error answering a subscription stanza whose roster or
/// request record could not be read or written (RFC 6120 §8.3).
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
/// roster. A request the contact sent is forgotten. The contact is sent
/// `unsubscribe` if the account was subscribed to it or asked to be, and
/// `unsubscribed` if it was subscribed to the account; nothing is sent to a
/// contact the account blocks (XEP-0191 §3.3), or while S2S is off. The
/// caller holds the account's lock (`_lock`) from before it read the item
/// it removed.
pub(crate) async fn remove_remote_contact(
    state: &WebSocketState,
    account: &BareJid,
    item: &RosterItem,
    _lock: &AccountLock,
) {
    forget_request(state, account, &item.jid).await;
    // With S2S off nothing could leave; with it on, nothing leaves for a
    // contact the account blocks.
    if !s2s_enabled(state) || blocks(state, account, &Jid::from(item.jid.clone())).await {
        return;
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
/// subscribed to, from the account's bare JID, off the connection's task.
/// The account's blocking list is read once; nothing is sent to a contact
/// it blocks (XEP-0191 §3.3), and nothing at all when it cannot be read.
pub(crate) fn spawn_probe_remote_contacts(state: &WebSocketState, account: &BareJid) {
    if !s2s_enabled(state) {
        return;
    }
    let Some(served) = state
        .deps
        .service_domains
        .served_domains(state.deps.auth_state.xmpp_domain.as_str())
    else {
        return;
    };
    let app_state = Arc::clone(&state.deps.app_state);
    let route = Arc::clone(&state.deps.remote_route);
    let account = account.clone();
    tokio::spawn(
        async move { probe_remote_contacts(&app_state, route.as_ref(), &served, &account).await }
            .instrument(tracing::Span::current()),
    );
}

async fn probe_remote_contacts(
    app_state: &crate::server::AppState,
    route: &dyn RemoteRoute,
    served: &ServedDomains,
    account: &BareJid,
) {
    let subscribed = match app_state
        .db_pool
        .roster_storage()
        .get_presence_subscription_jids(account)
        .await
    {
        Ok(subscribed) => subscribed,
        Err(error) => {
            warn!(%error, "reading the roster to probe remote contacts failed");
            return;
        }
    };
    let contacts: Vec<(BareJid, RemoteDestination)> = subscribed
        .into_iter()
        .filter_map(|contact| {
            match served
                .classify(Some(&Jid::from(contact.clone())))
                .unserved()
            {
                Some(UnservedDestination::Remote(remote)) => Some((contact, remote)),
                _ => None,
            }
        })
        .collect();
    if contacts.is_empty() {
        return;
    }
    let blocked = match app_state
        .db_pool
        .blocking_storage()
        .load_blocklist(account)
        .await
    {
        Ok(blocklist) => blocklist,
        Err(error) => {
            warn!(%error, "reading the blocking list to probe remote contacts failed");
            return;
        }
    };
    for (contact, remote) in contacts {
        if blocked.contains_jid(&Jid::from(contact.clone())) {
            continue;
        }
        let mut probe = Presence::new(PresenceType::Probe);
        probe.from = Some(Jid::from(account.clone()));
        probe.to = Some(Jid::from(contact));
        let _ = offer_presence_to(route, remote, probe).await;
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
