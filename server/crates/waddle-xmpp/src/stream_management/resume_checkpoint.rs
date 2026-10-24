//! Small owner-maintained checkpoint for validating a live resume before I/O interruption.

use std::sync::{Arc, Mutex};

use super::sequence::sequence_gt;

/// A live stream cannot honor this resume request without losing replay state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmResumeRejection {
    NotResumable,
    ReplayGap {
        inbound_count: u32,
        replay_gap_through: Option<u32>,
    },
    HandledTooHigh {
        outbound_count: u32,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SmResumeCheckpoint {
    pub resumable: bool,
    pub inbound_count: u32,
    pub outbound_count: u32,
    pub last_acked: u32,
    pub replay_gap_through: Option<u32>,
}

/// Shared checkpoint. Only the owning connection task may validate it and
/// authorize a destructive takeover; requesters only enqueue their request.
/// Poisoning terminates the process: a partial checkpoint must never authorize
/// takeover, and unwinding a connection task would leave sibling tasks live.
#[derive(Debug, Clone, Default)]
pub struct SmResumeCheckpointHandle(Arc<Mutex<SmResumeCheckpoint>>);

impl SmResumeCheckpointHandle {
    pub(super) fn update(&self, checkpoint: SmResumeCheckpoint) {
        *self
            .0
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = checkpoint;
    }

    pub(crate) fn validate(&self, h: u32) -> Result<(), SmResumeRejection> {
        let checkpoint = *self
            .0
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if !checkpoint.resumable {
            return Err(SmResumeRejection::NotResumable);
        }
        // Match detached-session validation: reject replay lower bounds before
        // classifying a handled count outside the exact wrapping send window.
        if sequence_gt(checkpoint.last_acked, h)
            || checkpoint
                .replay_gap_through
                .is_some_and(|gap| sequence_gt(gap, h))
        {
            return Err(SmResumeRejection::ReplayGap {
                inbound_count: checkpoint.inbound_count,
                replay_gap_through: checkpoint.replay_gap_through,
            });
        }
        if h.wrapping_sub(checkpoint.last_acked)
            > checkpoint
                .outbound_count
                .wrapping_sub(checkpoint.last_acked)
        {
            return Err(SmResumeRejection::HandledTooHigh {
                outbound_count: checkpoint.outbound_count,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_checkpoint_uses_detached_session_wrapping_and_gap_rules() {
        let handle = SmResumeCheckpointHandle::default();
        assert_eq!(handle.validate(0), Err(SmResumeRejection::NotResumable));
        handle.update(SmResumeCheckpoint {
            resumable: true,
            inbound_count: 7,
            outbound_count: 2,
            last_acked: u32::MAX - 1,
            replay_gap_through: None,
        });
        assert_eq!(handle.validate(u32::MAX), Ok(()));
        assert_eq!(handle.validate(1), Ok(()));
        assert_eq!(
            handle.validate(3),
            Err(SmResumeRejection::HandledTooHigh { outbound_count: 2 })
        );
        assert_eq!(
            handle.validate(u32::MAX - 2),
            Err(SmResumeRejection::ReplayGap {
                inbound_count: 7,
                replay_gap_through: None
            })
        );
        handle.update(SmResumeCheckpoint {
            resumable: true,
            inbound_count: 7,
            outbound_count: 2,
            last_acked: u32::MAX - 1,
            replay_gap_through: Some(0),
        });
        assert_eq!(
            handle.validate(u32::MAX),
            Err(SmResumeRejection::ReplayGap {
                inbound_count: 7,
                replay_gap_through: Some(0)
            })
        );
        assert_eq!(handle.validate(0), Ok(()));
        handle.update(SmResumeCheckpoint {
            resumable: true,
            outbound_count: 2,
            last_acked: 2,
            ..SmResumeCheckpoint::default()
        });
        assert_eq!(
            handle.validate(2_u32.wrapping_add(1 << 31)),
            Err(SmResumeRejection::HandledTooHigh { outbound_count: 2 })
        );
    }
}
