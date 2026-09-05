//! Concrete [`crate::SfuService`] impl backed by a LiveKit deployment.
//!
//! Keeps an in-memory registry of active calls keyed by [`CallId`]
//! containing the set of joined [`Identity`] values, used by the MUC
//! focus path to decide when a call has ended.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dashmap::DashMap;
use futures::{stream, StreamExt};
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use waddle_xmpp_core::OccupancySessionGeneration;

use crate::admin::{
    admin_base_url_from_ws, AdminCallObserver, ListedRoomName, LiveKitAdmin, ReqwestLiveKitAdmin,
};
use crate::call::{
    CallGeneration, CallId, CallState, CallTeardownIntentLite, Identity, MediaCapabilities,
    ObservedCallSids, ParticipantSid, RoomSid, SessionBinding, SessionScopedTeardown,
    SidObservationDirection, SidObservationDisposition, TeardownDisposition, TeardownTargetLite,
};
use crate::config::{SfuConfig, WebsocketUrl};
use crate::error::SfuError;
use crate::token::{mint_join_token, IssuedJti, JoinToken, Jti, MintInputs};
use crate::turn::{mint_turn_credential, TurnCredential, TurnHost};
use crate::SfuService;

/// Upper bound on outstanding (un-revoked) JTIs tracked per
/// `(call, identity)`. A participant should never be sitting on more
/// than a handful of concurrent tokens — every reconnect mints a
/// fresh one and the previous one is supposed to drop. The cap turns
/// a buggy client (or a malicious one trying to wedge the tracker)
/// from an unbounded memory leak into a strict FIFO: the oldest
/// outstanding JTI is dropped and forgotten when the cap is hit.
pub(crate) const MAX_ISSUED_PER_PARTICIPANT: usize = 16;

/// Upper bound on concurrent LiveKit admin REST calls in flight.
/// `unregister_call_participant` fires-and-forgets these from the
/// teardown hot path; with one HTTP round-trip + 5s timeout per call
/// a burst of session-terminates would otherwise spawn arbitrarily
/// many reqwest tasks. The semaphore is a fixed-size FIFO valve.
const ADMIN_CONCURRENCY: usize = 32;

/// Attempts allowed for a grant *downgrade* push. Losing voice must
/// actually reach LiveKit — a single dropped request would leave a
/// de-voiced occupant publishing — so a transient failure is retried a
/// few times before the warn-and-give-up path.
const GRANT_DOWNGRADE_ATTEMPTS: u32 = 3;

/// Linear backoff base between grant-downgrade attempts.
const GRANT_RETRY_BACKOFF: StdDuration = StdDuration::from_millis(250);

/// Grace window after registration before a participant becomes
/// eligible for ghost reconciliation. The registry is populated at
/// Jingle `session-initiate` — *before* the client actually connects
/// its WebSocket to LiveKit — so a freshly-registered participant is
/// legitimately absent from LiveKit's `ListParticipants` for the few
/// seconds it takes to ring + connect. Sweeping inside that window
/// would tear down a call that is still coming up; this grace period
/// keeps reconciliation to genuinely stale entries.
pub const RECONCILE_GRACE_SECONDS: i64 = 120;

/// Number of consecutive reconciliation passes a participant must be
/// observed absent from LiveKit before being swept (#1127). One
/// not-found observation is a weak signal: a LiveKit pod restart makes
/// a single pass report every room as gone while clients silently
/// reconnect and media keeps flowing. Requiring absence across two
/// consecutive passes turns the sweep into "confirmed gone for a full
/// reconcile interval" while still reaping genuinely departed
/// participants within ~2 passes.
pub(crate) const RECONCILE_ABSENT_PASSES: u32 = 2;

/// Generation tombstones survive a fully-cleared call long enough to
/// fence delayed teardown effects and quick rejoins across replicas.
const GENERATION_TOMBSTONE_TTL_HOURS: i64 = 25;

/// Maximum concurrent `ListParticipants` probes in one reconcile pass.
/// Each probe has a five-second HTTP timeout. Eight-way fan-out reduces
/// a pathological 100-room all-timeout pass from 500 seconds serially to
/// 13 waves (about 65 seconds), and keeps it under the 60-second interval
/// whenever at least one wave completes before the hard timeout.
pub const RECONCILE_CONCURRENCY: usize = 8;

#[derive(Debug, Clone)]
struct ParticipantState {
    participant_sid: Option<ParticipantSid>,
    /// When `participant_sid` was last written from a live observation
    /// (join webhook or occupancy probe). Lets the reconcile probe
    /// advance a stale sid without ever rolling back a newer webhook
    /// observation (#1612 review round 9).
    participant_sid_observed_at: Option<DateTime<Utc>>,
    /// The producing EVENT's `createdAt` for the stored sid, when the
    /// webhook envelope supplied one. Event-clock lineage (distinct
    /// from the local-clock stamp above): a redelivered stale join is
    /// refused when its event time does not postdate this (#1612
    /// review round 12).
    participant_sid_event_at: Option<DateTime<Utc>>,
    /// Set when a join carrying a DIFFERENT sid arrived with an event
    /// time EQUAL to the stored lineage: whole-second `createdAt`
    /// cannot order the two, so neither sid is authoritative. While
    /// contested, a leave matching the stored fence is deferred rather
    /// than executed — a delayed old leave must not clear a live
    /// same-second reconnect. Cleared by the occupancy reconcile once
    /// a probe STARTED AFTER the ambiguity arose confirms which sid is
    /// live, or by a strictly newer join (#1612 review round 14).
    participant_sid_contested_at: Option<DateTime<Utc>>,
    first_registered_at: DateTime<Utc>,
    registered_without_mint: bool,
    /// The signaling-session identifier (Jingle sid) that produced
    /// this registration, when the Jingle layer bound one (#1608).
    /// `None` for webhook/probe-restored registrations, which never
    /// saw the signaling leg.
    session: Option<SessionBinding>,
    /// The MUC occupant-session generation that produced this
    /// registration (#1703). `None` for non-MUC registrations and
    /// webhook/probe-restored entries.
    occupant_session: Option<OccupancySessionGeneration>,
}

impl ParticipantState {
    fn new(first_registered_at: DateTime<Utc>) -> Self {
        Self {
            participant_sid: None,
            participant_sid_observed_at: None,
            participant_sid_event_at: None,
            participant_sid_contested_at: None,
            first_registered_at,
            registered_without_mint: false,
            session: None,
            occupant_session: None,
        }
    }

    fn restored(
        first_registered_at: DateTime<Utc>,
        participant_sid: Option<ParticipantSid>,
    ) -> Self {
        Self {
            participant_sid_observed_at: participant_sid.is_some().then_some(first_registered_at),
            participant_sid_event_at: None,
            participant_sid_contested_at: None,
            participant_sid,
            first_registered_at,
            registered_without_mint: true,
            session: None,
            occupant_session: None,
        }
    }

    /// A fresh registration sourced from a webhook observation. Unlike
    /// [`Self::restored`] (probe/adoption results, which carry no event
    /// clock), this preserves the producing event's `createdAt` so the
    /// stored sid starts with its event lineage — without it, the next
    /// same-second join would skip both lineage gates and overwrite the
    /// fence (#1612 review round 14).
    fn observed(first_registered_at: DateTime<Utc>, observed_sids: &ObservedCallSids) -> Self {
        Self {
            participant_sid_event_at: observed_sids
                .participant_sid
                .is_some()
                .then_some(observed_sids.observed_event_at)
                .flatten(),
            ..Self::restored(first_registered_at, observed_sids.participant_sid.clone())
        }
    }
}

#[derive(Debug, Clone)]
struct CallEntry {
    generation: CallGeneration,
    /// When this entry was created. Guards the listing-learn arm for
    /// sid-less entries: a fresh same-name rejoin entry created AFTER a
    /// `ListRooms` request went out must not learn that snapshot's (old
    /// incarnation) sid (#1612 review round 9).
    created_at: DateTime<Utc>,
    room_sid: Option<RoomSid>,
    /// When `room_sid` was last written from a live observation (join
    /// webhook, adoption, or listing rotation). Guards reconcile-time
    /// rotation: a `ListRooms` snapshot taken BEFORE a webhook already
    /// advanced the sid must not roll the fence back to the stale
    /// incarnation (#1612 review round 8).
    room_sid_observed_at: Option<DateTime<Utc>>,
    participants: HashMap<Identity, ParticipantState>,
}

#[derive(Debug, Clone, Copy)]
struct GenerationEntry {
    last_generation: u64,
    last_cleared_at: Option<DateTime<Utc>>,
}

impl GenerationEntry {
    fn new(last_generation: u64) -> Self {
        Self {
            last_generation,
            last_cleared_at: None,
        }
    }

    fn next_generation(&mut self, floor: u64) -> CallGeneration {
        self.last_generation = self.last_generation.max(floor) + 1;
        self.last_cleared_at = None;
        CallGeneration::new(self.last_generation)
    }

    fn current_generation(&self) -> Option<CallGeneration> {
        (self.last_generation > 0).then(|| CallGeneration::new(self.last_generation))
    }

    fn mark_cleared(&mut self, generation: CallGeneration, cleared_at: DateTime<Utc>) {
        self.last_generation = self.last_generation.max(generation.as_u64());
        self.last_cleared_at = Some(cleared_at);
    }

    fn tombstone_expired(&self, cutoff: DateTime<Utc>) -> bool {
        self.last_cleared_at
            .is_some_and(|cleared_at| cleared_at <= cutoff)
    }
}

/// Shared registry of in-call participants. Held in an `Arc` so the
/// spawned admin teardown future can re-check membership before
/// firing `DeleteRoom`, closing the race where a fresh joiner
/// re-creates the call between local-clear and the remote evict.
type CallRegistry = Arc<DashMap<CallId, CallEntry>>;

/// Key identifying one participant within one call across the
/// per-participant side tables.
type ParticipantKey = (CallId, Identity);

/// Per-participant serialization for grant pushes. Shared with the
/// spawned admin tasks so only one `UpdateParticipant` per participant
/// is ever in flight.
type GrantLocks = Arc<DashMap<ParticipantKey, Arc<tokio::sync::Mutex<()>>>>;

/// Drop a `(call, identity)` grant lock once this task is the last
/// holder, so the map cannot grow without bound.
///
/// The count to compare against is 2, not 1: the map holds one `Arc`
/// and the calling task holds its own clone. Callers MUST have dropped
/// their `MutexGuard` first — the guard borrows the `Arc`, so a live
/// guard means a live reference.
fn reap_grant_lock(locks: &GrantLocks, key: &ParticipantKey) {
    locks.remove_if(key, |_, held| Arc::strong_count(held) == 2);
}

/// Result of [`LiveKitSfu::clear_local_state`].
#[derive(Debug, Clone)]
struct ClearOutcome {
    /// `identity` was actually registered against the call.
    was_present: bool,
    /// The call entry was removed because it was empty at the moment
    /// of the atomic conditional removal — i.e. we (and nobody
    /// concurrent) hold no participant for it any more. Only this
    /// flag may gate a `DeleteRoom` (#1129).
    emptied: bool,
    generation: CallGeneration,
    room_sid: Option<RoomSid>,
    participant_sid: Option<ParticipantSid>,
    /// The signaling-session binding the removed registration carried
    /// (#1608). Travels with the scheduled remote teardown so a rejoin
    /// that re-registers the same identity under a NEW session before
    /// the spawned/durable removal executes is refused by the
    /// executor's rebind check instead of being ejected.
    removed_session: Option<SessionBinding>,
    removed_occupant_session: Option<OccupancySessionGeneration>,
    /// The caller's unbound-registration policy, carried into the durable
    /// removal so a confirmed departure keeps its authority (#1703).
    unbound_occupant: crate::UnboundOccupantPolicy,
    /// Participants still registered after the clear.
    remaining: usize,
}

