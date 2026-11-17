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
//!    stopped being admitted applies nothing (and, for a direct message,
//!    again in its ingress acceptance);
//! 2. whether the account exists, the ledger of retired JIDs included, so
//!    that a retired JID is a nonexistent one whatever rows or resources
//!    remain;
//! 3. whether the account blocks the sender (XEP-0191).
//!
//! A non-contact learns nothing from the answers about which of these held
//! (`s2s-account-traffic-2047.md`, "No enumeration").
//!
//! A `chat` or `normal` message from a sender with a resource is accepted
//! through the ingress authority, as the recipient's half of a local direct
//! message ([`receive_direct`]): the ingress acceptance checks admission
//! again against the snapshot captured when it built the submission, and
//! reads the account's existence again, the ledger of retired JIDs
//! included, in its transaction, by the same lookup keys as the check
//! before it.
//!
//! Each stanza's work checks, before every effect, that the router has not
//! abandoned it ([`AccountWork`]), and the read-modify-write of an
//! account's roster items for remote contacts runs under the account's
//! lock ([`lock_account`]), so that one sender's stanzas take effect in
//! their order.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::warn;
use waddle_xmpp::carbons::CARBONS_NS;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::mam::storage::MamStorage;
use waddle_xmpp::muc::presence::NS_MUC_USER;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::protocol::dm_routing::{classify_dm_intake, OnlineResources, PendingDecision};
use waddle_xmpp::protocol::Blocklist;
use waddle_xmpp::pubsub::{NS_PUBSUB, NS_PUBSUB_EVENT};
use waddle_xmpp::roster::Subscription;
use waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0;
use waddle_xmpp::xep::{NS_INBOX, NS_WADDLE_INBOX};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::mam::MAM_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::interpret_loop::build_interpret_deps;
use super::WebSocketState;
use crate::db::actor::GetDatabase;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::DatabaseRosterStorage;
use crate::db::s2s_accounts::{
    OfflineAdmission, RemoteOfflineAdmissions, RemotePubSubSubscriptions, SubscriptionOutcome,
};
use crate::db::Database;
use crate::destination::{ServedDomains, UnservedDestination};
use crate::permissions::host_grant::PrincipalEvidence;
use crate::s2s::route::{AccountWork, LocalAccounts, RemotePrincipal};
use crate::server::routes::interpret::{
    deliver_peer_to_full_with_registered_remote, FullJidDeliveryOutcome,
};

/// How many stale candidates a message to the bare JID tries before it is
/// handled as if no resource were available.
const DELIVERY_ATTEMPTS: usize = 3;

/// How long the effects of a remote sender's accepted direct message are
/// waited for before the ingress authority's recovery is left to finish
/// them.
const DIRECT_EFFECTS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The most nodes one message's event notifications may name; a message
/// naming more is dropped, so that one stanza cannot cause more than this
/// many subscription lookups.
const MAX_EVENT_NODES: usize = 8;

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
    fn admits(&self, principal: &RemotePrincipal) -> bool {
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        if admits(&state, principal) {
            return true;
        }
        telemetry::not_admitted();
        false
    }

    async fn receive(
        &self,
        principal: &RemotePrincipal,
        to: &Jid,
        stanza: Stanza,
        work: &AccountWork,
    ) -> Vec<Stanza> {
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
            Stanza::Message(message) => receive_message(&state, principal, to, message, work).await,
            Stanza::Presence(presence) => {
                super::handlers::presence::receive_remote_presence(
                    &state, principal, to, presence, work,
                )
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

    async fn may_send(&self, account: &BareJid) -> bool {
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        account_exists(&state, account).await
    }
}

/// The per-account locks of this replica, by account. An entry lives while
/// a lock or a waiter holds it.
static ACCOUNT_LOCKS: LazyLock<StdMutex<HashMap<BareJid, Arc<Mutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// The lock of one account's roster items for remote contacts on this
/// replica, held from reading an item to writing it and pushing the change,
/// by inbound and outbound subscription handling alike. Replicas do not
/// share it.
pub(crate) struct AccountLock {
    account: BareJid,
    guard: Option<OwnedMutexGuard<()>>,
}

fn account_locks() -> std::sync::MutexGuard<'static, HashMap<BareJid, Arc<Mutex<()>>>> {
    // Nothing that holds the table can fail: it only looks up, inserts and
    // removes entries. A poisoned table could still hand two holders the
    // same account's roster at once, so the process stops instead.
    ACCOUNT_LOCKS
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
}

/// Take `account`'s lock, waiting for the holder.
pub(crate) async fn lock_account(account: &BareJid) -> AccountLock {
    let mutex = Arc::clone(
        account_locks()
            .entry(account.clone())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    );
    let guard = mutex.lock_owned().await;
    AccountLock {
        account: account.clone(),
        guard: Some(guard),
    }
}

