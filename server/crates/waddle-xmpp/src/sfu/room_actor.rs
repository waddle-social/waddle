//! SfuRoomActor — Kameo actor managing one active call room.

use super::peer::SfuPeer;
use super::RoomKey;
use jid::FullJid;
use kameo::message::Context;
use kameo::Actor;
use std::collections::HashMap;
use std::net::SocketAddr;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Actor that owns one [`SfuPeer`] per participant, keyed by Jingle session ID.
#[derive(Actor)]
pub struct SfuRoomActor {
    pub(crate) room_key: RoomKey,
    pub(crate) peers: HashMap<String, SfuPeer>, // keyed by Jingle SID
    pub(crate) local_addr: SocketAddr,
}

impl SfuRoomActor {
    pub fn new(room_key: RoomKey, local_addr: SocketAddr) -> Self {
        Self {
            room_key,
            peers: HashMap::new(),
            local_addr,
        }
    }

    pub fn participant_count(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
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
            SfuPeer::new_from_offer(&msg.sdp_offer, self.local_addr)?;

        peer.jid = Some(msg.jid.clone());
        peer.sid = msg.sid.clone();

        self.peers.insert(msg.sid.clone(), peer);

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
        match self.peers.remove(&msg.sid) {
            Some(mut peer) => {
                peer.disconnect();
                info!(
                    room = %self.room_key.0,
                    sid  = %msg.sid,
                    "Participant left SFU room"
                );
                Ok(self.is_empty())
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
        self.participant_count()
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
        let room = SfuRoomActor::new(
            RoomKey("test_room".to_string()),
            "127.0.0.1:10000".parse().unwrap(),
        );
        assert_eq!(room.room_key.0, "test_room");
        assert!(room.peers.is_empty());
    }

    #[tokio::test]
    async fn tracks_participant_count() {
        let room = SfuRoomActor::new(
            RoomKey("test_room".to_string()),
            "127.0.0.1:10000".parse().unwrap(),
        );
        assert_eq!(room.participant_count(), 0);
        assert!(room.is_empty());
    }
}
