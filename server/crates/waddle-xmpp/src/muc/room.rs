use std::collections::HashMap;

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};

use super::affiliation::{AffiliationList, FederatedAffiliationConfig, FederatedPermissionPolicy};
use super::pin::{PinPermission, PinnedEntry};
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
    /// #415: who may pin/unpin messages in this room. Default is
    /// `admins-only`; when set to `anyone`, any current occupant may
    /// pin. Set via the `urn:waddle:roomconfig:pinpermission` field on
    /// the standard XEP-0045 owner-config form.
    #[serde(default)]
    pub pin_permission: PinPermission,
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
            pin_permission: PinPermission::default(),
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
    /// Pinned messages in pin-time-desc order. A given `target_stanza_id`
    /// appears at most once. Held only in memory — pins do not survive
    /// room-actor shutdown by design (#414); persistence is a follow-up
    /// concern. Bounded by [`super::pin::MAX_PINNED_ENTRIES`].
    pub(super) pinned_entries: Vec<PinnedEntry>,
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
            pinned_entries: Vec::new(),
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

    /// Whether `bare_jid` is currently joined to this room as an
    /// occupant. Used by the pin-list IQ query handler to gate
    /// access on membership.
    pub fn has_occupant_with_bare_jid(&self, bare_jid: &BareJid) -> bool {
        self.occupants
            .values()
            .any(|o| o.real_jid.to_bare() == *bare_jid)
    }

    /// All pinned entries, newest pin first.
    pub fn pinned_entries(&self) -> &[PinnedEntry] {
        &self.pinned_entries
    }

    /// Add or replace a pinned entry. If a pin already exists for the
    /// same `target_stanza_id`, the previous entry is replaced and
    /// moved to the front. When [`super::pin::MAX_PINNED_ENTRIES`] is
    /// reached and the new entry isn't a replacement, the oldest entry
    /// is evicted to keep the list bounded — admins are responsible
    /// for periodic pruning, but the cap prevents pin-spam from
    /// exhausting room memory. Returns the previous entry for the
    /// same target, if any.
    pub fn upsert_pin(&mut self, entry: PinnedEntry) -> Option<PinnedEntry> {
        let previous = self.remove_pin_by_target(&entry.target_stanza_id);
        self.pinned_entries.insert(0, entry);
        if self.pinned_entries.len() > super::pin::MAX_PINNED_ENTRIES {
            self.pinned_entries.pop();
        }
        previous
    }

    /// Remove the pin entry matching `target_stanza_id`, if present.
    /// Compares by the typed XEP-0359 id field — the `by` JID is
    /// always the room itself for groupchat pins.
    pub fn remove_pin_by_target(
        &mut self,
        target_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> Option<PinnedEntry> {
        let position = self
            .pinned_entries
            .iter()
            .position(|e| e.target_stanza_id.id == target_stanza_id.id)?;
        Some(self.pinned_entries.remove(position))
    }
}

#[cfg(test)]
mod pin_state_tests {
    use super::*;
    use crate::muc::pin::{PinPreview, MAX_PINNED_ENTRIES};
    use chrono::{DateTime, Utc};
    use std::str::FromStr;
    use waddle_xmpp_core::xep0359::StanzaId;

    fn bare(s: &str) -> BareJid {
        BareJid::from_str(s).expect("valid bare jid")
    }

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-08T12:34:56Z")
            .expect("valid rfc3339")
            .with_timezone(&Utc)
    }

    fn stanza(id: &str) -> StanzaId {
        StanzaId::new(id.to_owned(), jid::Jid::from(bare("room@conf.example")))
    }

    fn entry(target: &str, pinner: &str) -> PinnedEntry {
        PinnedEntry {
            target_stanza_id: stanza(target),
            pinner_jid: bare(pinner),
            pinned_at: ts(),
            preview: PinPreview::new(bare("alice@example.com"), None, "hi", ts()),
        }
    }

    fn room() -> MucRoom {
        MucRoom::new(
            bare("room@conf.example"),
            "wad-1".into(),
            "chan-1".into(),
            RoomConfig::default(),
        )
    }

    #[test]
    fn upsert_appends_to_front_for_new_target() {
        let mut r = room();
        r.upsert_pin(entry("a", "admin@example.com"));
        r.upsert_pin(entry("b", "admin@example.com"));
        let ids: Vec<_> = r
            .pinned_entries()
            .iter()
            .map(|e| e.target_stanza_id.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn upsert_replaces_existing_target_and_returns_previous() {
        let mut r = room();
        r.upsert_pin(entry("a", "admin1@example.com"));
        r.upsert_pin(entry("b", "admin1@example.com"));
        let previous = r.upsert_pin(entry("a", "admin2@example.com"));
        let prev = previous.expect("previous entry returned");
        assert_eq!(prev.pinner_jid, bare("admin1@example.com"));
        let ids: Vec<_> = r
            .pinned_entries()
            .iter()
            .map(|e| e.target_stanza_id.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
        let updated = r
            .pinned_entries
            .iter()
            .find(|e| e.target_stanza_id.id == "a")
            .expect("found");
        assert_eq!(updated.pinner_jid, bare("admin2@example.com"));
    }

    #[test]
    fn remove_pin_by_target_returns_entry_when_present() {
        let mut r = room();
        r.upsert_pin(entry("a", "admin@example.com"));
        let removed = r.remove_pin_by_target(&stanza("a")).expect("entry removed");
        assert_eq!(removed.target_stanza_id.id, "a");
        assert!(r.pinned_entries().is_empty());
    }

    #[test]
    fn remove_pin_by_target_returns_none_when_absent() {
        let mut r = room();
        assert!(r.remove_pin_by_target(&stanza("nope")).is_none());
    }

    #[test]
    fn upsert_evicts_oldest_when_cap_reached() {
        let mut r = room();
        for n in 0..MAX_PINNED_ENTRIES {
            r.upsert_pin(entry(&format!("p-{n}"), "admin@example.com"));
        }
        assert_eq!(r.pinned_entries().len(), MAX_PINNED_ENTRIES);
        r.upsert_pin(entry("overflow", "admin@example.com"));
        assert_eq!(r.pinned_entries().len(), MAX_PINNED_ENTRIES);
        assert_eq!(
            r.pinned_entries()
                .first()
                .expect("non-empty")
                .target_stanza_id
                .id,
            "overflow"
        );
        assert!(r
            .pinned_entries()
            .iter()
            .all(|e| e.target_stanza_id.id != "p-0"));
    }

    #[test]
    fn fresh_room_has_no_pins() {
        // Pins are held only on `MucRoom`; when the actor is dropped or
        // the room is recreated via `MucRoom::new`, the list resets to
        // empty. This test pins the in-memory contract — there is no
        // persistence path. Future contributors adding persistence
        // must update this contract explicitly.
        let fresh = room();
        assert!(fresh.pinned_entries().is_empty());
    }
}
