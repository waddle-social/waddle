use async_trait::async_trait;
use tracing::debug;

use super::core::InMemorySmSessionRegistry;
use super::tombstone::scrub_session_unacked;
use super::{DetachedSession, SmRegistryError, SmSessionRegistry};

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(&self, session: DetachedSession) -> Result<(), SmRegistryError> {
        let stream_id = session.stream_id.clone();
        let jid = session.jid.clone();
        // Scope the RwLock guards in a block so they're definitively
        // dropped before any await point. RwLockWriteGuard is not
        // Send, and explicit `drop()` doesn't satisfy the async
        // future's lifetime analysis. Capture eviction victims
        // (jid-collision retain + max_sessions oldest) so we can
        // mirror their durable rows after releasing the lock.
        let mut evicted_stream_ids: Vec<String> = Vec::new();
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
                    evicted_stream_ids.push(id.clone());
                }
            }
            for (id, existing) in claimed.iter() {
                if id != &stream_id && existing.jid == jid {
                    evicted_stream_ids.push(id.clone());
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
                    sessions.remove(&oldest_key);
                    debug!(stream_id = %oldest_key, "Evicted oldest SM session to make room");
                    evicted_stream_ids.push(oldest_key);
                }
            }

            sessions.insert(stream_id.clone(), session.clone());
            sessions.len()
        };
        // Mirror in-memory evictions to durable storage so a restart
        // doesn't resurrect sessions that were displaced by a fresh
        // bind for the same JID or by max_sessions overflow. (Copilot
        // review on PR #344: durable rows for evicted streams must
        // not be silently rehydrated.)
        for evicted in &evicted_stream_ids {
            // Best-effort: failure to delete an evictee row means
            // the next restart MAY resurrect it via
            // restore_from_persistence (until its resume window
            // expires and the restore-time expired-filter drops it).
            // Bubbling the error here would fail the whole detach
            // because an unrelated evictee row couldn't be cleaned;
            // log loudly instead so operators can spot storage
            // health issues.
            if let Err(error) = self.persist_delete_session(evicted).await {
                debug!(
                    stream_id = %evicted,
                    error = %error,
                    "evicted SM session: durable delete failed; row will be \
                     filtered by restore-time expiry check"
                );
            }
        }

        self.persist_detached_session_snapshot(&session).await?;

        debug!(stream_id = %stream_id, count = count, "Stored detached SM session");
        Ok(())
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
        target_id: &str,
        archive_jid: &str,
    ) -> Result<usize, SmRegistryError> {
        let mut removed_total = 0usize;
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            removed_total += scrub_session_unacked(session, target_id, archive_jid);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            removed_total += scrub_session_unacked(session, target_id, archive_jid);
        }
        Ok(removed_total)
    }
}
