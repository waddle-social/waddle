//! Detach, compensation, owner loss, expiry and terminal custody.

use chrono::{DateTime, Utc};
use jid::FullJid;
use thiserror::Error;
use waddle_xmpp::ownership::{SessionGeneration, SmSessionKey};
use waddle_xmpp::stream_management::durable::{
    transition, DurableSmEvent, DurableSmState, OwnerFence, TransitionError,
};
use waddle_xmpp::stream_management::persistence::{PersistedSession, SmClaimFence};

use super::custody::{load_frames, OwnerError, OwnerRejection, RetainedFrame};
use super::error::DurableSmError;
use super::record::{self, DurableSessionRecord};
use super::resume::ResumedCommit;
use super::store::DurableSmStore;
use crate::db::Transaction;
use crate::sm_persistence::durable_rows;

/// The owner's projection of its resource at the moment its transport ends.
#[derive(Debug, Clone)]
pub struct DetachSnapshot {
    /// Presence, carbons, roster and blocklist interest and the occupancy
    /// session. Its stream id, JID, counters and detach time are replaced by
    /// the authority's own values when it is stored.
    pub resource: PersistedSession,
    /// The owner's handled count of client stanzas.
    pub inbound_handled: u64,
}

/// A known outcome of an operation that needs no owner fence.
#[derive(Debug, Error)]
pub enum LifecycleRejection {
    #[error("no such durable SM session")]
    UnknownSession,
    #[error("the session is at generation {stored:?}, not {presented:?}")]
    GenerationMismatch {
        stored: SessionGeneration,
        presented: SessionGeneration,
    },
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error("the resume window has not elapsed")]
    NotDue,
    #[error("this node does not hold the session claim it named")]
    ClaimFenceMissing,
    #[error(transparent)]
    Store(#[from] DurableSmError),
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error(transparent)]
    Rejected(#[from] LifecycleRejection),
    /// The commit outcome is unknown. Each lifecycle operation may be
    /// repeated: a transition that already happened is then rejected by the
    /// state machine.
    #[error("the commit outcome is unknown")]
    Indeterminate(#[source] DurableSmError),
}

/// A terminal session and how many frames it still retains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSession {
    pub session: SmSessionKey,
    pub full_jid: FullJid,
    pub state: DurableSmState,
    pub retained_frames: u64,
}

impl DurableSmStore {
    /// Move the owner's session to `Detached` and store its resource
    /// snapshot. The first detach time stands: a later detach of a session
    /// that was resumed but never completed does not renew its window.
    pub async fn detach(
        &self,
        fence: &OwnerFence,
        claim: Option<&SmClaimFence>,
        snapshot: DetachSnapshot,
        now: DateTime<Utc>,
    ) -> Result<(), OwnerError> {
        let (mut tx, mut stored) = self.lock_owned(fence, claim).await?;
        stored.state = transition(stored.state, DurableSmEvent::Detach)
            .map_err(|_| OwnerRejection::NotOwned(stored.state))?;
        stored.inbound_handled = stored.inbound_handled.max(snapshot.inbound_handled);
        let detached_at = *stored.detached_at.get_or_insert(now);
        let resource = PersistedSession {
            stream_id: record::stream_id(stored.session),
            jid: stored.full_jid.clone(),
            inbound_count: stored.inbound_handled as u32,
            outbound_count: stored.window.appended().wire(),
            last_acked: stored.window.acked().wire(),
            replay_gap_through: None,
            max_resume_time: Some(u32::try_from(stored.max_resume.as_secs()).unwrap_or(u32::MAX)),
            detached_at,
            max_resume_duration: stored.max_resume,
            ..snapshot.resource
        };
        let write = async {
            durable_rows::store_resource_snapshot(&mut tx, &stored.principal, &resource).await?;
            record::update(&mut tx, &stored, now).await
        };
        write.await.map_err(OwnerRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| OwnerError::Indeterminate(error.into()))
    }

    /// Return a resumed session that could not publish its endpoint to
    /// `Detached`. The stored resource snapshot and first detach time are
    /// kept, so the session stays resumable for the rest of its window. A
    /// compensation that names a superseded generation fails its fence and
    /// changes nothing.
    pub async fn compensate_resume(
        &self,
        resumed: &ResumedCommit,
        claim: Option<&SmClaimFence>,
        now: DateTime<Utc>,
    ) -> Result<(), OwnerError> {
        let (mut tx, mut stored) = self.lock_owned(resumed.fence(), claim).await?;
        if stored.state != DurableSmState::TransferHydrating {
            return Err(OwnerRejection::NotOwned(stored.state).into());
        }
        stored.state = DurableSmState::Detached;
        stored.detached_at.get_or_insert(now);
        record::update(&mut tx, &stored, now)
            .await
            .map_err(OwnerRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| OwnerError::Indeterminate(error.into()))
    }

