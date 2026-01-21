//! MUC Room Registry
//!
//! Manages active MUC room instances, providing lookup by room JID
//! and room creation/destruction.

use std::sync::Arc;

use dashmap::DashMap;
use jid::BareJid;
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

use super::{MucRoom, RoomConfig};
use crate::XmppError;

/// Handle to send messages to a MUC room.
#[derive(Debug, Clone)]
pub struct RoomHandle {
    /// Room JID
    pub room_jid: BareJid,
    /// Channel to send messages to the room
    pub sender: mpsc::Sender<RoomMessage>,
}

/// Messages that can be sent to a MUC room.
#[derive(Debug)]
pub enum RoomMessage {
    /// A groupchat message to broadcast
    Broadcast(BroadcastMessage),
    /// Request to get room info
    GetInfo(tokio::sync::oneshot::Sender<RoomInfo>),
}

/// A message to broadcast to all room occupants.
#[derive(Debug, Clone)]
pub struct BroadcastMessage {
    /// The sender's room JID (room@domain/nick)
    pub from_room_jid: jid::FullJid,
    /// The original message
    pub message: xmpp_parsers::message::Message,
}

/// Basic room information.
#[derive(Debug, Clone)]
pub struct RoomInfo {
    /// Room JID
    pub room_jid: BareJid,
    /// Number of occupants
    pub occupant_count: usize,
    /// Room name
    pub name: String,
}

/// Registry for managing active MUC rooms.
///
/// Thread-safe registry that maps room JIDs to room handles.
/// Uses DashMap for concurrent access without explicit locking.
pub struct MucRoomRegistry {
    /// Map of room JID to room handle
    rooms: DashMap<BareJid, RoomHandle>,
    /// Map of room JID to room data (for direct access)
    room_data: DashMap<BareJid, Arc<tokio::sync::RwLock<MucRoom>>>,
    /// MUC domain (e.g., "muc.waddle.social")
    muc_domain: String,
}

impl MucRoomRegistry {
    /// Create a new room registry.
    pub fn new(muc_domain: String) -> Self {
        info!(domain = %muc_domain, "Creating MUC room registry");
        Self {
            rooms: DashMap::new(),
            room_data: DashMap::new(),
            muc_domain,
        }
    }

    /// Get the MUC domain.
    pub fn muc_domain(&self) -> &str {
        &self.muc_domain
    }

    /// Check if a JID is a MUC room on this server.
    pub fn is_muc_jid(&self, jid: &BareJid) -> bool {
        jid.domain().as_str() == self.muc_domain
    }

    /// Get a room handle by JID.
    pub fn get_room(&self, room_jid: &BareJid) -> Option<RoomHandle> {
        self.rooms.get(room_jid).map(|r| r.value().clone())
    }

    /// Get direct access to room data (for reading/writing room state).
    pub fn get_room_data(&self, room_jid: &BareJid) -> Option<Arc<tokio::sync::RwLock<MucRoom>>> {
        self.room_data.get(room_jid).map(|r| r.value().clone())
    }

    /// Check if a room exists.
    pub fn room_exists(&self, room_jid: &BareJid) -> bool {
        self.rooms.contains_key(room_jid)
    }

    /// Get the number of active rooms.
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Create a new room or get existing.
    ///
    /// If the room already exists, returns the existing handle.
    /// Otherwise creates a new room with the given configuration.
    #[instrument(skip(self, config), fields(room = %room_jid))]
    pub fn get_or_create_room(
        &self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
    ) -> Result<RoomHandle, XmppError> {
        // Check if room already exists
        if let Some(handle) = self.rooms.get(&room_jid) {
            debug!("Room already exists");
            return Ok(handle.value().clone());
        }

        // Create new room
        let room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        let room_data = Arc::new(tokio::sync::RwLock::new(room));

        // Create channel for room messages
        let (sender, _receiver) = mpsc::channel(256);

        let handle = RoomHandle {
            room_jid: room_jid.clone(),
            sender,
        };

        // Insert into registries
        self.rooms.insert(room_jid.clone(), handle.clone());
        self.room_data.insert(room_jid.clone(), room_data);

        info!("Created new MUC room");
        Ok(handle)
    }

