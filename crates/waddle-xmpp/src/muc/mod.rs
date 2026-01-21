//! Multi-User Chat (MUC) implementation.
//!
//! Implements XEP-0045 for group chat functionality, with each room
//! managed as a Kameo actor for concurrent message handling.
//!
//! ## Affiliation Sync
//!
//! This module integrates with Waddle's Zanzibar-based permission system
//! to derive MUC affiliations. See [`affiliation`] for details on the
//! permission-to-affiliation mapping.

pub mod affiliation;

use std::collections::HashMap;

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};

use crate::types::{Affiliation, Role};
use affiliation::{AffiliationChange, AffiliationList};

/// MUC room configuration.
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
}

/// MUC room actor state.
#[derive(Debug)]
pub struct MucRoom {
    /// Room JID (bare)
    pub room_jid: BareJid,
    /// Associated Waddle ID
    pub waddle_id: String,
    /// Associated channel ID
    pub channel_id: String,
    /// Room configuration
    pub config: RoomConfig,
    /// Current occupants (nick -> Occupant)
    pub occupants: HashMap<String, Occupant>,
    /// Persistent affiliation list (synced with Zanzibar)
    affiliation_list: AffiliationList,
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
            occupants: HashMap::new(),
            affiliation_list: AffiliationList::new(),
        }
    }

    /// Add an occupant to the room.
    pub fn add_occupant(&mut self, occupant: Occupant) {
        self.occupants.insert(occupant.nick.clone(), occupant);
    }

    /// Remove an occupant from the room.
    pub fn remove_occupant(&mut self, nick: &str) -> Option<Occupant> {
        self.occupants.remove(nick)
    }

    /// Get an occupant by nickname.
    pub fn get_occupant(&self, nick: &str) -> Option<&Occupant> {
        self.occupants.get(nick)
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

    // === Affiliation Management ===

    /// Get the affiliation for a JID.
    pub fn get_affiliation(&self, jid: &BareJid) -> Affiliation {
        self.affiliation_list.get(jid)
    }

    /// Set the affiliation for a JID.
    ///
    /// Returns the change if the affiliation actually changed.
    /// Also updates any occupant with this JID.
    pub fn set_affiliation(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
    ) -> Option<AffiliationChange> {
        let change = self.affiliation_list.set(jid.clone(), affiliation);

        // Update any occupants with this JID
        if change.is_some() {
            for occupant in self.occupants.values_mut() {
                if occupant.real_jid.to_bare() == jid {
                    occupant.affiliation = affiliation;
                }
            }
        }

        change
    }

    /// Sync an occupant's affiliation from the persistent list.
    ///
    /// Call this when an occupant joins to ensure their affiliation
    /// matches the stored value.
    pub fn sync_occupant_affiliation(&mut self, nick: &str) -> Option<Affiliation> {
        if let Some(occupant) = self.occupants.get_mut(nick) {
            let stored = self.affiliation_list.get(&occupant.real_jid.to_bare());
            if occupant.affiliation != stored {
                occupant.affiliation = stored;
            }
            Some(stored)
        } else {
            None
        }
    }

    /// Get all JIDs with a specific affiliation.
    pub fn get_jids_by_affiliation(&self, affiliation: Affiliation) -> Vec<BareJid> {
        self.affiliation_list.by_affiliation(affiliation)
    }

    /// Get all affiliation entries for the room.
    pub fn get_all_affiliations(&self) -> Vec<affiliation::AffiliationEntry> {
        self.affiliation_list.all()
    }

    /// Check if a JID has at least the specified affiliation.
    pub fn has_affiliation_at_least(&self, jid: &BareJid, min: Affiliation) -> bool {
        self.affiliation_list.has_at_least(jid, min)
    }

    /// Check if a user can join this room based on affiliation.
    ///
    /// For members-only rooms, users need at least Member affiliation.
    pub fn can_user_join(&self, jid: &BareJid) -> bool {
        if !self.config.members_only {
            // Open room - anyone can join (unless banned)
            self.get_affiliation(jid) != Affiliation::Outcast
        } else {
            // Members-only - need at least Member affiliation
            self.has_affiliation_at_least(jid, Affiliation::Member)
        }
    }

    /// Derive the initial role for a user based on their affiliation.
    ///
    /// Per XEP-0045:
    /// - Owner/Admin -> Moderator role
    /// - Member -> Participant role (in moderated rooms, may be Visitor otherwise)
    /// - None -> Participant (if allowed) or Visitor
    pub fn derive_role_from_affiliation(&self, affiliation: Affiliation) -> Role {
        match affiliation {
            Affiliation::Owner | Affiliation::Admin => Role::Moderator,
            Affiliation::Member => {
                if self.config.moderated {
                    Role::Participant
                } else {
                    Role::Participant
                }
            }
            Affiliation::None => {
                if self.config.moderated {
                    Role::Visitor
                } else {
                    Role::Participant
                }
            }
            Affiliation::Outcast => Role::None, // Banned users cannot join
        }
    }

    /// Add an occupant with affiliation looked up from the list.
    ///
    /// This is the preferred way to add occupants as it ensures
    /// affiliation consistency.
    pub fn add_occupant_with_affiliation(&mut self, real_jid: FullJid, nick: String) -> &Occupant {
        let bare_jid = real_jid.to_bare();
        let affiliation = self.affiliation_list.get(&bare_jid);
        let role = self.derive_role_from_affiliation(affiliation);

        let occupant = Occupant {
            real_jid,
            nick: nick.clone(),
            role,
            affiliation,
        };

        self.occupants.insert(nick.clone(), occupant);
        self.occupants.get(&nick).unwrap()
    }

    /// Update affiliations from a resolver (async operation).
    ///
    /// This updates the local affiliation for a JID based on
    /// the result of an affiliation resolver query.
    pub fn update_affiliation_from_resolver(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
    ) -> Option<AffiliationChange> {
        self.set_affiliation(jid, affiliation)
    }

    /// Check if the room has at least one owner.
    pub fn has_owner(&self) -> bool {
        self.affiliation_list.has_owner()
    }
}