/// The fence evidence a scheduled remote teardown carries (#1608):
/// everything the executor may check before the destructive admin
/// call. Bundled so the schedule call sites stay reviewable.
struct RemoteTeardownEvidence {
    generation: Option<CallGeneration>,
    room_sid: Option<RoomSid>,
    participant_sid: Option<ParticipantSid>,
    /// The signaling-session binding the removed registration carried:
    /// a rejoin that re-registers the same identity under a NEW
    /// session before the spawned/durable removal executes is refused
    /// by the executor's rebind check instead of being ejected.
    session: Option<SessionBinding>,
    occupant_session: Option<OccupancySessionGeneration>,
    unbound_occupant: crate::UnboundOccupantPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidGuardDisposition {
    Applied { participant_rejoined: bool },
    RoomRotationPending,
    StaleSid,
}

#[derive(Debug, Clone)]
enum ClearDisposition {
    Cleared(ClearOutcome),
    NoCall,
    StaleSid,
    /// The stored session binding rejected the presented signaling
    /// session identifier; nothing was mutated (#1608).
    SessionMismatch,
}

/// Registration precondition for [`LiveKitSfu::clear_local_state`]
/// (#1608/#1703). Checked under the same call-entry guard as the
/// removal, so gate and clear are one atomic step.
#[derive(Clone, Copy)]
enum SessionGate<'a> {
    /// No session constraint: webhook-driven clears, reconciliation
    /// sweeps, and plain unregisters, whose authority is membership
    /// (the participant verifiably left) rather than a signaling
    /// stanza that could be a stale replay.
    Any,
    /// Clear only when the stored binding accepts the presented
    /// identifier: an unbound registration accepts anything, a bound
    /// registration requires equality. `Presented(None)` models a sid
    /// that could not become a binding — it can never match a bound
    /// session.
    Presented(Option<&'a SessionBinding>),
    /// Clear only when the stored occupant generation is exactly
    /// `Some(presented)`; an UNBOUND registration is cleared only when the
    /// caller's `unbound` policy says its evidence allows it (#1703).
    Occupant {
        presented: OccupancySessionGeneration,
        unbound: crate::UnboundOccupantPolicy,
        /// The #1608 sid rule, applied alongside the generation rule.
        sid: crate::SidEvidence<'a>,
    },
}

pub type TeardownFailureSink =
    Arc<dyn Fn(CallTeardownIntentLite) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Clone)]
struct TeardownReporter {
    runtime: Option<Handle>,
    sink: Option<TeardownFailureSink>,
    state: Arc<Mutex<TeardownReporterState>>,
}

#[derive(Default)]
struct TeardownReporterState {
    running: bool,
    pending: HashSet<CallTeardownIntentLite>,
}

impl TeardownReporter {
    fn report(&self, intents: impl IntoIterator<Item = CallTeardownIntentLite>) {
        let Some(sink) = &self.sink else {
            return;
        };
        let intents = intents.into_iter().collect::<Vec<_>>();
        let Some(runtime) = self.runtime.as_ref() else {
            for intent in intents {
                drop(sink(intent));
            }
            return;
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending.extend(intents);
            if state.running {
                return;
            }
            state.running = true;
        }
        let state = Arc::clone(&self.state);
        let sink = Arc::clone(sink);
        runtime.spawn(async move {
            loop {
                let batch = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.pending.drain().collect::<Vec<_>>()
                };
                for intent in batch {
                    sink(intent).await;
                }
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.pending.is_empty() {
                    state.running = false;
                    break;
                }
            }
        });
    }
}

/// Result of an idempotent LiveKit teardown attempt. `StaleGeneration` and
/// `Occupied` are successful no-ops and must not be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownExecution {
    Executed,
    StaleGeneration,
    Occupied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeardownGuard {
    Proceed,
    Stale,
    Unresolved,
}

/// Cloneable admin executor shared by the inline path and durable outbox
/// drainers. It owns no persistence policy.
#[derive(Clone)]
pub struct LiveKitTeardownExecutor {
    admin: Arc<dyn LiveKitAdmin>,
    calls: CallRegistry,
    reconcile_pass_completed: Arc<AtomicBool>,
    allow_missing_entry_before_reconcile: bool,
}

impl LiveKitTeardownExecutor {
    pub fn current_generation(&self, call_id: &CallId) -> Option<CallGeneration> {
        self.calls.get(call_id).map(|entry| entry.generation)
    }

    async fn remove_participant(
        &self,
        intent: &CallTeardownIntentLite,
        identity: &Identity,
        participant_sid: Option<&ParticipantSid>,
    ) -> Result<TeardownExecution, SfuError> {
        let call_id = &intent.call_id;
        let generation = intent.generation;
        let room_sid = intent.room_sid.as_ref();
        let entry_missing = self.calls.get(call_id).is_none();
        // A participant-sid fence is locally decidable only when the
        // registry tracks this identity in this call. A missing entry
        // (restart / non-hosting replica) AND an existing entry that no
        // longer tracks the identity (partial registry, identity rejoined
        // elsewhere with a newer sid) both leave the fence unresolved —
        // resolve it against LiveKit's live occupancy instead of
        // discarding it (#1612 review rounds 8-9).
        let participant_fence_unresolved_locally = self
            .calls
            .get(call_id)
            .is_none_or(|entry| !entry.participants.contains_key(identity));
        match self.guard(
            call_id,
            generation,
            room_sid,
            Some((identity, participant_sid)),
            intent.occupant_session.is_some(),
        ) {
            TeardownGuard::Proceed => {}
            TeardownGuard::Stale => return Ok(TeardownExecution::StaleGeneration),
            TeardownGuard::Unresolved => return Ok(TeardownExecution::Occupied),
        }
        if let Some(fence) = participant_sid {
            if participant_fence_unresolved_locally {
                let occupancy = self.admin.room_occupancy(call_id).await?;
                match occupancy
                    .waddle
                    .iter()
                    .find(|(occupant, _)| occupant == identity)
                {
                    // Already gone: the removal this intent wanted has
                    // happened; success without touching the room.
                    None => return Ok(TeardownExecution::Executed),
                    Some((_, Some(live))) if live != fence => {
                        return Ok(TeardownExecution::StaleGeneration)
                    }
                    Some(_) => {}
                }
            }
        } else if entry_missing {
            if let Some(fence) = room_sid {
                if let Some(live) = self.live_room_sid(call_id).await? {
                    if &live != fence {
                        return Ok(TeardownExecution::StaleGeneration);
                    }
                }
            }
        }
        // #1608: a session-bearing removal must not eject a
        // registration rebound to a NEWER session. Checked HERE — after
        // every await above (guard resolution, occupancy probes) — so
        // only the identity-keyed admin request itself remains outside
        // the check; that residue is irreducible for a remote effect
        // and converges via the participant_joined re-assertion and
        // reconciliation paths. A registration with no binding — or
        // none at all — proves nothing and does not block.
        if let Some(intent_session) = intent.session.as_ref() {
            let rebound = self.calls.get(call_id).is_some_and(|entry| {
                entry.participants.get(identity).is_some_and(|state| {
                    state
                        .session
                        .as_ref()
                        .is_some_and(|bound| bound != intent_session)
                })
            });
            if rebound {
                return Ok(TeardownExecution::StaleGeneration);
            }
        }
        // Unlike the sid check above this one FAILS CLOSED on an unbound
        // registration: a durable occupant intent is old by construction
        // (minted at the departure, executed after a retry or a restart),
        // so a registration restored without a generation in the meantime
        // may be the same user's live replacement whose media never dropped.
        // Only an entry bound to EXACTLY the intent's generation — or no
        // entry at all — lets the admin removal proceed; restored
        // registrations converge through the reconcile backstop.
        if let Some(intent_occupant_session) = intent.occupant_session.as_ref() {
            // `Some(Some(_))` bound, `Some(None)` unbound, `None` the call is
            // tracked but this identity is not.
            let tracked = self.calls.get(call_id).and_then(|entry| {
                entry
                    .participants
                    .get(identity)
                    .map(|state| state.occupant_session)
            });
            match tracked {
                Some(Some(bound)) if bound == *intent_occupant_session => {}
                // Restored without a generation: only an intent minted by a
                // CONFIRMED departure may remove it (#1703).
                Some(None) if intent.unbound_occupant == crate::UnboundOccupantPolicy::TearDown => {
                }
                Some(_) => return Ok(TeardownExecution::StaleGeneration),
                // A PARTIAL entry (another participant restored by a webhook
                // while this identity's live replacement is known only to
                // LiveKit) is as undecidable as a missing one: defer until
                // the reconcile pass has merged the live identities
                // (`merge_live_identities`), which then either binds nothing
                // (removal proceeds) or restores an unbound entry (refused).
                None if !entry_missing
                    && !self.reconcile_pass_completed.load(Ordering::Acquire)
                    && !self.allow_missing_entry_before_reconcile =>
                {
                    return Ok(TeardownExecution::Occupied);
                }
                None => {}
            }
        }
        self.admin.remove_participant(call_id, identity).await?;
        Ok(TeardownExecution::Executed)
    }

    /// LiveKit's current sid for this room name, if the room exists.
    async fn live_room_sid(&self, call_id: &CallId) -> Result<Option<RoomSid>, SfuError> {
        Ok(self
            .admin
            .list_rooms()
            .await?
            .into_iter()
            .find(|room| room.name.as_waddle() == Some(call_id))
            .and_then(|room| room.sid))
    }

    pub async fn delete_room_if_empty(
        &self,
        call_id: &CallId,
        departing: Option<&Identity>,
        generation: Option<CallGeneration>,
        room_sid: Option<&RoomSid>,
    ) -> Result<TeardownExecution, SfuError> {
        let entry_missing = self.calls.get(call_id).is_none();
        match self.guard(call_id, generation, room_sid, None, false) {
            TeardownGuard::Proceed => {}
            TeardownGuard::Stale => return Ok(TeardownExecution::StaleGeneration),
            TeardownGuard::Unresolved => return Ok(TeardownExecution::Occupied),
        }
        // Same missing-entry rule as `remove_participant`: a sid fence
        // that cannot be decided locally is resolved against LiveKit
        // before the destructive delete — an empty NEWER room (its
        // joiners still connecting) must not be deleted by a stale
        // intent from the previous incarnation (#1612 review).
        if entry_missing {
            if let Some(fence) = room_sid {
                if let Some(live) = self.live_room_sid(call_id).await? {
                    if &live != fence {
                        return Ok(TeardownExecution::StaleGeneration);
                    }
                }
            }
        }
        if self.calls.get(call_id).is_some() {
            return Ok(TeardownExecution::Occupied);
        }
        let occupancy = self.admin.room_occupancy(call_id).await?;
        let empty = match departing {
            Some(departing) => occupancy.is_empty_except(departing),
            None => occupancy.foreign == 0 && occupancy.waddle.is_empty(),
        };
        if !empty || self.calls.get(call_id).is_some() {
            return Ok(TeardownExecution::Occupied);
        }
        self.admin.delete_room(call_id).await?;
        Ok(TeardownExecution::Executed)
    }

    /// Execute a durable intent. Room intents use strict emptiness because
    /// they intentionally carry no participant identity; the inline path
    /// calls [`Self::delete_room_if_empty`] with its departing identity so a
    /// just-removed participant echoed by LiveKit does not block cleanup.
    pub async fn execute(
        &self,
        intent: &CallTeardownIntentLite,
    ) -> Result<TeardownExecution, SfuError> {
        match &intent.target {
            TeardownTargetLite::Participant {
                identity,
                participant_sid,
            } => {
                self.remove_participant(intent, identity, participant_sid.as_ref())
                    .await
            }
            TeardownTargetLite::Room => {
                self.delete_room_if_empty(
                    &intent.call_id,
                    None,
                    intent.generation,
                    intent.room_sid.as_ref(),
                )
                .await
            }
        }
    }

    fn guard(
        &self,
        call_id: &CallId,
        generation: Option<CallGeneration>,
        room_sid: Option<&RoomSid>,
        participant: Option<(&Identity, Option<&ParticipantSid>)>,
        occupant_fence: bool,
    ) -> TeardownGuard {
        let Some(entry) = self.calls.get(call_id) else {
            // An occupant-generation fence (#1703) is as undecidable without
            // a local entry as the sid fences: the intent waits for the
            // reconcile pass to restore the registry (a same-FullJID
            // replacement that re-registered in the meantime then fails the
            // occupant comparison) instead of ejecting by identity.
            let carries_fence = generation.is_some()
                || room_sid.is_some()
                || participant.is_some_and(|(_, participant_sid)| participant_sid.is_some())
                || occupant_fence;
            if carries_fence
                && !self.allow_missing_entry_before_reconcile
                && !self.reconcile_pass_completed.load(Ordering::Acquire)
            {
                return TeardownGuard::Unresolved;
            }
            return TeardownGuard::Proceed;
        };
        if generation.is_some_and(|generation| entry.generation > generation) {
            return TeardownGuard::Stale;
        }
        if let Some(observed) = room_sid {
            match entry.room_sid.as_ref() {
                Some(live) if live != observed => return TeardownGuard::Stale,
                Some(_) => {}
                None => return TeardownGuard::Unresolved,
            }
        }
        if let Some((identity, Some(observed))) = participant {
            if let Some(state) = entry.participants.get(identity) {
                match state.participant_sid.as_ref() {
                    Some(live) if live != observed => return TeardownGuard::Stale,
                    Some(_) => {}
                    None => return TeardownGuard::Unresolved,
                }
            }
        }
        TeardownGuard::Proceed
    }
}

