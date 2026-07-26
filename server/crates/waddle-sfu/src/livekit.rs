//! Concrete [`crate::SfuService`] impl backed by a LiveKit deployment.
//!
//! Keeps an in-memory registry of active calls keyed by [`CallId`]
//! containing the set of joined [`Identity`] values, used by the MUC
//! focus path to decide when a call has ended.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dashmap::DashMap;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

use crate::admin::{admin_base_url_from_ws, LiveKitAdmin, ReqwestLiveKitAdmin};
use crate::call::{CallId, CallState, Identity, MediaCapabilities};
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

/// Shared registry of in-call participants. Held in an `Arc` so the
/// spawned admin teardown future can re-check membership before
/// firing `DeleteRoom`, closing the race where a fresh joiner
/// re-creates the call between local-clear and the remote evict.
type CallRegistry = Arc<DashMap<CallId, HashSet<Identity>>>;

/// Result of [`LiveKitSfu::clear_local_state`].
#[derive(Debug, Clone, Copy)]
struct ClearOutcome {
    /// `identity` was actually registered against the call.
    was_present: bool,
    /// The call entry was removed because it was empty at the moment
    /// of the atomic conditional removal — i.e. we (and nobody
    /// concurrent) hold no participant for it any more. Only this
    /// flag may gate a `DeleteRoom` (#1129).
    emptied: bool,
    /// Participants still registered after the clear.
    remaining: usize,
}