impl Drop for AccountLock {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut locks = account_locks();
        // Only the map holds it: no holder and no waiter is left.
        if locks
            .get(&self.account)
            .is_some_and(|mutex| Arc::strong_count(mutex) == 1)
        {
            locks.remove(&self.account);
        }
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
    snapshot_admits(&state.deps.app_state, domain)
}

/// Whether the S2S snapshot in `app_state` admits `domain` now; nothing is
/// admitted without one.
fn snapshot_admits(app_state: &crate::server::AppState, domain: &jid::DomainRef) -> bool {
    let Some(federation) = app_state.s2s_federation.as_ref() else {
        return false;
    };
    CanonicalDomain::parse_canonical(domain.as_str())
        .is_ok_and(|domain| federation.admission().decide(&domain).is_ok())
}

/// Delete from `account`'s offline store the messages of remote senders
/// whose domain the replica's S2S snapshot no longer admits (ADR-0020 D1),
/// before the store is flushed to a session of the account: a domain
/// removed from the allowlist or denylisted after its senders' messages
/// were stored gets none of them delivered. A sender is remote when its
/// domain is not one this deployment serves (`served`). The sender of a
/// row that refers to the account's archive, as the ingress acceptance of
/// a remote sender's message stores it, is read from `archive`; the
/// archived copy itself stays in the account's archive. Only rows no
/// session has claimed are deleted; a claimed row is being handed to a
/// session already.
///
/// With S2S off nothing is checked: no remote traffic arrives then, the
/// messages stored while it was on were admitted when they arrived, and a
/// deployment without federation pays no extra read of the store at each
/// login. A store that cannot be read leaves every row in place.
pub(crate) async fn forget_unadmitted_remote_messages(
    app_state: &crate::server::AppState,
    served: Option<&ServedDomains>,
    storage: &Arc<dyn PendingDeliveryStorage>,
    archive: &Arc<dyn MamStorage>,
    account: &BareJid,
) {
    if app_state.s2s_federation.is_none() {
        return;
    }
    let Some(served) = served else {
        return;
    };
    let rows = match storage.list(account).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(%error, "reading an offline store to check remote senders' admission failed");
            return;
        }
    };
    for row in rows {
        if row.flushed_in_session.is_some() {
            continue;
        }
        let Some(sender) = stored_sender(archive, &row.payload).await else {
            continue;
        };
        let sender = &sender;
        let remote = matches!(
            served.classify(Some(sender)).unserved(),
            Some(UnservedDestination::Remote(_))
        );
        if !remote || snapshot_admits(app_state, sender.domain()) {
            continue;
        }
        // A flush may claim the row after it was read: only a row still
        // unclaimed is deleted, and one a flush took is the flush's.
        match storage.delete_unclaimed_row(&row.id).await {
            Ok(0) => {}
            Ok(_) => telemetry::offline_unadmitted(),
            Err(error) => {
                warn!(%error, "deleting a stored message of a domain no longer admitted failed");
            }
        }
    }
}

