//! The account side of traffic between remote senders and local accounts
//! (ADR-0020 D7; gate G18): what a remote principal's message, presence or
//! IQ request does to a local account, and the checks every such stanza
//! passes first.
//!
//! The S2S router hands this side only stanzas no remote room claimed, and
//! never a stanza a client handler would see: those handlers read the
//! sender from a local session. Every entry decides, in this order, before
//! anything is applied:
//!
//! 1. the sender's admission (D1), from its stream's evidence and the S2S
//!    snapshot the replica runs, so that work queued before its domain
//!    stopped being admitted applies nothing;
//! 2. whether the account exists, the ledger of retired JIDs included, so
//!    that a retired JID is a nonexistent one whatever rows or resources
//!    remain;
//! 3. whether the account blocks the sender (XEP-0191).
//!
//! A non-contact learns nothing from the answers about which of these held
//! (`s2s-account-traffic-2047.md`, "No enumeration").

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use tracing::warn;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::muc::presence::NS_MUC_USER;
use waddle_xmpp::pubsub::{NS_PUBSUB, NS_PUBSUB_EVENT};
use waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0;
use waddle_xmpp::xep::{NS_INBOX, NS_WADDLE_INBOX};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::interpret_loop::build_interpret_deps;
use super::WebSocketState;
use crate::db::actor::GetDatabase;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::DatabaseRosterStorage;
use crate::db::s2s_accounts::{RemotePubSubSubscriptions, SubscriptionOutcome};
use crate::db::Database;
use crate::permissions::host_grant::PrincipalEvidence;
use crate::s2s::route::{LocalAccounts, RemotePrincipal};
use crate::server::routes::interpret::{
    deliver_peer_to_full_with_registered_remote, FullJidDeliveryOutcome,
};

/// How many stale candidates a message to the bare JID tries before it is
/// handled as if no resource were available.
const DELIVERY_ATTEMPTS: usize = 3;

/// [`LocalAccounts`] over the WebSocket state: the accounts this
/// deployment serves, their rosters and their resources, live or
/// detached, on this replica or another.
///
/// It holds the state weakly: the state holds the router, which holds
/// this.
pub struct WebSocketLocalAccounts {
    state: Weak<WebSocketState>,
}