pub struct LiveKitSfu {
    config: SfuConfig,
    calls: CallRegistry,
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
    /// Monotonic ticket per `(call, identity)` for grant pushes, so a
    /// slow `UpdateParticipant` cannot overwrite a newer one's grants
    /// (see [`Self::schedule_permission_update`]). Shared with the
    /// spawned tasks; entries are dropped in `clear_local_state`
    /// alongside the rest of the participant's state.
    grant_generation: Arc<DashMap<(CallId, Identity), u64>>,
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
        Self {
            config,
            calls: Arc::new(DashMap::new()),
            issued: DashMap::new(),
            registered_at: DashMap::new(),
            absent_streak: DashMap::new(),
            revoked: DashMap::new(),
            grant_generation: Arc::new(DashMap::new()),
            admin,
            runtime: Handle::try_current().ok(),
            admin_permits: Arc::new(Semaphore::new(ADMIN_CONCURRENCY)),
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
        self.calls.get(call_id).map(|e| e.len()).unwrap_or(0)
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
    fn clear_local_state(&self, call_id: &CallId, identity: &Identity) -> ClearOutcome {
        let was_present = match self.calls.get_mut(call_id) {
            Some(mut entry) => entry.remove(identity),
            None => false,
        };

        if let Some((_, issued)) = self.issued.remove(&(call_id.clone(), identity.clone())) {
            for issued in issued {
                self.revoked.insert(issued.jti, issued.exp);
            }
        }
        self.registered_at
            .remove(&(call_id.clone(), identity.clone()));
        self.absent_streak
            .remove(&(call_id.clone(), identity.clone()));
        self.grant_generation
            .remove(&(call_id.clone(), identity.clone()));
        self.sweep_expired_revoked(Utc::now());

        // Atomic conditional removal: only drop the call entry if it
        // is *still* empty at removal time (see doc comment above).
        let emptied = self
            .calls
            .remove_if(call_id, |_, participants| participants.is_empty())
            .is_some();
        let remaining = if emptied {
            0
        } else {
            self.calls.get(call_id).map(|e| e.len()).unwrap_or(0)
        };

        ClearOutcome {
            was_present,
            emptied,
            remaining,
        }
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
    /// would evict them too. Spawn target is the runtime handle
    /// captured at construction; when none is attached (e.g. plain
    /// `#[test]` fixtures) the remote leg silently drops, matching
    /// pre-admin behaviour for those tests. The admin concurrency
    /// semaphore bounds in-flight HTTP tasks so a teardown burst
    /// can't fan out unboundedly.
    fn schedule_remote_teardown(&self, call_id: CallId, identity: Identity, we_just_emptied: bool) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let admin = Arc::clone(&self.admin);
        let permits = Arc::clone(&self.admin_permits);
        let calls = Arc::clone(&self.calls);
        runtime.spawn(async move {
            // `acquire_owned` returns `Err` only when the semaphore is
            // explicitly `close()`d. Production code never closes
            // `admin_permits`, so the `Err` arm is unreachable today;
            // the early-return is defensive scaffolding for a future
            // shutdown hook that may want to drain pending teardowns
            // without admitting new ones.
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };

            if let Err(err) = admin.remove_participant(&call_id, &identity).await {
                tracing::warn!(
                    call_id = %call_id,
                    identity = %identity.as_livekit_identity(),
                    error = %err,
                    "LiveKit RemoveParticipant failed; SFU may rely on DTLS timeout"
                );
            }
            if we_just_emptied {
                // Rejoin race: between local-clear and this point a
                // fresh participant may have re-registered (same
                // `call_id` is shared across all Muji occupants of a
                // MUC). `DeleteRoom` would evict that just-joined
                // session, so only proceed when the call is *still*
                // empty in our local view.
                if calls.get(&call_id).is_none() {
                    if let Err(err) = admin.delete_room(&call_id).await {
                        tracing::warn!(
                            call_id = %call_id,
                            error = %err,
                            "LiveKit DeleteRoom failed; empty room will linger until empty_timeout"
                        );
                    }
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
    /// Each push takes a monotonic ticket per `(call, identity)` under
    /// the shard lock before spawning, and drops itself if a later
    /// push has already been admitted. Without that, two grant
    /// changes for the same participant (a batch that revokes then
    /// grants voice, or two moderation IQs in quick succession) race
    /// on independent tasks through a 32-permit semaphore and can
    /// land on LiveKit out of order — leaving publish enabled after a
    /// revoke, which is exactly the state this feature exists to
    /// prevent.
    fn schedule_permission_update(
        &self,
        call_id: CallId,
        identity: Identity,
        capabilities: MediaCapabilities,
    ) {
        let key = (call_id.clone(), identity.clone());
        let ticket = {
            let mut entry = self.grant_generation.entry(key.clone()).or_insert(0);
            *entry += 1;
            *entry
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let admin = Arc::clone(&self.admin);
        let permits = Arc::clone(&self.admin_permits);
        let generation = Arc::clone(&self.grant_generation);
        runtime.spawn(async move {
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };
            // A newer push was admitted while we waited for a permit;
            // it carries the current grants, so ours is stale and
            // applying it would move LiveKit backwards.
            let superseded = || {
                generation
                    .get(&key)
                    .is_some_and(|current| *current > ticket)
            };
            if superseded() {
                return;
            }
            // A downgrade is a security-relevant convergence, so a
            // transient transport/5xx failure gets bounded retries
            // rather than a single best effort. Widening grants is not
            // retried: failing to restore publish rights is a
            // functional annoyance the next role change or rejoin
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
                    Ok(()) => return,
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
                if attempt == attempts || superseded() {
                    return;
                }
                tokio::time::sleep(GRANT_RETRY_BACKOFF * attempt).await;
            }
        });
    }

    /// One reconciliation pass against LiveKit's ground truth.
    ///
    /// For every call in the local registry, ask LiveKit who is
    /// actually connected (`ListParticipants`) and sweep any locally
    /// registered identity that LiveKit no longer reports — but only
    /// once that identity has been registered longer than `grace`, so
    /// a participant still ringing/connecting (the registry is
    /// populated at `session-initiate`, before the WebSocket connects)
    /// is never mistaken for a ghost. Returns the `(call, identity)`
    /// pairs that were swept so the caller (the webhook route's
    /// reconciliation task) can clear their MUC Muji presence via the
    /// same idempotent path the `participant_left` webhook uses.
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
    async fn reconcile_active_calls_inner(&self, grace: ChronoDuration) -> Vec<(CallId, Identity)> {
        let now = Utc::now();
        // Snapshot the registry into owned values up front so no
        // DashMap guard is held across the `.await` on the admin call.
        let snapshot: Vec<(CallId, Vec<Identity>)> = self
            .calls
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().iter().cloned().collect()))
            .collect();

        let mut swept = Vec::new();
        for (call_id, registered) in snapshot {
            let live = match self.admin.list_participant_identities(&call_id).await {
                Ok(live) => live,
                Err(err) => {
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
                let outcome = self.clear_local_state(&call_id, &identity);
                if outcome.was_present {
                    tracing::info!(
                        call_id = %call_id,
                        identity = %identity.as_livekit_identity(),
                        "SFU reconcile: swept ghost participant LiveKit no longer reports"
                    );
                    swept.push((call_id.clone(), identity));
                }
            }
        }
        swept
    }
}

impl crate::SfuReconciler for LiveKitSfu {
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
        self.calls
            .entry(call_id.clone())
            .or_default()
            .insert(identity.clone());
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

    fn has_call_participant(&self, call_id: &CallId, identity: &Identity) -> bool {
        self.calls
            .get(call_id)
            .is_some_and(|entry| entry.contains(identity))
    }

    fn unregister_call_participant(&self, call_id: &CallId, identity: &Identity) -> CallState {
        let ClearOutcome {
            was_present,
            emptied,
            remaining,
        } = self.clear_local_state(call_id, identity);

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
        self.schedule_remote_teardown(call_id.clone(), identity.clone(), we_just_emptied);

        state
    }

    fn note_participant_left(&self, call_id: &CallId, identity: &Identity) {
        // LiveKit's `participant_left` webhook is the SFU
        // acknowledging it already removed the participant — usually
        // because we asked it to. Doing only the local cleanup avoids
        // a feedback loop where the webhook fires another
        // `RemoveParticipant` against an already-removed participant
        // (LiveKit would return `not_found`, which is mapped to
        // success, but the round-trip is wasted and amplifies the
        // race with quick rejoins).
        let _ = self.clear_local_state(call_id, identity);
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
            .map(|entry| entry.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKey, ApiSecret, TurnSharedSecret};
    use chrono::Duration;
    use jid::FullJid;
    use url::Url;

    fn fixture_config() -> SfuConfig {
        SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::new(Url::parse("wss://livekit.waddle.social").unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.waddle.social"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-shared-secret-value"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        }
    }

    fn fixture_identity(name: &str) -> Identity {
        let jid: FullJid = format!("{name}@waddle.social/desktop")
            .parse()
            .expect("jid");
        Identity::from_jid(jid)
    }

    #[test]
    fn registry_tracks_participants_per_call() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("r1").unwrap();
        let a = fixture_identity("alice");
        let b = fixture_identity("bob");

        sfu.register_call_participant(&call, &a);
        sfu.register_call_participant(&call, &b);
        assert_eq!(sfu.participant_count(&call), 2);

        match sfu.unregister_call_participant(&call, &a) {
            CallState::Active { remaining } => assert_eq!(remaining, 1),
            CallState::Ended => panic!("call should still be active"),
        }

        match sfu.unregister_call_participant(&call, &b) {
            CallState::Ended => {}
            CallState::Active { .. } => panic!("call should end with no participants"),
        }
        assert_eq!(sfu.participant_count(&call), 0);
    }

    #[test]
    fn issue_join_token_returns_room_scoped_jwt() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c1").unwrap();
        let identity = fixture_identity("alice");

        let token = sfu
            .issue_join_token(&call, &identity, MediaCapabilities::direct_call_peer())
            .expect("token issued");
        assert_eq!(token.room, call);
        assert!(!token.jwt.as_str().is_empty());
    }

    #[test]
    fn issue_turn_credentials_yields_time_limited_pair() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let identity = fixture_identity("alice");
        let cred = sfu.issue_turn_credentials(&identity).expect("cred issued");
        assert!(cred.expires_at > chrono::Utc::now());
        assert!(cred
            .username
            .as_str()
            .contains("alice@waddle.social/desktop"));
    }

    #[test]
    fn unregister_revokes_every_jti_issued_to_the_participant() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c-revoke").unwrap();
        let alice = fixture_identity("alice");

        let t1 = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
            .unwrap();
        let t2 = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
            .unwrap();
        assert!(!sfu.is_revoked(&t1.jti));
        assert!(!sfu.is_revoked(&t2.jti));

        // Register + unregister: every previously-issued jti must
        // be revoked once the participant has left the call.
        sfu.register_call_participant(&call, &alice);
        sfu.unregister_call_participant(&call, &alice);

        assert!(sfu.is_revoked(&t1.jti));
        assert!(sfu.is_revoked(&t2.jti));
    }