pub struct LiveKitSfu {
    config: SfuConfig,
    calls: CallRegistry,
    call_generations: Arc<DashMap<CallId, GenerationEntry>>,
    /// Live JWT identifiers per `(call, identity)`, each carrying
    /// its `exp` so revocation entries can be swept once the token
    /// would have lapsed anyway. Capped at
    /// [`MAX_ISSUED_PER_PARTICIPANT`] entries per key — the oldest
    /// is evicted FIFO when a fresh token is minted past the cap so
    /// a misbehaving client cannot push the tracker into unbounded
    /// memory growth.
    issued: DashMap<(CallId, Identity), Vec<IssuedJti>>,
    /// Wall-clock instant each `(call, identity)` was last registered.
    /// Read only by the reconciliation backstop to enforce
    /// [`RECONCILE_GRACE_SECONDS`]: a participant absent from LiveKit's
    /// `ListParticipants` is only swept once it has been registered
    /// longer than the grace window, so a still-connecting joiner is
    /// never mistaken for a ghost. This is deliberately refreshed on
    /// every sighting/re-registration; durable teardown supersession
    /// instead reads `ParticipantState::first_registered_at`, which
    /// advances only on absent->present transitions. Kept in lockstep
    /// with `calls`: written in `register_call_participant`, removed
    /// in `clear_local_state`.
    registered_at: DashMap<(CallId, Identity), DateTime<Utc>>,
    /// Wall-clock instant the SFU last minted a join token for the
    /// current `(call, identity)` registration. Used by higher layers
    /// that need to distinguish a locally-minted rejoin from a
    /// participant that was merely observed after reconnecting through
    /// another node or an older still-valid token.
    last_minted_at: DashMap<(CallId, Identity), DateTime<Utc>>,
    /// Consecutive reconciliation passes each `(call, identity)` has
    /// been observed absent from LiveKit's `ListParticipants` (#1127).
    /// A participant is only swept once the streak reaches
    /// [`RECONCILE_ABSENT_PASSES`]; the streak resets when the
    /// participant is observed connected, when the pass for its call
    /// fails (absence unconfirmed), and on (re-)registration. Entries
    /// are removed in `clear_local_state` so the map cannot outgrow
    /// the registry.
    absent_streak: DashMap<(CallId, Identity), u32>,
    /// Map of revoked JWT identifiers to the `exp` of the token they
    /// belonged to. Entries are swept lazily once `Utc::now() > exp`:
    /// a revoked token past its expiry cannot be replayed regardless
    /// of whether the SFU still remembers its jti, so keeping it in
    /// the map after that point is pure overhead. LiveKit still does
    /// not consult this map on join, so every revocation path that
    /// should actively eject a live holder must ALSO schedule a
    /// guarded admin-side convergence (`RemoveParticipant` for
    /// revoke-to-nothing, `UpdateParticipant` for downgrades).
    revoked: DashMap<Jti, DateTime<Utc>>,
    /// Participant identities that must be ejected if they become
    /// observable again before their revoked token would have expired.
    /// This closes the late-join hole where `RemoveParticipant` runs
    /// before the holder of a now-revoked token actually connects:
    /// `participant_joined` / SID observation / reconcile adoption
    /// will re-arm the guarded eject until either a fresh authorized
    /// issuance clears this key or the revoked token lapses.
    pending_revocation_ejects: DashMap<ParticipantKey, DateTime<Utc>>,
    /// Latest media grants that SHOULD be in effect per
    /// `(call, identity)`, written before a push task is spawned and
    /// consumed by whichever task wins that key's lock. Last writer
    /// wins, so a superseded push never reaches LiveKit.
    desired_grants: Arc<DashMap<ParticipantKey, MediaCapabilities>>,
    /// Per-`(call, identity)` lock serializing grant pushes, so two
    /// concurrent `UpdateParticipant` requests for one participant
    /// cannot race on HTTP timing (see
    /// [`Self::schedule_permission_update`]). Entries are reaped by the
    /// last task to release them ([`reap_grant_lock`]) — deliberately
    /// NOT in `clear_local_state`, so a key's serialization survives an
    /// unregister/rejoin while a push is still in flight.
    grant_locks: GrantLocks,
    /// LiveKit admin REST client. Used to evict participants and
    /// delete empty rooms when a teardown signal arrives over XMPP —
    /// without this, LiveKit only notices a hangup when the underlying
    /// PeerConnection's DTLS read deadline expires, leaving "dtls
    /// timeout" warnings in `livekit-sfu` logs and stale rooms
    /// lingering up to `empty_timeout` (default 5 min).
    admin: Arc<dyn LiveKitAdmin>,
    /// Captured at construction time. Remote evict happens as a
    /// `tokio::spawn` here so the synchronous `SfuService` surface
    /// can stay sync and the IQ-result on session-terminate never
    /// blocks on the SFU's admin endpoint. `None` when the SFU was
    /// constructed outside a Tokio runtime (only the local-bookkeeping
    /// unit tests do this) — in that case the remote leg is a no-op,
    /// matching pre-LK-admin behaviour for those tests.
    runtime: Option<Handle>,
    /// Bounds the number of concurrent admin REST calls in flight so
    /// a teardown burst can't fan out into thousands of reqwest tasks
    /// — see [`ADMIN_CONCURRENCY`] for the cap.
    admin_permits: Arc<Semaphore>,
    teardown_reporter: TeardownReporter,
    reconcile_pass_completed: Arc<AtomicBool>,
}

impl std::fmt::Debug for LiveKitSfu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitSfu")
            .field("config", &self.config)
            .field("calls_len", &self.calls.len())
            .field("issued_len", &self.issued.len())
            .field("revoked_len", &self.revoked.len())
            .field("runtime_attached", &self.runtime.is_some())
            .finish()
    }
}

impl LiveKitSfu {
    fn next_call_generation(&self, call_id: &CallId, floor: u64) -> CallGeneration {
        self.call_generations
            .entry(call_id.clone())
            .or_insert_with(|| GenerationEntry::new(floor))
            .next_generation(floor)
    }

    fn mark_call_cleared(
        &self,
        call_id: &CallId,
        generation: CallGeneration,
        cleared_at: DateTime<Utc>,
    ) {
        self.call_generations
            .entry(call_id.clone())
            .or_insert_with(|| GenerationEntry::new(generation.as_u64()))
            .mark_cleared(generation, cleared_at);
    }

    fn prune_generation_tombstones(&self, now: DateTime<Utc>) {
        let cutoff = now - ChronoDuration::hours(GENERATION_TOMBSTONE_TTL_HOURS);
        // Never touch `calls` while holding a `call_generations` shard:
        // registration/adoption/rotation hold a `calls` guard and then
        // enter `next_call_generation` (which locks `call_generations`),
        // so nesting the maps here in the opposite order is an AB/BA
        // deadlock (#1612 review round 9). Snapshot candidates first,
        // consult `calls` lock-free of `call_generations`, and let
        // `remove_if` re-verify expiry under the shard lock.
        let expired: Vec<CallId> = self
            .call_generations
            .iter()
            .filter(|entry| entry.value().tombstone_expired(cutoff))
            .map(|entry| entry.key().clone())
            .collect();
        for call_id in expired {
            if self.calls.contains_key(&call_id) {
                continue;
            }
            self.call_generations
                .remove_if(&call_id, |_, entry| entry.tombstone_expired(cutoff));
        }
    }

    fn rotate_room_incarnation_from_listing(
        &self,
        call_id: &CallId,
        listed_room_sid: &RoomSid,
        listing_started_at: DateTime<Utc>,
    ) {
        let Some(mut entry) = self.calls.get_mut(call_id) else {
            return;
        };
        let Some(current_room_sid) = entry.room_sid.as_ref() else {
            // A sid-less entry created after this listing was requested
            // is a fresh same-name rejoin; the listing may be a snapshot
            // of the previous (already-cleared) incarnation, and stamping
            // its sid here would let that incarnation's delayed leave
            // webhooks pass the fence against the new registration.
            if entry.created_at > listing_started_at {
                tracing::info!(
                    call_id = %call_id,
                    listed_room_sid = %listed_room_sid,
                    "SFU reconcile: not learning a listing sid onto an entry created after the listing request"
                );
                return;
            }
            entry.room_sid = Some(listed_room_sid.clone());
            entry.room_sid_observed_at = Some(listing_started_at);
            return;
        };
        if current_room_sid == listed_room_sid {
            return;
        }
        // The stored sid was observed AFTER this listing was requested:
        // the listing is the stale snapshot (an old incarnation listed
        // just before a webhook restored the newer room). Rotating here
        // would roll the fence back and let a delayed `room_finished`
        // for the old room clear the current call.
        if entry
            .room_sid_observed_at
            .is_some_and(|observed_at| observed_at > listing_started_at)
        {
            tracing::info!(
                call_id = %call_id,
                listed_room_sid = %listed_room_sid,
                stored_room_sid = %current_room_sid,
                "SFU reconcile: ignoring stale room listing older than the stored sid observation"
            );
            return;
        }
        let next_generation = self.next_call_generation(call_id, entry.generation.as_u64());
        tracing::info!(
            call_id = %call_id,
            old_room_sid = %current_room_sid,
            new_room_sid = %listed_room_sid,
            old_generation = %entry.generation,
            new_generation = %next_generation,
            "SFU reconcile: LiveKit room sid rotated; reincarnating the local call entry"
        );
        entry.generation = next_generation;
        entry.room_sid = Some(listed_room_sid.clone());
        entry.room_sid_observed_at = Some(listing_started_at);
        for participant in entry.participants.values_mut() {
            participant.participant_sid = None;
        }
    }

    fn adopt_discovered_call(
        &self,
        call_id: &CallId,
        room_sid: Option<RoomSid>,
        participants: &[(Identity, Option<ParticipantSid>)],
        now: DateTime<Utc>,
    ) -> bool {
        if participants.is_empty() {
            return false;
        }
        let dashmap::Entry::Vacant(entry) = self.calls.entry(call_id.clone()) else {
            return false;
        };
        let generation = self.next_call_generation(call_id, 0);
        let participant_states: HashMap<Identity, ParticipantState> = participants
            .iter()
            .cloned()
            .map(|(identity, participant_sid)| {
                (identity, ParticipantState::restored(now, participant_sid))
            })
            .collect();
        entry.insert(CallEntry {
            generation,
            created_at: now,
            room_sid_observed_at: room_sid.as_ref().map(|_| now),
            room_sid,
            participants: participant_states,
        });
        for (identity, _participant_sid) in participants {
            self.registered_at
                .insert((call_id.clone(), identity.clone()), now);
            self.absent_streak
                .remove(&(call_id.clone(), identity.clone()));
            self.schedule_pending_revocation_eject_if_needed(call_id, identity);
        }
        true
    }

    /// Merge LiveKit-reported live identities into an EXISTING call
    /// entry that is missing them — the partial-restart case where one
    /// participant was re-registered by a webhook before the reconcile
    /// pass ran, so the vacant-entry adoption path never fires and the
    /// remaining connected identities would otherwise stay invisible
    /// (unknown-session terminates, missed survivor sweeps). Merged
    /// identities are restored (no mint) and stamped `now`, exactly
    /// like adoption. Returns how many were merged (#1449 codex
    /// round 3).
    fn merge_live_identities(
        &self,
        call_id: &CallId,
        live: &[(Identity, Option<ParticipantSid>)],
        now: DateTime<Utc>,
        probe_freshness_boundary: DateTime<Utc>,
    ) -> usize {
        let mut merged = 0;
        {
            let Some(mut entry) = self.calls.get_mut(call_id) else {
                return 0;
            };
            for (identity, participant_sid) in live {
                if let Some(participant) = entry.participants.get_mut(identity) {
                    if participant_sid.is_some()
                        && participant.participant_sid.is_none()
                        // First-SID learning is freshness-gated too
                        // (#1612 review round 11): a sid-less state
                        // REGISTERED AFTER the probe went out is a fresh
                        // incarnation, and the probe's sid belongs to
                        // the previous one — learning it would let a
                        // delayed old `participant_left` match the fence
                        // and clear the replacement.
                        && participant.first_registered_at <= probe_freshness_boundary
                    {
                        participant.participant_sid.clone_from(participant_sid);
                        participant.participant_sid_observed_at = Some(now);
                        participant.participant_sid_contested_at = None;
                    } else if participant_sid.is_some()
                        // Only a stored sid may be ADVANCED here; a sid-less
                        // state is exclusively the first-fill branch above,
                        // whose registration-time gate must not be bypassed
                        // via `None != Some`.
                        && participant.participant_sid.is_some()
                        && participant.participant_sid.as_ref() != participant_sid.as_ref()
                        && participant
                            .participant_sid_observed_at
                            .is_none_or(|observed_at| observed_at < probe_freshness_boundary)
                    {
                        // The authoritative occupancy reports a DIFFERENT
                        // sid than the fence we hold, and no webhook
                        // observation newer than this pass contradicts it:
                        // the participant reconnected within the same room
                        // incarnation and the join delivery was lost. Keep
                        // the fence current so a delayed old leave or
                        // teardown job cannot match the stale sid and
                        // clear the live participant (#1612 review
                        // round 9).
                        tracing::info!(
                            call_id = %call_id,
                            identity = %identity.as_livekit_identity(),
                            "SFU reconcile: advancing a stale participant sid from authoritative occupancy"
                        );
                        participant.participant_sid.clone_from(participant_sid);
                        participant.participant_sid_observed_at = Some(now);
                        participant.participant_sid_contested_at = None;
                    } else if participant_sid.is_some()
                        && participant.participant_sid.as_ref() == participant_sid.as_ref()
                        && participant
                            .participant_sid_contested_at
                            .is_some_and(|contested_at| contested_at < probe_freshness_boundary)
                    {
                        // The authoritative occupancy CONFIRMS the
                        // stored fence, and the probe went out after
                        // the ambiguity arose, so the contest is
                        // resolved in the stored sid's favor. A probe
                        // predating the contest proves nothing about
                        // which same-second twin survived (#1612
                        // review round 14).
                        participant.participant_sid_contested_at = None;
                    }
                } else {
                    entry.participants.insert(
                        identity.clone(),
                        ParticipantState::restored(now, participant_sid.clone()),
                    );
                    merged += 1;
                }
            }
        }
        // Re-arm pending revocation ejects for EVERY reconciled live
        // identity, not only newly inserted ones (#1612 review round
        // 14): when the pre-connect admin removal completed as
        // not-found and the join webhook was lost, reconciliation only
        // fills or advances the ALREADY-TRACKED identity's sid — the
        // merged-count gate would skip the eject backstop and leave
        // the revoked holder connected.
        for (identity, _participant_sid) in live {
            let key = (call_id.clone(), identity.clone());
            self.registered_at.entry(key.clone()).or_insert(now);
            self.absent_streak.remove(&key);
            self.schedule_pending_revocation_eject_if_needed(call_id, identity);
        }
        merged
    }

