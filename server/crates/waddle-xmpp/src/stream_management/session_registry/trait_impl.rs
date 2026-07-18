use async_trait::async_trait;
use tracing::debug;

use super::claims::DetachClaimAcquisitionOutcome;
use super::core::{
    DetachClaimFenceReservation, InMemorySmSessionRegistry, PendingClaimReleaseDisposition,
    PersistDetachedReplacementOutcome,
};
use super::{DetachedSession, SmRegistryError, SmSessionRegistry};
use crate::tombstone::{matching_tombstone_sequences, TombstoneTarget};

struct DetachReservationGuard<'a> {
    registry: &'a InMemorySmSessionRegistry,
    stream_id: &'a str,
    reservation: DetachClaimFenceReservation,
    armed: bool,
}

impl<'a> DetachReservationGuard<'a> {
    fn new(
        registry: &'a InMemorySmSessionRegistry,
        stream_id: &'a str,
        reservation: DetachClaimFenceReservation,
    ) -> Self {
        Self {
            registry,
            stream_id,
            reservation,
            armed: true,
        }
    }

    fn transfer(&mut self) {
        self.armed = false;
    }
}

impl Drop for DetachReservationGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.reservation
                .cancel_if_owned(self.registry, self.stream_id);
        }
    }
}

struct DisplacedPromotionGuard<'a> {
    registry: &'a InMemorySmSessionRegistry,
    sessions: Vec<DetachedSession>,
    publication_unknown: Option<(String, super::SmSessionGenerationId)>,
    armed: bool,
}

impl<'a> DisplacedPromotionGuard<'a> {
    fn new(registry: &'a InMemorySmSessionRegistry, sessions: Vec<DetachedSession>) -> Self {
        Self {
            registry,
            sessions,
            publication_unknown: None,
            armed: true,
        }
    }

    fn mark_publication_unknown(&mut self, session: &DetachedSession) {
        self.publication_unknown = Some((session.stream_id.clone(), session.generation_id));
    }

    fn clear_publication_unknown(&mut self) {
        self.publication_unknown = None;
    }

    fn transfer(mut self) -> Vec<DetachedSession> {
        self.armed = false;
        std::mem::take(&mut self.sessions)
    }

    fn forget_generation(&mut self, session: &DetachedSession) {
        self.sessions.retain(|candidate| {
            candidate.stream_id != session.stream_id
                || candidate.generation_id != session.generation_id
        });
    }

    fn rollback(&mut self) {
        for session in self.sessions.drain(..) {
            if self.publication_unknown.as_ref()
                == Some(&(session.stream_id.clone(), session.generation_id))
            {
                if let Err(error) = self.registry.park_publication_unknown(session) {
                    tracing::error!(
                        %error,
                        "store_session: cancellation could not park a publication-unknown \
                         same-id predecessor"
                    );
                }
                continue;
            }
            // Keep the pre-displacement payload off the resumable map until
            // the async retry path has re-read its durable rows. A tombstone
            // can scrub those rows while the replacement snapshot is
            // suspended, so publishing this copy directly would resurrect
            // the scrubbed stanza after the recent-tombstone window expires.
            let _ = self.registry.retain_pending_promotion_for_retry(session);
        }
        self.armed = false;
    }
}

impl Drop for DisplacedPromotionGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.rollback();
        }
    }
}

#[derive(Clone, Copy)]
enum RejectedDetachDisposition {
    TerminalCarrier {
        force_obsolete: bool,
        restore_predecessor: Option<super::SmSessionGenerationId>,
    },
    UnownedDurableCarrier,
    CommitUnknownPreservePublished,
    PublicationUnknownPark,
    Completed,
}

struct RejectedDetachCarrierGuard<'a> {
    registry: &'a InMemorySmSessionRegistry,
    session: DetachedSession,
    disposition: RejectedDetachDisposition,
}

impl<'a> RejectedDetachCarrierGuard<'a> {
    fn new(registry: &'a InMemorySmSessionRegistry, session: DetachedSession) -> Self {
        Self {
            registry,
            session,
            disposition: RejectedDetachDisposition::TerminalCarrier {
                force_obsolete: false,
                restore_predecessor: None,
            },
        }
    }

    fn mark_snapshot_commit_unknown(&mut self) {
        self.disposition = RejectedDetachDisposition::CommitUnknownPreservePublished;
    }

    fn mark_publication_unknown(&mut self) {
        self.disposition = RejectedDetachDisposition::PublicationUnknownPark;
    }

    fn reject_definitely_not_committed(
        &mut self,
        predecessor: Option<super::SmSessionGenerationId>,
    ) {
        self.disposition = RejectedDetachDisposition::TerminalCarrier {
            force_obsolete: predecessor.is_some(),
            restore_predecessor: predecessor,
        };
    }

    fn reject_authority_lost(&mut self) {
        self.disposition = RejectedDetachDisposition::TerminalCarrier {
            force_obsolete: true,
            restore_predecessor: None,
        };
    }

    fn reject_with_resumable_predecessor(&mut self) {
        self.disposition = RejectedDetachDisposition::TerminalCarrier {
            force_obsolete: true,
            restore_predecessor: None,
        };
    }

    fn remove_successor_from_resumability(&self) -> Result<(), SmRegistryError> {
        let mut sessions = self
            .registry
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if sessions
            .get(&self.session.stream_id)
            .is_some_and(|session| session.generation_id == self.session.generation_id)
        {
            sessions.remove(&self.session.stream_id);
        }
        drop(sessions);
        let mut claimed = self
            .registry
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if claimed
            .get(&self.session.stream_id)
            .is_some_and(|session| session.generation_id == self.session.generation_id)
        {
            claimed.remove(&self.session.stream_id);
        }
        Ok(())
    }

    fn reject_claim_acquisition(&mut self) {
        self.disposition = RejectedDetachDisposition::UnownedDurableCarrier;
    }

    fn complete(&mut self) {
        self.disposition = RejectedDetachDisposition::Completed;
    }
}

