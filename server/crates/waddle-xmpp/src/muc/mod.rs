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

pub mod admin;
pub mod affiliation;
pub mod messages;
pub mod owner;
pub mod presence;
pub mod room_actor;
pub mod room_registry;
pub mod room_registry_actor;

pub use admin::{
    build_admin_result, build_admin_set_result, build_role_result, is_affiliation_change_query,
    is_muc_admin_get, is_muc_admin_iq, is_muc_admin_set, is_muc_owner_get, is_muc_owner_set,
    is_role_change_query, parse_admin_query, AdminItem, AdminQuery, AffiliationChangeResult,
    KickBanInfo, MucStatusCode, RoleChangeResult, NS_MUC_ADMIN, NS_MUC_OWNER,
};
pub use messages::{
    create_broadcast_message, is_muc_groupchat, looks_like_muc_jid, MessageRouteResult, MucMessage,
    OutboundMucMessage,
};
pub use owner::{
    apply_config_form, build_config_form, build_config_result, build_destroy_notification,
    build_owner_set_result, parse_owner_query, ConfigFormData, DestroyRequest, OwnerAction,
    OwnerQuery, DATA_FORMS_NS, MUC_ROOMCONFIG_NS,
};
pub use presence::{
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_leave_presence, build_occupant_presence, build_occupant_presence_update,
    build_role_change_presence, parse_muc_presence, HistoryRequest, MucJoinRequest,
    MucLeaveRequest, MucPresenceAction, MucPresenceUpdateRequest, OutboundMucPresence,
};
pub use room_actor::RoomActorError;
pub use room_registry::{MucRoomRegistry, RoomHandle, RoomInfo, RoomMessage};
pub use room_registry_actor::RoomRegistryError;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid, Jid};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};
use xmpp_parsers::message::{Message, MessageType};

use crate::types::{Affiliation, Role};
use crate::XmppError;
use affiliation::{
    AffiliationChange, AffiliationList, FederatedAffiliationConfig, FederatedPermissionPolicy,
};

/// Check if a JID is from a remote server.
///
/// A JID is considered remote if its domain differs from the local server domain.
/// This is used to reject non-local MUC occupants on the WebSocket-only server.
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

/// Typed multi-language subject-text map carried by [`SubjectState`]
/// and the `OutboundEvent::PersistRoomSubject` / `RoomActor::SetSubject`
/// payloads.
///
/// Newtype around `BTreeMap<xml:lang, subject-text>` per the
/// typed-payloads hard rule (CLAUDE.md): a generic `BTreeMap<String, String>`
/// at a protocol boundary makes the xml:lang / subject-text
/// relationship type-indistinguishable from any other map of strings.
/// `RoomSubjectTexts` encapsulates that relationship and the
/// `xmpp_parsers::message::Message::subjects` ↔ persisted-map
/// conversion in one place so call sites don't reinvent it.
///
/// The empty-string key is the default-language entry, mirroring
/// `xmpp_parsers::message::Message::subjects`'s own
/// `BTreeMap<Lang, Subject>` shape (where `Lang = String`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSubjectTexts(std::collections::BTreeMap<String, String>);

impl RoomSubjectTexts {
    /// Empty map (no subject elements).
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture a `Message`'s typed `subjects` field by cloning every
    /// `xmpp_parsers::message::Subject`'s text and pairing it with
    /// its `xml:lang` key.
    pub fn from_message_subjects(
        subjects: &std::collections::BTreeMap<String, xmpp_parsers::message::Subject>,
    ) -> Self {
        Self(
            subjects
                .iter()
                .map(|(lang, subject)| (lang.clone(), subject.0.clone()))
                .collect(),
        )
    }

