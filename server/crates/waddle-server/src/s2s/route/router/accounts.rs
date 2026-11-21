//! The router's half of account traffic (ADR-0020 D7; gate G18): queued
//! stanzas for local accounts handed to the account side, remote requests
//! to local resources registered for correlation, and a local account's
//! messages and presence for remote JIDs that are not room traffic.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use jid::{BareJid, Jid};
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

use super::super::accounts::{
    carries_dm_pin, is_outbound_account_traffic, AccountWork, MAX_ACCOUNT_REPLIES,
};
use super::super::correlation::{
    CorrelationDirection, CorrelationError, CorrelationKey, PendingRequest, RequestKind,
};
use super::super::local::{LocalDeliveryOutcome, LocalOrigin};
use super::super::principal::RemotePrincipal;
use super::super::reach::Refusal;
use super::super::replies;
use super::super::room_delivery::AccountTraffic;
use super::super::telemetry;
use super::{duration_ms, lock, now_ms, Inner, Pair, Purpose, Queued};
use crate::destination::UnroutableReason;
use crate::s2s::conn::VerifiedPeerStream;

/// A remote request to a local resource this replica registered, counted
/// against its bounds until its response leaves or it expires.
#[derive(Debug)]
struct InboundEntry {
    peer: CanonicalDomain,
    expires_at_ms: i64,
}

/// The remote requests to local resources this replica registered and has
/// not yet seen answered or expire. A response that leaves from another
/// replica, where the requested resource's session is, completes the row
/// there; this replica forgets the entry only at its expiry. For requests
/// answered that way the bounds are therefore a rate: at most the bound's
/// number of such requests per request lifetime
/// ([`super::RouterLimits::account_request_ttl`], 60 s by default).
#[derive(Debug, Default)]
pub(super) struct InboundRequests {
    entries: HashMap<CorrelationKey, InboundEntry>,
    per_peer: HashMap<CanonicalDomain, usize>,
}

impl InboundRequests {
    fn reserve(
        &mut self,
        key: CorrelationKey,
        peer: CanonicalDomain,
        expires_at_ms: i64,
        total: usize,
        per_peer: usize,
    ) -> bool {
        if self.entries.contains_key(&key) {
            return false;
        }
        let of_peer = self.per_peer.get(&peer).copied().unwrap_or(0);
        if self.entries.len() >= total || of_peer >= per_peer {
            return false;
        }
        *self.per_peer.entry(peer.clone()).or_insert(0) += 1;
        self.entries.insert(
            key,
            InboundEntry {
                peer,
                expires_at_ms,
            },
        );
        true
    }

    fn release(&mut self, key: &CorrelationKey) {
        if let Some(entry) = self.entries.remove(key) {
            super::decrement(&mut self.per_peer, &entry.peer);
        }
    }

    fn expire(&mut self, now_ms: i64) {
        let expired: Vec<CorrelationKey> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at_ms <= now_ms)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.release(&key);
        }
    }
}

impl Inner {
    /// Whether an account side is attached, and so account traffic is
    /// enabled in both directions.
    pub(super) fn accounts_attached(&self) -> bool {
        self.accounts.get().is_some()
    }

    /// Decide one queued stanza for a local account. Without an account
    /// side it is refused as D7's interim rule says; otherwise an IQ
    /// request is admitted or refused here, and a message or presence goes
    /// to the account side, whose answers go back to the sender.
    pub(super) async fn account_traffic(self: &Arc<Self>, traffic: AccountTraffic) {
        let AccountTraffic {
            stream,
            principal,
            to,
            stanza,
        } = traffic;
        let Some(accounts) = self.accounts.get().cloned() else {
            return self.refuse_account_traffic(&stream, &stanza);
        };
        if let Stanza::Iq(iq) = &stanza {
            if matches!(iq.as_ref(), Iq::Get { .. } | Iq::Set { .. }) {
                return self.account_request(&stream, &principal, &to, stanza).await;
            }
            // Responses never reach the account side: they complete a
            // request or are dropped before they are queued.
            return;
        }
        // The account side's work runs in a task of its own, so that a
        // worker that stops waiting for it never drops it in the middle of
        // a write. When the worker stops waiting it abandons the work: the
        // account side then applies nothing more for this stanza, so the
        // sender's next stanza, decided next, is never overtaken by it, and
        // answers that come after the deadline are dropped.
        let work = AccountWork::new();
        let abandoned = work.clone();
        let task = self
            .tasks
            .spawn(async move { accounts.receive(&principal, &to, stanza, &work).await });
        let received = tokio::time::timeout(self.limits.account_timeout, task).await;
        let Ok(Ok(answers)) = received else {
            abandoned.abandon();
            telemetry::account_traffic_timed_out();
            return;
        };
        let total = answers.len();
        for answer in answers.into_iter().take(MAX_ACCOUNT_REPLIES) {
            self.reply(&stream, answer);
        }
        if total > MAX_ACCOUNT_REPLIES {
            telemetry::account_replies_dropped(total - MAX_ACCOUNT_REPLIES);
        }
    }