impl Drop for RejectedDetachCarrierGuard<'_> {
    fn drop(&mut self) {
        if matches!(
            self.disposition,
            RejectedDetachDisposition::PublicationUnknownPark
        ) {
            if let Err(error) = self.registry.park_publication_unknown(self.session.clone()) {
                tracing::error!(
                    stream_id = %self.session.stream_id,
                    generation_id = %self.session.generation_id,
                    %error,
                    "store_session: cancellation could not remove a publication-unknown \
                     successor from resumability"
                );
            }
            return;
        }
        let (force_obsolete, restore_predecessor, unowned_durable) = match self.disposition {
            RejectedDetachDisposition::TerminalCarrier {
                force_obsolete,
                restore_predecessor,
            } => (force_obsolete, restore_predecessor, false),
            RejectedDetachDisposition::UnownedDurableCarrier => (true, None, true),
            RejectedDetachDisposition::CommitUnknownPreservePublished
            | RejectedDetachDisposition::PublicationUnknownPark
            | RejectedDetachDisposition::Completed => return,
        };
        let (Ok(mut sessions), Ok(mut promotions), Ok(mut retries)) = (
            self.registry.sessions.write(),
            self.registry.pending_promotions.write(),
            self.registry.pending_promotion_retries.write(),
        ) else {
            tracing::error!(
                stream_id = %self.session.stream_id,
                generation_id = ?self.session.generation_id,
                "store_session: could not retain rejected detach payload after lock poisoning"
            );
            return;
        };
        let inserted = if unowned_durable {
            promotions.insert_unowned_durable_carrier(&self.session)
        } else {
            promotions.insert_terminal_carrier(
                &self.session,
                force_obsolete || restore_predecessor.is_some(),
            )
        };
        let present = inserted
            || promotions.contains_generation(&self.session.stream_id, self.session.generation_id);
        let restored = restore_predecessor
            .map(|generation_id| {
                promotions.restore_current_generation(&self.session.stream_id, generation_id)
            })
            .unwrap_or(true);
        if !present || !restored {
            tracing::error!(
                stream_id = %self.session.stream_id,
                generation_id = ?self.session.generation_id,
                "store_session: rejected detach carrier authority could not be recorded"
            );
            return;
        }
        retries.insert(self.session.clone());
        if sessions
            .get(&self.session.stream_id)
            .is_some_and(|session| session.generation_id == self.session.generation_id)
        {
            sessions.remove(&self.session.stream_id);
        }
    }
}

impl InMemorySmSessionRegistry {
    pub(super) fn retain_claim_for_durable_recovery(
        &self,
        stream_id: &str,
    ) -> Result<(), SmRegistryError> {
        let active_fence = self
            .claim_fences
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(stream_id)
            .cloned();
        if let Some(fence) = active_fence {
            return self
                .try_record_terminal_claim_fence_preserving_reservation(stream_id, fence)
                .then_some(())
                .ok_or_else(|| {
                    SmRegistryError::Internal(
                        "could not retain exact claim for durable recovery".to_string(),
                    )
                });
        }
        let already_retained = self
            .pending_claim_releases
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .iter()
            .any(|((pending_stream_id, _), disposition)| {
                pending_stream_id == stream_id
                    && *disposition == PendingClaimReleaseDisposition::RetainedForDurableRecovery
            });
        already_retained.then_some(()).ok_or_else(|| {
            SmRegistryError::Internal(
                "durable SM work has no exact claim-recovery inventory".to_string(),
            )
        })
    }

    fn current_durable_generation_id(
        &self,
        stream_id: &str,
    ) -> Result<Option<super::SmSessionGenerationId>, SmRegistryError> {
        if let Some(generation_id) = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(stream_id)
            .map(|session| session.generation_id)
        {
            return Ok(Some(generation_id));
        }
        if let Some(generation_id) = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(stream_id)
            .map(|session| session.generation_id)
        {
            return Ok(Some(generation_id));
        }
        Ok(self
            .pending_promotions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .current_durable_generation(stream_id))
    }

