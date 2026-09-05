use std::collections::HashMap;

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};

use super::affiliation::{AffiliationList, FederatedAffiliationConfig, FederatedPermissionPolicy};
use super::pin::{PinPermission, PinnedEntry};
use super::room_actor::OccupancyWatermark;
use super::subject::SubjectState;
use crate::types::{Affiliation, Role};
use crate::xep::xep0272::Muji;
use crate::xep::InCallPresenceState;

/// Check if a JID is from a remote server.
///
/// A JID is considered remote if its domain differs from the local server domain.
/// This is used to reject non-local MUC occupants on the WebSocket-only server.
pub fn is_remote_jid(jid: &FullJid, local_domain: &str) -> bool {
    jid.domain().as_str() != local_domain
}

/// XEP-0045 `muc#roomconfig_allowpm` — which occupant roles may send
/// private messages (§7.5) in this room. Wire values follow the XMPP
/// Registrar's field registration: `anyone`, `participants`,
/// `moderators`, `none`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowPm {
    /// Any occupant may send PMs (registrar default).
    #[default]
    Anyone,
    /// Occupants with role participant or moderator may send PMs.
    Participants,
    /// Only moderators may send PMs.
    Moderators,
    /// Private messages are disabled in this room.
    None,
}

impl AllowPm {
    /// Wire value used in the `muc#roomconfig_allowpm` data-form field.
    pub fn as_form_value(self) -> &'static str {
        match self {
            AllowPm::Anyone => "anyone",
            AllowPm::Participants => "participants",
            AllowPm::Moderators => "moderators",
            AllowPm::None => "none",
        }
    }

    /// Parse from the data-form `<value>` text. Returns `Option::None`
    /// for unknown / malformed values; callers keep the previous value.
    pub fn from_form_value(value: &str) -> Option<Self> {
        match value {
            "anyone" => Some(AllowPm::Anyone),
            "participants" => Some(AllowPm::Participants),
            "moderators" => Some(AllowPm::Moderators),
            "none" => Some(AllowPm::None),
            _ => None,
        }
    }

    /// Whether an occupant holding `role` may send a private message.
    /// `Role::None` (not an occupant) never qualifies — §7.5 PMs are an
    /// occupant-to-occupant exchange.
    pub fn permits(self, role: Role) -> bool {
        match self {
            AllowPm::Anyone => !matches!(role, Role::None),
            AllowPm::Participants => matches!(role, Role::Participant | Role::Moderator),
            AllowPm::Moderators => matches!(role, Role::Moderator),
            AllowPm::None => false,
        }
    }
}

/// MUC room configuration.
///
/// Configuration knobs only — the live subject (text + setter +
/// timestamp) is **not** part of `RoomConfig` because it is mutated by
/// the XEP-0045 §8.1 message-path, not by the owner-config form
/// (§10.2). It lives on `MucRoom.subject` as `Option<SubjectState>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomConfig {
    /// Room name (human-readable)
    pub name: String,
    /// Room description
    pub description: Option<String>,
    /// Whether the room is persistent
    pub persistent: bool,
    /// Whether the room is members-only
    pub members_only: bool,
    /// Whether the room is discoverable through the MUC service.
    pub public_room: bool,
    /// Whether the room is moderated
    pub moderated: bool,
    /// Maximum number of occupants (0 = unlimited)
    pub max_occupants: u32,
    /// Whether to log messages (for MAM)
    pub enable_logging: bool,
    /// Whether the room uses Waddle thread-oriented metadata.
    #[serde(default)]
    pub forum: bool,
    /// Whether this MUC room is a Waddle group DM rather than a channel.
    #[serde(default)]
    pub group_dm: bool,
    /// XEP-0045 §8.1 `muc#roomconfig_changesubject` (#1265 item 8):
    /// when true, any occupant (participant or visitor) may change the
    /// room subject; when false (the XEP default), moderators only.
    #[serde(default)]
    pub occupants_may_change_subject: bool,
    /// #415: who may pin/unpin messages in this room. Default is
    /// `admins-only`; when set to `anyone`, any current occupant may
    /// pin. Set via the `urn:waddle:roomconfig:pinpermission` field on
    /// the standard XEP-0045 owner-config form.
    #[serde(default)]
    pub pin_permission: PinPermission,
    /// XEP-0045 `muc#roomconfig_allowpm` (#1257): which occupant roles
    /// may send §7.5 private messages. Default `anyone`.
    #[serde(default)]
    pub allow_pm: AllowPm,
    /// Federation permission policy retained for serialized room configs.
    ///
    /// Remote XMPP federation is not served by Waddle, so active joins must still be local
    /// WebSocket C2S joins.
    pub federation_policy: FederatedPermissionPolicy,
    /// Legacy federation-affiliation configuration retained for serialized room configs.
    pub federated_affiliation_config: FederatedAffiliationConfig,
}