    /// End the owner's session deliberately. Retained frames stay in custody
    /// until [`Self::release_terminal`].
    pub async fn close(
        &self,
        fence: &OwnerFence,
        claim: Option<&SmClaimFence>,
        now: DateTime<Utc>,
    ) -> Result<(), OwnerError> {
        let (mut tx, mut stored) = self.lock_owned(fence, claim).await?;
        stored.state = transition(stored.state, DurableSmEvent::Close)
            .map_err(|_| OwnerRejection::NotOwned(stored.state))?;
        record::update(&mut tx, &stored, now)
            .await
            .map_err(OwnerRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| OwnerError::Indeterminate(error.into()))
    }

    /// Record that the owner of `generation` is gone, moving the session to
    /// `Detached`. On a clustered store the caller proves it by holding the
    /// session claim itself: the previous owner's claim row no longer exists.
    pub async fn record_owner_lost(
        &self,
        session: SmSessionKey,
        generation: SessionGeneration,
        successor: &SmClaimFence,
        now: DateTime<Utc>,
    ) -> Result<(), LifecycleError> {
        if !self.clustered {
            return Err(LifecycleRejection::Store(DurableSmError::Unsupported {
                operation: "owner-loss by claim on a store without claims",
            })
            .into());
        }
        let mut tx = self.begin_write().await.map_err(LifecycleRejection::from)?;
        if !self
            .claim_is_held(&mut tx, session, successor)
            .await
            .map_err(LifecycleRejection::from)?
        {
            return Err(LifecycleRejection::ClaimFenceMissing.into());
        }
        let mut stored = lock_generation(&mut tx, session, generation).await?;
        stored.state = transition(stored.state, DurableSmEvent::OwnerLost)
            .map_err(LifecycleRejection::from)?;
        stored.detached_at.get_or_insert(now);
        finish(tx, &stored, now).await
    }