    /// Build a [`LiveKitSfu`] backed by the production reqwest admin
    /// client. Captures the current Tokio runtime handle if one is
    /// active so the teardown hot path can fire `RemoveParticipant` +
    /// `DeleteRoom` admin calls without making the [`SfuService`]
    /// surface async.
    pub fn new(config: SfuConfig) -> Result<Self, SfuError> {
        let admin_base = admin_base_url_from_ws(&config.ws_url)?;
        let admin = ReqwestLiveKitAdmin::new(
            admin_base,
            config.api_key.clone(),
            config.api_secret.clone(),
        )?;
        Ok(Self::with_admin(config, Arc::new(admin)))
    }

    /// Build the production admin client with a typed failure observer.
    pub fn new_with_observer(
        config: SfuConfig,
        observer: Arc<dyn AdminCallObserver>,
    ) -> Result<Self, SfuError> {
        let admin_base = admin_base_url_from_ws(&config.ws_url)?;
        let admin = ReqwestLiveKitAdmin::new_with_observer(
            admin_base,
            config.api_key.clone(),
            config.api_secret.clone(),
            observer,
        )?;
        Ok(Self::with_admin(config, Arc::new(admin)))
    }

    /// Build with a caller-supplied admin client. Used by tests to
    /// inject a recording mock without standing up an HTTP server;
    /// production code goes through [`Self::new`] which constructs a
    /// real [`crate::admin::ReqwestLiveKitAdmin`] backed by reqwest.
    pub fn with_admin(config: SfuConfig, admin: Arc<dyn LiveKitAdmin>) -> Self {
        let runtime = Handle::try_current().ok();
        Self {
            config,
            calls: Arc::new(DashMap::new()),
            call_generations: Arc::new(DashMap::new()),
            issued: DashMap::new(),
            registered_at: DashMap::new(),
            last_minted_at: DashMap::new(),
            absent_streak: DashMap::new(),
            revoked: DashMap::new(),
            pending_revocation_ejects: DashMap::new(),
            desired_grants: Arc::new(DashMap::new()),
            grant_locks: Arc::new(DashMap::new()),
            admin,
            runtime: runtime.clone(),
            admin_permits: Arc::new(Semaphore::new(ADMIN_CONCURRENCY)),
            teardown_reporter: TeardownReporter {
                runtime,
                sink: None,
                state: Arc::new(Mutex::new(TeardownReporterState::default())),
            },
            reconcile_pass_completed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_teardown_failure_sink(mut self, sink: TeardownFailureSink) -> Self {
        self.teardown_reporter.sink = Some(sink);
        self
    }

    pub fn teardown_executor(&self) -> LiveKitTeardownExecutor {
        LiveKitTeardownExecutor {
            admin: Arc::clone(&self.admin),
            calls: Arc::clone(&self.calls),
            reconcile_pass_completed: Arc::clone(&self.reconcile_pass_completed),
            allow_missing_entry_before_reconcile: false,
        }
    }

    fn inline_teardown_executor(&self) -> LiveKitTeardownExecutor {
        LiveKitTeardownExecutor {
            admin: Arc::clone(&self.admin),
            calls: Arc::clone(&self.calls),
            reconcile_pass_completed: Arc::clone(&self.reconcile_pass_completed),
            allow_missing_entry_before_reconcile: true,
        }
    }

    pub fn config(&self) -> &SfuConfig {
        &self.config
    }

    /// Number of distinct identities currently registered against
    /// `call_id`. Exposed for test inspection; production code calls
    /// [`Self::unregister_call_participant`] which already returns a
    /// [`CallState`] derived from this.
    pub fn participant_count(&self, call_id: &CallId) -> usize {
        self.calls
            .get(call_id)
            .map(|entry| entry.participants.len())
            .unwrap_or(0)
    }

    /// Number of currently-tracked revoked JTIs. Exposed for tests
    /// to pin the bound on the revocation map.
    #[cfg(test)]
    pub(crate) fn revoked_count(&self) -> usize {
        self.revoked.len()
    }

    /// Number of currently-tracked issued JTIs for `(call, identity)`.
    /// Exposed for test inspection (this crate's FIFO-bound tests and
    /// the `waddle-xmpp` XEP-0166 suite's one-JTI-per-stanza pin,
    /// #1142); production code never branches on it. `#[doc(hidden)]`
    /// signals it is not a stable public surface (it must be `pub`, not
    /// `pub(crate)`, only because the consumer lives in another crate's
    /// integration tests).
    #[doc(hidden)]
    pub fn issued_count(&self, call_id: &CallId, identity: &Identity) -> usize {
        self.issued
            .get(&(call_id.clone(), identity.clone()))
            .map(|e| e.len())
            .unwrap_or(0)
    }

    /// Drop every revocation entry whose original token would have
    /// expired at or before `now`. Called from
    /// [`SfuService::unregister_call_participant`] so the map stays
    /// bounded under steady call churn.
    fn sweep_expired_revoked(&self, now: DateTime<Utc>) {
        self.revoked.retain(|_, exp| *exp > now);
        self.pending_revocation_ejects.retain(|_, exp| *exp > now);
    }

    fn arm_pending_revocation_eject(
        &self,
        call_id: &CallId,
        identity: &Identity,
        expires_at: DateTime<Utc>,
    ) {
        self.pending_revocation_ejects
            .insert((call_id.clone(), identity.clone()), expires_at);
    }

    fn has_pending_revocation_eject(&self, call_id: &CallId, identity: &Identity) -> bool {
        self.pending_revocation_ejects
            .get(&(call_id.clone(), identity.clone()))
            .is_some_and(|entry| *entry.value() > Utc::now())
    }

    fn clear_pending_revocation_eject(&self, call_id: &CallId, identity: &Identity) {
        self.pending_revocation_ejects
            .remove(&(call_id.clone(), identity.clone()));
    }

    /// A revoked JTI is only local state until LiveKit is told to act
    /// on it. Reuse the same participant-eviction path as a full
    /// unregister so the Lane D sink/outbox picks up runtime absence,
    /// saturation, or transient admin failures.
    fn schedule_revocation_eject(&self, call_id: &CallId, identity: &Identity) {
        let (generation, room_sid, participant_sid) = if let Some(entry) = self.calls.get(call_id) {
            (
                Some(entry.generation),
                entry.room_sid.clone(),
                entry
                    .participants
                    .get(identity)
                    .and_then(|state| state.participant_sid.clone()),
            )
        } else {
            (
                self.call_generations
                    .get(call_id)
                    .and_then(|generation| generation.current_generation()),
                None,
                None,
            )
        };
        self.schedule_remote_teardown(
            call_id.clone(),
            identity.clone(),
            false,
            RemoteTeardownEvidence {
                generation,
                room_sid,
                participant_sid,
                occupant_session: None,
                unbound_occupant: crate::UnboundOccupantPolicy::Keep,
                // Revocation authority is token state, not a signaling
                // terminate: rejoin protection here is the
                // registration's own clear_pending_revocation_eject,
                // so no session evidence is attached.
                session: None,
            },
        );
    }

    fn schedule_pending_revocation_eject_if_needed(&self, call_id: &CallId, identity: &Identity) {
        if self.has_pending_revocation_eject(call_id, identity) {
            self.schedule_revocation_eject(call_id, identity);
        }
    }

    fn guard_and_learn_observed_sids(
        call_id: &CallId,
        identity: &Identity,
        entry: &mut CallEntry,
        observed_sids: Option<&ObservedCallSids>,
        direction: SidObservationDirection,
        observed_at: DateTime<Utc>,
    ) -> SidGuardDisposition {
        let Some(observed_sids) = observed_sids else {
            return SidGuardDisposition::Applied {
                participant_rejoined: false,
            };
        };
        let identity_is_tracked = entry.participants.contains_key(identity);
        let mut participant_rejoined = false;

        // Validate the room SID before mutating the entry. A join from a
        // different room incarnation is retryable: the authoritative room
        // listing must rotate the stored fence before a redelivery may
        // advance participant state. Leaves remain strict stale no-ops.
        if let Some(room_sid) = observed_sids.room_sid.as_ref() {
            if let Some(stored_room_sid) = entry.room_sid.as_ref() {
                if stored_room_sid != room_sid {
                    return match direction {
                        SidObservationDirection::Join => {
                            tracing::info!(
                                call_id = %call_id,
                                identity = %identity.as_livekit_identity(),
                                room_sid = %room_sid,
                                stored_room_sid = %stored_room_sid,
                                "LiveKit join deferred until room sid rotation is reconciled"
                            );
                            SidGuardDisposition::RoomRotationPending
                        }
                        SidObservationDirection::Leave => {
                            tracing::warn!(
                                call_id = %call_id,
                                identity = %identity.as_livekit_identity(),
                                room_sid = %room_sid,
                                stored_room_sid = %stored_room_sid,
                                "LiveKit event ignored as stale: room sid mismatch"
                            );
                            SidGuardDisposition::StaleSid
                        }
                    };
                }
            }
        }

        if let Some(participant_sid) = observed_sids.participant_sid.as_ref() {
            if let Some(state) = entry.participants.get_mut(identity) {
                if let Some(stored_participant_sid) = state.participant_sid.as_ref() {
                    if stored_participant_sid != participant_sid {
                        match direction {
                            SidObservationDirection::Join => {
                                // A join may only ADVANCE the sid when its
                                // event time postdates the stored sid's
                                // event lineage: the delivery ledger
                                // deliberately re-executes `processing`
                                // events, so a stale redelivered join for
                                // the PREVIOUS incarnation can arrive after
                                // the reconnect's join was processed —
                                // rolling the fence backward would let the
                                // old leave clear the live participant
                                // (#1612 review round 12). Unknown event
                                // times keep the prior advance semantics.
                                if let (Some(event_at), Some(stored_event_at)) = (
                                    observed_sids.observed_event_at,
                                    state.participant_sid_event_at,
                                ) {
                                    if event_at < stored_event_at {
                                        tracing::warn!(
                                            call_id = %call_id,
                                            identity = %identity.as_livekit_identity(),
                                            participant_sid = %participant_sid,
                                            stored_participant_sid = %stored_participant_sid,
                                            "LiveKit join ignored as stale: event time predates \
                                             the stored sid's lineage"
                                        );
                                        return SidGuardDisposition::StaleSid;
                                    }
                                    // `createdAt` is whole seconds, so a
                                    // legitimate same-second reconnect and a
                                    // stale redelivery are indistinguishable
                                    // at equality. Neither overwrite nor
                                    // stale-ack: keep the stored fence,
                                    // mark the lineage CONTESTED so a
                                    // delayed leave matching the retained
                                    // fence cannot clear the live
                                    // reconnect, and let the occupancy
                                    // reconcile resolve which sid is live
                                    // (#1612 review rounds 13-14).
                                    if event_at == stored_event_at {
                                        tracing::info!(
                                            call_id = %call_id,
                                            identity = %identity.as_livekit_identity(),
                                            participant_sid = %participant_sid,
                                            stored_participant_sid = %stored_participant_sid,
                                            "LiveKit join with equal event time is unordered; \
                                             deferring sid resolution to reconciliation"
                                        );
                                        state.participant_sid_contested_at = Some(observed_at);
                                        return SidGuardDisposition::Applied {
                                            participant_rejoined: false,
                                        };
                                    }
                                }
                                tracing::info!(
                                    call_id = %call_id,
                                    identity = %identity.as_livekit_identity(),
                                    participant_sid = %participant_sid,
                                    stored_participant_sid = %stored_participant_sid,
                                    "LiveKit join advanced the participant sid for a new participant incarnation"
                                );
                                state.participant_sid = Some(participant_sid.clone());
                                state.participant_sid_observed_at = Some(observed_at);
                                state.participant_sid_event_at = observed_sids.observed_event_at;
                                state.participant_sid_contested_at = None;
                                state.first_registered_at = observed_at;
                                state.registered_without_mint = true;
                                participant_rejoined = true;
                            }
                            SidObservationDirection::Leave => {
                                tracing::warn!(
                                    call_id = %call_id,
                                    identity = %identity.as_livekit_identity(),
                                    participant_sid = %participant_sid,
                                    stored_participant_sid = %stored_participant_sid,
                                    "LiveKit event ignored as stale: participant sid mismatch"
                                );
                                return SidGuardDisposition::StaleSid;
                            }
                        }
                    } else if matches!(direction, SidObservationDirection::Leave)
                        && state.participant_sid_contested_at.is_some()
                    {
                        // The stored fence matches, but its lineage is
                        // contested by an unordered same-second join:
                        // this leave may belong to the departed twin
                        // while the OTHER sid is the live reconnect.
                        // Defer the destructive clear; the occupancy
                        // reconcile either advances the fence (making
                        // a redelivered leave mismatch) or removes a
                        // genuinely absent participant via the absent
                        // streak (#1612 review round 14).
                        tracing::warn!(
                            call_id = %call_id,
                            identity = %identity.as_livekit_identity(),
                            participant_sid = %participant_sid,
                            "LiveKit leave deferred: participant sid lineage is contested \
                             by an unordered same-second join"
                        );
                        return SidGuardDisposition::StaleSid;
                    }
                }
            }
        }

        // First-SID learning is restricted to Join-direction
        // observations (a `participant_joined` webhook or the
        // authoritative occupancy listing). A Leave-direction event can
        // be a delayed echo of the PREVIOUS same-name room arriving
        // before the new room's join: learning its sids here would
        // poison the fresh entry as if the old incarnation were
        // current, let the destructive teardown proceed against the new
        // call, and make the real join un-repairable because it then
        // conflicts with the poisoned fence (#1612 review).
        if matches!(direction, SidObservationDirection::Join) {
            if identity_is_tracked && entry.room_sid.is_none() {
                entry.room_sid.clone_from(&observed_sids.room_sid);
                if observed_sids.room_sid.is_some() {
                    entry.room_sid_observed_at = Some(observed_at);
                }
            }
            if let Some(state) = entry.participants.get_mut(identity) {
                if state.participant_sid.is_none() {
                    state
                        .participant_sid
                        .clone_from(&observed_sids.participant_sid);
                    if observed_sids.participant_sid.is_some() {
                        state.participant_sid_observed_at = Some(observed_at);
                        state.participant_sid_event_at = observed_sids.observed_event_at;
                    }
                }
            }
        }

        SidGuardDisposition::Applied {
            participant_rejoined,
        }
    }

    /// Drop `identity` from the in-memory registry and revoke every
    /// JWT it ever held. Returns the [`ClearOutcome`] the caller uses
    /// to distinguish "this participant just left the last seat"
    /// (warrants a `DeleteRoom` admin call) from "we never knew about
    /// this participant at all" and from "others remain".
    ///
    /// #1129: the call entry is dropped via an atomic
    /// `remove_if(|_, set| set.is_empty())` rather than an
    /// unconditional `remove`. Between releasing the per-entry guard
    /// (after removing `identity`) and the entry removal, a concurrent
    /// joiner may have registered into the same call; the conditional
    /// remove observes that registration under the shard lock and
    /// keeps the entry, so the joiner is neither evicted from the
    /// registry nor exposed to the spawned `DeleteRoom` (the caller
    /// derives its emptiness decision from `emptied`, which is `false`
    /// in that case).
    /// Shared body of `register_call_participant` /
    /// `register_call_participant_with_session`. The registration and
    /// its session binding are written under ONE call-entry guard so a
    /// concurrent session-scoped teardown can never observe the new
    /// registration carrying the previous session's binding or a
    /// half-updated one (#1608). A registration without a binding
    /// (`session = None`) — the 1:1 paths and non-signaling callers —
    /// clears any previous binding for the same reason: it belongs to
    /// a NEW session the registry knows no identifier for.
    fn register_participant_with_binding(
        &self,
        call_id: &CallId,
        identity: &Identity,
        session: Option<&SessionBinding>,
        occupant_session: Option<OccupancySessionGeneration>,
    ) {
        let now = Utc::now();
        match self.calls.entry(call_id.clone()) {
            dashmap::Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                if entry.participants.is_empty() {
                    entry.generation =
                        self.next_call_generation(call_id, entry.generation.as_u64());
                    entry.room_sid = None;
                    // A reused temporarily-empty entry IS a new
                    // incarnation: refresh the freshness stamps too, or
                    // an in-flight `ListRooms` snapshot of the previous
                    // room would pass the `created_at` gate and stamp
                    // the old sid onto this fresh registration (#1612
                    // review round 11).
                    entry.created_at = now;
                    entry.room_sid_observed_at = None;
                }
                entry
                    .participants
                    .entry(identity.clone())
                    .and_modify(|participant| {
                        participant.session = session.cloned();
                        participant.occupant_session = occupant_session;
                    })
                    .or_insert_with(|| ParticipantState {
                        session: session.cloned(),
                        occupant_session,
                        ..ParticipantState::new(now)
                    });
            }
            dashmap::Entry::Vacant(entry) => {
                let generation = self.next_call_generation(call_id, 0);
                let mut participants = HashMap::new();
                participants.insert(
                    identity.clone(),
                    ParticipantState {
                        session: session.cloned(),
                        occupant_session,
                        ..ParticipantState::new(now)
                    },
                );
                entry.insert(CallEntry {
                    generation,
                    created_at: now,
                    room_sid: None,
                    room_sid_observed_at: None,
                    participants,
                });
            }
        }
        // This registration is only reachable through the authorized
        // Jingle gate (the webhook path uses
        // `register_call_participant_observed`), and production mints
        // the join token BEFORE registering. A delayed old departure
        // sweeping between that mint and this register can revoke the
        // fresh token and arm an eject the mint-time clear already
        // missed — so the authorized registration clears it again
        // (#1612 review round 9).
        self.clear_pending_revocation_eject(call_id, identity);
        // Stamp (or refresh) the registration time so the
        // reconciliation backstop's grace window is measured from the
        // most recent (re)join, not a stale earlier attempt. A
        // (re-)registration also resets the absence streak (#1127):
        // any prior not-seen observations belong to the previous
        // connection attempt.
        self.registered_at
            .insert((call_id.clone(), identity.clone()), now);
        self.absent_streak
            .remove(&(call_id.clone(), identity.clone()));
    }

    /// Session-gated local-only cleanup shared by the trait's plain
    /// and session-scoped `note_participant_left` entry points
    /// (#1608). LiveKit's `participant_left` webhook is the SFU
    /// acknowledging it already removed the participant — usually
    /// because we asked it to. Doing only the local cleanup avoids a
    /// feedback loop where the webhook fires another
    /// `RemoveParticipant` against an already-removed participant.
    fn note_participant_left_gated(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        session_gate: SessionGate<'_>,
    ) -> SessionScopedTeardown {
        let clear = match self.clear_local_state(call_id, identity, observed_sids, session_gate) {
            ClearDisposition::Cleared(clear) => clear,
            ClearDisposition::SessionMismatch => return SessionScopedTeardown::SessionMismatch,
            ClearDisposition::StaleSid => {
                return SessionScopedTeardown::Applied(TeardownDisposition::StaleSid);
            }
            ClearDisposition::NoCall => {
                return SessionScopedTeardown::Applied(TeardownDisposition::Applied(
                    CallState::Active { remaining: 0 },
                ));
            }
        };

        let state = if clear.was_present && clear.emptied {
            CallState::Ended
        } else {
            CallState::Active {
                remaining: clear.remaining,
            }
        };
        SessionScopedTeardown::Applied(TeardownDisposition::Applied(state))
    }

    /// Session-gated unregister shared by the trait's plain and
    /// session-scoped teardown entry points (#1608). On
    /// `SessionMismatch` NOTHING has been mutated and no SFU-side
    /// eviction is scheduled.
    fn unregister_participant_gated(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        session_gate: SessionGate<'_>,
    ) -> SessionScopedTeardown {
        let clear = self.clear_local_state(call_id, identity, observed_sids, session_gate);
        let (
            was_present,
            emptied,
            generation,
            room_sid,
            participant_sid,
            removed_session,
            removed_occupant_session,
            unbound_occupant,
            remaining,
        ) = match clear {
            ClearDisposition::Cleared(ClearOutcome {
                was_present,
                emptied,
                generation,
                room_sid,
                participant_sid,
                removed_session,
                removed_occupant_session,
                unbound_occupant,
                remaining,
            }) => (
                was_present,
                emptied,
                Some(generation),
                room_sid,
                participant_sid,
                removed_session,
                removed_occupant_session,
                unbound_occupant,
                remaining,
            ),
            ClearDisposition::SessionMismatch => return SessionScopedTeardown::SessionMismatch,
            ClearDisposition::StaleSid => {
                return SessionScopedTeardown::Applied(TeardownDisposition::StaleSid);
            }
            ClearDisposition::NoCall => {
                return SessionScopedTeardown::Applied(TeardownDisposition::Applied(
                    CallState::Active { remaining: 0 },
                ));
            }
        };

        let state = if was_present && emptied {
            CallState::Ended
        } else {
            // `Active { remaining }` covers two cases that look the
            // same to the caller (don't broadcast "call ended"): the
            // normal "other participants are still here" path, and
            // the defensive "this participant was never registered"
            // path where `remaining == 0` does not imply we just
            // emptied a known-active call. Treating an unknown
            // identity as `Ended` would broadcast a phantom
            // call-ended signal to the MUC.
            CallState::Active { remaining }
        };

        // Schedule the LiveKit-side evict. `RemoveParticipant` always
        // runs (LiveKit may know about the participant even when our
        // local registry has lost track — federation, stale state).
        // `DeleteRoom` only fires when the atomic conditional removal
        // confirmed we just emptied a call we previously tracked
        // (#1129 — a concurrent joiner keeps `emptied == false`), and
        // the spawn re-checks the registry inside the future to close
        // the rejoin race.
        let we_just_emptied = was_present && emptied;
        self.schedule_remote_teardown(
            call_id.clone(),
            identity.clone(),
            we_just_emptied,
            RemoteTeardownEvidence {
                generation,
                room_sid,
                participant_sid,
                session: removed_session,
                occupant_session: removed_occupant_session,
                unbound_occupant,
            },
        );

        SessionScopedTeardown::Applied(TeardownDisposition::Applied(state))
    }

    fn clear_local_state(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        session_gate: SessionGate<'_>,
    ) -> ClearDisposition {
        let now = Utc::now();
        let mut entry = match self.calls.get_mut(call_id) {
            Some(entry) => entry,
            None => return ClearDisposition::NoCall,
        };
        if matches!(
            Self::guard_and_learn_observed_sids(
                call_id,
                identity,
                entry.value_mut(),
                observed_sids,
                SidObservationDirection::Leave,
                now,
            ),
            SidGuardDisposition::StaleSid
        ) {
            return ClearDisposition::StaleSid;
        }
        // #1608: the session gate and the removal below share this
        // call-entry guard, so a concurrent initiate rebinding the
        // registration either lands before this check (and rejects the
        // stale clear) or after the whole clear — never in between.
        match session_gate {
            SessionGate::Any => {}
            SessionGate::Presented(presented) => {
                if let Some(bound) = entry
                    .participants
                    .get(identity)
                    .and_then(|participant| participant.session.as_ref())
                {
                    if presented != Some(bound) {
                        return ClearDisposition::SessionMismatch;
                    }
                }
            }
            SessionGate::Occupant {
                presented,
                unbound,
                sid,
            } => {
                let participant = entry.participants.get(identity);
                match participant.and_then(|participant| participant.occupant_session) {
                    Some(bound) if bound == presented => {}
                    None if unbound == crate::UnboundOccupantPolicy::TearDown => {}
                    _ => return ClearDisposition::SessionMismatch,
                }
                if !sid.accepts(participant.and_then(|participant| participant.session.as_ref())) {
                    return ClearDisposition::SessionMismatch;
                }
            }
        }

        let participant_sid = entry
            .participants
            .get(identity)
            .and_then(|participant| participant.participant_sid.clone());
        let removed_session = entry
            .participants
            .get(identity)
            .and_then(|participant| participant.session.clone());
        let removed_occupant_session = entry
            .participants
            .get(identity)
            .and_then(|participant| participant.occupant_session);
        let unbound_occupant = match session_gate {
            SessionGate::Occupant { unbound, .. } => unbound,
            SessionGate::Any | SessionGate::Presented(_) => crate::UnboundOccupantPolicy::Keep,
        };
        let room_sid = entry.room_sid.clone();
        let was_present = entry.participants.remove(identity).is_some();
        let generation = entry.generation;
        drop(entry);

        // Revoke only tokens minted BEFORE this departure was observed
        // (#1612 review round 8): between dropping the entry guard and
        // this sweep, a concurrent Jingle initiate can mint a replacement
        // token for the same (call, identity). That fresh JTI belongs to
        // the NEW incarnation — capturing it here would revoke it and arm
        // a pending eject that later disconnects the newly authorized
        // caller once their `participant_joined` arrives.
        let mut replacement_minted = false;
        if let Some((_, issued)) = self.issued.remove(&(call_id.clone(), identity.clone())) {
            // Strict `<`: a mint in the SAME clock tick as the departure
            // observation is ambiguous, and the safe reading is "fresh" —
            // wrongly revoking a replacement disconnects an authorized
            // caller, while wrongly sparing a departed token only leaves
            // a JWT that lapses on its own `exp` (#1612 review round 9).
            let (departed_tokens, fresh_tokens): (Vec<IssuedJti>, Vec<IssuedJti>) =
                issued.into_iter().partition(|token| token.minted_at < now);
            let mut latest_exp = None;
            for issued in departed_tokens {
                latest_exp = Some(
                    latest_exp.map_or(issued.exp, |current: DateTime<Utc>| current.max(issued.exp)),
                );
                self.revoked.insert(issued.jti, issued.exp);
            }
            if !fresh_tokens.is_empty() {
                replacement_minted = true;
                // Merge-preserve: an even newer mint may already have
                // repopulated the bucket after the `remove` above.
                self.issued
                    .entry((call_id.clone(), identity.clone()))
                    .or_default()
                    .extend(fresh_tokens);
            }
            // A surviving replacement token proves a concurrent
            // re-authorization: arming the eject would disconnect it.
            // The live-bucket recheck and the arm are ATOMIC under the
            // `issued` entry guard (#1612 review rounds 12-13): the
            // mint path holds this same guard across its eject clear
            // and token push, so under the guard either the fresh token
            // is already visible (we skip the arm) or the racing mint
            // has not run yet — in which case its own eject clear, and
            // the registration's clear after it, erase the arm we make
            // here. No check/act gap remains.
            if let (Some(latest_exp), false) = (latest_exp, replacement_minted) {
                let key = (call_id.clone(), identity.clone());
                let bucket = self.issued.entry(key.clone()).or_default();
                if bucket.is_empty() {
                    self.arm_pending_revocation_eject(call_id, identity, latest_exp);
                }
                drop(bucket);
                self.issued.remove_if(&key, |_, bucket| bucket.is_empty());
            }
        }
        self.registered_at
            .remove(&(call_id.clone(), identity.clone()));
        // The replacement mint refreshed `last_minted_at`; clearing it
        // would blind the durable-teardown supersession fence to the
        // rejoin it exists to detect.
        if !replacement_minted {
            self.last_minted_at
                .remove(&(call_id.clone(), identity.clone()));
        }
        self.absent_streak
            .remove(&(call_id.clone(), identity.clone()));
        // Dropping a queued grant intent here is correct, and load-bearing
        // to reason about: every caller of `clear_local_state` means the
        // participant is gone or is being removed, so a pending downgrade
        // has nothing left to apply.
        //   - `unregister_call_participant` also schedules
        //     `RemoveParticipant`, which strictly supersedes any grant
        //     change; and if that admin call fails, the participant is by
        //     then no longer an occupant, so the voice-reconciliation
        //     backstop evicts them.
        //   - `note_participant_left` runs because LiveKit told us they
        //     already left.
        //   - the reconciliation sweep only clears participants LiveKit
        //     has confirmed absent across consecutive passes.
        // A new caller that does NOT imply departure must not reuse this
        // path, or it would silently discard a downgrade for someone still
        // publishing.
        self.desired_grants
            .remove(&(call_id.clone(), identity.clone()));
        // `grant_locks` is deliberately NOT cleared here: a push task
        // may still hold this key's mutex, and dropping the entry would
        // let a post-rejoin push create a fresh mutex and run
        // concurrently with it — the exact interleaving the per-key
        // lock exists to prevent. The lock is reaped by the last task
        // to release it (see `schedule_permission_update`).
        self.sweep_expired_revoked(Utc::now());

        // Atomic conditional removal: only drop the call entry if it
        // is *still* empty at removal time (see doc comment above).
        let emptied = self
            .calls
            .remove_if(call_id, |_, entry| entry.participants.is_empty())
            .is_some();
        let remaining = if emptied {
            0
        } else {
            self.calls
                .get(call_id)
                .map(|entry| entry.participants.len())
                .unwrap_or(0)
        };
        if emptied {
            self.mark_call_cleared(call_id, generation, now);
        }

        ClearDisposition::Cleared(ClearOutcome {
            was_present,
            emptied,
            generation,
            room_sid,
            participant_sid,
            removed_session,
            removed_occupant_session,
            unbound_occupant,
            remaining,
        })
    }

    /// Fire-and-forget the LiveKit admin REST calls that mirror a
    /// local unregister. `RemoveParticipant` always runs because
    /// LiveKit may know about the participant even when our local
    /// registry has lost track of them (stale state, alternate
    /// federation entry). `DeleteRoom` only fires when we *know* we
    /// just emptied a call — gated on `was_present && remaining == 0`
    /// at the call site — and even then re-checks `calls` inside the
    /// spawn to close the rejoin race: another participant may have
    /// registered in the same tick, in which case kicking the room
    /// would evict them too — and then confirmed against LiveKit's
    /// own participant list, because local emptiness says nothing
    /// about participants registered on another replica (#1445); see
    /// [`LiveKitTeardownExecutor::delete_room_if_empty`]. Spawn target is the
    /// runtime handle captured at construction. When none is attached, when
    /// the admin semaphore is saturated, or when an admitted admin call
    /// fails, the corresponding typed effect is handed to
    /// `teardown_failure_sink` for durable retry. The availability gate
    /// bounds both in-flight calls and spawned tasks during a teardown burst.
    ///
    /// The participant intent and, for the last participant, the room intent
    /// are reported before spawning the inline admin future. That closes the
    /// local-clear-before-spawn crash window for both effects. The residual
    /// gap is the sink's own async enqueue; fully persisting before the side
    /// effect would require an async `SfuService` surface.
    fn schedule_remote_teardown(
        &self,
        call_id: CallId,
        identity: Identity,
        we_just_emptied: bool,
        evidence: RemoteTeardownEvidence,
    ) {
        let RemoteTeardownEvidence {
            generation,
            room_sid,
            participant_sid,
            session,
            occupant_session,
            unbound_occupant,
        } = evidence;
        let participant_intent = CallTeardownIntentLite {
            call_id: call_id.clone(),
            target: TeardownTargetLite::Participant {
                identity: identity.clone(),
                participant_sid,
            },
            generation,
            room_sid: room_sid.clone(),
            occupant_session,
            unbound_occupant,
            session,
        };
        let room_intent = we_just_emptied.then(|| CallTeardownIntentLite {
            call_id: call_id.clone(),
            target: TeardownTargetLite::Room,
            generation,
            room_sid,
            occupant_session: None,
            unbound_occupant: crate::UnboundOccupantPolicy::Keep,
            session: None,
        });
        let report_all = || {
            self.teardown_reporter
                .report(std::iter::once(participant_intent.clone()).chain(room_intent.clone()));
        };
        let Some(runtime) = self.runtime.as_ref() else {
            // Invoking the sink still reports the typed effects to non-async
            // embedders. Production construction always captures a runtime;
            // without one there is no executor on which an async persistence
            // implementation could make progress.
            report_all();
            return;
        };
        // Do not create a future that can wait indefinitely behind the
        // semaphore. Saturation hands the effects directly to the durable
        // retry sink, bounding the number of spawned teardown tasks.
        let permit = match Arc::clone(&self.admin_permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                report_all();
                return;
            }
        };
        self.teardown_reporter
            .report(std::iter::once(participant_intent.clone()).chain(room_intent.clone()));
        let executor = self.inline_teardown_executor();
        runtime.spawn(async move {
            let _permit = permit;
            let _ = executor.execute(&participant_intent).await;
            if we_just_emptied {
                let _ = executor
                    .delete_room_if_empty(
                        &call_id,
                        Some(&identity),
                        generation,
                        room_intent
                            .as_ref()
                            .and_then(|intent| intent.room_sid.as_ref()),
                    )
                    .await;
            }
        });
    }