    /// Create a room explicitly.
    #[instrument(skip(self, config), fields(room = %room_jid))]
    pub fn create_room(
        &self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
    ) -> Result<RoomHandle, XmppError> {
        if self.rooms.contains_key(&room_jid) {
            return Err(XmppError::muc(format!(
                "Room {} already exists",
                room_jid
            )));
        }

        self.get_or_create_room(room_jid, waddle_id, channel_id, config)
    }

    /// Destroy a room.
    #[instrument(skip(self), fields(room = %room_jid))]
    pub fn destroy_room(&self, room_jid: &BareJid) -> Option<RoomHandle> {
        self.room_data.remove(room_jid);
        let removed = self.rooms.remove(room_jid);
        if removed.is_some() {
            info!("Destroyed MUC room");
        } else {
            warn!("Attempted to destroy non-existent room");
        }
        removed.map(|(_, handle)| handle)
    }

    /// List all room JIDs.
    pub fn list_rooms(&self) -> Vec<BareJid> {
        self.rooms.iter().map(|r| r.key().clone()).collect()
    }

    /// Get room info for all rooms.
    pub async fn list_room_info(&self) -> Vec<RoomInfo> {
        let mut infos = Vec::new();
        for entry in self.room_data.iter() {
            let room = entry.value().read().await;
            infos.push(RoomInfo {
                room_jid: room.room_jid.clone(),
                occupant_count: room.occupant_count(),
                name: room.config.name.clone(),
            });
        }
        infos
    }
}

impl std::fmt::Debug for MucRoomRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MucRoomRegistry")
            .field("muc_domain", &self.muc_domain)
            .field("room_count", &self.rooms.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_room_jid(name: &str) -> BareJid {
        format!("{}@muc.example.com", name).parse().unwrap()
    }

    #[test]
    fn test_registry_creation() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());
        assert_eq!(registry.muc_domain(), "muc.example.com");
        assert_eq!(registry.room_count(), 0);
    }

    #[test]
    fn test_is_muc_jid() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());

        let muc_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let other_jid: BareJid = "user@example.com".parse().unwrap();

        assert!(registry.is_muc_jid(&muc_jid));
        assert!(!registry.is_muc_jid(&other_jid));
    }

    #[test]
    fn test_create_room() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());
        let room_jid = test_room_jid("testroom");

        let result = registry.create_room(
            room_jid.clone(),
            "waddle-1".to_string(),
            "channel-1".to_string(),
            RoomConfig::default(),
        );

        assert!(result.is_ok());
        assert!(registry.room_exists(&room_jid));
        assert_eq!(registry.room_count(), 1);
    }

    #[test]
    fn test_create_duplicate_room_fails() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());
        let room_jid = test_room_jid("testroom");

        registry
            .create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        let result = registry.create_room(
            room_jid,
            "waddle-1".to_string(),
            "channel-1".to_string(),
            RoomConfig::default(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_get_or_create_room() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());
        let room_jid = test_room_jid("testroom");

        // First call creates the room
        let handle1 = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        // Second call returns existing room
        let handle2 = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        assert_eq!(handle1.room_jid, handle2.room_jid);
        assert_eq!(registry.room_count(), 1);
    }

    #[test]
    fn test_destroy_room() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());
        let room_jid = test_room_jid("testroom");

        registry
            .create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        assert!(registry.room_exists(&room_jid));

        let removed = registry.destroy_room(&room_jid);
        assert!(removed.is_some());
        assert!(!registry.room_exists(&room_jid));
        assert_eq!(registry.room_count(), 0);
    }

    #[test]
    fn test_list_rooms() {
        let registry = MucRoomRegistry::new("muc.example.com".to_string());

        registry
            .create_room(
                test_room_jid("room1"),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        registry
            .create_room(
                test_room_jid("room2"),
                "waddle-1".to_string(),
                "channel-2".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        let rooms = registry.list_rooms();
        assert_eq!(rooms.len(), 2);
    }
}
