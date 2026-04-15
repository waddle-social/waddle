//! SFU (Selective Forwarding Unit) — XMPP-native group call media server.
//!
//! The SFU is an XMPP component at `sfu.{domain}` that speaks Jingle (XEP-0166)
//! to negotiate WebRTC sessions with clients. Each active call is a `SfuRoomActor`
//! owning str0m `Rtc` instances per participant.

pub mod net;
pub mod peer;
pub mod room_actor;
pub mod sdp;
pub mod service_actor;

use std::collections::HashMap;
use tokio::sync::RwLock;

/// Unique key for an SFU call room, derived from waddle + channel IDs.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct RoomKey(pub String);

impl RoomKey {
    /// Parse a room key from a Jingle session ID.
    /// Session IDs are formatted as `{waddle_id}_{channel_id}_{uuid}`.
    pub fn from_session_id(sid: &str) -> Option<Self> {
        let parts: Vec<&str> = sid.splitn(3, '_').collect();
        if parts.len() >= 2 {
            Some(Self(format!("{}_{}", parts[0], parts[1])))
        } else {
            None
        }
    }
}

/// Registry of active SFU call rooms.
#[derive(Debug, Default)]
pub struct SfuRegistry {
    rooms: RwLock<HashMap<RoomKey, kameo::actor::ActorRef<room_actor::SfuRoomActor>>>,
}

impl SfuRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_room(
        &self,
        key: &RoomKey,
    ) -> Option<kameo::actor::ActorRef<room_actor::SfuRoomActor>> {
        self.rooms.read().await.get(key).cloned()
    }

    pub async fn insert_room(
        &self,
        key: RoomKey,
        actor_ref: kameo::actor::ActorRef<room_actor::SfuRoomActor>,
    ) {
        self.rooms.write().await.insert(key, actor_ref);
    }

    pub async fn remove_room(&self, key: &RoomKey) {
        self.rooms.write().await.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_room_key_from_session_id() {
        let key = RoomKey::from_session_id("waddle123_channel456_some-uuid").unwrap();
        assert_eq!(key.0, "waddle123_channel456");
    }

    #[test]
    fn rejects_invalid_session_id() {
        assert!(RoomKey::from_session_id("no-underscores").is_none());
    }

    #[test]
    fn room_key_from_two_part_sid() {
        let key = RoomKey::from_session_id("w_c").unwrap();
        assert_eq!(key.0, "w_c");
    }
}
