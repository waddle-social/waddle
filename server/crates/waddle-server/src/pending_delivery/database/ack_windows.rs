//! ACK deletion proofs outlive a connection and gate every claim release.
//! A database outage cannot persist new evidence, so retain it in the shared
//! storage instance until deletion succeeds. Never clear row sequence ownership
//! while any acknowledged interval for its session remains unsettled.
use super::{DatabasePendingDeliveryStorage, PendingStorageError, SmClaimFence, SmSessionId};
use crate::db::Transaction;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, PartialEq, Eq)]
pub(super) enum AckWindowAuthority {
    Fenced(SmClaimFence),
    NonResumable,
    Portable,
}

#[derive(Clone, PartialEq, Eq)]
struct AckWindow {
    from_exclusive: u32,
    to_inclusive: u32,
    authority: AckWindowAuthority,
}

#[derive(Default)]
pub(super) struct SessionAckWindows {
    sessions: dashmap::DashMap<SmSessionId, Arc<PendingAckWindows>>,
}

#[derive(Default)]
pub(super) struct PendingAckWindows {
    windows: Mutex<AckWindowQueue>,
    pub(super) operation: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct AckWindowQueue {
    pending: VecDeque<AckWindow>,
    deferred: VecDeque<AckWindow>,
    registration_closed: bool,
}

impl SessionAckWindows {
    pub(super) fn for_session(&self, session: &SmSessionId) -> Arc<PendingAckWindows> {
        self.sessions.entry(session.clone()).or_default().clone()
    }

    pub(super) fn sweep(&self) -> usize {
        let mut removed = 0;
        self.sessions.retain(|_, state| {
            let idle = Arc::strong_count(state) == 1
                && state
                    .windows
                    .lock()
                    .is_ok_and(|windows| windows.pending.is_empty() && windows.deferred.is_empty());
            removed += usize::from(idle);
            !idle
        });
        removed
    }
}

impl PendingAckWindows {
    fn windows(&self) -> Result<std::sync::MutexGuard<'_, AckWindowQueue>, PendingStorageError> {
        self.windows
            .lock()
            .map_err(|_| PendingStorageError::Other("ACK deletion queue poisoned".into()))
    }

    pub(super) fn is_empty(&self) -> Result<bool, PendingStorageError> {
        let windows = self.windows()?;
        Ok(windows.pending.is_empty() && windows.deferred.is_empty())
    }

    pub(super) fn has_only_nonresumable_proofs(&self) -> Result<bool, PendingStorageError> {
        let windows = self.windows()?;
        Ok(windows
            .pending
            .iter()
            .chain(&windows.deferred)
            .all(|window| matches!(window.authority, AckWindowAuthority::NonResumable)))
    }

    pub(super) fn has_only_portable_proofs(&self) -> Result<bool, PendingStorageError> {
        let windows = self.windows()?;
        Ok(windows
            .pending
            .iter()
            .chain(&windows.deferred)
            .all(|window| matches!(window.authority, AckWindowAuthority::Portable)))
    }

    pub(super) fn retain(
        &self,
        from_exclusive: u32,
        to_inclusive: u32,
        authority: AckWindowAuthority,
    ) -> Result<(), PendingStorageError> {
        if from_exclusive != to_inclusive {
            let window = AckWindow {
                from_exclusive,
                to_inclusive,
                authority,
            };
            let mut windows = self.windows()?;
            let pending = if windows.registration_closed {
                &mut windows.deferred
            } else {
                &mut windows.pending
            };
            if !pending.contains(&window) {
                pending.push_back(window);
            }
        }
        Ok(())
    }

