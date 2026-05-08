use std::collections::HashMap;

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};

use super::affiliation::{AffiliationList, FederatedAffiliationConfig, FederatedPermissionPolicy};
use super::subject::SubjectState;
use crate::types::{Affiliation, Role};

/// Check if a JID is from a remote server.
///
/// A JID is considered remote if its domain differs from the local server domain.
/// This is used to reject non-local MUC occupants on the WebSocket-only server.
pub fn is_remote_jid(jid: &FullJid, local_domain: &str) -> bool {
    jid.domain().as_str() != local_domain
}

/// MUC room configuration.
///
/// Configuration knobs only — the live subject (text + setter +
/// timestamp) is **not** part of `RoomConfig` because it is mutated by
/// the XEP-0045 §8.1 message-path, not by the owner-config form
/// (§10.2). It lives on `MucRoom.subject` as `Option<SubjectState>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomConfig {
    /// Room name (human-readable)
    pub name: String,
    /// Room description
    pub description: Option<String>,
    /// Whether the room is persistent
    pub persistent: bool,
    /// Whether the room is members-only
    pub members_only: bool,
    /// Whether the room is moderated
    pub moderated: bool,
    /// Maximum number of occupants (0 = unlimited)
    pub max_occupants: u32,
    /// Whether to log messages (for MAM)
    pub enable_logging: bool,
    /// Whether the room uses Waddle thread-oriented metadata.
    #[serde(default)]
    pub forum: bool,
    /// Federation permission policy retained for serialized room configs.
    ///
    /// Remote XMPP federation is not served by Waddle, so active joins must still be local
    /// WebSocket C2S joins.
    pub federation_policy: FederatedPermissionPolicy,
    /// Legacy federation-affiliation configuration retained for serialized room configs.
    pub federated_affiliation_config: FederatedAffiliationConfig,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            persistent: true,
            members_only: true,
            moderated: false,
            max_occupants: 0,
            enable_logging: true,
            forum: false,
            federation_policy: FederatedPermissionPolicy::default(),
            federated_affiliation_config: FederatedAffiliationConfig::open_member(),
        }
    }
}

/// A room occupant (user currently in the room).
#[derive(Debug, Clone)]
pub struct Occupant {
    /// Real JID of the user
    pub real_jid: FullJid,
    /// Nickname in the room
    pub nick: String,
    /// Current role in the room
    pub role: Role,
    /// Affiliation with the room
    pub affiliation: Affiliation,
    /// Whether this occupant is from a remote server.
    pub is_remote: bool,
    /// The home server domain for this occupant.
    pub home_server: Option<String>,
}

/// MUC room actor state.
#[derive(Debug, Clone)]
pub struct MucRoom {
    /// Room JID (bare)
    pub room_jid: BareJid,
    /// Associated Waddle ID
    pub waddle_id: String,
    /// Associated channel ID
    pub channel_id: String,
    /// Room configuration
    pub config: RoomConfig,
    /// XEP-0045 §7.2.15 / §8.1 subject state.
    pub subject: Option<SubjectState>,
    /// Current occupants (nick -> Occupant)
    pub occupants: HashMap<String, Occupant>,
    /// Active sessions for each room nick (nick -> full JIDs).
    pub(super) occupant_sessions: HashMap<String, Vec<FullJid>>,
    /// Persistent affiliation list (synced with Zanzibar)
    pub(super) affiliation_list: AffiliationList,
    /// Per-nickname occupancy generation, bumped each time a nickname
    /// transitions from absent to present.
    pub(super) nickname_generation: HashMap<String, u64>,
    /// Lower bound for fresh nickname generations in this room actor's lifetime.
    pub(super) generation_floor: u64,
}

impl MucRoom {
    /// Create a new MUC room.
    pub fn new(
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
    ) -> Self {
        Self {
            room_jid,
            waddle_id,
            channel_id,
            config,
            subject: None,
            occupants: HashMap::new(),
            occupant_sessions: HashMap::new(),
            affiliation_list: AffiliationList::new(),
            nickname_generation: HashMap::new(),
            generation_floor: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0),
        }
    }

    /// Add an occupant to the room.
    ///
    /// When this is a fresh join (nickname not currently present), the
    /// per-nickname occupancy generation is bumped — used by XEP-0308
    /// to disallow corrections across leave/rejoin cycles.
    pub fn add_occupant(&mut self, occupant: Occupant) {
        if !self.occupants.contains_key(&occupant.nick) {
            let floor = self.generation_floor;
            let gen = self
                .nickname_generation
                .entry(occupant.nick.clone())
                .or_insert(floor);
            *gen += 1;
        }
        self.occupant_sessions
            .insert(occupant.nick.clone(), vec![occupant.real_jid.clone()]);
        self.occupants.insert(occupant.nick.clone(), occupant);
    }

    /// Current occupancy generation for `nick`, or `None` if the
    /// nickname has never been observed since this actor was created.
    pub fn current_nickname_generation(&self, nick: &str) -> Option<u64> {
        self.nickname_generation.get(nick).copied()
    }

    /// Remove an occupant from the room.
    pub fn remove_occupant(&mut self, nick: &str) -> Option<Occupant> {
        self.occupant_sessions.remove(nick);
        self.occupants.remove(nick)
    }

    /// Get an occupant by nickname.
    pub fn get_occupant(&self, nick: &str) -> Option<&Occupant> {
        self.occupants.get(nick)
    }

    /// Get all active sessions for a nickname.
    pub fn get_occupant_sessions(&self, nick: &str) -> Vec<FullJid> {
        self.occupant_sessions
            .get(nick)
            .cloned()
            .or_else(|| self.occupants.get(nick).map(|o| vec![o.real_jid.clone()]))
            .unwrap_or_default()
    }

    /// Remove a specific full-JID session for a nickname.
    ///
    /// Returns `Some(true)` if that session was the last one for the nick and
    /// the occupant was removed, `Some(false)` if the nick still has active
    /// sessions, and `None` if the nickname doesn't exist in the room.
    pub fn remove_occupant_session(&mut self, nick: &str, jid: &FullJid) -> Option<bool> {
        if !self.occupants.contains_key(nick) {
            return None;
        }

        let fallback_real_jid = self.occupants.get(nick).map(|o| o.real_jid.clone());
        let sessions = self
            .occupant_sessions
            .entry(nick.to_string())
            .or_insert_with(|| fallback_real_jid.into_iter().collect());

        let previous_len = sessions.len();
        sessions.retain(|candidate| candidate != jid);

        if previous_len == sessions.len() {
            return Some(false);
        }

        if sessions.is_empty() {
            self.occupant_sessions.remove(nick);
            self.occupants.remove(nick);
            return Some(true);
        }

        if let Some(occupant) = self.occupants.get_mut(nick) {
            if occupant.real_jid == *jid {
                occupant.real_jid = sessions[0].clone();
            }
        }

        Some(false)
    }

    /// Get the number of occupants.
    pub fn occupant_count(&self) -> usize {
        self.occupants.len()
    }

    /// Check if the room is full.
    pub fn is_full(&self) -> bool {
        if self.config.max_occupants == 0 {
            false
        } else {
            self.occupants.len() >= self.config.max_occupants as usize
        }
    }
}
