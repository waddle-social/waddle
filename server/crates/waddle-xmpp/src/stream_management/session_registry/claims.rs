use jid::FullJid;
use tracing::debug;

use super::core::InMemorySmSessionRegistry;
use super::{DetachedSession, SmClaimCompletion, SmRegistryError};

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

    /// Atomically claim a resumable session for a single resume attempt.
    ///
    /// Claimed sessions stay writable by detached fanout so stanzas routed
    /// during the claim-to-registration handoff can be merged into the final
    /// replay batch before the claim is completed.
    pub async fn claim_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let Some(session) = sessions.remove(stream_id) else {
            return Ok(None);
        };
        if session.is_expired() {
            return Ok(None);
        }
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if claimed.contains_key(stream_id) {
            sessions.insert(stream_id.to_string(), session);
            return Ok(None);
        }
        claimed.insert(stream_id.to_string(), session.clone());
        Ok(Some(session))
    }

    /// Release a previously claimed session without consuming it.
    pub async fn release_claim(&self, stream_id: &str) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
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
        Ok(())
    }

    /// Complete a previously claimed session, returning the claimed copy with
    /// any stanzas recorded during the handoff and removing detached replay
    /// eligibility from the registry.
    pub async fn complete_claim(
        &self,
        stream_id: &str,
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
            claimed.contains_key(stream_id)
        };
        if !exists {
            return Ok(None);
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
        Ok(outcome)
    }

    /// Remove a stored detached session only if it has not been claimed by a
    /// resume attempt.
    pub async fn remove_stored_session_if_unclaimed(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        // Persist-first ordering: peek + abort if claimed, durably
        // erase, then remove from in-memory. Same rationale as
        // complete_claim — failure to durably erase aborts the
        // operation so a transient storage error doesn't leave an
        // orphan that restart resurrects.
        let exists_unclaimed = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.contains_key(stream_id) && !claimed.contains_key(stream_id)
        };
        if !exists_unclaimed {
            return Ok(None);
        }
        self.persist_delete_session(stream_id).await?;
        let removed = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        Ok(removed)
    }

    /// Invalidate detached sessions for a FullJID after a fresh bind has
    /// replaced that stream identity.
    pub async fn invalidate_sessions_for_jid(
        &self,
        jid: &FullJid,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        // Persist-first: enumerate matching stream-ids under a brief
        // lock, durably erase each, then remove from in-memory. If
        // any durable erase fails, abort before in-memory mutation
        // so a transient storage error doesn't leave orphans.
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
            self.persist_delete_session(stream_id).await?;
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
            if let Some(session) = claimed.remove(stream_id) {
                removed.push(session);
            }
        }
        Ok(removed)
    }
}
