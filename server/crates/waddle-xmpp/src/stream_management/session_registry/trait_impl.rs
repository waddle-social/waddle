use async_trait::async_trait;
use tracing::debug;

use super::core::{DetachClaimFenceReservation, InMemorySmSessionRegistry};
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
    armed: bool,
}

impl<'a> DisplacedPromotionGuard<'a> {
    fn new(registry: &'a InMemorySmSessionRegistry, sessions: Vec<DetachedSession>) -> Self {
        Self {
            registry,
            sessions,
            armed: true,
        }
    }

    fn transfer(mut self) -> Vec<DetachedSession> {
        self.armed = false;
        std::mem::take(&mut self.sessions)
    }

    fn rollback(&mut self) {
        for session in self.sessions.drain(..) {
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

impl InMemorySmSessionRegistry {
    pub(super) async fn store_session_with_principal_inner(
        &self,
        session: DetachedSession,
        principal: Option<&crate::auth::AuthenticatedPrincipalRef>,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let stream_id = session.stream_id.clone();
        let jid = session.jid.clone();
        let stream_lock = self.stream_lock(&stream_id)?;
        let _stream_guard = stream_lock.lock().await;
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
            sessions.retain(|existing_stream_id, existing| {
                existing_stream_id == &stream_id || existing.jid != jid
            });
            // Same-stream claimed entries are unreachable here (early
            // return above), but keep the predicate symmetric with
            // `sessions` so a claim is never evicted by its own stream.
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
                displacement_pending.insert(displaced_session.stream_id.clone());
            }
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
        let displaced_guard = DisplacedPromotionGuard::new(self, displaced);
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
        if let Err(error) = self
            .persist_detached_session_snapshot(&session, principal)
            .await
        {
            detach_reservation.cancel_if_owned(self, &stream_id);
            return Err(error);
        }

        // ADR-0017 Phase 3 Slice 5: acquire (or self-reacquire) this node's
        // `ClaimStore` claim for the just-detached session now that its
        // durable snapshot has landed — see
        // `claims.rs::acquire_claim_store_entry_for_detach`'s doc comment
        // for the acquire-on-detach half of the acquire-then-hydrate
        // invariant this slice establishes.
        detach_reservation_guard.transfer();
        self.acquire_claim_store_entry_for_detach(&stream_id, detach_reservation)
            .await;

        debug!(stream_id = %stream_id, count = count, "Stored detached SM session");
        Ok(displaced_guard.transfer())
    }
}

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(
        &self,
        session: DetachedSession,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        self.store_session_with_principal_inner(session, None).await
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
        let exists = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.contains_key(stream_id)
        };
        if !exists {
            debug!(stream_id = %stream_id, "SM session not found");
            return Ok(None);
        }
        self.persist_delete_session(stream_id).await?;
        let (removed, detached_removed, claimed_removed) = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let (removed, detached_removed) = match sessions.remove(stream_id) {
                Some(session) => {
                    if session.is_expired() {
                        debug!(stream_id = %stream_id, "SM session found but expired");
                        (None, true)
                    } else {
                        debug!(stream_id = %stream_id, "Retrieved and removed SM session");
                        (Some(session), true)
                    }
                }
                None => {
                    debug!(stream_id = %stream_id, "SM session not found");
                    (None, false)
                }
            };
            let claimed_removed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id)
                .is_some();
            (removed, detached_removed, claimed_removed)
        };
        // Removing a claimed copy ends that claim: release its
        // `ClaimStore` entry (after the guards drop — release awaits).
        // ADR-0017 Phase 3 Slice 5: a plain (non-claimed) `sessions` removal
        // ends the claim too, REGARDLESS of the returned value's expiry
        // (`detached_removed` tracks the actual map removal, not the
        // filtered `removed` return value) — the durable row is already
        // gone (`persist_delete_session` above), and a plain `sessions`
        // entry is claim-backed under this slice's
        // acquire-then-hydrate/acquire-on-detach invariant.
        if claimed_removed || detached_removed {
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
            let removed_session = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                match sessions.get(stream_id) {
                    Some(session) if session.is_expired() => sessions.remove(stream_id),
                    _ => None,
                }
            };
            if removed_session.is_none() {
                continue;
            }
            removed += 1;
            // Best-effort: cleanup paths log and continue rather
            // than aborting the whole sweep on a single bad row.
            // Restart-time expired-filter still drops anything that
            // slipped through.
            match self.persist_delete_session(stream_id).await {
                Ok(()) => {
                    // ADR-0017 Phase 3 Slice 5: the durable row and the
                    // in-memory entry are both gone — release the
                    // `ClaimStore` claim this node held for it.
                    self.release_claim_store_entry(stream_id).await;
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
        let mut snapshots: Vec<(String, Vec<(u32, String, chrono::DateTime<chrono::Utc>)>)> =
            Vec::new();
        {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            for (stream_id, session) in sessions.iter() {
                snapshots.push((
                    stream_id.clone(),
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
        let mut matched: Vec<(String, Vec<u32>)> = Vec::new();
        for (stream_id, entries) in snapshots {
            let eligible = entries
                .into_iter()
                .filter(|(_, _, received_at)| *received_at <= scrub_horizon)
                .map(|(sequence, xml, _)| (sequence, xml))
                .collect::<Vec<_>>();
            let sequences = matching_tombstone_sequences(&eligible, target);
            if !sequences.is_empty() {
                matched.push((stream_id, sequences));
            }
        }

        // Phase 3 (issue #1145 durability fix): per stream, under its
        // stream lock (serializing with detached-append snapshots so a
        // concurrent full-snapshot write cannot resurrect the rows we
        // just deleted), erase the durable rows FIRST and only then
        // the in-memory entries. If the durable delete fails, the
        // in-memory entries are deliberately left in place — memory
        // and storage stay consistent, and the caller's error path
        // logs that the pre-scrub stanza may still replay so the
        // failure is never silent.
        let mut removed_total = 0usize;
        let mut durable_failures = 0usize;
        for (stream_id, sequences) in matched {
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            if let Some(storage) = &self.persistence {
                if let Err(error) = storage
                    .delete_unacked(
                        &crate::pending_delivery::SmSessionId::new(stream_id.clone()),
                        &sequences,
                    )
                    .await
                {
                    durable_failures += 1;
                    debug!(
                        stream_id = %stream_id,
                        error = %error,
                        "tombstone scrub: durable delete_unacked failed; keeping the \
                         in-memory entries so memory and storage stay consistent"
                    );
                    continue;
                }
            }
            let removed_here = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                match sessions.get_mut(&stream_id) {
                    Some(session) => {
                        let before = session.unacked_stanzas.len();
                        session.unacked_stanzas.retain(|entry| {
                            entry.original_receipt_at > scrub_horizon
                                || !sequences.contains(&entry.sequence)
                        });
                        Some(before - session.unacked_stanzas.len())
                    }
                    None => None,
                }
            };
            let removed_here = match removed_here {
                Some(count) => Some(count),
                None => {
                    let mut claimed = self
                        .claimed_sessions
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    match claimed.get_mut(&stream_id) {
                        Some(session) => {
                            let before = session.unacked_stanzas.len();
                            session.unacked_stanzas.retain(|entry| {
                                entry.original_receipt_at > scrub_horizon
                                    || !sequences.contains(&entry.sequence)
                            });
                            Some(before - session.unacked_stanzas.len())
                        }
                        None => None,
                    }
                }
            };
            let removed_here = match removed_here {
                Some(count) => count,
                None => {
                    let mut retries = self
                        .pending_promotion_retries
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    match retries.get_mut(&stream_id) {
                        Some(session) => {
                            let before = session.unacked_stanzas.len();
                            session.unacked_stanzas.retain(|entry| {
                                entry.original_receipt_at > scrub_horizon
                                    || !sequences.contains(&entry.sequence)
                            });
                            before - session.unacked_stanzas.len()
                        }
                        None => 0,
                    }
                }
            };
            removed_total += removed_here;
        }

        // Phase 4 (durable-side sweep): durable rows can exist for
        // streams absent from BOTH in-memory maps — displaced
        // mid-promotion, janitor-drained mid-promotion, or parked
        // between a promotion failure and its retry re-insert
        // (persist-until-confirmed contract). Those rows are invisible
        // to phases 1-3, but a restart resurrects and promotes them,
        // so a retraction landing in that window must scrub them too.
        // COST NOTE: this enumerates every durable session + queue;
        // scrubs are rare (retraction / moderation only), so a full
        // listing is acceptable here.
        if let Some(storage) = &self.persistence {
            let stored = storage
                .list_all_sessions_with_unacked()
                .await
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
            for (persisted, unacked) in stored {
                let stream_id = persisted.stream_id.as_str().to_string();
                // Stream lock first: serializes with store_session /
                // reinsert_for_retry so the durable delete and the
                // current local-inventory scrub are atomic with respect
                // to a stream moving between retry, detached, and claimed
                // ownership. Do not skip rows merely because the stream is
                // currently in a map: it may have entered after phase 1.
                let stream_lock = self.stream_lock(&stream_id)?;
                let _stream_guard = stream_lock.lock().await;
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
                match storage
                    .delete_unacked(
                        &crate::pending_delivery::SmSessionId::new(stream_id.clone()),
                        &sequences,
                    )
                    .await
                {
                    Ok(deleted) => {
                        removed_total += deleted as usize;
                        for inventory in [
                            &self.sessions,
                            &self.claimed_sessions,
                            &self.pending_promotion_retries,
                        ] {
                            if let Some(session) = inventory
                                .write()
                                .map_err(|_| {
                                    SmRegistryError::Internal("Lock poisoned".to_string())
                                })?
                                .get_mut(&stream_id)
                            {
                                session.unacked_stanzas.retain(|entry| {
                                    entry.original_receipt_at > scrub_horizon
                                        || !sequences.contains(&entry.sequence)
                                });
                            }
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
