//! SfuRoomActor — Kameo actor managing one active call room.

use super::peer::SfuPeer;
use super::{PeerStore, RoomKey};
use jid::FullJid;
use kameo::message::Context;
use kameo::Actor;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Actor that manages participants in one SFU call room.
///
/// Peers are stored in the shared [`PeerStore`] (behind `Arc<RwLock>`) so the
/// UDP net loop can access them directly without actor message-passing.
#[derive(Actor)]
pub struct SfuRoomActor {
    pub(crate) room_key: RoomKey,
    pub(crate) peer_store: Arc<PeerStore>,
    pub(crate) local_addr: SocketAddr,
}

impl SfuRoomActor {
    pub fn new(room_key: RoomKey, local_addr: SocketAddr, peer_store: Arc<PeerStore>) -> Self {
        Self {
            room_key,
            peer_store,
            local_addr,
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Add a participant to the room via SDP offer; returns the SDP answer.
pub struct AddParticipant {
    pub sid: String,
    pub jid: FullJid,
    pub sdp_offer: String,
}

impl kameo::message::Message<AddParticipant> for SfuRoomActor {
    type Reply = Result<String, String>;

    async fn handle(
        &mut self,
        msg: AddParticipant,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let (mut peer, answer_sdp) =
            SfuPeer::new_from_offer(&msg.sdp_offer, self.local_addr, self.room_key.clone())?;

        peer.jid = Some(msg.jid.clone());
        peer.sid = msg.sid.clone();

        self.peer_store.insert(msg.sid.clone(), peer).await;

        info!(
            room = %self.room_key.0,
            sid  = %msg.sid,
            jid  = %msg.jid,
            "Participant joined SFU room"
        );

        Ok(answer_sdp)
    }
}

/// Remove a participant from the room; returns `Ok(true)` when the room is now empty.
pub struct RemoveParticipant {
    pub sid: String,
}

impl kameo::message::Message<RemoveParticipant> for SfuRoomActor {
    type Reply = Result<bool, String>;

    async fn handle(
        &mut self,
        msg: RemoveParticipant,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match self.peer_store.remove(&msg.sid).await {
            Some(mut peer) => {
                peer.disconnect();
                info!(
                    room = %self.room_key.0,
                    sid  = %msg.sid,
                    "Participant left SFU room"
                );
                let remaining = self.peer_store.peer_count_in_room(&self.room_key).await;
                Ok(remaining == 0)
            }
            None => {
                warn!(
                    room = %self.room_key.0,
                    sid  = %msg.sid,
                    "RemoveParticipant: unknown SID"
                );
                Err(format!("No participant with sid '{}'", msg.sid))
            }
        }
    }
}

/// Return the current participant count.
pub struct GetParticipantCount;

impl kameo::message::Message<GetParticipantCount> for SfuRoomActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetParticipantCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.peer_store.peer_count_in_room(&self.room_key).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_room_with_key() {
        let peer_store = Arc::new(PeerStore::new());
        let room = SfuRoomActor::new(
            RoomKey("test_room".to_string()),
            "127.0.0.1:10000".parse().expect("valid addr"),
            peer_store,
        );
        assert_eq!(room.room_key.0, "test_room");
    }

    #[tokio::test]
    async fn tracks_participant_count() {
        let peer_store = Arc::new(PeerStore::new());
        let room_key = RoomKey("test_room".to_string());
        let room = SfuRoomActor::new(
            room_key.clone(),
            "127.0.0.1:10000".parse().expect("valid addr"),
            peer_store.clone(),
        );
        let count = room.peer_store.peer_count_in_room(&room_key).await;
        assert_eq!(count, 0);
    }
}
