//! ADR-0017 Phase 3 Slice 6: cross-node XEP-0198 resume via claim-steal
//! (element 8).
//!
//! `claim_session` (in `claims.rs`) is single-node/self-claim only: it
//! either finds the stream id in this process's own `sessions` map, or it
//! doesn't (`Ok(None)`). When it returns `Ok(None)`,
//! `stream_management.rs::handle_sm_resume` falls back to
//! [`InMemorySmSessionRegistry::attempt_cross_node_resume`] (or, in
//! `waddle-server`, its split [`InMemorySmSessionRegistry::prepare_cross_node_resume`]
//! / [`InMemorySmSessionRegistry::finish_cross_node_steal`] pair — see
//! "Cancellation boundary" below), which implements element 8's resume
//! branches:
//!
//! 1. **Detached, owned elsewhere**: a persisted snapshot already exists
//!    (the owning node already flushed it, live or dead) — identity check
//!    against the persisted snapshot's bound JID, then the consent/epoch
//!    -only `steal_for_resume` CAS, then a targeted
//!    [`InMemorySmSessionRegistry::hydrate_reclaimed`] (Slice 5's discipline
//!    — never `restore_from_persistence`, which is startup-only).
//! 2. **Live, owned elsewhere**: no persisted snapshot yet — a
//!    [`RemoteResumeAsker`] asks the owning node to force-detach (its own
//!    defense-in-depth identity check gates that destructive close), and on
//!    success this falls through to branch 1 against the now-persisted
//!    snapshot.
//! 3. **Owner unreachable, lease fresh**: the ask fails at the transport
//!    layer — held-response retry with backoff, capped at
//!    `handshake_budget` (the caller-supplied resume-handshake timeout).
//! 4. **Unclaimed, but persisted (FIX C)**: no claim exists on this entity
//!    at all, yet a persisted snapshot does — this node's own earlier
//!    repair release (see "Cancellation boundary" below) or the Slice 5
//!    fail-open-detach gap can both produce exactly this state. Direct
//!    `ensure_claimed`/acquire, then hydrate, then resume — never falls
//!    through to `NotFound` just because there was nothing to *steal*.
//!
//! Single-node/non-clustering behavior is unchanged: with no cluster claim
//! store wired (or no claim row on the entity at all and no persisted
//! snapshot either), this returns [`CrossNodeResumeOutcome::NotFound`] —
//! exactly the outcome `claim_session` itself already produces for "nothing
//! here," so the caller's existing `<failed/>` `item-not-found` path needs
//! no change.
//!
//! **One-shot CAS, not a retried one (phase plan, Slice 6 "Held-response
//! window vs. client-side retry" note + the janitor-vs-resume ordering
//! invariant, major fix 11 — both texts say the loser "fails cleanly," not
//! "retries")**: the claim epoch this call observed **before** its first
//! ask is captured once, up front, and the eventual
//! [`crate::ownership::ClaimStore::steal_for_resume`] call binds that SAME
//! original epoch — never a freshly re-read one. This is deliberate: if
//! this call retries its ask (branch 2/3) and, by the time it finally
//! reaches branch 1, some other actor has already bumped the epoch (the
//! true winner of a two-simultaneous-live-resume race, or the orphan
//! reaper's `steal_stale` winning mid-resume), this call's own CAS against
//! the stale original epoch is *guaranteed* to lose (`ClaimError::Conflict`)
//! rather than being able to steal the claim back a second time later. A
//! CAS loss here is an immediate, clean [`CrossNodeResumeOutcome::NotFound`]
//! — never a further retry of the CAS itself. The retry loop below exists
//! only to wait out an ask that hasn't resolved yet (owner unreachable, or a
//! transient "not live locally" race) — never to re-attempt a CAS that has
//! already lost.
//!
//! ## Cancellation boundary (FIX A/B/C, corrects deviation 47's original claim)
//!
//! `waddle-server::handle_sm_resume` races a cross-node resume attempt
//! against this node's graceful-shutdown token (so a held resume can never
//! delay this connection's own shutdown handling). Deviation 47 originally
//! raced the ENTIRE `attempt_cross_node_resume` call — including the
//! terminal `steal_for_resume` → `hydrate_reclaimed` → `claim_session`
//! sequence — and claimed "no committed steal can be lost," reasoning that
//! `tokio::select!` drops the losing future rather than polling it, so
//! nothing could commit after cancellation. **That reasoning only holds for
//! the CAS call itself.** `tokio::select!` can just as easily drop the
//! future *between* `steal_for_resume` committing in Postgres and
//! `hydrate_reclaimed`/`claim_session` completing — leaving a self-owned,
//! un-hydrated claim under a FRESH lease that the orphan reaper's
//! `OwnerStale` predicate can never fire against (the "owner" is this very
//! node, alive and heartbeating). The identical hazard exists even with no
//! cancellation involved at all: an ORDINARY `hydrate_reclaimed`/
//! `claim_session` failure (a storage read error, a lost self-reacquire
//! race, an internal bound expiring) after the CAS has already won leaves
//! the exact same wedge — worse than the shutdown case, because nothing
//! ever naturally reclaims it.
//!
//! The fix splits this call in two, at exactly the boundary where the first
//! durable write is about to be issued:
//!
//! - [`InMemorySmSessionRegistry::prepare_cross_node_resume`] does
//!   everything read-only: the initial claim snapshot read, the identity
//!   check, and the branch-2/3 ask/hold/backoff loop. It returns a
//!   [`CrossNodeResumeStage`] — either a terminal outcome, or a
//!   [`StealTicket`] recording exactly what write is about to happen. This
//!   is the ONLY part `handle_sm_resume` races against the shutdown token;
//!   cancelling it can never lose a write because none has happened yet.
//! - [`InMemorySmSessionRegistry::finish_cross_node_steal`] performs the
//!   write: the CAS (or, for the FIX C branch, a plain `ensure_claimed`)
//!   followed by `hydrate_reclaimed` and `claim_session`. It is called
//!   un-raced, un-timeout-dropped — every internal step still carries its
//!   own bound (a hung Postgres call must not hang this forever). A bound
//!   expiring before an ownership epoch is returned retains a bounded,
//!   read-only `current_claim` reconciliation item; a post-win bound feeds
//!   FIX B's exact repair. Neither path replays the one-shot CAS.
//! - **FIX B — post-win repair**: once the CAS (or FIX C's direct acquire)
//!   has won, any subsequent `hydrate_reclaimed`/`claim_session` failure —
//!   error or bound expiry — triggers
//!   [`InMemorySmSessionRegistry::repair_failed_local_claim`]: an
//!   epoch-gated, best-effort `ClaimStore::release` of the claim this call
//!   just won, retried a bounded few times. Exact terminal-release inventory
//!   is published before the first attempt, so cancellation or exhaustion
//!   leaves the janitor a durable retry owner even though a live node's own
//!   fresh lease prevents the orphan reaper from stealing the claim.
//! - **FIX C — the unclaimed-but-persisted branch** (this module's branch
//!   4, above) is what makes a successful repair actually recoverable: a
//!   client's very next resume retry must find a working path back in, not
//!   fall through to a bare `NotFound` just because there is no foreign
//!   claim left to *steal*.

use std::time::Duration;

use jid::BareJid;
use tokio::time::Instant;

use crate::ownership::{
    verify_resume_identity, ClaimEpoch, ClaimError, Entity, EntityType, NodeIdentity,
    ResumeIdentityProof,
};
use crate::stream_management::persistence::PersistedSession;

use super::core::InMemorySmSessionRegistry;
use super::{DetachedSession, SmRegistryError};

/// Initial backoff between held-response handshake retries (branch 3).
/// Not plan-specified verbatim; a conservative, short starting point so a
/// transient blip resolves quickly while a genuinely unreachable owner
/// still backs off rather than hammering the swarm.
const INITIAL_HANDSHAKE_BACKOFF: Duration = Duration::from_millis(100);

/// Backoff ceiling between held-response handshake retries (branch 3).
const MAX_HANDSHAKE_BACKOFF: Duration = Duration::from_secs(2);

/// Bound on the terminal CAS/acquire call in
/// [`InMemorySmSessionRegistry::finish_cross_node_steal`] (FIX A). Entirely
/// independent of the caller's `handshake_budget` — deliberately: once
/// `prepare_cross_node_resume` has decided to steal/acquire, the terminal
/// write sequence must never be cut short by an exhausted held-response
/// budget (that was exactly deviation 47's gap). A timeout here means the
/// CAS/acquire's own outcome is genuinely unknown (it may have committed
/// server-side with the reply lost) — surfaced as a typed error and never
/// retried, matching the one-shot-CAS discipline; there is nothing for FIX
/// B to repair yet because this call does not know whether it won.
const FINISH_STEAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on the post-win [`InMemorySmSessionRegistry::hydrate_reclaimed`]
/// call in [`InMemorySmSessionRegistry::complete_local_claim`] (FIX A/B). A
/// timeout past this point means the CAS/acquire is KNOWN to have won —
/// expiry here routes to [`InMemorySmSessionRegistry::repair_failed_local_claim`],
/// never to a dropped/abandoned future.
const FINISH_HYDRATE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on the post-hydrate `claim_session` self-reacquire in
/// [`InMemorySmSessionRegistry::complete_local_claim`] (FIX A/B). Same
/// repair-on-expiry contract as [`FINISH_HYDRATE_TIMEOUT`].
const FINISH_CLAIM_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on each individual repair-release attempt (FIX B).
const REPAIR_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times [`InMemorySmSessionRegistry::repair_failed_local_claim`]
/// retries its `ClaimStore::release` call before giving up and surfacing a
/// loud, named error (FIX B). The release is the PRIMARY path back to
/// sanity for a post-win hydrate/claim failure. The normal exact-release
/// inventory remains the fallback if every inline attempt fails.
const REPAIR_RELEASE_MAX_ATTEMPTS: u32 = 3;

/// Delay between [`REPAIR_RELEASE_MAX_ATTEMPTS`] repair-release retries.
const REPAIR_RELEASE_RETRY_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, PartialEq, Eq)]
enum MissingRepairSource {
    Error,
    NotFound,
}

/// Ask a remote node (identified by its `node_id`) to release a live SM
/// session for cross-node resume (element 8's "live, owned elsewhere"
/// branch — XEP-0198's "Resumption" section `<conflict/>`-close SHOULD).
///
/// Implemented in `waddle-server` via `RelayHandle` (the swarm's cross-node
/// ask); this trait keeps `waddle-xmpp` free of any clustering/swarm/libp2p
/// dependency — the same injection pattern [`crate::ownership::ClaimStore`]
/// already uses for the Postgres CAS implementation.
#[async_trait::async_trait]
pub trait RemoteResumeAsker: Send + Sync {
    /// Ask node `node_id` to force-detach its live SM session `stream_id`
    /// on behalf of `requester_bare_jid`. The remote node performs its own
    /// defense-in-depth identity check before doing anything destructive
    /// (ADR-0017 Phase 3 plan, Slice 6: "the identity check gates the
    /// destructive close itself, not just the subsequent CAS").
    async fn ask_remote_detach(
        &self,
        node_id: &str,
        stream_id: &str,
        requester_bare_jid: &BareJid,
    ) -> RemoteResumeAskOutcome;
}

