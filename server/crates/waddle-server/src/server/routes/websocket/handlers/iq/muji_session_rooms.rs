//! Per-replica record of the room each Muji Jingle session belongs to.
//!
//! XEP-0272 puts `<muji room='...'/>` only on the `session-initiate`; every
//! later action, a `session-terminate` included, names the session by the
//! XEP-0166 `sid` alone. In a cluster the replica that receives the
//! initiate relays it to the room owner, so only the owner's Jingle handler
//! learns the `sid` to room mapping, while the client's own replica receives
//! the plain terminate. This record, kept on the replica that accepted the
//! initiate from the client, resolves a plain terminate to its room so it
//! takes the same locality, relay and presence-clear path as a terminate
//! that carries the room itself.

use std::collections::HashMap;
use std::sync::Mutex;

use jid::{BareJid, FullJid};
use waddle_xmpp::registry::ConnectionRegistry;

/// Upper bound on recorded sessions before dead connections are pruned.
const PRUNE_THRESHOLD: usize = 8192;

#[derive(Default)]
pub(crate) struct MujiSessionRooms {
    rooms: Mutex<HashMap<(FullJid, String), BareJid>>,
}

impl MujiSessionRooms {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(FullJid, String), BareJid>> {
        self.rooms
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    /// Record that `sid`, initiated by `jid`, belongs to `room`.
    pub(crate) fn record(
        &self,
        connections: &ConnectionRegistry,
        jid: &FullJid,
        sid: &str,
        room: &BareJid,
    ) {
        let mut rooms = self.lock();
        if rooms.len() >= PRUNE_THRESHOLD {
            rooms.retain(|(owner, _), _| connections.occupancy_session_of(owner).is_some());
        }
        rooms.insert((jid.clone(), sid.to_owned()), room.clone());
    }

    /// The room of the session `(jid, sid)`, when this replica recorded it.
    pub(crate) fn room_of(&self, jid: &FullJid, sid: &str) -> Option<BareJid> {
        self.lock().get(&(jid.clone(), sid.to_owned())).cloned()
    }

    /// Drop the session once its terminate has been carried out.
    pub(crate) fn forget(&self, jid: &FullJid, sid: &str) {
        self.lock().remove(&(jid.clone(), sid.to_owned()));
    }
}
