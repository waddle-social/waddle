use async_trait::async_trait;
use tracing::{debug, warn};

use super::core::InMemorySmSessionRegistry;
use super::{DetachedSession, SmRegistryError, SmSessionRegistry};
use crate::tombstone::{matching_tombstone_sequences, TombstoneTarget};

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(
        &self,
        session: DetachedSession,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let stream_id = session.stream_id.clone();
        let jid = session.jid.clone();
        let stream_lock = self.stream_lock(&stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        // Scope the RwLock guards in a block so they're definitively
        // dropped before any await point. RwLockWriteGuard is not
        // Send, and explicit `drop()` doesn't satisfy the async
        // future's lifetime analysis. Capture eviction victims
        // (jid-collision retain + max_sessions oldest) IN FULL so the
        // caller can run XEP-0198 §5 promotion on their unacked
        // queues (issue #1097) — previously they were silently
        // dropped and their durable rows mirror-deleted.
        let mut displaced: Vec<DetachedSession> = Vec::new();
        let count = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
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
            sessions.retain(|existing_stream_id, existing| {
                existing_stream_id == &stream_id || existing.jid != jid
            });
            claimed.retain(|existing_stream_id, existing| {
                existing_stream_id != &stream_id && existing.jid != jid
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

            sessions.insert(stream_id.clone(), session.clone());
            sessions.len()
        };
        // Durable rows for displaced sessions are deliberately NOT
        // deleted here. They follow the drain_expired/confirm_drained
        // persist-until-confirmed contract: the caller promotes each
        // displaced session's unacked queue (XEP-0198 §5 alt-resource
        // → offline storage → error chain) and calls
        // `confirm_drained` on success, which erases the rows. If the
        // process crashes before promotion, restore_from_persistence
        // rehydrates them and the SM-expiry janitor retries.

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
        if let Err(error) = self.persist_detached_session_snapshot(&session).await {
            for displaced_session in displaced {
                if displaced_session.stream_id == stream_id {
                    // Already (re)inserted above as the new session's
                    // map entry; taking it through the retry path would
                    // clobber that entry.
                    continue;
                }
                let displaced_stream_id = displaced_session.stream_id.clone();
                if let Err(reinsert_error) = self.reinsert_for_retry_unlocked(displaced_session) {
                    warn!(
                        stream_id = %displaced_stream_id,
                        error = %reinsert_error,
                        "store_session: snapshot write failed and re-inserting a \
                         displaced session for retry also failed; its durable rows \
                         are stranded until restart"
                    );
                }
            }
            return Err(error);
        }

        debug!(stream_id = %stream_id, count = count, "Stored detached SM session");
        Ok(displaced)
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
        let removed = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let removed = match sessions.remove(stream_id) {
                Some(session) => {
                    if session.is_expired() {
                        debug!(stream_id = %stream_id, "SM session found but expired");
                        None
                    } else {
                        debug!(stream_id = %stream_id, "Retrieved and removed SM session");
                        Some(session)
                    }
                }
                None => {
                    debug!(stream_id = %stream_id, "SM session not found");
                    None
                }
            };
            self.claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id);
            removed
        };
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
            if let Err(error) = self.persist_delete_session(stream_id).await {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "expired SM session: durable delete failed in cleanup; \
                     restart-time expiry filter will drop the orphan"
                );
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
        self.record_recent_tombstone(target)?;
        // Phase 1 (issue #1145 lock-scope fix): snapshot every queue
        // under READ locks only. XML parsing of every entry used to
        // run under the sessions write lock, stalling all detach /
        // resume traffic for the duration of a full-registry scan.
        let mut snapshots: Vec<(String, Vec<(u32, String)>)> = Vec::new();
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
                        .map(|entry| (entry.sequence, entry.stanza_xml.clone()))
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
                        .map(|entry| (entry.sequence, entry.stanza_xml.clone()))
                        .collect(),
                ));
            }
        }

        // Phase 2: parse and match with NO registry lock held. A
        // queue can change between snapshot and removal; removing by
        // exact (stream_id, sequence) pairs below is safe regardless.
        let mut matched: Vec<(String, Vec<u32>)> = Vec::new();
        for (stream_id, entries) in snapshots {
            let sequences = matching_tombstone_sequences(&entries, target);
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
                        session
                            .unacked_stanzas
                            .retain(|entry| !sequences.contains(&entry.sequence));
                        Some(before - session.unacked_stanzas.len())
                    }
                    None => None,
                }
            };
            let removed_here = match removed_here {
                Some(count) => count,
                None => {
                    let mut claimed = self
                        .claimed_sessions
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    match claimed.get_mut(&stream_id) {
                        Some(session) => {
                            let before = session.unacked_stanzas.len();
                            session
                                .unacked_stanzas
                                .retain(|entry| !sequences.contains(&entry.sequence));
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
                // reinsert_for_retry so the off-map check and the
                // durable delete are atomic with respect to a stream
                // re-entering the maps.
                let stream_lock = self.stream_lock(&stream_id)?;
                let _stream_guard = stream_lock.lock().await;
                let in_map = self
                    .sessions
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                    .contains_key(&stream_id)
                    || self
                        .claimed_sessions
                        .read()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                        .contains_key(&stream_id);
                if in_map {
                    // Already covered by the in-map phases above.
                    continue;
                }
                let sequences: Vec<u32> = unacked
                    .iter()
                    .filter(|entry| target.matches_message_element(&entry.stanza.to_element()))
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
                    Ok(deleted) => removed_total += deleted as usize,
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