/// The sender of a stored message: the `from` of a message stored whole,
/// or of the archived message a row refers to. `None` when it has none or
/// the archive cannot be read; such a row is left in place.
async fn stored_sender(archive: &Arc<dyn MamStorage>, payload: &PendingPayload) -> Option<Jid> {
    match payload {
        PendingPayload::Transient(message) => message.from.clone(),
        PendingPayload::Archived(stanza_id) => {
            match archive
                .get_message_by_archive_or_stanza_id(&stanza_id.by.to_bare(), stanza_id.as_str())
                .await
            {
                Ok(archived) => archived.map(|archived| archived.from),
                Err(error) => {
                    warn!(%error, "reading a stored message's sender to check its admission failed");
                    None
                }
            }
        }
    }
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

/// `account`'s whole XEP-0191 list, read once to check many addresses
/// against it. `None` when it cannot be read: the caller then treats every
/// address as blocked.
pub(crate) async fn blocklist(state: &WebSocketState, account: &BareJid) -> Option<Blocklist> {
    let db = database(state).await?;
    match DatabaseBlockingStorage::new(db)
        .list_blocked_jid_entries(account)
        .await
    {
        Ok(entries) => Some(Blocklist::new(entries)),
        Err(error) => {
            warn!(%error, "reading a blocking list for s2s account traffic failed");
            None
        }
    }
}

/// The subscription state of `account`'s roster item for `contact`, if it
/// has one.
async fn subscription_of(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> Option<Subscription> {
    let db = database(state).await?;
    DatabaseRosterStorage::new(db)
        .get_roster_item(account, contact)
        .await
        .ok()
        .flatten()
        .map(|row| row.subscription.parse().unwrap_or_default())
}

/// Whether `account` lets `contact` see its presence: a roster item with a
/// subscription `from` or `both` (RFC 6121 §8.1, §8.5.3.1).
pub(crate) async fn shares_presence_with(
    state: &WebSocketState,
    account: &BareJid,
    contact: &BareJid,
) -> bool {
    matches!(
        subscription_of(state, account, contact).await,
        Some(Subscription::From | Subscription::Both)
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
        subscription_of(state, account, contact).await,
        Some(Subscription::To | Subscription::Both)
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
/// for its own accounts: the inbox elements, the DM pin marker, the
/// XEP-0280 `<received/>` and `<sent/>` wrappers of carbon copies and the
/// XEP-0313 `<result/>` wrapper of archive results, the stanza ids it
/// could have assigned ([`strip_local_stanza_ids`]), and the XEP-0511 link
/// metadata and link-preview requests a local sender's message has
/// stripped before this server stamps its own card. A carbon or an archive
/// result names the account's own traffic; one from a remote sender is a
/// forgery a client could take for its own (XEP-0280 §11). A client trusts
/// a link card as this server's (it promotes an inline video to a player
/// only for a URL a card names), so a remote sender's card is removed as a
/// local client's is, and no card is stamped in its place: this server
/// resolves no preview for a remote sender.
fn strip_private_payloads(message: &mut Message, served: Option<&ServedDomains>) {
    strip_local_stanza_ids(message, served);
    waddle_xmpp::xep::strip_link_preview_requests(message);
    waddle_xmpp::xep::strip_link_metadata(message);
    message.payloads.retain(|payload| {
        let namespace = payload.ns();
        namespace != NS_INBOX
            && namespace != NS_WADDLE_INBOX
            && namespace != NS_WADDLE_PIN_V0
            && !payload.is("received", CARBONS_NS)
            && !payload.is("sent", CARBONS_NS)
            && !payload.is("result", MAM_NS)
    });
}

/// Remove from a remote message every XEP-0359 `<stanza-id/>` this
/// deployment could have assigned: one whose `by` names an address at a
/// domain it serves (the recipient account's archive among them), or that
/// cannot be read as an address. A client takes an id in its own
/// account's name for the one its archive gave the message, so a remote
/// sender must not supply it; an entity that assigns stanza ids deletes
/// one whose `by` it would set (XEP-0359 §3); the recipient pass replaces
/// only an id in the name it stamps itself, and a stored message has not
/// met it yet. An id a remote entity assigned in its own name is left, as
/// a routing entity leaves it (§3). Without the served domains every
/// `<stanza-id/>` is removed.
fn strip_local_stanza_ids(message: &mut Message, served: Option<&ServedDomains>) {
    message.payloads.retain(|payload| {
        if !payload.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID) {
            return true;
        }
        let Some(served) = served else {
            return false;
        };
        let Some(by) = payload.attr("by").and_then(|by| by.parse::<Jid>().ok()) else {
            return false;
        };
        matches!(
            served.classify(Some(&by)).unserved(),
            Some(UnservedDestination::Remote(_))
        )
    });
}

/// The nodes the XEP-0060 event notifications in `message` name, if it
/// carries any: one entry for each child of each `<event/>`, `None` for a
/// child that names no node (§7.1.2: `items`, `purge`, `delete`,
/// `configuration` and `subscription` all name one).
pub(crate) fn event_nodes(message: &Message) -> Option<Vec<Option<String>>> {
    let mut events = message
        .payloads
        .iter()
        .filter(|payload| payload.is("event", NS_PUBSUB_EVENT))
        .peekable();
    events.peek()?;
    Some(
        events
            .flat_map(|event| {
                event
                    .children()
                    .map(|child| child.attr("node").map(ToOwned::to_owned))
            })
            .collect(),
    )
}

/// A message from a remote principal to the local account address `to`
/// (RFC 6121 §8.5; `s2s-account-traffic-2047.md`, "Messages").
async fn receive_message(
    state: &WebSocketState,
    principal: &RemotePrincipal,
    to: &Jid,
    mut message: Message,
    work: &AccountWork,
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
    let sender = principal.account().as_bare_jid().clone();
    let full = to.clone().try_into_full().ok();
    let served = state
        .deps
        .service_domains
        .served_domains(state.deps.auth_state.xmpp_domain.as_str());
    // An event notification passes its gate before anything else; one
    // carried by an error is never a notification the account asked for.
    if let Some(nodes) = event_nodes(&message) {
        if message.type_ == MessageType::Error
            || !event_admitted(state, &account, &sender, &nodes).await
        {
            return Vec::new();
        }
        strip_private_payloads(&mut message, served.as_ref());
        deliver_headline(
            state,
            &account,
            &sender,
            full.as_ref(),
            &message,
            Notification::AskedFor,
            work,
        )
        .await;
        return Vec::new();
    }
    strip_private_payloads(&mut message, served.as_ref());
    match message.type_ {
        MessageType::Error => {
            if let Some(full) = full.as_ref() {
                if reaches_full_jid(state, &account, &sender, full).await && !work.abandoned() {
                    let _ = deliver(state, full, &Stanza::Message(message)).await;
                }
            }
            Vec::new()
        }
        MessageType::Headline => {
            deliver_headline(
                state,
                &account,
                &sender,
                full.as_ref(),
                &message,
                Notification::Unsolicited,
                work,
            )
            .await;
            Vec::new()
        }
        MessageType::Chat | MessageType::Normal => {
            // Nothing answers the sender, whatever becomes of the message, so
            // that no sender learns whether the account exists or is online.
            if let Some(from) = message
                .from
                .clone()
                .and_then(|from| from.try_into_full().ok())
                .filter(|from| from.to_bare() == sender)
            {
                receive_direct(
                    state,
                    principal,
                    &account,
                    &from,
                    full.as_ref(),
                    message,
                    work,
                )
                .await;
                return Vec::new();
            }
            // A sender without a resource has no place in an ingress
            // acceptance, which names a full sender: its message is delivered
            // through the recipient's connection and stored offline whole.
            let admitted =
                match deliver_direct(state, &account, &sender, full.as_ref(), &message, work).await
                {
                    Direct::Handled => return Vec::new(),
                    Direct::Untaken { admitted } => admitted,
                };
            // RFC 6121 §8.5.2.2.1: stored for the account's next session,
            // from a contact and a stranger alike.
            store_offline(state, &account, &sender, &message, work, admitted).await;
            Vec::new()
        }
        MessageType::Groupchat => Vec::new(),
    }
}

/// Where a remote sender's `chat` or `normal` message to an account goes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectRoute {
    /// A live resource: the full JID the message names, or the one most
    /// available resource.
    Live(FullJid),
    /// A detached stream-management session awaiting resumption.
    Detached(FullJid),
    /// No resource: the account's offline store.
    Offline,
}

impl DirectRoute {
    /// The address the plan routes the message to: the selected resource,
    /// or the account's bare JID.
    fn address(&self, account: &BareJid) -> Jid {
        match self {
            Self::Live(resource) | Self::Detached(resource) => Jid::from(resource.clone()),
            Self::Offline => Jid::from(account.clone()),
        }
    }
}

/// Select where a `chat` or `normal` message from a remote sender goes
/// (RFC 6121 §8.5.2.1.1, §8.5.3; `s2s-account-traffic-2047.md`,
/// "Messages"): the full JID it names when that resource is bound, live or
/// detached; otherwise, for a `chat` message or one to the bare JID, the
/// live available resource with the highest non-negative priority (ties
/// broken by address), else a connected resource that has not sent presence
/// yet, else a detached session that was available, else the offline
/// store. A `normal` message to a full JID that is not bound is dropped
/// (`None`, §8.5.3.2.1).
///
/// One resource is selected: its recipient pass archives the message once,
/// projects it into the inbox once and copies it to the account's other
/// Carbons-enabled resources as a received carbon (XEP-0280 §6).
async fn select_direct_route(
    state: &WebSocketState,
    account: &BareJid,
    full: Option<&FullJid>,
    message: &Message,
) -> Option<DirectRoute> {
    let resources = super::handlers::account_reads::resources(state, account).await;
    if let Some(full) = full {
        if is_detached(state, full).await {
            return Some(DirectRoute::Detached(full.clone()));
        }
        if resources.contains(full) {
            return Some(DirectRoute::Live(full.clone()));
        }
        if message.type_ != MessageType::Chat {
            return None;
        }
    }
    if let Some(resource) = non_negative_resources(state, account)
        .await
        .into_iter()
        .next()
    {
        return Some(DirectRoute::Live(resource));
    }
    // A connected resource that has not sent presence yet, as local
    // delivery to the bare JID does (RFC 6121 §8.5.2.1: an available or
    // connected resource). An available resource with a negative priority
    // is never one (§8.5.2.1.1).
    let available: Vec<FullJid> =
        super::handlers::account_reads::available_resources(state, account)
            .await
            .into_iter()
            .map(|(resource, _)| resource)
            .collect();
    let mut connected: Vec<FullJid> = resources
        .into_iter()
        .filter(|resource| !available.contains(resource))
        .collect();
    connected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    if let Some(resource) = connected.into_iter().next() {
        return Some(DirectRoute::Live(resource));
    }
    if let Some(resource) = detached_resources(state, account).await.into_iter().next() {
        return Some(DirectRoute::Detached(resource));
    }
    Some(DirectRoute::Offline)
}

/// Whether a XEP-0424 retraction or a XEP-0308 correction in `message`,
/// from the remote `sender`, names a message of `account`'s archive that
/// the sender wrote. The recipient pass tombstones the archived message a
/// retraction names by its id, and keys a correction's state under the
/// message it replaces; for a local sender its own server checked the
/// author first (XEP-0424 Business Rules, XEP-0308 §5), and for a remote
/// one this does, by the same lookup the tombstone resolves its target
/// with. A message that retracts and corrects nothing, or names a message
/// the archive does not hold, passes; one whose target another sender
/// wrote, or whose target cannot be read, does not, and is dropped
/// unanswered.
async fn targets_own_message(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    message: &Message,
) -> bool {
    let target = match waddle_xmpp::xep::xep0424::extract_retraction_from_message(message) {
        Some(waddle_xmpp::xep::xep0424::RetractionKind::Request(retraction)) => {
            retraction.retracts_id
        }
        _ => match waddle_xmpp::xep::extract_correction_from_message(message) {
            Some(correction) => correction.replaces_id,
            None => return true,
        },
    };
    match state
        .deps
        .protocol
        .mam_storage
        .get_message_by_message_id(account, &target)
        .await
    {
        Ok(None) => true,
        Ok(Some(archived)) => archived.from.to_bare() == *sender,
        Err(error) => {
            warn!(%error, "reading the target of a remote sender's retraction or correction failed");
            false
        }
    }
}

/// The S2S admission policy this replica runs (ADR-0020 D1), from which the
/// acceptance of a remote sender's message decides its domain again.
fn admission_policy(
    state: &WebSocketState,
) -> Option<crate::permissions::host_grant::PrincipalPolicy> {
    let domain =
        CanonicalDomain::parse_canonical(state.deps.auth_state.xmpp_domain.as_str()).ok()?;
    Some(state.deps.app_state.grant_principals(domain))
}

/// Accept a `chat` or `normal` message from the remote sender `from` to
/// `account` through the ingress authority, as a local direct message is
/// accepted for its recipient: the recipient's archive (XEP-0313), inbox
/// and received carbons (XEP-0280), its delivery to the selected resource
/// or a detached session, or, with no resource, the offline store
/// (XEP-0160) and the push notification of a local direct message. The
/// sender's own archive, inbox and carbons are its server's. `full` is the
/// full JID the message names, if any.
///
/// A stranger's message that would be held by a detached session or stored
/// offline is admitted only within the strangers' bounds
/// ([`admit_offline`]), and one the store would not keep is not held for a
/// stranger at all. Nothing the plan or the acceptance does answers the
/// sender: a plan that would answer it, or act on anybody but the
/// recipient, is not committed ([`classify_remote_direct_plan`]), and the
/// acceptance's frames are recorded as answered without being sent.
///
/// [`classify_remote_direct_plan`]: crate::server::routes::interpret::classify_remote_direct_plan
async fn receive_direct(
    state: &WebSocketState,
    principal: &RemotePrincipal,
    account: &BareJid,
    from: &FullJid,
    full: Option<&FullJid>,
    message: Message,
    work: &AccountWork,
) {
    use crate::ingress::submission::digest_input;
    use crate::ingress::{
        ImmediateSink, IngressPrincipal, IngressStreamIdentity, IngressSubmission,
        RemoteIngressTarget, RemoteOccupantPrincipal,
    };
    use crate::permissions::host_grant::StreamEvidence;
    use crate::server::routes::interpret::{
        classify_remote_direct_plan, plan_remote_direct_message, RemoteDirectPlan,
    };
    use waddle_xmpp::ingress::{DigestContext, NormalizedTarget, TransportGeneration};

    let sender = from.to_bare();
    let Some(route) = select_direct_route(state, account, full, &message).await else {
        return;
    };
    if !targets_own_message(state, account, &sender, &message).await {
        telemetry::direct_refused();
        return;
    }
    let Some(admission) = admission_policy(state) else {
        return;
    };
    let target = match message.to.as_ref() {
        Some(to) => match to.try_as_full() {
            Ok(to) => NormalizedTarget::Full(to.clone()),
            Err(to) => NormalizedTarget::Bare(to.clone()),
        },
        None => return,
    };
    // The recipient's archive is the one authority of this deployment that
    // stamps the message; the sender's archive is its own server's.
    let Ok(digest_input) = digest_input(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![account.clone()],
            stanza_lang: None,
        },
    ) else {
        telemetry::direct_refused();
        return;
    };
    let deps = build_interpret_deps(state, None);
    let plan = plan_remote_direct_message(&deps, route.address(account), message.clone()).await;
    let held = match classify_remote_direct_plan(&plan, account, &sender) {
        RemoteDirectPlan::Recipient { held } => held,
        RemoteDirectPlan::Empty => return,
        RemoteDirectPlan::Refused => {
            telemetry::direct_refused();
            return;
        }
    };
    // What a detached session holds goes to the offline store when its
    // resume window closes, so a stranger's message is held by one only as
    // it would be stored. The plan, not the route selected before it,
    // decides: a resource that left meanwhile sends the message to the
    // store.
    if held && !admit_offline(state, account, &sender, &message).await {
        return;
    }
    if work.abandoned() {
        return;
    }
    let submission = IngressSubmission {
        identity: IngressStreamIdentity::Remote {
            account: sender.clone(),
            target: RemoteIngressTarget::Account {
                admission: Box::new(admission),
            },
        },
        principal: IngressPrincipal::Remote(RemoteOccupantPrincipal {
            account: sender,
            evidence: StreamEvidence::of(principal),
        }),
        sender: from.clone(),
        target,
        plan,
        digest_input,
        connection_generation: TransportGeneration::Host,
    };
    let authority = &state.deps.protocol.ingress;
    let decision = authority.commit(&submission).await;
    if !decision.class.advances() {
        // The acceptance's assertions refuse a domain no longer admitted
        // and a recipient that does not exist; anything else that did not
        // commit is a failure, and the message is lost (S2S has no
        // retransmission of a stanza the receiving server took).
        if decision.class == waddle_xmpp::telemetry::attributes::IngressDecisionClass::PrincipalMissing
        {
            telemetry::direct_refused();
        } else {
            telemetry::direct_failed();
        }
        return;
    }
    telemetry::direct_accepted();
    // The acceptance is committed: its effects run within their own budget,
    // and an effect that does not finish is recovered by the ingress
    // authority.
    let executed = authority.execute(&decision, &ImmediateSink, &deps);
    let Ok(mut report) = tokio::time::timeout(DIRECT_EFFECTS_TIMEOUT, executed).await else {
        return;
    };
    // A frame of the acceptance would answer the sender. A remote sender is
    // answered nothing, so none is sent, and the obligations are recorded as
    // answered.
    if authority
        .complete_frame_obligations(&mut report)
        .await
        .is_err()
    {
        warn!("recording a remote sender's accepted direct message failed");
    }
}

/// Whether the offline store would keep `message` for an account with no
/// resource, as the classifier of local direct messages decides: XEP-0334
/// hints are honoured, and a message carrying only a chat state is not
/// kept.
fn storable(message: &Message) -> bool {
    !matches!(
        classify_dm_intake(message, &OnlineResources::empty(), &Blocklist::empty()).pending,
        PendingDecision::None
    )
}

/// Whether `message`, from the remote `sender`, may be held for `account`
/// by its offline store or by a detached session, which hands what it holds
/// to that store when its resume window closes: always from a contact; from
/// anybody else only a message the store would keep ([`storable`]),
/// admitted within the bounds ([`RemoteOfflineAdmissions`]) and counted. It
/// is classified before it is counted, so that a message the store would
/// not keep never uses a stranger's share. A message that is not admitted
/// is dropped by the caller; nothing tells the sender.
async fn admit_offline(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    message: &Message,
) -> bool {
    if shares_presence_with(state, account, sender).await {
        return true;
    }
    if !storable(message) {
        return false;
    }
    let Some(db) = database(state).await else {
        return false;
    };
    match RemoteOfflineAdmissions::new(db)
        .admit(account, sender, chrono::Utc::now().timestamp_millis())
        .await
    {
        Ok(OfflineAdmission::Admitted) => true,
        Ok(OfflineAdmission::Bounds) => {
            telemetry::offline_dropped();
            false
        }
        Err(error) => {
            warn!(%error, "admitting a remote sender's message to offline storage failed");
            false
        }
    }
}

/// Whether `resource` is held by a detached stream-management session, on
/// this replica or recorded durably for another, whatever its presence was
/// when it detached, and is not connected: a session that resumed while a
/// record of its detachment lingers is not detached. A resource the
/// connection registry has no entry for is taken for not connected,
/// whatever the account coordinator still binds for it: the coordinator
/// unbinds an ended connection only after the registry forgets it. The
/// registry holds this replica's connections and, on the replica that
/// owns the account, the remote resources it mirrors; a resource that
/// resumed on another replica while a record of its detachment lingers
/// may therefore read as detached here, which applies the stricter rule.
/// A registry that cannot be read answers `true`, for the same reason.
async fn is_detached(state: &WebSocketState, resource: &FullJid) -> bool {
    let account = resource.to_bare();
    let held = match state
        .deps
        .protocol
        .sm_session_registry
        .detached_resources_for_user(&account)
        .await
    {
        Ok(resources) => resources.contains(resource),
        Err(error) => {
            warn!(%error, "listing detached resources for a remote sender's stanza failed");
            true
        }
    };
    if !held {
        return false;
    }
    if !state
        .deps
        .protocol
        .connection_registry
        .is_connected(resource)
    {
        return true;
    }
    !super::handlers::account_reads::resources(state, &account)
        .await
        .contains(resource)
}

/// Whether a remote `sender`'s stanza that the offline store would never
/// keep (a `headline` other than an event notification the account asked
/// for, an `error` message, directed presence) may reach
/// `account`'s full JID `resource`: always when the resource is not
/// detached; when it is, only from a contact. A detached session holds what
/// it is sent until it resumes, within one queue for every sender, so a
/// non-contact's stanzas reach it only as they would reach the offline
/// store, and these would not.
pub(crate) async fn reaches_full_jid(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    resource: &FullJid,
) -> bool {
    !is_detached(state, resource).await || shares_presence_with(state, account, sender).await
}

/// Store `message`, a `chat` or `normal` message from the remote `sender`
/// that no resource of `account` took, in the account's offline store
/// (XEP-0160), if the store would keep it ([`storable`]). Unless it was
/// `admitted` already, it is admitted first ([`admit_offline`]). A message
/// beyond a bound, beyond the store's quota, or that cannot be stored is
/// dropped; nothing tells the sender.
async fn store_offline(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    message: &Message,
    work: &AccountWork,
    admitted: bool,
) {
    if !storable(message) {
        return;
    }
    if !admitted && !admit_offline(state, account, sender, message).await {
        return;
    }
    if work.abandoned() {
        return;
    }
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: account.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(message.clone())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    match state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(row)
        .await
    {
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::Inserted) => telemetry::offline_stored(),
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::QuotaExceeded) => {
            telemetry::offline_dropped();
        }
        Err(error) => warn!(%error, "storing a remote sender's message offline failed"),
    }
}