impl RoomConfig {
    /// Whether admission requires at least Member affiliation.
    ///
    /// Group DMs are always membership-scoped, even if an untrusted or
    /// legacy config payload carries a false `members_only` flag.
    pub const fn requires_membership(&self) -> bool {
        self.members_only || self.group_dm
    }

    /// Restore the invariant that every group DM is members-only.
    pub fn normalized(mut self) -> Self {
        if self.group_dm {
            self.members_only = true;
        }
        self
    }
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            persistent: true,
            members_only: true,
            public_room: true,
            moderated: false,
            max_occupants: 0,
            enable_logging: true,
            forum: false,
            group_dm: false,
            occupants_may_change_subject: false,
            pin_permission: PinPermission::default(),
            allow_pm: AllowPm::default(),
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
    /// Join watermark for each live occupant session. Replacement rejoins
    /// advance the watermark so deferred cleanup can refuse stale departures.
    pub(super) session_watermarks: HashMap<FullJid, OccupancyWatermark>,
    /// Process-wide occupancy order at which each session joined (see
    /// `LeaveAttemptId`); transplanted with the roster.
    pub(super) session_orders: HashMap<FullJid, super::room_actor::OccupancyOrder>,
    /// Connection identity of each live occupant session. A same-full-JID
    /// rejoin overwrites the previous generation so stale cleanup can prove
    /// it belongs to the displaced connection.
    pub(super) session_generations: HashMap<FullJid, waddle_xmpp_core::OccupancySessionGeneration>,
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
    /// Per-session `<muji xmlns='urn:xmpp:jingle:muji:0'/>` advertised
    /// state (XEP-0272), keyed by nick and then by full JID. Mirrors the client-emitted
    /// presence extension so:
    ///
    /// 1. Joining occupants see existing call indicators when their
    ///    initial occupant-list replay runs (the join handler reads
    ///    from this map and appends the `<muji/>` payload to the
    ///    replayed presence for any nick that has one).
    /// 2. Presence-update broadcasts can use the persisted state as
    ///    the source of truth rather than echoing a possibly-stale
    ///    payload from the client.
    ///
    /// Each full-JID entry is independent so that when a user has
    /// multiple sessions sharing the same nick (web + mobile), one
    /// resource can join or leave the call without overwriting the
    /// sibling resource's advertisement. The room-level wire shape is
    /// still one occupant presence per nick; `muji_for_nick` aggregates
    /// these session entries, preferring active contents over preparing.
    pub(super) muji_state: HashMap<String, HashMap<FullJid, Muji>>,
    /// Per-session in-call presence state (`urn:waddle:in-call:0`,
    /// #1029/#1030), keyed by nick then by full JID, carrying the full
    /// [`InCallPresenceState`](crate::xep::InCallPresenceState) (raised
    /// hand + mute) that session advertises. Mirrors
    /// [`muji_state`](Self::muji_state):
    ///
    /// 1. Late joiners see who already has a hand raised or is muted —
    ///    the join handler reads this map and appends the `<in-call>`
    ///    payload to the replayed presence for any session that owns one.
    /// 2. Presence-update broadcasts reflect the room-authoritative
    ///    per-session state rather than echoing the client payload, so a
    ///    sibling resource's presence cannot be stamped with another
    ///    session's in-call state.
    ///
    /// Only non-empty states are stored; an entry is cleared when the
    /// session lowers its hand and unmutes, leaves the call (`<muji/>`
    /// cleared), or leaves the room.
    pub(super) in_call_state: HashMap<String, HashMap<FullJid, InCallPresenceState>>,
}

