use jid::FullJid;
use tracing::debug;

use crate::ownership::{ClaimError, Entity, EntityType};

use super::core::{
    DetachClaimFenceReservation, InMemorySmSessionRegistry, PendingClaimAcquisitionDisposition,
    PendingClaimReleaseDisposition, CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
};
use super::{DetachedSession, SmClaimCompletion, SmRegistryError};

#[derive(Debug)]
pub(super) enum ClaimSessionOutcome {
    Claimed(Box<DetachedSession>),
    MissingOrExpired,
    LostClaim,
}

/// The ownership half of a live detach is deliberately separate from the
/// snapshot result. Only `Established` proves that a remote force-detach may
/// acknowledge the session as stealable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(super) enum DetachClaimAcquisitionOutcome {
    Established,
    AmbiguousTracked,
    Rejected(DetachClaimRejection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetachClaimRejection {
    ForeignOwner,
    Draining,
    AuthorityDisabled,
    InvalidOperation,
    PublicationAuthorityLost,
}

fn definite_detach_claim_rejection(error: &ClaimError) -> Option<DetachClaimRejection> {
    match error {
        ClaimError::Backend(_) | ClaimError::Poisoned => None,
        ClaimError::AlreadyClaimed | ClaimError::Conflict => {
            Some(DetachClaimRejection::ForeignOwner)
        }
        ClaimError::Draining => Some(DetachClaimRejection::Draining),
        ClaimError::AuthorityDisabled => Some(DetachClaimRejection::AuthorityDisabled),
        ClaimError::SmSessionExcludedFromStealIntent => {
            Some(DetachClaimRejection::InvalidOperation)
        }
    }
}

/// The `ClaimStore` entity naming an SM session's ownership claim (element
/// 8: `entity_type = sm_session`, `entity = ` the SM-ID/stream id).
fn sm_session_entity(stream_id: &str) -> Entity {
    Entity::new(EntityType::SmSession, stream_id.to_string())
}

struct PendingAcquisitionGuard<'a> {
    registry: &'a InMemorySmSessionRegistry,
    stream_id: String,
    identity: crate::ownership::NodeIdentity,
    disposition: PendingClaimAcquisitionDisposition,
    armed: bool,
}

struct ClaimCompletionCancellationGuard<'a> {
    registry: &'a InMemorySmSessionRegistry,
    stream_id: String,
    generation_id: super::SmSessionGenerationId,
    armed: bool,
}

impl<'a> ClaimCompletionCancellationGuard<'a> {
    /// Restore resumability when the completion future is cancelled inside
    /// this process after its durable delete has committed. This is a
    /// process-local safety net: it deliberately does not claim crash
    /// durability for the delete-to-live handoff. A persisted `Resuming`
    /// state (or atomic current-to-terminal transition) is a separate
    /// protocol change outside this registry cleanup.
    fn new(
        registry: &'a InMemorySmSessionRegistry,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Self {
        Self {
            registry,
            stream_id: stream_id.to_string(),
            generation_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ClaimCompletionCancellationGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (Ok(mut sessions), Ok(mut claimed)) = (
            self.registry.sessions.write(),
            self.registry.claimed_sessions.write(),
        ) else {
            tracing::warn!(
                stream_id = %self.stream_id,
                "cancelled SM claim completion could not restore detached resumability"
            );
            return;
        };
        if sessions.contains_key(&self.stream_id) {
            return;
        }
        let matches = claimed
            .get(&self.stream_id)
            .is_some_and(|session| session.generation_id == self.generation_id);
        if matches {
            if let Some(session) = claimed.remove(&self.stream_id) {
                sessions.insert(self.stream_id.clone(), session);
            }
        }
    }
}

impl<'a> PendingAcquisitionGuard<'a> {
    fn new(
        registry: &'a InMemorySmSessionRegistry,
        stream_id: &str,
        identity: crate::ownership::NodeIdentity,
        disposition: PendingClaimAcquisitionDisposition,
    ) -> Self {
        Self {
            registry,
            stream_id: stream_id.to_string(),
            identity,
            disposition,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingAcquisitionGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut pending) = self.registry.pending_claim_acquisitions.write() {
            pending.insert((
                self.stream_id.clone(),
                self.identity.clone(),
                self.disposition,
            ));
        } else {
            tracing::warn!(
                stream_id = %self.stream_id,
                "cancelled SM claim acquisition could not retain ambiguous claim responsibility"
            );
        }
    }
}

struct PendingPromotionRetryLease<'a> {
    registry: &'a InMemorySmSessionRegistry,
    session: Option<DetachedSession>,
}

struct PromotionReservationGuard {
    promotions: std::sync::Arc<std::sync::RwLock<super::core::PendingPromotions>>,
    stream_id: String,
    generation_id: super::SmSessionGenerationId,
    nonce: super::SmPromotionLeaseNonce,
    armed: bool,
}

/// Once an exact durable generation has been deleted, its already-promoted
/// payload must never become retryable again. Keep this guard armed across
/// the durable-work probe and every synchronous retirement check so task
/// cancellation (or an early return) hands the exact claim to reconciliation
/// and removes only this generation's promotion carriers.
struct DeletedPromotionProbeGuard<'registry, 'lease> {
    registry: &'registry InMemorySmSessionRegistry,
    lease: &'lease mut super::SmSessionPromotionLease,
    stream_id: String,
    generation_id: super::SmSessionGenerationId,
    authority: super::SmSessionPromotionAuthority,
    claim_fence: Option<super::super::persistence::SmClaimFence>,
    nonce: super::SmPromotionLeaseNonce,
    armed: bool,
}

impl<'registry, 'lease> DeletedPromotionProbeGuard<'registry, 'lease> {
    fn new(
        registry: &'registry InMemorySmSessionRegistry,
        lease: &'lease mut super::SmSessionPromotionLease,
    ) -> Self {
        Self {
            registry,
            stream_id: lease.stream_id.to_string(),
            generation_id: lease.generation_id,
            authority: lease.authority,
            claim_fence: lease.claim_fence.clone(),
            nonce: lease.nonce,
            lease,
            armed: true,
        }
    }

    fn finish(mut self, retain_claim: bool) -> bool {
        let mut retired = self.registry.retire_deleted_promotion_generation(
            &self.stream_id,
            self.generation_id,
            self.authority,
            self.nonce,
            self.claim_fence.as_ref(),
            retain_claim,
        );
        if !retired && !retain_claim {
            retired = self.registry.retire_deleted_promotion_generation(
                &self.stream_id,
                self.generation_id,
                self.authority,
                self.nonce,
                self.claim_fence.as_ref(),
                true,
            );
        }
        if retired {
            self.lease.reservation_active = false;
            self.armed = false;
        }
        retired
    }
}

impl Drop for DeletedPromotionProbeGuard<'_, '_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.registry.retire_deleted_promotion_generation(
            &self.stream_id,
            self.generation_id,
            self.authority,
            self.nonce,
            self.claim_fence.as_ref(),
            true,
        ) {
            self.lease.reservation_active = false;
            self.armed = false;
        } else {
            tracing::error!(
                stream_id = %self.stream_id,
                generation_id = ?self.generation_id,
                "deleted SM promotion could not retain its exact claim handoff"
            );
        }
    }
}

impl PromotionReservationGuard {
    fn new(
        promotions: std::sync::Arc<std::sync::RwLock<super::core::PendingPromotions>>,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> Self {
        Self {
            promotions,
            stream_id: stream_id.to_string(),
            generation_id,
            nonce,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PromotionReservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut promotions) = self.promotions.write() {
            promotions.release_reservation(&self.stream_id, self.generation_id, self.nonce);
        }
    }
}

impl<'a> PendingPromotionRetryLease<'a> {
    fn new(registry: &'a InMemorySmSessionRegistry, session: DetachedSession) -> Self {
        Self {
            registry,
            session: Some(session),
        }
    }

    fn session_mut(&mut self) -> &mut DetachedSession {
        self.session.as_mut().expect("retry lease is armed")
    }

    fn finish(mut self) -> DetachedSession {
        self.session.take().expect("retry lease is armed")
    }

    fn discard(mut self) {
        self.session.take();
    }
}

impl Drop for PendingPromotionRetryLease<'_> {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let stream_id = session.stream_id.clone();
        if let Err(error) = self.registry.retain_pending_promotion_for_retry(session) {
            tracing::warn!(
                %stream_id,
                %error,
                "cancelled retry reconciliation could not restore promotion ownership"
            );
        }
    }
}

struct DrainedSessionBatch<'a> {
    registry: &'a InMemorySmSessionRegistry,
    sessions: Vec<DetachedSession>,
}

impl<'a> DrainedSessionBatch<'a> {
    fn new(registry: &'a InMemorySmSessionRegistry, capacity: usize) -> Self {
        Self {
            registry,
            sessions: Vec::with_capacity(capacity),
        }
    }

    fn push(&mut self, session: DetachedSession) {
        self.sessions.push(session);
    }

    fn len(&self) -> usize {
        self.sessions.len()
    }

    fn finish(mut self) -> Vec<DetachedSession> {
        std::mem::take(&mut self.sessions)
    }
}

impl Drop for DrainedSessionBatch<'_> {
    fn drop(&mut self) {
        for session in self.sessions.drain(..) {
            let stream_id = session.stream_id.clone();
            if let Err(error) = self.registry.retain_pending_promotion_for_retry(session) {
                tracing::warn!(
                    %stream_id,
                    %error,
                    "cancelled expiry drain could not restore promotion ownership"
                );
            }
        }
    }
}

impl InMemorySmSessionRegistry {
    /// Complete the process-local half of a promotion whose durable
    /// generation has already been deleted.
    ///
    /// All fallible validation happens before mutation. Holding these locks
    /// together makes the exact payload retirement and optional claim
    /// handoff one synchronous transition that is safe to invoke from Drop.
    fn retire_deleted_promotion_generation(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        authority: super::SmSessionPromotionAuthority,
        nonce: super::SmPromotionLeaseNonce,
        claim_fence: Option<&super::super::persistence::SmClaimFence>,
        retain_claim: bool,
    ) -> bool {
        let (Ok(mut promotions), Ok(mut retries), Ok(mut pending_releases)) = (
            self.pending_promotions.write(),
            self.pending_promotion_retries.write(),
            self.pending_claim_releases.write(),
        ) else {
            return false;
        };
        let expected_current = authority == super::SmSessionPromotionAuthority::CurrentDurable;
        let reservation_matches = match authority {
            super::SmSessionPromotionAuthority::CurrentDurable => {
                promotions.current_reservation_matches(stream_id, generation_id, nonce)
            }
            super::SmSessionPromotionAuthority::TerminalDurable => {
                promotions.terminal_reservation_matches(stream_id, generation_id, nonce)
            }
            super::SmSessionPromotionAuthority::ObsoleteGeneration => false,
        };
        if !reservation_matches
            || promotions.is_current(stream_id, generation_id) != Some(expected_current)
        {
            return false;
        }
        if retain_claim {
            if let Some(fence) = claim_fence {
                if promotions.claim_fence(stream_id, generation_id).as_ref() != Some(fence) {
                    return false;
                }
            }
        }

        // Reservation and authority were validated under this same write
        // lock, so the exact retirement cannot fail without an internal
        // invariant violation.
        let retired = promotions.retire_under_reservation(stream_id, generation_id, nonce)
            == Some(expected_current);
        if !retired {
            return false;
        }
        retries.remove_generation(stream_id, generation_id);
        if retain_claim {
            if let Some(fence) = claim_fence {
                pending_releases
                    .entry((stream_id.to_string(), fence.clone()))
                    .or_insert(PendingClaimReleaseDisposition::RetainedForDurableRecovery);
            }
        }
        true
    }

