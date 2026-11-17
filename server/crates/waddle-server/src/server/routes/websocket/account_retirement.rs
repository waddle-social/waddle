//! What this replica does with a retired account's sessions (ADR-0020 D8).
//!
//! Once this replica takes up a request to end a retired account's sessions
//! (`crate::auth::session_endings`), it holds the account in its
//! [`RetiredAccounts`]. From then on:
//!
//! - a binding of the account is refused before it is published
//!   (`registration`), as is a resume: the stream ends with
//!   `<not-authorized/>` (RFC 6120 §4.9.3.12);
//! - every live stream of the account this replica hosts is force-detached
//!   as a fresh-bind replacement, the terminal close that a replacing bind
//!   uses, and its peer is sent `<not-authorized/>` instead of
//!   `<conflict/>`. The connection's own teardown then broadcasts its
//!   unavailable presence, leaves its rooms (a local room fans the departure
//!   out to its remote occupants) and its remote rooms, retires its binding
//!   and closes its stream-management session without detaching it;
//! - every stream-management session of the account that waits for a
//!   resume is ended, in memory and on the durable authority, through the
//!   terminal path an invalidated detached session takes, which also
//!   broadcasts its unavailable presence and leaves its rooms.
//!
//! What this replica holds is the evidence: a stream counts as closed once
//! the registry no longer holds its binding, and the detached sessions as
//! ended once none of the account's is left. Nothing here leaves the
//! replica's own state; the S2S router already refuses everything a
//! retired account sends, so the account's remote contacts are sent no
//! final unavailable presence.
//!
//! Log lines carry no JID.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use jid::{BareJid, FullJid};
use tracing::{debug, warn};
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::registry::{
    ConnectionEntry, ForceDetachOfferError, ForceDetachOrigin, ForceDetachRequest,
};

use super::WebSocketState;
use crate::auth::directory::canonical_account_jid;
use crate::auth::session_endings::EndedSessions;
use crate::sm_promotion::DisplacedPromotionDeps;

/// Longest the replica waits for one live stream of a retired account to
/// close after it asked the connection to: the longest wait a replacing
/// bind gives a displaced binding.
const STREAM_CLOSE_WAIT: Duration = Duration::from_secs(10);
/// How often the registry is read again while a closing stream's binding
/// is still published.
const STREAM_CLOSE_POLL: Duration = Duration::from_millis(50);

/// The canonical bare JID of the account `jid` names, the form a request
/// names it in. `None` for a JID that names no account.
fn account_key(jid: &BareJid) -> Option<BareJid> {
    canonical_account_jid(jid.node()?.as_str(), jid.domain().as_str())
}

/// When this replica took up the retirement of an account.
#[derive(Clone, Copy, Debug)]
struct TakenUp {
    /// The newest request for the account, on its recorder's clock.
    requested_at_ms: i64,
    /// When this replica took that request up, on its own clock.
    at: Instant,
}

/// The retired accounts whose requests this replica has taken up within
/// the window, keyed by canonical bare JID. Each test state holds its own,
/// so several servers in one process keep apart.
#[derive(Debug, Default)]
pub struct RetiredAccounts {
    accounts: DashMap<BareJid, TakenUp>,
}

impl RetiredAccounts {
    /// Hold `account`, named in canonical form by a request recorded at
    /// `requested_at_ms`.
    pub(crate) fn insert(&self, account: BareJid, requested_at_ms: i64) {
        let taken_up = TakenUp {
            requested_at_ms,
            at: Instant::now(),
        };
        self.accounts
            .entry(account)
            .and_modify(|held| {
                if held.requested_at_ms < requested_at_ms {
                    *held = taken_up;
                }
            })
            .or_insert(taken_up);
    }

    /// Whether `jid` names an account this replica holds as retired.
    pub(crate) fn holds(&self, jid: &BareJid) -> bool {
        if self.accounts.is_empty() {
            return false;
        }
        self.accounts.contains_key(jid)
            || account_key(jid).is_some_and(|key| self.accounts.contains_key(&key))
    }

    /// Forget the accounts whose newest request was recorded before
    /// `before_ms`: a replica acts on requests of the window only.
    pub(crate) fn forget_requested_before(&self, before_ms: i64) {
        self.accounts
            .retain(|_, held| held.requested_at_ms >= before_ms);
    }

    /// The accounts this replica took up within the last `within`.
    pub(crate) fn taken_up_within(&self, within: Duration) -> Vec<BareJid> {
        self.accounts
            .iter()
            .filter(|held| held.at.elapsed() < within)
            .map(|held| held.key().clone())
            .collect()
    }
}

/// What one pass over a retired account's sessions did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EndingPass {
    pub(crate) ended: EndedSessions,
    /// Whether no stream and no detached session of the account is left on
    /// this replica.
    pub(crate) complete: bool,
}

/// End every live stream and every detached session of the retired
/// account `account` (canonical) that this replica holds. The caller holds
/// the account in [`RetiredAccounts`] first, so no binding of it is
/// published after this pass reads the registry.
pub(crate) async fn end_account_sessions(state: &WebSocketState, account: &BareJid) -> EndingPass {
    let (streams_closed, streams_complete) = close_live_streams(state, account).await;
    let (detached_sessions_ended, detached_complete) = end_detached_sessions(state, account).await;
    EndingPass {
        ended: EndedSessions {
            streams_closed,
            detached_sessions_ended,
        },
        complete: streams_complete && detached_complete,
    }
}