    /// After a single-node process restart no binding of the previous
    /// process exists, so every owned session is detached. Returns how many
    /// sessions moved. A clustered store refuses: other nodes' owners are
    /// alive, and owner loss is proven per session by its claim.
    pub async fn detach_owners_of_previous_process(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, LifecycleError> {
        if self.clustered {
            return Err(LifecycleRejection::Store(DurableSmError::Unsupported {
                operation: "process-wide owner loss on a clustered store",
            })
            .into());
        }
        let mut tx = self.begin_write().await.map_err(LifecycleRejection::from)?;
        let moved = record::detach_all_owned(&mut tx, now)
            .await
            .map_err(LifecycleRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| LifecycleError::Indeterminate(error.into()))?;
        Ok(moved)
    }

    /// Detached sessions whose resume window has elapsed at `now`.
    pub async fn due_for_expiry(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<SmSessionKey>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let due = record::due_for_expiry(&mut tx, now, limit).await?;
        tx.rollback().await?;
        Ok(due)
    }

    /// Expire one detached session whose window has elapsed. The decision is
    /// made under the row lock, so a resume that committed first wins and
    /// the expiry is rejected.
    pub async fn expire(
        &self,
        session: SmSessionKey,
        claim: Option<&SmClaimFence>,
        now: DateTime<Utc>,
    ) -> Result<TerminalSession, LifecycleError> {
        let (tx, mut stored) = self.lock_unowned(session, claim).await?;
        let due = stored.detached_at.is_some_and(|detached_at| {
            now.signed_duration_since(detached_at)
                .to_std()
                .is_ok_and(|elapsed| elapsed >= stored.max_resume)
        });
        if stored.state == DurableSmState::Detached && !due {
            return Err(LifecycleRejection::NotDue.into());
        }
        stored.state =
            transition(stored.state, DurableSmEvent::Expire).map_err(LifecycleRejection::from)?;
        let terminal = terminal_session(&stored);
        finish(tx, &stored, now).await?;
        Ok(terminal)
    }

    /// End every session of `resource` that is not already terminal, because
    /// the resource was bound again. A clustered store refuses: the binder
    /// holds the account's claim, not each session's, and no fence joins the
    /// two yet.
    pub async fn replace_resource_sessions(
        &self,
        resource: &FullJid,
        now: DateTime<Utc>,
    ) -> Result<Vec<TerminalSession>, LifecycleError> {
        if self.clustered {
            return Err(LifecycleRejection::Store(DurableSmError::Unsupported {
                operation: "resource replacement on a clustered store",
            })
            .into());
        }
        let mut tx = self.begin_write().await.map_err(LifecycleRejection::from)?;
        let sessions = record::load_resource_for_update(&mut tx, resource)
            .await
            .map_err(LifecycleRejection::from)?;
        let mut replaced = Vec::new();
        for mut stored in sessions {
            let Ok(state) = transition(stored.state, DurableSmEvent::Replace) else {
                continue;
            };
            stored.state = state;
            record::update(&mut tx, &stored, now)
                .await
                .map_err(LifecycleRejection::from)?;
            replaced.push(terminal_session(&stored));
        }
        tx.commit()
            .await
            .map_err(|error| LifecycleError::Indeterminate(error.into()))?;
        Ok(replaced)
    }

    /// The frames a terminal session still holds in custody, in ordinal
    /// order, for the consumer that transfers them to offline delivery.
    pub async fn terminal_frames(
        &self,
        session: SmSessionKey,
    ) -> Result<Vec<RetainedFrame>, LifecycleError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| LifecycleRejection::Store(error.into()))?;
        let stored = record::load(&mut tx, session)
            .await
            .map_err(LifecycleRejection::from)?
            .ok_or(LifecycleRejection::UnknownSession)?;
        if !stored.state.is_terminal() {
            return Err(
                LifecycleRejection::Transition(TransitionError::NotApplicable {
                    state: stored.state,
                    event: DurableSmEvent::Expire,
                })
                .into(),
            );
        }
        let frames = load_frames(&mut tx, session, stored.window.replay())
            .await
            .map_err(LifecycleRejection::from)?;
        tx.rollback()
            .await
            .map_err(|error| LifecycleRejection::Store(error.into()))?;
        Ok(frames)
    }

    /// Terminal sessions that still hold custody for the consumer to
    /// transfer and release: retained frames, a resource snapshot or
    /// operation outcomes.
    pub async fn terminal_with_custody(
        &self,
        limit: u32,
    ) -> Result<Vec<SmSessionKey>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let sessions = record::terminal_with_custody(&mut tx, limit).await?;
        tx.rollback().await?;
        Ok(sessions)
    }

    /// The resource snapshot a terminal session still holds: the state its
    /// last owner stored when it detached. Absent when the session ended
    /// without detaching, or after its custody was released.
    pub async fn terminal_resource(
        &self,
        session: SmSessionKey,
    ) -> Result<Option<PersistedSession>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let resource =
            durable_rows::load_resource_snapshot(&mut tx, &record::stream_id(session)).await?;
        tx.rollback().await?;
        Ok(resource)
    }

    /// The `sm_sessions` and `sm_unacked` stream id of every session this
    /// authority holds. Another reader of those relations uses it to leave
    /// the authority's rows alone.
    pub async fn stream_ids(
        &self,
    ) -> Result<Vec<waddle_xmpp::pending_delivery::SmSessionId>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let ids = record::all_stream_ids(&mut tx).await?;
        tx.rollback().await?;
        Ok(ids)
    }

    /// Release a terminal session's custody after its frames were
    /// transferred: delete the frames and the resource snapshot and close
    /// the window. The session row remains as a tombstone.
    pub async fn release_terminal(
        &self,
        session: SmSessionKey,
        claim: Option<&SmClaimFence>,
        now: DateTime<Utc>,
    ) -> Result<(), LifecycleError> {
        let (mut tx, mut stored) = self.lock_unowned(session, claim).await?;
        if !stored.state.is_terminal() {
            return Err(
                LifecycleRejection::Transition(TransitionError::NotApplicable {
                    state: stored.state,
                    event: DurableSmEvent::Expire,
                })
                .into(),
            );
        }
        let release = async {
            let stream = record::stream_id(session);
            durable_rows::delete_all_unacked(&mut tx, &stream).await?;
            durable_rows::delete_resource_snapshot(&mut tx, &stream).await?;
            record::delete_operations(&mut tx, session).await
        };
        release.await.map_err(LifecycleRejection::from)?;
        stored.window = stored.window.expose_all();
        let (window, _) = stored
            .window
            .acknowledge(
                stored.window.appended().wire(),
                waddle_xmpp::stream_management::durable::AckKind::Live,
            )
            .map_err(|_| {
                LifecycleRejection::Store(DurableSmError::CorruptRecord {
                    field: "outbound window",
                })
            })?;
        stored.window = window;
        stored.unacked_bytes = 0;
        finish(tx, &stored, now).await
    }

    /// The `sm_sessions` and `sm_unacked` stream id of every session that
    /// can still be resumed or is owned. A reader that releases work claimed
    /// under a session id uses it to leave a detached session's claims in
    /// place.
    pub async fn live_stream_ids(
        &self,
    ) -> Result<Vec<waddle_xmpp::pending_delivery::SmSessionId>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let ids = record::live_stream_ids(&mut tx).await?;
        tx.rollback().await?;
        Ok(ids)
    }

    /// Released terminal tombstones last updated before `before`, oldest
    /// first: the rows a purge deletes.
    pub async fn purgeable_terminal(
        &self,
        before: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<SmSessionKey>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let sessions = record::released_terminal_before(&mut tx, before, limit).await?;
        tx.rollback().await?;
        Ok(sessions)
    }

    /// Delete one released terminal tombstone last updated before `before`.
    /// Returns whether a row was deleted.
    ///
    /// An account resource row may name the session, by a foreign key
    /// checked at commit. The caller releases every such row first; a
    /// delete that still fails means a row names the session, and is
    /// repeated after that row is released.
    pub async fn purge_terminal_session(
        &self,
        session: SmSessionKey,
        before: DateTime<Utc>,
    ) -> Result<bool, LifecycleError> {
        let mut tx = self.begin_write().await.map_err(LifecycleRejection::from)?;
        let purged = record::purge_released_terminal_session(&mut tx, session, before)
            .await
            .map_err(LifecycleRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| LifecycleError::Indeterminate(error.into()))?;
        Ok(purged > 0)
    }

    /// Delete released terminal tombstones last updated before `before`.
    /// Returns how many rows were deleted.
    pub async fn purge_terminal(&self, before: DateTime<Utc>) -> Result<u64, LifecycleError> {
        let mut tx = self.begin_write().await.map_err(LifecycleRejection::from)?;
        let purged = record::purge_released_terminal(&mut tx, before)
            .await
            .map_err(LifecycleRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| LifecycleError::Indeterminate(error.into()))?;
        Ok(purged)
    }

    /// Begin a write transaction, assert the claim and lock a session that
    /// the caller does not own.
    async fn lock_unowned(
        &self,
        session: SmSessionKey,
        claim: Option<&SmClaimFence>,
    ) -> Result<(Transaction<'_>, DurableSessionRecord), LifecycleRejection> {
        let claim = self.write_claim(claim)?;
        let mut tx = self.begin_write().await?;
        if let Some(claim) = claim {
            if !self.claim_is_held(&mut tx, session, claim).await? {
                return Err(LifecycleRejection::ClaimFenceMissing);
            }
        }
        let stored = record::load_for_update(&mut tx, session)
            .await?
            .ok_or(LifecycleRejection::UnknownSession)?;
        Ok((tx, stored))
    }
}

async fn lock_generation(
    tx: &mut Transaction<'_>,
    session: SmSessionKey,
    generation: SessionGeneration,
) -> Result<DurableSessionRecord, LifecycleRejection> {
    let stored = record::load_for_update(tx, session)
        .await?
        .ok_or(LifecycleRejection::UnknownSession)?;
    if stored.generation != generation {
        return Err(LifecycleRejection::GenerationMismatch {
            stored: stored.generation,
            presented: generation,
        });
    }
    Ok(stored)
}

async fn finish(
    mut tx: Transaction<'_>,
    stored: &DurableSessionRecord,
    now: DateTime<Utc>,
) -> Result<(), LifecycleError> {
    record::update(&mut tx, stored, now)
        .await
        .map_err(LifecycleRejection::from)?;
    tx.commit()
        .await
        .map_err(|error| LifecycleError::Indeterminate(error.into()))
}

fn terminal_session(stored: &DurableSessionRecord) -> TerminalSession {
    TerminalSession {
        session: stored.session,
        full_jid: stored.full_jid.clone(),
        state: stored.state,
        retained_frames: stored.window.outstanding(),
    }
}
