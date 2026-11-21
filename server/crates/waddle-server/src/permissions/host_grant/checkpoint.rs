//! Points between a decision for a managed room and the effect it guards.
//!
//! A surface that awaits something after it decided (a read of the room
//! actor or the archive, the room's configuration lock) decides again, or
//! checks its decision's ticket, before the effect. These are the points
//! where it waits: a test holds a request at one of them to land a revoke
//! there and to see that the effect does not follow. Without the
//! `test-support` feature every point is a no-op.

use jid::BareJid;

/// A point a request for a managed room reaches after its decision and
/// before the effect that decision guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecisionCheckpoint {
    /// An owner's configuration change was authorized and has not taken the
    /// room's configuration lock yet.
    OwnerConfigAuthorized,
    /// The occupants of a muc#admin role list were read from the room actor
    /// and not yet answered.
    AdminRoleListRead,
    /// An extension's archive query was read from the archive and not yet
    /// answered.
    ExtensionArchiveRead,
    /// The room's own message was archived and not yet delivered.
    SystemMessageArchived,
}

/// Reached by a request for `room` at `point`. A no-op unless a test holds
/// that point of that room.
pub(crate) async fn reached(point: DecisionCheckpoint, room: &BareJid) {
    #[cfg(feature = "test-support")]
    gates::wait(point, room).await;
    #[cfg(not(feature = "test-support"))]
    let _ = (point, room);
}

#[cfg(feature = "test-support")]
pub use gates::{pause_at, CheckpointGate};

#[cfg(feature = "test-support")]
mod gates {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};

    use jid::BareJid;
    use tokio::sync::Notify;

    use super::DecisionCheckpoint;

    type Key = (DecisionCheckpoint, BareJid);

    static GATES: LazyLock<Mutex<HashMap<Key, Arc<CheckpointGate>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// A held point: the next request that reaches it waits there until
    /// the test releases it.
    #[derive(Default)]
    pub struct CheckpointGate {
        reached: Notify,
        released: Notify,
    }

    impl CheckpointGate {
        /// Wait until a request has reached the point.
        pub async fn wait_until_reached(&self) {
            self.reached.notified().await;
        }

        /// Let the request that reached the point go on.
        pub fn release(&self) {
            self.released.notify_one();
        }
    }

    /// Hold the next request for `room` that reaches `point`.
    pub fn pause_at(point: DecisionCheckpoint, room: BareJid) -> Arc<CheckpointGate> {
        let gate = Arc::new(CheckpointGate::default());
        GATES
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .insert((point, room), Arc::clone(&gate));
        gate
    }

    pub(super) async fn wait(point: DecisionCheckpoint, room: &BareJid) {
        let gate = GATES
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .remove(&(point, room.clone()));
        if let Some(gate) = gate {
            gate.reached.notify_one();
            gate.released.notified().await;
        }
    }
}