    /// Move every outstanding JTI for `(call_id, identity)` into the
    /// revocation map WITHOUT touching the participant registry. Used
    /// on a mid-call grant downgrade: the participant stays in the
    /// call (listen-only), and their pre-downgrade tokens are marked
    /// spent.
    ///
    /// LiveKit ignores the revoked map on join, so the active
    /// enforcement here is the paired `UpdateParticipant` push, not
    /// the map write itself. A later join with a stale token is
    /// converged by re-asserting the latest grants on
    /// `participant_joined`.
    fn revoke_issued_tokens(&self, call_id: &CallId, identity: &Identity) {
        if let Some((_, issued)) = self.issued.remove(&(call_id.clone(), identity.clone())) {
            for issued in issued {
                self.revoked.insert(issued.jti, issued.exp);
            }
        }
        self.sweep_expired_revoked(Utc::now());
    }

    /// Fire-and-forget the LiveKit admin `UpdateParticipant` call that
    /// pushes replacement grants to a live participant. Same spawn +
    /// permit shape as [`Self::schedule_remote_teardown`]; when no
    /// runtime handle is attached (plain `#[test]` fixtures) the
    /// remote leg silently drops.
    ///
    /// Pushes for the same `(call, identity)` are strictly serialized
    /// through a per-key lock, and each task pushes whatever the
    /// *latest* desired grants are when it acquires that lock rather
    /// than the value it was spawned with. Two grant changes for the
    /// same participant (a batch that revokes then grants voice, two
    /// moderation IQs in a row, a role change racing a config flip)
    /// therefore cannot land on LiveKit out of order and leave publish
    /// enabled after a revoke — the exact state this feature exists to
    /// prevent. A superseded task finds the desired entry already
    /// consumed and exits without a round-trip.
    ///
    /// Serializing on a dedicated per-key lock rather than a
    /// generation counter is deliberate: a counter can only decide
    /// whether to *start* a request, so two in-flight requests still
    /// race on HTTP timing.
    fn schedule_permission_update(
        &self,
        call_id: CallId,
        identity: Identity,
        capabilities: MediaCapabilities,
    ) {
        let key = (call_id.clone(), identity.clone());
        // Publish the intent before spawning so a task that wins the
        // lock always observes the newest value, even one written after
        // it was spawned.
        self.desired_grants.insert(key.clone(), capabilities);
        let lock = self
            .grant_locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let Some(runtime) = self.runtime.as_ref() else {
            // No runtime (plain `#[test]` fixtures): the remote leg
            // drops, so don't leak the intent.
            self.desired_grants.remove(&key);
            self.grant_locks.remove(&key);
            return;
        };
        let admin = Arc::clone(&self.admin);
        let permits = Arc::clone(&self.admin_permits);
        let desired = Arc::clone(&self.desired_grants);
        let locks = Arc::clone(&self.grant_locks);
        runtime.spawn(async move {
            let serialized = lock.lock().await;
            // Acquire the admin slot BEFORE claiming the intent. Waiting
            // on an exhausted semaphore can take arbitrarily long, and a
            // value claimed beforehand would go stale in our hand: a
            // pending downgrade would be recorded behind our lock while
            // we still held (and would then send) the older
            // publish-enabling grant, briefly restoring publishing for
            // an occupant who has already been devoiced.
            let Ok(mut permit) = Arc::clone(&permits).acquire_owned().await else {
                // Only reachable if `admin_permits` is closed, which
                // nothing does today; reap anyway so no path leaks.
                drop(serialized);
                reap_grant_lock(&locks, &key);
                return;
            };
            // Claim the latest intent. Absent => a task that ran before
            // us already applied it (or the participant was cleared),
            // so there is nothing left to converge.
            let Some((_, mut capabilities)) = desired.remove(&key) else {
                drop(serialized);
                reap_grant_lock(&locks, &key);
                return;
            };
            // A downgrade is a security-relevant convergence, so a
            // transient transport/5xx failure gets bounded retries
            // rather than a single best effort. Widening grants is not
            // retried: failing to restore publish rights is a
            // functional annoyance the next voice change or rejoin
            // fixes, and retrying it could race a fresh downgrade.
            let attempts = if capabilities.is_listen_only() {
                GRANT_DOWNGRADE_ATTEMPTS
            } else {
                1
            };
            for attempt in 1..=attempts {
                match admin
                    .update_participant(&call_id, &identity, capabilities)
                    .await
                {
                    Ok(()) => break,
                    Err(err) => {
                        tracing::warn!(
                            call_id = %call_id,
                            identity = %identity.as_livekit_identity(),
                            attempt,
                            attempts,
                            error = %err,
                            // This crate sits below the telemetry crate
                            // (`waddle-xmpp` depends on it, not the
                            // reverse), so this WARN is the alert
                            // signal for a participant whose live
                            // grants no longer match their MUC voice.
                            "LiveKit UpdateParticipant failed; live media grants lag the MUC voice"
                        );
                    }
                }
                if attempt == attempts {
                    break;
                }
                // A fresher intent arrived; adopt it rather than
                // retrying a value we already know is stale.
                if let Some((_, fresher)) = desired.remove(&key) {
                    capabilities = fresher;
                }
                // Release the admin slot across the backoff so a
                // retrying downgrade doesn't hold one of the 32 slots
                // idle during a LiveKit incident.
                drop(permit);
                tokio::time::sleep(GRANT_RETRY_BACKOFF * attempt).await;
                // And again after the sleep: the backoff window is the
                // longest gap in which an intent can overtake us.
                if let Some((_, fresher)) = desired.remove(&key) {
                    capabilities = fresher;
                }
                let Ok(next) = Arc::clone(&permits).acquire_owned().await else {
                    drop(serialized);
                    reap_grant_lock(&locks, &key);
                    return;
                };
                permit = next;
            }
            drop(serialized);
            reap_grant_lock(&locks, &key);
        });
    }