/// What became of a `chat` or `normal` message offered to the account's
/// resources.
enum Direct {
    /// A resource took it, it was dropped, or the router abandoned the
    /// work: nothing more is applied for it.
    Handled,
    /// Nothing took it; `admitted` when it was already admitted to the
    /// offline store's bounds for a detached session that then did not
    /// take it.
    Untaken { admitted: bool },
}

/// Deliver a `chat` or `normal` message from the remote `sender`: to the
/// full JID it names if that resource is bound, otherwise (a `chat` message
/// only, §8.5.3.2.1) to the one most available resource, a connected one
/// when none is available, or a detached one. A detached session hands what
/// it holds to the offline store when its resume window closes, so a
/// message is handed to one, named or selected, only once admitted as it
/// would be to the store ([`admit_offline`]); one that is not admitted is
/// dropped. It is admitted at most once.
async fn deliver_direct(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    full: Option<&FullJid>,
    message: &Message,
    work: &AccountWork,
) -> Direct {
    let stanza = Stanza::Message(message.clone());
    if work.abandoned() {
        return Direct::Handled;
    }
    let mut admitted = false;
    if let Some(full) = full {
        if is_detached(state, full).await {
            if !admit_offline(state, account, sender, message).await {
                return Direct::Handled;
            }
            admitted = true;
        }
        if work.abandoned() {
            return Direct::Handled;
        }
        match deliver(state, full, &stanza).await {
            Delivery::Taken | Delivery::Dropped => return Direct::Handled,
            Delivery::NotBound if message.type_ != MessageType::Chat => return Direct::Handled,
            Delivery::NotBound => {}
        }
    }
    let live = non_negative_resources(state, account).await;
    for resource in live.iter().take(DELIVERY_ATTEMPTS) {
        if work.abandoned() {
            return Direct::Handled;
        }
        match deliver(state, resource, &stanza).await {
            Delivery::Taken | Delivery::Dropped => return Direct::Handled,
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
            if work.abandoned() {
                return Direct::Handled;
            }
            match deliver(state, resource, &stanza).await {
                Delivery::Taken | Delivery::Dropped => return Direct::Handled,
                Delivery::NotBound => {}
            }
        }
    }
    let detached = detached_resources(state, account).await;
    if detached.is_empty() {
        return Direct::Untaken { admitted };
    }
    if !admitted && !admit_offline(state, account, sender, message).await {
        return Direct::Handled;
    }
    for resource in detached.iter().take(DELIVERY_ATTEMPTS) {
        if work.abandoned() {
            return Direct::Handled;
        }
        match deliver(state, resource, &stanza).await {
            Delivery::Taken | Delivery::Dropped => return Direct::Handled,
            Delivery::NotBound => {}
        }
    }
    Direct::Untaken { admitted: true }
}