    /// Retire one exact fence after the backend either released it or proved
    /// it was no longer owned, while preserving any same-stream replacement
    /// fence and every nonterminal payload generation. Exact terminal
    /// generations are already durably archived, so their local retry
    /// carriers are retired with the fence. No local carrier may keep using
    /// a fence after either terminal outcome.
    pub(super) fn retire_exact_claim_handoff_locally(
        &self,
        stream_id: &str,
        fence: &super::super::persistence::SmClaimFence,
    ) -> bool {
        let (
            Ok(mut promotions),
            Ok(mut retries),
            Ok(mut reclaimed_reservations),
            Ok(mut pending_releases),
            Ok(mut active_fences),
            Ok(mut pending_hydrations),
        ) = (
            self.pending_promotions.write(),
            self.pending_promotion_retries.write(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
            self.pending_reclaimed_hydrations.write(),
        )
        else {
            return false;
        };
        let key = (stream_id.to_string(), fence.clone());
        if !pending_releases.contains_key(&key) {
            return false;
        }
        for generation_id in promotions.relinquish_exact_claim_fence(stream_id, fence) {
            retries.remove_generation(stream_id, generation_id);
        }
        if active_fences.get(stream_id) == Some(fence) {
            active_fences.remove(stream_id);
        }
        let matching_hydration_reservations = pending_hydrations
            .keys()
            .filter_map(|(pending_stream_id, pending_fence, reservation)| {
                (pending_stream_id == stream_id && pending_fence == fence).then_some(*reservation)
            })
            .collect::<std::collections::HashSet<_>>();
        pending_hydrations.retain(|(pending_stream_id, pending_fence, _), _| {
            pending_stream_id != stream_id || pending_fence != fence
        });
        if reclaimed_reservations
            .get(stream_id)
            .is_some_and(|reservation| matching_hydration_reservations.contains(reservation))
        {
            reclaimed_reservations.remove(stream_id);
        }
        pending_releases.remove(&key);
        true
    }

    /// Atomically move a claim acquired for `<enable/>` out of the active
    /// live-authority map and into terminal exact-release inventory. This is
    /// synchronous so a cancellation guard can call it from `Drop` when the
    /// WebSocket task disappears before enabled state is published.
    pub fn defer_unpublished_enabled_claim_release(&self, stream_id: &str) -> bool {
        self.defer_enabled_claim_release(stream_id)
    }

    /// Move a claim from a connection that lost its same-full-JID registry
    /// slot into exact terminal cleanup. The replacement owns routing and
    /// cleanup for the JID, but never owns this connection's distinct SM id.
    pub fn defer_superseded_enabled_claim_release(&self, stream_id: &str) -> bool {
        self.defer_enabled_claim_release(stream_id)
    }

    fn defer_enabled_claim_release(&self, stream_id: &str) -> bool {
        let fence = self
            .claim_fences
            .read()
            .ok()
            .and_then(|fences| fences.get(stream_id).cloned());
        fence.is_some_and(|fence| self.try_record_terminal_claim_fence(stream_id, fence))
    }
    /// Remove every expired session and return the detached state in full.
    ///
    /// Callers (notably the server-side janitor) need the JID and stream id
    /// of each expired session so they can run associated cleanup —
    /// removing MUC occupants, evicting routing entries, and discarding
    /// sidecar auth context. `cleanup_expired` only returns a count, which
    /// isn't enough for that work.
    /// Drain every detached + claimed session from the in-memory
    /// view, regardless of expiry status. Intended for the
    /// graceful-shutdown path (issue #209 slice (d) phase 4 +
    /// locked Q8 = B): the server is exiting, so it walks the full
    /// session set and hands each one's unacked queue to the Q6
    /// promotion path before terminating.
    ///
    /// **This method does NOT delete durable rows.** The caller is
    /// expected to invoke [`Self::confirm_drained`] for each
    /// session AFTER its unacked queue has been successfully
    /// promoted. This ordering ensures that if promotion fails
    /// mid-batch (timeout, panic, storage error), the failed
    /// sessions' durable rows survive and a subsequent restart can
    /// retry promotion. (Copilot review on PR #346: previous
    /// implementation deleted durable rows up-front, losing
    /// stanzas on any partial-promotion failure.)
    async fn drain_promotion_retries_into(
        &self,
        drained: &mut DrainedSessionBatch<'_>,
    ) -> Result<(), SmRegistryError> {
        let retry_generations = self
            .pending_promotion_retries
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .generation_keys()
            .map(|(stream_id, generation_id)| (stream_id.clone(), generation_id))
            .collect::<Vec<_>>();
        for (stream_id, generation_id) in retry_generations {
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            if self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .generation_reservation_active(&stream_id, generation_id)
            {
                continue;
            }
            let Some(session) = self
                .pending_promotion_retries
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove_generation(&stream_id, generation_id)
            else {
                continue;
            };
            let mut retry = PendingPromotionRetryLease::new(self, session);
            self.reconcile_retry_payload(retry.session_mut()).await;
            let still_pending = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .contains_generation(&stream_id, generation_id);
            if still_pending {
                drained.push(retry.finish());
            } else {
                retry.discard();
            }
        }
        Ok(())
    }

    pub async fn drain_all_for_shutdown(&self) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let stream_ids: Vec<String> = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.keys().cloned().collect()
        };
        let retry_count = self
            .pending_promotion_retries
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .len();
        let mut drained =
            DrainedSessionBatch::new(self, stream_ids.len().saturating_add(retry_count));
        self.drain_promotion_retries_into(&mut drained).await?;
        for stream_id in &stream_ids {
            let stream_lock = self.stream_lock(stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let removed = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let mut promotions = self
                    .pending_promotions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let removed = sessions.remove(stream_id);
                if let Some(session) = removed.as_ref() {
                    if !promotions.insert_current(session) {
                        sessions.insert(stream_id.clone(), session.clone());
                        return Err(SmRegistryError::Internal(
                            "shutdown drain could not reserve exact promotion generation"
                                .to_string(),
                        ));
                    }
                }
                removed
            };
            if let Some(session) = removed {
                drained.push(session);
            }
        }
        Ok(drained.finish())
    }

    /// Count distinct process-local SM ownership responsibilities that must
    /// converge before graceful shutdown may call an empty drain quiet.
    /// This deliberately shares the complete, deduplicated ownership
    /// inventory used by self-fencing so acquisition reservations, ambiguous
    /// ownership mutations, reclaimed hydration work, and release-only exact
    /// handoffs cannot disappear behind an empty resumable-session view.
    pub fn pending_shutdown_recovery_count(&self) -> Result<usize, SmRegistryError> {
        self.locally_owned_claim_ids()
            .map(|stream_ids| stream_ids.len())
            .ok_or_else(|| SmRegistryError::Internal("Lock poisoned".to_string()))
    }

    /// Count claim-shaped shutdown responsibilities independently from the
    /// broader recovery inventory used for quiet detection.
    ///
    /// Exact fences are deduplicated by the full `(stream id, owner, epoch)`
    /// tuple, not by stream id, so a late old-fence handoff and a same-id
    /// successor remain two distinct responsibilities. Reservation-only
    /// ownership ambiguity is reported separately and never feeds the
    /// `claims_abandoned_on_drain` counter as though it were a proven claim.
    pub fn pending_shutdown_claim_responsibility_counts(
        &self,
    ) -> Result<super::SmShutdownClaimResponsibilityCounts, SmRegistryError> {
        let poisoned = || SmRegistryError::Internal("Lock poisoned".to_string());
        // Hold the canonical claim-bookkeeping lock order used by every
        // source/destination transfer. Independent snapshots cannot be made
        // safe by ordering alone because exact responsibility moves in both
        // directions between active and pending carriers.
        let (promotions, reservations, reclaimed, pending, fences, hydrations) = (
            self.pending_promotions.read().map_err(|_| poisoned())?,
            self.claim_fence_reservations
                .read()
                .map_err(|_| poisoned())?,
            self.reclaimed_claim_reservations
                .read()
                .map_err(|_| poisoned())?,
            self.pending_claim_releases.read().map_err(|_| poisoned())?,
            self.claim_fences.read().map_err(|_| poisoned())?,
            self.pending_reclaimed_hydrations
                .read()
                .map_err(|_| poisoned())?,
        );
        let mut exact = std::collections::HashSet::new();
        exact.extend(
            promotions
                .shutdown_claim_fences()
                .map(|(stream_id, fence)| (stream_id.clone(), fence.clone())),
        );
        exact.extend(
            hydrations
                .keys()
                .map(|(stream_id, fence, _)| (stream_id.clone(), fence.clone())),
        );
        exact.extend(
            fences
                .iter()
                .map(|(stream_id, fence)| (stream_id.clone(), fence.clone())),
        );
        exact.extend(pending.keys().cloned());
        let hydrated_reservations = hydrations
            .keys()
            .map(|(stream_id, _, reservation)| (stream_id.as_str(), *reservation))
            .collect::<std::collections::HashSet<_>>();
        let mut unknown = reservations
            .iter()
            .filter(|stream_id| {
                super::core::claim_reservation_requires_independent_capacity(
                    &pending, &fences, stream_id,
                )
            })
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        unknown.extend(
            reclaimed
                .iter()
                .filter(|&(stream_id, reservation)| {
                    !hydrated_reservations.contains(&(stream_id.as_str(), *reservation))
                        && super::core::claim_reservation_requires_independent_capacity(
                            &pending, &fences, stream_id,
                        )
                })
                .map(|(stream_id, _)| stream_id.clone()),
        );
        Ok(super::SmShutdownClaimResponsibilityCounts {
            exact: exact.len(),
            unknown: unknown.len(),
        })
    }

    /// Snapshot every currently-live SM session id (detached, claimed,
    /// or moving through promote → confirm). Returns `None` if any
    /// internal lock is poisoned
    /// — the caller (claim-expiry janitor) MUST treat that as
    /// "skip this sweep" rather than proceed with a partial set,
    /// since an empty live set would trigger mass-release of every
    /// claim. (Copilot review on PR #360.)
    ///
    /// "Live" here means the session's durable record is still
    /// resumable: its resume window hasn't closed yet OR a resume
    /// claim is in flight. Sessions that have already been drained
    /// and `confirm_drained`'d are absent from this set, and their
    /// `pending_delivery` claims are eligible for orphan recovery.
    pub fn live_session_ids(&self) -> Option<Vec<String>> {
        let sessions = self.sessions.read().ok()?;
        let claimed = self.claimed_sessions.read().ok()?;
        let promotions = self.pending_promotions.read().ok()?;
        let mut out: Vec<String> = sessions.keys().cloned().collect();
        out.extend(claimed.keys().cloned());
        out.extend(promotions.iter().cloned());
        out.sort();
        out.dedup();
        Some(out)
    }

    /// Snapshot every SM claim this process must still account for during
    /// ownership reconciliation. Unlike [`Self::live_session_ids`], this
    /// includes terminal exact-release responsibilities: they are not
    /// resumable sessions, but their release may not have committed yet and
    /// the local node must not forget that possible ownership.
    pub fn locally_owned_claim_ids(&self) -> Option<Vec<String>> {
        let mut out = self.live_session_ids()?;
        // Snapshot one bookkeeping lock at a time to avoid lock-order
        // inversion with admission. Read transfer sources before their
        // destinations so a concurrent source -> destination move can be
        // observed twice but never disappear between snapshots.
        {
            let reservations = self.claim_fence_reservations.read().ok()?;
            out.extend(reservations.iter().cloned());
        }
        {
            let reservations = self.reclaimed_claim_reservations.read().ok()?;
            out.extend(reservations.keys().cloned());
        }
        {
            let pending_acquisitions = self.pending_claim_acquisitions.read().ok()?;
            out.extend(
                pending_acquisitions
                    .iter()
                    .map(|(stream_id, _, _)| stream_id.clone()),
            );
        }
        {
            let displaced = self.pending_promotions.read().ok()?;
            out.extend(displaced.iter().cloned());
        }
        {
            let retries = self.pending_promotion_retries.read().ok()?;
            out.extend(retries.iter().map(|(stream_id, _)| stream_id.clone()));
        }
        {
            let pending_epoch_failures = self.pending_epoch_failure_reconciliations.read().ok()?;
            out.extend(pending_epoch_failures.iter().cloned());
        }
        {
            let pending_hydration = self.pending_reclaimed_hydrations.read().ok()?;
            out.extend(
                pending_hydration
                    .keys()
                    .map(|(stream_id, _, _)| stream_id.clone()),
            );
        }
        {
            let pending_lookups = self.pending_reclaimed_claim_lookups.read().ok()?;
            out.extend(
                pending_lookups
                    .keys()
                    .map(|(stream_id, _, _)| stream_id.clone()),
            );
        }
        {
            let fences = self.claim_fences.read().ok()?;
            out.extend(fences.keys().cloned());
        }
        {
            let pending = self.pending_claim_releases.read().ok()?;
            out.extend(pending.keys().map(|(stream_id, _)| stream_id.clone()));
        }
        out.sort();
        out.dedup();
        Some(out)
    }

    /// Snapshot claim responsibilities tied to one immutable node identity.
    /// Used after self-fence rotation: fresh admissions now carry the new
    /// identity, while anything returned here is provably stale local work
    /// that the final pre-rotation inventory may have missed.
    pub fn locally_owned_claim_ids_for_owner(
        &self,
        owner: &crate::ownership::NodeIdentity,
    ) -> Option<Vec<String>> {
        let mut out = Vec::new();
        {
            let pending = self.pending_claim_acquisitions.read().ok()?;
            out.extend(
                pending
                    .iter()
                    .filter(|(_, pending_owner, _)| pending_owner == owner)
                    .map(|(stream_id, _, _)| stream_id.clone()),
            );
        }
        {
            let lookups = self.pending_reclaimed_claim_lookups.read().ok()?;
            out.extend(
                lookups
                    .keys()
                    .filter(|(_, pending_owner, _)| pending_owner == owner)
                    .map(|(stream_id, _, _)| stream_id.clone()),
            );
        }
        {
            let hydrations = self.pending_reclaimed_hydrations.read().ok()?;
            out.extend(
                hydrations
                    .keys()
                    .filter(|(_, fence, _)| fence.owner() == owner)
                    .map(|(stream_id, _, _)| stream_id.clone()),
            );
        }
        {
            let fences = self.claim_fences.read().ok()?;
            out.extend(
                fences
                    .iter()
                    .filter(|(_, fence)| fence.owner() == owner)
                    .map(|(stream_id, _)| stream_id.clone()),
            );
        }
        {
            let pending = self.pending_claim_releases.read().ok()?;
            out.extend(
                pending
                    .iter()
                    .filter(|((_, fence), _)| fence.owner() == owner)
                    .map(|((stream_id, _), _)| stream_id.clone()),
            );
        }
        out.sort();
        out.dedup();
        Some(out)
    }

    /// Retry exact terminal releases retained after a backend error/timeout.
    /// Entries still represented by a live/detached session are excluded:
    /// those claims remain intentionally held.
    pub async fn retry_pending_claim_releases(
        &self,
        limit: usize,
    ) -> super::SmClaimReleaseRetrySummary {
        self.retry_pending_claim_releases_observing(limit, |_| {})
            .await
    }

    /// Retry exact claim handoffs and synchronously observe each completed
    /// local outcome. The observer runs before the next await, so graceful
    /// shutdown cannot lose already-completed release accounting if its
    /// combined retry budget later cancels this pass.
    pub async fn retry_pending_claim_releases_observing<F>(
        &self,
        limit: usize,
        mut observe: F,
    ) -> super::SmClaimReleaseRetrySummary
    where
        F: FnMut(super::SmClaimReleaseRetryOutcome),
    {
        let mut summary = super::SmClaimReleaseRetrySummary::default();
        let epoch_failures = self
            .pending_epoch_failure_reconciliations
            .read()
            .map(|pending| pending.iter().take(limit).cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut budget_used = 0;
        for stream_id in epoch_failures {
            budget_used += 1;
            if let Err(error) = self
                .reconcile_claim_after_epoch_lookup_failure(&stream_id)
                .await
            {
                tracing::debug!(
                    stream_id,
                    %error,
                    "SM epoch-failure reconciliation remains pending"
                );
            }
        }
        let uncertain = self
            .pending_claim_acquisitions
            .read()
            .map(|pending| {
                pending
                    .iter()
                    .take(limit.saturating_sub(budget_used))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (stream_id, identity, disposition) in uncertain {
            budget_used += 1;
            // Rejected-enable reconciliation is a terminal release retry.
            // The same stream id may have become detached/claimed through a
            // later successful local path while the original acquisition was
            // uncertain. Serialize with that stream and fail closed on map
            // uncertainty so the retry never releases a now-live claim.
            if disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable {
                let Ok(stream_lock) = self.stream_lock(&stream_id) else {
                    continue;
                };
                let _stream_guard = stream_lock.lock().await;
                if self
                    .reconcile_rejected_enable_while_live(&stream_id, &identity)
                    .await
                {
                    continue;
                }
                if let Some(outcome) = self
                    .reconcile_uncertain_claim_acquisition_locked(&stream_id, identity, disposition)
                    .await
                {
                    summary.record(outcome);
                    observe(outcome);
                }
                continue;
            }
            if let Some(outcome) = self
                .reconcile_uncertain_claim_acquisition_with_outcome(
                    &stream_id,
                    identity,
                    disposition,
                )
                .await
            {
                summary.record(outcome);
                observe(outcome);
            }
        }
        let pending = {
            let Ok(pending) = self.pending_claim_releases.read() else {
                return summary;
            };
            pending
                .iter()
                .map(|((stream_id, fence), disposition)| {
                    (stream_id.clone(), fence.clone(), *disposition)
                })
                .collect::<Vec<_>>()
        };
        for (stream_id, fence, _disposition) in pending {
            if budget_used >= limit {
                break;
            }
            let Ok(stream_lock) = self.stream_lock(&stream_id) else {
                continue;
            };
            let _stream_guard = stream_lock.lock().await;
            let still_pending = self
                .pending_claim_releases
                .read()
                .map(|current| current.contains_key(&(stream_id.clone(), fence.clone())))
                .unwrap_or(false);
            if !still_pending {
                continue;
            }
            budget_used += 1;
            if self.any_durable_work_may_remain(&stream_id).await {
                let entity = sm_session_entity(&stream_id);
                let outcome = match tokio::time::timeout(
                    CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    self.claim_store
                        .fence(&entity, fence.owner(), fence.epoch()),
                )
                .await
                {
                    Ok(Ok(true)) => {
                        // Durable recovery still needs this exact shared
                        // claim. Keep the local handoff marker so a later
                        // empty-durable proof can release it.
                        super::SmClaimReleaseRetryOutcome::Retained
                    }
                    Ok(Ok(false)) => {
                        // The pending fence has already been superseded or
                        // lost. Retire its exact active/promotion authority
                        // together with the marker; a different replacement
                        // fence and all payload generations remain untouched.
                        if self.retire_exact_claim_handoff_locally(&stream_id, &fence) {
                            if let Some(storage) = &self.persistence {
                                let session_id =
                                    crate::pending_delivery::SmSessionId::new(stream_id.clone());
                                storage.evict_claim_cache(&session_id, &fence);
                            }
                            super::SmClaimReleaseRetryOutcome::Disproved
                        } else {
                            super::SmClaimReleaseRetryOutcome::Retained
                        }
                    }
                    Ok(Err(error)) => {
                        debug!(
                            stream_id,
                            %error,
                            "could not verify pending SM claim handoff; retaining exact release responsibility"
                        );
                        super::SmClaimReleaseRetryOutcome::Retained
                    }
                    Err(_) => {
                        tracing::warn!(
                            stream_id,
                            timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                            "pending SM claim handoff verification timed out; retaining exact release responsibility"
                        );
                        super::SmClaimReleaseRetryOutcome::Retained
                    }
                };
                summary.record(outcome);
                observe(outcome);
                continue;
            }
            let outcome = self
                .release_claim_store_entry_under(&stream_id, fence)
                .await;
            summary.record(outcome);
            observe(outcome);
        }
        debug_assert_eq!(
            summary.attempted,
            summary
                .released
                .saturating_add(summary.disproved)
                .saturating_add(summary.retained),
            "every exact release retry attempt must have exactly one outcome"
        );
        summary
    }

    pub(super) fn stream_liveness(&self, stream_id: &str) -> Option<bool> {
        match (
            self.sessions.read(),
            self.claimed_sessions.read(),
            self.pending_promotions.read(),
        ) {
            (Ok(sessions), Ok(claimed), Ok(promotions)) => Some(
                sessions.contains_key(stream_id)
                    || claimed.contains_key(stream_id)
                    || promotions.contains(stream_id),
            ),
            _ => None,
        }
    }

    fn generation_lifecycle_state(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<(bool, Option<bool>, bool)> {
        let sessions = self.sessions.read().ok()?;
        let claimed = self.claimed_sessions.read().ok()?;
        let promotions = self.pending_promotions.read().ok()?;
        let generation_live = sessions
            .get(stream_id)
            .or_else(|| claimed.get(stream_id))
            .is_some_and(|session| session.generation_id == generation_id);
        let replacement_live = sessions
            .get(stream_id)
            .or_else(|| claimed.get(stream_id))
            .is_some_and(|session| session.generation_id != generation_id)
            || promotions
                .current_durable_generation(stream_id)
                .is_some_and(|current| current != generation_id);
        Some((
            generation_live,
            promotions.is_current(stream_id, generation_id),
            replacement_live,
        ))
    }

    /// Move a locally resumable successor into the exact-generation
    /// promotion inventory before publishing a claim returned for an older
    /// node incarnation. This keeps the full queue while preventing resume
    /// under stale publication authority; the verified fence remains active
    /// only for the promote -> confirm lifecycle.
    fn move_live_successor_to_current_promotion(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        let (Ok(mut sessions), Ok(mut claimed), Ok(mut promotions), Ok(mut retries)) = (
            self.sessions.write(),
            self.claimed_sessions.write(),
            self.pending_promotions.write(),
            self.pending_promotion_retries.write(),
        ) else {
            return false;
        };
        let successor = sessions
            .get(stream_id)
            .or_else(|| claimed.get(stream_id))
            .filter(|session| session.generation_id == generation_id)
            .cloned();
        let Some(successor) = successor else {
            return promotions.is_current(stream_id, generation_id) == Some(true);
        };
        let represented = if promotions.is_current(stream_id, generation_id) == Some(true) {
            true
        } else if promotions.contains_generation(stream_id, generation_id) {
            promotions.restore_current_generation(stream_id, generation_id)
        } else {
            promotions.insert_current(&successor)
        };
        if !represented {
            return false;
        }
        retries.insert(successor.clone());
        if sessions
            .get(stream_id)
            .is_some_and(|session| session.generation_id == successor.generation_id)
        {
            sessions.remove(stream_id);
        }
        if claimed
            .get(stream_id)
            .is_some_and(|session| session.generation_id == successor.generation_id)
        {
            claimed.remove(stream_id);
        }
        true
    }

    /// A definitive ownership rejection must make the stream non-resumable,
    /// but cannot discard the snapshot's full queue. Convert any local
    /// successor into an obsolete durable carrier and unlink that exact
    /// generation from bare-row mutation authority. The immediate store path
    /// may additionally retire the stream fence; reconciliation deliberately
    /// leaves it alone because a same-stream successor may now own it.
    pub(super) fn retire_detach_after_definite_claim_rejection(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        retire_stream_fence: bool,
    ) -> bool {
        {
            let (Ok(mut sessions), Ok(mut claimed), Ok(mut promotions), Ok(mut retries)) = (
                self.sessions.write(),
                self.claimed_sessions.write(),
                self.pending_promotions.write(),
                self.pending_promotion_retries.write(),
            ) else {
                return false;
            };
            let successor = sessions
                .get(stream_id)
                .or_else(|| claimed.get(stream_id))
                .filter(|session| session.generation_id == generation_id)
                .cloned();
            if let Some(successor) = successor {
                let represented = promotions
                    .contains_generation(stream_id, successor.generation_id)
                    || promotions.insert_unowned_durable_carrier(&successor);
                if !represented {
                    return false;
                }
                retries.insert(successor.clone());
                if sessions
                    .get(stream_id)
                    .is_some_and(|session| session.generation_id == successor.generation_id)
                {
                    sessions.remove(stream_id);
                }
                if claimed
                    .get(stream_id)
                    .is_some_and(|session| session.generation_id == successor.generation_id)
                {
                    claimed.remove(stream_id);
                }
            }
            if let Some(purged_generation) =
                promotions.demote_generation_for_external_claim_loss(stream_id, generation_id)
            {
                retries.remove_generation(stream_id, purged_generation);
            }
        }

        if !retire_stream_fence {
            return true;
        }

        let active_fence = self
            .claim_fences
            .read()
            .ok()
            .and_then(|fences| fences.get(stream_id).cloned());
        if let Some(fence) = active_fence.as_ref() {
            if !self
                .try_record_terminal_claim_fence_preserving_reservation(stream_id, fence.clone())
            {
                return false;
            }
        }
        let preserved_releases = self
            .pending_claim_releases
            .read()
            .map(|pending| {
                pending
                    .keys()
                    .filter(|(id, _)| id == stream_id)
                    .map(|(_, fence)| fence.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.forget_claim_locally_preserving_terminal_releases_locked(
            stream_id,
            &preserved_releases,
        );
        true
    }

    /// Retire a rejected-enable acquisition only after a bounded,
    /// non-creating lookup accounts for its possible committed claim. A live
    /// detached session is not proof by itself: detach-time acquisition is
    /// best effort and may have failed under a newer node identity.
    ///
    /// Returns `true` when the stream is live or its liveness is uncertain,
    /// so the caller must not run the terminal `ensure_claimed` path.
    pub(super) async fn reconcile_rejected_enable_while_live(
        &self,
        stream_id: &str,
        pending_identity: &crate::ownership::NodeIdentity,
    ) -> bool {
        match self.stream_liveness(stream_id) {
            Some(false) => return false,
            None => return true,
            Some(true) => {}
        }

        // One reservation cannot cover both a possibly-new generation from
        // detach reconciliation and terminal cleanup of the rejected-enable
        // generation. Let the live detach resolve first; a later sweep can
        // then spend the slot on terminal conversion without undercounting.
        let detach_still_uncertain = match self.pending_claim_acquisitions.read() {
            Ok(pending) => pending.iter().any(|(id, _, disposition)| {
                id == stream_id
                    && matches!(
                        disposition,
                        PendingClaimAcquisitionDisposition::RetainDetachedSession(_)
                    )
            }),
            Err(_) => return true,
        };
        if detach_still_uncertain {
            return true;
        }

        let entity = sm_session_entity(stream_id);
        let snapshot = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim_after_pending_writes(&entity),
        )
        .await
        {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(_)) | Err(_) => return true,
        };

        if let Some(snapshot) = snapshot {
            if snapshot.owner == *pending_identity {
                let fence = super::super::persistence::SmClaimFence::new(
                    snapshot.owner,
                    snapshot.claim_epoch,
                );
                if *pending_identity == self.node_identity.current() {
                    if !self.try_record_claim_fence(stream_id, fence) {
                        return true;
                    }
                } else {
                    // An old incarnation's snapshot is not authority for the
                    // current live lifecycle. Hand the untouched backend
                    // claim to fresh-owner orphan discovery; releasing it
                    // would hide the durable successor from claim-first
                    // recovery.
                    if !self.try_record_durable_claim_handoff(stream_id, fence) {
                        return true;
                    }
                }
            }
        }

        self.remove_pending_claim_acquisition(
            stream_id,
            pending_identity,
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        );
        true
    }

    fn remove_pending_claim_acquisition(
        &self,
        stream_id: &str,
        identity: &crate::ownership::NodeIdentity,
        disposition: PendingClaimAcquisitionDisposition,
    ) {
        let no_remaining_for_stream = {
            let Ok(mut pending) = self.pending_claim_acquisitions.write() else {
                return;
            };
            if !pending.remove(&(stream_id.to_string(), identity.clone(), disposition)) {
                return;
            }
            !pending.iter().any(|(id, _, _)| id == stream_id)
        };
        if no_remaining_for_stream {
            self.cancel_claim_fence_reservation(stream_id);
        }
    }

    pub(super) async fn reconcile_uncertain_claim_acquisition(
        &self,
        stream_id: &str,
        identity: crate::ownership::NodeIdentity,
        disposition: PendingClaimAcquisitionDisposition,
    ) {
        let _ = self
            .reconcile_uncertain_claim_acquisition_with_outcome(stream_id, identity, disposition)
            .await;
    }

    async fn reconcile_uncertain_claim_acquisition_with_outcome(
        &self,
        stream_id: &str,
        identity: crate::ownership::NodeIdentity,
        disposition: PendingClaimAcquisitionDisposition,
    ) -> Option<super::SmClaimReleaseRetryOutcome> {
        let Ok(stream_lock) = self.stream_lock(stream_id) else {
            return None;
        };
        let _stream_guard = stream_lock.lock().await;
        self.reconcile_uncertain_claim_acquisition_locked(stream_id, identity, disposition)
            .await
    }

    async fn reconcile_uncertain_claim_acquisition_locked(
        &self,
        stream_id: &str,
        identity: crate::ownership::NodeIdentity,
        disposition: PendingClaimAcquisitionDisposition,
    ) -> Option<super::SmClaimReleaseRetryOutcome> {
        let pending_key = (stream_id.to_string(), identity.clone(), disposition);
        let still_pending = self
            .pending_claim_acquisitions
            .read()
            .map(|pending| pending.contains(&pending_key))
            .unwrap_or(false);
        if !still_pending {
            return None;
        }
        let entity = sm_session_entity(stream_id);
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.ensure_claimed(&entity, &identity),
        )
        .await
        {
            Ok(Ok(epoch)) => {
                let fence = super::super::persistence::SmClaimFence::new(identity.clone(), epoch);
                match disposition {
                    PendingClaimAcquisitionDisposition::ReleaseRejectedEnable => {
                        // Convert to terminal inventory before the release
                        // await. The janitor's outer wall-clock budget may
                        // cancel this future at any await; pending exact
                        // responsibility must already be durable in memory.
                        let recorded =
                            self.try_record_terminal_claim_fence(stream_id, fence.clone());
                        if !recorded {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert(pending_key);
                            }
                            return None;
                        }
                        if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                            pending.remove(&pending_key);
                        }
                        if !self.any_durable_work_may_remain(stream_id).await {
                            return Some(
                                self.release_claim_store_entry_under(stream_id, fence).await,
                            );
                        }
                    }
                    PendingClaimAcquisitionDisposition::RetainDetachedSession(generation_id) => {
                        let Some((generation_live, promotion_current, replacement_live)) =
                            self.generation_lifecycle_state(stream_id, generation_id)
                        else {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert(pending_key);
                            }
                            return None;
                        };
                        let target_is_current = !replacement_live
                            && (generation_live || promotion_current == Some(true));
                        if target_is_current {
                            let recorded = self.node_identity.with_current(|current_identity| {
                                let represented =
                                    if generation_live && current_identity != &identity {
                                        self.move_live_successor_to_current_promotion(
                                            stream_id,
                                            generation_id,
                                        )
                                    } else {
                                        true
                                    };
                                represented
                                    && self
                                        .try_record_verified_claim_fence(stream_id, fence.clone())
                            });
                            if !recorded {
                                if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                    pending.insert(pending_key);
                                }
                                return None;
                            }
                            self.remove_pending_claim_acquisition(
                                stream_id,
                                &identity,
                                disposition,
                            );
                            return None;
                        }

                        // The target generation was superseded while its CAS
                        // outcome was unknown. Settle only that generation;
                        // never publish or terminalize the newer lifecycle's
                        // active fence. If both generations observe the same
                        // exact claim, the successor already accounts for it.
                        if generation_live
                            && !self.retire_detach_after_definite_claim_rejection(
                                stream_id,
                                generation_id,
                                false,
                            )
                        {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert(pending_key);
                            }
                            return None;
                        }
                        let active_matches = self
                            .claim_fences
                            .read()
                            .is_ok_and(|active| active.get(stream_id) == Some(&fence));
                        if active_matches && replacement_live {
                            self.remove_pending_claim_acquisition(
                                stream_id,
                                &identity,
                                disposition,
                            );
                            return None;
                        }
                        if !self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert(pending_key);
                            }
                            return None;
                        }
                        self.remove_pending_claim_acquisition(stream_id, &identity, disposition);
                        if !self.any_durable_work_may_remain(stream_id).await {
                            return Some(
                                self.release_claim_store_entry_under(stream_id, fence).await,
                            );
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                if definite_detach_claim_rejection(&error).is_some() {
                    if let PendingClaimAcquisitionDisposition::RetainDetachedSession(
                        generation_id,
                    ) = disposition
                    {
                        let retire_stream_fence = self
                            .generation_lifecycle_state(stream_id, generation_id)
                            .is_some_and(
                                |(generation_live, promotion_current, replacement_live)| {
                                    !replacement_live
                                        && (generation_live || promotion_current == Some(true))
                                },
                            );
                        if !self.retire_detach_after_definite_claim_rejection(
                            stream_id,
                            generation_id,
                            retire_stream_fence,
                        ) {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert((stream_id.to_string(), identity, disposition));
                            }
                            return None;
                        }
                    }
                    self.remove_pending_claim_acquisition(stream_id, &identity, disposition);
                } else if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                    pending.insert((stream_id.to_string(), identity, disposition));
                }
            }
            Err(_) => {
                if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                    pending.insert((stream_id.to_string(), identity, disposition));
                }
            }
        }
        None
    }

    pub fn pending_claim_release_count(&self) -> usize {
        self.pending_claim_releases
            .read()
            .map_or(0, |pending| pending.len())
    }

    /// Purely local, best-effort demotion of `stream_id`'s claim. Exact
    /// release responsibility is retained before the active cache entry is
    /// removed. Bare-row promotion generations remain available as
    /// payload-only retries; already-archived terminal carriers retire from
    /// local retry inventory. No backend round-trip is required:
    /// self-fencing must still complete while Postgres is unreachable.
    pub async fn forget_claim_locally(&self, stream_id: &str) {
        self.forget_claim_locally_matching_owner(stream_id, None)
            .await;
    }

    /// Demote the active `stream_id` lifecycle only when its exact fence
    /// still belongs to `owner`. Owner-filtered inventories can also select
    /// a stream through an older terminal carrier after its fence moved to
    /// pending-release inventory; retire those typed owner-matching terminal
    /// carriers without disturbing a replacement lifecycle under a fresh
    /// owner.
    pub async fn forget_claim_locally_owned_by(
        &self,
        stream_id: &str,
        owner: &crate::ownership::NodeIdentity,
    ) {
        self.forget_claim_locally_matching_owner(stream_id, Some(owner))
            .await;
    }

    async fn forget_claim_locally_matching_owner(
        &self,
        stream_id: &str,
        expected_owner: Option<&crate::ownership::NodeIdentity>,
    ) {
        let Ok(stream_lock) = self.stream_lock(stream_id) else {
            return;
        };
        let _stream_guard = stream_lock.lock().await;
        self.node_identity
            .with_publications_blocked(|| {
                let active_fence = self
                    .claim_fences
                    .read()
                    .ok()
                    .and_then(|fences| fences.get(stream_id).cloned());
                let active_fence_matches_expected_owner = expected_owner.is_none_or(|owner| {
                    active_fence
                        .as_ref()
                        .map(super::super::persistence::SmClaimFence::owner)
                        == Some(owner)
                });
                if let Some(fence) = active_fence
                    .as_ref()
                    .filter(|_| active_fence_matches_expected_owner)
                {
                    if !self.try_record_terminal_claim_fence_preserving_reservation(
                        stream_id,
                        fence.clone(),
                    ) {
                        tracing::warn!(
                            stream_id,
                            "local claim demotion could not retain exact release responsibility"
                        );
                        return;
                    }
                }
                if let (Ok(mut promotions), Ok(mut retries)) = (
                    self.pending_promotions.write(),
                    self.pending_promotion_retries.write(),
                ) {
                    let purged_generations =
                        if let Some(owner) = expected_owner {
                            let mut purged =
                                promotions.purge_terminal_generations_owned_by(stream_id, owner);
                            if active_fence_matches_expected_owner {
                                purged.extend(promotions.demote_for_external_claim_loss(
                                    stream_id,
                                    active_fence.as_ref(),
                                ));
                            }
                            purged
                        } else {
                            promotions.demote_for_external_claim_loss(stream_id, None)
                        };
                    for generation_id in purged_generations {
                        retries.remove_generation(stream_id, generation_id);
                    }
                } else {
                    tracing::warn!(
                        stream_id,
                        "local claim demotion could not unlink pending promotion authority"
                    );
                    return;
                }
                if !active_fence_matches_expected_owner {
                    return;
                }
                let preserved_releases = match active_fence {
                    Some(fence) => vec![fence],
                    None => self
                        .pending_claim_releases
                        .read()
                        .map(|pending| {
                            pending
                                .keys()
                                .filter(|(id, _)| id == stream_id)
                                .map(|(_, fence)| fence.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                };
                self.forget_claim_locally_preserving_terminal_releases_locked(
                    stream_id,
                    &preserved_releases,
                );
            })
            .await;
    }

    pub(super) fn forget_claim_locally_locked(
        &self,
        stream_id: &str,
        preserve_terminal_release: Option<&super::super::persistence::SmClaimFence>,
    ) {
        let preserved_releases = preserve_terminal_release
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        self.forget_claim_locally_preserving_terminal_releases_locked(
            stream_id,
            &preserved_releases,
        );
    }

    /// Fail-closed durable-work probe for non-Q6 current-row deletion paths.
    /// A transient read failure keeps the shared claim: same-id terminal
    /// carriers may still need that exact fence even though the bare current
    /// row was deleted successfully.
    fn local_other_work_may_remain(
        &self,
        stream_id: &str,
        excluding_generation: super::SmSessionGenerationId,
    ) -> bool {
        self.sessions.read().map_or(true, |sessions| {
            sessions
                .get(stream_id)
                .is_some_and(|session| session.generation_id != excluding_generation)
        }) || self.claimed_sessions.read().map_or(true, |sessions| {
            sessions
                .get(stream_id)
                .is_some_and(|session| session.generation_id != excluding_generation)
        }) || self
            .pending_claim_acquisitions
            .read()
            .map_or(true, |pending| {
                pending.iter().any(|(id, _, _)| id == stream_id)
            })
            || self.pending_promotions.read().map_or(true, |promotions| {
                promotions.contains_other_generation(stream_id, excluding_generation)
            })
    }

    /// Fail closed when deciding whether a stream with no claimed-map entry
    /// can surrender its shared claim. Unlike
    /// [`Self::local_other_work_may_remain`], this excludes no generation:
    /// an absent `claimed_sessions` entry provides no proof that a same-stream
    /// live session, terminal generation, or publication-unknown payload has
    /// been retired.
    fn local_work_may_remain(&self, stream_id: &str) -> bool {
        self.sessions
            .read()
            .map_or(true, |sessions| sessions.contains_key(stream_id))
            || self
                .claimed_sessions
                .read()
                .map_or(true, |sessions| sessions.contains_key(stream_id))
            || self
                .pending_claim_acquisitions
                .read()
                .map_or(true, |pending| {
                    pending.iter().any(|(id, _, _)| id == stream_id)
                })
            || self
                .pending_promotions
                .read()
                .map_or(true, |promotions| promotions.contains(stream_id))
    }

    pub(super) async fn any_durable_work_may_remain(&self, stream_id: &str) -> bool {
        if self.local_work_may_remain(stream_id) {
            return true;
        }
        let Some(storage) = self.persistence.as_ref() else {
            return false;
        };
        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
        match storage.has_durable_work(&session_id).await {
            Ok(remains) => remains,
            Err(error) => {
                debug!(
                    stream_id,
                    %error,
                    "could not prove SM durable work empty; retaining the shared claim"
                );
                true
            }
        }
    }

    /// Probe shared durable work while treating exact in-memory generations
    /// as already slated for synchronous removal by the caller. Ambiguous
    /// acquisitions and every pending promotion remain claim-bearing, and a
    /// persistence error fails closed.
    pub(super) async fn durable_work_may_remain_ignoring_map_generations(
        &self,
        stream_id: &str,
        ignored_generations: &[super::SmSessionGenerationId],
    ) -> bool {
        let mapped_work_remains = self.sessions.read().map_or(true, |sessions| {
            sessions
                .get(stream_id)
                .is_some_and(|session| !ignored_generations.contains(&session.generation_id))
        }) || self.claimed_sessions.read().map_or(true, |sessions| {
            sessions
                .get(stream_id)
                .is_some_and(|session| !ignored_generations.contains(&session.generation_id))
        });
        if mapped_work_remains
            || self
                .pending_claim_acquisitions
                .read()
                .map_or(true, |pending| {
                    pending.iter().any(|(id, _, _)| id == stream_id)
                })
            || self
                .pending_promotions
                .read()
                .map_or(true, |promotions| promotions.contains(stream_id))
        {
            return true;
        }
        let Some(storage) = self.persistence.as_ref() else {
            return false;
        };
        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
        match storage.has_durable_work(&session_id).await {
            Ok(remains) => remains,
            Err(error) => {
                debug!(
                    stream_id,
                    %error,
                    "could not prove SM durable work empty; retaining the shared claim"
                );
                true
            }
        }
    }

    fn forget_claim_locally_preserving_terminal_releases_locked(
        &self,
        stream_id: &str,
        preserved_releases: &[super::super::persistence::SmClaimFence],
    ) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(stream_id);
        }
        if let Ok(mut claimed) = self.claimed_sessions.write() {
            claimed.remove(stream_id);
        }
        // A reservation/acquisition/lookup represents an ownership CAS that
        // may still be in flight outside this shard. Preserve that ambiguous,
        // capacity-counted responsibility until its read-only reconciliation
        // proves loss; clearing it here lets a late CAS completion recreate an
        // unrepresented fresh claim after demotion.
        if let Ok(mut epoch_failures) = self.pending_epoch_failure_reconciliations.write() {
            epoch_failures.remove(stream_id);
        }
        let mut forgotten_fences = preserved_releases.to_vec();
        if let (
            Ok(_reservations),
            Ok(reclaimed_reservations),
            Ok(mut releases),
            Ok(mut fences),
            Ok(mut hydrations),
        ) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
            self.pending_reclaimed_hydrations.write(),
        ) {
            let active_reclaimed_reservation = reclaimed_reservations.get(stream_id).copied();
            hydrations.retain(|(id, _, reservation), _| {
                id != stream_id || Some(*reservation) == active_reclaimed_reservation
            });
            if let Some(fence) = fences.remove(stream_id) {
                forgotten_fences.push(fence);
            }
            releases.retain(|(id, pending_fence), _| {
                if id == stream_id {
                    if preserved_releases.contains(pending_fence) {
                        return true;
                    }
                    forgotten_fences.push(pending_fence.clone());
                    false
                } else {
                    true
                }
            });
        }
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
            forgotten_fences.sort_by(|left, right| {
                left.owner()
                    .node_id
                    .cmp(&right.owner().node_id)
                    .then_with(|| left.owner().node_epoch.cmp(&right.owner().node_epoch))
                    .then_with(|| left.epoch().cmp(&right.epoch()))
            });
            forgotten_fences.dedup();
            for fence in forgotten_fences {
                storage.evict_claim_cache(&session_id, &fence);
            }
        }
    }

    /// Retire a claimed session after this process rotates away from the
    /// incarnation recorded in `expected`. The exact fence is converted to
    /// terminal retry inventory before local resumability is removed, so a
    /// timeout, cancellation, or backend failure cannot strand an untracked
    /// old-incarnation claim. A stale caller cannot retire a replacement
    /// lifecycle because the active fence must still match exactly.
    pub async fn abandon_claim_after_identity_rotation(
        &self,
        stream_id: &str,
        expected: &super::super::persistence::SmClaimFence,
    ) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let stream_guard = stream_lock.lock().await;
        let active_matches = self
            .claim_fences
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(stream_id)
            == Some(expected);
        if !active_matches {
            return Err(SmRegistryError::Internal(
                "identity-rotation cleanup no longer owns the expected exact fence".to_string(),
            ));
        }
        let pending_promotion = {
            let promotions = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if promotions.current_reservation_active(stream_id) {
                return Err(SmRegistryError::Internal(
                    "identity-rotation cleanup cannot demote an active promotion lease".to_string(),
                ));
            }
            promotions.contains(stream_id)
        };
        if !self.try_record_terminal_claim_fence(stream_id, expected.clone()) {
            return Err(SmRegistryError::Internal(
                "identity-rotation cleanup could not retain the exact fence".to_string(),
            ));
        }
        if pending_promotion
            && !self
                .pending_promotions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .demote_for_successor(stream_id)
        {
            return Err(SmRegistryError::Internal(
                "identity-rotation cleanup could not unlink the pending promotion".to_string(),
            ));
        }
        self.sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        self.claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        drop(stream_guard);
        Ok(())
    }

    /// Reconcile an ISR epoch-lookup failure atomically with respect to node
    /// identity rotation. Reading the live identity inside the stream shard
    /// closes both rotation windows: a stale route snapshot cannot preserve
    /// an old-incarnation claim, and rotation cannot occur between the stale
    /// test and ordinary claimed-to-detached reinsertion.
    pub async fn reconcile_claim_after_epoch_lookup_failure(
        &self,
        stream_id: &str,
    ) -> Result<(), SmRegistryError> {
        // Publish retry responsibility before the first await. The websocket
        // task may be cancelled while waiting for the shard or backend read;
        // without this entry the session would remain claimed and invisible
        // to every future janitor pass.
        self.pending_epoch_failure_reconciliations
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .insert(stream_id.to_string());
        let stream_lock = self.stream_lock(stream_id)?;
        let stream_guard = stream_lock.lock().await;
        let fence = self
            .claim_fences
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(stream_id)
            .cloned();
        let Some(fence) = fence else {
            self.sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id);
            self.claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id);
            self.pending_epoch_failure_reconciliations
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id);
            return Ok(());
        };
        let identity_is_stale = self.node_identity.current() != *fence.owner();
        let backend_exact = if identity_is_stale {
            // Rotation is sufficient to stop local publication, but it is
            // not evidence that the old exact backend claim disappeared.
            // Preserve it for fresh-owner orphan discovery; the generic
            // handoff retry will later disprove an already-superseded F.
            true
        } else {
            let entity = sm_session_entity(stream_id);
            match tokio::time::timeout(
                CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                self.claim_store.current_claim(&entity),
            )
            .await
            {
                Ok(Ok(snapshot)) => snapshot.is_some_and(|snapshot| {
                    snapshot.owner == *fence.owner() && snapshot.claim_epoch == fence.epoch()
                }),
                Ok(Err(error)) => {
                    self.pending_epoch_failure_reconciliations
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                        .insert(stream_id.to_string());
                    return Err(SmRegistryError::Internal(format!(
                        "epoch-failure owner lookup failed: {error}"
                    )));
                }
                Err(_) => {
                    self.pending_epoch_failure_reconciliations
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                        .insert(stream_id.to_string());
                    return Err(SmRegistryError::Internal(
                        "epoch-failure owner lookup timed out".to_string(),
                    ));
                }
            }
        };
        let disproved_fence = self.node_identity.with_current(|current_identity| {
            if fence.owner() != current_identity {
                if !self.try_record_durable_claim_handoff(stream_id, fence.clone()) {
                    return Err(SmRegistryError::Internal(
                        "stale-identity cleanup could not retain the exact handoff".to_string(),
                    ));
                }
                self.sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .remove(stream_id);
                self.claimed_sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .remove(stream_id);
                return Ok(None);
            }

            if !backend_exact {
                if !self.try_record_durable_claim_handoff(stream_id, fence.clone()) {
                    return Err(SmRegistryError::Internal(
                        "lost-ownership cleanup could not retain the disproved exact fence"
                            .to_string(),
                    ));
                }
                self.sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .remove(stream_id);
                self.claimed_sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .remove(stream_id);
                return Ok(Some(fence.clone()));
            }

            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .get(stream_id)
                .cloned()
                .ok_or_else(|| {
                    SmRegistryError::Internal(
                        "epoch-failure reconciliation lost its claimed session".to_string(),
                    )
                })?;
            if claimed.is_expired() {
                if !self.move_live_successor_to_current_promotion(stream_id, claimed.generation_id)
                {
                    return Err(SmRegistryError::Internal(
                        "epoch-failure cleanup could not retain expired durable promotion"
                            .to_string(),
                    ));
                }
                return Ok(None);
            }
            let session = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id)
                .ok_or_else(|| {
                    SmRegistryError::Internal(
                        "claimed session disappeared under its stream shard".to_string(),
                    )
                })?;
            self.sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .insert(stream_id.to_string(), session);
            Ok(None)
        })?;
        self.pending_epoch_failure_reconciliations
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        drop(stream_guard);
        if let Some(fence) = disproved_fence {
            let local_cleared = self.retire_exact_claim_handoff_locally(stream_id, &fence);
            if local_cleared {
                if let Some(storage) = &self.persistence {
                    let session_id =
                        crate::pending_delivery::SmSessionId::new(stream_id.to_string());
                    storage.evict_claim_cache(&session_id, &fence);
                }
            }
        }
        Ok(())
    }

    /// Acquire exact generation authority before Q6 starts.
    pub async fn acquire_promotion_lease(
        &self,
        session: &DetachedSession,
    ) -> Result<Option<super::SmSessionPromotionLease>, SmRegistryError> {
        let stream_id = session.stream_id.as_str();
        let generation_id = session.generation_id;
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        let nonce = super::SmPromotionLeaseNonce::new();
        let active_claim_fence = self
            .claim_fences
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(stream_id)
            .cloned();
        let mut promotions = self
            .pending_promotions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let authority = match promotions.authority(stream_id, generation_id) {
            Some(authority) => authority,
            None => return Ok(None),
        };
        if !promotions.reserve_generation(stream_id, generation_id, nonce) {
            return Ok(None);
        }
        let claim_fence = match authority {
            super::SmSessionPromotionAuthority::CurrentDurable => match active_claim_fence {
                Some(fence) => promotions.retain_claim_fence(stream_id, generation_id, fence),
                None => promotions.claim_fence(stream_id, generation_id),
            },
            super::SmSessionPromotionAuthority::TerminalDurable => {
                // A terminal generation may share its stream id with a live
                // successor. Never borrow that successor's active fence;
                // retain only the exact fence captured when this terminal
                // carrier was archived or hydrated.
                promotions.claim_fence(stream_id, generation_id)
            }
            super::SmSessionPromotionAuthority::ObsoleteGeneration => None,
        };
        drop(promotions);
        let mut reservation_guard = PromotionReservationGuard::new(
            std::sync::Arc::clone(&self.pending_promotions),
            stream_id,
            generation_id,
            nonce,
        );
        if authority != super::SmSessionPromotionAuthority::ObsoleteGeneration
            && claim_fence.is_none()
            && self
                .persistence
                .as_ref()
                .is_some_and(|storage| storage.requires_exact_claim_fence())
        {
            return Err(SmRegistryError::Persistence(
                super::super::persistence::SmPersistenceError::NotOwner {
                    entity: sm_session_entity(stream_id),
                },
            ));
        }
        let lease = super::SmSessionPromotionLease {
            stream_id: crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
            generation_id,
            authority,
            claim_fence,
            nonce,
            pending_promotions: std::sync::Arc::clone(&self.pending_promotions),
            reservation_active: true,
        };
        reservation_guard.disarm();
        Ok(Some(lease))
    }

    /// Lock and revalidate one exact current-generation lease before a
    /// pending-delivery or persistence mutation. The returned guard owns the
    /// stream shard across the caller's storage await, so local demotion
    /// cannot race between validation and commit.
    pub async fn lock_current_promotion_mutation<'lease>(
        &self,
        lease: &'lease super::SmSessionPromotionLease,
    ) -> Result<super::SmCurrentPromotionMutationGuard<'lease>, SmRegistryError> {
        if !std::sync::Arc::ptr_eq(&self.pending_promotions, &lease.pending_promotions) {
            return Err(SmRegistryError::Internal(
                "promotion lease belongs to another registry".to_string(),
            ));
        }
        let operation = self
            .lock_session_operation(lease.session_id().as_str())
            .await?;
        let valid = lease.reservation_active
            && lease.authority == super::SmSessionPromotionAuthority::CurrentDurable
            && self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .current_reservation_matches(
                    lease.session_id().as_str(),
                    lease.generation_id,
                    lease.nonce,
                );
        if !valid {
            return Err(SmRegistryError::PromotionAuthorityLost);
        }
        Ok(super::SmCurrentPromotionMutationGuard {
            _operation: operation,
            lease,
        })
    }

    /// Lock and revalidate one exact terminal-generation lease. The guard's
    /// generation-qualified key is the only durable namespace this authority
    /// may mutate; same-stream successor state remains out of bounds.
    pub async fn lock_terminal_promotion_mutation<'lease>(
        &self,
        lease: &'lease super::SmSessionPromotionLease,
    ) -> Result<super::SmTerminalPromotionMutationGuard<'lease>, SmRegistryError> {
        if !std::sync::Arc::ptr_eq(&self.pending_promotions, &lease.pending_promotions) {
            return Err(SmRegistryError::Internal(
                "promotion lease belongs to another registry".to_string(),
            ));
        }
        let operation = self
            .lock_session_operation(lease.session_id().as_str())
            .await?;
        let valid = lease.reservation_active
            && lease.authority == super::SmSessionPromotionAuthority::TerminalDurable
            && self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .terminal_reservation_matches(
                    lease.session_id().as_str(),
                    lease.generation_id,
                    lease.nonce,
                );
        if !valid {
            return Err(SmRegistryError::PromotionAuthorityLost);
        }
        Ok(super::SmTerminalPromotionMutationGuard {
            _operation: operation,
            lease,
        })
    }

    pub async fn confirm_drained(
        &self,
        session: &DetachedSession,
    ) -> super::SmSessionDrainConfirmation {
        let Ok(Some(mut lease)) = self.acquire_promotion_lease(session).await else {
            return super::SmSessionDrainConfirmation::Unconfirmed;
        };
        self.confirm_drained_under(&mut lease).await
    }

    /// Delete/retire one exact generation. Current generations delete under
    /// their captured fence, then release the in-memory ClaimStore last.
    pub async fn confirm_drained_under(
        &self,
        lease: &mut super::SmSessionPromotionLease,
    ) -> super::SmSessionDrainConfirmation {
        self.confirm_drained_under_observing(lease, |_| {}).await
    }

    /// Delete/retire one exact generation and synchronously report any exact
    /// ClaimStore release outcome reached by that retirement. The observer
    /// runs in the same poll that completes the release, before this future
    /// can reach another cancellation point.
    pub async fn confirm_drained_under_observing<F>(
        &self,
        lease: &mut super::SmSessionPromotionLease,
        mut observe: F,
    ) -> super::SmSessionDrainConfirmation
    where
        F: FnMut(super::SmClaimReleaseRetryOutcome),
    {
        let stream_id = lease.stream_id.clone();
        let stream_id_str = stream_id.as_str();
        let generation_id = lease.generation_id;
        let Ok(stream_lock) = self.stream_lock(stream_id_str) else {
            return super::SmSessionDrainConfirmation::Unconfirmed;
        };
        let _stream_guard = stream_lock.lock().await;
        let reservation_valid = self
            .pending_promotions
            .read()
            .ok()
            .is_some_and(|promotions| match lease.authority {
                super::SmSessionPromotionAuthority::CurrentDurable => promotions
                    .current_reservation_matches(stream_id_str, generation_id, lease.nonce),
                super::SmSessionPromotionAuthority::TerminalDurable => promotions
                    .terminal_reservation_matches(stream_id_str, generation_id, lease.nonce),
                super::SmSessionPromotionAuthority::ObsoleteGeneration => {
                    promotions.authority(stream_id_str, generation_id)
                        == Some(super::SmSessionPromotionAuthority::ObsoleteGeneration)
                        && promotions.reservation_matches(stream_id_str, generation_id, lease.nonce)
                }
            });
        if !reservation_valid {
            return super::SmSessionDrainConfirmation::Unconfirmed;
        }
        if lease.authority == super::SmSessionPromotionAuthority::ObsoleteGeneration {
            let retired = self
                .pending_promotions
                .write()
                .ok()
                .and_then(|mut promotions| {
                    promotions.retire_under_reservation(stream_id_str, generation_id, lease.nonce)
                })
                == Some(false);
            if !retired {
                return super::SmSessionDrainConfirmation::Unconfirmed;
            }
            lease.reservation_active = false;
            if let Ok(mut retries) = self.pending_promotion_retries.write() {
                retries.remove_generation(stream_id_str, generation_id);
            }
            return super::SmSessionDrainConfirmation::ObsoleteGenerationRetired;
        }
        if lease.authority == super::SmSessionPromotionAuthority::CurrentDurable {
            let successor_present = self
                .sessions
                .read()
                .map_or(true, |sessions| sessions.contains_key(stream_id_str))
                || self
                    .claimed_sessions
                    .read()
                    .map_or(true, |sessions| sessions.contains_key(stream_id_str))
                || self
                    .pending_claim_acquisitions
                    .read()
                    .map_or(true, |pending| {
                        pending.iter().any(|(id, _, _)| id == stream_id_str)
                    })
                || self
                    .pending_promotion_retries
                    .read()
                    .map_or(true, |retries| {
                        retries
                            .get_generation(stream_id_str, generation_id)
                            .is_some()
                    });
            if successor_present {
                return super::SmSessionDrainConfirmation::Unconfirmed;
            }
        }
        let durable_delete = match lease.authority {
            super::SmSessionPromotionAuthority::CurrentDurable => {
                match lease.claim_fence.as_ref() {
                    Some(fence) => {
                        self.persist_delete_session_under_fence(stream_id_str, fence)
                            .await
                    }
                    None if self
                        .persistence
                        .as_ref()
                        .is_none_or(|storage| !storage.requires_exact_claim_fence()) =>
                    {
                        self.persist_delete_session(stream_id_str).await
                    }
                    None => Err(SmRegistryError::Internal(
                        "confirm_drained lacks an exact captured claim fence".to_string(),
                    )),
                }
            }
            super::SmSessionPromotionAuthority::TerminalDurable => {
                let key = super::super::persistence::SmTerminalGenerationKey::new(
                    stream_id.clone(),
                    generation_id,
                );
                match self.persistence.as_ref() {
                    Some(storage) => match lease.claim_fence.as_ref() {
                        Some(fence) => storage
                            .delete_terminal_generation_under_fence(&key, fence)
                            .await
                            .map_err(SmRegistryError::Persistence),
                        None if !storage.requires_exact_claim_fence() => storage
                            .delete_terminal_generation(&key)
                            .await
                            .map_err(SmRegistryError::Persistence),
                        None => Err(SmRegistryError::Internal(
                            "terminal confirm lacks an exact captured claim fence".to_string(),
                        )),
                    },
                    // Pure in-memory registries have no terminal table. The
                    // exact local inventory is the only carrier and its
                    // retirement is therefore the idempotent durable delete.
                    None => Ok(()),
                }
            }
            super::SmSessionPromotionAuthority::ObsoleteGeneration => {
                unreachable!("obsolete promotion generations return before durable retirement")
            }
        };
        match durable_delete {
            Ok(()) => {
                // The durable generation is now definitely gone. From this
                // point forward cancellation must retire its already-
                // promoted payload instead of making it retryable again.
                let post_delete = DeletedPromotionProbeGuard::new(self, lease);
                // A bare current row and any number of exact terminal rows
                // share one ClaimStore entity. Deleting one carrier never
                // authorizes claim release until persistence proves that no
                // durable work remains for the stream id. Probe failures
                // retain the exact claim for later reconciliation, but the
                // successfully promoted payload is terminally retired.
                let local_other_work =
                    self.local_other_work_may_remain(stream_id_str, generation_id);
                let durable_work_remains = match &self.persistence {
                    Some(storage) => match storage.has_durable_work(&stream_id).await {
                        Ok(remains) => remains || local_other_work,
                        Err(error) => {
                            debug!(
                                stream_id = %stream_id,
                                %error,
                                "durable SM retirement could not prove the stream empty"
                            );
                            return if post_delete.finish(true) {
                                super::SmSessionDrainConfirmation::PayloadRetiredClaimReconciliationPending
                            } else {
                                super::SmSessionDrainConfirmation::Unconfirmed
                            };
                        }
                    },
                    None => local_other_work,
                };
                let fence = post_delete.claim_fence.clone();
                if !durable_work_remains {
                    if let Some(fence) = fence.as_ref() {
                        if !self.try_record_terminal_claim_fence(stream_id_str, fence.clone()) {
                            return if post_delete.finish(true) {
                                super::SmSessionDrainConfirmation::PayloadRetiredClaimReconciliationPending
                            } else {
                                super::SmSessionDrainConfirmation::Unconfirmed
                            };
                        }
                    }
                }
                let authority = post_delete.authority;
                if !post_delete.finish(false) {
                    return super::SmSessionDrainConfirmation::Unconfirmed;
                }
                if !durable_work_remains {
                    let preserved_releases = self
                        .pending_claim_releases
                        .read()
                        .map(|pending| {
                            pending
                                .keys()
                                .filter(|(pending_stream_id, _)| pending_stream_id == stream_id_str)
                                .map(|(_, pending_fence)| pending_fence.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_else(|_| fence.iter().cloned().collect());
                    self.forget_claim_locally_preserving_terminal_releases_locked(
                        stream_id_str,
                        &preserved_releases,
                    );
                    if let Some(fence) = fence {
                        let outcome = self
                            .release_claim_store_entry_under(stream_id_str, fence)
                            .await;
                        observe(outcome);
                    }
                }
                match authority {
                    super::SmSessionPromotionAuthority::CurrentDurable => {
                        super::SmSessionDrainConfirmation::CurrentDurableConfirmed
                    }
                    super::SmSessionPromotionAuthority::TerminalDurable => {
                        super::SmSessionDrainConfirmation::TerminalDurableConfirmed
                    }
                    super::SmSessionPromotionAuthority::ObsoleteGeneration => unreachable!(
                        "obsolete promotion generations return before durable retirement"
                    ),
                }
            }
            Err(SmRegistryError::Persistence(
                super::super::persistence::SmPersistenceError::NotOwner { .. },
            )) => {
                if self.abandon_promotion_authority_locked(lease) {
                    super::SmSessionDrainConfirmation::AuthorityLost
                } else {
                    super::SmSessionDrainConfirmation::Unconfirmed
                }
            }
            Err(error) => {
                debug!(stream_id = %stream_id, %error, "durable SM retirement failed");
                super::SmSessionDrainConfirmation::Unconfirmed
            }
        }
    }

    /// Synchronously poison an exact promotion lease after a fenced backend
    /// proves ownership loss. A normal durable generation is retired after
    /// its old exact fence is retained. A carrier whose snapshot was proven
    /// never published is instead demoted to payload-only and left queued;
    /// in either non-retired case this returns `false` so callers keep their
    /// retry carrier armed.
    #[must_use = "the retry carrier may only be completed after authority retirement succeeds"]
    pub fn abandon_promotion_authority(&self, lease: &mut super::SmSessionPromotionLease) -> bool {
        self.abandon_promotion_authority_locked(lease)
    }

    fn abandon_promotion_authority_locked(
        &self,
        lease: &mut super::SmSessionPromotionLease,
    ) -> bool {
        if lease.authority == super::SmSessionPromotionAuthority::ObsoleteGeneration {
            return false;
        }
        let stream_id = lease.stream_id.clone();
        let stream_id_str = stream_id.as_str();
        let generation_id = lease.generation_id;
        let exact = self
            .pending_promotions
            .read()
            .ok()
            .is_some_and(|promotions| {
                promotions.reservation_matches(stream_id_str, generation_id, lease.nonce)
            });
        if !exact {
            return false;
        }
        // `NotOwner` proves only that this promotion can no longer mutate
        // the durable SM row. It does not prove that the exact ClaimStore
        // row captured by the lease disappeared: an identity rotation can
        // make persistence reject the old fence while that old-incarnation
        // claim still needs an exact release. Convert it to terminal retry
        // inventory before retiring promotion ownership.
        if let Some(expected) = lease.claim_fence.as_ref() {
            if !self.try_record_terminal_claim_fence(stream_id_str, expected.clone()) {
                return false;
            }
            if let Some(storage) = &self.persistence {
                storage.evict_claim_cache(&lease.stream_id, expected);
            }
        }
        if lease.authority == super::SmSessionPromotionAuthority::TerminalDurable {
            let retired = self
                .pending_promotions
                .write()
                .ok()
                .and_then(|mut promotions| {
                    promotions.retire_under_reservation(stream_id_str, generation_id, lease.nonce)
                })
                == Some(false);
            if !retired {
                return false;
            }
            if let Ok(mut retries) = self.pending_promotion_retries.write() {
                retries.remove_generation(stream_id_str, generation_id);
            }
            lease.reservation_active = false;
            return true;
        }
        let (retired, demoted_for_payload_retry) = self
            .pending_promotions
            .write()
            .ok()
            .map(|mut promotions| {
                if promotions.is_definitely_never_published(stream_id_str, generation_id)
                    || promotions.is_current(stream_id_str, generation_id) == Some(false)
                {
                    (
                        false,
                        promotions.demote_for_payload_retry_under_reservation(
                            stream_id_str,
                            generation_id,
                            lease.nonce,
                        ),
                    )
                } else {
                    (
                        promotions.retire_under_reservation(
                            stream_id_str,
                            generation_id,
                            lease.nonce,
                        ) == Some(true),
                        false,
                    )
                }
            })
            .unwrap_or((false, false));
        if demoted_for_payload_retry {
            lease.reservation_active = false;
            return false;
        }
        if !retired {
            return false;
        }
        if let Ok(mut retries) = self.pending_promotion_retries.write() {
            retries.remove_generation(stream_id_str, generation_id);
        }
        lease.reservation_active = false;
        true
    }

    /// Increment the persistent promotion-failure counter for
    /// `stream_id` and return the new value. Used by the SM-expiry
    /// janitor to detect runaway retry loops on permanent storage or
    /// blocklist failures.
    #[cfg(test)]
    pub async fn record_promotion_failure(&self, stream_id: &str) -> Result<u32, SmRegistryError> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(0);
        };
        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
        persistence
            .record_promotion_failure(&session_id)
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub async fn record_promotion_failure_under(
        &self,
        lease: &super::SmSessionPromotionLease,
    ) -> Result<u32, SmRegistryError> {
        let stream_lock = self.stream_lock(lease.stream_id.as_str())?;
        let _stream_guard = stream_lock.lock().await;
        let authorized = self
            .pending_promotions
            .read()
            .ok()
            .is_some_and(|promotions| match lease.authority {
                super::SmSessionPromotionAuthority::CurrentDurable => promotions
                    .current_reservation_matches(
                        lease.stream_id.as_str(),
                        lease.generation_id,
                        lease.nonce,
                    ),
                super::SmSessionPromotionAuthority::TerminalDurable => promotions
                    .terminal_reservation_matches(
                        lease.stream_id.as_str(),
                        lease.generation_id,
                        lease.nonce,
                    ),
                super::SmSessionPromotionAuthority::ObsoleteGeneration => false,
            });
        if !authorized {
            return Err(SmRegistryError::Internal(
                "promotion failure update lacks exact generation authority".to_string(),
            ));
        }
        let Some(persistence) = self.persistence.as_ref() else {
            return if lease.authority == super::SmSessionPromotionAuthority::TerminalDurable {
                self.pending_promotions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .record_failure_under_reservation(
                        lease.stream_id.as_str(),
                        lease.generation_id,
                        lease.nonce,
                    )
                    .ok_or_else(|| {
                        SmRegistryError::Internal(
                            "terminal promotion failure update lost exact authority".to_string(),
                        )
                    })
            } else {
                Ok(0)
            };
        };
        let session_id = lease.stream_id.clone();
        match lease.authority {
            super::SmSessionPromotionAuthority::CurrentDurable => {
                match lease.claim_fence.as_ref() {
                    Some(fence) => persistence
                        .record_promotion_failure_under_fence(&session_id, fence)
                        .await
                        .map_err(SmRegistryError::Persistence),
                    None if !persistence.requires_exact_claim_fence() => persistence
                        .record_promotion_failure(&session_id)
                        .await
                        .map_err(SmRegistryError::Persistence),
                    None => Err(SmRegistryError::Internal(
                        "promotion failure update lacks an exact captured claim fence".to_string(),
                    )),
                }
            }
            super::SmSessionPromotionAuthority::TerminalDurable => {
                let key = super::super::persistence::SmTerminalGenerationKey::new(
                    session_id,
                    lease.generation_id,
                );
                match lease.claim_fence.as_ref() {
                    Some(fence) => persistence
                        .record_terminal_promotion_failure_under_fence(&key, fence)
                        .await
                        .map_err(SmRegistryError::Persistence),
                    None if !persistence.requires_exact_claim_fence() => persistence
                        .record_terminal_promotion_failure(&key)
                        .await
                        .map_err(SmRegistryError::Persistence),
                    None => Err(SmRegistryError::Internal(
                        "terminal promotion failure update lacks an exact captured claim fence"
                            .to_string(),
                    )),
                }
            }
            super::SmSessionPromotionAuthority::ObsoleteGeneration => unreachable!(
                "obsolete promotion authority is rejected before durable failure recording"
            ),
        }
    }

    /// Delete promoted rows from one exact terminal generation only.
    pub async fn delete_terminal_unacked_sequences_under(
        &self,
        lease: &super::SmSessionPromotionLease,
        sequences: &[u32],
    ) -> Result<u64, SmRegistryError> {
        let stream_lock = self.stream_lock(lease.stream_id.as_str())?;
        let _stream_guard = stream_lock.lock().await;
        let authorized = lease.authority == super::SmSessionPromotionAuthority::TerminalDurable
            && self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .terminal_reservation_matches(
                    lease.stream_id.as_str(),
                    lease.generation_id,
                    lease.nonce,
                );
        if !authorized {
            return Err(SmRegistryError::PromotionAuthorityLost);
        }
        let Some(storage) = self.persistence.as_ref() else {
            return Ok(0);
        };
        if sequences.is_empty() {
            return Ok(0);
        }
        let key = super::super::persistence::SmTerminalGenerationKey::new(
            lease.stream_id.clone(),
            lease.generation_id,
        );
        match lease.claim_fence.as_ref() {
            Some(fence) => storage
                .delete_terminal_unacked_under_fence(&key, sequences, fence)
                .await
                .map_err(SmRegistryError::Persistence),
            None if !storage.requires_exact_claim_fence() => storage
                .delete_terminal_unacked(&key, sequences)
                .await
                .map_err(SmRegistryError::Persistence),
            None => Err(SmRegistryError::Internal(
                "terminal unacked-row delete lacks an exact captured claim fence".to_string(),
            )),
        }
    }

    /// Increment the process-local retry budget for an obsolete generation
    /// without touching the same-id successor's durable counter.
    pub fn record_obsolete_promotion_failure_under(
        &self,
        lease: &super::SmSessionPromotionLease,
    ) -> Result<u32, SmRegistryError> {
        if lease.authority != super::SmSessionPromotionAuthority::ObsoleteGeneration {
            return Err(SmRegistryError::Internal(
                "obsolete promotion failure update used durable mutation authority".to_string(),
            ));
        }
        self.pending_promotions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .record_failure_under_reservation(
                lease.stream_id.as_str(),
                lease.generation_id,
                lease.nonce,
            )
            .ok_or_else(|| {
                SmRegistryError::Internal(
                    "obsolete promotion failure update lacks exact generation authority"
                        .to_string(),
                )
            })
    }

    /// Drain expired sessions from the in-memory view. Returns the
    /// drained sessions for the caller to run Q6 promotion on.
    ///
    /// **Does NOT delete durable rows.** The caller MUST invoke
    /// [`Self::confirm_drained`] for each session AFTER its unacked
    /// queue has been successfully promoted. If promotion fails
    /// mid-batch, the failed sessions' durable rows survive so a
    /// restart can retry. (Copilot review on PR #346: previous
    /// up-front delete lost stanzas on partial-promotion failure.)
    pub async fn drain_expired(&self) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let retry_count = self
            .pending_promotion_retries
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .len();
        let mut drained = DrainedSessionBatch::new(self, retry_count);
        self.drain_promotion_retries_into(&mut drained).await?;
        let expired_ids: Vec<String> = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions
                .iter()
                .filter(|(_, session)| session.is_expired())
                .map(|(stream_id, _)| stream_id.clone())
                .collect()
        };
        for stream_id in &expired_ids {
            let stream_lock = self.stream_lock(stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let removed = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let mut promotions = self
                    .pending_promotions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                if promotions.current_reservation_active(stream_id) {
                    continue;
                }
                let removed = match sessions.get(stream_id) {
                    Some(session) if session.is_expired() => sessions.remove(stream_id),
                    _ => None,
                };
                match removed {
                    Some(session) if promotions.insert_current(&session) => Some(session),
                    Some(session) => {
                        sessions.insert(stream_id.clone(), session);
                        None
                    }
                    None => None,
                }
            };
            if let Some(session) = removed {
                drained.push(session);
            }
        }
        if !drained.sessions.is_empty() {
            debug!(removed = drained.len(), "Cleaned up expired SM sessions");
        }
        Ok(drained.finish())
    }

    /// Re-insert a session whose XEP-0198 §5 promotion failed back
    /// into the in-memory map, forced expired, WITHOUT touching
    /// durable state (mirrors the #1098 hydrate-expired pattern).
    ///
    /// `drain_expired` scans only memory, so a promotion failure that
    /// left the session out of the map would strand its durable rows
    /// until the next restart — contradicting the janitor's "retried
    /// on the next pass" contract. Forcing expiry keeps the session
    /// non-resumable on the wire (peek/take/claim all gate on
    /// `is_expired()`) while making the janitor's next tick retry the
    /// promote → confirm chain; the persistent promotion-failure
    /// counter still dead-letters runaway loops.
    ///
    /// `detached_at` is deliberately preserved (round-2 review R3): a
    /// reinsert that refreshed it to ≈now made repeatedly-failing
    /// sessions immortal against the max_sessions min-by-detached_at
    /// eviction, sacrificing healthy resumable sessions under a
    /// degraded backend. Forcing `max_resume_time = 0` alone keeps the
    /// session expired (its true detach time is already in the past),
    /// so peek/take/claim still refuse it while the janitor retries.
    #[cfg(test)]
    pub async fn reinsert_for_retry(
        &self,
        mut session: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(&session.stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        self.reconcile_retry_payload(&mut session).await;
        self.reinsert_for_retry_unlocked(session)
    }

    pub async fn reinsert_for_retry_under(
        &self,
        lease: &mut super::SmSessionPromotionLease,
        mut session: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(&session.stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        let (authority, reservation_valid) = {
            let promotions = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            (
                promotions.authority(&session.stream_id, session.generation_id),
                promotions.reservation_matches(
                    lease.stream_id.as_str(),
                    lease.generation_id,
                    lease.nonce,
                ),
            )
        };
        if lease.stream_id.as_str() != session.stream_id
            || lease.generation_id != session.generation_id
            || authority != Some(lease.authority)
            || !reservation_valid
        {
            return Err(SmRegistryError::Internal(
                "promotion retry restore lacks exact generation authority".to_string(),
            ));
        }
        self.reconcile_retry_payload(&mut session).await;
        self.reinsert_for_retry_unlocked(session)
    }

    async fn reconcile_retry_payload(&self, session: &mut DetachedSession) {
        // Retry-horizon guard (adversarial-review finding D): the
        // uncounted retry path (record_promotion_failure itself
        // erroring) re-inserted the in-memory copy VERBATIM forever.
        // A XEP-0424/0425 scrub that ran while the session was off-map
        // (phase 4) deleted only the durable rows, so once the
        // RECENT_TOMBSTONE_TTL expired the retained in-memory copy
        // promoted the retracted stanza anyway. Diff the queue against
        // the durable rows under the stream lock and drop entries whose
        // rows no longer exist — durable storage is the scrub's source
        // of truth. A read failure keeps the queue verbatim
        // (at-least-once beats dropping stanzas on a storage blip).
        // The diff is authoritative only when the durable session row
        // exists (round-6 review): a phase-4 scrub deletes unacked rows
        // but leaves the session row, whereas a session whose
        // store_session snapshot write FAILED has neither — dropping
        // its queue would silently lose messages on the very storage
        // blip this path tolerates.
        let authority =
            self.pending_promotions.read().ok().and_then(|promotions| {
                promotions.authority(&session.stream_id, session.generation_id)
            });
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(session.stream_id.clone());
            let durable_sequences = match authority {
                Some(super::SmSessionPromotionAuthority::CurrentDurable) => {
                    match storage.get_session(&session_id).await {
                        Ok(Some(_)) => storage.list_unacked(&session_id).await.map(Some),
                        Ok(None) => Ok(None),
                        Err(error) => Err(error),
                    }
                }
                Some(super::SmSessionPromotionAuthority::TerminalDurable) => {
                    let key = super::super::persistence::SmTerminalGenerationKey::new(
                        session_id,
                        session.generation_id,
                    );
                    storage.get_terminal_generation(&key).await.map(|terminal| {
                        terminal.map(|terminal| terminal.snapshot().unacked().to_vec())
                    })
                }
                Some(super::SmSessionPromotionAuthority::ObsoleteGeneration) | None => return,
            };
            match durable_sequences {
                Ok(Some(rows)) => {
                    let durable: std::collections::HashSet<u32> =
                        rows.iter().map(|row| row.sequence).collect();
                    session
                        .unacked_stanzas
                        .retain(|entry| durable.contains(&entry.sequence));
                }
                Ok(None) => {
                    debug!(
                        stream_id = %session.stream_id,
                        "reinsert_for_retry: no durable generation row; keeping the \
                         in-memory queue verbatim (at-least-once)"
                    );
                }
                Err(error) => {
                    debug!(
                        stream_id = %session.stream_id,
                        error = %error,
                        "reinsert_for_retry: durable generation read failed; keeping the \
                         in-memory queue verbatim (at-least-once)"
                    );
                }
            }
        }
    }

    /// Map-insert half of [`Self::reinsert_for_retry`]: force the
    /// session expired and publish it in the detached map WITHOUT
    /// taking its stream shard lock. Only for callers that already
    /// hold a (possibly different stream's) shard lock —
    /// `store_session`'s snapshot-failure path re-inserts the sessions
    /// it displaced while still holding the NEW stream's shard lock,
    /// and two crossed store_session calls taking each other's shard
    /// locks would deadlock.
    pub(super) fn reinsert_for_retry_unlocked(
        &self,
        mut session: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        session.max_resume_time = Some(0);
        let stream_id = session.stream_id.clone();
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let promotions = self
            .pending_promotions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if !promotions.contains_generation(&stream_id, session.generation_id) {
            return Err(SmRegistryError::Internal(
                "promotion retry generation is no longer pending".to_string(),
            ));
        }
        let mut retries = self
            .pending_promotion_retries
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if promotions.current_reservation_active(&stream_id)
            || promotions.is_current(&stream_id, session.generation_id) != Some(true)
            || sessions.contains_key(&stream_id)
        {
            retries.insert(session);
        } else {
            sessions.insert(stream_id, session);
        }
        Ok(())
    }

    /// Synchronous cancellation fallback for a server-side promotion task.
    /// The durable row and exact claim remain intact; forcing the local copy
    /// expired makes the next SM janitor sweep retry promote → confirm.
    pub fn retain_pending_promotion_for_retry(
        &self,
        session: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let stream_id = session.stream_id.clone();
        let promotions = self
            .pending_promotions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if !promotions.contains_generation(&stream_id, session.generation_id) {
            return Ok(());
        }
        self.pending_promotion_retries
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .insert(session);
        Ok(())
    }

    /// Register one exact non-resumable terminal generation for Q6.
    ///
    /// Returns `true` only when restart/reclaimed hydration inserted a new
    /// generation and this method also parked its retry payload. A live
    /// same-id replacement already has the predecessor in `PendingPromotions`;
    /// that entry is upgraded in place and returns `false`, leaving the live
    /// caller as the sole owner of its handed-off payload.
    pub fn retain_terminal_durable_promotion(
        &self,
        session: DetachedSession,
        promotion_attempts: u32,
        claim_fence: super::super::persistence::SmClaimFence,
    ) -> Result<bool, SmRegistryError> {
        let mut promotions = self
            .pending_promotions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let mut retries = self
            .pending_promotion_retries
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        match promotions.retain_terminal_durable(&session, promotion_attempts, claim_fence) {
            super::core::TerminalPromotionRetention::Inserted => {
                retries.insert(session);
                Ok(true)
            }
            super::core::TerminalPromotionRetention::Upgraded
            | super::core::TerminalPromotionRetention::Unchanged => Ok(false),
        }
    }

    /// Park one exact generation after both an atomic publication result and
    /// its marker read were ambiguous.
    ///
    /// The same-stream shard must already be held by the lifecycle caller.
    /// This method is synchronous so that A and B can be parked before that
    /// shard is released. It never makes an unknown generation leasable and
    /// removes only the matching generation from resumable maps.
    pub fn park_publication_unknown(
        &self,
        session: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let claim_fence = self
            .claim_fences
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(&session.stream_id)
            .cloned();
        let (mut sessions, mut claimed, mut promotions, mut retries) = (
            self.sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_promotions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_promotion_retries
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
        );
        if promotions.generation_reservation_active(&session.stream_id, session.generation_id) {
            return Err(SmRegistryError::Internal(
                "cannot park publication-unknown state while its promotion lease is active"
                    .to_string(),
            ));
        }
        if sessions
            .get(&session.stream_id)
            .is_some_and(|current| current.generation_id == session.generation_id)
        {
            sessions.remove(&session.stream_id);
        }
        if claimed
            .get(&session.stream_id)
            .is_some_and(|current| current.generation_id == session.generation_id)
        {
            claimed.remove(&session.stream_id);
        }
        promotions.park_publication_unknown(&session, claim_fence);
        retries.insert(session);
        Ok(())
    }

    /// Restore the predecessor after an ambiguous same-id replacement is
    /// proven definitely uncommitted. The caller already holds the stream
    /// shard and has dealt with the exact successor publication guard.
    pub fn restore_resumable_after_uncommitted_replace(
        &self,
        predecessor: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let stream_id = predecessor.stream_id.clone();
        let generation_id = predecessor.generation_id;
        let (mut sessions, mut claimed, mut promotions, mut retries) = (
            self.sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_promotions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_promotion_retries
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
        );
        if promotions.generation_reservation_active(&stream_id, generation_id) {
            return Err(SmRegistryError::Internal(
                "cannot restore a predecessor while its promotion lease is active".to_string(),
            ));
        }
        if sessions
            .get(&stream_id)
            .is_some_and(|session| session.generation_id != generation_id)
            || claimed
                .get(&stream_id)
                .is_some_and(|session| session.generation_id != generation_id)
        {
            return Err(SmRegistryError::Internal(
                "cannot restore a predecessor over a different live generation".to_string(),
            ));
        }
        if promotions.contains_generation(&stream_id, generation_id)
            && !promotions.remove_unreserved_generation(&stream_id, generation_id)
        {
            return Err(SmRegistryError::Internal(
                "could not retire predecessor promotion inventory".to_string(),
            ));
        }
        claimed.remove(&stream_id);
        sessions.insert(stream_id.clone(), predecessor);
        retries.remove_generation(&stream_id, generation_id);
        Ok(())
    }

    /// Ensure this node holds `stream_id`'s `ClaimStore` claim at
    /// `<enable/>` time (ADR-0017 Phase 3 Slice 6, element 8: "claims row
    /// created at `<enable/>` time"). `ensure_claimed`, not a bare
    /// `acquire` (deviation 26/Slice 5's own forward note): a fresh
    /// `<enable/>` for a stream id this node has never seen still gets a
    /// plain fresh claim (there is no existing row to self-reacquire
    /// against), but `ensure_claimed`'s self-idempotence is exactly what
    /// keeps this call from spuriously conflicting with the detach-time
    /// `acquire_claim_store_entry_for_detach` call for the *same*
    /// stream-id on this same node later in this session's lifetime — the
    /// two call sites coexist with no explicit hand-off protocol between
    /// them precisely because both go through the same idempotent-for-self
    /// primitive.
    ///
    /// This admission is authoritative: the caller must not enable SM unless
    /// it receives a publication guard and must retain that guard through the
    /// synchronous `StreamManagementState::enable` publication. Proceeding
    /// after a timeout/error would create an
    /// enabled session that cross-node resume cannot discover and would also
    /// leave the socket blocked indefinitely when the backend hangs. Called
    /// with no stream-shard lock held: `stream_id` is freshly minted and
    /// cannot yet appear in `sessions`/`claimed_sessions`, so there is nothing
    /// else to coordinate with under that lock.
    pub async fn ensure_session_claim(
        &self,
        stream_id: &str,
    ) -> Option<crate::ownership::CurrentNodeIdentityGuard> {
        if !self.reserve_claim_fence_capacity(stream_id) {
            tracing::warn!(stream_id = %stream_id, "handle_sm_enable: exact-release backlog capacity exhausted");
            return None;
        }
        let entity = sm_session_entity(stream_id);
        let identity = self.node_identity.current();
        let mut acquisition_guard = PendingAcquisitionGuard::new(
            self,
            stream_id,
            identity.clone(),
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        );
        let claimed = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.ensure_claimed(&entity, &identity),
        )
        .await
        {
            Ok(Ok(epoch)) => {
                let fence = super::super::persistence::SmClaimFence::new(identity.clone(), epoch);
                let Some(publication_guard) = self.node_identity.guard_if_current(&identity).await
                else {
                    if self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                        acquisition_guard.disarm();
                        self.release_claim_store_entry_under(stream_id, fence).await;
                    }
                    return None;
                };
                let recorded = self.try_record_claim_fence(stream_id, fence.clone());
                if !recorded {
                    drop(publication_guard);
                    if self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                        acquisition_guard.disarm();
                        self.release_claim_store_entry_under(stream_id, fence).await;
                    }
                    return None;
                } else {
                    Some(publication_guard)
                }
            }
            Ok(Err(error)) => {
                self.cancel_claim_fence_reservation(stream_id);
                tracing::warn!(
                    stream_id = %stream_id,
                    %error,
                    "handle_sm_enable: ClaimStore ensure_claimed failed; rejecting SM enable"
                );
                None
            }
            Err(_) => {
                tracing::warn!(stream_id = %stream_id, "handle_sm_enable: ClaimStore ensure_claimed timed out");
                if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                    pending.insert((
                        stream_id.to_string(),
                        identity.clone(),
                        PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
                    ));
                }
                self.reconcile_uncertain_claim_acquisition(
                    stream_id,
                    identity,
                    PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
                )
                .await;
                None
            }
        };
        acquisition_guard.disarm();
        claimed
    }

    /// Atomically claim a resumable session for a single resume attempt.
    ///
    /// Claimed sessions stay writable by detached fanout so stanzas routed
    /// during the claim-to-registration handoff can be merged into the final
    /// replay batch before the claim is completed.
    ///
    /// The injected `ClaimStore` is the **authority** on this entity's
    /// ownership claim (ADR-0017 Phase 3 Slice 1, Q2 "retrofit, not wrap"):
    /// a granted [`ClaimStore::acquire`](crate::ownership::ClaimStore::acquire)
    /// is what makes the in-memory claim below real, and
    /// [`ClaimError::AlreadyClaimed`] reproduces the pre-Slice-1
    /// already-claimed outcome (`Ok(None)`, no map mutation) exactly —
    /// every `ClaimStore` implementation enforces the identical single-node
    /// "one live claim per entity" invariant the `claimed_sessions` map used
    /// to enforce by itself. `sessions`/`claimed_sessions` now hold session
    /// *state* only; they are never consulted to decide whether a claim is
    /// granted.
    pub async fn claim_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        match self.claim_session_typed(stream_id).await? {
            ClaimSessionOutcome::Claimed(session) => Ok(Some(*session)),
            ClaimSessionOutcome::MissingOrExpired | ClaimSessionOutcome::LostClaim => Ok(None),
        }
    }

    /// Internal claim variant that preserves whether `None` means the
    /// detached state disappeared/expired or exact backend ownership was
    /// lost. The latter has already retired only exact local state and leaves
    /// any surviving backend claim available to durable-recovery discovery.
    pub(super) async fn claim_session_typed(
        &self,
        stream_id: &str,
    ) -> Result<ClaimSessionOutcome, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;

        // Peek (not remove): is there a live, unexpired session to claim at
        // all? A session that doesn't exist, or is already expired, is not
        // something the pre-Slice-1 semantics would ever have queried a
        // claim for either — this filters those cases before the store is
        // even asked, it does not itself decide the claim.
        let peeked = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.get(stream_id).cloned()
        };
        let Some(session) = peeked else {
            return Ok(ClaimSessionOutcome::MissingOrExpired);
        };
        if session.is_expired() {
            // Left untouched in `sessions` (never removed) so the
            // janitor's next `drain_expired` pass still finds it — a
            // #1098-hydrated expired session must stay visible for its
            // unacked queue to run the XEP-0198 §5 promote → confirm
            // chain.
            return Ok(ClaimSessionOutcome::MissingOrExpired);
        }

        let entity = sm_session_entity(stream_id);
        // ADR-0017 Phase 3 Slice 5: `ensure_claimed`, not a bare `acquire`.
        // Under the acquire-then-hydrate/acquire-on-detach invariant this
        // slice establishes (every entry in `sessions` is backed by a held
        // `ClaimStore` claim — see `core.rs::restore_from_persistence` and
        // `trait_impl.rs::store_session`), a resume attempt against a
        // session THIS node already claimed (the overwhelmingly common case
        // — the node that hydrated/detached it is usually the one a
        // resuming client reconnects to) must self-reacquire idempotently
        // rather than spuriously failing with `AlreadyClaimed` against its
        // own row. A genuinely different node's claim still correctly fails
        // here (`ensure_claimed` only self-reacquires for an exact
        // node/epoch match).
        let identity = self.node_identity.current();
        if !self.reserve_claim_fence_capacity(stream_id) {
            return Err(SmRegistryError::Internal(
                "claim_session: exact ownership capacity exhausted".to_string(),
            ));
        }
        let mut acquisition_guard = PendingAcquisitionGuard::new(
            self,
            stream_id,
            identity.clone(),
            PendingClaimAcquisitionDisposition::RetainDetachedSession(session.generation_id),
        );
        // FIX 5: bounded — this call runs under `stream_id`'s shard lock
        // (`_stream_guard`, held since the peek above), and that lock is
        // shared by every other stream id hashing to the same shard (see
        // `CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`'s doc comment for the
        // shard-fan-in rationale). A hung `ensure_claimed` call must fail
        // this resume attempt typed rather than stall every other stream id
        // sharing this shard indefinitely.
        let epoch = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.ensure_claimed(&entity, &identity),
        )
        .await
        {
            Ok(Ok(epoch)) => epoch,
            Ok(Err(ClaimError::AlreadyClaimed | ClaimError::Conflict | ClaimError::Draining)) => {
                let active_fence = self
                    .claim_fences
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .get(stream_id)
                    .cloned();
                if let Some(active_fence) = active_fence {
                    if !self.try_record_terminal_claim_fence(stream_id, active_fence.clone()) {
                        return Err(SmRegistryError::Internal(
                            "claim_session: lost ownership but could not retain its exact fence"
                                .to_string(),
                        ));
                    }
                    acquisition_guard.disarm();
                    self.forget_claim_locally_locked(stream_id, Some(&active_fence));
                } else {
                    acquisition_guard.disarm();
                    self.cancel_claim_fence_reservation(stream_id);
                    self.forget_claim_locally_locked(stream_id, None);
                }
                return Ok(ClaimSessionOutcome::LostClaim);
            }
            Ok(Err(other)) => {
                return Err(SmRegistryError::Internal(format!(
                    "claim_session: ClaimStore ensure_claimed returned an ambiguous failure; \
                     retaining acquisition responsibility for read-only reconciliation: {other}"
                )));
            }
            Err(_timeout) => {
                return Err(SmRegistryError::Internal(format!(
                    "claim_session: ClaimStore ensure_claimed timed out after \
                     {CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT:?} while holding this stream's \
                     shard lock (FIX 5); resume attempt fails rather than stalling every \
                     other stream id sharing this lock"
                )));
            }
        };
        let Some(publication_guard) = self.node_identity.guard_if_current(&identity).await else {
            let fence = super::super::persistence::SmClaimFence::new(identity, epoch);
            if !self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                return Err(SmRegistryError::Internal(
                    "claim_session: identity rotated but exact release could not be retained"
                        .to_string(),
                ));
            }
            acquisition_guard.disarm();
            self.forget_claim_locally_locked(stream_id, Some(&fence));
            return Ok(ClaimSessionOutcome::LostClaim);
        };

        // The store granted the claim: perform the matching in-memory move.
        // Most same-stream mutators share this shard, but a detach for a
        // different stream can evict this entry by full JID while holding
        // that other stream's shard. Re-check presence and expiry under the
        // sessions-map write lock. An expired retry entry is restored in the
        // same critical section so the displacement janitor keeps ownership
        // of its promote/confirm path.
        let removed = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            match sessions.remove(stream_id) {
                Some(session) if session.is_expired() => {
                    sessions.insert(stream_id.to_string(), session);
                    None
                }
                other => other,
            }
        };
        let Some(session) = removed else {
            let fence = super::super::persistence::SmClaimFence::new(identity, epoch);
            if !self.try_record_claim_fence(stream_id, fence) {
                return Err(SmRegistryError::Internal(
                    "claim_session: displaced session lost its exact ownership inventory"
                        .to_string(),
                ));
            }
            acquisition_guard.disarm();
            drop(publication_guard);
            return Ok(ClaimSessionOutcome::MissingOrExpired);
        };
        {
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed.insert(stream_id.to_string(), session.clone());
        }
        let fence = super::super::persistence::SmClaimFence::new(identity, epoch);
        let recorded = self.try_record_claim_fence(stream_id, fence.clone());
        drop(publication_guard);
        if !recorded {
            if let Ok(mut claimed) = self.claimed_sessions.write() {
                claimed.remove(stream_id);
            }
            if let Ok(mut sessions) = self.sessions.write() {
                sessions.insert(stream_id.to_string(), session);
            }
            if self.try_record_terminal_claim_fence_preserving_reservation(stream_id, fence) {
                return Err(SmRegistryError::Internal(
                    "claim_session: exact fence publication deferred for owned recovery"
                        .to_string(),
                ));
            }
            return Err(SmRegistryError::Internal(
                "claim_session: exact ownership publication and terminal retention both failed"
                    .to_string(),
            ));
        }
        acquisition_guard.disarm();
        Ok(ClaimSessionOutcome::Claimed(Box::new(session)))
    }

    /// Release a previously claimed session without consuming it.
    ///
    /// **ADR-0017 Phase 3 Slice 5 invariant**: an entry present in
    /// `sessions` is always backed by a held `ClaimStore` claim (see
    /// `core.rs::restore_from_persistence` and
    /// `trait_impl.rs::store_session`, which both acquire the claim before
    /// inserting). A session whose claim (not resume) attempt is merely
    /// aborted goes back into `sessions` here **still owned by this node**
    /// — its `ClaimStore` claim must therefore be KEPT, not released, or a
    /// concurrent claim-scoped hydration elsewhere (another node's restore
    /// pass, or the orphan reaper) could observe the entity as unclaimed
    /// and take it over while this node still holds it in memory, exactly
    /// the double-ownership hazard acquire-then-hydrate exists to prevent.
    /// Expiry while a resume attempt owns the session does not make its
    /// unacknowledged queue disposable. The expired session is reinserted as
    /// well: it remains non-resumable, but becomes visible to the janitor's
    /// drain -> promote -> confirm chain, which retires its durable row and
    /// releases the claim only after every same-stream generation is gone.
    ///
    /// When the entry is absent from `claimed_sessions`, release is allowed
    /// only after both local lifecycle inventory and persistence prove the
    /// stream empty. This protects terminal and publication-unknown
    /// generations that share the stream-level claim.
    pub async fn release_claim(&self, stream_id: &str) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        self.release_claim_locked(stream_id).await
    }

    /// Return a claimed session to the resumable pool while reusing the
    /// caller's shard and current-incarnation authorities.
    pub async fn release_claim_with_authority(
        &self,
        operation: super::SmSessionOperationGuard,
        authority: &crate::ownership::CurrentNodeIdentityGuard,
    ) -> Result<(), SmRegistryError> {
        self.validate_operation_authority(&operation, authority)?;
        self.release_claim_locked(&operation.stream_id).await
    }

    async fn release_claim_locked(&self, stream_id: &str) -> Result<(), SmRegistryError> {
        let reinserted = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let session = claimed.remove(stream_id);
            match session {
                Some(session) => {
                    sessions.insert(stream_id.to_string(), session);
                    true
                }
                None => false,
            }
        };
        if !reinserted && !self.any_durable_work_may_remain(stream_id).await {
            self.release_claim_store_entry(stream_id).await;
        }
        Ok(())
    }

    /// Acquire (or self-reacquire) `stream_id`'s `ClaimStore` claim and
    /// record the granted epoch, for a session newly entering `sessions`
    /// from a live detach (ADR-0017 Phase 3 Slice 5's acquire-on-detach
    /// half of the acquire-then-hydrate/acquire-on-detach invariant —
    /// `core.rs::restore_from_persistence` is the other half, for sessions
    /// entering `sessions` at startup instead of at detach time).
    ///
    /// The result is three-way rather than best-effort. A definite rejection
    /// removes local resumability while retaining the complete payload for
    /// Q6 promotion. A backend/timeout ambiguity keeps both the successor and
    /// a bounded reconciliation marker, but is not proof that a remote node
    /// may steal the session. Only `Established` publishes an exact active
    /// fence and authorizes a successful force-detach acknowledgement.
    pub(super) async fn acquire_claim_store_entry_for_detach(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        reservation: DetachClaimFenceReservation,
    ) -> DetachClaimAcquisitionOutcome {
        let entity = sm_session_entity(stream_id);
        let identity = self.node_identity.current();
        let mut acquisition_guard = PendingAcquisitionGuard::new(
            self,
            stream_id,
            identity.clone(),
            PendingClaimAcquisitionDisposition::RetainDetachedSession(generation_id),
        );
        // FIX 5: bounded — this call runs under `stream_id`'s shard lock
        // (`store_session` holds it for the whole function, including this
        // call), shared by every other stream id hashing to the same shard
        // (see `CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`'s doc comment). Best
        // A timeout is commit-ambiguous: the guard publishes reconciliation
        // responsibility before returning, and the caller must not emit a
        // force-detach success acknowledgement.
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.ensure_claimed(&entity, &identity),
        )
        .await
        {
            Ok(Ok(epoch)) => {
                let fence = super::super::persistence::SmClaimFence::new(identity.clone(), epoch);
                let Some(publication_guard) = self.node_identity.guard_if_current(&identity).await
                else {
                    if self.try_record_terminal_claim_fence_for_detach(
                        stream_id,
                        fence.clone(),
                        reservation,
                    ) {
                        acquisition_guard.disarm();
                        return DetachClaimAcquisitionOutcome::Rejected(
                            DetachClaimRejection::PublicationAuthorityLost,
                        );
                    }
                    return DetachClaimAcquisitionOutcome::AmbiguousTracked;
                };
                let recorded = self.try_record_claim_fence(stream_id, fence.clone());
                drop(publication_guard);
                if !recorded {
                    // This fence belongs to the current, fresh node. Preserve
                    // both the exact claim and the acquisition reservation so
                    // reconciliation can retry active-fence publication; a
                    // stale-owner orphan sweep cannot steal from a fresh
                    // lease, and releasing here would hide the committed row.
                    if self.try_record_terminal_claim_fence_preserving_reservation(stream_id, fence)
                    {
                        return DetachClaimAcquisitionOutcome::AmbiguousTracked;
                    }
                    return DetachClaimAcquisitionOutcome::AmbiguousTracked;
                }
                acquisition_guard.disarm();
                DetachClaimAcquisitionOutcome::Established
            }
            Ok(Err(error)) => {
                if let Some(rejection) = definite_detach_claim_rejection(&error) {
                    reservation.cancel_if_owned(self, stream_id);
                    acquisition_guard.disarm();
                    tracing::warn!(
                        stream_id = %stream_id,
                        %error,
                        ?rejection,
                        "store_session: detached-session claim was definitively rejected"
                    );
                    DetachClaimAcquisitionOutcome::Rejected(rejection)
                } else {
                    tracing::warn!(
                        stream_id = %stream_id,
                        %error,
                        "store_session: detached-session claim result is ambiguous; \
                         retaining bounded reconciliation responsibility"
                    );
                    DetachClaimAcquisitionOutcome::AmbiguousTracked
                }
            }
            Err(_timeout) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "store_session: ClaimStore ensure_claimed timed out while holding this \
                     stream's shard lock (FIX 5); retaining bounded reconciliation responsibility"
                );
                DetachClaimAcquisitionOutcome::AmbiguousTracked
            }
        }
    }

    /// Release `stream_id`'s `ClaimStore` entry under the immutable
    /// owner+epoch fence this registry last observed for it.
    /// `pub(super)` because every removal from `claimed_sessions` must
    /// release its claim, and two of those paths live in `trait_impl`
    /// (`store_session`'s jid-collision eviction, `take_session`).
    pub(super) async fn release_claim_store_entry(&self, stream_id: &str) {
        let fence = self
            .claim_fences
            .read()
            .ok()
            .and_then(|fences| fences.get(stream_id).cloned());
        if let Some(fence) = fence {
            self.release_claim_store_entry_under(stream_id, fence).await;
        } else {
            debug!(
                stream_id = %stream_id,
                "ClaimStore release skipped because no immutable owner+epoch fence was recorded"
            );
        }
    }

    /// Release `stream_id`'s `ClaimStore` entry under an explicitly-known
    /// fence (the caller already holds it, so there is nothing to look up in
    /// `claim_fences`). `release_exact` distinguishes an observed backend
    /// release from proof that the fence is no longer owned; an error or
    /// timeout retains the exact handoff for another retry.
    /// `pub(super)`: `core.rs::restore_from_persistence` (ADR-0017 Phase 3
    /// Slice 5) also releases an already-known epoch directly (a
    /// just-claimed, never-hydrated poison-pill row), without going through
    /// `claim_fences` first.
    pub(super) async fn release_claim_store_entry_under(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> super::SmClaimReleaseRetryOutcome {
        // Convert an active exact fence into supervised handoff state before
        // deciding whether a release may be issued. A concurrent same-stream
        // acquisition reservation can block issue-marking; in that case the
        // retained marker, not an otherwise-unscanned active fence, owns the
        // later retry after the reservation resolves.
        if !self.try_record_terminal_claim_fence_preserving_reservation(stream_id, fence.clone()) {
            tracing::warn!(
                stream_id = %stream_id,
                "ClaimStore release skipped because exact handoff responsibility could not be recorded"
            );
            return super::SmClaimReleaseRetryOutcome::Retained;
        }
        // Publish the possible-late-completion state before the release
        // future is first polled. Cancellation, timeout, and opaque backend
        // errors must never leave an issued release looking reusable.
        if !self.mark_claim_release_may_complete(stream_id, &fence) {
            tracing::warn!(
                stream_id = %stream_id,
                "ClaimStore release skipped because issue responsibility could not be recorded"
            );
            return super::SmClaimReleaseRetryOutcome::Retained;
        }
        let entity = sm_session_entity(stream_id);
        let backend_outcome = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .release_exact(&entity, fence.owner(), fence.epoch()),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "ClaimStore release failed; retaining the exact owner+epoch fence for retry"
                );
                return super::SmClaimReleaseRetryOutcome::Retained;
            }
            Err(_) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "ClaimStore release timed out while holding a stream shard lock; retaining \
                     the exact owner+epoch fence for retry"
                );
                return super::SmClaimReleaseRetryOutcome::Retained;
            }
        };
        let local_cleared = self.retire_exact_claim_handoff_locally(stream_id, &fence);
        // ADR-0017 Phase 3 Slice 5 debt (a): every claim-ending path evicts
        // the fenced persistence's per-stream epoch cache (a no-op for the
        // portable/in-memory persistence, which keeps no such cache — see
        // `SmPersistenceStorage::evict_claim_cache`'s doc comment).
        if local_cleared {
            if let Some(storage) = &self.persistence {
                let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
                storage.evict_claim_cache(&session_id, &fence);
            }
        }
        match (backend_outcome, local_cleared) {
            (crate::ownership::ExactReleaseOutcome::Released, true) => {
                super::SmClaimReleaseRetryOutcome::Released
            }
            (crate::ownership::ExactReleaseOutcome::NotOwned, true) => {
                super::SmClaimReleaseRetryOutcome::Disproved
            }
            (_, false) => super::SmClaimReleaseRetryOutcome::Retained,
        }
    }

    /// Complete a previously claimed session, returning the claimed copy with
    /// any stanzas recorded during the handoff and removing detached replay
    /// eligibility from the registry.
    pub async fn complete_claim(
        &self,
        stream_id: &str,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        self.complete_claim_checked(stream_id, None).await
    }

    /// Destructively complete a claimed session while reusing authority the
    /// caller acquired after this stream's shard. This preserves the global
    /// shard-before-identity lock order and avoids reacquiring the
    /// writer-preferring identity gate inside fenced persistence.
    pub async fn complete_claim_with_authority(
        &self,
        operation: super::SmSessionOperationGuard,
        authority: &crate::ownership::CurrentNodeIdentityGuard,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        self.validate_operation_authority(&operation, authority)?;
        self.complete_claim_checked_locked(&operation.stream_id, None, Some(authority))
            .await
    }

    fn validate_operation_authority(
        &self,
        operation: &super::SmSessionOperationGuard,
        authority: &crate::ownership::CurrentNodeIdentityGuard,
    ) -> Result<(), SmRegistryError> {
        let expected_shard = self.stream_lock(&operation.stream_id)?;
        if !std::sync::Arc::ptr_eq(&expected_shard, &operation.shard) {
            return Err(SmRegistryError::Internal(
                "SM operation guard belongs to a different registry".to_string(),
            ));
        }
        if !self.node_identity.owns_guard(authority) {
            return Err(SmRegistryError::Internal(
                "SM identity authority belongs to a different registry".to_string(),
            ));
        }
        let fence_owner = self
            .claim_fences
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(&operation.stream_id)
            .map(|fence| fence.owner().clone());
        if fence_owner.as_ref() != Some(authority.identity()) {
            return Err(SmRegistryError::Internal(
                "SM identity authority does not match the active claim fence".to_string(),
            ));
        }
        Ok(())
    }

    /// Complete a claim only if the final claimed session still has
    /// every stanza needed by `client_h`. If late detached fanout
    /// during the claim handoff evicted an older stanza, the claim is
    /// restored to the detached pool and the caller must return
    /// XEP-0198 `<failed/>` instead of `<resumed/>`.
    pub async fn complete_claim_if_resumable(
        &self,
        stream_id: &str,
        client_h: u32,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        self.complete_claim_checked(stream_id, Some(client_h)).await
    }

    async fn complete_claim_checked(
        &self,
        stream_id: &str,
        client_h: Option<u32>,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        self.complete_claim_checked_locked(stream_id, client_h, None)
            .await
    }

    async fn complete_claim_checked_locked(
        &self,
        stream_id: &str,
        client_h: Option<u32>,
        authority: Option<&crate::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        // Persist-first ordering: durably erase the session BEFORE
        // we hand it back to the resuming connection. If the durable
        // delete fails, abort the resume — the in-memory entry stays
        // in claimed_sessions and the caller can retry, or
        // release_claim to put it back. Without persist-first, a
        // successful in-memory completion + failed durable delete
        // would leave an orphan row that
        // `restore_from_persistence` would resurrect on next
        // restart, exposing a stale `<resume previd='…'/>` for an
        // already-live session (Codex P1 + Copilot + Qodo on PR
        // #344).
        let exists = {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed.get(stream_id).cloned()
        };
        let Some(session) = exists else {
            return Ok(None);
        };
        if session.is_expired() {
            // Expiry is a drain transition, not permission to discard the
            // unacked queue. A session can cross its resume deadline after
            // `claim_session` moved it off the janitor-visible map but before
            // completion starts. Put that exact generation back without
            // deleting its durable current row or releasing its claim; the
            // normal drain -> promote -> confirm chain owns terminal cleanup.
            let restored = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let mut claimed = self
                    .claimed_sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                if claimed
                    .get(stream_id)
                    .is_some_and(|candidate| candidate.generation_id == session.generation_id)
                {
                    claimed.remove(stream_id).inspect(|restored| {
                        sessions.insert(stream_id.to_string(), restored.clone());
                    })
                } else {
                    None
                }
            };
            return Ok(restored.map(SmClaimCompletion::Expired));
        }
        if let Some(client_h) = client_h {
            // Ordering matters, mirroring the websocket resume path:
            // `handled_count_exceeds_outbound` is an exact mod-2^32
            // window from last_acked that also classifies the
            // regressed half-space as "outside the window".
            // `can_resume_from` rejects a regressed `h` first, so a
            // stale mod-behind `h` stays ReplayWindowTruncated (a
            // failed resume) rather than HandledCountTooHigh; an
            // ahead-of-window `h` passes it and hits the too-high
            // branch below. (#1099: this replaces a naive
            // `client_h > outbound_count` check that had a half-window
            // blind spot at `h == outbound + 2^31`.)
            if !session.is_expired() && !session.can_resume_from(client_h) {
                let restored = {
                    let mut claimed = self
                        .claimed_sessions
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    claimed.remove(stream_id)
                };
                if let Some(restored) = restored {
                    {
                        let mut sessions = self
                            .sessions
                            .write()
                            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                        sessions.insert(stream_id.to_string(), restored.clone());
                    }
                    // The resume attempt failed, but the detached session is
                    // resumable state again. Keep its shared claim until the
                    // eventual take/promote/confirm path has retired both the
                    // bare row and every same-id terminal generation.
                    return Ok(Some(SmClaimCompletion::ReplayWindowTruncated(restored)));
                }
                return Ok(None);
            }
            if !session.is_expired() && session.handled_count_exceeds_outbound(client_h) {
                let restored = {
                    let mut claimed = self
                        .claimed_sessions
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    claimed.remove(stream_id)
                };
                if let Some(restored) = restored {
                    {
                        let mut sessions = self
                            .sessions
                            .write()
                            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                        sessions.insert(stream_id.to_string(), restored.clone());
                    }
                    // Closing the failed resume connection does not retire
                    // the restored detached lifecycle. Its shared claim stays
                    // held until durable retirement proves the stream has no
                    // current, terminal, or publication-unknown work left.
                    return Ok(Some(SmClaimCompletion::HandledCountTooHigh(restored)));
                }
                return Ok(None);
            }
        }
        let mut cancellation_guard =
            ClaimCompletionCancellationGuard::new(self, stream_id, session.generation_id);
        let delete_result = if let Some(authority) = authority {
            self.persist_delete_session_with_authority(stream_id, authority)
                .await
        } else {
            self.persist_delete_session(stream_id).await
        };
        if let Err(error) = delete_result {
            cancellation_guard.disarm();
            return Err(error);
        }
        let durable_work_remains = self
            .durable_work_may_remain_ignoring_map_generations(stream_id, &[session.generation_id])
            .await;
        if durable_work_remains {
            // The durable probe is deliberately fail-closed. Before the
            // claimed-map generation stops carrying this stream's ownership,
            // move its exact fence into the retry inventory scanned by
            // `retry_pending_claim_releases`. Otherwise a probe error could
            // leave only an unscanned active fence after this completion
            // returns the session to the live connection.
            self.retain_claim_for_durable_recovery(stream_id)?;
        }
        // Now remove from in-memory; the durable side has already
        // committed.
        let removed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        cancellation_guard.disarm();
        // The entry-time `is_expired` check is this completion's expiry
        // linearization point, before the first destructive await above.
        // Once completion starts for a live resume window, the durable delete
        // commits that resume rather than reclassifying elapsed wall time as
        // terminal queue loss afterward.
        let outcome = removed.map(SmClaimCompletion::Resumed);
        if outcome.is_some() && !durable_work_remains {
            // Successful completion ends this claim; expired generations
            // returned above stay claimed until drain confirmation.
            self.release_claim_store_entry(stream_id).await;
        }
        Ok(outcome)
    }

    /// Remove a stored detached session from the in-memory view only
    /// if it has not been claimed by a resume attempt, WITHOUT
    /// deleting its durable rows.
    ///
    /// Follows the displaced-session persist-until-confirmed contract
    /// (issue #1097): the caller MUST run the XEP-0198 §5 promote →
    /// [`Self::confirm_drained`] chain on the returned session — only
    /// the confirm erases the durable rows. (The previous shape,
    /// `remove_stored_session_if_unclaimed`, durably deleted up-front;
    /// the ownership-moved detach path then discarded the returned
    /// session, losing the unacked queue entirely.)
    pub async fn displace_stored_session_if_unclaimed(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .contains_key(stream_id);
        if claimed {
            return Ok(None);
        }
        let removed = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut promotions = self
                .pending_promotions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let removed = sessions.remove(stream_id);
            match removed {
                Some(session) if promotions.insert_current(&session) => Some(session),
                Some(session) => {
                    sessions.insert(stream_id.to_string(), session);
                    None
                }
                None => None,
            }
        };
        Ok(removed)
    }

    /// Invalidate detached sessions for a FullJID after a fresh bind has
    /// replaced that stream identity.
    ///
    /// Follows the drain_expired/confirm_drained persist-until-
    /// confirmed contract (issue #1097): the removed sessions'
    /// durable rows are deliberately NOT deleted here. The caller
    /// promotes each returned session's unacked queue (XEP-0198 §5:
    /// the freshly-bound resource is a natural alt-resource target)
    /// and calls [`Self::confirm_drained`] on success. If the process
    /// crashes before promotion, `restore_from_persistence`
    /// rehydrates the rows and the SM-expiry janitor retries.
    pub async fn invalidate_sessions_for_jid(
        &self,
        jid: &FullJid,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let matching_generations: Vec<(String, super::SmSessionGenerationId, FullJid)> = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut candidates: Vec<(String, super::SmSessionGenerationId, FullJid)> = sessions
                .iter()
                .filter(|(_, s)| s.jid == *jid)
                .map(|(id, session)| (id.clone(), session.generation_id, session.jid.clone()))
                .collect();
            for (id, session) in claimed.iter() {
                if session.jid == *jid {
                    candidates.push((id.clone(), session.generation_id, session.jid.clone()));
                }
            }
            candidates
        };
        let mut removed = DrainedSessionBatch::new(self, matching_generations.len());
        for (stream_id, generation_id, expected_jid) in matching_generations {
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let (removed_detached, removed_claimed) = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let mut claimed = self
                    .claimed_sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let mut promotions = self
                    .pending_promotions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let detached_matches = sessions.get(&stream_id).is_some_and(|session| {
                    session.generation_id == generation_id && session.jid == expected_jid
                });
                let claimed_matches = claimed.get(&stream_id).is_some_and(|session| {
                    session.generation_id == generation_id && session.jid == expected_jid
                });
                let mut removed_detached = detached_matches
                    .then(|| sessions.remove(&stream_id))
                    .flatten();
                let mut removed_claimed = claimed_matches
                    .then(|| claimed.remove(&stream_id))
                    .flatten();
                if let Some(session) = removed_detached.as_ref() {
                    if !promotions.insert_current(session) {
                        sessions.insert(stream_id.clone(), session.clone());
                        removed_detached = None;
                    }
                }
                if let Some(session) = removed_claimed.as_ref() {
                    if !promotions.insert_current(session) {
                        claimed.insert(stream_id.clone(), session.clone());
                        removed_claimed = None;
                    }
                }
                (removed_detached, removed_claimed)
            };
            // Deliberately NO eager `ClaimStore` release here (the same
            // release-before-durable-delete hazard `store_session`'s
            // eviction path closed in the Slice 5 review): the durable row
            // still exists, so releasing now would let another node's
            // restore/orphan-reaper hydrate a copy that our caller's later
            // `confirm_drained` deletes out from under it. The sole
            // production caller (`invalidate_older_detached_sessions`) runs
            // every returned session through `promote_displaced_sessions`,
            // which releases via `confirm_drained` after the durable delete
            // — or re-inserts for janitor retry with the claim still held.
            if let Some(session) = removed_detached {
                removed.push(session);
            }
            if let Some(session) = removed_claimed {
                removed.push(session);
            }
        }
        Ok(removed.finish())
    }
}