    /// D7's interim refusal of account traffic, as if the account did not
    /// exist (RFC 6121 §8.5.1): a message or IQ request receives
    /// `service-unavailable`, presence and responses nothing.
    pub(super) fn refuse_account_traffic(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        stanza: &Stanza,
    ) {
        tracing::debug!(refusal = ?Refusal::AccountTraffic, "refusing a stanza from an s2s stream");
        if matches!(stanza, Stanza::Presence(_)) {
            return;
        }
        let home = stream.local().as_domain_ref().to_owned();
        if let Some(reply) = replies::error_reply(stanza, replies::service_unavailable(&home)) {
            self.reply(stream, reply);
        }
    }

    /// A remote principal's IQ request for a local resource (RFC 6121
    /// §8.5.3.1): delivered to that resource only when the account side
    /// admits it, registered first so that only the resource's own
    /// response leaves; refused with `service-unavailable` otherwise, the
    /// same answer whatever the account's state.
    async fn account_request(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        principal: &RemotePrincipal,
        to: &Jid,
        stanza: Stanza,
    ) {
        let home = stream.local().as_domain_ref().to_owned();
        let refuse = |router: &Arc<Self>, stanza: &Stanza| {
            if let Some(reply) = replies::error_reply(stanza, replies::service_unavailable(&home)) {
                router.reply(stream, reply);
            }
        };
        let Some(accounts) = self.accounts.get().cloned() else {
            return refuse(self, &stanza);
        };
        // D1 at the effect boundary: a domain the replica no longer admits
        // gets nothing, not even a refusal, and nothing is read for it.
        if !accounts.admits(principal) {
            return;
        }
        let Ok(resource) = to.clone().try_into_full() else {
            return refuse(self, &stanza);
        };
        // A remote room the resource occupies or is joining may ask its
        // occupant (XEP-0410 §3.2 reflects a self-ping, and occupants send
        // requests through the room).
        let room_occupant = matches!(
            self.rooms.get(&resource, &principal.address().to_bare()).await,
            Ok(Some(occupancy)) if occupancy.phase.occupies()
        );
        let admitted = tokio::time::timeout(
            self.limits.account_timeout,
            accounts.admits_request(principal, &resource, room_occupant),
        )
        .await;
        if !matches!(admitted, Ok(true)) {
            return refuse(self, &stanza);
        }
        let Stanza::Iq(iq) = &stanza else {
            return;
        };
        let (kind, payload_namespace) = match iq.as_ref() {
            Iq::Get { payload, .. } => (RequestKind::Get, payload.ns()),
            Iq::Set { payload, .. } => (RequestKind::Set, payload.ns()),
            Iq::Result { .. } | Iq::Error { .. } => return,
        };
        let now = now_ms();
        let expires_at_ms = now + duration_ms(self.limits.account_request_ttl);
        let key = CorrelationKey {
            direction: CorrelationDirection::Inbound,
            local: Jid::from(resource.clone()),
            remote: principal.address().clone(),
            id: iq.id().to_owned(),
        };
        let reserved = lock(&self.inbound_requests).reserve(
            key.clone(),
            principal.home().clone(),
            expires_at_ms,
            self.limits.account_requests,
            self.limits.account_requests_per_peer,
        );
        if !reserved {
            telemetry::account_request_refused();
            let error = replies::error(
                &home,
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                "Too many requests from this domain are pending.",
            );
            if let Some(reply) = replies::error_reply(&stanza, error) {
                self.reply(stream, reply);
            }
            return;
        }
        let pending = PendingRequest {
            key: key.clone(),
            kind,
            payload_namespace,
            owner: self.owner.clone(),
            expires_at_ms,
        };
        let registered = self.correlations.register(&pending, now).await;
        if let Err(error) = registered {
            lock(&self.inbound_requests).release(&key);
            let error = match error {
                CorrelationError::Duplicate | CorrelationError::KeyTooLong => {
                    replies::bad_request(&home)
                }
                CorrelationError::Storage(_) => replies::error(
                    &home,
                    ErrorType::Wait,
                    DefinedCondition::InternalServerError,
                    "The request could not be recorded.",
                ),
            };
            if let Some(reply) = replies::error_reply(&stanza, error) {
                self.reply(stream, reply);
            }
            return;
        }
        let delivered = match self.local.get() {
            Some(local) => tokio::time::timeout(
                self.limits.room_delivery_timeout,
                local.deliver(&resource, stanza.clone(), LocalOrigin::Remote),
            )
            .await
            .unwrap_or(LocalDeliveryOutcome::Unavailable),
            None => LocalDeliveryOutcome::Unavailable,
        };
        if delivered == LocalDeliveryOutcome::Delivered {
            return;
        }
        // Nothing reached a resource: the request is withdrawn and answered
        // as if no resource matched (RFC 6121 §8.5.3.2.3).
        lock(&self.inbound_requests).release(&key);
        if matches!(self.correlations.withdraw(&key).await, Ok(true)) {
            refuse(self, &stanza);
        }
    }

