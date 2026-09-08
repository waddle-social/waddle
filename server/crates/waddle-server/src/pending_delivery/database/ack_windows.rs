//! ACK deletion proofs outlive a connection and gate every claim release.
//! A database outage cannot persist new evidence, so retain it in the shared
//! storage instance until deletion succeeds. Never clear row sequence ownership
//! while any acknowledged interval for its session remains unsettled.
use super::*;
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
    windows: Mutex<VecDeque<AckWindow>>,
    pub(super) operation: tokio::sync::Mutex<()>,
}

impl SessionAckWindows {
    pub(super) fn for_session(&self, session: &SmSessionId) -> Arc<PendingAckWindows> {
        self.sessions.entry(session.clone()).or_default().clone()
    }

    pub(super) fn sweep(&self) -> usize {
        let mut removed = 0;
        self.sessions.retain(|_, state| {
            let idle = Arc::strong_count(state) == 1
                && state.windows.lock().is_ok_and(|windows| windows.is_empty());
            removed += usize::from(idle);
            !idle
        });
        removed
    }
}

impl PendingAckWindows {
    fn windows(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, VecDeque<AckWindow>>, PendingStorageError> {
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
            if !windows.contains(&window) {
                windows.push_back(window);
            }
        }
        Ok(())
    }
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
            let Some(window) = state.windows()?.front().copied() else {
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
            state.windows()?.pop_front();
        }
    }
}