    fn scrub_local_tombstone_generation(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        sequences: &[u32],
        scrub_horizon: &chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, SmRegistryError> {
        let scrub = |session: &mut DetachedSession| {
            let before = session.unacked_stanzas.len();
            session.unacked_stanzas.retain(|entry| {
                entry.original_receipt_at > *scrub_horizon || !sequences.contains(&entry.sequence)
            });
            before - session.unacked_stanzas.len()
        };
        let mut removed = 0usize;
        if let Some(session) = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get_mut(stream_id)
            .filter(|session| session.generation_id == generation_id)
        {
            removed = removed.saturating_add(scrub(session));
        }
        if let Some(session) = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get_mut(stream_id)
            .filter(|session| session.generation_id == generation_id)
        {
            removed = removed.saturating_add(scrub(session));
        }
        if let Some(session) = self
            .pending_promotion_retries
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get_generation_mut(stream_id, generation_id)
        {
            removed = removed.saturating_add(scrub(session));
        }
        Ok(removed)
    }
}

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(
        &self,
        mut session: DetachedSession,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        session.generation_id = super::SmSessionGenerationId::new();
        let stream_id = session.stream_id.clone();
        let jid = session.jid.clone();
        // Arm the payload fallback before awaiting the shard so cancellation
        // while queued for the lock cannot drop the detached unacked state.
        let rejected_detach_guard = RejectedDetachCarrierGuard::new(self, session.clone());
        let stream_lock = self.stream_lock(&stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        // Move the armed fallback into a binding declared after the shard
        // guard. From this point on reverse drop order runs every publication
        // fallback before unlocking the same-stream lifecycle.
        let mut rejected_detach_guard = rejected_detach_guard;
        if self
            .pending_promotions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .current_reservation_active(&stream_id)
        {
            return Err(SmRegistryError::Internal(
                "store_session: current promotion generation is leased".to_string(),
            ));
        }
        {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if claimed.contains_key(&stream_id) {
                debug!(
                    stream_id = %stream_id,
                    "store_session skipped: resume claim in flight owns this stream"
                );
                rejected_detach_guard.complete();
                return Ok(Vec::new());
            }
        }
        // Reserve exact-fence/reconciliation capacity before publishing the
        // detached session in memory or durable storage. Once either snapshot
        // exists, a successful or ambiguous backend claim must have bounded
        // local ownership bookkeeping; rejecting after publication would
        // strand an unrecorded live-node claim.
        let Some(detach_reservation) = self.reserve_detach_claim_fence_capacity(&stream_id) else {
            return Err(SmRegistryError::Internal(
                "store_session: exact claim-fence capacity exhausted before detach publication"
                    .to_string(),
            ));
        };
        let mut detach_reservation_guard =
            DetachReservationGuard::new(self, &stream_id, detach_reservation);
        // Scope the RwLock guards in a block so they're definitively
        // dropped before any await point. RwLockWriteGuard is not
        // Send, and explicit `drop()` doesn't satisfy the async
        // future's lifetime analysis. Capture eviction victims
        // (jid-collision retain + max_sessions oldest) IN FULL so the
        // caller can run XEP-0198 §5 promotion on their unacked
        // queues (issue #1097) — previously they were silently
        // dropped and their durable rows mirror-deleted.
        let mut displaced: Vec<DetachedSession> = Vec::new();
        let state_result = (|| -> Result<usize, SmRegistryError> {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut displacement_pending = self
                .pending_promotions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            // Issue #1139: if a resume claim for this EXACT stream id
            // is in flight, the claiming connection owns the handoff.
            // The old connection's late detach must not evict the
            // claim (that made the resume fail <failed
            // item-not-found/>), and it must not store a stale
            // duplicate either — a shadow `sessions` entry (and a
            // fresh durable snapshot) would outlive the claim's
            // persist-first `complete_claim` delete and resurrect an
            // already-resumed stream. Skip the store entirely; the
            // handoff ends in complete_claim (durably erased) or
            // release_claim (session returned to the detached pool),
            // both of which supersede this stale copy.
            // Capture jid-collision evictions in `sessions` before
            // retain mutates; same for `claimed`.
            if let Some(existing) = sessions.get(&stream_id) {
                displaced.push(existing.clone());
            }
            for (id, existing) in sessions.iter() {
                if id != &stream_id && existing.jid == jid {
                    displaced.push(existing.clone());
                }
            }
            for (id, existing) in claimed.iter() {
                if id != &stream_id && existing.jid == jid {
                    displaced.push(existing.clone());
                }
            }
            if displaced.iter().any(|candidate| {
                displacement_pending.current_reservation_active(&candidate.stream_id)
            }) {
                return Err(SmRegistryError::Internal(
                    "store_session: displaced promotion generation is already leased".to_string(),
                ));
            }
            // FIX 1 (council-adjudicated, ADR-0017 Phase 3 Slice 5
            // corrigenda): a `claimed_sessions` eviction here (same-stream
            // re-store or jid-collision) does NOT release its `ClaimStore`
            // entry — it flows through `displaced` above exactly like a
            // plain `sessions` eviction, and its claim is released only by
            // `confirm_drained`, after the caller's XEP-0198 §5 promotion
            // succeeds and the durable row is actually deleted. Releasing
            // eagerly here (the previous behavior) opened a window where a
            // second node's `restore_from_persistence`/orphan reaper could
            // observe the entity as unclaimed, hydrate its own in-memory
            // copy, and then have the durable row deleted out from under it
            // by this node's own later `confirm_drained` — the same
            // double-ownership hazard acquire-then-hydrate exists to
            // prevent, just reached via the eviction path instead of a
            // release-before-delete race.
            // The exact predecessor was captured above. Remove it before
            // applying the capacity policy so a same-id replacement does
            // not count the predecessor as a second live slot (or select it
            // again as the oldest eviction victim at capacity).
            sessions.remove(&stream_id);
            sessions.retain(|_, existing| existing.jid != jid);
            // Same-stream claimed entries are unreachable here (early
            // return above), but preserve one defensively so a claim is
            // never evicted by its own stream.
            claimed.retain(|existing_stream_id, existing| {
                existing_stream_id == &stream_id || existing.jid != jid
            });

            if sessions.len() >= self.max_sessions {
                // Remove oldest session
                if let Some(oldest_key) = sessions
                    .iter()
                    .min_by_key(|(_, s)| s.detached_at)
                    .map(|(k, _)| k.clone())
                {
                    if let Some(oldest) = sessions.remove(&oldest_key) {
                        debug!(stream_id = %oldest_key, "Evicted oldest SM session to make room");
                        displaced.push(oldest);
                    }
                }
            }

            for displaced_session in &displaced {
                let inserted = displacement_pending.insert_current(displaced_session);
                debug_assert!(inserted, "active displaced leases were preflighted");
            }
            let demoted = displacement_pending.demote_for_successor(&stream_id);
            debug_assert!(demoted, "same-stream lease was preflighted under its shard");
            sessions.insert(stream_id.clone(), session.clone());
            Ok(sessions.len())
        })();
        let count = match state_result {
            Ok(count) => count,
            Err(error) => {
                detach_reservation.cancel_if_owned(self, &stream_id);
                return Err(error);
            }
        };
        let same_id_predecessor = displaced
            .iter()
            .find(|displaced| displaced.stream_id == stream_id)
            .cloned();
        let rollback_current_generation = same_id_predecessor
            .as_ref()
            .map(|displaced| displaced.generation_id);
        let same_id_predecessor_fence = if same_id_predecessor.is_some() {
            self.claim_fences
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .get(&stream_id)
                .cloned()
        } else {
            None
        };
        let mut displaced_guard = DisplacedPromotionGuard::new(self, displaced);
        if let Some(predecessor) = same_id_predecessor.as_ref() {
            displaced_guard.mark_publication_unknown(predecessor);
            rejected_detach_guard.mark_publication_unknown();
        } else {
            rejected_detach_guard.mark_snapshot_commit_unknown();
        }
        // Durable rows for displaced sessions are deliberately NOT
        // deleted here. They follow the drain_expired/confirm_drained
        // persist-until-confirmed contract: the caller promotes each
        // displaced session's unacked queue (XEP-0198 §5 alt-resource
        // → offline storage → error chain) and calls
        // `confirm_drained` on success, which erases the rows. If the
        // process crashes before promotion, restore_from_persistence
        // rehydrates them and the SM-expiry janitor retries.
        //
        // FIX 1: this now includes former `claimed_sessions` evictions too
        // — their `ClaimStore` claim stays held (re-entering `sessions` via
        // the snapshot-failure retry path below keeps it, since a plain
        // `sessions` entry is claim-backed under this slice's
        // acquire-then-hydrate/acquire-on-detach invariant) until
        // `confirm_drained` releases it after the durable delete commits.

        // `store_session` publishes the session in memory before its first
        // durable snapshot is written so the cleanup path can keep draining
        // the old live channel. Hold the same stream lock used by detached
        // append snapshots until the initial snapshot has landed; otherwise a
        // concurrent append can persist a newer queue and then get overwritten
        // by this stale first snapshot.
        //
        // Displaced-session limbo guard: at this point the displaced
        // sessions are already off both maps. If the snapshot write
        // fails we return Err and the caller drops the displaced vec —
        // without re-insertion their durable rows would be stranded
        // (invisible to drain_expired, which scans memory only) until
        // a restart. Re-insert them as expired-for-retry BEFORE
        // propagating the error so the SM-expiry janitor's next pass
        // runs their promote → confirm chain. The map insert happens
        // WITHOUT taking each displaced stream's shard lock — we hold
        // this stream's shard lock, and two crossed store_session
        // calls re-inserting each other's displaced sessions would
        // otherwise deadlock.
        let persistence_result = match same_id_predecessor.as_ref() {
            Some(predecessor) => {
                self.persist_detached_session_replacement(&session, predecessor)
                    .await
            }
            None => self
                .persist_detached_session_snapshot(&session)
                .await
                .map(|()| PersistDetachedReplacementOutcome::Committed),
        };
        match persistence_result {
            Ok(PersistDetachedReplacementOutcome::Committed) => {}
            Ok(PersistDetachedReplacementOutcome::PublicationUnknown(error)) => {
                self.park_publication_unknown(session.clone())?;
                if let Some(predecessor) = same_id_predecessor.as_ref() {
                    displaced_guard.forget_generation(predecessor);
                    self.park_publication_unknown(predecessor.clone())?;
                }
                rejected_detach_guard.complete();
                detach_reservation.cancel_if_owned(self, &stream_id);
                return Err(SmRegistryError::Persistence(error));
            }
            Err(error) => {
                let definitely_not_committed = matches!(
                error,
                SmRegistryError::Persistence(
                    super::super::persistence::SmPersistenceError::SnapshotDefinitelyNotCommitted(
                        _
                    )
                )
            );
                let authority_lost = matches!(
                    error,
                    SmRegistryError::Persistence(
                        super::super::persistence::SmPersistenceError::NotOwner { .. }
                    )
                );
                if definitely_not_committed {
                    detach_reservation.cancel_if_owned(self, &stream_id);
                    if let Some(predecessor) = same_id_predecessor.as_ref() {
                        displaced_guard.forget_generation(predecessor);
                        rejected_detach_guard.remove_successor_from_resumability()?;
                        if let Err(restore_error) =
                            self.restore_resumable_after_uncommitted_replace(predecessor.clone())
                        {
                            self.park_publication_unknown(session.clone())?;
                            self.park_publication_unknown(predecessor.clone())?;
                            rejected_detach_guard.complete();
                            return Err(restore_error);
                        }
                        rejected_detach_guard.reject_with_resumable_predecessor();
                    } else {
                        rejected_detach_guard
                            .reject_definitely_not_committed(rollback_current_generation);
                    }
                } else if authority_lost {
                    displaced_guard.clear_publication_unknown();
                    // A fenced NotOwner is not commit-ambiguous: the transaction
                    // rejected this node before publishing the successor. Do not
                    // leave an unclaimed local resume copy, and do not reassert a
                    // same-id predecessor whose exact fence is now stale. Retain
                    // that old fence only as terminal exact-release inventory;
                    // the predecessor remains an obsolete payload-only token.
                    let stale_fence = self
                        .claim_fences
                        .read()
                        .ok()
                        .and_then(|fences| fences.get(&stream_id).cloned());
                    if let Some(stale_fence) = stale_fence {
                        if self.try_record_terminal_claim_fence_for_detach(
                            &stream_id,
                            stale_fence.clone(),
                            detach_reservation,
                        ) {
                            self.forget_claim_locally_locked(&stream_id, Some(&stale_fence));
                        }
                    } else {
                        detach_reservation.cancel_if_owned(self, &stream_id);
                    }
                    rejected_detach_guard.reject_authority_lost();
                } else {
                    // Commit acknowledgement is ambiguous. The successor may be
                    // the durable row, so removing it from memory would make a
                    // self-owned committed snapshot invisible to both resume and
                    // orphan recovery. Keep it published and complete the same
                    // bounded claim/reconciliation handoff as a successful
                    // snapshot, while leaving the predecessor obsolete.
                    detach_reservation_guard.transfer();
                    let claim_outcome = self
                        .acquire_claim_store_entry_for_detach(
                            &stream_id,
                            session.generation_id,
                            detach_reservation,
                        )
                        .await;
                    match claim_outcome {
                        DetachClaimAcquisitionOutcome::Established => {
                            rejected_detach_guard.complete();
                            return Err(match error {
                                SmRegistryError::Persistence(source) => {
                                    SmRegistryError::ResumabilityPreserved(source)
                                }
                                error => error,
                            });
                        }
                        DetachClaimAcquisitionOutcome::AmbiguousTracked => {
                            rejected_detach_guard.complete();
                            tracing::warn!(
                                stream_id = %stream_id,
                                persistence_error = %error,
                                "store_session: both snapshot acknowledgement and claim acquisition \
                                 remain ambiguous"
                            );
                            return Err(SmRegistryError::DetachClaimAmbiguous);
                        }
                        DetachClaimAcquisitionOutcome::Rejected(rejection) => {
                            if self.retire_detach_after_definite_claim_rejection(
                                &stream_id,
                                session.generation_id,
                                true,
                            ) {
                                rejected_detach_guard.complete();
                            } else {
                                rejected_detach_guard.reject_claim_acquisition();
                            }
                            tracing::warn!(
                                stream_id = %stream_id,
                                persistence_error = %error,
                                ?rejection,
                                "store_session: ambiguous snapshot lost detach claim authority; \
                                 retaining payload as an unowned durable carrier"
                            );
                            return Err(SmRegistryError::DetachClaimRejected);
                        }
                    }
                }
                return Err(error);
            }
        }

        // The atomic commit already made the predecessor a generation-keyed
        // terminal row. Publish that exact authority before B's claim
        // acquisition can branch or become ambiguous; otherwise A could be
        // leased as a payload-only obsolete carrier and its durable terminal
        // row would be orphaned.
        if let Some(predecessor) = same_id_predecessor.as_ref() {
            let mark_result = same_id_predecessor_fence.clone().ok_or_else(|| {
                SmRegistryError::Internal(
                    "same-id terminal predecessor lacks its pre-replacement claim fence"
                        .to_string(),
                )
            });
            let mark_result = match mark_result {
                Ok(fence) => self
                    .retain_terminal_durable_promotion(predecessor.clone(), 0, fence)
                    .map(|_| ()),
                Err(error) => Err(error),
            };
            if let Err(error) = mark_result {
                self.park_publication_unknown(session.clone())?;
                displaced_guard.forget_generation(predecessor);
                self.park_publication_unknown(predecessor.clone())?;
                rejected_detach_guard.complete();
                detach_reservation.cancel_if_owned(self, &stream_id);
                return Err(error);
            }
            displaced_guard.clear_publication_unknown();
            rejected_detach_guard.mark_snapshot_commit_unknown();
        }

        // ADR-0017 Phase 3 Slice 5: acquire (or self-reacquire) this node's
        // `ClaimStore` claim for the just-detached session now that its
        // durable snapshot has landed — see
        // `claims.rs::acquire_claim_store_entry_for_detach`'s doc comment
        // for the acquire-on-detach half of the acquire-then-hydrate
        // invariant this slice establishes.
        detach_reservation_guard.transfer();
        match self
            .acquire_claim_store_entry_for_detach(
                &stream_id,
                session.generation_id,
                detach_reservation,
            )
            .await
        {
            DetachClaimAcquisitionOutcome::Established => {
                rejected_detach_guard.complete();
            }
            DetachClaimAcquisitionOutcome::AmbiguousTracked => {
                rejected_detach_guard.complete();
                return Err(SmRegistryError::DetachClaimAmbiguous);
            }
            DetachClaimAcquisitionOutcome::Rejected(rejection) => {
                if self.retire_detach_after_definite_claim_rejection(
                    &stream_id,
                    session.generation_id,
                    true,
                ) {
                    rejected_detach_guard.complete();
                } else {
                    rejected_detach_guard.reject_claim_acquisition();
                }
                tracing::warn!(
                    stream_id = %stream_id,
                    ?rejection,
                    "store_session: persisted snapshot lost detach claim authority; \
                     retaining payload as an unowned durable carrier"
                );
                return Err(SmRegistryError::DetachClaimRejected);
            }
        }

        debug!(stream_id = %stream_id, count = count, "Stored detached SM session");
        Ok(displaced_guard.transfer())
    }

    async fn take_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        // Persist-first ordering (same rationale as complete_claim):
        // peek to see if the session exists, durably erase, then
        // remove from in-memory. Failure to durably erase aborts
        // the take so the caller can retry without leaving an
        // orphan row in storage that restart would resurrect.
        let (generation_id, claimed_generation_id) = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let generation_id = sessions.get(stream_id).map(|session| session.generation_id);
            let claimed_generation_id = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .get(stream_id)
                .map(|session| session.generation_id);
            (generation_id, claimed_generation_id)
        };
        let Some(generation_id) = generation_id else {
            debug!(stream_id = %stream_id, "SM session not found");
            return Ok(None);
        };
        let mut retiring_generations = vec![generation_id];
        if claimed_generation_id.is_some_and(|claimed| claimed != generation_id) {
            retiring_generations.push(claimed_generation_id.expect("checked as present"));
        }
        self.persist_delete_session(stream_id).await?;
        // Keep the exact map carriers recoverable across cancellation while
        // persistence proves whether a same-id terminal sibling still needs
        // the shared claim. The probe explicitly ignores only the generations
        // this take will remove synchronously below.
        let durable_work_remains = self
            .durable_work_may_remain_ignoring_map_generations(stream_id, &retiring_generations)
            .await;
        if durable_work_remains {
            // Once both map carriers disappear, an active fence alone is not
            // scanned by release retries. Convert it synchronously to exact
            // durable-recovery inventory before removing the final carrier.
            self.retain_claim_for_durable_recovery(stream_id)?;
        }
        let removed_session = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if sessions
                .get(stream_id)
                .is_some_and(|session| session.generation_id == generation_id)
            {
                sessions.remove(stream_id)
            } else {
                None
            }
        };
        let claimed_removed = {
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if claimed_generation_id.is_some_and(|generation_id| {
                claimed
                    .get(stream_id)
                    .is_some_and(|session| session.generation_id == generation_id)
            }) {
                claimed.remove(stream_id).is_some()
            } else {
                false
            }
        };
        let detached_removed = removed_session.is_some();
        let removed = removed_session.and_then(|session| {
            if session.is_expired() {
                debug!(stream_id = %stream_id, "SM session found but expired");
                None
            } else {
                debug!(stream_id = %stream_id, "Retrieved and removed SM session");
                Some(session)
            }
        });
        if !detached_removed {
            debug!(stream_id = %stream_id, "SM session changed before exact removal");
        }
        // Removing a claimed copy ends that claim: release its
        // `ClaimStore` entry (after the guards drop — release awaits).
        // ADR-0017 Phase 3 Slice 5: a plain (non-claimed) `sessions` removal
        // ends the claim too, REGARDLESS of the returned value's expiry
        // (`detached_removed` tracks the actual map removal, not the
        // filtered `removed` return value) — the durable row is already
        // gone (`persist_delete_session` above), and a plain `sessions`
        // entry is claim-backed under this slice's
        // acquire-then-hydrate/acquire-on-detach invariant.
        if (claimed_removed || detached_removed) && !durable_work_remains {
            self.release_claim_store_entry(stream_id).await;
        }
        Ok(removed)
    }

    async fn peek_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        match sessions.get(stream_id) {
            Some(session) => {
                if session.is_expired() {
                    Ok(None)
                } else {
                    Ok(Some(session.clone()))
                }
            }
            None => Ok(None),
        }
    }

    async fn cleanup_expired(&self) -> Result<usize, SmRegistryError> {
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

        let mut removed = 0usize;
        for stream_id in &expired_ids {
            let stream_lock = self.stream_lock(stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let expired_session = {
                let sessions = self
                    .sessions
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                match sessions.get(stream_id) {
                    Some(session) if session.is_expired() => Some(session.clone()),
                    _ => None,
                }
            };
            let Some(expired_session) = expired_session else {
                continue;
            };
            // Best-effort: cleanup paths log and continue rather
            // than aborting the whole sweep on a single bad row.
            // Restart-time expired-filter still drops anything that
            // slipped through.
            match self.persist_delete_session(stream_id).await {
                Ok(()) => {
                    // Keep the expired carrier locally recoverable until the
                    // durable sibling probe completes. Cancellation or a
                    // transient probe failure can therefore be retried by the
                    // next cleanup sweep instead of stranding a live claim.
                    let claimed_work_remains = self
                        .claimed_sessions
                        .read()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                        .contains_key(stream_id);
                    let durable_work_remains = claimed_work_remains
                        || self
                            .durable_work_may_remain_ignoring_map_generations(
                                stream_id,
                                &[expired_session.generation_id],
                            )
                            .await;
                    if durable_work_remains && !claimed_work_remains {
                        // A fail-closed durable probe is not itself a retry
                        // carrier. Publish the exact retained handoff before
                        // removing the last map entry so recovery can later
                        // re-probe and release after storage recovers.
                        if let Err(error) = self.retain_claim_for_durable_recovery(stream_id) {
                            debug!(
                                stream_id = %stream_id,
                                error = %error,
                                "expired SM session could not retain claim recovery inventory"
                            );
                            continue;
                        }
                    }
                    let removed_session = {
                        let mut sessions = self
                            .sessions
                            .write()
                            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                        if sessions.get(stream_id).is_some_and(|session| {
                            session.generation_id == expired_session.generation_id
                                && session.is_expired()
                        }) {
                            sessions.remove(stream_id)
                        } else {
                            None
                        }
                    };
                    if removed_session.is_none() {
                        debug!(
                            stream_id = %stream_id,
                            generation_id = %expired_session.generation_id,
                            "expired SM session changed before exact removal"
                        );
                        continue;
                    }
                    removed += 1;
                    // ADR-0017 Phase 3 Slice 5: the durable row and the
                    // in-memory entry are both gone — release the
                    // `ClaimStore` claim this node held for it only when no
                    // exact terminal generation still shares that claim.
                    if !durable_work_remains {
                        self.release_claim_store_entry(stream_id).await;
                    }
                }
                Err(error) => {
                    debug!(
                        stream_id = %stream_id,
                        error = %error,
                        "expired SM session: durable delete failed in cleanup; \
                         restart-time expiry filter will drop the orphan"
                    );
                }
            }
        }
        Ok(removed)
    }

    async fn session_count(&self) -> usize {
        self.sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    async fn scrub_unacked_for_tombstone(
        &self,
        target: &TombstoneTarget,
    ) -> Result<usize, SmRegistryError> {
        // Phase 0 (round-2 review R2): record the tombstone identity
        // BEFORE any scrub phase runs so a promotion already holding a
        // drained copy of a session (off both maps, pending row not
        // yet inserted) re-checks it and drops matching stanzas
        // instead of delivering retracted content on the next login.
        let scrub_recorded_at = chrono::Utc::now();
        let scrub_horizon = scrub_recorded_at + super::TOMBSTONE_CLOCK_SKEW_SLACK;
        self.record_recent_tombstone(target)?;
        // Phase 1 (issue #1145 lock-scope fix): snapshot every queue
        // under READ locks only. XML parsing of every entry used to
        // run under the sessions write lock, stalling all detach /
        // resume traffic for the duration of a full-registry scan.
        let mut snapshots: Vec<(
            String,
            super::SmSessionGenerationId,
            Vec<(u32, String, chrono::DateTime<chrono::Utc>)>,
        )> = Vec::new();
        {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            for (stream_id, session) in sessions.iter() {
                snapshots.push((
                    stream_id.clone(),
                    session.generation_id,
                    session
                        .unacked_stanzas
                        .iter()
                        .map(|entry| {
                            (
                                entry.sequence,
                                entry.stanza_xml.clone(),
                                entry.original_receipt_at,
                            )
                        })
                        .collect(),
                ));
            }
        }
        {
            let retries = self
                .pending_promotion_retries
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            for (stream_id, session) in retries.iter() {
                snapshots.push((
                    stream_id.clone(),
                    session.generation_id,
                    session
                        .unacked_stanzas
                        .iter()
                        .map(|entry| {
                            (
                                entry.sequence,
                                entry.stanza_xml.clone(),
                                entry.original_receipt_at,
                            )
                        })
                        .collect(),
                ));
            }
        }
        {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            for (stream_id, session) in claimed.iter() {
                snapshots.push((
                    stream_id.clone(),
                    session.generation_id,
                    session
                        .unacked_stanzas
                        .iter()
                        .map(|entry| {
                            (
                                entry.sequence,
                                entry.stanza_xml.clone(),
                                entry.original_receipt_at,
                            )
                        })
                        .collect(),
                ));
            }
        }

        // Phase 2: parse and match with NO registry lock held. A
        // queue can change between snapshot and removal; removing by
        // exact (stream_id, sequence) pairs below is safe regardless.
        let mut matched: Vec<(String, super::SmSessionGenerationId, Vec<u32>)> = Vec::new();
        for (stream_id, generation_id, entries) in snapshots {
            let eligible = entries
                .into_iter()
                .filter(|(_, _, received_at)| *received_at <= scrub_horizon)
                .map(|(sequence, xml, _)| (sequence, xml))
                .collect::<Vec<_>>();
            let sequences = matching_tombstone_sequences(&eligible, target);
            if !sequences.is_empty() {
                matched.push((stream_id, generation_id, sequences));
            }
        }

        // Phase 3 (issue #1145 durability fix): per exact generation, under its
        // stream lock (serializing with detached-append snapshots so a
        // concurrent full-snapshot write cannot resurrect rows), erase
        // durable rows FIRST only when this exact generation still owns the
        // bare durable stream. Obsolete retry generations are payload-only:
        // deleting their sequence numbers from the successor's durable row
        // could erase unrelated content. If a required durable delete fails,
        // that exact generation's in-memory entries remain in place.
        let mut removed_total = 0usize;
        let mut durable_failures = 0usize;
        for (stream_id, generation_id, sequences) in matched {
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let owns_durable_row =
                self.current_durable_generation_id(&stream_id)? == Some(generation_id);
            let terminal_fence = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .authority(&stream_id, generation_id)
                .filter(|authority| {
                    *authority == super::SmSessionPromotionAuthority::TerminalDurable
                })
                .and_then(|_| {
                    self.pending_promotions
                        .read()
                        .ok()?
                        .claim_fence(&stream_id, generation_id)
                });
            if let Some(storage) = &self.persistence {
                let delete_result = if owns_durable_row {
                    let session_id = crate::pending_delivery::SmSessionId::new(stream_id.clone());
                    let fence = self
                        .claim_fences
                        .read()
                        .ok()
                        .and_then(|fences| fences.get(&stream_id).cloned());
                    match fence.as_ref() {
                        Some(fence) => {
                            storage
                                .delete_unacked_under_fence(&session_id, &sequences, fence)
                                .await
                        }
                        None if !storage.requires_exact_claim_fence() => {
                            storage.delete_unacked(&session_id, &sequences).await
                        }
                        None => Err(super::super::persistence::SmPersistenceError::NotOwner {
                            entity: crate::ownership::Entity::new(
                                crate::ownership::EntityType::SmSession,
                                stream_id.clone(),
                            ),
                        }),
                    }
                } else if terminal_fence.is_some()
                    || self
                        .pending_promotions
                        .read()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                        .authority(&stream_id, generation_id)
                        == Some(super::SmSessionPromotionAuthority::TerminalDurable)
                {
                    let key = super::super::persistence::SmTerminalGenerationKey::new(
                        crate::pending_delivery::SmSessionId::new(stream_id.clone()),
                        generation_id,
                    );
                    match terminal_fence.as_ref() {
                        Some(fence) => {
                            storage
                                .delete_terminal_unacked_under_fence(&key, &sequences, fence)
                                .await
                        }
                        None if !storage.requires_exact_claim_fence() => {
                            storage.delete_terminal_unacked(&key, &sequences).await
                        }
                        None => Err(super::super::persistence::SmPersistenceError::NotOwner {
                            entity: crate::ownership::Entity::new(
                                crate::ownership::EntityType::SmSession,
                                stream_id.clone(),
                            ),
                        }),
                    }
                } else {
                    Ok(0)
                };
                if let Err(error) = delete_result {
                    durable_failures += 1;
                    debug!(
                        stream_id = %stream_id,
                        generation_id = ?generation_id,
                        error = %error,
                        "tombstone scrub: exact-generation durable delete failed; \
                         keeping that generation's in-memory entries"
                    );
                    continue;
                }
            }
            removed_total = removed_total.saturating_add(self.scrub_local_tombstone_generation(
                &stream_id,
                generation_id,
                &sequences,
                &scrub_horizon,
            )?);
        }

        // Phase 4 (durable-side sweep): durable rows can exist for
        // streams absent from BOTH in-memory maps — displaced
        // mid-promotion, janitor-drained mid-promotion, or parked
        // between a promotion failure and its retry re-insert
        // (persist-until-confirmed contract). Those rows are invisible
        // to phases 1-3, but a restart resurrects and promotes them,
        // so a retraction landing in that window must scrub them too.
        // COST NOTE: this enumerates every durable session, then re-reads each
        // queue under its stream lock. Scrubs are rare (retraction / moderation
        // only), so the N+1 scan is acceptable. The locked re-read is required:
        // using queue contents captured before this lock could race a
        // same-stream successor snapshot and delete its unrelated same-sequence
        // row.
        if let Some(storage) = &self.persistence {
            let stored = storage
                .list_all_sessions()
                .await
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
            for persisted in stored {
                let persisted_stream_id = persisted.stream_id.clone();
                let stream_id = persisted.stream_id.as_str().to_string();
                // Stream lock first: serializes with store_session /
                // reinsert_for_retry so the durable delete and the
                // current local-inventory scrub are atomic with respect
                // to a stream moving between retry, detached, and claimed
                // ownership. Do not skip rows merely because the stream is
                // currently in a map: it may have entered after phase 1.
                let stream_lock = self.stream_lock(&stream_id)?;
                let _stream_guard = stream_lock.lock().await;
                let unacked = match storage.list_unacked(&persisted_stream_id).await {
                    Ok(unacked) => unacked,
                    Err(error) => {
                        durable_failures += 1;
                        debug!(
                            stream_id = %stream_id,
                            error = %error,
                            "tombstone scrub: locked durable queue re-read failed; preserving \
                             rows and surfacing the possible replay"
                        );
                        continue;
                    }
                };
                let sequences: Vec<u32> = unacked
                    .iter()
                    .filter(|entry| {
                        entry.original_receipt_at <= scrub_horizon
                            && target.matches_message_element(&entry.stanza.to_element())
                    })
                    .map(|entry| entry.sequence)
                    .collect();
                if sequences.is_empty() {
                    continue;
                }
                let local_current_generation = self.current_durable_generation_id(&stream_id)?;
                let active_fence = self
                    .claim_fences
                    .read()
                    .ok()
                    .and_then(|fences| fences.get(&stream_id).cloned());
                let delete_result = match active_fence.as_ref() {
                    Some(fence) => {
                        storage
                            .delete_unacked_under_fence(&persisted_stream_id, &sequences, fence)
                            .await
                    }
                    None if !storage.requires_exact_claim_fence() => {
                        storage
                            .delete_unacked(&persisted_stream_id, &sequences)
                            .await
                    }
                    None => Err(super::super::persistence::SmPersistenceError::NotOwner {
                        entity: crate::ownership::Entity::new(
                            crate::ownership::EntityType::SmSession,
                            stream_id.clone(),
                        ),
                    }),
                };
                match delete_result {
                    Ok(deleted) => {
                        removed_total += deleted as usize;
                        if let Some(generation_id) = local_current_generation {
                            // The durable row is keyed by bare stream ID and
                            // therefore represents only the current durable
                            // generation. Mirror its deletion into that exact
                            // local generation; an obsolete retry may reuse the
                            // same sequence for unrelated content and must be
                            // matched independently in phases 1-3.
                            let _ = self.scrub_local_tombstone_generation(
                                &stream_id,
                                generation_id,
                                &sequences,
                                &scrub_horizon,
                            )?;
                        }
                    }
                    Err(error) => {
                        durable_failures += 1;
                        debug!(
                            stream_id = %stream_id,
                            error = %error,
                            "tombstone scrub: durable-side delete_unacked failed for an \
                             off-map stream; rows preserved and the caller's error path \
                             surfaces the possible replay"
                        );
                    }
                }
            }

            // Terminal generations use their exact generation-qualified
            // namespace. Re-read each one under the stream shard before
            // matching so an atomic same-id replacement or partial prune
            // cannot race this scrub and make A's sequence delete B's row.
            let terminal_generations = storage
                .list_terminal_generations()
                .await
                .map_err(|error| SmRegistryError::Internal(error.to_string()))?;
            for terminal_entry in terminal_generations {
                let key = terminal_entry.key().clone();
                if let super::super::persistence::TerminalGenerationScanEntry::Corrupt {
                    detail,
                    ..
                } = terminal_entry
                {
                    durable_failures += 1;
                    debug!(
                        terminal_generation = %key,
                        %detail,
                        "tombstone scrub: corrupt terminal generation requires recovery \
                         quarantine before it can be inspected"
                    );
                    continue;
                }
                let stream_id = key.stream_id().as_str().to_string();
                let generation_id = key.generation_id();
                let stream_lock = self.stream_lock(&stream_id)?;
                let _stream_guard = stream_lock.lock().await;
                let terminal = match storage.get_terminal_generation(&key).await {
                    Ok(Some(terminal)) => terminal,
                    Ok(None) => continue,
                    Err(error) => {
                        durable_failures += 1;
                        debug!(
                            terminal_generation = %key,
                            %error,
                            "tombstone scrub: exact terminal re-read failed; preserving rows"
                        );
                        continue;
                    }
                };
                let sequences = terminal
                    .snapshot()
                    .unacked()
                    .iter()
                    .filter(|entry| {
                        entry.original_receipt_at <= scrub_horizon
                            && target.matches_message_element(&entry.stanza.to_element())
                    })
                    .map(|entry| entry.sequence)
                    .collect::<Vec<_>>();
                if sequences.is_empty() {
                    continue;
                }
                let retained_fence = self
                    .pending_promotions
                    .read()
                    .ok()
                    .and_then(|promotions| promotions.claim_fence(&stream_id, generation_id));
                let active_fence = self
                    .claim_fences
                    .read()
                    .ok()
                    .and_then(|fences| fences.get(&stream_id).cloned());
                let fence = retained_fence.or(active_fence);
                let delete_result = match fence.as_ref() {
                    Some(fence) => {
                        storage
                            .delete_terminal_unacked_under_fence(&key, &sequences, fence)
                            .await
                    }
                    None if !storage.requires_exact_claim_fence() => {
                        storage.delete_terminal_unacked(&key, &sequences).await
                    }
                    None => Err(super::super::persistence::SmPersistenceError::NotOwner {
                        entity: crate::ownership::Entity::new(
                            crate::ownership::EntityType::SmSession,
                            stream_id.clone(),
                        ),
                    }),
                };
                match delete_result {
                    Ok(deleted) => {
                        removed_total = removed_total.saturating_add(deleted as usize);
                        removed_total =
                            removed_total.saturating_add(self.scrub_local_tombstone_generation(
                                &stream_id,
                                generation_id,
                                &sequences,
                                &scrub_horizon,
                            )?);
                    }
                    Err(error) => {
                        durable_failures += 1;
                        debug!(
                            terminal_generation = %key,
                            %error,
                            "tombstone scrub: exact terminal delete failed; preserving rows"
                        );
                    }
                }
            }
        }

        if durable_failures > 0 {
            return Err(SmRegistryError::Internal(format!(
                "tombstone scrub: durable delete_unacked failed for {durable_failures} \
                 stream(s); matching entries were preserved for retry"
            )));
        }
        Ok(removed_total)
    }
}