/// Result of applying one session's Muji presence update.
#[derive(Debug, Clone)]
pub struct MujiPresenceState {
    /// The exact payload that should be reflected to the sender session.
    pub sender_muji: Option<Muji>,
    /// The aggregate payload that represents the occupant nick to every
    /// other session in the room.
    pub room_muji: Option<Muji>,
    /// Exact per-session Muji payloads still advertised for this nick
    /// after the update. Preparing is a resource-owned coordination
    /// state, so callers that need to preserve XEP-0272 joining
    /// semantics should prefer this list over the aggregate snapshot.
    pub session_mujis: Vec<(FullJid, Muji)>,
    /// True when this update changed the room from no active Muji call
    /// to at least one active call advertisement.
    pub active_call_started: bool,
}

impl MucRoom {
    /// This room's XEP-0045 moderation state, as the typed input to
    /// [`crate::types::Role::voice`]. Every voice decision — text
    /// broadcast (§7.5) and SFU media grants alike — goes through
    /// that one predicate, so they cannot drift apart.
    pub fn moderation(&self) -> crate::types::Moderation {
        crate::types::Moderation::from_moderated_flag(self.config.moderated)
    }

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
            config: config.normalized(),
            subject: None,
            occupants: HashMap::new(),
            occupant_sessions: HashMap::new(),
            session_watermarks: HashMap::new(),
            session_orders: HashMap::new(),
            session_generations: HashMap::new(),
            affiliation_list: AffiliationList::new(),
            nickname_generation: HashMap::new(),
            generation_floor: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0),
            pinned_entries: Vec::new(),
            muji_state: HashMap::new(),
            in_call_state: HashMap::new(),
        }
    }

    /// Apply a `<muji xmlns='urn:xmpp:jingle:muji:0'/>` presence update
    /// (XEP-0272) from `nick`, identified by the specific session
    /// (`originator`) that emitted it.
    ///
    /// Per XEP-0272 §Leaving, the absence of `<muji/>` (or an empty
    /// element with neither `<preparing/>` nor `<content>` children)
    /// means the participant has left the call; we mirror that by
    /// clearing the entry when [`Muji::is_empty`] is true.
    ///
    /// Returns the sender reflection and the post-update aggregate
    /// state for `nick`. The presence broadcaster uses the aggregate
    /// state for other occupants so one same-nick resource cannot hide
    /// active or preparing state owned by another resource.
    pub fn upsert_muji_presence(
        &mut self,
        nick: &str,
        originator: FullJid,
        muji: Muji,
    ) -> MujiPresenceState {
        let had_active_call = self.room_has_active_muji();
        if muji.is_empty() {
            if let Some(entries) = self.muji_state.get_mut(nick) {
                entries.remove(&originator);
                if entries.is_empty() {
                    self.muji_state.remove(nick);
                }
            }
            let room_muji = self.muji_for_nick(nick);
            let active_call_started =
                !had_active_call && room_muji.as_ref().is_some_and(Muji::is_active);
            MujiPresenceState {
                sender_muji: None,
                room_muji,
                session_mujis: self.muji_sessions_for_nick(nick),
                active_call_started,
            }
        } else {
            self.muji_state
                .entry(nick.to_owned())
                .or_default()
                .insert(originator, muji.clone());
            let room_muji = self.muji_for_nick(nick);
            let active_call_started =
                !had_active_call && room_muji.as_ref().is_some_and(Muji::is_active);
            MujiPresenceState {
                sender_muji: Some(muji),
                room_muji,
                session_mujis: self.muji_sessions_for_nick(nick),
                active_call_started,
            }
        }
    }

    /// Currently-advertised `<muji/>` element for `nick`, if any.
    /// Used by the join handler to enrich the replayed occupant
    /// presence list with active-call indicators for late joiners.
    pub fn muji_for_nick(&self, nick: &str) -> Option<Muji> {
        let entries = self.muji_state.get(nick)?;
        let mut preparing = false;
        let mut contents = Vec::new();
        let mut ordered_entries: Vec<_> = entries.iter().collect();
        ordered_entries.sort_by_key(|(jid, _)| jid.to_string());
        for (_, muji) in ordered_entries {
            preparing |= muji.preparing;
            for content in &muji.contents {
                if !contents.contains(content) {
                    contents.push(content.clone());
                }
            }
        }
        if !preparing && contents.is_empty() {
            return None;
        }
        Some(Muji {
            room: None,
            preparing,
            contents,
        })
    }

    fn room_has_active_muji(&self) -> bool {
        self.muji_state
            .values()
            .flat_map(|entries| entries.values())
            .any(Muji::is_active)
    }

    /// Currently-advertised Muji payload for one exact session.
    pub fn muji_for_session(&self, nick: &str, jid: &FullJid) -> Option<Muji> {
        self.muji_state
            .get(nick)
            .and_then(|entries| entries.get(jid))
            .filter(|muji| !muji.is_empty())
            .cloned()
    }

    /// Exact per-session Muji payloads for `nick`, sorted by full JID
    /// for deterministic replay and broadcast ordering.
    pub fn muji_sessions_for_nick(&self, nick: &str) -> Vec<(FullJid, Muji)> {
        let Some(entries) = self.muji_state.get(nick) else {
            return Vec::new();
        };
        let mut entries: Vec<_> = entries
            .iter()
            .filter(|(_, muji)| !muji.is_empty())
            .map(|(jid, muji)| (jid.clone(), muji.clone()))
            .collect();
        entries.sort_by_key(|(jid, _)| jid.to_string());
        entries
    }

    /// Clear a nickname's stored Muji advertisement if `originator`
    /// owns it.
    ///
    /// XEP-0272 §Leaving says the Muji information is removed from
    /// the participant's MUC presence when leaving the conference.
    /// A regular available presence without `<muji/>` is therefore a
    /// canonical clear signal for an occupant that previously
    /// advertised Muji state.
    pub fn clear_muji_presence(&mut self, nick: &str, originator: &FullJid) -> MujiPresenceState {
        if let Some(entries) = self.muji_state.get_mut(nick) {
            entries.remove(originator);
            if entries.is_empty() {
                self.muji_state.remove(nick);
            }
        }
        MujiPresenceState {
            sender_muji: None,
            room_muji: self.muji_for_nick(nick),
            session_mujis: self.muji_sessions_for_nick(nick),
            active_call_started: false,
        }
    }

    /// Apply an `<in-call xmlns='urn:waddle:in-call:0'>` presence state
    /// (#1029 raised hand / #1030 mute) from `nick`'s `originator`
    /// session. A non-empty state records the session's full
    /// [`InCallPresenceState`]; an empty state removes it. Mirrors
    /// [`upsert_muji_presence`](Self::upsert_muji_presence): the entry is
    /// bound to the exact emitting session so one resource can change its
    /// in-call state without disturbing a sibling's.
    pub fn upsert_in_call_state(
        &mut self,
        nick: &str,
        originator: FullJid,
        state: InCallPresenceState,
    ) {
        if state.is_empty() {
            self.clear_in_call_state(nick, &originator);
        } else {
            self.in_call_state
                .entry(nick.to_owned())
                .or_default()
                .insert(originator, state);
        }
    }

    /// Clear `originator`'s in-call advertisement for `nick`, if it owns
    /// one. Used when a session clears all its in-call sub-states, leaves
    /// the call, or leaves the room.
    pub fn clear_in_call_state(&mut self, nick: &str, originator: &FullJid) {
        if let Some(sessions) = self.in_call_state.get_mut(nick) {
            sessions.remove(originator);
            if sessions.is_empty() {
                self.in_call_state.remove(nick);
            }
        }
    }

    /// In-call presence state advertised by one exact `jid` session of
    /// `nick`, or the empty default when that session advertises none.
    pub fn in_call_state_for_session(&self, nick: &str, jid: &FullJid) -> InCallPresenceState {
        self.in_call_state
            .get(nick)
            .and_then(|sessions| sessions.get(jid))
            .copied()
            .unwrap_or_default()
    }

    /// Per-session in-call states for `nick` (only non-empty), sorted by
    /// full JID for deterministic replay and broadcast ordering.
    pub fn in_call_sessions_for_nick(&self, nick: &str) -> Vec<(FullJid, InCallPresenceState)> {
        let Some(sessions) = self.in_call_state.get(nick) else {
            return Vec::new();
        };
        let mut sessions: Vec<(FullJid, InCallPresenceState)> = sessions
            .iter()
            .filter(|(_, state)| !state.is_empty())
            .map(|(jid, state)| (jid.clone(), *state))
            .collect();
        sessions.sort_by_key(|(jid, _)| jid.to_string());
        sessions
    }

    /// Add an occupant to the room.
    ///
    /// When this is a fresh join (nickname not currently present), the
    /// per-nickname occupancy generation is bumped — used by XEP-0308
    /// to disallow corrections across leave/rejoin cycles.
    pub fn add_occupant(&mut self, occupant: Occupant) {
        self.session_watermarks
            .insert(occupant.real_jid.clone(), OccupancyWatermark::initial());
        self.session_orders.insert(
            occupant.real_jid.clone(),
            super::room_actor::next_occupancy_order(),
        );
        self.session_generations.insert(
            occupant.real_jid.clone(),
            waddle_xmpp_core::OccupancySessionGeneration::mint(),
        );
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
        if let Some(sessions) = self.occupant_sessions.remove(nick) {
            for session in sessions {
                self.session_watermarks.remove(&session);
                self.session_orders.remove(&session);
                self.session_generations.remove(&session);
            }
        }
        self.muji_state.remove(nick);
        self.in_call_state.remove(nick);
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
    ///
    /// If `muji_state[nick]` was originated by the leaving session,
    /// the entry is cleared even when other sessions for the same
    /// nick remain. This avoids ghost call advertisements when one
    /// resource of a multi-resource occupant disconnects mid-call:
    /// alice/desktop on the call, alice/mobile in the room (not on
    /// the call), desktop drops → the call advertisement must clear,
    /// not persist under mobile's session.
    /// Drop the call (XEP-0272 Muji) and in-call advertisements one session
    /// owns without removing the session itself: a same-full-JID rejoin by a
    /// DIFFERENT connection generation must not inherit the displaced
    /// connection's chip/hand/mute state (#1703).
    pub(super) fn clear_session_call_state(&mut self, nick: &str, jid: &FullJid) {
        if let Some(entries) = self.muji_state.get_mut(nick) {
            entries.remove(jid);
            if entries.is_empty() {
                self.muji_state.remove(nick);
            }
        }
        if let Some(in_call) = self.in_call_state.get_mut(nick) {
            in_call.remove(jid);
            if in_call.is_empty() {
                self.in_call_state.remove(nick);
            }
        }
    }

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

        // Clear call advertisement when the originating session leaves
        // even if peer sessions for the same nick remain. Without this
        // clause the chip would stay lit (and replay to late joiners)
        // until the user's LAST session for this nick departs.
        if let Some(entries) = self.muji_state.get_mut(nick) {
            entries.remove(jid);
            if entries.is_empty() {
                self.muji_state.remove(nick);
            }
        }
        // The in-call advertisement is session-owned just like the call
        // advertisement above; drop the leaving session's entry so a
        // lingering hand/mute isn't replayed to late joiners.
        if let Some(in_call) = self.in_call_state.get_mut(nick) {
            in_call.remove(jid);
            if in_call.is_empty() {
                self.in_call_state.remove(nick);
            }
        }
        self.session_watermarks.remove(jid);
        self.session_orders.remove(jid);
        self.session_generations.remove(jid);

        if sessions.is_empty() {
            self.occupant_sessions.remove(nick);
            self.muji_state.remove(nick);
            self.in_call_state.remove(nick);
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

    pub fn session_watermark(&self, jid: &FullJid) -> Option<OccupancyWatermark> {
        self.session_watermarks.get(jid).copied()
    }

    pub fn set_session_watermark(&mut self, jid: FullJid, watermark: OccupancyWatermark) {
        self.session_orders
            .insert(jid.clone(), super::room_actor::next_occupancy_order());
        self.session_watermarks.insert(jid, watermark);
    }

    pub fn session_order(&self, jid: &FullJid) -> Option<super::room_actor::OccupancyOrder> {
        self.session_orders.get(jid).copied()
    }

    pub fn session_generation(
        &self,
        jid: &FullJid,
    ) -> Option<waddle_xmpp_core::OccupancySessionGeneration> {
        self.session_generations.get(jid).copied()
    }

    /// Install an occupancy order captured BEFORE the join's durable
    /// projection awaited: a cleanup attempt minted while the projection was
    /// blocked must classify this session as its target, not as a newer
    /// replacement (#1647, codex round 27).
    pub fn set_session_order(&mut self, jid: &FullJid, order: super::room_actor::OccupancyOrder) {
        self.session_orders.insert(jid.clone(), order);
    }

    pub fn set_session_generation(
        &mut self,
        jid: &FullJid,
        generation: waddle_xmpp_core::OccupancySessionGeneration,
    ) {
        self.session_generations.insert(jid.clone(), generation);
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

    /// True when this room carries no in-memory state that would be
    /// lost on eviction from the room registry: zero occupants, no
    /// stored subject, no pinned entries, and no explicit
    /// affiliation grants.
    ///
    /// Resolver-derived affiliations (#1110) do NOT block dormancy:
    /// they are written by the join-path authz resolver and re-derived
    /// on the next join by construction, so dropping them with the
    /// actor loses nothing. Explicit grants (admin IQ writes, bans,
    /// the instant-room creator's Owner) are in-memory only and keep
    /// the room non-dormant.
    ///
    /// The dormancy janitor uses this predicate to safely reap
    /// persistent rooms whose `RoomActor` is holding nothing but a
    /// mailbox and an empty `MucRoom`. Eviction in this state is
    /// equivalent to never having spawned the actor: any future
    /// access will `GetOrCreateRoom` the actor afresh with identical
    /// initial state. Rooms with subject, pins, or affiliations are
    /// NOT dormant — those caches are in-memory only today and
    /// dropping them would lose user-visible state.
    pub fn is_dormant(&self) -> bool {
        // muji_state is included in the predicate so a panic-shed
        // session leaving stale call advertisements in the actor
        // can't trigger eviction while a chip is still lit. The
        // happy path keeps muji_state in lock-step with occupants
        // (removing the last session clears the entry), so the new
        // check fires only on bug paths — but the alternative is a
        // silent "in-call indicator for nobody" UX.
        self.occupants.is_empty()
            && self.subject.is_none()
            && self.pinned_entries.is_empty()
            && !self.affiliation_list.has_explicit_grants()
            && self.muji_state.is_empty()
            && self.in_call_state.is_empty()
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

#[cfg(test)]
mod in_call_state_tests {
    use super::*;
    use std::str::FromStr;

    fn room() -> MucRoom {
        MucRoom::new(
            BareJid::from_str("room@conf.example").expect("valid bare jid"),
            "wad-1".into(),
            "chan-1".into(),
            RoomConfig::default(),
        )
    }

    fn full(s: &str) -> FullJid {
        FullJid::from_str(s).expect("valid full jid")
    }

    fn raised() -> InCallPresenceState {
        InCallPresenceState {
            hand_raised: true,
            muted: false,
        }
    }

    fn muted() -> InCallPresenceState {
        InCallPresenceState {
            hand_raised: false,
            muted: true,
        }
    }

    fn nick_has_raised_hand(r: &MucRoom, nick: &str) -> bool {
        r.in_call_sessions_for_nick(nick)
            .iter()
            .any(|(_, state)| state.hand_raised)
    }

    #[test]
    fn upsert_records_and_empty_state_clears_for_a_session() {
        let mut r = room();
        let web = full("alice@example.com/web");

        r.upsert_in_call_state("alice", web.clone(), raised());
        assert!(r.in_call_state_for_session("alice", &web).hand_raised);
        assert!(nick_has_raised_hand(&r, "alice"));

        r.upsert_in_call_state("alice", web.clone(), InCallPresenceState::default());
        assert!(!r.in_call_state_for_session("alice", &web).hand_raised);
        assert!(!nick_has_raised_hand(&r, "alice"));
    }

    #[test]
    fn mute_is_carried_per_session_independently_of_the_hand() {
        let mut r = room();
        let web = full("alice@example.com/web");

        // Muted but hand down — the session entry exists and carries mute.
        r.upsert_in_call_state("alice", web.clone(), muted());
        let state = r.in_call_state_for_session("alice", &web);
        assert!(state.muted, "session advertises mute");
        assert!(!state.hand_raised, "mute does not imply a raised hand");
        assert_eq!(
            r.in_call_sessions_for_nick("alice"),
            vec![(web.clone(), muted())]
        );

        // Unmuting clears the session's entry entirely (empty state).
        r.upsert_in_call_state("alice", web.clone(), InCallPresenceState::default());
        assert!(!r.in_call_state_for_session("alice", &web).muted);
        assert!(r.in_call_sessions_for_nick("alice").is_empty());
    }

    #[test]
    fn hand_and_mute_combine_for_one_session() {
        let mut r = room();
        let web = full("alice@example.com/web");
        let both = InCallPresenceState {
            hand_raised: true,
            muted: true,
        };
        r.upsert_in_call_state("alice", web.clone(), both);
        assert_eq!(r.in_call_state_for_session("alice", &web), both);
    }

    #[test]
    fn in_call_state_aggregates_across_sessions() {
        let mut r = room();
        let web = full("alice@example.com/web");
        let mobile = full("alice@example.com/mobile");

        // Web raises; mobile explicitly empty must not contribute.
        r.upsert_in_call_state("alice", web.clone(), raised());
        r.upsert_in_call_state("alice", mobile.clone(), InCallPresenceState::default());
        assert_eq!(
            r.in_call_sessions_for_nick("alice"),
            vec![(web.clone(), raised())]
        );

        // Mobile mutes — both sessions advertised, sorted by full JID.
        r.upsert_in_call_state("alice", mobile.clone(), muted());
        assert_eq!(
            r.in_call_sessions_for_nick("alice"),
            vec![(mobile.clone(), muted()), (web.clone(), raised())]
        );

        // Clearing web leaves the nick's mobile mute advertised.
        r.clear_in_call_state("alice", &web);
        assert_eq!(
            r.in_call_sessions_for_nick("alice"),
            vec![(mobile, muted())]
        );
    }

    #[test]
    fn remove_occupant_clears_in_call_state() {
        let mut r = room();
        let web = full("alice@example.com/web");
        r.upsert_in_call_state("alice", web, raised());
        r.remove_occupant("alice");
        assert!(r.in_call_sessions_for_nick("alice").is_empty());
    }
}

#[cfg(test)]
mod allow_pm_tests {
    use super::*;

    /// XEP-0045 `muc#roomconfig_allowpm` (#1257): role-permission table.
    #[test]
    fn allow_pm_permission_table() {
        for role in [Role::Visitor, Role::Participant, Role::Moderator] {
            assert!(AllowPm::Anyone.permits(role));
            assert!(!AllowPm::None.permits(role));
        }
        assert!(!AllowPm::Participants.permits(Role::Visitor));
        assert!(AllowPm::Participants.permits(Role::Participant));
        assert!(AllowPm::Participants.permits(Role::Moderator));
        assert!(!AllowPm::Moderators.permits(Role::Visitor));
        assert!(!AllowPm::Moderators.permits(Role::Participant));
        assert!(AllowPm::Moderators.permits(Role::Moderator));
        assert!(!AllowPm::Anyone.permits(Role::None));
    }

    /// Registrar wire values round-trip; unknown values are rejected so
    /// callers keep the previous policy.
    #[test]
    fn allow_pm_form_value_round_trip() {
        for value in [
            AllowPm::Anyone,
            AllowPm::Participants,
            AllowPm::Moderators,
            AllowPm::None,
        ] {
            assert_eq!(AllowPm::from_form_value(value.as_form_value()), Some(value));
        }
        assert_eq!(AllowPm::from_form_value("bogus"), Option::None);
        assert_eq!(AllowPm::default(), AllowPm::Anyone);
    }
}
