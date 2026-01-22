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
pub mod messages;
pub mod presence;
pub mod room_registry;

pub use messages::{
    create_broadcast_message, is_muc_groupchat, looks_like_muc_jid, MessageRouteResult, MucMessage,
    OutboundMucMessage,
};
pub use presence::{
    build_leave_presence, build_occupant_presence, parse_muc_presence, MucJoinRequest,
    MucLeaveRequest, MucPresenceAction, OutboundMucPresence,
};
pub use room_registry::{MucRoomRegistry, RoomHandle, RoomInfo, RoomMessage};

use std::collections::HashMap;

use jid::{BareJid, FullJid, Jid};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};
use xmpp_parsers::message::{Message, MessageType};

use crate::types::{Affiliation, Role};
use crate::XmppError;
use affiliation::{AffiliationChange, AffiliationList};

/// Check if a JID is from a remote server.
///
/// A JID is considered remote if its domain differs from the local server domain.
/// This is used for S2S federation to determine which occupants need presence/messages
/// routed via server-to-server connections.
///
/// # Arguments
/// * `jid` - The JID to check
/// * `local_domain` - The local server's domain (e.g., "waddle.social")
///
/// # Returns
/// `true` if the JID is from a different domain, `false` if local
///
/// # Example
/// ```ignore
/// use jid::FullJid;
///
/// let jid: FullJid = "user@remote.example.com/resource".parse().unwrap();
/// assert!(is_remote_jid(&jid, "waddle.social"));
///
/// let local_jid: FullJid = "user@waddle.social/resource".parse().unwrap();
/// assert!(!is_remote_jid(&local_jid, "waddle.social"));
/// ```
pub fn is_remote_jid(jid: &FullJid, local_domain: &str) -> bool {
    jid.domain().as_str() != local_domain
}

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
    /// Whether this occupant is from a remote server (S2S federation)
    pub is_remote: bool,
    /// The home server domain for this occupant (for S2S routing)
    pub home_server: Option<String>,
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
            Affiliation::Member => Role::Participant,
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
    ///
    /// If `local_domain` is provided, the occupant's remote status will be
    /// automatically detected based on whether their JID domain matches.
    ///
    /// # Arguments
    /// * `real_jid` - The user's full JID
    /// * `nick` - The nickname to use in the room
    /// * `local_domain` - Optional local server domain for remote detection
    pub fn add_occupant_with_affiliation(
        &mut self,
        real_jid: FullJid,
        nick: String,
        local_domain: Option<&str>,
    ) -> &Occupant {
        let bare_jid = real_jid.to_bare();
        let affiliation = self.affiliation_list.get(&bare_jid);
        let role = self.derive_role_from_affiliation(affiliation);

        // Determine remote status based on domain comparison
        let (is_remote, home_server) = match local_domain {
            Some(domain) => {
                let jid_domain = real_jid.domain().as_str();
                let remote = jid_domain != domain;
                let server = if remote {
                    Some(jid_domain.to_string())
                } else {
                    None
                };
                (remote, server)
            }
            None => (false, None),
        };

        let occupant = Occupant {
            real_jid,
            nick: nick.clone(),
            role,
            affiliation,
            is_remote,
            home_server,
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

    // === Message Broadcasting ===

    /// Broadcast a message to all occupants in the room.
    ///
    /// Per XEP-0045:
    /// - The message is sent from the room JID with sender's nick as resource
    /// - All occupants receive the message (including the sender as echo)
    /// - Visitors in moderated rooms cannot send messages
    ///
    /// Returns a list of outbound messages to send to each occupant.
    #[instrument(skip(self, message), fields(room = %self.room_jid))]
    pub fn broadcast_message(
        &self,
        sender_nick: &str,
        message: &Message,
    ) -> Result<Vec<OutboundMucMessage>, XmppError> {
        // Verify sender is an occupant
        let sender = self.occupants.get(sender_nick).ok_or_else(|| {
            XmppError::forbidden(Some(format!(
                "You are not an occupant of {}",
                self.room_jid
            )))
        })?;

        // Check if sender has permission to speak
        if self.config.moderated && sender.role == Role::Visitor {
            return Err(XmppError::forbidden(Some(
                "Visitors cannot speak in moderated rooms".to_string(),
            )));
        }

        // Build the 'from' JID: room@domain/sender_nick
        let from_room_jid = self
            .room_jid
            .with_resource_str(sender_nick)
            .map_err(|e| XmppError::internal(format!("Invalid nick as resource: {}", e)))?;

        debug!(
            sender = %sender_nick,
            occupant_count = self.occupants.len(),
            "Broadcasting message to room occupants"
        );

        // Create outbound messages for all occupants
        let mut outbound = Vec::with_capacity(self.occupants.len());

        for occupant in self.occupants.values() {
            let mut broadcast_msg = message.clone();
            broadcast_msg.type_ = MessageType::Groupchat;
            broadcast_msg.from = Some(Jid::from(from_room_jid.clone()));
            broadcast_msg.to = Some(Jid::from(occupant.real_jid.clone()));

            outbound.push(OutboundMucMessage::new(
                occupant.real_jid.clone(),
                broadcast_msg,
            ));
        }

        debug!(
            message_count = outbound.len(),
            "Created broadcast messages for occupants"
        );

        Ok(outbound)
    }

    /// Find the occupant by their real JID.
    ///
    /// Useful for routing incoming messages to find the sender's nick.
    pub fn find_occupant_by_real_jid(&self, jid: &FullJid) -> Option<&Occupant> {
        self.occupants.values().find(|o| &o.real_jid == jid)
    }

    /// Find the occupant's nick by their real JID.
    pub fn find_nick_by_real_jid(&self, jid: &FullJid) -> Option<&str> {
        self.find_occupant_by_real_jid(jid)
            .map(|o| o.nick.as_str())
    }

    // === Remote Occupant Management (S2S Federation) ===

    /// Get all remote occupants in the room.
    ///
    /// Returns occupants whose `is_remote` flag is true, meaning they are
    /// connected via S2S federation from another server.
    ///
    /// This is useful for routing presence updates and messages to remote
    /// servers during federation.
    pub fn get_remote_occupants(&self) -> Vec<&Occupant> {
        self.occupants
            .values()
            .filter(|o| o.is_remote)
            .collect()
    }

    /// Get all occupants grouped by their home server domain.
    ///
    /// Returns a map from domain name to list of occupants from that domain.
    /// Local occupants (where `home_server` is `None`) are grouped under
    /// the key "local".
    ///
    /// This is useful for efficient S2S routing - instead of sending individual
    /// stanzas, you can batch messages/presence by destination server.
    ///
    /// # Example
    /// ```ignore
    /// let occupants_by_domain = room.get_occupants_by_domain();
    /// for (domain, occupants) in occupants_by_domain {
    ///     if domain == "local" {
    ///         // Handle local occupants via C2S
    ///     } else {
    ///         // Route to remote server via S2S
    ///         s2s_pool.send_to_server(&domain, stanzas);
    ///     }
    /// }
    /// ```
    pub fn get_occupants_by_domain(&self) -> HashMap<String, Vec<&Occupant>> {
        let mut by_domain: HashMap<String, Vec<&Occupant>> = HashMap::new();

        for occupant in self.occupants.values() {
            let domain = occupant
                .home_server
                .as_deref()
                .unwrap_or("local")
                .to_string();

            by_domain.entry(domain).or_default().push(occupant);
        }

        by_domain
    }

    /// Get occupants from a specific domain.
    ///
    /// # Arguments
    /// * `domain` - The domain to filter by. Use "local" for local occupants,
    ///              or a specific domain name for remote occupants.
    pub fn get_occupants_for_domain(&self, domain: &str) -> Vec<&Occupant> {
        if domain == "local" {
            // Return occupants without a home_server (local users)
            self.occupants
                .values()
                .filter(|o| o.home_server.is_none())
                .collect()
        } else {
            // Return occupants from the specified remote domain
            self.occupants
                .values()
                .filter(|o| o.home_server.as_deref() == Some(domain))
                .collect()
        }
    }

    /// Get the count of remote occupants.
    pub fn remote_occupant_count(&self) -> usize {
        self.occupants.values().filter(|o| o.is_remote).count()
    }

    /// Get the count of local occupants.
    pub fn local_occupant_count(&self) -> usize {
        self.occupants.values().filter(|o| !o.is_remote).count()
    }

    /// Get all unique remote server domains that have occupants in this room.
    ///
    /// Useful for determining which S2S connections are needed for this room.
    pub fn get_remote_domains(&self) -> Vec<String> {
        let mut domains: Vec<String> = self
            .occupants
            .values()
            .filter_map(|o| o.home_server.clone())
            .collect();

        domains.sort();
        domains.dedup();
        domains
    }
}
