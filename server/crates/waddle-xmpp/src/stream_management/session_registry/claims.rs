use jid::FullJid;
use tracing::debug;

use crate::ownership::{ClaimError, Entity, EntityType};

use super::core::{
    DetachClaimFenceReservation, InMemorySmSessionRegistry, PendingClaimAcquisitionDisposition,
    CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
};
use super::{DetachedSession, SmClaimCompletion, SmRegistryError};

#[derive(Debug)]
pub(super) enum ClaimSessionOutcome {
    Claimed(Box<DetachedSession>),
    MissingOrExpired,
    LostClaim,
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
    /// Return an in-flight resume claim to the detached pool from a
    /// cancellation guard.  This is synchronous specifically so a dropped
    /// WebSocket resume future cannot strand the claimed snapshot between
    /// awaits.  The durable ownership fence is intentionally retained: an
    /// unexpired detached session remains owned by this node, exactly as in
    /// [`Self::release_claim`].
    ///
    /// If bookkeeping cannot be acquired, the exact fence is transferred to
    /// the existing terminal-release inventory rather than being forgotten.
    pub fn defer_claimed_resume_release(&self, stream_id: &str) -> bool {
        let reinserted = match (self.sessions.write(), self.claimed_sessions.write()) {
            (Ok(mut sessions), Ok(mut claimed)) => match claimed.remove(stream_id) {
                Some(session) if !session.is_expired() => {
                    sessions.insert(stream_id.to_string(), session);
                    true
                }
                Some(session) => {
                    claimed.insert(stream_id.to_string(), session);
                    false
                }
                None => false,
            },
            _ => false,
        };
        if reinserted {
            return true;
        }
        self.defer_enabled_claim_release(stream_id)
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
    pub async fn drain_all_for_shutdown(&self) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let stream_ids: Vec<String> = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.keys().cloned().collect()
        };
        let mut drained = DrainedSessionBatch::new(self, stream_ids.len());
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
                if removed.is_some() {
                    promotions.insert(stream_id.clone());
                }
                removed
            };
            if let Some(session) = removed {
                drained.push(session);
            }
        }
        Ok(drained.finish())
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
            out.extend(retries.keys().cloned());
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
            out.extend(pending.iter().map(|(stream_id, _)| stream_id.clone()));
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
                    .filter(|(_, fence)| fence.owner() == owner)
                    .map(|(stream_id, _)| stream_id.clone()),
            );
        }
        out.sort();
        out.dedup();
        Some(out)
    }

    /// Retry exact terminal releases retained after a backend error/timeout.
    /// Entries still represented by a live/detached session are excluded:
    /// those claims remain intentionally held.
    pub async fn retry_pending_claim_releases(&self, limit: usize) -> usize {
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
                self.reconcile_uncertain_claim_acquisition_locked(
                    &stream_id,
                    identity,
                    disposition,
                )
                .await;
                continue;
            }
            self.reconcile_uncertain_claim_acquisition(&stream_id, identity, disposition)
                .await;
        }
        let pending = {
            let Ok(pending) = self.pending_claim_releases.read() else {
                return 0;
            };
            pending
                .iter()
                .map(|(stream_id, fence)| (stream_id.clone(), fence.clone()))
                .collect::<Vec<_>>()
        };
        let mut attempted = 0;
        for (stream_id, fence) in pending {
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
                .map(|current| current.contains(&(stream_id.clone(), fence.clone())))
                .unwrap_or(false);
            if !still_pending {
                continue;
            }
            match self.stream_liveness(&stream_id) {
                None => continue,
                Some(true) => {
                    let Ok(active) = self.claim_fences.read() else {
                        continue;
                    };
                    if active.get(&stream_id) == Some(&fence) {
                        // This fence still authorizes the live lifecycle.
                        continue;
                    }
                    // A different/absent active fence means this is terminal
                    // exact cleanup. Its owner+epoch CAS is safe to retry
                    // while the replacement lifecycle remains live.
                }
                Some(false) => {}
            }
            budget_used += 1;
            attempted += 1;
            self.release_claim_store_entry_under(&stream_id, fence)
                .await;
        }
        attempted
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

    fn stream_liveness_and_promotion(&self, stream_id: &str) -> Option<(bool, bool)> {
        let sessions = self.sessions.read().ok()?;
        let claimed = self.claimed_sessions.read().ok()?;
        let promotions = self.pending_promotions.read().ok()?;
        Some((
            sessions.contains_key(stream_id) || claimed.contains_key(stream_id),
            promotions.contains(stream_id),
        ))
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
                    && *disposition == PendingClaimAcquisitionDisposition::RetainDetachedSession
            }),
            Err(_) => return true,
        };
        if detach_still_uncertain {
            return true;
        }

        let entity = sm_session_entity(stream_id);
        let snapshot = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim(&entity),
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
                    // current live lifecycle. Preserve it only as terminal
                    // exact-release responsibility, then attempt that release
                    // without ever publishing the old fence for live writes.
                    if !self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                        return true;
                    }
                    self.release_claim_store_entry_under(stream_id, fence).await;
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
        let Ok(stream_lock) = self.stream_lock(stream_id) else {
            return;
        };
        let _stream_guard = stream_lock.lock().await;
        self.reconcile_uncertain_claim_acquisition_locked(stream_id, identity, disposition)
            .await;
    }

    async fn reconcile_uncertain_claim_acquisition_locked(
        &self,
        stream_id: &str,
        identity: crate::ownership::NodeIdentity,
        disposition: PendingClaimAcquisitionDisposition,
    ) {
        let pending_key = (stream_id.to_string(), identity.clone(), disposition);
        let still_pending = self
            .pending_claim_acquisitions
            .read()
            .map(|pending| pending.contains(&pending_key))
            .unwrap_or(false);
        if !still_pending {
            return;
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
                            return;
                        }
                        if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                            pending.remove(&pending_key);
                        }
                        self.release_claim_store_entry_under(stream_id, fence).await;
                    }
                    PendingClaimAcquisitionDisposition::RetainDetachedSession => {
                        let Some((stream_live, promotion_pending)) =
                            self.stream_liveness_and_promotion(stream_id)
                        else {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert(pending_key);
                            }
                            return;
                        };
                        let (recorded, terminal) =
                            self.node_identity.with_current(|current_identity| {
                                let retain = promotion_pending
                                    || (stream_live && current_identity == &identity);
                                let recorded = if retain {
                                    self.try_record_verified_claim_fence(stream_id, fence.clone())
                                } else {
                                    self.try_record_verified_terminal_claim_fence(
                                        stream_id,
                                        fence.clone(),
                                    )
                                };
                                if recorded {
                                    if let Ok(mut pending) = self.pending_claim_acquisitions.write()
                                    {
                                        pending.remove(&pending_key);
                                    }
                                }
                                (recorded, !retain)
                            });
                        if !recorded {
                            if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                                pending.insert(pending_key);
                            }
                            return;
                        }
                        if terminal {
                            self.forget_claim_locally_locked(stream_id, Some(&fence));
                            self.release_claim_store_entry_under(stream_id, fence).await;
                        }
                    }
                }
            }
            Ok(Err(ClaimError::AlreadyClaimed | ClaimError::Conflict | ClaimError::Draining)) => {
                self.remove_pending_claim_acquisition(stream_id, &identity, disposition);
            }
            Ok(Err(_)) | Err(_) => {
                if let Ok(mut pending) = self.pending_claim_acquisitions.write() {
                    pending.insert((stream_id.to_string(), identity, disposition));
                }
            }
        }
    }

    pub fn pending_claim_release_count(&self) -> usize {
        self.pending_claim_releases
            .read()
            .map_or(0, |pending| pending.len())
    }

    /// Purely local, best-effort forgetting of `stream_id`'s claim
    /// (ADR-0017 Phase 3 Slice 5, carried debt (b): the
    /// `LocallyClaimedEntities::demote` contract). Removes it from both
    /// `sessions` and `claimed_sessions` and evicts its cached epoch (and
    /// the fenced persistence's own epoch cache), WITHOUT calling
    /// `ClaimStore::release` — the demotion-reconciliation caller already
    /// knows Postgres reassigned (or is reassigning) this entity elsewhere,
    /// so a release round-trip here is both unnecessary and, per
    /// `demote`'s own contract, must not be REQUIRED to succeed while
    /// Postgres is unreachable (the self-fencing trigger this method
    /// exists to serve).
    pub async fn forget_claim_locally(&self, stream_id: &str) {
        let Ok(stream_lock) = self.stream_lock(stream_id) else {
            return;
        };
        let _stream_guard = stream_lock.lock().await;
        self.node_identity
            .with_publications_blocked(|| self.forget_claim_locally_locked(stream_id, None))
            .await;
    }

    pub(super) fn forget_claim_locally_locked(
        &self,
        stream_id: &str,
        preserve_terminal_release: Option<&super::super::persistence::SmClaimFence>,
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
        let mut forgotten_fences = preserve_terminal_release
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
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
            releases.retain(|(id, pending_fence)| {
                if id == stream_id {
                    if preserve_terminal_release == Some(pending_fence) {
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
        if !self.try_record_terminal_claim_fence(stream_id, expected.clone()) {
            return Err(SmRegistryError::Internal(
                "identity-rotation cleanup could not retain the exact fence".to_string(),
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

        self.release_claim_store_entry_under(stream_id, expected.clone())
            .await;
        Ok(())
    }

    /// Reconcile an epoch-lookup failure atomically with respect to node
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
            false
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
        let release_fence = self.node_identity.with_current(|current_identity| {
            if fence.owner() != current_identity || !backend_exact {
                if !self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                    return Err(SmRegistryError::Internal(
                        "lost-ownership cleanup could not retain the exact fence".to_string(),
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

            let resumable = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .get(stream_id)
                .is_some_and(|session| !session.is_expired());
            if !resumable {
                if !self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                    return Err(SmRegistryError::Internal(
                        "epoch-failure cleanup could not retain the exact fence".to_string(),
                    ));
                }
                self.claimed_sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .remove(stream_id);
                return Ok(Some(fence.clone()));
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
        if let Some(fence) = release_fence {
            self.release_claim_store_entry_under(stream_id, fence).await;
        }
        Ok(())
    }

    /// Confirm that a drained session has been fully promoted —
    /// delete its durable row so a subsequent restart doesn't
    /// resurrect it. Best-effort: failures are logged but not
    /// returned, since at this point the unacked queue has already
    /// been promoted and the recipient will see the message via
    /// pending_delivery flush; a stale durable row would just be
    /// filtered by the restart-time expiry check eventually.
    ///
    /// Pair with [`Self::drain_all_for_shutdown`]: drain returns
    /// the sessions, caller promotes each, caller calls
    /// `confirm_drained` per session after successful promotion.
    ///
    /// Returns `true` iff the durable row was deleted and this entity's
    /// `ClaimStore` claim release was attempted — the SM session's own
    /// "final fenced write, then release" sequence (ADR-0017 Phase 3 Slice
    /// 10). `false` means the durable row survives for a restart-time
    /// retry (the claim is deliberately left held, not released) — the
    /// caller (`session_janitors::spawn_graceful_shutdown_drain`) counts
    /// this the same way Slice 10's generic per-entity drain counts an
    /// abandoned entity, feeding the shared `claims_released_on_drain`/
    /// `claims_abandoned_on_drain` observability.
    pub async fn confirm_drained(&self, stream_id: &str) -> bool {
        let stream_lock = match self.stream_lock(stream_id) {
            Ok(lock) => lock,
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "graceful-shutdown drain: stream lock lookup failed before durable delete"
                );
                return false;
            }
        };
        let _stream_guard = stream_lock.lock().await;
        match self.persist_delete_session(stream_id).await {
            Ok(()) => {
                // ADR-0017 Phase 3 Slice 5: the durable row is gone, so the
                // entity's `ClaimStore` claim must end with it — otherwise
                // every expired-then-promoted session leaves a permanent
                // `clustering_claims` row this node can never naturally
                // release again (nothing else ever revisits a deleted
                // session's entity).
                let fence = self
                    .claim_fences
                    .read()
                    .ok()
                    .and_then(|fences| fences.get(stream_id).cloned());
                if let Some(fence) = fence {
                    if !self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                        debug!(
                            stream_id,
                            "confirm_drained: durable rows deleted but exact release could not be retained"
                        );
                        return false;
                    }
                    self.forget_claim_locally_locked(stream_id, Some(&fence));
                    if let Ok(mut promotions) = self.pending_promotions.write() {
                        promotions.remove(stream_id);
                    }
                    if let Ok(mut retries) = self.pending_promotion_retries.write() {
                        retries.remove(stream_id);
                    }
                    self.release_claim_store_entry_under(stream_id, fence).await;
                } else {
                    self.forget_claim_locally_locked(stream_id, None);
                    if let Ok(mut promotions) = self.pending_promotions.write() {
                        promotions.remove(stream_id);
                    }
                    if let Ok(mut retries) = self.pending_promotion_retries.write() {
                        retries.remove(stream_id);
                    }
                }
                true
            }
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "graceful-shutdown drain: durable delete failed; \
                     restart-time expiry filter will catch the orphan"
                );
                false
            }
        }
    }

    /// Increment the persistent promotion-failure counter for
    /// `stream_id` and return the new value. Used by the SM-expiry
    /// janitor to detect runaway retry loops on permanent storage or
    /// blocklist failures.
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
        let retry_ids = self
            .pending_promotion_retries
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut drained = DrainedSessionBatch::new(self, retry_ids.len());
        for stream_id in retry_ids {
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let Some(session) = self
                .pending_promotion_retries
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(&stream_id)
            else {
                continue;
            };
            let mut retry = PendingPromotionRetryLease::new(self, session);
            self.reconcile_retry_payload(retry.session_mut()).await;
            let still_pending = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .contains(&stream_id);
            if still_pending {
                drained.push(retry.finish());
            } else {
                retry.discard();
            }
        }
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
                let removed = match sessions.get(stream_id) {
                    Some(session) if session.is_expired() => sessions.remove(stream_id),
                    _ => None,
                };
                if removed.is_some() {
                    promotions.insert(stream_id.clone());
                }
                removed
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
    pub async fn reinsert_for_retry(
        &self,
        mut session: DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(&session.stream_id)?;
        let _stream_guard = stream_lock.lock().await;
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
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(session.stream_id.clone());
            match storage.get_session(&session_id).await {
                Ok(Some(_)) => match storage.list_unacked(&session_id).await {
                    Ok(rows) => {
                        let durable: std::collections::HashSet<u32> =
                            rows.iter().map(|row| row.sequence).collect();
                        session
                            .unacked_stanzas
                            .retain(|entry| durable.contains(&entry.sequence));
                    }
                    Err(error) => {
                        debug!(
                            stream_id = %session.stream_id,
                            error = %error,
                            "reinsert_for_retry: durable list_unacked failed; keeping the \
                             in-memory queue verbatim (at-least-once)"
                        );
                    }
                },
                Ok(None) => {
                    debug!(
                        stream_id = %session.stream_id,
                        "reinsert_for_retry: no durable session row (snapshot never \
                         landed); keeping the in-memory queue verbatim (at-least-once)"
                    );
                }
                Err(error) => {
                    debug!(
                        stream_id = %session.stream_id,
                        error = %error,
                        "reinsert_for_retry: durable get_session failed; keeping the \
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
        let mut promotions = self
            .pending_promotions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        sessions.insert(stream_id.clone(), session);
        promotions.insert(stream_id);
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
        if !promotions.contains(&stream_id) {
            return Ok(());
        }
        self.pending_promotion_retries
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .insert(stream_id, session);
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
    /// lost. Cross-node resume must repair the latter after it has already
    /// hydrated and published a local fence.
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
            PendingClaimAcquisitionDisposition::RetainDetachedSession,
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
                    self.release_claim_store_entry_under(stream_id, active_fence)
                        .await;
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
            self.release_claim_store_entry_under(stream_id, fence).await;
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
            if self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                acquisition_guard.disarm();
                self.forget_claim_locally_locked(stream_id, Some(&fence));
                self.release_claim_store_entry_under(stream_id, fence).await;
                return Ok(ClaimSessionOutcome::LostClaim);
            }
            if let Ok(mut claimed) = self.claimed_sessions.write() {
                claimed.remove(stream_id);
            }
            if let Ok(mut sessions) = self.sessions.write() {
                sessions.insert(stream_id.to_string(), session);
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
    /// Only when the session is expired (and therefore NOT reinserted —
    /// left for the janitor's drain_expired/promote/confirm chain, which
    /// releases the claim itself via [`Self::confirm_drained`]) or was
    /// absent from `claimed_sessions` altogether does this release the
    /// store entry: those are the only cases where the entity genuinely
    /// stops being tracked by this call.
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
                Some(session) if !session.is_expired() => {
                    sessions.insert(stream_id.to_string(), session);
                    true
                }
                _ => false,
            }
        };
        if !reinserted {
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
    /// Best-effort by design: a stream id is a freshly minted UUID per
    /// session (ADR-0017 element 8), so a genuine collision with another
    /// node's still-live claim on the exact same id is not expected in
    /// practice. Refusing to store the just-detached session over a claim
    /// failure would risk losing its unacked queue entirely — worse than
    /// proceeding without a durable claim record, which only means this
    /// specific stream id will not be reachable to a startup restore or the
    /// orphan reaper on a *different* node until this node's own next
    /// successful acquire for it (e.g. this node's own restart). Logged at
    /// `warn` so a persistently failing acquire (e.g. a genuinely wedged
    /// `ClaimStore` backend) is visible.
    pub(super) async fn acquire_claim_store_entry_for_detach(
        &self,
        stream_id: &str,
        reservation: DetachClaimFenceReservation,
    ) {
        let acquisition_reserved = self.has_claim_fence_reservation(stream_id);
        let entity = sm_session_entity(stream_id);
        let identity = self.node_identity.current();
        let mut acquisition_guard = PendingAcquisitionGuard::new(
            self,
            stream_id,
            identity.clone(),
            PendingClaimAcquisitionDisposition::RetainDetachedSession,
        );
        // FIX 5: bounded — this call runs under `stream_id`'s shard lock
        // (`store_session` holds it for the whole function, including this
        // call), shared by every other stream id hashing to the same shard
        // (see `CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`'s doc comment). Best
        // effort by design (see this function's doc comment above): a
        // timeout is logged and treated exactly like any other
        // `ensure_claimed` failure here — proceed without a durable claim
        // record rather than block every other stream id sharing this
        // shard.
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
                    if self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                        acquisition_guard.disarm();
                        self.release_claim_store_entry_under(stream_id, fence).await;
                    }
                    return;
                };
                let recorded = self.try_record_claim_fence(stream_id, fence.clone());
                drop(publication_guard);
                if !recorded {
                    if self.try_record_terminal_claim_fence(stream_id, fence.clone()) {
                        acquisition_guard.disarm();
                        self.release_claim_store_entry_under(stream_id, fence).await;
                    }
                    return;
                }
                acquisition_guard.disarm();
            }
            Ok(Err(error)) => {
                reservation.cancel_if_owned(self, stream_id);
                acquisition_guard.disarm();
                tracing::warn!(
                    stream_id = %stream_id,
                    %error,
                    "store_session: ClaimStore ensure_claimed failed for a freshly \
                     detached session; proceeding without a durable claim record \
                     (best-effort — see this function's doc comment)"
                );
            }
            Err(_timeout) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "store_session: ClaimStore ensure_claimed timed out while holding this \
                     stream's shard lock (FIX 5); proceeding without a durable claim record \
                     (best-effort — see this function's doc comment)"
                );
                if acquisition_reserved {
                    // Do not issue a second ClaimStore call while the caller
                    // holds this stream's shard lock. The bounded SM janitor
                    // owns the guard-published reconciliation after
                    // `store_session` returns and releases the shard.
                } else {
                    acquisition_guard.disarm();
                }
            }
        }
    }

    /// Release `stream_id`'s `ClaimStore` entry under whatever epoch this
    /// registry last observed for it (falling back to epoch 0 if none was
    /// recorded — harmless for the in-process store, and `release`'s own
    /// contract treats a losing epoch as a no-op rather than an error).
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
    /// epoch (the caller already holds it, so there is nothing to look up
    /// in `claim_fences`). Best-effort: a `ClaimStore::release` failure is
    /// logged, never propagated — `release`'s own contract treats a losing
    /// epoch as a no-op, and this is the same "claim already gone" case.
    /// `pub(super)`: `core.rs::restore_from_persistence` (ADR-0017 Phase 3
    /// Slice 5) also releases an already-known epoch directly (a
    /// just-claimed, never-hydrated poison-pill row), without going through
    /// `claim_fences` first.
    pub(super) async fn release_claim_store_entry_under(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) {
        let entity = sm_session_entity(stream_id);
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .release(&entity, fence.owner(), fence.epoch()),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "ClaimStore release failed; retaining the exact owner+epoch fence for retry"
                );
                if let Ok(mut pending) = self.pending_claim_releases.write() {
                    pending.insert((stream_id.to_string(), fence));
                }
                return;
            }
            Err(_) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "ClaimStore release timed out while holding a stream shard lock; retaining \
                     the exact owner+epoch fence for retry"
                );
                if let Ok(mut pending) = self.pending_claim_releases.write() {
                    pending.insert((stream_id.to_string(), fence));
                }
                return;
            }
        }
        if let Ok(mut fences) = self.claim_fences.write() {
            if fences.get(stream_id) == Some(&fence) {
                fences.remove(stream_id);
            }
        }
        if let Ok(mut pending) = self.pending_claim_releases.write() {
            pending.remove(&(stream_id.to_string(), fence.clone()));
        }
        // ADR-0017 Phase 3 Slice 5 debt (a): every claim-ending path evicts
        // the fenced persistence's per-stream epoch cache (a no-op for the
        // portable/in-memory persistence, which keeps no such cache — see
        // `SmPersistenceStorage::evict_claim_cache`'s doc comment).
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
            storage.evict_claim_cache(&session_id, &fence);
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
                    // Terminal path (ADR-0017 Phase 3 Slice 1): a failed
                    // resume the caller closes on — release the claim, same
                    // as the `HandledCountTooHigh` branch below.
                    self.release_claim_store_entry(stream_id).await;
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
                    // Terminal path (ADR-0017 Phase 3 Slice 1): the caller
                    // treats HandledCountTooHigh as a failed resume and closes
                    // the connection, so this ends the claim — release its
                    // `ClaimStore` entry (guard scoped above so the release
                    // awaits after it drops), mirroring the
                    // `ReplayWindowTruncated` branch. The origin/main merge
                    // dropped this side effect when it removed the naive
                    // pre-#1099 resume check it was co-located with.
                    self.release_claim_store_entry(stream_id).await;
                    return Ok(Some(SmClaimCompletion::HandledCountTooHigh(restored)));
                }
                return Ok(None);
            }
        }
        if let Some(authority) = authority {
            self.persist_delete_session_with_authority(stream_id, authority)
                .await?;
        } else {
            self.persist_delete_session(stream_id).await?;
        }
        // Now remove from in-memory; the durable side has already
        // committed.
        let outcome = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id)
            .map(|session| {
                if session.is_expired() {
                    SmClaimCompletion::Expired(session)
                } else {
                    SmClaimCompletion::Resumed(session)
                }
            });
        if outcome.is_some() {
            // Terminal path (ADR-0017 Phase 3 Slice 1 fix): both
            // `Resumed` and `Expired` end this claim — release the store
            // entry so a successful resume does not leak it forever.
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
            if removed.is_some() {
                promotions.insert(stream_id.to_string());
            }
            removed
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
        let matching_ids: Vec<String> = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut ids: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| s.jid == *jid)
                .map(|(id, _)| id.clone())
                .collect();
            for (id, s) in claimed.iter() {
                if s.jid == *jid {
                    ids.push(id.clone());
                }
            }
            ids
        };
        let mut removed = Vec::new();
        for stream_id in &matching_ids {
            let stream_lock = self.stream_lock(stream_id)?;
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
                let removed = (sessions.remove(stream_id), claimed.remove(stream_id));
                if removed.0.is_some() || removed.1.is_some() {
                    promotions.insert(stream_id.clone());
                }
                removed
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
        Ok(removed)
    }
}
