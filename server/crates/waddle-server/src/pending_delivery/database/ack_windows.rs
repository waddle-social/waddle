//! ACK deletion proofs outlive a connection and gate every claim release.
//! A database outage cannot persist new evidence, so retain it in the shared
//! storage instance until deletion succeeds. Never clear row sequence ownership
//! while any acknowledged interval for its session remains unsettled.
use super::{DatabasePendingDeliveryStorage, PendingStorageError, SmSessionId};
use crate::db::Transaction;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
struct AckWindow {
    from_exclusive: u32,
    to_inclusive: u32,
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

    pub(super) fn retain(
        &self,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<(), PendingStorageError> {
        if from_exclusive != to_inclusive {
            let window = AckWindow {
                from_exclusive,
                to_inclusive,
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
    /// snapshot every proof that registered before this linearization point.
    /// New registrations remain cancellation-safe in `deferred` and are
    /// merged back when the guard commits or drops.
    pub(super) fn begin_release(&self) -> Result<AckWindowRelease<'_>, PendingStorageError> {
        let mut windows = self.windows()?;
        if windows.registration_closed {
            return Err(PendingStorageError::Other(
                "ACK release gate is already closed".to_owned(),
            ));
        }
        windows.registration_closed = true;
        let pending = windows
            .pending
            .iter()
            .map(|window| (window.from_exclusive, window.to_inclusive))
            .collect();
        Ok(AckWindowRelease {
            state: self,
            pending,
            committed: false,
        })
    }

    fn finish_release(&self, settled: usize) -> Result<(), PendingStorageError> {
        let mut windows = self.windows()?;
        let removed = settled.min(windows.pending.len());
        for _ in 0..removed {
            windows.pending.pop_front();
        }
        let deferred = std::mem::take(&mut windows.deferred);
        for window in deferred {
            if !windows.pending.contains(&window) {
                windows.pending.push_back(window);
            }
        }
        windows.registration_closed = false;
        if removed == settled {
            Ok(())
        } else {
            Err(PendingStorageError::Other(
                "ACK deletion queue changed while settling".to_owned(),
            ))
        }
    }
}

pub(super) struct AckWindowRelease<'a> {
    state: &'a PendingAckWindows,
    pending: Vec<(u32, u32)>,
    committed: bool,
}

impl AckWindowRelease<'_> {
    pub(super) fn pending(&self) -> &[(u32, u32)] {
        &self.pending
    }

    /// Forget the snapshot only after the transaction that deleted its rows
    /// committed. Drop without this call preserves all ACK evidence.
    pub(super) fn committed(mut self) -> Result<(), PendingStorageError> {
        self.state.finish_release(self.pending.len())?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for AckWindowRelease<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.state.finish_release(0);
        }
    }
}

/// Apply all currently queued ACK deletions in the caller's transaction.
/// The caller holds `state.operation` through the transaction and removes the
/// returned prefix only after commit; an unknown commit therefore retries
/// idempotent deletes instead of losing its proof.
pub(super) async fn settle_ack_windows_in_transaction(
    tx: &mut Transaction<'_>,
    session: &SmSessionId,
    windows: &[(u32, u32)],
) -> Result<(), PendingStorageError> {
    for (from_exclusive, to_inclusive) in windows {
        let sql = if from_exclusive <= to_inclusive {
            "DELETE FROM pending_delivery WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL AND outbound_sequence > ? AND outbound_sequence <= ?"
        } else {
            "DELETE FROM pending_delivery WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL AND (outbound_sequence > ? OR outbound_sequence <= ?)"
        };
        tx.execute(
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
    Ok(())
}

impl DatabasePendingDeliveryStorage {
    /// Caller holds this session's operation lock through deletion and any
    /// subsequent release. Short queue locks never span a database await.
    pub(super) async fn settle_ack_windows(
        &self,
        session: &SmSessionId,
        state: &PendingAckWindows,
    ) -> Result<u64, PendingStorageError> {
        let mut removed = 0;
        loop {
            let Some(window) = state.windows()?.pending.front().copied() else {
                return Ok(removed);
            };
            let sql = if window.from_exclusive <= window.to_inclusive {
                "DELETE FROM pending_delivery WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL AND outbound_sequence > ? AND outbound_sequence <= ?"
            } else {
                "DELETE FROM pending_delivery WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL AND (outbound_sequence > ? OR outbound_sequence <= ?)"
            };
            removed += self
                .execute(
                    sql,
                    crate::db_params![
                        session.as_str().to_string(),
                        i64::from(window.from_exclusive),
                        i64::from(window.to_inclusive)
                    ],
                )
                .await?;
            state.windows()?.pending.pop_front();
        }
    }
}
