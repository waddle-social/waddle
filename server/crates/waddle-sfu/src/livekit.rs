//! Concrete [`crate::SfuService`] impl backed by a LiveKit deployment.
//!
//! Keeps an in-memory registry of active calls keyed by [`CallId`]
//! containing the set of joined [`Identity`] values, used by the MUC
//! focus path to decide when a call has ended.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dashmap::DashMap;
use futures::{stream, StreamExt};
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

use crate::admin::{admin_base_url_from_ws, LiveKitAdmin, ReqwestLiveKitAdmin};
use crate::call::{
    CallGeneration, CallId, CallState, CallTeardownIntentLite, Identity, MediaCapabilities,
    ObservedCallSids, ParticipantSid, RoomSid, SidObservationDisposition, TeardownDisposition,
    TeardownTargetLite,
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

/// Maximum concurrent `ListParticipants` probes in one reconcile pass.
/// Each probe has a five-second HTTP timeout. Eight-way fan-out reduces
/// a pathological 100-room all-timeout pass from 500 seconds serially to
/// 13 waves (about 65 seconds), and keeps it under the 60-second interval
/// whenever at least one wave completes before the hard timeout.
pub const RECONCILE_CONCURRENCY: usize = 8;

#[derive(Debug, Clone)]
struct ParticipantState {
    participant_sid: Option<ParticipantSid>,
}

impl ParticipantState {
    fn new() -> Self {
        Self {
            participant_sid: None,
        }
    }
}

#[derive(Debug, Clone)]
struct CallEntry {
    generation: CallGeneration,
    room_sid: Option<RoomSid>,
    participants: HashMap<Identity, ParticipantState>,
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
    /// Participants still registered after the clear.
    remaining: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidGuardDisposition {
    Applied,
    StaleSid,
}

#[derive(Debug, Clone)]
enum ClearDisposition {
    Cleared(ClearOutcome),
    NoCall,
    StaleSid,
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
}

impl LiveKitTeardownExecutor {
    pub fn current_generation(&self, call_id: &CallId) -> Option<CallGeneration> {
        self.calls.get(call_id).map(|entry| entry.generation)
    }

    pub async fn remove_participant(
        &self,
        call_id: &CallId,
        identity: &Identity,
        generation: Option<CallGeneration>,
        room_sid: Option<&RoomSid>,
        participant_sid: Option<&ParticipantSid>,
    ) -> Result<TeardownExecution, SfuError> {
        match self.guard(
            call_id,
            generation,
            room_sid,
            Some((identity, participant_sid)),
        ) {
            TeardownGuard::Proceed => {}
            TeardownGuard::Stale => return Ok(TeardownExecution::StaleGeneration),
            TeardownGuard::Unresolved => return Ok(TeardownExecution::Occupied),
        }
        self.admin.remove_participant(call_id, identity).await?;
        Ok(TeardownExecution::Executed)
    }

    pub async fn delete_room_if_empty(
        &self,
        call_id: &CallId,
        departing: Option<&Identity>,
        generation: Option<CallGeneration>,
        room_sid: Option<&RoomSid>,
    ) -> Result<TeardownExecution, SfuError> {
        match self.guard(call_id, generation, room_sid, None) {
            TeardownGuard::Proceed => {}
            TeardownGuard::Stale => return Ok(TeardownExecution::StaleGeneration),
            TeardownGuard::Unresolved => return Ok(TeardownExecution::Occupied),
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
                self.remove_participant(
                    &intent.call_id,
                    identity,
                    intent.generation,
                    intent.room_sid.as_ref(),
                    participant_sid.as_ref(),
                )
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
    ) -> TeardownGuard {
        let Some(entry) = self.calls.get(call_id) else {
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
    call_generations: Arc<DashMap<CallId, u64>>,
    /// Live JWT identifiers per `(call, identity)`, each carrying
    /// its `exp` so revocation entries can be swept once the token
    /// would have lapsed anyway. Capped at
    /// [`MAX_ISSUED_PER_PARTICIPANT`] entries per key — the oldest
    /// is evicted FIFO when a fresh token is minted past the cap so
    /// a misbehaving client cannot push the tracker into unbounded
    /// memory growth.
    issued: DashMap<(CallId, Identity), Vec<IssuedJti>>,
    /// Wall-clock instant each `(call, identity)` was registered.
    /// Read only by the reconciliation backstop to enforce
    /// [`RECONCILE_GRACE_SECONDS`]: a participant absent from LiveKit's
    /// `ListParticipants` is only swept once it has been registered
    /// longer than the grace window, so a still-connecting joiner is
    /// never mistaken for a ghost. Kept in lockstep with `calls`:
    /// written in `register_call_participant`, removed in
    /// `clear_local_state`.
    registered_at: DashMap<(CallId, Identity), DateTime<Utc>>,
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
    /// the map after that point is pure overhead. Bookkeeping today —
    /// LiveKit itself doesn't call back to verify jti, so a stolen
    /// token stays usable until its `exp`. Documented limitation; the
    /// path-to-real-revocation needs LiveKit cooperation (webhook
    /// validation hook) or a shared revocation store (Redis) once
    /// Waddle scales past a single SFU instance.
    revoked: DashMap<Jti, DateTime<Utc>>,
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
    fn adopt_discovered_call(
        &self,
        call_id: &CallId,
        room_sid: Option<RoomSid>,
        participants: &[Identity],
        now: DateTime<Utc>,
    ) -> bool {
        if participants.is_empty() {
            return false;
        }
        let dashmap::Entry::Vacant(entry) = self.calls.entry(call_id.clone()) else {
            return false;
        };
        let generation = {
            let mut last_generation = self.call_generations.entry(call_id.clone()).or_insert(0);
            *last_generation += 1;
            CallGeneration::new(*last_generation)
        };
        let participant_states: HashMap<Identity, ParticipantState> = participants
            .iter()
            .cloned()
            .map(|identity| (identity, ParticipantState::new()))
            .collect();
        entry.insert(CallEntry {
            generation,
            room_sid,
            participants: participant_states,
        });
        for identity in participants {
            self.registered_at
                .insert((call_id.clone(), identity.clone()), now);
            self.absent_streak
                .remove(&(call_id.clone(), identity.clone()));
        }
        true
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
            absent_streak: DashMap::new(),
            revoked: DashMap::new(),
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
    }

    fn guard_and_learn_observed_sids(
        call_id: &CallId,
        identity: &Identity,
        entry: &mut CallEntry,
        observed_sids: Option<&ObservedCallSids>,
    ) -> SidGuardDisposition {
        let Some(observed_sids) = observed_sids else {
            return SidGuardDisposition::Applied;
        };
        let identity_is_tracked = entry.participants.contains_key(identity);

        // Validate every learned value before mutating the entry. A
        // participant-SID mismatch must be a true no-op, including when
        // the same event also carries the first room SID we have seen.
        if let Some(room_sid) = observed_sids.room_sid.as_ref() {
            if let Some(stored_room_sid) = entry.room_sid.as_ref() {
                if stored_room_sid != room_sid {
                    tracing::warn!(
                        call_id = %call_id,
                        identity = %identity.as_livekit_identity(),
                        room_sid = %room_sid,
                        stored_room_sid = %stored_room_sid,
                        "LiveKit event ignored as stale: room sid mismatch"
                    );
                    return SidGuardDisposition::StaleSid;
                }
            }
        }

        if let Some(participant_sid) = observed_sids.participant_sid.as_ref() {
            if let Some(state) = entry.participants.get(identity) {
                if let Some(stored_participant_sid) = state.participant_sid.as_ref() {
                    if stored_participant_sid != participant_sid {
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
            }
        }

        if identity_is_tracked && entry.room_sid.is_none() {
            entry.room_sid.clone_from(&observed_sids.room_sid);
        }
        if let Some(state) = entry.participants.get_mut(identity) {
            if state.participant_sid.is_none() {
                state
                    .participant_sid
                    .clone_from(&observed_sids.participant_sid);
            }
        }

        SidGuardDisposition::Applied
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
    fn clear_local_state(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> ClearDisposition {
        let mut entry = match self.calls.get_mut(call_id) {
            Some(entry) => entry,
            None => return ClearDisposition::NoCall,
        };
        if matches!(
            Self::guard_and_learn_observed_sids(
                call_id,
                identity,
                entry.value_mut(),
                observed_sids
            ),
            SidGuardDisposition::StaleSid
        ) {
            return ClearDisposition::StaleSid;
        }

        let participant_sid = entry
            .participants
            .get(identity)
            .and_then(|participant| participant.participant_sid.clone());
        let room_sid = entry.room_sid.clone();
        let was_present = entry.participants.remove(identity).is_some();
        let generation = entry.generation;
        drop(entry);

        if let Some((_, issued)) = self.issued.remove(&(call_id.clone(), identity.clone())) {
            for issued in issued {
                self.revoked.insert(issued.jti, issued.exp);
            }
        }
        self.registered_at
            .remove(&(call_id.clone(), identity.clone()));
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

        ClearDisposition::Cleared(ClearOutcome {
            was_present,
            emptied,
            generation,
            room_sid,
            participant_sid,
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
    /// [`LiveKitTeardownExecutor::delete_room_if_empty`]. Spawn target is the runtime handle
    /// captured at construction. When none is attached, when the admin
    /// semaphore is saturated, or when an admitted admin call fails, the
    /// corresponding typed effect is handed to `teardown_failure_sink` for
    /// durable retry. The availability gate bounds both in-flight calls and
    /// spawned tasks during a teardown burst.
    fn schedule_remote_teardown(
        &self,
        call_id: CallId,
        identity: Identity,
        we_just_emptied: bool,
        generation: Option<CallGeneration>,
        room_sid: Option<RoomSid>,
        participant_sid: Option<ParticipantSid>,
    ) {
        let participant_intent = CallTeardownIntentLite {
            call_id: call_id.clone(),
            target: TeardownTargetLite::Participant {
                identity: identity.clone(),
                participant_sid,
            },
            generation,
            room_sid: room_sid.clone(),
        };
        let room_intent = we_just_emptied.then(|| CallTeardownIntentLite {
            call_id: call_id.clone(),
            target: TeardownTargetLite::Room,
            generation,
            room_sid,
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
        let executor = self.teardown_executor();
        let reporter = self.teardown_reporter.clone();
        runtime.spawn(async move {
            let _permit = permit;
            if !matches!(
                executor
                    .remove_participant(
                        &call_id,
                        &identity,
                        generation,
                        participant_intent.room_sid.as_ref(),
                        match &participant_intent.target {
                            TeardownTargetLite::Participant {
                                participant_sid, ..
                            } => participant_sid.as_ref(),
                            TeardownTargetLite::Room => None,
                        },
                    )
                    .await,
                Ok(TeardownExecution::Executed | TeardownExecution::StaleGeneration)
            ) {
                reporter.report([participant_intent]);
            }
            if we_just_emptied
                && executor
                    .delete_room_if_empty(
                        &call_id,
                        Some(&identity),
                        generation,
                        room_intent
                            .as_ref()
                            .and_then(|intent| intent.room_sid.as_ref()),
                    )
                    .await
                    .is_err()
            {
                if let Some(intent) = room_intent {
                    reporter.report([intent]);
                }
            }
        });
    }

    /// Move every outstanding JTI for `(call_id, identity)` into the
    /// revocation map WITHOUT touching the participant registry. Used
    /// on a mid-call grant downgrade: the participant stays in the
    /// call (listen-only), and their pre-downgrade tokens are marked
    /// spent.
    ///
    /// This is local bookkeeping, NOT enforcement: LiveKit reads
    /// permissions off the JWT at join and never asks us about the
    /// jti (see the `revoked` field docs). Enforcement for a rejoin
    /// with a stale token comes from re-asserting permissions on the
    /// `participant_joined` webhook.
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

        match self.admin.list_rooms().await {
            Ok(listed) => {
                for room in listed {
                    let Ok(call_id) = CallId::new(room.name) else {
                        tracing::debug!("SFU reconcile: ignoring non-Waddle LiveKit room name");
                        continue;
                    };
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
            let registered = if was_registered {
                registered
            } else {
                if self.adopt_discovered_call(&call_id, listed_room_sid, &live, now) {
                    rooms_adopted += 1;
                    tracing::info!(
                        call_id = %call_id,
                        participants = live.len(),
                        "SFU reconcile: adopted active LiveKit room missing from local registry"
                    );
                }
                live.clone()
            };
            let live_set: HashSet<String> =
                live.iter().map(Identity::as_livekit_identity).collect();

            for identity in registered {
                let key = (call_id.clone(), identity.clone());
                if live_set.contains(&identity.as_livekit_identity()) {
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
                let outcome = self.clear_local_state(&call_id, &identity, None);
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
        crate::ReconcilePassSummary {
            swept,
            rooms_examined,
            rooms_adopted,
            rooms_swept: swept_rooms.len() as u64,
            occupancy_failures,
        }
    }
}

impl crate::SfuReconciler for LiveKitSfu {
    fn live_participants<'a>(&'a self, call_id: &'a CallId) -> crate::LiveParticipantsFuture<'a> {
        Box::pin(async move {
            match self.admin.room_occupancy(call_id).await {
                Ok(occupancy) => Some(occupancy.waddle),
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
        while entry.len() >= MAX_ISSUED_PER_PARTICIPANT {
            entry.remove(0);
        }
        entry.push(IssuedJti {
            jti: token.jti.clone(),
            exp: token.expires_at,
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
        match self.calls.entry(call_id.clone()) {
            dashmap::Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                if entry.participants.is_empty() {
                    let mut last_generation =
                        self.call_generations.entry(call_id.clone()).or_insert(0);
                    *last_generation += 1;
                    entry.generation = CallGeneration::new(*last_generation);
                    entry.room_sid = None;
                }
                entry
                    .participants
                    .entry(identity.clone())
                    .or_insert_with(ParticipantState::new);
            }
            dashmap::Entry::Vacant(entry) => {
                let generation = {
                    let mut last_generation =
                        self.call_generations.entry(call_id.clone()).or_insert(0);
                    *last_generation += 1;
                    CallGeneration::new(*last_generation)
                };
                let mut participants = HashMap::new();
                participants.insert(identity.clone(), ParticipantState::new());
                entry.insert(CallEntry {
                    generation,
                    room_sid: None,
                    participants,
                });
            }
        }
        // Stamp (or refresh) the registration time so the
        // reconciliation backstop's grace window is measured from the
        // most recent (re)join, not a stale earlier attempt. A
        // (re-)registration also resets the absence streak (#1127):
        // any prior not-seen observations belong to the previous
        // connection attempt.
        self.registered_at
            .insert((call_id.clone(), identity.clone()), Utc::now());
        self.absent_streak
            .remove(&(call_id.clone(), identity.clone()));
    }

    fn register_call_participant_observed(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: &ObservedCallSids,
    ) -> SidObservationDisposition {
        match self.calls.entry(call_id.clone()) {
            dashmap::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                if matches!(
                    Self::guard_and_learn_observed_sids(
                        call_id,
                        identity,
                        entry,
                        Some(observed_sids),
                    ),
                    SidGuardDisposition::StaleSid
                ) {
                    return SidObservationDisposition::StaleSid;
                }
                if entry.participants.is_empty() {
                    let mut last_generation =
                        self.call_generations.entry(call_id.clone()).or_insert(0);
                    *last_generation += 1;
                    entry.generation = CallGeneration::new(*last_generation);
                }
                entry
                    .participants
                    .entry(identity.clone())
                    .or_insert_with(|| ParticipantState {
                        participant_sid: observed_sids.participant_sid.clone(),
                    });
                if entry.room_sid.is_none() {
                    entry.room_sid.clone_from(&observed_sids.room_sid);
                }
            }
            dashmap::Entry::Vacant(vacant) => {
                let generation = {
                    let mut last_generation =
                        self.call_generations.entry(call_id.clone()).or_insert(0);
                    *last_generation += 1;
                    CallGeneration::new(*last_generation)
                };
                let mut participants = HashMap::new();
                participants.insert(
                    identity.clone(),
                    ParticipantState {
                        participant_sid: observed_sids.participant_sid.clone(),
                    },
                );
                vacant.insert(CallEntry {
                    generation,
                    room_sid: observed_sids.room_sid.clone(),
                    participants,
                });
            }
        }
        self.registered_at
            .insert((call_id.clone(), identity.clone()), Utc::now());
        self.absent_streak
            .remove(&(call_id.clone(), identity.clone()));
        SidObservationDisposition::Applied
    }

    fn has_call_participant(&self, call_id: &CallId, identity: &Identity) -> bool {
        self.calls
            .get(call_id)
            .is_some_and(|entry| entry.participants.contains_key(identity))
    }

    fn revoke_issued_token(&self, call_id: &CallId, identity: &Identity, jti: &Jti) {
        let key = (call_id.clone(), identity.clone());
        let mut exp = None;
        if let Some(mut issued) = self.issued.get_mut(&key) {
            if let Some(position) = issued.iter().position(|entry| entry.jti == *jti) {
                exp = Some(issued.remove(position).exp);
            }
        }
        // Only a JTI we can prove we minted (still present in the
        // pair's issued window) is recorded. The jti reaches this
        // method from an UNVERIFIED claim inside the bounced stanza,
        // so unconditionally inserting would let crafted undeliverable
        // IQs grow the revocation map without bound. The bounce's
        // fresh mint is always still in the window at bounce time, so
        // the #1444 compensation is unaffected.
        let Some(exp) = exp else { return };
        // Don't leave an empty bucket behind for the common
        // mint-then-immediately-revoke bounce case; `remove_if`
        // re-checks under the shard lock so a concurrent mint that
        // repopulated the vec is preserved.
        self.issued
            .remove_if(&key, |_, issuances| issuances.is_empty());
        self.revoked.insert(jti.clone(), exp);
    }

    fn unregister_call_participant(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> TeardownDisposition {
        let clear = self.clear_local_state(call_id, identity, observed_sids);
        let (was_present, emptied, generation, room_sid, participant_sid, remaining) = match clear {
            ClearDisposition::Cleared(ClearOutcome {
                was_present,
                emptied,
                generation,
                room_sid,
                participant_sid,
                remaining,
            }) => (
                was_present,
                emptied,
                Some(generation),
                room_sid,
                participant_sid,
                remaining,
            ),
            ClearDisposition::StaleSid => return TeardownDisposition::StaleSid,
            ClearDisposition::NoCall => {
                let generation = self
                    .call_generations
                    .get(call_id)
                    .map(|generation| CallGeneration::new(*generation));
                self.schedule_remote_teardown(
                    call_id.clone(),
                    identity.clone(),
                    false,
                    generation,
                    observed_sids.and_then(|sids| sids.room_sid.clone()),
                    observed_sids.and_then(|sids| sids.participant_sid.clone()),
                );
                return TeardownDisposition::Applied(CallState::Active { remaining: 0 });
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
            generation,
            room_sid,
            participant_sid,
        );

        TeardownDisposition::Applied(state)
    }

    fn note_participant_left(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> TeardownDisposition {
        // LiveKit's `participant_left` webhook is the SFU
        // acknowledging it already removed the participant — usually
        // because we asked it to. Doing only the local cleanup avoids
        // a feedback loop where the webhook fires another
        // `RemoveParticipant` against an already-removed participant
        // (LiveKit would return `not_found`, which is mapped to
        // success, but the round-trip is wasted and amplifies the
        // race with quick rejoins).
        let clear = match self.clear_local_state(call_id, identity, observed_sids) {
            ClearDisposition::Cleared(clear) => clear,
            ClearDisposition::StaleSid => return TeardownDisposition::StaleSid,
            ClearDisposition::NoCall => {
                return TeardownDisposition::Applied(CallState::Active { remaining: 0 });
            }
        };

        let state = if clear.was_present && clear.emptied {
            CallState::Ended
        } else {
            CallState::Active {
                remaining: clear.remaining,
            }
        };
        TeardownDisposition::Applied(state)
    }

    fn observe_call_participant_sids(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> SidObservationDisposition {
        let Some(mut entry) = self.calls.get_mut(call_id) else {
            return SidObservationDisposition::Applied;
        };
        if !entry.participants.contains_key(identity) {
            return SidObservationDisposition::Applied;
        }
        match Self::guard_and_learn_observed_sids(
            call_id,
            identity,
            entry.value_mut(),
            observed_sids,
        ) {
            SidGuardDisposition::Applied => SidObservationDisposition::Applied,
            SidGuardDisposition::StaleSid => SidObservationDisposition::StaleSid,
        }
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