    /// One reconciliation pass against LiveKit's ground truth.
    ///
    /// List active LiveKit rooms, union them with the local registry,
    /// ask who is actually connected (`ListParticipants`), adopt
    /// Waddle-owned occupants missing after a process restart, and sweep any locally
    /// registered identity that LiveKit no longer reports — but only
    /// once that identity has been registered longer than `grace`, so
    /// a participant still ringing/connecting (the registry is
    /// populated at `session-initiate`, before the WebSocket connects)
    /// is never mistaken for a ghost. The returned summary carries the
    /// `(call, identity)` pairs swept so the caller can clear their MUC
    /// Muji presence via the same idempotent path the
    /// `participant_left` webhook uses, plus pass-level counts for
    /// telemetry emitted by `waddle-server`.
    ///
    /// Swept entries are cleared with [`Self::clear_local_state`]
    /// (registry removal + JWT revocation) only — no admin
    /// `RemoveParticipant`/`DeleteRoom` is fired, because LiveKit
    /// already does not have these participants (that is precisely why
    /// they are ghosts), and a room LiveKit reports as gone will lapse
    /// on its own `empty_timeout`.
    ///
    /// A `ListParticipants` failure for a given call (network/5xx) is
    /// logged and that call is skipped this pass — absence cannot be
    /// confirmed, so nothing is swept and the call's absence streaks
    /// are reset. The next pass retries.
    ///
    /// #1127: a single absent observation never sweeps. Each pass a
    /// (grace-aged) participant is absent from `ListParticipants`
    /// bumps its absence streak; the sweep fires only once the streak
    /// reaches [`RECONCILE_ABSENT_PASSES`] — i.e. the participant was
    /// gone across two consecutive passes a full reconcile interval
    /// apart. A LiveKit pod restart therefore cannot mass-terminate
    /// live calls: the first post-restart pass (rooms not found ⇒
    /// empty participant lists) only marks streaks, clients silently
    /// rejoin, and the second pass observes them connected and clears
    /// the streaks.
    async fn reconcile_active_calls_inner(
        &self,
        grace: ChronoDuration,
    ) -> crate::ReconcilePassSummary {
        let now = Utc::now();
        self.prune_generation_tombstones(now);
        // Snapshot the registry into owned values up front so no
        // DashMap guard is held across the `.await` on the admin call.
        let mut rooms: HashMap<CallId, (Vec<Identity>, Option<RoomSid>, bool)> = self
            .calls
            .iter()
            .map(|entry| {
                (
                    entry.key().clone(),
                    (
                        entry.value().participants.keys().cloned().collect(),
                        None,
                        true,
                    ),
                )
            })
            .collect();

        let mut listing_authoritative = true;
        let listing_started_at = Utc::now();
        match self.admin.list_rooms().await {
            Ok(listed) => {
                for room in listed {
                    let ListedRoomName::Waddle(call_id) = room.name else {
                        tracing::debug!("SFU reconcile: ignoring non-Waddle LiveKit room name");
                        continue;
                    };
                    if let Some(listed_room_sid) = room.sid.as_ref() {
                        self.rotate_room_incarnation_from_listing(
                            &call_id,
                            listed_room_sid,
                            listing_started_at,
                        );
                    }
                    rooms
                        .entry(call_id)
                        .and_modify(|(_, listed_sid, _)| {
                            if listed_sid.is_none() {
                                listed_sid.clone_from(&room.sid);
                            }
                        })
                        .or_insert_with(|| (Vec::new(), room.sid, false));
                }
            }
            Err(error) => {
                listing_authoritative = false;
                tracing::warn!(
                    error = %error,
                    "SFU reconcile: ListRooms failed; continuing with registry rooms only"
                );
            }
        }

        let rooms_examined = rooms.len() as u64;
        let probes = stream::iter(rooms.into_iter().map(
            |(call_id, (registered, listed_room_sid, was_registered))| async move {
                let occupancy = self.admin.room_occupancy(&call_id).await;
                (
                    call_id,
                    registered,
                    listed_room_sid,
                    was_registered,
                    occupancy,
                )
            },
        ))
        .buffer_unordered(RECONCILE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        let mut swept = Vec::new();
        let mut rooms_adopted = 0_u64;
        let mut occupancy_failures = 0_u64;
        let mut swept_rooms = HashSet::new();
        for (call_id, registered, listed_room_sid, was_registered, occupancy) in probes {
            // Ghost detection reasons only about participants we
            // minted; a foreign participant (recorder, SIP) is by
            // definition not one of our registry entries.
            let live = match occupancy {
                Ok(occupancy) => occupancy.waddle,
                Err(err) => {
                    occupancy_failures += 1;
                    tracing::warn!(
                        call_id = %call_id,
                        error = %err,
                        "SFU reconcile: ListParticipants failed; skipping this call this pass"
                    );
                    // Absence cannot be confirmed this pass, so the
                    // streaks accumulated so far are no longer
                    // "consecutive" — reset them (#1127) rather than
                    // let a failed pass count towards the sweep.
                    for identity in registered {
                        self.absent_streak.remove(&(call_id.clone(), identity));
                    }
                    continue;
                }
            };
            // Sid rotation for registered rooms is handled once per
            // pass by `rotate_room_incarnation_from_listing` (called
            // on the listing before per-room probes), so by this
            // point `entry.room_sid` already matches the listed sid.
            let registered = if was_registered {
                let merged = self.merge_live_identities(&call_id, &live, now, listing_started_at);
                if merged > 0 {
                    tracing::info!(
                        call_id = %call_id,
                        merged,
                        "SFU reconcile: merged live LiveKit identities missing from the \
                         partially restored call entry"
                    );
                }
                registered
            } else {
                // Revalidate the room sid at adoption time (#1612 review
                // round 13): the original room can finish and a same-name
                // successor start between the pass's `ListRooms` and this
                // occupancy probe, and adopting the successor's occupants
                // under the predecessor's sid poisons the fence — a
                // delayed old `room_finished` would then clear the live
                // call. A fresh single listing is acceptable here: this
                // path only runs for registry-vacant (restart) rooms. On
                // listing failure adopt sid-less; later observations
                // teach the current sid under the usual gates.
                let adoption_room_sid = match self.admin.list_rooms().await {
                    Ok(rooms) => rooms
                        .into_iter()
                        .find(|room| room.name.as_waddle() == Some(&call_id))
                        .and_then(|room| room.sid),
                    Err(error) => {
                        tracing::warn!(
                            call_id = %call_id,
                            %error,
                            "SFU reconcile: sid revalidation listing failed; adopting sid-less"
                        );
                        None
                    }
                };
                if adoption_room_sid != listed_room_sid {
                    tracing::info!(
                        call_id = %call_id,
                        "SFU reconcile: room sid changed between listing and occupancy probe; \
                         adopting with the revalidated sid"
                    );
                }
                if self.adopt_discovered_call(&call_id, adoption_room_sid, &live, now) {
                    rooms_adopted += 1;
                    tracing::info!(
                        call_id = %call_id,
                        participants = live.len(),
                        "SFU reconcile: adopted active LiveKit room missing from local registry"
                    );
                }
                live.iter()
                    .map(|(identity, _participant_sid)| identity.clone())
                    .collect()
            };
            let live_set: HashSet<Identity> = live
                .iter()
                .map(|(identity, _participant_sid)| identity.clone())
                .collect();

            for identity in registered {
                let key = (call_id.clone(), identity.clone());
                if live_set.contains(&identity) {
                    // Genuinely connected — not a ghost. Clear any
                    // absence streak from a transient not-found
                    // observation (e.g. a LiveKit restart).
                    self.absent_streak.remove(&key);
                    continue;
                }
                // Absent from LiveKit. Only count the observation once
                // past the grace window; a freshly-registered
                // participant may simply not have finished connecting.
                let aged_out = self
                    .registered_at
                    .get(&key)
                    .map(|entry| now - *entry.value() >= grace)
                    // No timestamp recorded (e.g. an entry registered
                    // before this field existed) → treat as eligible.
                    .unwrap_or(true);
                if !aged_out {
                    continue;
                }
                // #1127: require the absence to persist across
                // RECONCILE_ABSENT_PASSES consecutive passes before
                // sweeping. Room-not-found (empty list) right after a
                // LiveKit restart is indistinguishable from a real
                // departure on one observation — the second pass
                // disambiguates, because reconnected clients are
                // reported again while genuinely departed ones stay
                // absent.
                let streak = {
                    let mut entry = self.absent_streak.entry(key.clone()).or_insert(0);
                    *entry += 1;
                    *entry
                };
                if streak < RECONCILE_ABSENT_PASSES {
                    tracing::debug!(
                        call_id = %call_id,
                        identity = %identity.as_livekit_identity(),
                        streak,
                        "SFU reconcile: participant absent; awaiting a confirming pass"
                    );
                    continue;
                }
                let outcome = self.clear_local_state(&call_id, &identity, None, SessionGate::Any);
                if let ClearDisposition::Cleared(ClearOutcome {
                    was_present: true, ..
                }) = outcome
                {
                    tracing::info!(
                        call_id = %call_id,
                        identity = %identity.as_livekit_identity(),
                        "SFU reconcile: swept ghost participant LiveKit no longer reports"
                    );
                    swept_rooms.insert(call_id.clone());
                    swept.push((call_id.clone(), identity));
                }
            }
        }
        swept.sort_by(|(left_call, left_identity), (right_call, right_identity)| {
            left_call.as_str().cmp(right_call.as_str()).then_with(|| {
                left_identity
                    .as_livekit_identity()
                    .cmp(&right_identity.as_livekit_identity())
            })
        });
        let summary = crate::ReconcilePassSummary {
            swept,
            rooms_examined,
            rooms_adopted,
            rooms_swept: swept_rooms.len() as u64,
            occupancy_failures,
        };
        // The startup fence (missing-entry arm of the teardown guard)
        // may only open after an AUTHORITATIVE pass: the listing
        // succeeded and every occupancy probe answered. A degraded
        // pass has not obtained the SID/generation inventory the
        // fence exists to wait for (#1449 codex round 3).
        if listing_authoritative && occupancy_failures == 0 {
            self.reconcile_pass_completed.store(true, Ordering::Release);
        }
        summary
    }
}

impl crate::SfuReconciler for LiveKitSfu {
    fn live_participants<'a>(&'a self, call_id: &'a CallId) -> crate::LiveParticipantsFuture<'a> {
        Box::pin(async move {
            match self.admin.room_occupancy(call_id).await {
                Ok(occupancy) => Some(
                    occupancy
                        .waddle
                        .into_iter()
                        .map(|(identity, _participant_sid)| identity)
                        .collect(),
                ),
                Err(error) => {
                    // `None`, never an empty vec: an outage must not be
                    // mistaken for "nobody is connected", which would
                    // silently disable the caller's convergence. WARN
                    // because that convergence is the stale-token
                    // backstop — this crate sits below the telemetry
                    // crate, so the log is the alert signal.
                    tracing::warn!(
                        call_id = %call_id,
                        error = %error,
                        "LiveKit ListParticipants failed; cannot confirm live participants, \
                         so voice-grant convergence is skipped for this call this pass"
                    );
                    None
                }
            }
        })
    }

