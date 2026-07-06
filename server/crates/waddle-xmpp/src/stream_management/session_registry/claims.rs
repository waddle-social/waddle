use jid::FullJid;
use tracing::debug;

use crate::ownership::{ClaimError, Entity, EntityType};

use super::core::InMemorySmSessionRegistry;
use super::{DetachedSession, SmClaimCompletion, SmRegistryError};

/// The `ClaimStore` entity naming an SM session's ownership claim (element
/// 8: `entity_type = sm_session`, `entity = ` the SM-ID/stream id).
fn sm_session_entity(stream_id: &str) -> Entity {
    Entity::new(EntityType::SmSession, stream_id.to_string())
}

impl InMemorySmSessionRegistry {
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
        let mut drained = Vec::with_capacity(stream_ids.len());
        for stream_id in &stream_ids {
            let stream_lock = self.stream_lock(stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            if let Some(session) = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id)
            {
                drained.push(session);
            }
        }
        Ok(drained)
    }

    /// Snapshot every currently-live SM session id (detached +
    /// claimed). Returns `None` if either internal lock is poisoned
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
        let mut out: Vec<String> = sessions.keys().cloned().collect();
        out.extend(claimed.keys().cloned());
        Some(out)
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
    pub async fn confirm_drained(&self, stream_id: &str) {
        let stream_lock = match self.stream_lock(stream_id) {
            Ok(lock) => lock,
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "graceful-shutdown drain: stream lock lookup failed before durable delete"
                );
                return;
            }
        };
        let _stream_guard = stream_lock.lock().await;
        if let Err(error) = self.persist_delete_session(stream_id).await {
            debug!(
                stream_id = %stream_id,
                error = %error,
                "graceful-shutdown drain: durable delete failed; \
                 restart-time expiry filter will catch the orphan"
            );
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
        let mut drained = Vec::with_capacity(expired_ids.len());
        for stream_id in &expired_ids {
            let stream_lock = self.stream_lock(stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let removed = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                match sessions.get(stream_id) {
                    Some(session) if session.is_expired() => sessions.remove(stream_id),
                    _ => None,
                }
            };
            if let Some(session) = removed {
                drained.push(session);
            }
        }
        if !drained.is_empty() {
            debug!(removed = drained.len(), "Cleaned up expired SM sessions");
        }
        Ok(drained)
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
        self.reinsert_for_retry_unlocked(session)
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
        self.sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .insert(session.stream_id.clone(), session);
        Ok(())
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
            return Ok(None);
        };
        if session.is_expired() {
            // Left untouched in `sessions` (never removed) so the
            // janitor's next `drain_expired` pass still finds it — a
            // #1098-hydrated expired session must stay visible for its
            // unacked queue to run the XEP-0198 §5 promote → confirm
            // chain.
            return Ok(None);
        }

        let entity = sm_session_entity(stream_id);
        let epoch = match self.claim_store.acquire(&entity, &self.node_identity).await {
            Ok(epoch) => epoch,
            Err(ClaimError::AlreadyClaimed) => return Ok(None),
            Err(other) => {
                return Err(SmRegistryError::Internal(format!(
                    "claim_session: ClaimStore acquire failed for an entity this \
                     registry believed was unclaimed: {other}"
                )));
            }
        };

        // The store granted the claim: perform the matching in-memory move.
        // `stream_id` is guaranteed still present and unexpired here — this
        // stream's `tokio::sync::Mutex` guard has been held continuously
        // since the peek above, and every other mutator of this stream's
        // map entry takes that same lock first — but the removal is
        // handled defensively (never `unwrap`/`expect`) rather than assumed,
        // releasing the just-granted store claim so it cannot leak if that
        // invariant is ever violated by a future change.
        let removed = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.remove(stream_id)
        };
        let Some(session) = removed else {
            self.release_claim_store_entry_under(stream_id, epoch).await;
            return Ok(None);
        };
        {
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed.insert(stream_id.to_string(), session.clone());
        }
        if let Ok(mut epochs) = self.claim_epochs.write() {
            epochs.insert(stream_id.to_string(), epoch);
        }
        Ok(Some(session))
    }

    /// Release a previously claimed session without consuming it.
    ///
    /// Also releases this entity's `ClaimStore` ownership claim, mirroring
    /// [`Self::claim_session`]'s acquire — every terminal path that removes
    /// a session from `claimed_sessions` must release the matching store
    /// entry, or that entity's claim leaks forever (ADR-0017 Phase 3 Slice
    /// 1 fix: this was previously true of every terminal branch here, in
    /// [`Self::complete_claim`]/[`Self::complete_claim_if_resumable`], and
    /// in [`Self::invalidate_sessions_for_jid`]).
    pub async fn release_claim(&self, stream_id: &str) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let session = claimed.remove(stream_id);
            if let Some(session) = session {
                if !session.is_expired() {
                    sessions.insert(stream_id.to_string(), session);
                }
            }
        }
        self.release_claim_store_entry(stream_id).await;
        Ok(())
    }

    /// Release `stream_id`'s `ClaimStore` entry under whatever epoch this
    /// registry last observed for it (falling back to epoch 0 if none was
    /// recorded — harmless for the in-process store, and `release`'s own
    /// contract treats a losing epoch as a no-op rather than an error).
    /// `pub(super)` because every removal from `claimed_sessions` must
    /// release its claim, and two of those paths live in `trait_impl`
    /// (`store_session`'s jid-collision eviction, `take_session`).
    pub(super) async fn release_claim_store_entry(&self, stream_id: &str) {
        let epoch = self
            .claim_epochs
            .write()
            .ok()
            .and_then(|mut epochs| epochs.remove(stream_id))
            .unwrap_or(crate::ownership::ClaimEpoch(0));
        self.release_claim_store_entry_under(stream_id, epoch).await;
    }

    /// Release `stream_id`'s `ClaimStore` entry under an explicitly-known
    /// epoch (the caller already holds it, so there is nothing to look up
    /// in `claim_epochs`). Best-effort: a `ClaimStore::release` failure is
    /// logged, never propagated — `release`'s own contract treats a losing
    /// epoch as a no-op, and this is the same "claim already gone" case.
    async fn release_claim_store_entry_under(
        &self,
        stream_id: &str,
        epoch: crate::ownership::ClaimEpoch,
    ) {
        let entity = sm_session_entity(stream_id);
        if let Err(error) = self
            .claim_store
            .release(&entity, &self.node_identity, epoch)
            .await
        {
            debug!(
                stream_id = %stream_id,
                error = %error,
                "ClaimStore release failed (best-effort: release's own contract treats \
                 a losing epoch as a no-op, not an error)"
            );
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
            if !session.is_expired() && client_h > session.outbound_count {
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
                    // Terminal path (ADR-0017 Phase 3 Slice 1 fix): the
                    // claim ends here just as surely as a `Resumed`/
                    // `Expired` completion — the store entry must be
                    // released or it leaks forever.
                    self.release_claim_store_entry(stream_id).await;
                    return Ok(Some(SmClaimCompletion::HandledCountTooHigh(restored)));
                }
                return Ok(None);
            }
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
                    // Terminal path (ADR-0017 Phase 3 Slice 1 fix): same
                    // leak as `HandledCountTooHigh` above.
                    self.release_claim_store_entry(stream_id).await;
                    return Ok(Some(SmClaimCompletion::ReplayWindowTruncated(restored)));
                }
                return Ok(None);
            }
        }
        self.persist_delete_session(stream_id).await?;
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
        let removed = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
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
            let removed_claimed = {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let mut claimed = self
                    .claimed_sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                if let Some(session) = sessions.remove(stream_id) {
                    removed.push(session);
                }
                claimed.remove(stream_id)
            };
            if let Some(session) = removed_claimed {
                // Terminal path (ADR-0017 Phase 3 Slice 1 fix): a fresh
                // bind displacing a *claimed* session ends that claim just
                // as surely as `release_claim`/`complete_claim` do —
                // release the store entry so it does not leak.
                self.release_claim_store_entry(stream_id).await;
                removed.push(session);
            }
        }
        Ok(removed)
    }
}