    /// The remote request a local response completed has left: it no
    /// longer counts against this replica's bounds.
    pub(super) fn release_inbound(&self, key: &CorrelationKey) {
        if key.direction == CorrelationDirection::Inbound {
            lock(&self.inbound_requests).release(key);
        }
    }

    /// Forget the remote requests whose lifetime has passed.
    pub(super) fn expire_inbound(&self) {
        lock(&self.inbound_requests).expire(now_ms());
    }

    /// Route a local account's message or presence for a remote JID that
    /// is not room traffic (gate G18): account traffic, routed once an
    /// account side is attached; [`Inner::route`] has already refused it
    /// for an account that may not send. A `groupchat` message is room
    /// traffic only, and a message carrying the DM pin marker never leaves.
    pub(super) async fn route_account(
        self: &Arc<Self>,
        pair: Pair,
        sender: Jid,
        stanza: Stanza,
    ) -> Result<(), UnroutableReason> {
        if !self.accounts_attached() || !is_outbound_account_traffic(&stanza) {
            return Err(UnroutableReason::RefusedByPolicy);
        }
        if let Stanza::Message(message) = &stanza {
            if carries_dm_pin(message) {
                return Err(UnroutableReason::RefusedByPolicy);
            }
        }
        let purpose = self.account_purpose(&sender, &pair)?;
        if self.enqueue(pair, Queued { stanza, purpose }) {
            Ok(())
        } else {
            Err(UnroutableReason::EgressBusy)
        }
    }

    /// The purpose of an account stanza from `sender` on `pair`, holding
    /// one place of the sender's share of that pair's queue: an account may
    /// hold at most [`super::RouterLimits::account_queue_per_sender`] places
    /// on one pair, so that one account's burst never fills a pair's queue
    /// for the others. The share is per pair, so that a pair whose stream
    /// cannot be established, and whose queue holds the account's stanzas
    /// until they are answered, never holds back the account's traffic to
    /// other domains. [`UnroutableReason::EgressBusy`] when its share is
    /// full.
    pub(super) fn account_purpose(
        &self,
        sender: &Jid,
        pair: &Pair,
    ) -> Result<Purpose, UnroutableReason> {
        let key = (sender.to_bare(), pair.clone());
        let mut shares = lock(&self.account_shares);
        let held = shares.get(&key).copied().unwrap_or(0);
        if held >= self.limits.account_queue_per_sender {
            telemetry::account_share_full();
            return Err(UnroutableReason::EgressBusy);
        }
        shares.insert(key.clone(), held + 1);
        Ok(Purpose::Account {
            sender: sender.clone(),
            _share: AccountShare {
                shares: Arc::clone(&self.account_shares),
                key,
            },
        })
    }

    /// Whether `sender`, an address of the served domain with a localpart,
    /// is a local account's: the senders [`Self::account_may_send`] is
    /// asked about.
    pub(super) fn sent_by_account(&self, sender: &Jid) -> bool {
        sender.node().is_some() && *sender.domain() == *self.served.local()
    }

    /// Whether the local account that `sender` names may send to remote
    /// JIDs now (it exists and has not been retired), as the account side
    /// decides; any failure, or no account side, refuses.
    pub(super) async fn account_may_send(&self, sender: &Jid) -> bool {
        let Some(accounts) = self.accounts.get().cloned() else {
            return false;
        };
        let account = sender.to_bare();
        matches!(
            tokio::time::timeout(self.limits.account_timeout, accounts.may_send(&account)).await,
            Ok(true)
        )
    }

    /// An account stanza that could not be sent: a message's sender learns
    /// why (RFC 6120 §10.4.3); presence is dropped, since the server sends
    /// most of it on the account's behalf and nothing waits for it.
    pub(super) async fn answer_account(
        &self,
        sender: &Jid,
        stanza: &Stanza,
        failure: super::Failure,
    ) {
        if !matches!(stanza, Stanza::Message(_)) {
            return;
        }
        let Ok(sender) = sender.clone().try_into_full() else {
            return;
        };
        if let Some(reply) = replies::error_reply(stanza, failure.error(self.home())) {
            self.deliver_local(&sender, reply, LocalOrigin::Server)
                .await;
        }
    }
}

/// The places each local account's stanzas hold on each pair's queue.
pub(super) type AccountShares = Arc<Mutex<HashMap<(BareJid, Pair), usize>>>;

/// One place of an account's share of one pair's queue, given back when
/// the stanza holding it is sent, answered or dropped.
#[derive(Debug)]
pub(super) struct AccountShare {
    shares: AccountShares,
    key: (BareJid, Pair),
}

impl Drop for AccountShare {
    fn drop(&mut self) {
        super::decrement(&mut lock(&self.shares), &self.key);
    }
}