    /// Insert one `<subject xml:lang='...'>` per persisted entry into
    /// `msg.subjects`, wrapped in xmpp_parsers' typed `Subject`. Used
    /// by the join-time replay builder.
    pub fn apply_to_message(&self, msg: &mut xmpp_parsers::message::Message) {
        for (lang, text) in &self.0 {
            msg.subjects
                .insert(lang.clone(), xmpp_parsers::message::Subject(text.clone()));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, lang: &str) -> Option<&str> {
        self.0.get(lang).map(String::as_str)
    }
}

impl From<std::collections::BTreeMap<String, String>> for RoomSubjectTexts {
    fn from(map: std::collections::BTreeMap<String, String>) -> Self {
        Self(map)
    }
}

impl FromIterator<(String, String)> for RoomSubjectTexts {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// XEP-0045 §7.2.15 / §8.1 room subject state.
///
/// `texts` carries every `<subject xml:lang='...'>` variant from the
/// originating §8.1 message, keyed by `xml:lang` (the empty string is
/// the default-language entry). All entries with empty values
/// represent an **explicitly cleared** subject — XEP-0045 §7.2.15
/// distinguishes this from "never set", which is represented by
/// `MucRoom.subject == None`. Persisting every language variant rather
/// than a single canonical text avoids losing localized subjects: the
/// reflected broadcast and the join-time replay would otherwise carry
/// different `<subject>` element sets.
///
/// `setter` and `setter_nick` are the bare JID of the occupant who
/// last set the subject and the nickname they were using at that
/// moment; `setter_nick` is frozen here rather than re-resolved at
/// emission so that historical join-time emissions remain stable
/// across nick changes and after the setter has left. `set_at` powers
/// the XEP-0203 `<delay/>` stamp on the join-time emission and the
/// XEP-0421 occupant-id derivation uses `setter` as the bare-JID input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectState {
    pub texts: RoomSubjectTexts,
    pub setter: BareJid,
    pub setter_nick: String,
    pub set_at: DateTime<Utc>,
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
    /// XEP-0045 §7.2.15 / §8.1 subject state. `None` = "never set"
    /// (joiners receive an empty `<subject/>` with no `<delay/>` and no
    /// `<occupant-id/>`). `Some(SubjectState { texts, .. })` where every
    /// value in `texts` is empty = "explicitly cleared" (joiners
    /// receive empty `<subject/>` plus `<delay/>` and `<occupant-id/>`
    /// for the user who cleared it). The two cases are distinguishable
    /// on the wire per the §7.2.15 SHOULD that `<delay/>` be included
    /// for actively-cleared subjects.
    pub subject: Option<SubjectState>,
    /// Current occupants (nick -> Occupant)
    pub occupants: HashMap<String, Occupant>,
    /// Active sessions for each room nick (nick -> full JIDs).
    occupant_sessions: HashMap<String, Vec<FullJid>>,
    /// Persistent affiliation list (synced with Zanzibar)
    affiliation_list: AffiliationList,
    /// Per-nickname occupancy generation, bumped each time a nickname
    /// transitions from absent to present (XEP-0308 §3 SHOULD #2: a
    /// full-JID leaving and rejoining should not be allowed to correct
    /// messages from the previous occupancy). Each fresh nickname is
    /// seeded from [`Self::generation_floor`] before the first bump so
    /// post-restart generation values cannot collide with pre-restart
    /// archived rows.
    nickname_generation: HashMap<String, u64>,
    /// Lower bound for fresh nickname generations in this room actor's
    /// lifetime. Initialised at room creation from the wall clock so
    /// that every restart of the server (or recreation of the actor)
    /// uses a higher floor than any prior archive row's generation,
    /// which closes the §3 SHOULD #2 correction window across server
    /// boundaries. Pre-restart rows recorded with generation N have
    /// N < floor; post-restart fresh joins start at floor+1, so the
    /// equality check in `verify_muc_occupancy_generation` cannot
    /// match.
    generation_floor: u64,
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
            // Seed the floor from the wall clock in milliseconds. Any
            // future restart will produce a higher floor than any
            // generation values archived under the previous floor,
            // since each fresh-nick generation is at most floor + (#
            // distinct same-nickname rejoins observed in this actor's
            // lifetime), which is bounded well below the millisecond
            // tick rate.
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
        if let Some(existing) = self.occupants.get(&nick) {
            if existing.real_jid.to_bare() == real_jid.to_bare() {
                let sessions = self
                    .occupant_sessions
                    .entry(nick.clone())
                    .or_insert_with(|| vec![existing.real_jid.clone()]);
                if !sessions.iter().any(|session| session == &real_jid) {
                    sessions.push(real_jid);
                }
                return self
                    .occupants
                    .get(&nick)
                    .expect("occupant exists while adding same-bare session");
            }
        }

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
            real_jid: real_jid.clone(),
            nick: nick.clone(),
            role,
            affiliation,
            is_remote,
            home_server,
        };