/// Whether a `headline` is an event notification the account asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notification {
    /// It passed the event gate ([`event_admitted`]): a subscription the
    /// account made, or the personal eventing service of a contact it is
    /// subscribed to.
    AskedFor,
    /// Any other `headline`.
    Unsolicited,
}

/// Deliver a `headline` from the remote `sender`: to the full JID it names
/// if bound, or to every available resource with a non-negative priority
/// (RFC 6121 §8.5.2.1.1). It is never stored. A detached full JID receives
/// an event notification the account asked for, and any other `headline`
/// only from a contact ([`reaches_full_jid`]).
async fn deliver_headline(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    full: Option<&FullJid>,
    message: &Message,
    notification: Notification,
    work: &AccountWork,
) {
    let stanza = Stanza::Message(message.clone());
    if let Some(full) = full {
        let reaches = notification == Notification::AskedFor
            || reaches_full_jid(state, account, sender, full).await;
        if reaches && !work.abandoned() {
            let _ = deliver(state, full, &stanza).await;
        }
        return;
    }
    for resource in non_negative_resources(state, account).await {
        if work.abandoned() {
            return;
        }
        let _ = deliver(state, &resource, &stanza).await;
    }
}

/// Whether event notifications from `sender` about `nodes` may reach
/// `account`: the sender is a remote account whose presence the account is
/// subscribed to (its personal eventing service, XEP-0163), or the account
/// subscribed to every one of the nodes of that service. A notification
/// naming no node, or more than [`MAX_EVENT_NODES`], passes only the
/// first.
async fn event_admitted(
    state: &WebSocketState,
    account: &BareJid,
    sender: &BareJid,
    nodes: &[Option<String>],
) -> bool {
    if sender.node().is_some() && subscribed_to(state, account, sender).await {
        return true;
    }
    if nodes.is_empty() || nodes.len() > MAX_EVENT_NODES {
        return false;
    }
    let Some(db) = database(state).await else {
        return false;
    };
    let store = RemotePubSubSubscriptions::new(db);
    for node in nodes {
        let Some(node) = node else {
            return false;
        };
        if !matches!(store.holds(account, sender, node).await, Ok(true)) {
            return false;
        }
    }
    true
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

    /// A remote sender's direct message was accepted through the ingress
    /// authority for the recipient account.
    pub(crate) fn direct_accepted() {
        counter_add!(
            "waddle.s2s.account_traffic.direct_accepted",
            "{stanza}",
            "Direct messages from remote senders accepted through the ingress authority for a local account: archived, projected, delivered or stored as a local direct message is for its recipient.",
            1,
        );
    }

    /// A remote sender's direct message was refused unanswered before or
    /// at its acceptance, by a rule: its plan would have answered the
    /// sender or acted beyond the recipient's account, its retraction or
    /// correction named another sender's message, it could not be read as
    /// offered, or the acceptance's assertions refused its domain or its
    /// recipient.
    pub(crate) fn direct_refused() {
        counter_add!(
            "waddle.s2s.account_traffic.direct_refused",
            "{stanza}",
            "Direct messages from remote senders refused unanswered by a rule before or at their ingress acceptance.",
            1,
        );
    }

    /// A remote sender's direct message whose ingress acceptance failed to
    /// commit (a timeout, an exhausted retry, an unknown outcome, a storage
    /// failure): it is lost unanswered.
    pub(crate) fn direct_failed() {
        counter_add!(
            "waddle.s2s.account_traffic.direct_failed",
            "{stanza}",
            "Direct messages from remote senders whose ingress acceptance did not commit, lost unanswered.",
            1,
        );
    }

    /// A message from a remote sender without a resource that no resource
    /// took was stored whole in the account's offline store.
    pub(crate) fn offline_stored() {
        counter_add!(
            "waddle.s2s.account_traffic.offline_stored",
            "{stanza}",
            "Messages from remote senders without a resource that no resource of the local account took, stored whole in the account's offline store; a sender with a resource's message is accepted through the ingress authority instead.",
            1,
        );
    }

    /// A remote sender's message no resource took was dropped unanswered:
    /// the sender's, its domain's or the account's bound for
    /// non-contacts, or the offline store's quota, was reached.
    pub(crate) fn offline_dropped() {
        counter_add!(
            "waddle.s2s.account_traffic.offline_dropped",
            "{stanza}",
            "Messages from remote senders that a detached session or the offline store would have held, dropped unanswered because a bound on offline storage for remote senders, or the offline store's quota for a message stored whole, was reached.",
            1,
        );
    }

    /// A stored message of a remote sender whose domain is no longer
    /// admitted was deleted before the offline store was flushed.
    pub(crate) fn offline_unadmitted() {
        counter_add!(
            "waddle.s2s.account_traffic.offline_unadmitted",
            "{stanza}",
            "Stored messages from remote senders deleted before an offline flush because the replica's S2S snapshot no longer admits the sender's domain.",
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