    fn reconcile_active_calls(&self, grace: ChronoDuration) -> crate::ReconcileFuture<'_> {
        Box::pin(self.reconcile_active_calls_inner(grace))
    }
}

impl SfuService for LiveKitSfu {
    fn issue_join_token(
        &self,
        call_id: &CallId,
        identity: &Identity,
        capabilities: MediaCapabilities,
    ) -> Result<JoinToken, SfuError> {
        let token = mint_join_token(MintInputs {
            api_key: &self.config.api_key,
            api_secret: &self.config.api_secret,
            ws_url: &self.config.ws_url,
            call_id,
            identity,
            capabilities,
            ttl: self.config.token_ttl,
        })?;
        // Track the (jti, exp) pair against `(call, identity)` so a
        // subsequent unregister revokes every JWT this participant
        // ever held for the call. Cap the per-participant vec to
        // bound memory under reconnect storms or a misbehaving
        // client; oldest entries are evicted FIFO and silently
        // forgotten (their tokens will simply lapse on their own
        // `exp`, which the rest of this struct already relies on).
        let mut entry = self
            .issued
            .entry((call_id.clone(), identity.clone()))
            .or_default();
        self.clear_pending_revocation_eject(call_id, identity);
        let minted_at = Utc::now();
        self.last_minted_at
            .insert((call_id.clone(), identity.clone()), minted_at);
        let mut protected_from_empty_bucket_eject = false;
        if let Some(mut call_entry) = self.calls.get_mut(call_id) {
            if let Some(participant) = call_entry.participants.get_mut(identity) {
                protected_from_empty_bucket_eject = participant.registered_without_mint;
                participant.registered_without_mint = false;
            }
        }
        while entry.len() >= MAX_ISSUED_PER_PARTICIPANT {
            entry.remove(0);
        }
        entry.push(IssuedJti {
            jti: token.jti.clone(),
            exp: token.expires_at,
            protected_from_empty_bucket_eject,
            minted_at,
        });
        Ok(token)
    }