    #[test]
    fn revocation_is_scoped_per_participant() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c-scope").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        let alice_token = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
            .unwrap();
        let bob_token = sfu
            .issue_join_token(&call, &bob, MediaCapabilities::direct_call_peer())
            .unwrap();

        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        sfu.unregister_call_participant(&call, &alice);

        // Alice's hangup must not revoke bob's still-active token.
        assert!(sfu.is_revoked(&alice_token.jti));
        assert!(!sfu.is_revoked(&bob_token.jti));
    }

    #[test]
    fn issued_jti_vec_is_capped_per_participant() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c-cap").unwrap();
        let alice = fixture_identity("alice");

        // Mint well past the cap; every fresh token should slot in,
        // but the per-participant vec must never exceed it.
        for _ in 0..(MAX_ISSUED_PER_PARTICIPANT * 3) {
            sfu.issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
                .expect("token issued");
            assert!(
                sfu.issued_count(&call, &alice) <= MAX_ISSUED_PER_PARTICIPANT,
                "issued vec must stay <= MAX_ISSUED_PER_PARTICIPANT"
            );
        }
        assert_eq!(
            sfu.issued_count(&call, &alice),
            MAX_ISSUED_PER_PARTICIPANT,
            "issued vec must saturate exactly at the cap"
        );
    }

    #[test]
    fn revoked_entries_are_swept_once_past_expiry() {
        use chrono::Duration as ChronoDuration;
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");

        // Seed the revoked map directly with a past-exp entry so
        // the test does not depend on real-time tickdown of the
        // token TTL.
        let stale_jti = Jti::new();
        let fresh_jti = Jti::new();
        sfu.revoked
            .insert(stale_jti.clone(), Utc::now() - ChronoDuration::seconds(60));
        sfu.revoked
            .insert(fresh_jti.clone(), Utc::now() + ChronoDuration::seconds(60));

        // Reading the stale jti must return false (the token can
        // no longer be replayed regardless) AND drop the entry.
        assert!(!sfu.is_revoked(&stale_jti));
        assert!(sfu.is_revoked(&fresh_jti));
        assert_eq!(sfu.revoked_count(), 1);

        // Running the unregister-path sweep clears any other stale
        // entries that piled up since the last sweep.
        sfu.revoked
            .insert(Jti::new(), Utc::now() - ChronoDuration::seconds(1));
        let alice = fixture_identity("alice");
        let call = CallId::new("c-sweep").unwrap();
        sfu.register_call_participant(&call, &alice);
        sfu.unregister_call_participant(&call, &alice);
        assert_eq!(
            sfu.revoked_count(),
            1,
            "unregister sweep must clear past-exp entries; one fresh entry should remain"
        );
    }

    #[test]
    fn register_is_idempotent() {
        let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
        let call = CallId::new("c1").unwrap();
        let identity = fixture_identity("alice");

        sfu.register_call_participant(&call, &identity);
        sfu.register_call_participant(&call, &identity);
        assert_eq!(sfu.participant_count(&call), 1);
    }

    // -------- Admin-evict path (tokio runtime present) --------

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use crate::admin::LiveKitAdmin;

    #[derive(Default)]
    struct RecordingAdmin {
        remove_calls: Mutex<Vec<(CallId, Identity)>>,
        delete_calls: Mutex<Vec<CallId>>,
        update_calls: Mutex<Vec<(CallId, Identity, MediaCapabilities)>>,
        /// What LiveKit "reports" as connected per call. A call absent
        /// from the map lists as empty (room not found). Drives the
        /// reconciliation tests.
        live: Mutex<std::collections::HashMap<CallId, Vec<Identity>>>,
        /// When set, `list_participant_identities` errors instead of
        /// returning a set — used to assert reconcile skips a call it
        /// can't confirm rather than sweeping it.
        list_errors: Mutex<bool>,
    }

    impl RecordingAdmin {
        fn remove_snapshot(&self) -> Vec<(CallId, Identity)> {
            self.remove_calls.lock().expect("recording lock").clone()
        }

        fn delete_snapshot(&self) -> Vec<CallId> {
            self.delete_calls.lock().expect("recording lock").clone()
        }

        fn update_snapshot(&self) -> Vec<(CallId, Identity, MediaCapabilities)> {
            self.update_calls.lock().expect("recording lock").clone()
        }

        fn set_live(&self, call: &CallId, identities: Vec<Identity>) {
            self.live
                .lock()
                .expect("recording lock")
                .insert(call.clone(), identities);
        }

        fn fail_list(&self) {
            self.set_list_failing(true);
        }

        fn set_list_failing(&self, failing: bool) {
            *self.list_errors.lock().expect("recording lock") = failing;
        }
    }

    impl LiveKitAdmin for RecordingAdmin {
        fn remove_participant<'a>(
            &'a self,
            room: &'a CallId,
            identity: &'a Identity,
        ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
            let room = room.clone();
            let identity = identity.clone();
            Box::pin(async move {
                self.remove_calls
                    .lock()
                    .expect("recording lock")
                    .push((room, identity));
                Ok(())
            })
        }

        fn delete_room<'a>(
            &'a self,
            room: &'a CallId,
        ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
            let room = room.clone();
            Box::pin(async move {
                self.delete_calls.lock().expect("recording lock").push(room);
                Ok(())
            })
        }

        fn update_participant<'a>(
            &'a self,
            room: &'a CallId,
            identity: &'a Identity,
            capabilities: MediaCapabilities,
        ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
            let room = room.clone();
            let identity = identity.clone();
            Box::pin(async move {
                self.update_calls.lock().expect("recording lock").push((
                    room,
                    identity,
                    capabilities,
                ));
                Ok(())
            })
        }

        fn list_participant_identities<'a>(
            &'a self,
            room: &'a CallId,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<Identity>, SfuError>> + Send + 'a>> {
            let room = room.clone();
            Box::pin(async move {
                if *self.list_errors.lock().expect("recording lock") {
                    return Err(SfuError::InvalidCallId("simulated list failure".into()));
                }
                Ok(self
                    .live
                    .lock()
                    .expect("recording lock")
                    .get(&room)
                    .cloned()
                    .unwrap_or_default())
            })
        }
    }

    /// Yield enough times for any spawned admin task on the current
    /// runtime to make progress. The spawned future does a couple of
    /// `Mutex` operations and returns, so two yields are more than
    /// sufficient; tighten or loosen if `RecordingAdmin` grows steps.
    async fn drain_admin_tasks() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn update_capabilities_pushes_permission_for_registered_participant() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-grants").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);

        let caps = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted);
        sfu.update_participant_capabilities(&call, &alice, caps);
        drain_admin_tasks().await;

        let updates = admin.update_snapshot();
        assert_eq!(updates.len(), 1, "UpdateParticipant fires exactly once");
        assert_eq!(&updates[0].0, &call);
        assert_eq!(
            updates[0].1.as_livekit_identity(),
            alice.as_livekit_identity()
        );
        assert_eq!(updates[0].2, caps);
        assert!(
            sfu.has_call_participant(&call, &alice),
            "a grant update must not unregister the participant"
        );
        assert!(
            admin.remove_snapshot().is_empty(),
            "a grant update must not evict"
        );
    }

    /// A downgrade must NOT be gated on local registration. Our
    /// per-process registry can legitimately have lost a participant
    /// LiveKit still holds (reconnect after `participant_left`, room
    /// actor migrated between cluster nodes, reconcile sweep), and
    /// skipping the push for them would let a de-voiced occupant keep
    /// publishing — a fail-open in the one direction that must never
    /// fail open. Mirrors `unregister_call_participant`'s
    /// always-run `RemoveParticipant`.
    #[tokio::test]
    async fn downgrade_pushes_even_when_the_local_registry_lost_the_participant() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-grants-ghost").unwrap();
        let alice = fixture_identity("alice");
        assert!(
            !sfu.has_call_participant(&call, &alice),
            "fixture models a participant absent from the local registry"
        );

        sfu.update_participant_capabilities(
            &call,
            &alice,
            MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
        );
        drain_admin_tasks().await;

        let updates = admin.update_snapshot();
        assert_eq!(
            updates.len(),
            1,
            "the downgrade must still reach LiveKit: {updates:?}"
        );
        assert!(updates[0].2.is_listen_only());
    }

    #[tokio::test]
    async fn downgrade_to_listen_only_revokes_outstanding_tokens_but_keeps_participant() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-demote").unwrap();
        let alice = fixture_identity("alice");
        let token = sfu
            .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
            .expect("token");
        sfu.register_call_participant(&call, &alice);

        sfu.update_participant_capabilities(
            &call,
            &alice,
            MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
        );
        drain_admin_tasks().await;

        assert!(
            sfu.is_revoked(&token.jti),
            "a not-yet-used pre-demotion token must not be replayable with stale publish rights"
        );
        assert_eq!(sfu.issued_count(&call, &alice), 0);
        assert!(
            sfu.has_call_participant(&call, &alice),
            "the demoted participant stays in the call as a listener"
        );
        assert_eq!(admin.update_snapshot().len(), 1);
    }

    #[tokio::test]
    async fn upgrade_to_voice_does_not_revoke_tokens() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-promote").unwrap();
        let alice = fixture_identity("alice");
        let token = sfu
            .issue_join_token(
                &call,
                &alice,
                MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
            )
            .expect("token");
        sfu.register_call_participant(&call, &alice);

        sfu.update_participant_capabilities(
            &call,
            &alice,
            MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Voiced),
        );
        drain_admin_tasks().await;

        assert!(
            !sfu.is_revoked(&token.jti),
            "a promotion widens grants; existing tokens stay valid"
        );
        assert_eq!(admin.update_snapshot().len(), 1);
        assert!(admin.update_snapshot()[0].2.can_publish);
    }

    /// A slow push must never overwrite a newer one's grants. Two
    /// updates admitted back-to-back (a batch that revokes then
    /// re-grants voice, or two moderation IQs in a row) run on
    /// independent tasks; only the latest may reach LiveKit.
    #[tokio::test]
    async fn a_superseded_grant_push_is_dropped_before_reaching_livekit() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-order").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);

        let voiced = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Voiced);
        let muted = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted);
        // Stale promotion first, then the demotion that supersedes it.
        // Both are queued before either task gets to run.
        sfu.update_participant_capabilities(&call, &alice, voiced);
        sfu.update_participant_capabilities(&call, &alice, muted);
        drain_admin_tasks().await;

        let updates = admin.update_snapshot();
        assert!(
            !updates.is_empty(),
            "at least the newest push must reach LiveKit"
        );
        assert!(
            updates.iter().all(|(_, _, caps)| caps.is_listen_only()),
            "no superseded (publish-enabling) push may land after the demotion: {updates:?}"
        );
        assert_eq!(
            updates.last().expect("non-empty").2,
            muted,
            "the last write must be the newest grants"
        );
    }

    #[tokio::test]
    async fn unregister_schedules_remove_participant_on_the_admin_client() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-evict").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);

        // Alice leaves: RemoveParticipant must fire; DeleteRoom must
        // NOT fire because bob is still in the call.
        let state = sfu.unregister_call_participant(&call, &alice);
        assert!(matches!(state, CallState::Active { remaining: 1 }));
        drain_admin_tasks().await;

        let removes = admin.remove_snapshot();
        assert_eq!(
            removes.len(),
            1,
            "RemoveParticipant should fire exactly once"
        );
        assert_eq!(&removes[0].0, &call);
        assert_eq!(
            removes[0].1.as_livekit_identity(),
            alice.as_livekit_identity()
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must not fire while the call still has participants"
        );
    }

    #[tokio::test]
    async fn unregister_last_participant_also_schedules_delete_room() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-empty").unwrap();
        let alice = fixture_identity("alice");

        sfu.register_call_participant(&call, &alice);
        let state = sfu.unregister_call_participant(&call, &alice);
        assert_eq!(state, CallState::Ended);
        drain_admin_tasks().await;

        let deletes = admin.delete_snapshot();
        assert_eq!(deletes.len(), 1, "DeleteRoom must fire on last participant");
        assert_eq!(&deletes[0], &call);

        let removes = admin.remove_snapshot();
        assert_eq!(
            removes.len(),
            1,
            "RemoveParticipant still fires for the last leaver"
        );
        assert_eq!(&removes[0].0, &call);
    }

    #[tokio::test]
    async fn unregister_of_unknown_identity_fires_remove_participant_but_not_delete_room() {
        // Edge case: a session-terminate arrives without a matching
        // register (e.g. server-side state was lost, a client races
        // a re-init, a replayed terminate from a long-dead session).
        // `RemoveParticipant` must still fire because LiveKit may
        // hold the participant via a separate path. `DeleteRoom`
        // MUST NOT fire — we don't know the call's true state, and
        // tearing it down could evict participants we never tracked.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-ghost").unwrap();
        let ghost = fixture_identity("mallory");

        let state = sfu.unregister_call_participant(&call, &ghost);
        assert!(
            matches!(state, CallState::Active { remaining: 0 }),
            "ghost unregister must NOT report CallState::Ended; got {state:?}",
        );
        drain_admin_tasks().await;

        let removes = admin.remove_snapshot();
        assert_eq!(removes.len(), 1);
        assert_eq!(
            removes[0].1.as_livekit_identity(),
            ghost.as_livekit_identity()
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must not fire when we never tracked the participant",
        );
    }

    #[tokio::test]
    async fn note_participant_left_clears_local_state_without_admin_call() {
        // The LiveKit webhook bridge calls this path when LiveKit's
        // `participant_left` fires. Doing a back-channel admin
        // RemoveParticipant here would amplify the wire traffic (LK
        // would 404 our redundant call) and racily kick fresh
        // rejoiners. The trait contract forbids it; assert the
        // production impl honours it.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-webhook").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);

        sfu.note_participant_left(&call, &alice);
        drain_admin_tasks().await;

        assert_eq!(sfu.participant_count(&call), 0, "registry must be cleared");
        assert!(
            admin.remove_snapshot().is_empty(),
            "note_participant_left must NOT spawn RemoveParticipant",
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "note_participant_left must NOT spawn DeleteRoom",
        );
    }

    #[tokio::test]
    async fn last_participant_delete_room_skipped_when_someone_rejoins() {
        // Race: Alice hangs up (clearing local state + scheduling
        // teardown), Bob joins the same MUC call before the spawn
        // gets to its DeleteRoom step. The re-check inside the
        // spawn must observe Bob's registration and suppress
        // DeleteRoom so Bob's session is not evicted. We simulate
        // the rejoin by registering Bob immediately after Alice's
        // unregister returns, before yielding to the spawn.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-rejoin").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        sfu.register_call_participant(&call, &alice);
        let state = sfu.unregister_call_participant(&call, &alice);
        assert_eq!(state, CallState::Ended);

        // Bob rejoins before the spawned future polls. With a single-
        // threaded current-thread runtime this synchronous register
        // is guaranteed to land before any `yield_now`-scheduled
        // continuation observes the registry.
        sfu.register_call_participant(&call, &bob);

        drain_admin_tasks().await;

        let removes = admin.remove_snapshot();
        assert_eq!(
            removes.len(),
            1,
            "RemoveParticipant for Alice must still fire"
        );
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must be suppressed by the rejoin re-check; got {:?}",
            admin.delete_snapshot(),
        );
    }

    // -------- Reconciliation backstop --------

    use crate::SfuReconciler;

    #[tokio::test]
    async fn reconcile_sweeps_ghost_absent_from_livekit() {
        // Alice + Bob registered; LiveKit reports only Alice connected
        // (Bob's participant_left webhook was lost). With a zero grace
        // window Bob must be swept — after TWO consecutive absent
        // passes (#1127) — and returned for presence cleanup; Alice
        // must remain. No admin remove/delete is fired — the ghost is
        // already gone from LiveKit.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("general@muc.waddle.social").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");
        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        admin.set_live(&call, vec![alice.clone()]);

        let first_pass = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
        assert!(
            first_pass.is_empty(),
            "one absent observation must not sweep (#1127): {first_pass:?}"
        );
        assert!(
            sfu.has_call_participant(&call, &bob),
            "Bob must survive the first absent pass"
        );

        let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

        assert_eq!(swept, vec![(call.clone(), bob.clone())]);
        assert!(sfu.has_call_participant(&call, &alice), "Alice must remain");
        assert!(
            !sfu.has_call_participant(&call, &bob),
            "Bob must be swept from the registry"
        );
        assert_eq!(sfu.participant_count(&call), 1);
        assert!(
            admin.remove_snapshot().is_empty() && admin.delete_snapshot().is_empty(),
            "reconcile must not fire admin RemoveParticipant/DeleteRoom for already-gone ghosts"
        );
    }

    #[tokio::test]
    async fn reconcile_respects_registration_grace_window() {
        // A just-registered participant LiveKit hasn't seen yet (still
        // ringing/connecting) must NOT be swept while inside the grace
        // window — sweeping here would tear down a call coming up.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("room@muc.waddle.social").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);
        // LiveKit reports nobody (room not yet created / mid-connect).
        admin.set_live(&call, vec![]);

        let swept = sfu
            .reconcile_active_calls(ChronoDuration::seconds(3600))
            .await;

        assert!(
            swept.is_empty(),
            "a participant inside the grace window must not be swept"
        );
        assert_eq!(sfu.participant_count(&call), 1);
    }

    #[tokio::test]
    async fn reconcile_keeps_genuinely_connected_participants() {
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("room2@muc.waddle.social").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);
        admin.set_live(&call, vec![alice.clone()]);

        let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

        assert!(swept.is_empty(), "connected participant must not be swept");
        assert!(sfu.has_call_participant(&call, &alice));
    }

    #[tokio::test]
    async fn reconcile_skips_calls_it_cannot_confirm() {
        // If ListParticipants fails for a call, absence cannot be
        // confirmed; nothing is swept and the next pass retries.
        let admin = Arc::new(RecordingAdmin::default());
        admin.fail_list();
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("room3@muc.waddle.social").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);

        let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

        assert!(
            swept.is_empty(),
            "a call whose participant list could not be fetched must not be swept"
        );
        assert_eq!(sfu.participant_count(&call), 1);
    }

    #[tokio::test]
    async fn reconcile_livekit_restart_does_not_mass_terminate_live_calls() {
        // #1127: a LiveKit pod restart makes one pass report every
        // room as not-found (empty participant list). Clients silently
        // rejoin before the next pass. Nothing may be swept.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call_a = CallId::new("standup@muc.waddle.social").unwrap();
        let call_b = CallId::new("alice@waddle.social::dm-1").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");
        sfu.register_call_participant(&call_a, &alice);
        sfu.register_call_participant(&call_b, &bob);

        // Pass 1: restart — LiveKit knows no rooms (both list empty).
        let pass1 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
        assert!(pass1.is_empty(), "restart pass must not sweep: {pass1:?}");
        assert_eq!(sfu.participant_count(&call_a), 1);
        assert_eq!(sfu.participant_count(&call_b), 1);

        // Clients reconnected before pass 2.
        admin.set_live(&call_a, vec![alice.clone()]);
        admin.set_live(&call_b, vec![bob.clone()]);
        let pass2 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
        assert!(pass2.is_empty(), "reconnected clients must not be swept");

        // Pass 3: streaks were reset by the connected observation, so
        // a later single absent blip still does not sweep.
        admin.set_live(&call_a, vec![]);
        let pass3 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
        assert!(
            pass3.is_empty(),
            "streak must have been reset by the connected pass: {pass3:?}"
        );
        assert!(sfu.has_call_participant(&call_a, &alice));
    }

    #[tokio::test]
    async fn reconcile_failed_pass_resets_absence_streak() {
        // #1127 AC: the absence tracker resets on a failed pass — two
        // absent observations separated by a ListParticipants failure
        // are not "consecutive".
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("room@muc.waddle.social").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);
        admin.set_live(&call, vec![]);

        // Absent pass 1 → streak 1.
        assert!(sfu
            .reconcile_active_calls(ChronoDuration::zero())
            .await
            .is_empty());
        // Failed pass → streak reset.
        admin.set_list_failing(true);
        assert!(sfu
            .reconcile_active_calls(ChronoDuration::zero())
            .await
            .is_empty());
        admin.set_list_failing(false);
        // Absent pass again → streak restarts at 1, still no sweep.
        let third = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
        assert!(
            third.is_empty(),
            "failed pass must reset the streak: {third:?}"
        );
        assert_eq!(sfu.participant_count(&call), 1);
        // Second CONSECUTIVE absent pass → swept.
        let fourth = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
        assert_eq!(fourth, vec![(call.clone(), alice)]);
        assert_eq!(sfu.participant_count(&call), 0);
    }

    #[tokio::test]
    async fn reconcile_streak_resets_on_reregistration() {
        // A participant re-registering (fresh session-initiate /
        // rejoin) invalidates absence observed against the previous
        // attempt.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("room@muc.waddle.social").unwrap();
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);
        admin.set_live(&call, vec![]);

        assert!(sfu
            .reconcile_active_calls(ChronoDuration::zero())
            .await
            .is_empty());
        // Rejoin between passes.
        sfu.register_call_participant(&call, &alice);
        // This absent pass is the FIRST of the new registration.
        assert!(
            sfu.reconcile_active_calls(ChronoDuration::zero())
                .await
                .is_empty(),
            "re-registration must reset the absence streak"
        );
        assert_eq!(sfu.participant_count(&call), 1);
    }

    // -------- #1129 teardown/join race --------

    #[test]
    fn concurrent_join_during_teardown_is_never_clobbered() {
        // #1129: `clear_local_state` used to compute `remaining == 0`
        // under the entry guard, drop it, then unconditionally remove
        // the call entry — deleting a joiner who registered in the
        // window. The atomic `remove_if` closes that: after BOTH an
        // unregister(alice) and a register(bob) have completed, bob
        // must always be present in the registry, whatever the
        // interleaving. Run many racing iterations to exercise the
        // window.
        let sfu = Arc::new(
            LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test (no runtime)"),
        );
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        for i in 0..200 {
            let call = CallId::new(format!("race-{i}")).unwrap();
            sfu.register_call_participant(&call, &alice);

            let leaver = {
                let sfu = Arc::clone(&sfu);
                let call = call.clone();
                let alice = alice.clone();
                std::thread::spawn(move || {
                    let _ = sfu.unregister_call_participant(&call, &alice);
                })
            };
            sfu.register_call_participant(&call, &bob);
            leaver.join().expect("leaver thread");

            assert!(
                sfu.has_call_participant(&call, &bob),
                "iteration {i}: concurrent joiner was clobbered by teardown (#1129)"
            );
        }
    }

    #[tokio::test]
    async fn delete_room_not_fired_when_joiner_lands_before_conditional_remove() {
        // #1129 second half: when the joiner wins the race, the
        // unregister must report the call as still active (not Ended)
        // so no DeleteRoom is scheduled against the fresh joiner.
        let admin = Arc::new(RecordingAdmin::default());
        let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
        let call = CallId::new("r-joiner-race").unwrap();
        let alice = fixture_identity("alice");
        let bob = fixture_identity("bob");

        sfu.register_call_participant(&call, &alice);
        // Simulate the joiner landing inside alice's teardown window:
        // remove alice from the set (step 1 of clear_local_state),
        // register bob, then run the full unregister — the conditional
        // removal must observe bob and keep the entry.
        sfu.calls
            .get_mut(&call)
            .expect("entry exists")
            .remove(&alice);
        sfu.register_call_participant(&call, &bob);

        let state = sfu.unregister_call_participant(&call, &alice);
        assert!(
            matches!(state, CallState::Active { remaining: 1 }),
            "joiner present at conditional-remove time must keep the call active; got {state:?}"
        );
        drain_admin_tasks().await;
        assert!(
            admin.delete_snapshot().is_empty(),
            "DeleteRoom must not fire while the fresh joiner is registered"
        );
        assert!(sfu.has_call_participant(&call, &bob));
    }
}