/// The live streams of `account` this replica hosts, with the binding each
/// one holds.
fn hosted_streams(state: &WebSocketState, account: &BareJid) -> Vec<(FullJid, ConnectionEntry)> {
    let registry = &state.deps.protocol.connection_registry;
    registry
        .list_connections()
        .into_iter()
        .filter(|jid| {
            let bare = jid.to_bare();
            bare == *account || account_key(&bare).as_ref() == Some(account)
        })
        .filter_map(|jid| {
            let entry = registry.entry(&jid)?;
            (entry.is_locally_hosted() && entry.binding_ref().is_some()).then_some((jid, entry))
        })
        .collect()
}

/// Close every live stream of `account` this replica hosts. Returns how
/// many closed and whether none is left.
pub(crate) async fn close_live_streams(state: &WebSocketState, account: &BareJid) -> (u64, bool) {
    let streams = hosted_streams(state, account);
    if streams.is_empty() {
        return (0, true);
    }
    let outcomes = futures::future::join_all(
        streams
            .into_iter()
            .map(|(jid, entry)| async move { close_live_stream(state, &jid, &entry).await }),
    )
    .await;
    let closed = outcomes.iter().filter(|closed| **closed).count();
    let closed_count = u64::try_from(closed).unwrap_or(u64::MAX);
    if closed_count > 0 {
        waddle_xmpp::counter_add!(
            "waddle.account_retirement.streams_closed",
            "{stream}",
            "Live streams of retired accounts this replica closed",
            closed_count,
        );
    }
    (closed_count, closed == outcomes.len())
}

/// Ask the connection hosting `jid` to end its binding as a terminal
/// replacement, and wait until the registry no longer holds that binding.
async fn close_live_stream(state: &WebSocketState, jid: &FullJid, entry: &ConnectionEntry) -> bool {
    let Some(binding) = entry.binding_ref().cloned() else {
        return true;
    };
    let deadline = tokio::time::Instant::now() + STREAM_CLOSE_WAIT;
    let (ack, ack_rx) = tokio::sync::oneshot::channel();
    let offer = entry.try_force_detach_and_interrupt(ForceDetachRequest {
        origin: ForceDetachOrigin::FreshBindReplacement,
        target_binding: Some(binding.clone()),
        requester_bare_jid: jid.to_bare(),
        ack,
    });
    match offer {
        // The acknowledgement only says when the connection's teardown
        // ended; a dropped one is an unknown outcome. The registry decides.
        Ok(_) => {
            let _ = tokio::time::timeout_at(deadline, ack_rx).await;
        }
        Err(ForceDetachOfferError::Full(_)) => {
            debug!("a retired account's stream is busy with another detach; checking its binding");
        }
        Err(ForceDetachOfferError::Closed(_) | ForceDetachOfferError::NoLocalBinding(_)) => {}
    }
    loop {
        if !holds_binding(state, jid, &binding) {
            return true;
        }
        if tokio::time::Instant::now() + STREAM_CLOSE_POLL > deadline {
            warn!("a retired account's stream did not close in time; trying again next tick");
            return false;
        }
        tokio::time::sleep(STREAM_CLOSE_POLL).await;
    }
}

fn holds_binding(state: &WebSocketState, jid: &FullJid, binding: &BindingRef) -> bool {
    state
        .deps
        .protocol
        .connection_registry
        .entry(jid)
        .is_some_and(|entry| entry.binding_ref() == Some(binding))
}

/// End every stream-management session of `account` that waits for a
/// resume: those this replica keeps in memory and those of the durable
/// authority. Returns how many this replica ended and whether none is
/// left.
async fn end_detached_sessions(state: &WebSocketState, account: &BareJid) -> (u64, bool) {
    let registry = &state.deps.protocol.sm_session_registry;
    let resources: BTreeSet<FullJid> = match registry.detached_resources_for_user(account).await {
        Ok(resources) => resources.into_iter().collect(),
        Err(error) => {
            warn!(%error, "a retired account's detached sessions could not be read");
            return (0, false);
        }
    };
    if resources.is_empty() {
        return (0, true);
    }
    let mut ended: u64 = 0;
    for resource in &resources {
        match registry.invalidate_sessions_for_jid(resource).await {
            Ok(removed) if !removed.is_empty() => {
                // As a replacing bind does: what the sessions retained moves
                // to offline delivery, then each one leaves its rooms and
                // its presence ends.
                crate::sm_promotion::promote_displaced_sessions(
                    removed.clone(),
                    DisplacedPromotionDeps {
                        sm_registry: registry,
                        connection_registry: &state.deps.protocol.connection_registry,
                        user_registry: &state.deps.protocol.user_registry,
                        pending_storage: &state.deps.protocol.pending_delivery_storage,
                        blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
                        server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                    },
                )
                .await;
                for detached in removed {
                    super::cleanup::cleanup_invalidated_detached_session(state, detached, None)
                        .await;
                    ended = ended.saturating_add(1);
                }
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "a retired account's detached session could not be ended");
            }
        }
        let durable = super::cleanup::replace_durable_resource_sessions(state, resource).await;
        ended = ended.saturating_add(u64::try_from(durable).unwrap_or(u64::MAX));
    }
    if ended > 0 {
        waddle_xmpp::counter_add!(
            "waddle.account_retirement.detached_sessions_ended",
            "{session}",
            "Detached stream-management sessions of retired accounts this replica ended",
            ended,
        );
    }
    // A session whose claim another replica holds is left to it, and one
    // that resumed meanwhile is a live stream again: the next pass takes
    // either.
    let complete = match registry.detached_resources_for_user(account).await {
        Ok(left) => left.is_empty(),
        Err(error) => {
            warn!(%error, "a retired account's detached sessions could not be read again");
            false
        }
    };
    (ended, complete)
}