        let floor = self.generation_floor;
        let gen = self
            .nickname_generation
            .entry(nick.clone())
            .or_insert(floor);
        *gen += 1;

        self.occupant_sessions.insert(nick.clone(), vec![real_jid]);
        self.occupants.insert(nick.clone(), occupant);
        self.occupants
            .get(&nick)
            .expect("occupant just inserted on previous line")
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
            for recipient_jid in self.get_occupant_sessions(&occupant.nick) {
                let mut broadcast_msg = message.clone();
                broadcast_msg.type_ = MessageType::Groupchat;
                broadcast_msg.from = Some(Jid::from(from_room_jid.clone()));
                broadcast_msg.to = Some(Jid::from(recipient_jid.clone()));

                outbound.push(OutboundMucMessage::new(recipient_jid, broadcast_msg));
            }
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
        self.occupants.values().find(|occupant| {
            self.get_occupant_sessions(&occupant.nick)
                .iter()
                .any(|session| session == jid)
        })
    }

    /// Find the occupant's nick by their real JID.
    pub fn find_nick_by_real_jid(&self, jid: &FullJid) -> Option<&str> {
        self.find_occupant_by_real_jid(jid).map(|o| o.nick.as_str())
    }

    // === Remote Occupant Metadata ===

    /// Get all remote occupants in the room.
    ///
    /// Returns occupants whose `is_remote` flag is true, meaning they are
    /// connected from another server.
    ///
    /// This is useful for routing presence updates and messages to remote
    /// servers during federation.
    pub fn get_remote_occupants(&self) -> Vec<&Occupant> {
        self.occupants.values().filter(|o| o.is_remote).collect()
    }

    /// Get all occupants grouped by their home server domain.
    ///
    /// Returns a map from domain name to list of occupants from that domain.
    /// Local occupants (where `home_server` is `None`) are grouped under
    /// the key "local".
    ///
    /// This is useful for callers that need to inspect room locality.
    ///
    /// # Example
    /// ```ignore
    /// let occupants_by_domain = room.get_occupants_by_domain();
    /// for (domain, occupants) in occupants_by_domain {
    ///     if domain == "local" {
    ///         // Handle local occupants via C2S
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
    ///   or a specific domain name for remote occupants.
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
    /// Useful for determining whether a room contains non-local occupants.
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

    // === Subject/Topic Management (XEP-0045 §7.2.15 / §8.1) ===

    /// Apply a §8.1 subject change. `texts` mirrors the originating
    /// message's `<subject xml:lang='...'>` map; an entry whose value
    /// is empty represents an explicit clear (still a `Some(SubjectState)`
    /// so future joins emit `<delay/>` per §7.2.15 SHOULD).
    ///
    /// Authorization is enforced upstream by
    /// `protocol::room::subject::MucSubjectHandler` against the frozen
    /// `RoomContext` snapshot — this method assumes the change has
    /// already passed §8.1's role-based gate.
    pub fn set_subject(
        &mut self,
        texts: RoomSubjectTexts,
        setter: BareJid,
        setter_nick: String,
        set_at: DateTime<Utc>,
    ) {
        self.subject = Some(SubjectState {
            texts,
            setter,
            setter_nick,
            set_at,
        });
    }
}