    fn issue_turn_credentials(&self, identity: &Identity) -> Result<TurnCredential, SfuError> {
        mint_turn_credential(
            &self.config.turn_shared_secret,
            identity,
            self.config.turn_ttl,
        )
    }

    fn register_call_participant(&self, call_id: &CallId, identity: &Identity) {
        self.register_participant_with_binding(call_id, identity, None, None);
    }

    fn register_call_participant_with_session(
        &self,
        call_id: &CallId,
        identity: &Identity,
        session: &SessionBinding,
        occupant: OccupancySessionGeneration,
    ) {
        self.register_participant_with_binding(call_id, identity, Some(session), Some(occupant));
    }

    fn register_call_participant_observed(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: &ObservedCallSids,
    ) -> SidObservationDisposition {
        let now = Utc::now();
        match self.calls.entry(call_id.clone()) {
            dashmap::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                match Self::guard_and_learn_observed_sids(
                    call_id,
                    identity,
                    entry,
                    Some(observed_sids),
                    SidObservationDirection::Join,
                    now,
                ) {
                    SidGuardDisposition::Applied { .. } => {}
                    SidGuardDisposition::RoomRotationPending => {
                        return SidObservationDisposition::RoomRotationPending;
                    }
                    SidGuardDisposition::StaleSid => {
                        return SidObservationDisposition::StaleSid;
                    }
                }
                if entry.participants.is_empty() {
                    entry.generation =
                        self.next_call_generation(call_id, entry.generation.as_u64());
                    // Observed reuse of a temporarily-empty entry is a
                    // new incarnation exactly like the Jingle-register
                    // arm: refresh the freshness stamps, or an
                    // in-flight listing of the previous room passes the
                    // `created_at` gate and rotates the fresh room sid
                    // backward (#1612 review round 12).
                    entry.created_at = now;
                    entry.room_sid = None;
                    entry.room_sid_observed_at = None;
                }
                entry
                    .participants
                    .entry(identity.clone())
                    .or_insert_with(|| ParticipantState::observed(now, observed_sids));
                if entry.room_sid.is_none() {
                    entry.room_sid.clone_from(&observed_sids.room_sid);
                    if observed_sids.room_sid.is_some() {
                        entry.room_sid_observed_at = Some(now);
                    }
                }
            }
            dashmap::Entry::Vacant(vacant) => {
                let generation = self.next_call_generation(call_id, 0);
                let mut participants = HashMap::new();
                participants.insert(
                    identity.clone(),
                    ParticipantState::observed(now, observed_sids),
                );
                vacant.insert(CallEntry {
                    generation,
                    created_at: now,
                    room_sid_observed_at: observed_sids.room_sid.as_ref().map(|_| now),
                    room_sid: observed_sids.room_sid.clone(),
                    participants,
                });
            }
        }
        self.registered_at
            .insert((call_id.clone(), identity.clone()), now);
        self.absent_streak
            .remove(&(call_id.clone(), identity.clone()));
        self.schedule_pending_revocation_eject_if_needed(call_id, identity);
        SidObservationDisposition::Applied
    }

    fn has_call_participant(&self, call_id: &CallId, identity: &Identity) -> bool {
        self.calls
            .get(call_id)
            .is_some_and(|entry| entry.participants.contains_key(identity))
    }

    fn participant_session_binding(
        &self,
        call_id: &CallId,
        identity: &Identity,
    ) -> Option<SessionBinding> {
        self.calls.get(call_id).and_then(|entry| {
            entry
                .participants
                .get(identity)
                .and_then(|participant| participant.session.clone())
        })
    }

    fn participant_occupant_session(
        &self,
        call_id: &CallId,
        identity: &Identity,
    ) -> Option<OccupancySessionGeneration> {
        self.calls.get(call_id).and_then(|entry| {
            entry
                .participants
                .get(identity)
                .and_then(|participant| participant.occupant_session)
        })
    }

    fn participant_registered_at(
        &self,
        call_id: &CallId,
        identity: &Identity,
    ) -> Option<DateTime<Utc>> {
        if !self.has_call_participant(call_id, identity) {
            return None;
        }
        self.calls.get(call_id).and_then(|entry| {
            entry
                .participants
                .get(identity)
                .map(|participant| participant.first_registered_at)
        })
    }

    fn participant_last_minted_at(
        &self,
        call_id: &CallId,
        identity: &Identity,
    ) -> Option<DateTime<Utc>> {
        if !self.has_call_participant(call_id, identity) {
            return None;
        }
        self.last_minted_at
            .get(&(call_id.clone(), identity.clone()))
            .map(|entry| *entry.value())
    }

    fn revoke_issued_token(&self, call_id: &CallId, identity: &Identity, jti: &Jti) {
        let key = (call_id.clone(), identity.clone());
        let mut revoked_issuance = None;
        if let Some(mut issued) = self.issued.get_mut(&key) {
            if let Some(position) = issued.iter().position(|entry| entry.jti == *jti) {
                revoked_issuance = Some(issued.remove(position));
            }
        }
        // Only a JTI we can prove we minted (still present in the
        // pair's issued window) is recorded. The jti reaches this
        // method from an UNVERIFIED claim inside the bounced stanza,
        // so unconditionally inserting would let crafted undeliverable
        // IQs grow the revocation map without bound. The bounce's
        // fresh mint is always still in the window at bounce time, so
        // the #1444 compensation is unaffected.
        let Some(revoked_issuance) = revoked_issuance else {
            return;
        };
        // The ejection decision must be the ATOMIC removal result:
        // a fresh mint can repopulate the bucket between a plain read
        // and the removal, and a stale pre-read would then eject the
        // newly authorized participant (#1449 codex round 3).
        // `remove_if` re-checks under the shard lock, so a concurrent
        // mint that repopulated the vec both survives and suppresses
        // the ejection.
        let bucket_emptied = self
            .issued
            .remove_if(&key, |_, issuances| issuances.is_empty())
            .is_some();
        self.revoked.insert(jti.clone(), revoked_issuance.exp);
        if bucket_emptied {
            self.last_minted_at.remove(&key);
            if revoked_issuance.protected_from_empty_bucket_eject {
                tracing::warn!(
                    call_id = %call_id,
                    identity = %identity.as_livekit_identity(),
                    "Skipping empty-bucket revocation eject for participant restored without a locally minted token"
                );
                return;
            }
            self.arm_pending_revocation_eject(call_id, identity, revoked_issuance.exp);
            self.schedule_revocation_eject(call_id, identity);
        }
    }

    fn unregister_call_participant(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> TeardownDisposition {
        match self.unregister_participant_gated(call_id, identity, observed_sids, SessionGate::Any)
        {
            SessionScopedTeardown::Applied(disposition) => disposition,
            // Unreachable with `SessionGate::Any`; degrade to the
            // same no-op disposition an unknown identity gets.
            SessionScopedTeardown::SessionMismatch => {
                TeardownDisposition::Applied(CallState::Active { remaining: 0 })
            }
        }
    }

    fn unregister_call_participant_if_session_matches(
        &self,
        call_id: &CallId,
        identity: &Identity,
        presented: Option<&SessionBinding>,
        observed_sids: Option<&ObservedCallSids>,
    ) -> SessionScopedTeardown {
        self.unregister_participant_gated(
            call_id,
            identity,
            observed_sids,
            SessionGate::Presented(presented),
        )
    }

    fn unregister_call_participant_if_occupant_matches(
        &self,
        call_id: &CallId,
        identity: &Identity,
        presented: OccupancySessionGeneration,
        unbound: crate::UnboundOccupantPolicy,
        sid: crate::SidEvidence<'_>,
        observed_sids: Option<&ObservedCallSids>,
    ) -> SessionScopedTeardown {
        self.unregister_participant_gated(
            call_id,
            identity,
            observed_sids,
            SessionGate::Occupant {
                presented,
                unbound,
                sid,
            },
        )
    }

    fn note_participant_left(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> TeardownDisposition {
        match self.note_participant_left_gated(call_id, identity, observed_sids, SessionGate::Any) {
            SessionScopedTeardown::Applied(disposition) => disposition,
            // Unreachable with `SessionGate::Any`; degrade to the
            // same no-op disposition an unknown identity gets.
            SessionScopedTeardown::SessionMismatch => {
                TeardownDisposition::Applied(CallState::Active { remaining: 0 })
            }
        }
    }

    fn note_participant_left_if_session_matches(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        presented: Option<&SessionBinding>,
    ) -> SessionScopedTeardown {
        self.note_participant_left_gated(
            call_id,
            identity,
            observed_sids,
            SessionGate::Presented(presented),
        )
    }

    fn note_participant_left_if_occupant_matches(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        presented: OccupancySessionGeneration,
        unbound: crate::UnboundOccupantPolicy,
        sid: crate::SidEvidence<'_>,
    ) -> SessionScopedTeardown {
        self.note_participant_left_gated(
            call_id,
            identity,
            observed_sids,
            SessionGate::Occupant {
                presented,
                unbound,
                sid,
            },
        )
    }

    fn observe_call_participant_sids(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        direction: SidObservationDirection,
    ) -> SidObservationDisposition {
        let Some(mut entry) = self.calls.get_mut(call_id) else {
            return SidObservationDisposition::Applied;
        };
        if !entry.participants.contains_key(identity) {
            return SidObservationDisposition::Applied;
        }
        let now = Utc::now();
        let disposition = match Self::guard_and_learn_observed_sids(
            call_id,
            identity,
            entry.value_mut(),
            observed_sids,
            direction,
            now,
        ) {
            SidGuardDisposition::Applied {
                participant_rejoined,
            } => {
                if participant_rejoined {
                    self.registered_at
                        .insert((call_id.clone(), identity.clone()), now);
                    self.absent_streak
                        .remove(&(call_id.clone(), identity.clone()));
                }
                SidObservationDisposition::Applied
            }
            SidGuardDisposition::RoomRotationPending => {
                SidObservationDisposition::RoomRotationPending
            }
            SidGuardDisposition::StaleSid => SidObservationDisposition::StaleSid,
        };
        drop(entry);
        if matches!(disposition, SidObservationDisposition::Applied) {
            self.schedule_pending_revocation_eject_if_needed(call_id, identity);
        }
        disposition
    }

    fn update_participant_capabilities(
        &self,
        call_id: &CallId,
        identity: &Identity,
        capabilities: MediaCapabilities,
    ) {
        // Deliberately NOT gated on local registration — see the
        // trait doc. LiveKit may hold a participant our per-process
        // registry lost (reconnect after `participant_left`, actor
        // migration, reconcile sweep); skipping the push for them
        // would let a de-voiced occupant keep publishing. A
        // participant LiveKit doesn't know resolves to Twirp
        // `not_found`, which the admin client maps to success.
        if capabilities.is_listen_only() {
            self.revoke_issued_tokens(call_id, identity);
        }
        self.schedule_permission_update(call_id.clone(), identity.clone(), capabilities);
    }

    fn is_revoked(&self, jti: &Jti) -> bool {
        // Lazy sweep on the read path: an entry past its `exp` is
        // by definition unusable and so reads as not-revoked. Drop
        // it from the map so memory doesn't grow on every check.
        if let Some(entry) = self.revoked.get(jti) {
            let exp = *entry.value();
            drop(entry);
            if Utc::now() >= exp {
                self.revoked.remove(jti);
                return false;
            }
            return true;
        }
        false
    }

    fn ws_url(&self) -> &WebsocketUrl {
        &self.config.ws_url
    }

    fn turn_host(&self) -> &TurnHost {
        &self.config.turn_host
    }

    fn webhook_secret(&self) -> &crate::config::ApiSecret {
        &self.config.webhook_secret
    }

    fn participants_for_call(&self, call_id: &CallId) -> Vec<Identity> {
        self.calls
            .get(call_id)
            .map(|entry| entry.participants.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