/// Typed outcome of [`RemoteResumeAsker::ask_remote_detach`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteResumeAskOutcome {
    /// The remote node force-detached (or already had no live connection
    /// and the session was already persisted) — a persisted snapshot
    /// should now be readable.
    Detached,
    /// The remote node's own identity check rejected `requester_bare_jid`.
    IdentityMismatch,
    /// The remote node reports no live local session under this
    /// `stream_id` (a race with a concurrent detach/expiry/promotion) —
    /// caller should re-check local persistence and retry.
    NotLiveRemotely,
    /// The ask itself failed at the transport layer (unreachable, timed
    /// out, ...) — the owner-unreachable branch.
    Unreachable,
}

/// Outcome of [`InMemorySmSessionRegistry::attempt_cross_node_resume`].
#[derive(Debug)]
pub enum CrossNodeResumeOutcome {
    /// No cross-node claim on this stream id (absent, already self-owned,
    /// or this call's one-shot CAS lost) — caller should fall back to
    /// exactly the local, single-node `<failed/>` `item-not-found` path it
    /// already had.
    NotFound,
    /// The requester's SASL-authenticated bare JID did not match the
    /// snapshot's bound JID — caller should return `<failed/>`
    /// `not-authorized` without ever touching the claim (element 8).
    NotAuthorized,
    /// Won the steal and hydrated the session locally — behaves exactly
    /// like `claim_session`'s `Ok(Some(_))` from here on.
    Claimed(Box<DetachedSession>),
    /// The remote owner never became reachable/consenting within the
    /// held-response window — caller should return `<failed/>` with the
    /// phase plan's chosen `resource-constraint` condition (XEP-0198 does
    /// not itself demonstrate this condition for this case — see the
    /// plan's XEP fact-check note: "our chosen condition," not an
    /// XEP-0198-named one).
    OwnerUnreachable,
    /// Won the steal but a post-win hydrate/claim step failed transiently
    /// (storage error or timeout) and the just-won claim was released for a
    /// clean client retry. The durable session may well still exist, so this
    /// MUST NOT surface as `item-not-found` — storage loss never masquerades
    /// as absence; callers map it to an `internal-server-error`-class
    /// rejection instead.
    StorageUnavailable,
}

/// A pre-write decision produced by
/// [`InMemorySmSessionRegistry::prepare_cross_node_resume`] (FIX A's
/// cancellation boundary): everything up to and including this ticket's
/// construction is read-only and safe to race against a cancellation token.
/// Everything after — [`InMemorySmSessionRegistry::finish_cross_node_steal`]
/// — performs the actual write and MUST run to completion un-raced, or a
/// committed write can be stranded exactly as deviation 47 originally
/// (incorrectly) claimed was impossible. See this module's "Cancellation
/// boundary" doc section.
#[derive(Debug)]
pub struct StealTicket {
    entity: Entity,
    stream_id: String,
    mode: StealTicketMode,
}

#[derive(Debug)]
enum StealTicketMode {
    /// Branches 1/2/3: steal the claim from a foreign owner via the
    /// consent/epoch-only CAS, binding the epoch this call observed before
    /// its first ask (module doc's one-shot-CAS discipline).
    Steal {
        observed_epoch: ClaimEpoch,
        witness: ResumeIdentityProof,
    },
    /// Branch 4 (FIX C): no foreign claim exists at all, but a persisted
    /// snapshot does — acquire (self-reacquire-idempotent) directly rather
    /// than stealing.
    DirectAcquire,
}

/// Outcome of [`InMemorySmSessionRegistry::prepare_cross_node_resume`]: a
/// terminal outcome ready to hand straight to the caller, or a
/// [`StealTicket`] ready for
/// [`InMemorySmSessionRegistry::finish_cross_node_steal`]. See this
/// module's "Cancellation boundary" doc section for why this split exists.
#[derive(Debug)]
pub enum CrossNodeResumeStage {
    /// No write is going to happen — hand this straight to the caller,
    /// exactly as if `attempt_cross_node_resume` had returned it directly.
    Terminal(CrossNodeResumeOutcome),
    /// A write is about to happen. The caller MUST pass this to
    /// [`InMemorySmSessionRegistry::finish_cross_node_steal`] without
    /// racing that call against anything (shutdown tokens included).
    ReadyToSteal(StealTicket),
}

impl InMemorySmSessionRegistry {
    /// ADR-0017 Phase 3 Slice 6: attempt a cross-node XEP-0198 resume for
    /// `stream_id`. Called by `stream_management.rs::handle_sm_resume`
    /// ONLY after its own `claim_session` returned `Ok(None)` — i.e. this
    /// node has no local record of the session. See the module doc for the
    /// branch dispatch, the one-shot-CAS discipline, and the
    /// single-node-unchanged guarantee.
    ///
    /// This is a convenience wrapper around
    /// [`Self::prepare_cross_node_resume`] +
    /// [`Self::finish_cross_node_steal`] for callers that have no
    /// cancellation boundary of their own to enforce (every test in this
    /// crate, and any future non-`waddle-server` embedder).
    /// `waddle-server::handle_sm_resume` calls the split pair directly
    /// instead, so it can race only the prepare half against its
    /// graceful-shutdown token — see this module's "Cancellation boundary"
    /// doc section.
    pub async fn attempt_cross_node_resume(
        &self,
        stream_id: &str,
        requester_bare_jid: &BareJid,
        handshake_budget: Duration,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        match self
            .prepare_cross_node_resume(stream_id, requester_bare_jid, handshake_budget)
            .await?
        {
            CrossNodeResumeStage::Terminal(outcome) => Ok(outcome),
            CrossNodeResumeStage::ReadyToSteal(ticket) => {
                self.finish_cross_node_steal(ticket).await
            }
        }
    }

    /// The cancellable, read-only half (FIX A). Safe to race against a
    /// cancellation token: nothing in this function ever issues a durable
    /// write, so dropping this future mid-flight can never strand a
    /// committed change. Returns either a terminal outcome or a
    /// [`StealTicket`] for [`Self::finish_cross_node_steal`].
    pub async fn prepare_cross_node_resume(
        &self,
        stream_id: &str,
        requester_bare_jid: &BareJid,
        handshake_budget: Duration,
    ) -> Result<CrossNodeResumeStage, SmRegistryError> {
        let entity = Entity::new(EntityType::SmSession, stream_id.to_string());

        let Some(snapshot) =
            self.claim_store
                .current_claim(&entity)
                .await
                .map_err(|e| match e {
                    // Poisoned = persistently broken store: storage health,
                    // same classification as the ownership decorator.
                    crate::ownership::ClaimError::Backend(_)
                    | crate::ownership::ClaimError::Poisoned => {
                        SmRegistryError::StorageUnavailable(
                            super::traits::StorageOutageCause::Backend,
                        )
                    }
                    other => SmRegistryError::Internal(other.to_string()),
                })?
        else {
            // FIX C: no claim at all does not mean nothing to resume — a
            // persisted snapshot can outlive its claim (this node's own
            // FIX B repair release, or the Slice 5 fail-open-detach gap).
            return self
                .prepare_unclaimed_persisted_resume(&entity, stream_id, requester_bare_jid)
                .await;
        };
        let me = self.node_identity.current();
        if snapshot.owner.node_id == me.node_id && snapshot.owner.node_epoch == me.node_epoch {
            // Should already have been served by `claim_session`'s own
            // self-reacquire path; reaching this means a resume-vs-resume
            // race moved the claim back to this node between that call and
            // this one. Let the caller retry `claim_session` fresh rather
            // than double-claim here.
            return Ok(CrossNodeResumeStage::Terminal(
                CrossNodeResumeOutcome::NotFound,
            ));
        }
        // Captured once — see the module doc's "one-shot CAS" discipline.
        // The eventual `steal_for_resume` call binds this exact value,
        // never a freshly re-read one.
        let observed_epoch = snapshot.claim_epoch;
        let owner_node_id = snapshot.owner.node_id;

        let deadline = Instant::now() + handshake_budget;
        let mut backoff = INITIAL_HANDSHAKE_BACKOFF;
        loop {
            // Council-adjudicated FIX 1: every await in this loop is bound
            // by the budget still remaining — never by its own, independent
            // timeout. Without this, a single slow step (most importantly
            // `ask_remote_detach`, whose own mailbox/reply timeouts plus its
            // stale-ref retry-once can together run well past
            // `handshake_budget` — a wedged remote owner could hold this
            // call for roughly double the configured messaging timeouts)
            // could blow through `handshake_budget` on its own, defeating
            // the whole point of a bounded held-response window. A budget
            // that has already elapsed before this iteration even starts is
            // handled the same way a mid-step timeout is: classify the
            // window expiry once (see [`Self::classify_window_expiry`])
            // rather than issuing a doomed zero-duration timeout.
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self
                    .classify_window_expiry(&entity)
                    .await
                    .map(CrossNodeResumeStage::Terminal);
            }

            // Branch 1 (fast path): already detached/persisted.
            let persisted = match tokio::time::timeout(
                remaining,
                self.load_persisted_snapshot(stream_id),
            )
            .await
            {
                Ok(result) => result?,
                Err(_timeout) => {
                    return self
                        .classify_window_expiry(&entity)
                        .await
                        .map(CrossNodeResumeStage::Terminal);
                }
            };
            if let Some(persisted) = persisted {
                let Some(proof) =
                    verify_resume_identity(requester_bare_jid, &persisted.jid.to_bare())
                else {
                    return Ok(CrossNodeResumeStage::Terminal(
                        CrossNodeResumeOutcome::NotAuthorized,
                    ));
                };
                // FIX A: from this point on a write is imminent. No further
                // budget/deadline check runs here — once ready to steal,
                // `handshake_budget` no longer governs anything;
                // `finish_cross_node_steal`'s own fixed, independent bounds
                // (`FINISH_STEAL_TIMEOUT` et al.) take over, and their
                // expiry repairs (FIX B) rather than drops.
                return Ok(CrossNodeResumeStage::ReadyToSteal(StealTicket {
                    entity,
                    stream_id: stream_id.to_string(),
                    mode: StealTicketMode::Steal {
                        observed_epoch,
                        witness: proof,
                    },
                }));
            }

            // Branch 2/3: not yet persisted — ask the remote owner.
            let Some(asker) = self.remote_resume.clone() else {
                // A foreign claim exists but no asker is wired: either this
                // deployment mis-wires clustering (claim store present,
                // resume bridge absent), or clustering is disabled outright,
                // in which case `current_claim` above would already have
                // short-circuited via `NotFound` against
                // `InProcessClaimStore`. Treat as unreachable rather than
                // silently proceeding.
                return self
                    .classify_window_expiry(&entity)
                    .await
                    .map(CrossNodeResumeStage::Terminal);
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self
                    .classify_window_expiry(&entity)
                    .await
                    .map(CrossNodeResumeStage::Terminal);
            }
            // FIX 1: bound the ask itself — this is the primary offender
            // the fix targets (see the loop's own doc comment above). A
            // timeout here is handled identically to
            // `RemoteResumeAskOutcome::Unreachable`: fall through to the
            // same bounded backoff-and-retry below.
            let ask_outcome = match tokio::time::timeout(
                remaining,
                asker.ask_remote_detach(&owner_node_id, stream_id, requester_bare_jid),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_timeout) => RemoteResumeAskOutcome::Unreachable,
            };
            match ask_outcome {
                RemoteResumeAskOutcome::IdentityMismatch => {
                    return Ok(CrossNodeResumeStage::Terminal(
                        CrossNodeResumeOutcome::NotAuthorized,
                    ));
                }
                // `Detached`/`NotLiveRemotely`/`Unreachable` all fall
                // through to the SAME bounded backoff-and-retry below —
                // deliberately no bare `continue` here (a past bug: retrying
                // `Detached`/`NotLiveRemotely` with no deadline check and no
                // backoff at all could spin indefinitely, e.g. if branch 1's
                // persistence read keeps missing for a reason the ask
                // outcome alone cannot diagnose). Every retry, regardless of
                // which outcome triggered it, is bounded by `deadline` and
                // paced by `backoff`.
                RemoteResumeAskOutcome::Detached
                | RemoteResumeAskOutcome::NotLiveRemotely
                | RemoteResumeAskOutcome::Unreachable => {}
            }