    /// Close ACK registration for the final part of a release transaction and
    /// snapshot proofs for one exact fence before this linearization point.
    /// New registrations remain cancellation-safe in `deferred` and are
    /// merged back when the guard commits or drops.
    pub(super) fn begin_release(
        &self,
        session: &SmSessionId,
    ) -> Result<AckWindowRelease<'_>, PendingStorageError> {
        self.begin_release_for_fence(session, None)
    }

    pub(super) fn begin_release_for_fence(
        &self,
        session: &SmSessionId,
        expected_fence: Option<&SmClaimFence>,
    ) -> Result<AckWindowRelease<'_>, PendingStorageError> {
        let mut windows = self.windows()?;
        if windows.registration_closed {
            return Err(PendingStorageError::Other(
                "ACK release gate is already closed".to_owned(),
            ));
        }
        if expected_fence.is_none()
            && windows
                .pending
                .iter()
                .chain(&windows.deferred)
                .any(|window| matches!(window.authority, AckWindowAuthority::Fenced(_)))
        {
            return Err(PendingStorageError::NotOwner {
                entity: waddle_xmpp::ownership::Entity::new(
                    waddle_xmpp::ownership::EntityType::SmSession,
                    session.as_str().to_string(),
                ),
            });
        }
        windows.registration_closed = true;
        let settled: Vec<AckWindow> = windows
            .pending
            .iter()
            .filter(|window| match (&window.authority, expected_fence) {
                (AckWindowAuthority::Fenced(fence), Some(expected)) => fence == expected,
                (AckWindowAuthority::NonResumable | AckWindowAuthority::Portable, None) => true,
                _ => false,
            })
            .cloned()
            .collect();
        let stale = if expected_fence.is_some() {
            windows
                .pending
                .iter()
                .filter(|window| {
                    !matches!(
                        (&window.authority, expected_fence),
                        (AckWindowAuthority::Fenced(fence), Some(expected)) if fence == expected
                    )
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let pending = settled
            .iter()
            .map(|window| (window.from_exclusive, window.to_inclusive))
            .collect();
        Ok(AckWindowRelease {
            state: self,
            pending,
            settled,
            stale,
            expected_fence: expected_fence.cloned(),
            committed: false,
        })
    }

    pub(super) fn discard_fence(
        &self,
        expected_fence: &SmClaimFence,
    ) -> Result<(), PendingStorageError> {
        let mut windows = self.windows()?;
        windows
            .pending
            .retain(|window| {
                !matches!(&window.authority, AckWindowAuthority::Fenced(fence) if fence == expected_fence)
            });
        windows
            .deferred
            .retain(|window| {
                !matches!(&window.authority, AckWindowAuthority::Fenced(fence) if fence == expected_fence)
            });
        Ok(())
    }

    fn finish_release(
        &self,
        settled: &[AckWindow],
        stale: &[AckWindow],
        expected_fence: Option<&SmClaimFence>,
        committed: bool,
    ) -> Result<(), PendingStorageError> {
        let mut windows = self.windows()?;
        let mut changed = false;
        for proof in settled.iter().chain(stale) {
            let Some(index) = windows.pending.iter().position(|window| window == proof) else {
                changed = true;
                continue;
            };
            let _ = windows.pending.remove(index);
        }
        let deferred = std::mem::take(&mut windows.deferred);
        for window in deferred {
            if committed
                && expected_fence.is_some()
                && !matches!(
                    (&window.authority, expected_fence),
                    (AckWindowAuthority::Fenced(fence), Some(expected)) if fence == expected
                )
            {
                continue;
            }
            if !windows.pending.contains(&window) {
                windows.pending.push_back(window);
            }
        }
        windows.registration_closed = false;
        if changed {
            Err(PendingStorageError::Other(
                "ACK deletion queue changed while settling".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

pub(super) struct AckWindowRelease<'a> {
    state: &'a PendingAckWindows,
    pending: Vec<(u32, u32)>,
    settled: Vec<AckWindow>,
    stale: Vec<AckWindow>,
    expected_fence: Option<SmClaimFence>,
    committed: bool,
}

impl AckWindowRelease<'_> {
    pub(super) fn pending(&self) -> &[(u32, u32)] {
        &self.pending
    }

    /// Forget the snapshot only after the transaction that deleted its rows
    /// committed. Drop without this call preserves all ACK evidence.
    pub(super) fn committed(mut self) -> Result<(), PendingStorageError> {
        self.state.finish_release(
            &self.settled,
            &self.stale,
            self.expected_fence.as_ref(),
            true,
        )?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for AckWindowRelease<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.state.finish_release(&[], &[], None, false);
        }
    }
}

/// Apply all currently queued ACK deletions in the caller's transaction.
/// The caller holds `state.operation` through the transaction and removes the
/// returned proof set only after commit; an unknown commit therefore
/// retries idempotent deletes instead of losing its proof.
pub(super) async fn settle_ack_windows_in_transaction(
    tx: &mut Transaction<'_>,
    session: &SmSessionId,
    windows: &[(u32, u32)],
) -> Result<u64, PendingStorageError> {
    let mut removed = 0;
    for (from_exclusive, to_inclusive) in windows {
        let sql = if from_exclusive <= to_inclusive {
            "DELETE FROM pending_delivery WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL AND outbound_sequence > ? AND outbound_sequence <= ?"
        } else {
            "DELETE FROM pending_delivery WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL AND (outbound_sequence > ? OR outbound_sequence <= ?)"
        };
        removed += tx
            .execute(
                sql,
                crate::db_params![
                    session.as_str().to_string(),
                    i64::from(*from_exclusive),
                    i64::from(*to_inclusive)
                ],
            )
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    }
    Ok(removed)
}

impl DatabasePendingDeliveryStorage {
    /// Caller holds this session's operation lock through deletion and any
    /// subsequent release. Short queue locks never span a database await.
    pub(super) async fn settle_ack_windows(
        &self,
        session: &SmSessionId,
        state: &PendingAckWindows,
    ) -> Result<u64, PendingStorageError> {
        let proofs_authorized = if self.fencing.is_some() {
            state.has_only_nonresumable_proofs()?
        } else {
            state.has_only_portable_proofs()?
        };
        if !proofs_authorized {
            return Err(PendingStorageError::NotOwner {
                entity: waddle_xmpp::ownership::Entity::new(
                    waddle_xmpp::ownership::EntityType::SmSession,
                    session.as_str().to_string(),
                ),
            });
        }
        let release = state.begin_release(session)?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        let removed =
            settle_ack_windows_in_transaction(&mut tx, session, release.pending()).await?;
        tx.commit()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        release.committed()?;
        Ok(removed)
    }
}