impl WebSocketLocalAccounts {
    pub fn new(state: &Arc<WebSocketState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

#[async_trait]
impl LocalAccounts for WebSocketLocalAccounts {
    async fn receive(&self, principal: &RemotePrincipal, to: &Jid, stanza: Stanza) -> Vec<Stanza> {
        let Some(state) = self.state.upgrade() else {
            return Vec::new();
        };
        if !admits(&state, principal) {
            telemetry::not_admitted();
            return Vec::new();
        }
        // Room traffic reaches an occupant through the router's occupancy
        // table, before this side. What still carries a room's
        // `muc#user` element here is from a room the resource never joined
        // or has left, and is dropped; a mediated invitation or its decline
        // is addressed to a non-occupant and is account traffic.
        if is_stray_room_traffic(&stanza) {
            return Vec::new();
        }
        match stanza {
            Stanza::Message(message) => receive_message(&state, principal, to, message).await,
            Stanza::Presence(presence) => {
                super::handlers::presence::receive_remote_presence(&state, principal, to, presence)
                    .await
            }
            // IQ requests are admitted by the router through
            // `admits_request`, never handed here.
            Stanza::Iq(_) => Vec::new(),
        }
    }

    async fn admits_request(
        &self,
        principal: &RemotePrincipal,
        to: &FullJid,
        room_occupant: bool,
    ) -> bool {
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        if !admits(&state, principal) {
            telemetry::not_admitted();
            return false;
        }
        let account = to.to_bare();
        if !account_exists(&state, &account).await
            || blocks(&state, &account, principal.address()).await
        {
            return false;
        }
        room_occupant
            || shares_presence_with(&state, &account, principal.account().as_bare_jid()).await
    }
}

/// Whether `stanza` carries a room's `muc#user` element (XEP-0045) other
/// than a mediated invitation or its decline.
fn is_stray_room_traffic(stanza: &Stanza) -> bool {
    let payloads = match stanza {
        Stanza::Message(message) => &message.payloads,
        Stanza::Presence(presence) => &presence.payloads,
        Stanza::Iq(_) => return false,
    };
    payloads.iter().any(|payload| {
        payload.is("x", NS_MUC_USER)
            && !(matches!(stanza, Stanza::Message(_))
                && (payload.has_child("invite", NS_MUC_USER)
                    || payload.has_child("decline", NS_MUC_USER)))
    })
}

/// Whether S2S is enabled on this replica: it runs an S2S snapshot.
pub(crate) fn s2s_enabled(state: &WebSocketState) -> bool {
    state.deps.app_state.s2s_federation.is_some()
}

/// Whether `principal`'s domain is admitted now (ADR-0020 D1), a
/// denylisted parent domain included, under the snapshot the replica
/// runs.
pub(crate) fn admits(state: &WebSocketState, principal: &RemotePrincipal) -> bool {
    let Ok(domain) = CanonicalDomain::parse_canonical(state.deps.auth_state.xmpp_domain.as_str())
    else {
        return false;
    };
    let witness = state.deps.app_state.grant_principals(domain).witness(
        principal.account().as_bare_jid(),
        &PrincipalEvidence::of(principal),
    );
    witness
        .remote()
        .is_some_and(|remote| remote.admission().is_ok())
}

/// Whether the S2S snapshot the replica runs admits `domain` now.
pub(crate) fn admits_domain(state: &WebSocketState, domain: &jid::DomainRef) -> bool {
    let Some(federation) = state.deps.app_state.s2s_federation.as_ref() else {
        return false;
    };
    CanonicalDomain::parse_canonical(domain.as_str())
        .is_ok_and(|domain| federation.admission().decide(&domain).is_ok())
}

pub(crate) async fn database(state: &WebSocketState) -> Option<Database> {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => Some(db),
        Err(error) => {
            warn!(%error, "the account database is unavailable for s2s account traffic");
            None
        }
    }
}

/// Whether `account` is an account of this deployment that has not been
/// retired. Any failure answers `false`: the stanza is then handled as for
/// a nonexistent account, which a remote sender cannot tell apart.
pub(crate) async fn account_exists(state: &WebSocketState, account: &BareJid) -> bool {
    let actor = state.deps.app_state.db_pool.global_actor();
    let exists = crate::auth::local_account_jid_exists(
        actor,
        account,
        state.deps.auth_state.xmpp_domain.as_str(),
    )
    .await;
    if !matches!(exists, Ok(true)) {
        return false;
    }
    let Some(db) = database(state).await else {
        return false;
    };
    matches!(
        crate::auth::retirement::jid_is_retired(&db, account).await,
        Ok(false)
    )
}

/// Whether `account`'s XEP-0191 list blocks `sender` (full, bare and domain
/// entries). A list that cannot be read blocks.
pub(crate) async fn blocks(state: &WebSocketState, account: &BareJid, sender: &Jid) -> bool {
    let Some(db) = database(state).await else {
        return true;
    };
    DatabaseBlockingStorage::new(db)
        .is_blocked_jid(account, sender)
        .await
        .unwrap_or(true)
}

/// The subscription state of `account`'s roster item for `contact`, if it
/// has one.
async fn subscription_of(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> Option<String> {
    let db = database(state).await?;
    DatabaseRosterStorage::new(db)
        .get_roster_item(account, contact)
        .await
        .ok()
        .flatten()
        .map(|row| row.subscription)
}

/// Whether `account` lets `contact` see its presence: a roster item with a
/// subscription `from` or `both` (RFC 6121 §8.1, §8.5.3.1).
pub(crate) async fn shares_presence_with(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> bool {
    matches!(
        subscription_of(state, account, contact).await.as_deref(),
        Some("from" | "both")
    )
}

/// Whether `account` is subscribed to `contact`'s presence: a roster item
/// with a subscription `to` or `both`.
pub(crate) async fn subscribed_to(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> bool {
    matches!(
        subscription_of(state, account, contact).await.as_deref(),
        Some("to" | "both")
    )
}

/// How a delivery to one resource ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Handed to the resource or its stream-management custody.
    Taken,
    /// No such resource is bound.
    NotBound,
    /// Dropped by the recipient's rules or for a transient reason; it is not
    /// offered elsewhere, so that it cannot arrive twice.
    Dropped,
}

/// Deliver a remote entity's `stanza` to the local resource `to` through
/// the recipient pass of a peer stanza: the recipient's blocking list,
/// XEP-0359 ids, the archive, the inbox and received carbons apply as for
/// any stanza from another entity.
pub(crate) async fn deliver(state: &WebSocketState, to: &FullJid, stanza: &Stanza) -> Delivery {
    let deps = build_interpret_deps(state, None);
    match deliver_peer_to_full_with_registered_remote(&deps, to, stanza).await {
        FullJidDeliveryOutcome::Delivered
        | FullJidDeliveryOutcome::QueuedDetached
        | FullJidDeliveryOutcome::MaybeCommitted => Delivery::Taken,
        FullJidDeliveryOutcome::Unavailable => Delivery::NotBound,
        FullJidDeliveryOutcome::Dropped => Delivery::Dropped,
    }
}

/// The available resources of `account` with a non-negative priority,
/// most available first: highest priority, then by address.
pub(crate) async fn non_negative_resources(
    state: &WebSocketState,
    account: &BareJid,
) -> Vec<FullJid> {
    let mut resources: Vec<(FullJid, i8)> =
        super::handlers::account_reads::available_resources(state, account)
            .await
            .into_iter()
            .filter(|(_, priority)| *priority >= 0)
            .collect();
    resources.sort_by(|(left, left_priority), (right, right_priority)| {
        right_priority
            .cmp(left_priority)
            .then_with(|| left.as_str().cmp(right.as_str()))
    });
    resources
        .into_iter()
        .map(|(resource, _)| resource)
        .collect()
}

/// The detached stream-management resources of `account` that were
/// available with a non-negative priority when they detached.
async fn detached_resources(state: &WebSocketState, account: &BareJid) -> Vec<FullJid> {
    match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_presence_states_for_user(account)
        .await
    {
        Ok(states) => {
            let mut resources: Vec<FullJid> = states
                .into_iter()
                .filter(|detached| detached.priority >= 0)
                .map(|detached| detached.resource)
                .collect();
            resources.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            resources
        }
        Err(error) => {
            warn!(%error, "listing detached resources for a remote sender's message failed");
            Vec::new()
        }
    }
}

/// A stanza error this server generates, attributed to its domain.
fn error(
    state: &WebSocketState,
    type_: ErrorType,
    condition: DefinedCondition,
    text: &str,
) -> StanzaError {
    let mut error = StanzaError::new(type_, condition, "en", text);
    if let Ok(domain) = jid::BareJid::new(state.deps.auth_state.xmpp_domain.as_str()) {
        error.by = Some(Jid::from(domain));
    }
    error
}

/// `service-unavailable` answering `message`, from its recipient to its
/// sender, without the body (RFC 6121 §8.5).
fn refuse_message(state: &WebSocketState, message: &Message) -> Vec<Stanza> {
    if message.type_ == MessageType::Error {
        return Vec::new();
    }
    let mut reply = Message::error(message.from.clone());
    reply.from = message.to.clone();
    reply.id = message.id.clone();
    reply.payloads.push(minidom::Element::from(error(
        state,
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        "The recipient is not available.",
    )));
    vec![Stanza::Message(reply)]
}

/// Remove from a remote message the payloads only this server originates
/// for its own accounts: the inbox elements and the DM pin marker.
fn strip_private_payloads(message: &mut Message) {
    message.payloads.retain(|payload| {
        let namespace = payload.ns();
        namespace != NS_INBOX && namespace != NS_WADDLE_INBOX && namespace != NS_WADDLE_PIN_V0
    });
}

/// The node an XEP-0060 event notification names, if `message` carries
/// one (§7.1.2: `items`, `purge`, `delete`, `configuration` and
/// `subscription` children of `<event/>` all name it).
pub(crate) fn event_node(message: &Message) -> Option<Option<String>> {
    let event = message
        .payloads
        .iter()
        .find(|payload| payload.is("event", NS_PUBSUB_EVENT))?;
    Some(
        event
            .children()
            .find_map(|child| child.attr("node").map(ToOwned::to_owned)),
    )
}

/// A message from a remote principal to the local account address `to`
/// (RFC 6121 §8.5; `s2s-account-traffic-2047.md`, "Messages").
async fn receive_message(
    state: &WebSocketState,
    principal: &RemotePrincipal,
    to: &Jid,
    mut message: Message,
) -> Vec<Stanza> {
    // An account is not a room: the same answer in every state.
    if message.type_ == MessageType::Groupchat {
        return refuse_message(state, &message);
    }
    let account = to.to_bare();
    if !account_exists(state, &account).await || blocks(state, &account, principal.address()).await
    {
        return Vec::new();
    }
    strip_private_payloads(&mut message);
    let sender = principal.account().as_bare_jid().clone();
    let full = to.clone().try_into_full().ok();
    if message.type_ == MessageType::Error {
        if let Some(full) = &full {
            let _ = deliver(state, full, &Stanza::Message(message)).await;
        }
        return Vec::new();
    }
    if let Some(node) = event_node(&message) {
        if !event_admitted(state, &account, &sender, node.as_deref()).await {
            return Vec::new();
        }
        deliver_headline(state, &account, full.as_ref(), &message).await;
        return Vec::new();
    }
    match message.type_ {
        MessageType::Headline => {
            deliver_headline(state, &account, full.as_ref(), &message).await;
            Vec::new()
        }
        MessageType::Chat | MessageType::Normal => {
            if deliver_direct(state, &account, full.as_ref(), &message).await {
                return Vec::new();
            }
            // Nothing took it and nothing stores it: a contact, who may know
            // whether the account is online, is told; anybody else is not.
            if shares_presence_with(state, &account, &sender).await {
                return refuse_message(state, &message);
            }
            Vec::new()
        }
        MessageType::Groupchat | MessageType::Error => Vec::new(),
    }
}

/// Deliver a `chat` or `normal` message: to the full JID it names if that
/// resource is bound, otherwise (a `chat` message only, §8.5.3.2.1) to the
/// one most available resource, a connected one when none is available,
/// or a detached one. `false` when nothing took it.
async fn deliver_direct(
    state: &WebSocketState,
    account: &BareJid,
    full: Option<&FullJid>,
    message: &Message,
) -> bool {
    let stanza = Stanza::Message(message.clone());
    if let Some(full) = full {
        match deliver(state, full, &stanza).await {
            Delivery::Taken | Delivery::Dropped => return true,
            Delivery::NotBound if message.type_ != MessageType::Chat => return true,
            Delivery::NotBound => {}
        }
    }
    let live = non_negative_resources(state, account).await;
    for resource in live.iter().take(DELIVERY_ATTEMPTS) {
        match deliver(state, resource, &stanza).await {
            Delivery::Taken | Delivery::Dropped => return true,
            Delivery::NotBound => {}
        }
    }
    // A connected resource that has not sent presence yet, as local
    // delivery to the bare JID does (RFC 6121 §8.5.2.1: an available or
    // connected resource). An available resource with a negative priority
    // is never one (§8.5.2.1.1).
    if live.is_empty() {
        let available: Vec<FullJid> =
            super::handlers::account_reads::available_resources(state, account)
                .await
                .into_iter()
                .map(|(resource, _)| resource)
                .collect();
        let mut connected: Vec<FullJid> = super::handlers::account_reads::resources(state, account)
            .await
            .into_iter()
            .filter(|resource| !available.contains(resource))
            .collect();
        connected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        for resource in connected.iter().take(DELIVERY_ATTEMPTS) {
            match deliver(state, resource, &stanza).await {
                Delivery::Taken | Delivery::Dropped => return true,
                Delivery::NotBound => {}
            }
        }
    }
    for resource in detached_resources(state, account)
        .await
        .iter()
        .take(DELIVERY_ATTEMPTS)
    {
        match deliver(state, resource, &stanza).await {
            Delivery::Taken | Delivery::Dropped => return true,
            Delivery::NotBound => {}
        }
    }
    false
}

/// Deliver a `headline` (an event notification included): to the full JID
/// it names if bound, or to every available resource with a non-negative
/// priority (RFC 6121 §8.5.2.1.1). It is never stored.
async fn deliver_headline(
    state: &WebSocketState,
    account: &BareJid,
    full: Option<&FullJid>,
    message: &Message,
) {
    let stanza = Stanza::Message(message.clone());
    if let Some(full) = full {
        let _ = deliver(state, full, &stanza).await;
        return;
    }
    for resource in non_negative_resources(state, account).await {
        let _ = deliver(state, &resource, &stanza).await;
    }
}

/// Whether an event notification from `sender` about `node` may reach
/// `account`: the account subscribed to that node of that service, or the
/// sender is a remote account whose presence the account is subscribed to
/// (its personal eventing service, XEP-0163).
async fn event_admitted(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    node: Option<&str>,
) -> bool {
    if let (Some(node), Some(db)) = (node, database(state).await) {
        if matches!(
            RemotePubSubSubscriptions::new(db)
                .holds(account, sender, node)
                .await,
            Ok(true)
        ) {
            return true;
        }
    }
    sender.node().is_some() && subscribed_to(state, account, sender).await
}

/// The node and subscribed JID of an XEP-0060 `<subscribe/>` or
/// `<unsubscribe/>` request, and whether it subscribes.
fn subscription_request(iq: &Iq) -> Option<(bool, String)> {
    let Iq::Set { payload, .. } = iq else {
        return None;
    };
    if !payload.is("pubsub", NS_PUBSUB) {
        return None;
    }
    payload.children().find_map(|child| {
        let subscribes = if child.is("subscribe", NS_PUBSUB) {
            true
        } else if child.is("unsubscribe", NS_PUBSUB) {
            false
        } else {
            return None;
        };
        child.attr("node").map(|node| (subscribes, node.to_owned()))
    })
}

/// Whether `iq`, a resource's request to a remote JID, would be refused for
/// the account's bound on remote PubSub subscriptions: a `<subscribe/>`
/// beyond it is answered with `resource-constraint` before it leaves.
pub(crate) async fn exceeds_subscription_bound(
    state: &WebSocketState,
    sender: &FullJid,
    iq: &Iq,
) -> bool {
    if !s2s_enabled(state) {
        return false;
    }
    let Some((true, _)) = subscription_request(iq) else {
        return false;
    };
    let Some(db) = database(state).await else {
        return true;
    };
    !matches!(
        RemotePubSubSubscriptions::new(db)
            .has_room(&sender.to_bare())
            .await,
        Ok(true)
    )
}

/// Record, or forget, the remote PubSub subscription a resource's request
/// to `service` asks for, once the remote route took the request.
pub(crate) async fn record_subscription_request(
    state: &WebSocketState,
    sender: &FullJid,
    service: &Jid,
    iq: &Iq,
) {
    if !s2s_enabled(state) {
        return;
    }
    let Some((subscribes, node)) = subscription_request(iq) else {
        return;
    };
    let Some(db) = database(state).await else {
        return;
    };
    let store = RemotePubSubSubscriptions::new(db);
    let account = sender.to_bare();
    let service = service.to_bare();
    let outcome = if subscribes {
        store
            .record(
                &account,
                &service,
                &node,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .map(|outcome| outcome == SubscriptionOutcome::Recorded)
    } else {
        store.remove(&account, &service, &node).await.map(|()| true)
    };
    if !matches!(outcome, Ok(true)) {
        warn!("recording a remote pubsub subscription failed");
    }
}

/// The account side's metrics. No attribute names a JID, a domain or a
/// stanza id.
pub(crate) mod telemetry {
    use waddle_xmpp::counter_add;

    /// A stanza reached the account side from a domain the replica's S2S
    /// snapshot no longer admits.
    pub(crate) fn not_admitted() {
        counter_add!(
            "waddle.s2s.account_traffic.not_admitted",
            "{stanza}",
            "Stanzas from S2S streams that reached the account side from a domain the replica's S2S snapshot no longer admits, applied and answered with nothing.",
            1,
        );
    }

    /// A remote sender's subscription request was dropped because the
    /// account's record of pending requests was full.
    pub(crate) fn pending_dropped() {
        counter_add!(
            "waddle.s2s.account_traffic.pending_dropped",
            "{stanza}",
            "Subscription requests from remote senders dropped unanswered because the account's record of pending requests, or its share for the sender's domain, was full, or the request was too large to record.",
            1,
        );
    }
}