            if Instant::now() >= deadline {
                return self
                    .classify_window_expiry(&entity)
                    .await
                    .map(CrossNodeResumeStage::Terminal);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(backoff.min(remaining)).await;
            backoff = (backoff * 2).min(MAX_HANDSHAKE_BACKOFF);
            if Instant::now() >= deadline {
                return self
                    .classify_window_expiry(&entity)
                    .await
                    .map(CrossNodeResumeStage::Terminal);
            }
        }
    }

    /// FIX C: branch 4. No `ClaimStore` claim exists on this entity at all
    /// — but a persisted snapshot might, if this node's own
    /// [`Self::repair_failed_local_claim`] released a claim it could not
    /// hydrate/self-claim, or the Slice 5 fail-open-detach gap left a
    /// snapshot behind with no claim ever created for it. Without this
    /// branch, both of those cases would fall straight through to
    /// `NotFound` forever (nothing ever creates a fresh claim for an
    /// already-persisted, already-unclaimed entity) — the exact permanent
    /// wedge FIX B's repair exists to make recoverable.
    async fn prepare_unclaimed_persisted_resume(
        &self,
        entity: &Entity,
        stream_id: &str,
        requester_bare_jid: &BareJid,
    ) -> Result<CrossNodeResumeStage, SmRegistryError> {
        let Some(persisted) = self.load_persisted_snapshot(stream_id).await? else {
            return Ok(CrossNodeResumeStage::Terminal(
                CrossNodeResumeOutcome::NotFound,
            ));
        };
        let Some(_proof) = verify_resume_identity(requester_bare_jid, &persisted.jid.to_bare())
        else {
            return Ok(CrossNodeResumeStage::Terminal(
                CrossNodeResumeOutcome::NotAuthorized,
            ));
        };
        // No foreign claim to prove consent against — `ResumeIdentityProof`
        // is only meaningful for `steal_for_resume`'s consent CAS, which
        // has nothing to steal from here. The identity check above is
        // still element 8's mandatory gate; `ensure_claimed` itself needs
        // no witness, only `NodeIdentity`.
        Ok(CrossNodeResumeStage::ReadyToSteal(StealTicket {
            entity: entity.clone(),
            stream_id: stream_id.to_string(),
            mode: StealTicketMode::DirectAcquire,
        }))
    }

    /// The uncancellable write half (FIX A). MUST be called to completion
    /// — never raced against a cancellation token, never wrapped in a
    /// timeout that would drop it — or a claim this call wins can be
    /// stranded self-owned and un-hydrated (this module's "Cancellation
    /// boundary" doc section). Every internal step still carries its own
    /// fixed bound; a bound expiring after the claim is won routes to FIX
    /// B's repair rather than abandoning the sequence.
    pub async fn finish_cross_node_steal(
        &self,
        ticket: StealTicket,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        let StealTicket {
            entity,
            stream_id,
            mode,
        } = ticket;
        match mode {
            StealTicketMode::Steal {
                observed_epoch,
                witness,
            } => {
                self.finish_steal(&entity, observed_epoch, witness, &stream_id)
                    .await
            }
            StealTicketMode::DirectAcquire => self.finish_direct_acquire(&entity, &stream_id).await,
        }
    }

    /// Branches 1/2/3's terminal CAS. This call either wins or loses,
    /// once. A loss is an immediate, clean `NotFound` — never retried
    /// (module doc's "one-shot CAS" discipline). A timeout here means the
    /// outcome is genuinely unknown (the CAS may have committed
    /// server-side with the reply lost to `FINISH_STEAL_TIMEOUT`) — it is
    /// retained for a later read-only owner/epoch lookup, never replayed
    /// against the already-consumed `observed_epoch`. On a win, hands off to
    /// [`Self::complete_local_claim`] — from this point FIX B's repair
    /// covers every subsequent failure.
    async fn finish_steal(
        &self,
        entity: &Entity,
        observed_epoch: ClaimEpoch,
        witness: ResumeIdentityProof,
        stream_id: &str,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        let me = self.node_identity.current();
        let Some(reservation) = self.reserve_reclaimed_claim_capacity(entity) else {
            return Err(SmRegistryError::Internal(
                "attempt_cross_node_resume: exact ownership capacity exhausted".to_string(),
            ));
        };
        match tokio::time::timeout(
            FINISH_STEAL_TIMEOUT,
            self.claim_store
                .steal_for_resume(entity, observed_epoch, witness, &me),
        )
        .await
        {
            Ok(Ok(new_epoch)) => {
                self.complete_local_claim(entity, me, new_epoch, stream_id, reservation)
                    .await
            }
            Ok(Err(ClaimError::Conflict)) => {
                self.cancel_reclaimed_claim_capacity(entity, reservation);
                Ok(CrossNodeResumeOutcome::NotFound)
            }
            Ok(Err(other)) => {
                self.cancel_reclaimed_claim_capacity(entity, reservation);
                Err(SmRegistryError::Internal(format!(
                    "attempt_cross_node_resume: steal_for_resume failed: {other}"
                )))
            }
            Err(_timeout) => {
                self.defer_uncertain_reclaimed_claim(entity, &me, reservation);
                Err(SmRegistryError::Internal(
                    "attempt_cross_node_resume: steal_for_resume timed out; outcome retained for non-replaying reconciliation".to_string(),
                ))
            }
        }
    }

    /// Branch 4's (FIX C) terminal acquire: no foreign claim exists, so
    /// there is nothing to steal — a plain, self-reacquire-idempotent
    /// `ensure_claimed` is enough. `AlreadyClaimed` means another actor won
    /// a fresh claim on this entity between `prepare_cross_node_resume`'s
    /// `current_claim` miss and this call — a clean `NotFound`, letting the
    /// caller retry fresh rather than fight over it here. On success, hands
    /// off to the SAME [`Self::complete_local_claim`] the steal path uses —
    /// FIX B's repair covers this path's post-win failures identically.
    ///
    /// ADR-0017 Phase 3 Slice 10 FIX 3 (council-adjudicated): `Draining`
    /// folds into the SAME `NotFound` arm as `AlreadyClaimed`, not the
    /// `Err(Internal)` catch-all below. This node refusing a NEW claim
    /// because it is marked draining is, from the resuming client's point
    /// of view, indistinguishable from "someone else already owns this" —
    /// a benign "try again elsewhere" signal (surfaced as the conformant
    /// XEP-0198 `<failed><item-not-found/></failed>` by this method's
    /// caller), never an `<internal-server-error/>`. Mirrors
    /// `pending_delivery::database::claim_error_to_pending_storage_error`
    /// and `sm_persistence_fenced.rs`'s identical
    /// `AlreadyClaimed | Conflict | Draining` grouping.
    async fn finish_direct_acquire(
        &self,
        entity: &Entity,
        stream_id: &str,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        let me = self.node_identity.current();
        let Some(reservation) = self.reserve_reclaimed_claim_capacity(entity) else {
            return Err(SmRegistryError::Internal(
                "attempt_cross_node_resume: exact ownership capacity exhausted".to_string(),
            ));
        };
        match tokio::time::timeout(
            FINISH_STEAL_TIMEOUT,
            self.claim_store.ensure_claimed(entity, &me),
        )
        .await
        {
            Ok(Ok(epoch)) => {
                self.complete_local_claim(entity, me, epoch, stream_id, reservation)
                    .await
            }
            Ok(Err(ClaimError::AlreadyClaimed | ClaimError::Draining)) => {
                self.cancel_reclaimed_claim_capacity(entity, reservation);
                Ok(CrossNodeResumeOutcome::NotFound)
            }
            Ok(Err(other)) => {
                self.cancel_reclaimed_claim_capacity(entity, reservation);
                Err(SmRegistryError::Internal(format!(
                    "attempt_cross_node_resume: FIX C direct-acquire ensure_claimed failed: {other}"
                )))
            }
            Err(_timeout) => {
                self.defer_uncertain_reclaimed_claim(entity, &me, reservation);
                Err(SmRegistryError::Internal(
                    "attempt_cross_node_resume: direct acquire timed out; outcome retained for non-replaying reconciliation".to_string(),
                ))
            }
        }
    }

    /// Shared post-win tail for both [`Self::finish_steal`] and
    /// [`Self::finish_direct_acquire`]: hydrate the just-(re)claimed entity,
    /// then self-claim it. Every failure from here on — an error OR an
    /// internal bound expiring — routes to
    /// [`Self::repair_failed_local_claim`] (FIX B) rather than surfacing as
    /// a bare error that leaves the claim self-owned and un-hydrated.
    async fn complete_local_claim(
        &self,
        entity: &Entity,
        owner: NodeIdentity,
        epoch: ClaimEpoch,
        stream_id: &str,
        reservation: super::ReclaimedClaimReservation,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        let fence = super::super::persistence::SmClaimFence::new(owner, epoch);
        let hydration = match tokio::time::timeout(
            FINISH_HYDRATE_TIMEOUT,
            self.hydrate_reclaimed_typed(entity, &fence, reservation),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => {
                return self
                    .repair_failed_local_claim(
                        entity,
                        &fence,
                        stream_id,
                        reservation,
                        MissingRepairSource::Error,
                        format!("hydrate_reclaimed errored: {error}"),
                    )
                    .await;
            }
            Err(_timeout) => {
                return self
                    .repair_failed_local_claim(
                        entity,
                        &fence,
                        stream_id,
                        reservation,
                        MissingRepairSource::Error,
                        format!("hydrate_reclaimed timed out after {FINISH_HYDRATE_TIMEOUT:?}"),
                    )
                    .await;
            }
        };
        match hydration {
            super::ReclaimedHydrationOutcome::Hydrated => {}
            super::ReclaimedHydrationOutcome::LostClaim => {
                return self
                    .repair_failed_local_claim(
                        entity,
                        &fence,
                        stream_id,
                        reservation,
                        MissingRepairSource::NotFound,
                        "hydrate_reclaimed definitively lost the claim".to_string(),
                    )
                    .await;
            }
            outcome => {
                return self
                    .repair_failed_local_claim(
                        entity,
                        &fence,
                        stream_id,
                        reservation,
                        MissingRepairSource::Error,
                        format!("hydrate_reclaimed returned {outcome:?}"),
                    )
                    .await;
            }
        }

        match tokio::time::timeout(FINISH_CLAIM_TIMEOUT, self.claim_session_typed(stream_id)).await
        {
            Ok(Ok(super::claims::ClaimSessionOutcome::Claimed(session))) => {
                Ok(CrossNodeResumeOutcome::Claimed(session))
            }
            // Hydrated successfully but the session is now expired — a
            // legitimate race against its own resume window, matching
            // `claim_session`'s own semantics for this case. NOT a
            // post-win failure: the claim correctly stays held by this
            // node, backing the in-memory copy `hydrate_reclaimed` just
            // inserted, for the janitor's ordinary drain/promote/confirm
            // chain to release in due course. No repair.
            Ok(Ok(super::claims::ClaimSessionOutcome::MissingOrExpired)) => {
                Ok(CrossNodeResumeOutcome::NotFound)
            }
            Ok(Ok(super::claims::ClaimSessionOutcome::LostClaim)) => {
                self.repair_failed_local_claim(
                    entity,
                    &fence,
                    stream_id,
                    reservation,
                    MissingRepairSource::NotFound,
                    "claim_session definitively lost exact ownership post-hydrate".to_string(),
                )
                .await
            }
            Ok(Err(error)) => {
                self.repair_failed_local_claim(
                    entity,
                    &fence,
                    stream_id,
                    reservation,
                    MissingRepairSource::Error,
                    format!("claim_session errored post-hydrate: {error}"),
                )
                .await
            }
            Err(_timeout) => {
                self.repair_failed_local_claim(
                    entity,
                    &fence,
                    stream_id,
                    reservation,
                    MissingRepairSource::Error,
                    format!("claim_session timed out after {FINISH_CLAIM_TIMEOUT:?} post-hydrate"),
                )
                .await
            }
        }
    }

    /// FIX B: undo a post-win `hydrate_reclaimed`/`claim_session` failure
    /// so this entity is not left wedged — self-owned, un-hydrated, under a
    /// FRESH lease the orphan reaper's `OwnerStale` predicate can never
    /// fire against.
    ///
    /// Forgets any local in-memory trace first (best-effort, matches
    /// `forget_claim_locally`'s own no-release contract — the `ClaimStore`
    /// release below is this function's own job, done unconditionally so
    /// nothing forgotten locally is ever left dangling in the backing
    /// store). Then releases the just-won claim, retried a bounded few
    /// times: the release is the PRIMARY path back to sanity here, not a
    /// belt-and-suspenders alongside some other recovery — a live node's
    /// own fresh lease means no other mechanism will ever reclaim this
    /// entity. Every attempt (success or failure) is logged; total failure
    /// after every retry is a loud `error!` plus a typed `Err` naming
    /// exactly what happened, so an operator has both the failed original
    /// operation and the failed repair in one message.
    ///
    /// On success, returns `Ok(NotFound)` — matching `claim_session`'s own
    /// "nothing here" semantics — rather than surfacing the original
    /// failure as an error: a repaired claim means the entity is once again
    /// unclaimed-but-persisted (branch 4/FIX C), which the client's very
    /// next resume retry can walk straight into.
    async fn prepare_failed_local_claim_release(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        stream_id: &str,
        reservation: super::ReclaimedClaimReservation,
    ) -> Result<bool, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        if !self.transfer_reclaimed_claim_to_exact_release(entity, fence, reservation)? {
            return Ok(false);
        }
        self.forget_claim_locally_locked(stream_id, Some(fence));
        Ok(true)
    }

    async fn repair_failed_local_claim(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        stream_id: &str,
        reservation: super::ReclaimedClaimReservation,
        missing_source: MissingRepairSource,
        reason: String,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        if !self
            .prepare_failed_local_claim_release(entity, fence, stream_id, reservation)
            .await?
        {
            if missing_source == MissingRepairSource::NotFound {
                return Ok(CrossNodeResumeOutcome::NotFound);
            }
            return Err(SmRegistryError::Internal(format!(
                "attempt_cross_node_resume: post-win repair could not transfer {stream_id} \
                 into exact-release inventory after {reason}; reclaimed responsibility retained"
            )));
        }

        for attempt in 1..=REPAIR_RELEASE_MAX_ATTEMPTS {
            match tokio::time::timeout(
                REPAIR_RELEASE_TIMEOUT,
                self.claim_store
                    .release(entity, fence.owner(), fence.epoch()),
            )
            .await
            {
                Ok(Ok(())) => {
                    self.complete_terminal_claim_release(stream_id, fence);
                    tracing::warn!(
                        stream_id = %stream_id,
                        entity = %entity,
                        reason = %reason,
                        attempt,
                        "attempt_cross_node_resume: repaired a post-win hydrate/claim failure \
                         by releasing the just-won claim (FIX B) — a client retry can now \
                         succeed via the unclaimed-but-persisted resume branch (FIX C)"
                    );
                    return Ok(match missing_source {
                        MissingRepairSource::NotFound => CrossNodeResumeOutcome::NotFound,
                        MissingRepairSource::Error => CrossNodeResumeOutcome::StorageUnavailable,
                    });
                }
                Ok(Err(error)) => {
                    tracing::warn!(
                        stream_id = %stream_id,
                        entity = %entity,
                        %error,
                        attempt,
                        "attempt_cross_node_resume: post-win repair release attempt failed"
                    );
                }
                Err(_timeout) => {
                    tracing::warn!(
                        stream_id = %stream_id,
                        entity = %entity,
                        attempt,
                        timeout = ?REPAIR_RELEASE_TIMEOUT,
                        "attempt_cross_node_resume: post-win repair release attempt timed out"
                    );
                }
            }
            if attempt < REPAIR_RELEASE_MAX_ATTEMPTS {
                tokio::time::sleep(REPAIR_RELEASE_RETRY_DELAY).await;
            }
        }
        tracing::error!(
            stream_id = %stream_id,
            entity = %entity,
            reason = %reason,
            attempts = REPAIR_RELEASE_MAX_ATTEMPTS,
            "attempt_cross_node_resume: post-win repair release FAILED after every retry — \
             exact terminal-release inventory remains queued for janitor retry because the \
             orphan reaper cannot steal a claim held under this live node's fresh lease"
        );
        Err(SmRegistryError::Internal(format!(
            "attempt_cross_node_resume: post-win repair failed for {stream_id} after {reason} \
             ({REPAIR_RELEASE_MAX_ATTEMPTS} release attempt(s) exhausted)"
        )))
    }

    /// Council-adjudicated FIX 6: the two terminal conditions on
    /// held-response window expiry (element 8's owner-unreachable branch),
    /// distinguished by one final, unlocked [`crate::ownership::ClaimStore::current_claim`]
    /// re-check — never retried further. XEP-0198's own text and examples
    /// only demonstrate `<failed/>` `item-not-found` for "the session is
    /// known gone"; this plan's own `resource-constraint` choice (see the
    /// module's XEP fact-check note in the phase plan) is reserved for the
    /// genuinely distinct "owner still fresh, just unreachable for the
    /// whole window" case:
    ///
    /// - The claim is gone entirely, or its owner's own lease has since
    ///   expired ([`crate::ownership::ClaimSnapshot::owner_lease_fresh`] is
    ///   `false`) — the session is known gone: `<failed/>` `item-not-found`.
    /// - The claim still exists and its owner's lease is still fresh — the
    ///   owner is merely unreachable, not gone: `<failed/>`
    ///   `resource-constraint`.
    ///
    /// Deliberately takes no `observed_epoch` — whichever claim is on file
    /// *right now* (even if a different actor has since won it) is what
    /// decides between the two terminal conditions; this call never
    /// re-attempts a CAS, so there is nothing to fence against.
    async fn classify_window_expiry(
        &self,
        entity: &Entity,
    ) -> Result<CrossNodeResumeOutcome, SmRegistryError> {
        match self
            .claim_store
            .current_claim(entity)
            .await
            .map_err(|e| match e {
                // Poisoned = persistently broken store: storage health,
                // same classification as the ownership decorator.
                crate::ownership::ClaimError::Backend(_)
                | crate::ownership::ClaimError::Poisoned => {
                    SmRegistryError::StorageUnavailable(super::traits::StorageOutageCause::Backend)
                }
                other => SmRegistryError::Internal(other.to_string()),
            })? {
            None => Ok(CrossNodeResumeOutcome::NotFound),
            Some(snapshot) if !snapshot.owner_lease_fresh => Ok(CrossNodeResumeOutcome::NotFound),
            Some(_) => Ok(CrossNodeResumeOutcome::OwnerUnreachable),
        }
    }

    async fn load_persisted_snapshot(
        &self,
        stream_id: &str,
    ) -> Result<Option<PersistedSession>, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(None);
        };
        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
        // Durable-session reads are storage-backed by definition; a failed
        // read during a cross-node resume is a database incident, not a
        // registry logic error.
        storage.get_session(&session_id).await.map_err(|_e| {
            SmRegistryError::StorageUnavailable(super::traits::StorageOutageCause::Backend)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{ClaimEpoch, ClaimStore, InProcessClaimStore, SharedNodeIdentity};
    use crate::stream_management::persistence::{
        InMemorySmPersistence, PersistedUnackedStanza, SmClaimFence, SmPersistenceError,
        SmPersistenceStorage,
    };
    use crate::stream_management::session_registry::core::PendingClaimAcquisitionDisposition;
    use std::sync::Arc;

    /// Council-adjudicated FIX 1 test double: an asker that sleeps far
    /// longer than any sane handshake budget before ever answering.
    struct SlowAsker {
        delay: Duration,
        asks_seen: std::sync::atomic::AtomicUsize,
    }

    impl SlowAsker {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                asks_seen: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl RemoteResumeAsker for SlowAsker {
        async fn ask_remote_detach(
            &self,
            _node_id: &str,
            _stream_id: &str,
            _requester_bare_jid: &BareJid,
        ) -> RemoteResumeAskOutcome {
            self.asks_seen
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            RemoteResumeAskOutcome::Unreachable
        }
    }

    fn alice() -> BareJid {
        "alice@example.com".parse().expect("valid jid")
    }

    /// FIX 1: a deliberately-slow asker (its own delay is an order of
    /// magnitude longer than the configured handshake budget) must never
    /// let the total call exceed that budget by more than a small
    /// scheduling slop — every await in the retry loop is bound by the
    /// remaining budget, not by the asker's own timeout. Uses
    /// `tokio::time::pause()` so the assertion is exact and does not
    /// depend on real wall-clock scheduling.
    #[tokio::test(start_paused = true)]
    async fn fix1_slow_asker_never_holds_past_the_handshake_budget() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let owner = crate::ownership::NodeIdentity::new("owner-node", "owner-epoch");
        let entity = Entity::new(EntityType::SmSession, "stream-slow".to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let me = crate::ownership::NodeIdentity::new("resuming-node", "resuming-epoch");
        // The asker's own delay (1 hour) is wildly longer than the budget
        // (2 seconds) — if any await in the loop were unbounded, this test
        // would hang for an hour of *simulated* time and the assertion
        // below would fail (or, under real time, the test would time out).
        let asker = Arc::new(SlowAsker::new(Duration::from_secs(3600)));
        let registry = InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store, SharedNodeIdentity::new(me))
            .with_remote_resume_asker(asker.clone());

        let budget = Duration::from_secs(2);
        let started = tokio::time::Instant::now();
        let outcome = registry
            .attempt_cross_node_resume("stream-slow", &alice(), budget)
            .await
            .expect("attempt_cross_node_resume must not error");
        let elapsed = started.elapsed();

        assert!(
            matches!(outcome, CrossNodeResumeOutcome::OwnerUnreachable),
            "owner claim is present and (in-process) always reports a fresh lease"
        );
        assert!(
            elapsed <= budget + Duration::from_millis(500),
            "total hold {elapsed:?} exceeded the {budget:?} budget by more than scheduling slop"
        );
        assert!(
            asker.asks_seen.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the slow asker must actually have been asked at least once"
        );
    }

    /// FIX B test double (ADR-0017 Phase 3 Slice 11 corrigenda, council-
    /// adjudicated, deviation 110): reports `Unreachable` for its first
    /// `unreachable_calls` asks, then writes the persisted snapshot
    /// directly into the shared, in-memory persistence store the registry
    /// under test reads from — standing in for "the remote owner finally
    /// force-detached and persisted the session" — and reports `Detached`
    /// on that and every later ask. The retry loop's own subsequent
    /// iteration finds the snapshot at branch 1's top-of-loop re-check and
    /// steals it, so a well-behaved caller never needs a further ask.
    struct FlakyAsker {
        asks_seen: std::sync::atomic::AtomicUsize,
        unreachable_calls: usize,
        persistence: Arc<InMemorySmPersistence>,
        jid: jid::FullJid,
    }

    impl FlakyAsker {
        fn new(
            unreachable_calls: usize,
            persistence: Arc<InMemorySmPersistence>,
            jid: jid::FullJid,
        ) -> Self {
            Self {
                asks_seen: std::sync::atomic::AtomicUsize::new(0),
                unreachable_calls,
                persistence,
                jid,
            }
        }
    }

    #[async_trait::async_trait]
    impl RemoteResumeAsker for FlakyAsker {
        async fn ask_remote_detach(
            &self,
            _node_id: &str,
            stream_id: &str,
            _requester_bare_jid: &BareJid,
        ) -> RemoteResumeAskOutcome {
            let call_index = self
                .asks_seen
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call_index < self.unreachable_calls {
                return RemoteResumeAskOutcome::Unreachable;
            }
            self.persistence
                .upsert_session(make_persisted_session(stream_id, &self.jid))
                .await
                .expect("seed the persisted snapshot once the flaky asker finally succeeds");
            RemoteResumeAskOutcome::Detached
        }
    }

    /// FIX B (ADR-0017 Phase 3 Slice 11 corrigenda, council-adjudicated,
    /// deviation 110): branch 3 of cross-node resume (owner unreachable →
    /// HOLD + retry, succeeding on a later attempt) previously had only
    /// failure/timeout coverage in this module — `SlowAsker` (above), and
    /// every `UnreachableAsker`/`SlowAsker`-shaped double elsewhere in this
    /// crate, report `Unreachable` on EVERY call, so the bounded
    /// backoff-and-retry loop's own SUCCESS path (a later ask actually
    /// landing on branch 1's persisted-snapshot re-check) was never
    /// exercised. Mirrors
    /// `fix1_slow_asker_never_holds_past_the_handshake_budget`'s structure
    /// (single in-process `ClaimStore`, no Postgres, `start_paused = true`)
    /// but with `FlakyAsker` in place of `SlowAsker`.
    #[tokio::test(start_paused = true)]
    async fn fix_b_flaky_asker_holds_then_succeeds_on_a_later_attempt() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let owner = crate::ownership::NodeIdentity::new("owner-node", "owner-epoch");
        let entity = Entity::new(EntityType::SmSession, "stream-flaky".to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let persistence = Arc::new(InMemorySmPersistence::new());
        let me = crate::ownership::NodeIdentity::new("resuming-node", "resuming-epoch");
        // Unreachable for the first two asks — forces at least two full
        // backoff iterations (`INITIAL_HANDSHAKE_BACKOFF` = 100ms, then
        // 200ms after doubling) before the third ask finally succeeds.
        let asker = Arc::new(FlakyAsker::new(2, Arc::clone(&persistence), jid.clone()));
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::clone(&persistence) as Arc<dyn SmPersistenceStorage>)
            .with_claim_store(claim_store, SharedNodeIdentity::new(me))
            .with_remote_resume_asker(asker.clone());

        // Wide enough for several backoff iterations (100ms + 200ms + ...),
        // never exercised by the pre-existing failure-only coverage.
        let budget = Duration::from_secs(5);
        let outcome = registry
            .attempt_cross_node_resume("stream-flaky", &jid.to_bare(), budget)
            .await
            .expect("attempt_cross_node_resume must not error");

        assert!(
            matches!(outcome, CrossNodeResumeOutcome::Claimed(_)),
            "branch 3's hold-and-retry loop must succeed once a later ask actually detaches \
             the remote session; got {outcome:?}"
        );
        assert!(
            asker.asks_seen.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "the retry loop must actually have iterated at least twice before landing on the \
             successful ask, proving branch 3's hold-and-retry path (not a first-attempt \
             success) is what's under test"
        );
    }

    /// FIX 6: when the held-response window expires and the claim has
    /// disappeared entirely (the "session known gone" case), the outcome
    /// must be `NotFound` (→ `<failed/>` `item-not-found`), never
    /// `OwnerUnreachable` (→ `resource-constraint`) — distinguishing the
    /// two terminal conditions rather than collapsing them. This exercises
    /// the "claim gone" half of `classify_window_expiry` in-process (no
    /// Postgres needed); the "owner lease expired but claim still exists"
    /// half needs a real `clustering_nodes` liveness row and is covered by
    /// the Postgres-gated suite (`xep0198_cross_node_resume.rs`).
    #[tokio::test(start_paused = true)]
    async fn fix6_window_expiry_with_claim_gone_reports_not_found() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let owner = crate::ownership::NodeIdentity::new("owner-node", "owner-epoch");
        let entity = Entity::new(EntityType::SmSession, "stream-vanishing".to_string());
        let epoch = claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let me = crate::ownership::NodeIdentity::new("resuming-node", "resuming-epoch");
        // An asker that releases the claim out from under the owner on its
        // very first ask, then reports unreachable — simulating the claim
        // vanishing (e.g. a concurrent orphan-reaper GC) during the held
        // -response window.
        struct ReleasingAsker {
            claim_store: Arc<dyn ClaimStore>,
            entity: Entity,
            owner: crate::ownership::NodeIdentity,
            epoch: ClaimEpoch,
        }
        #[async_trait::async_trait]
        impl RemoteResumeAsker for ReleasingAsker {
            async fn ask_remote_detach(
                &self,
                _node_id: &str,
                _stream_id: &str,
                _requester_bare_jid: &BareJid,
            ) -> RemoteResumeAskOutcome {
                let _ = self
                    .claim_store
                    .release(&self.entity, &self.owner, self.epoch)
                    .await;
                RemoteResumeAskOutcome::Unreachable
            }
        }
        let asker = Arc::new(ReleasingAsker {
            claim_store: Arc::clone(&claim_store),
            entity: entity.clone(),
            owner,
            epoch,
        });
        let registry = InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store, SharedNodeIdentity::new(me))
            .with_remote_resume_asker(asker);

        let outcome = registry
            .attempt_cross_node_resume("stream-vanishing", &alice(), Duration::from_millis(300))
            .await
            .expect("attempt_cross_node_resume must not error");
        assert!(
            matches!(outcome, CrossNodeResumeOutcome::NotFound),
            "claim vanished mid-window: must report NotFound (item-not-found), not \
             OwnerUnreachable (resource-constraint); got {outcome:?}"
        );
    }

    /// Persistence test double (FIX A regression guard, scenario 3): every
    /// method delegates to a real [`InMemorySmPersistence`], except
    /// `get_session`, which sleeps for `delay` first from its SECOND call
    /// onward. Under a paused clock (`#[tokio::test(start_paused = true)]`)
    /// that sleep auto-advances virtual time by `delay` without costing any
    /// real wall-clock time — letting a test cheaply simulate "the
    /// handshake budget has long since expired by the time hydration
    /// completes."
    ///
    /// The first call is deliberately NOT delayed: it is
    /// `prepare_cross_node_resume`'s own branch-1 snapshot read (still
    /// budget-bound by design — that part of the sequence is meant to stay
    /// cancellable/timeout-safe, see this module's "Cancellation boundary"
    /// doc section), and delaying it too would just make the call
    /// classify as owner-unreachable before ever reaching the CAS. The
    /// SECOND call is `hydrate_reclaimed`'s own read, issued only after
    /// the CAS has already won — exactly the call this test needs to
    /// outlast the nominal budget.
    struct DelayedGetSessionPersistence {
        inner: InMemorySmPersistence,
        delay: Duration,
        corrupt_after_first: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SmPersistenceStorage for DelayedGetSessionPersistence {
        async fn upsert_session(
            &self,
            session: PersistedSession,
        ) -> Result<(), SmPersistenceError> {
            self.inner.upsert_session(session).await
        }

        async fn get_session(
            &self,
            stream_id: &crate::pending_delivery::SmSessionId,
        ) -> Result<Option<PersistedSession>, SmPersistenceError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.corrupt_after_first && call >= 1 {
                return Err(SmPersistenceError::Corrupt {
                    stream_id: stream_id.clone(),
                    detail: "injected cross-node poison row".to_string(),
                });
            }
            if call >= 1 {
                tokio::time::sleep(self.delay).await;
            }
            self.inner.get_session(stream_id).await
        }

        async fn delete_session(
            &self,
            stream_id: &crate::pending_delivery::SmSessionId,
        ) -> Result<(), SmPersistenceError> {
            self.inner.delete_session(stream_id).await
        }

        async fn append_unacked(
            &self,
            stanza: PersistedUnackedStanza,
        ) -> Result<(), SmPersistenceError> {
            self.inner.append_unacked(stanza).await
        }

        async fn ack_through(
            &self,
            stream_id: &crate::pending_delivery::SmSessionId,
            up_to_sequence: u32,
        ) -> Result<u64, SmPersistenceError> {
            self.inner.ack_through(stream_id, up_to_sequence).await
        }

        async fn delete_unacked(
            &self,
            stream_id: &crate::pending_delivery::SmSessionId,
            sequences: &[u32],
        ) -> Result<u64, SmPersistenceError> {
            self.inner.delete_unacked(stream_id, sequences).await
        }

        async fn list_unacked(
            &self,
            stream_id: &crate::pending_delivery::SmSessionId,
        ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
            self.inner.list_unacked(stream_id).await
        }

        async fn list_expired_sessions(
            &self,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
            self.inner.list_expired_sessions(now).await
        }

        async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
            self.inner.list_all_sessions().await
        }

        async fn store_session_atomic_with_principal(
            &self,
            principal: &crate::auth::AuthenticatedPrincipalRef,
            session: PersistedSession,
            unacked: Vec<PersistedUnackedStanza>,
        ) -> Result<(), SmPersistenceError> {
            self.inner
                .store_session_atomic_with_principal(principal, session, unacked)
                .await
        }

        async fn get_session_principal(
            &self,
            stream_id: &crate::pending_delivery::SmSessionId,
        ) -> Result<Option<crate::auth::AuthenticatedPrincipalRef>, SmPersistenceError> {
            self.inner.get_session_principal(stream_id).await
        }
    }

    fn make_persisted_session(stream_id: &str, jid: &jid::FullJid) -> PersistedSession {
        PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
            user_id: jid.to_bare().to_string(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 1,
            outbound_count: 1,
            last_acked: 1,
            replay_gap_through: None,
            max_resume_time: Some(300),
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        }
    }

    /// A minimal in-memory `DetachedSession` used only to pre-seed
    /// `sessions` and force `hydrate_reclaimed`'s "already present" skip
    /// path (`fix_b_post_win_hydrate_failure_repairs_and_fix_c_retry_succeeds`)
    /// — its field values are otherwise unused by that test.
    fn make_test_placeholder_session(stream_id: &str, jid: &jid::FullJid) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        }
    }

    /// FIX A regression guard (test scenario 3, prompt's numbering): the
    /// terminal CAS→hydrate→claim sequence must complete even when the
    /// caller's `handshake_budget` has long since (virtually) expired by
    /// the time hydration finishes — proving `finish_cross_node_steal` no
    /// longer consults `handshake_budget`/the prepare-phase deadline at
    /// all, closing the exact gap deviation 47's "no committed steal can
    /// be lost" claim missed (a budget-bound drop mid-sequence). Uses a
    /// paused clock so the assertion is exact and costs no real wall time:
    /// `DelayedGetSessionPersistence` sleeps 10x the nominal
    /// `handshake_budget` inside the post-CAS-win `hydrate_reclaimed` call
    /// (its own internal `get_session` read), auto-advancing virtual time
    /// well past the budget's deadline while `finish_cross_node_steal` is
    /// still mid-flight.
    #[tokio::test(start_paused = true)]
    async fn fix_a_budget_expiry_mid_finish_does_not_drop_the_sequence() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let owner = crate::ownership::NodeIdentity::new("owner-node", "owner-epoch");
        let entity = Entity::new(EntityType::SmSession, "stream-budget-expiry".to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let persistence = DelayedGetSessionPersistence {
            inner: InMemorySmPersistence::new(),
            // Comfortably longer than `tiny_budget` below (100x), but
            // still comfortably inside `FINISH_HYDRATE_TIMEOUT` — the
            // whole point is that virtual time blows past the OLD,
            // budget-derived bound while staying inside the NEW,
            // independent one, and the call must still succeed. Under
            // the paused clock this costs no real time, only virtual
            // time.
            delay: Duration::from_secs(5),
            corrupt_after_first: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        persistence
            .inner
            .upsert_session(make_persisted_session("stream-budget-expiry", &jid))
            .await
            .expect("seed the persisted snapshot");

        let me = crate::ownership::NodeIdentity::new("resuming-node", "resuming-epoch");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(persistence))
            .with_claim_store(claim_store, SharedNodeIdentity::new(me));

        // Deliberately much shorter than the persistence's own delay: by
        // the OLD design (CAS+hydrate bound by the budget's own remaining
        // time) this would abort the sequence with an internal error
        // before hydration ever finished. The new design must succeed
        // regardless, because `finish_cross_node_steal` no longer
        // consults this budget once `prepare_cross_node_resume` has
        // handed off a `ReadyToSteal` ticket.
        let tiny_budget = Duration::from_millis(50);
        let outcome = registry
            .attempt_cross_node_resume("stream-budget-expiry", &jid.to_bare(), tiny_budget)
            .await
            .expect(
                "finish_cross_node_steal must not error even though the nominal budget \
                     expired mid-sequence",
            );
        assert!(
            matches!(outcome, CrossNodeResumeOutcome::Claimed(_)),
            "expected Claimed despite the budget expiring mid-finish; got {outcome:?}"
        );
    }

    /// A post-win repair must publish exact terminal inventory before its
    /// first backend release attempt. Leaving that inventory in place models
    /// cancellation or exhaustion of every inline attempt; the janitor still
    /// owns a bounded retry.
    #[tokio::test]
    async fn repair_transfer_retains_exact_release_until_backend_success() {
        let registry = InMemorySmSessionRegistry::with_capacity(1);
        let entity = Entity::new(EntityType::SmSession, "repair-release-retry".to_string());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve reclaimed repair capacity");
        let fence = SmClaimFence::new(
            crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation"),
            ClaimEpoch(7),
        );

        assert!(registry
            .prepare_failed_local_claim_release(&entity, &fence, &entity.id, reservation,)
            .await
            .expect("prepare repair release"));
        assert_eq!(registry.pending_claim_release_count(), 1);
        assert_eq!(registry.claim_fence_capacity_used(), 1);
        assert!(
            registry
                .reserve_reclaimed_claim_capacity(&Entity::new(
                    EntityType::SmSession,
                    "another-repair".to_string(),
                ))
                .is_none(),
            "a failed inline release must remain capacity-counted for janitor retry"
        );

        registry.complete_terminal_claim_release(&entity.id, &fence);
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert_eq!(registry.claim_fence_capacity_used(), 0);
    }

    #[tokio::test]
    async fn repair_demotion_clears_promotion_parked_after_exact_release_transfer() {
        let registry = InMemorySmSessionRegistry::with_capacity(1);
        let entity = Entity::new(
            EntityType::SmSession,
            "repair-demotion-clears-late-promotion".to_string(),
        );
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve reclaimed repair capacity");
        let fence = SmClaimFence::new(
            crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation"),
            ClaimEpoch(7),
        );
        assert!(registry
            .transfer_reclaimed_claim_to_exact_release(&entity, &fence, reservation)
            .expect("transfer repair fence to exact release"));

        // The cancelled-resume guard does not take the stream shard. Model
        // it winning after the repair transfer released its map locks but
        // before `prepare_failed_local_claim_release` forgets local state.
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let mut session = make_test_placeholder_session(&entity.id, &jid);
        session.max_resume_time = Some(1);
        session.detached_at = std::time::Instant::now() - Duration::from_secs(120);
        registry
            .claimed_sessions
            .write()
            .expect("claimed sessions")
            .insert(entity.id.clone(), session);
        assert!(registry.defer_claimed_resume_release(&entity.id));

        let stream_lock = registry.stream_lock(&entity.id).expect("stream lock");
        let _stream_guard = stream_lock.lock().await;
        registry.forget_claim_locally_locked(&entity.id, Some(&fence));

        assert!(registry
            .drain_expired()
            .await
            .expect("drain after repair demotion")
            .is_empty());
        assert!(registry
            .live_session_ids()
            .expect("live inventory after repair demotion")
            .is_empty());
        assert_eq!(
            registry.pending_claim_release_count(),
            1,
            "repair demotion must preserve its exact terminal release responsibility"
        );
        assert!(
            !registry.defer_claimed_resume_release(&entity.id),
            "a late cancelled-resume transition must observe Missing and not re-park"
        );
    }

    #[tokio::test]
    async fn definitive_lost_claim_returns_not_found_without_repair() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let entity = Entity::new(EntityType::SmSession, "definitive-lost-claim".to_string());
        let foreign = crate::ownership::NodeIdentity::new("foreign-node", "foreign-incarnation");
        claim_store
            .acquire(&entity, &foreign)
            .await
            .expect("foreign claim");
        let me = crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation");
        let registry = InMemorySmSessionRegistry::with_capacity(1).with_claim_store(
            Arc::clone(&claim_store),
            SharedNodeIdentity::new(me.clone()),
        );
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve attempted local claim");

        let outcome = registry
            .complete_local_claim(&entity, me, ClaimEpoch(999), &entity.id, reservation)
            .await
            .expect("definitive ownership loss is not an internal repair error");
        assert!(matches!(outcome, CrossNodeResumeOutcome::NotFound));
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert_eq!(registry.claim_fence_capacity_used(), 0);
    }

    #[tokio::test]
    async fn post_verification_lost_claim_retires_its_active_fence() {
        let registry = InMemorySmSessionRegistry::with_capacity(1);
        let entity = Entity::new(EntityType::SmSession, "verified-lost-claim".to_string());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve reclaimed claim");
        let fence = SmClaimFence::new(crate::ownership::NodeIdentity::local(), ClaimEpoch(7));
        assert!(registry.try_record_verified_reclaimed_fence(
            &entity.id,
            fence.clone(),
            reservation,
        ));

        let outcome = registry
            .repair_failed_local_claim(
                &entity,
                &fence,
                &entity.id,
                reservation,
                MissingRepairSource::NotFound,
                "claim lost after exact fence publication".to_string(),
            )
            .await
            .expect("post-verification ownership loss cleanup");
        assert!(matches!(outcome, CrossNodeResumeOutcome::NotFound));
        assert!(!registry
            .claim_fences
            .read()
            .expect("claim fences")
            .contains_key(&entity.id));
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert_eq!(registry.claim_fence_capacity_used(), 0);
    }

    #[tokio::test]
    async fn lost_claim_bookkeeping_failure_never_collapses_to_not_found() {
        let registry = InMemorySmSessionRegistry::with_capacity(1);
        let entity = Entity::new(EntityType::SmSession, "poisoned-lost-claim".to_string());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve reclaimed claim");
        let fence = SmClaimFence::new(crate::ownership::NodeIdentity::local(), ClaimEpoch(7));
        assert!(registry.try_record_verified_reclaimed_fence(
            &entity.id,
            fence.clone(),
            reservation,
        ));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pending = registry
                .pending_claim_releases
                .write()
                .expect("pending releases");
            panic!("inject terminal inventory lock failure");
        }));

        let result = registry
            .repair_failed_local_claim(
                &entity,
                &fence,
                &entity.id,
                reservation,
                MissingRepairSource::NotFound,
                "claim lost after exact fence publication".to_string(),
            )
            .await;
        assert!(
            matches!(result, Err(SmRegistryError::Internal(_))),
            "bookkeeping failure is not proof that the exact repair source is absent"
        );
        assert_eq!(
            registry
                .claim_fences
                .read()
                .expect("claim fences")
                .get(&entity.id),
            Some(&fence)
        );
        assert_eq!(registry.claim_fence_capacity_used(), 1);
    }

    #[tokio::test]
    async fn poison_quarantine_repairs_the_fresh_claim_and_capacity() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let entity = Entity::new(
            EntityType::SmSession,
            "poison-cross-node-repair".to_string(),
        );
        let foreign = crate::ownership::NodeIdentity::new("foreign-node", "foreign-incarnation");
        claim_store
            .acquire(&entity, &foreign)
            .await
            .expect("foreign claim");
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let persistence = Arc::new(DelayedGetSessionPersistence {
            inner: InMemorySmPersistence::new(),
            delay: Duration::ZERO,
            corrupt_after_first: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        persistence
            .inner
            .upsert_session(make_persisted_session(&entity.id, &jid))
            .await
            .expect("seed poison target");
        let me = crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation");
        let registry = InMemorySmSessionRegistry::with_capacity(1)
            .with_persistence(Arc::clone(&persistence) as Arc<dyn SmPersistenceStorage>)
            .with_claim_store(Arc::clone(&claim_store), SharedNodeIdentity::new(me));

        let outcome = registry
            .attempt_cross_node_resume(&entity.id, &jid.to_bare(), Duration::from_secs(2))
            .await
            .expect("poison quarantine must repair rather than strand its fresh claim");
        // Corrupt session data is a storage-class failure: the durable row
        // exists, so the repair must NOT masquerade as absence
        // (`item-not-found`); it surfaces as retryable StorageUnavailable.
        assert!(matches!(
            outcome,
            CrossNodeResumeOutcome::StorageUnavailable
        ));
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("current claim")
            .is_none());
        assert!(persistence
            .inner
            .get_session(&crate::pending_delivery::SmSessionId::new(
                entity.id.clone()
            ))
            .await
            .expect("quarantined session lookup")
            .is_none());
        assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert_eq!(registry.claim_fence_capacity_used(), 0);
    }

    #[tokio::test]
    async fn post_hydrate_claim_loss_forgets_the_stale_local_lifecycle() {
        let store = Arc::new(CommitThenHangClaimStore {
            inner: InProcessClaimStore::new(),
            hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
            hang_steal_once: std::sync::atomic::AtomicBool::new(false),
            ensure_calls: std::sync::atomic::AtomicUsize::new(0),
            // `complete_local_claim` first verifies ownership while
            // hydrating, then self-ensures once more while moving the
            // detached session into the claimed map. Steal between those
            // two operations.
            steal_on_ensure_call: Some(2),
            commit_then_error_on_ensure_call: None,
        });
        let entity = Entity::new(EntityType::SmSession, "post-hydrate-claim-loss");
        let me = NodeIdentity::new("resume-node", "resume-incarnation");
        let epoch = store
            .acquire(&entity, &me)
            .await
            .expect("seed resume-node claim");
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let persistence = InMemorySmPersistence::new();
        persistence
            .upsert_session(make_persisted_session(&entity.id, &jid))
            .await
            .expect("seed durable session");
        let registry = InMemorySmSessionRegistry::with_capacity(1)
            .with_persistence(Arc::new(persistence))
            .with_claim_store(
                Arc::clone(&store) as Arc<dyn ClaimStore>,
                SharedNodeIdentity::new(me.clone()),
            );
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve reclaimed claim");

        let outcome = registry
            .complete_local_claim(&entity, me, epoch, &entity.id, reservation)
            .await
            .expect("definitive post-hydrate loss is repaired");

        assert!(matches!(outcome, CrossNodeResumeOutcome::NotFound));
        let current = store
            .current_claim(&entity)
            .await
            .expect("current claim")
            .expect("foreign claim survives stale exact release");
        assert_eq!(current.owner.node_id, "post-hydrate-stealer");
        assert!(!registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key(&entity.id));
        assert!(!registry
            .claimed_sessions
            .read()
            .expect("claimed sessions")
            .contains_key(&entity.id));
        assert!(!registry
            .claim_fences
            .read()
            .expect("claim fences")
            .contains_key(&entity.id));
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert_eq!(registry.claim_fence_capacity_used(), 0);
    }

    #[tokio::test]
    async fn post_hydrate_commit_unknown_claim_retains_reconciliation_responsibility() {
        let store = Arc::new(CommitThenHangClaimStore {
            inner: InProcessClaimStore::new(),
            hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
            hang_steal_once: std::sync::atomic::AtomicBool::new(false),
            ensure_calls: std::sync::atomic::AtomicUsize::new(0),
            steal_on_ensure_call: None,
            commit_then_error_on_ensure_call: Some(2),
        });
        let entity = Entity::new(EntityType::SmSession, "post-hydrate-commit-unknown");
        let me = NodeIdentity::new("resume-node", "resume-incarnation");
        let original_epoch = store
            .acquire(&entity, &me)
            .await
            .expect("seed resume-node claim");
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let persistence = InMemorySmPersistence::new();
        persistence
            .upsert_session(make_persisted_session(&entity.id, &jid))
            .await
            .expect("seed durable session");
        let registry = InMemorySmSessionRegistry::with_capacity(2)
            .with_persistence(Arc::new(persistence))
            .with_claim_store(
                Arc::clone(&store) as Arc<dyn ClaimStore>,
                SharedNodeIdentity::new(me.clone()),
            );
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve reclaimed claim");

        registry
            .complete_local_claim(&entity, me.clone(), original_epoch, &entity.id, reservation)
            .await
            .expect_err("commit-unknown tail must remain pending, not report clean loss");

        assert!(registry.has_claim_fence_reservation(&entity.id));
        assert!(registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisitions")
            .contains(&(
                entity.id.clone(),
                me,
                super::super::core::PendingClaimAcquisitionDisposition::RetainDetachedSession,
            )));
        let committed = store
            .current_claim(&entity)
            .await
            .expect("current claim")
            .expect("fresh claim committed before the lost response");
        assert!(committed.claim_epoch > original_epoch);

        registry.retry_pending_claim_releases(2).await;
        assert!(!registry.has_claim_fence_reservation(&entity.id));
        assert!(registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisitions")
            .iter()
            .all(|(stream_id, _, _)| stream_id != &entity.id));
        assert_eq!(
            registry
                .claim_fences
                .read()
                .expect("claim fences")
                .get(&entity.id)
                .map(SmClaimFence::epoch),
            Some(committed.claim_epoch)
        );
        assert!(registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key(&entity.id));
        assert!(registry
            .claim_session(&entity.id)
            .await
            .expect("reconciled claim")
            .is_some());
    }

    #[tokio::test]
    async fn stale_repair_never_forgets_a_newer_active_lifecycle() {
        let registry = InMemorySmSessionRegistry::with_capacity(2);
        let entity = Entity::new(EntityType::SmSession, "stale-repair".to_string());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve old reclaimed generation");
        let owner = crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation");
        let old_fence = SmClaimFence::new(owner.clone(), ClaimEpoch(7));
        assert!(registry
            .transfer_reclaimed_claim_to_exact_release(&entity, &old_fence, reservation,)
            .expect("transfer old repair fence"));

        let newer_fence = SmClaimFence::new(owner, ClaimEpoch(8));
        registry
            .claim_fences
            .write()
            .expect("claim fences")
            .insert(entity.id.clone(), newer_fence.clone());
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        registry.sessions.write().expect("sessions").insert(
            entity.id.clone(),
            make_test_placeholder_session(&entity.id, &jid),
        );

        assert!(
            !registry
                .prepare_failed_local_claim_release(&entity, &old_fence, &entity.id, reservation,)
                .await
                .expect("stale repair preparation"),
            "an old pending release cannot authorize forgetting a newer lifecycle"
        );
        assert!(registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key(&entity.id));
        assert_eq!(
            registry
                .claim_fences
                .read()
                .expect("claim fences")
                .get(&entity.id),
            Some(&newer_fence)
        );
        assert!(registry
            .pending_claim_releases
            .read()
            .expect("pending releases")
            .contains(&(entity.id.clone(), old_fence)));
    }

    #[tokio::test]
    async fn stale_repair_never_forgets_a_reserved_detach_lifecycle() {
        let registry = InMemorySmSessionRegistry::with_capacity(2);
        let entity = Entity::new(EntityType::SmSession, "reserved-stale-repair".to_string());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve old reclaimed generation");
        let owner = crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation");
        let old_fence = SmClaimFence::new(owner.clone(), ClaimEpoch(7));
        assert!(registry
            .transfer_reclaimed_claim_to_exact_release(&entity, &old_fence, reservation,)
            .expect("transfer old repair fence"));
        assert!(registry.reserve_claim_fence_capacity(&entity.id));
        registry
            .pending_claim_acquisitions
            .write()
            .expect("pending acquisitions")
            .insert((
                entity.id.clone(),
                owner,
                PendingClaimAcquisitionDisposition::RetainDetachedSession,
            ));
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        registry.sessions.write().expect("sessions").insert(
            entity.id.clone(),
            make_test_placeholder_session(&entity.id, &jid),
        );

        assert!(
            !registry
                .prepare_failed_local_claim_release(&entity, &old_fence, &entity.id, reservation,)
                .await
                .expect("stale repair preparation"),
            "an old pending release cannot consume a newer detach reservation"
        );
        assert!(registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key(&entity.id));
        assert!(registry.has_claim_fence_reservation(&entity.id));
        assert!(registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisitions")
            .contains(&(
                entity.id.clone(),
                crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation"),
                PendingClaimAcquisitionDisposition::RetainDetachedSession,
            )));
        assert!(registry
            .pending_claim_releases
            .read()
            .expect("pending releases")
            .contains(&(entity.id.clone(), old_fence)));
    }

    #[tokio::test]
    async fn stale_repair_never_forgets_a_claimless_live_replacement() {
        let registry = InMemorySmSessionRegistry::with_capacity(2);
        let entity = Entity::new(EntityType::SmSession, "claimless-stale-repair".to_string());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("reserve old reclaimed generation");
        let old_fence = SmClaimFence::new(
            crate::ownership::NodeIdentity::new("repair-node", "repair-incarnation"),
            ClaimEpoch(7),
        );
        assert!(registry
            .transfer_reclaimed_claim_to_exact_release(&entity, &old_fence, reservation,)
            .expect("transfer old repair fence"));
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");
        registry.sessions.write().expect("sessions").insert(
            entity.id.clone(),
            make_test_placeholder_session(&entity.id, &jid),
        );

        assert!(
            !registry
                .prepare_failed_local_claim_release(&entity, &old_fence, &entity.id, reservation,)
                .await
                .expect("stale repair preparation"),
            "an old pending release cannot authorize forgetting a claimless live replacement"
        );
        assert!(registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key(&entity.id));
        assert!(registry
            .pending_claim_releases
            .read()
            .expect("pending releases")
            .contains(&(entity.id.clone(), old_fence)));
    }

    /// FIX B/C regression guard: an ordinary (non-cancellation)
    /// `hydrate_reclaimed` failure after the CAS has already won must
    /// repair (release the just-won claim) rather than strand it — and a
    /// subsequent resume attempt must succeed via FIX C's
    /// unclaimed-but-persisted branch, actually recovering the session,
    /// rather than dead-ending at `NotFound` forever.
    #[tokio::test(start_paused = true)]
    async fn fix_b_post_win_hydrate_failure_repairs_and_fix_c_retry_succeeds() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let owner = crate::ownership::NodeIdentity::new("owner-node", "owner-epoch");
        let entity = Entity::new(EntityType::SmSession, "stream-hydrate-fail".to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");

        // Persistence IS attached (needed for branch 1 to fire at all —
        // `current_claim` finds the owner's claim, `load_persisted_snapshot`
        // must find a row too, or the loop falls through to the
        // branch-2/3 remote ask instead of ever reaching the CAS). The row
        // stays in place the whole test: nothing here ever calls
        // `complete_claim`/`confirm_drained`, the only paths that delete it.
        let persistence = InMemorySmPersistence::new();
        persistence
            .upsert_session(make_persisted_session("stream-hydrate-fail", &jid))
            .await
            .expect("seed the persisted snapshot");

        let me = crate::ownership::NodeIdentity::new("resuming-node", "resuming-epoch");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(persistence))
            .with_claim_store(
                Arc::clone(&claim_store),
                SharedNodeIdentity::new(me.clone()),
            );

        // Ordinary (non-cancellation, non-timeout) `hydrate_reclaimed`
        // failure reproduction: pre-seed this node's OWN in-memory
        // `sessions` map with a placeholder entry for this stream id.
        // `hydrate_reclaimed` skips (returns `0` hydrated) whenever the
        // stream id is already present in `sessions`/`claimed_sessions` —
        // exactly the "sibling" this test proves, with no need for any
        // timing race: the CAS below still wins cleanly, but the
        // subsequent hydrate is an ordinary, deterministic no-op.
        registry.sessions.write().expect("sessions lock").insert(
            "stream-hydrate-fail".to_string(),
            make_test_placeholder_session("stream-hydrate-fail", &jid),
        );

        let outcome = registry
            .attempt_cross_node_resume(
                "stream-hydrate-fail",
                &jid.to_bare(),
                Duration::from_secs(2),
            )
            .await
            .expect("attempt_cross_node_resume must not error: FIX B repairs, it does not fail");
        assert!(
            matches!(outcome, CrossNodeResumeOutcome::StorageUnavailable),
            "post-win hydrate failure must repair to a retryable StorageUnavailable — the \
             durable session still exists, so it must not masquerade as item-not-found; got \
             {outcome:?}"
        );

        // FIX B's repair must have released the claim entirely — prove it
        // directly against the shared `ClaimStore` before even trying the
        // FIX C retry.
        assert!(
            claim_store
                .current_claim(&entity)
                .await
                .expect("current_claim must not error")
                .is_none(),
            "the just-won claim must have been released by FIX B's repair"
        );
        assert_eq!(
            registry.pending_claim_release_count(),
            0,
            "successful inline repair must retire its terminal retry inventory"
        );
        // ...and the repair's `forget_claim_locally` must have cleared the
        // placeholder too, or the FIX C retry below would hit the same
        // "already present" skip forever.
        assert!(
            !registry
                .sessions
                .read()
                .expect("sessions lock")
                .contains_key("stream-hydrate-fail"),
            "FIX B's repair must forget the placeholder it could not properly hydrate"
        );

        // FIX C: a second attempt now finds no claim (released above) but
        // the persisted row is still there — the direct-acquire branch
        // must actually recover the session this time (nothing is
        // pre-seeded into `sessions` anymore), proving the client's retry
        // is not just "not wedged" but genuinely successful.
        let retry_outcome = registry
            .attempt_cross_node_resume(
                "stream-hydrate-fail",
                &jid.to_bare(),
                Duration::from_secs(2),
            )
            .await
            .expect("the FIX C retry must not error");
        assert!(
            matches!(retry_outcome, CrossNodeResumeOutcome::Claimed(_)),
            "FIX C's retry must actually recover the persisted session; got {retry_outcome:?}"
        );
    }

    /// `ClaimStore` test double that delegates every method to a real
    /// [`InProcessClaimStore`], except [`ClaimStore::ensure_claimed`],
    /// which always reports [`ClaimError::Draining`] — modeling a node
    /// that is mid-graceful-drain (ADR-0017 Phase 3 Slice 10: the
    /// acquire-side draining gate refuses any NEW claim while this node
    /// is marked draining) fielding a resume request for an entity it
    /// does not already own.
    struct DrainingClaimStore {
        inner: InProcessClaimStore,
    }

    struct CommitThenHangClaimStore {
        inner: InProcessClaimStore,
        hang_ensure_once: std::sync::atomic::AtomicBool,
        hang_steal_once: std::sync::atomic::AtomicBool,
        ensure_calls: std::sync::atomic::AtomicUsize,
        steal_on_ensure_call: Option<usize>,
        commit_then_error_on_ensure_call: Option<usize>,
    }

    #[async_trait::async_trait]
    impl ClaimStore for CommitThenHangClaimStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            self.inner.ensure_schema().await
        }
        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }
        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            let call = self
                .ensure_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if self.commit_then_error_on_ensure_call == Some(call) {
                let current = self
                    .inner
                    .current_claim(entity)
                    .await?
                    .ok_or(ClaimError::Conflict)?;
                self.inner
                    .release(entity, &current.owner, current.claim_epoch)
                    .await?;
                self.inner.ensure_claimed(entity, me).await?;
                return Err(ClaimError::Backend(
                    "injected lost response after fresh claim commit".to_string(),
                ));
            }
            if self.steal_on_ensure_call == Some(call) {
                let current = self
                    .inner
                    .current_claim(entity)
                    .await?
                    .ok_or(ClaimError::Conflict)?;
                self.inner
                    .release(entity, &current.owner, current.claim_epoch)
                    .await?;
                self.inner
                    .acquire(
                        entity,
                        &NodeIdentity::new("post-hydrate-stealer", "foreign-incarnation"),
                    )
                    .await?;
            }
            let epoch = self.inner.ensure_claimed(entity, me).await?;
            if self
                .hang_ensure_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return std::future::pending().await;
            }
            Ok(epoch)
        }
        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            staleness: crate::ownership::StalePredicate,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }
        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            witness: crate::ownership::ResumeIdentityProof,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            let epoch = self
                .inner
                .steal_for_resume(entity, observed, witness, me)
                .await?;
            if self
                .hang_steal_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return std::future::pending().await;
            }
            Ok(epoch)
        }
        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<crate::ownership::ClaimSnapshot>, ClaimError> {
            self.inner.current_claim(entity).await
        }
        async fn fence(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            self.inner.fence(entity, me, mine).await
        }
        async fn release(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            self.inner.release(entity, me, mine).await
        }
        async fn release_many(
            &self,
            entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn direct_acquire_commit_before_timeout_is_reconciled_without_replay() {
        let store = Arc::new(CommitThenHangClaimStore {
            inner: InProcessClaimStore::new(),
            hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
            hang_steal_once: std::sync::atomic::AtomicBool::new(false),
            ensure_calls: std::sync::atomic::AtomicUsize::new(0),
            steal_on_ensure_call: None,
            commit_then_error_on_ensure_call: None,
        });
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
        let persistence = InMemorySmPersistence::new();
        persistence
            .upsert_session(make_persisted_session("direct-timeout", &jid))
            .await
            .expect("persist");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(persistence))
            .with_claim_store(
                store,
                SharedNodeIdentity::new(NodeIdentity::new("new-node", "incarnation")),
            );

        registry
            .attempt_cross_node_resume("direct-timeout", &jid.to_bare(), Duration::from_secs(2))
            .await
            .expect_err("the first call times out after the committed acquire");
        assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
        assert!(registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key("direct-timeout"));
    }

    #[tokio::test(start_paused = true)]
    async fn resume_steal_commit_before_timeout_is_reconciled_without_replay() {
        let store = Arc::new(CommitThenHangClaimStore {
            inner: InProcessClaimStore::new(),
            hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
            hang_steal_once: std::sync::atomic::AtomicBool::new(true),
            ensure_calls: std::sync::atomic::AtomicUsize::new(0),
            steal_on_ensure_call: None,
            commit_then_error_on_ensure_call: None,
        });
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
        let persistence = InMemorySmPersistence::new();
        persistence
            .upsert_session(make_persisted_session("steal-timeout", &jid))
            .await
            .expect("persist");
        let entity = Entity::new(EntityType::SmSession, "steal-timeout");
        store
            .acquire(&entity, &NodeIdentity::new("old-node", "incarnation"))
            .await
            .expect("old claim");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(persistence))
            .with_claim_store(
                store,
                SharedNodeIdentity::new(NodeIdentity::new("new-node", "incarnation")),
            );

        registry
            .attempt_cross_node_resume("steal-timeout", &jid.to_bare(), Duration::from_secs(2))
            .await
            .expect_err("the first call times out after the committed steal");
        assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
        assert!(registry
            .sessions
            .read()
            .expect("sessions")
            .contains_key("steal-timeout"));
    }

    #[async_trait::async_trait]
    impl ClaimStore for DrainingClaimStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            self.inner.ensure_schema().await
        }
        async fn acquire(
            &self,
            entity: &Entity,
            me: &crate::ownership::NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }
        async fn ensure_claimed(
            &self,
            _entity: &Entity,
            _me: &crate::ownership::NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            Err(ClaimError::Draining)
        }
        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            staleness: crate::ownership::StalePredicate,
            me: &crate::ownership::NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }
        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            witness: crate::ownership::ResumeIdentityProof,
            me: &crate::ownership::NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_for_resume(entity, observed, witness, me)
                .await
        }
        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<crate::ownership::ClaimSnapshot>, ClaimError> {
            self.inner.current_claim(entity).await
        }
        async fn fence(
            &self,
            entity: &Entity,
            me: &crate::ownership::NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            self.inner.fence(entity, me, mine).await
        }
        async fn release(
            &self,
            entity: &Entity,
            me: &crate::ownership::NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            self.inner.release(entity, me, mine).await
        }
        async fn release_many(
            &self,
            entities: &[Entity],
            me: &crate::ownership::NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    /// ADR-0017 Phase 3 Slice 10 FIX 3 (council-adjudicated): a resume
    /// attempt against a genuinely unclaimed-but-persisted entity
    /// (FIX C's branch 4), on a node whose `ClaimStore::ensure_claimed`
    /// reports `Draining`, must resolve to the same benign
    /// `CrossNodeResumeOutcome::NotFound` (→ conformant XEP-0198
    /// `<failed><item-not-found/></failed>`) as any other "someone else
    /// owns this"/"nothing to claim here" outcome — never surface as an
    /// `<internal-server-error/>`.
    #[tokio::test(start_paused = true)]
    async fn fix3_draining_node_direct_acquire_reports_not_found_not_internal_error() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(DrainingClaimStore {
            inner: InProcessClaimStore::new(),
        });
        let jid: jid::FullJid = "alice@example.com/phone".parse().expect("valid jid");

        // No claim exists on this entity at all (FIX C's precondition) —
        // but a persisted snapshot does, so `prepare_cross_node_resume`
        // reaches branch 4 (`DirectAcquire`) rather than falling straight
        // through to `NotFound` for lack of anything to steal.
        let persistence = InMemorySmPersistence::new();
        persistence
            .upsert_session(make_persisted_session("stream-draining", &jid))
            .await
            .expect("seed the persisted snapshot");

        let me = crate::ownership::NodeIdentity::new("draining-node", "draining-epoch");
        let registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(persistence))
            .with_claim_store(claim_store, SharedNodeIdentity::new(me));

        let outcome = registry
            .attempt_cross_node_resume("stream-draining", &jid.to_bare(), Duration::from_secs(2))
            .await
            .expect(
                "a draining node's refused acquire must resolve cleanly, never as an \
                 Err/internal-server-error",
            );
        assert!(
            matches!(outcome, CrossNodeResumeOutcome::NotFound),
            "a draining node's direct-acquire refusal must report NotFound \
             (item-not-found), not error; got {outcome:?}"
        );
    }
}
