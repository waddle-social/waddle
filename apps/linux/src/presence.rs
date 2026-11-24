//! Own presence (ADR-010), without any widgets.
//!
//! The account has one status mode, the same on every device: `Automatic`
//! lets each device's idle time decide (Away after 10 minutes without
//! input, Extended Away after 30), `Manual` pins one status until it is
//! reset. A manual pick is stored in the account's own PEP node and taken
//! up by every device, at once when online and otherwise on connect. This
//! mirrors Apple's `SessionCoordinator+Presence`; the controller returns
//! [`Effect`]s for the window to perform, so the rules are testable
//! without a connection or a clock.

use waddle_xmpp_client_ffi::{WaddleManualStatus, WaddleStatusPreference};

/// Without input for this long, the device is Away.
pub const AWAY_AFTER_MS: i64 = 10 * 60 * 1000;
/// Without input for this long, the device is Extended Away.
pub const EXTENDED_AWAY_AFTER_MS: i64 = 30 * 60 * 1000;
/// Readings of the same input differ by a hair, so only a clearly later
/// one is new input.
const SAME_INPUT_MS: i64 = 1000;

/// What the account shows (RFC 6121 §4.7.2.1 `<show/>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Show {
    Available,
    /// Free to chat.
    Chat,
    Away,
    ExtendedAway,
    DoNotDisturb,
}

impl Show {
    /// The `<show/>` value; none means available.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            Self::Available => None,
            Self::Chat => Some("chat"),
            Self::Away => Some("away"),
            Self::ExtendedAway => Some("xa"),
            Self::DoNotDisturb => Some("dnd"),
        }
    }

    /// From a received `<show/>` of an available presence.
    pub fn from_wire(show: Option<&str>) -> Self {
        match show {
            Some("chat") => Self::Chat,
            Some("away") => Self::Away,
            Some("xa") => Self::ExtendedAway,
            Some("dnd") => Self::DoNotDisturb,
            _ => Self::Available,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Available => "Online",
            Self::Chat => "Free to chat",
            Self::Away => "Away",
            Self::ExtendedAway => "Extended away",
            Self::DoNotDisturb => "Do not disturb",
        }
    }

    /// Reachable now: shown among those who are here.
    pub fn is_here(self) -> bool {
        matches!(self, Self::Available | Self::Chat | Self::DoNotDisturb)
    }
}

/// A status picked by hand. Extended Away is never picked: only a long
/// idle device reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualStatus {
    Available,
    Chat,
    Away,
    DoNotDisturb,
}

impl ManualStatus {
    pub fn show(self) -> Show {
        match self {
            Self::Available => Show::Available,
            Self::Chat => Show::Chat,
            Self::Away => Show::Away,
            Self::DoNotDisturb => Show::DoNotDisturb,
        }
    }
}

/// How the account's presence is decided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PresenceMode {
    #[default]
    Automatic,
    Manual(ManualStatus),
}

impl PresenceMode {
    pub fn from_wire(preference: WaddleStatusPreference) -> Self {
        match preference {
            WaddleStatusPreference::Automatic => Self::Automatic,
            WaddleStatusPreference::Manual { status } => Self::Manual(match status {
                WaddleManualStatus::Available => ManualStatus::Available,
                WaddleManualStatus::Chat => ManualStatus::Chat,
                WaddleManualStatus::Away => ManualStatus::Away,
                WaddleManualStatus::Dnd => ManualStatus::DoNotDisturb,
            }),
        }
    }

    pub fn to_wire(self) -> WaddleStatusPreference {
        match self {
            Self::Automatic => WaddleStatusPreference::Automatic,
            Self::Manual(status) => WaddleStatusPreference::Manual {
                status: match status {
                    ManualStatus::Available => WaddleManualStatus::Available,
                    ManualStatus::Chat => WaddleManualStatus::Chat,
                    ManualStatus::Away => WaddleManualStatus::Away,
                    ManualStatus::DoNotDisturb => WaddleManualStatus::Dnd,
                },
            },
        }
    }

    /// The picker's rows, in order. "Free to chat" picked on another
    /// client shows as Online.
    pub const PICKER: [(&'static str, Self); 4] = [
        ("Automatic", Self::Automatic),
        ("Online", Self::Manual(ManualStatus::Available)),
        ("Away", Self::Manual(ManualStatus::Away)),
        ("Do not disturb", Self::Manual(ManualStatus::DoNotDisturb)),
    ];

    pub fn picker_index(self) -> u32 {
        match self {
            Self::Automatic => 0,
            Self::Manual(ManualStatus::Available | ManualStatus::Chat) => 1,
            Self::Manual(ManualStatus::Away) => 2,
            Self::Manual(ManualStatus::DoNotDisturb) => 3,
        }
    }

    pub fn from_picker_index(index: u32) -> Option<Self> {
        Self::PICKER
            .get(usize::try_from(index).ok()?)
            .map(|(_, mode)| *mode)
    }
}

/// The presence this device sends for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnPresence {
    pub show: Show,
    /// XEP-0319: when input stopped, in UTC milliseconds.
    pub idle_since_ms: Option<i64>,
    /// The RFC 6121 status message.
    pub status: Option<String>,
}

impl OwnPresence {
    /// XEP-0319 `<idle since/>` as an `xs:dateTime`.
    pub fn idle_since(&self) -> Option<String> {
        let millis = self.idle_since_ms?;
        chrono::DateTime::from_timestamp_millis(millis)
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }
}

/// What this device's idle time says its presence is. The stamp is when
/// input stopped, never now.
pub fn auto_away(now_ms: i64, last_active_ms: i64) -> (Show, Option<i64>) {
    let idle_for = now_ms.saturating_sub(last_active_ms);
    if idle_for >= EXTENDED_AWAY_AFTER_MS {
        (Show::ExtendedAway, Some(last_active_ms))
    } else if idle_for >= AWAY_AFTER_MS {
        (Show::Away, Some(last_active_ms))
    } else {
        (Show::Available, None)
    }
}

/// What the window should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send this presence.
    Announce(OwnPresence),
    /// Store this mode on the account; report back with
    /// [`Controller::published`].
    Publish(PresenceMode),
    /// Read the account's stored mode; report back with
    /// [`Controller::fetched`].
    Fetch(FetchToken),
}

/// Identifies a stored-mode read, so a pick made meanwhile wins over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchToken {
    generation: u64,
    mode: PresenceMode,
}

#[derive(Debug, Default)]
pub struct Controller {
    mode: PresenceMode,
    status_text: Option<String>,
    last_active_ms: Option<i64>,
    /// A picked status held auto-away off: idle time counts from when
    /// that ends, not from the input before it.
    restart_when_automatic: bool,
    online: bool,
    sent: Option<OwnPresence>,
    /// What shows now, as of the last reading.
    shown: Option<Show>,
    /// The mode the server is known to hold.
    stored: Option<PresenceMode>,
    publishing: Option<PresenceMode>,
    needs_another_pass: bool,
    /// Another device's pick that arrived while ours was on its way.
    heard_while_publishing: Option<PresenceMode>,
    /// Moves when another device changes `stored`, so a read or a publish
    /// under way then does not put an older value back.
    generation: u64,
}

impl Controller {
    pub fn mode(&self) -> PresenceMode {
        self.mode
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status_text.as_deref()
    }

    /// What this device shows now (as of the last reading).
    pub fn shown(&self) -> Show {
        self.shown.unwrap_or(Show::Available)
    }

    /// The user did something on this device. Reports may come late and
    /// out of order: only a later one counts.
    pub fn note_input(&mut self, at_ms: i64) {
        if self
            .last_active_ms
            .is_none_or(|last| at_ms > last.saturating_add(SAME_INPUT_MS))
        {
            self.last_active_ms = Some(at_ms);
        }
    }

    /// Work out what this device should show now and announce it if that
    /// changed. The window calls it on a timer and after input.
    pub fn refresh(&mut self, now_ms: i64) -> Vec<Effect> {
        let (show, idle_since_ms) = self.read_idle_clock(now_ms);
        self.shown = Some(show);
        let next = OwnPresence {
            show,
            idle_since_ms,
            status: self.status_text.clone(),
        };
        if !self.online || self.sent.as_ref() == Some(&next) {
            return Vec::new();
        }
        self.sent = Some(next.clone());
        vec![Effect::Announce(next)]
    }

    /// Pick a status for the account, or `Automatic`. Shown at once;
    /// stored when online, or on the next connect.
    pub fn set_mode(&mut self, mode: PresenceMode, now_ms: i64) -> Vec<Effect> {
        self.mode = mode;
        let mut effects = self.refresh(now_ms);
        effects.extend(self.store());
        effects
    }

    /// The RFC 6121 status message; blank removes it.
    pub fn set_status_text(&mut self, text: &str, now_ms: i64) -> Vec<Effect> {
        let trimmed = text.trim();
        self.status_text = (!trimmed.is_empty()).then(|| trimmed.to_string());
        self.refresh(now_ms)
    }

    /// The connection came up: announce, then store a pick made offline
    /// or take up what the account has stored.
    pub fn connected(&mut self, now_ms: i64) -> Vec<Effect> {
        self.online = true;
        self.sent = None;
        let mut effects = self.refresh(now_ms);
        if self.has_unstored_pick() {
            effects.extend(self.store());
        } else {
            effects.push(Effect::Fetch(self.fetch_token()));
        }
        effects
    }

    pub fn disconnected(&mut self) {
        self.online = false;
        self.sent = None;
        // A publish under way will not be answered; the next connect
        // stores the pick again.
        self.publishing = None;
        self.needs_another_pass = false;
        self.heard_while_publishing = None;
    }

    /// The account's stored mode was read (`None`: it could not be).
    pub fn fetched(
        &mut self,
        token: FetchToken,
        stored: Option<PresenceMode>,
        now_ms: i64,
    ) -> Vec<Effect> {
        if !self.online {
            return Vec::new();
        }
        // A pick here or on another device arrived while reading: it is
        // newer than what was read.
        if self.mode != token.mode || self.generation != token.generation {
            return self.store();
        }
        let Some(stored) = stored else {
            return Vec::new();
        };
        self.stored = Some(stored);
        self.mode = stored;
        self.refresh(now_ms)
    }

    /// A store started by [`Effect::Publish`] ended; `mode` is what it
    /// stored.
    pub fn published(&mut self, mode: PresenceMode, accepted: bool, now_ms: i64) -> Vec<Effect> {
        self.publishing = None;
        let heard = self.heard_while_publishing.take();
        let mut effects = Vec::new();
        if accepted {
            self.stored = Some(mode);
        } else if let Some(heard) = heard {
            self.stored = Some(heard);
            if self.mode == mode {
                self.mode = heard;
                effects.extend(self.refresh(now_ms));
            }
        }
        let again = self.needs_another_pass || (accepted && self.mode != mode);
        self.needs_another_pass = false;
        if again {
            effects.extend(self.store());
        }
        effects
    }

    /// Another device of the account picked `mode`. Taken up here without
    /// storing it again, or two devices would answer each other forever.
    pub fn heard(&mut self, mode: PresenceMode, now_ms: i64) -> Vec<Effect> {
        if self.publishing.is_some() {
            self.heard_while_publishing = Some(mode);
            return Vec::new();
        }
        self.generation += 1;
        self.stored = Some(mode);
        if self.mode == mode {
            return Vec::new();
        }
        self.mode = mode;
        self.refresh(now_ms)
    }

    /// Forget this account's state on sign-out. The pick stays on the
    /// server for the other devices.
    pub fn reset(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    fn fetch_token(&self) -> FetchToken {
        FetchToken {
            generation: self.generation,
            mode: self.mode,
        }
    }

    fn read_idle_clock(&mut self, now_ms: i64) -> (Show, Option<i64>) {
        if let PresenceMode::Manual(status) = self.mode {
            self.restart_when_automatic = true;
            return (status.show(), None);
        }
        let mut last = self.last_active_ms.unwrap_or(now_ms);
        if self.restart_when_automatic {
            last = now_ms;
            self.restart_when_automatic = false;
        }
        self.last_active_ms = Some(last);
        auto_away(now_ms, last)
    }

    /// A pick the server does not hold yet. Before anything is known of
    /// the server, only a picked status counts: a new device is Automatic
    /// by default and must not write that over the account's pick.
    fn has_unstored_pick(&self) -> bool {
        match self.stored {
            Some(stored) => self.mode != stored,
            None => self.mode != PresenceMode::Automatic,
        }
    }

    /// Store the picked mode unless the server already holds it. A pick
    /// made while another is on its way is stored after it, never dropped.
    fn store(&mut self) -> Vec<Effect> {
        if !self.online {
            return Vec::new();
        }
        if self.publishing.is_some() {
            self.needs_another_pass = true;
            return Vec::new();
        }
        if self.stored == Some(self.mode) {
            return Vec::new();
        }
        self.publishing = Some(self.mode);
        vec![Effect::Publish(self.mode)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60 * 1000;

    fn announced(effects: &[Effect]) -> Vec<&OwnPresence> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Announce(presence) => Some(presence),
                _ => None,
            })
            .collect()
    }

    fn published(effects: &[Effect]) -> Vec<PresenceMode> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Publish(mode) => Some(*mode),
                _ => None,
            })
            .collect()
    }

    fn fetch(effects: &[Effect]) -> Option<FetchToken> {
        effects.iter().find_map(|effect| match effect {
            Effect::Fetch(token) => Some(*token),
            _ => None,
        })
    }

    const AWAY: PresenceMode = PresenceMode::Manual(ManualStatus::Away);
    const DND: PresenceMode = PresenceMode::Manual(ManualStatus::DoNotDisturb);

    fn connected() -> Controller {
        let mut controller = Controller::default();
        controller.note_input(0);
        controller.connected(0);
        controller
    }

    #[test]
    fn idle_time_picks_away_then_extended_away_stamped_with_last_input() {
        assert_eq!(auto_away(9 * MIN, 0), (Show::Available, None));
        assert_eq!(auto_away(10 * MIN, 0), (Show::Away, Some(0)));
        assert_eq!(auto_away(29 * MIN, 0), (Show::Away, Some(0)));
        assert_eq!(auto_away(30 * MIN, 0), (Show::ExtendedAway, Some(0)));
    }

    #[test]
    fn a_device_with_nothing_stored_announces_and_reads_the_account() {
        let mut controller = Controller::default();
        controller.note_input(0);
        let effects = controller.connected(0);
        assert_eq!(announced(&effects)[0].show, Show::Available);
        assert!(published(&effects).is_empty());
        assert!(fetch(&effects).is_some());
    }

    #[test]
    fn a_new_device_takes_the_status_picked_elsewhere() {
        let mut controller = Controller::default();
        controller.note_input(0);
        let token = fetch(&controller.connected(0)).expect("fetch");
        let effects = controller.fetched(token, Some(DND), 0);
        assert_eq!(controller.mode(), DND);
        assert_eq!(announced(&effects)[0].show, Show::DoNotDisturb);
        assert!(published(&effects).is_empty());
    }

    #[test]
    fn a_stored_pick_that_cannot_be_read_leaves_the_device_as_it_was() {
        let mut controller = Controller::default();
        let token = fetch(&controller.connected(0)).expect("fetch");
        assert!(controller.fetched(token, None, 0).is_empty());
        assert_eq!(controller.mode(), PresenceMode::Automatic);
    }

    #[test]
    fn a_pick_is_shown_and_stored_for_the_other_devices() {
        let mut controller = connected();
        let effects = controller.set_mode(AWAY, 1000);
        assert_eq!(announced(&effects)[0].show, Show::Away);
        assert_eq!(announced(&effects)[0].idle_since_ms, None);
        assert_eq!(published(&effects), vec![AWAY]);
    }

    #[test]
    fn resetting_to_automatic_is_stored_too() {
        let mut controller = Controller::default();
        let token = fetch(&controller.connected(0)).expect("fetch");
        controller.fetched(token, Some(DND), 0);
        let effects = controller.set_mode(PresenceMode::Automatic, 1000);
        assert_eq!(announced(&effects)[0].show, Show::Available);
        assert_eq!(published(&effects), vec![PresenceMode::Automatic]);
    }

    #[test]
    fn picking_the_status_already_held_stores_nothing() {
        let mut controller = Controller::default();
        let token = fetch(&controller.connected(0)).expect("fetch");
        controller.fetched(token, Some(AWAY), 0);
        assert!(controller.set_mode(AWAY, 1000).is_empty());
    }

    #[test]
    fn a_status_picked_offline_wins_over_the_stored_one_on_connect() {
        let mut controller = Controller::default();
        controller.set_mode(DND, 0);
        let effects = controller.connected(0);
        assert_eq!(announced(&effects)[0].show, Show::DoNotDisturb);
        assert_eq!(published(&effects), vec![DND]);
        assert!(fetch(&effects).is_none());
    }

    #[test]
    fn a_pick_the_server_did_not_take_is_stored_on_the_next_connect() {
        let mut controller = connected();
        controller.set_mode(AWAY, 0);
        assert!(controller.published(AWAY, false, 0).is_empty());
        controller.disconnected();
        let effects = controller.connected(0);
        assert_eq!(published(&effects), vec![AWAY]);
    }

    #[test]
    fn a_pick_made_while_another_is_being_stored_is_stored_after_it() {
        let mut controller = connected();
        assert_eq!(published(&controller.set_mode(AWAY, 0)), vec![AWAY]);
        assert!(published(&controller.set_mode(DND, 0)).is_empty());
        let effects = controller.published(AWAY, true, 0);
        assert_eq!(published(&effects), vec![DND]);
        assert!(controller.published(DND, true, 0).is_empty());
    }

    #[test]
    fn another_devices_pick_is_taken_up_without_being_stored_again() {
        let mut controller = connected();
        let effects = controller.heard(DND, 0);
        assert_eq!(controller.mode(), DND);
        assert_eq!(announced(&effects)[0].show, Show::DoNotDisturb);
        assert!(published(&effects).is_empty());
        // Hearing the same pick again changes nothing.
        assert!(controller.heard(DND, 0).is_empty());
    }

    #[test]
    fn a_pick_heard_while_storing_stands_if_ours_was_not_taken() {
        let mut controller = connected();
        controller.set_mode(AWAY, 0);
        assert!(controller.heard(DND, 0).is_empty());
        let effects = controller.published(AWAY, false, 0);
        assert_eq!(controller.mode(), DND);
        assert_eq!(announced(&effects)[0].show, Show::DoNotDisturb);
    }

    #[test]
    fn a_pick_made_while_reading_wins_over_what_was_read() {
        let mut controller = Controller::default();
        let token = fetch(&controller.connected(0)).expect("fetch");
        let effects = controller.set_mode(AWAY, 0);
        assert_eq!(published(&effects), vec![AWAY]);
        controller.fetched(token, Some(DND), 0);
        assert_eq!(controller.mode(), AWAY);
    }

    #[test]
    fn nothing_is_sent_while_offline_and_unchanged_presence_is_not_repeated() {
        let mut controller = Controller::default();
        controller.note_input(0);
        assert!(controller.refresh(0).is_empty());
        let mut controller = connected();
        assert!(controller.refresh(MIN).is_empty());
    }

    #[test]
    fn auto_away_announces_away_then_extended_away_and_input_brings_it_back() {
        let mut controller = connected();
        let away = controller.refresh(10 * MIN);
        assert_eq!(announced(&away)[0].show, Show::Away);
        assert_eq!(announced(&away)[0].idle_since_ms, Some(0));
        assert!(controller.refresh(11 * MIN).is_empty());
        let extended = controller.refresh(30 * MIN);
        assert_eq!(announced(&extended)[0].show, Show::ExtendedAway);
        controller.note_input(31 * MIN);
        let back = controller.refresh(31 * MIN);
        assert_eq!(announced(&back)[0].show, Show::Available);
        assert_eq!(announced(&back)[0].idle_since_ms, None);
    }

    #[test]
    fn a_picked_status_holds_auto_away_off_and_idle_counts_from_when_it_ends() {
        let mut controller = connected();
        controller.set_mode(DND, 5 * MIN);
        // Long idle while picked: still Do not disturb.
        let held = controller.refresh(60 * MIN);
        assert!(held.is_empty());
        let effects = controller.set_mode(PresenceMode::Automatic, 60 * MIN);
        assert_eq!(announced(&effects)[0].show, Show::Available);
        assert!(controller.refresh(69 * MIN).is_empty());
        let away = controller.refresh(70 * MIN);
        assert_eq!(announced(&away)[0].idle_since_ms, Some(60 * MIN));
    }

    #[test]
    fn only_clearly_later_input_counts() {
        let mut controller = Controller::default();
        controller.note_input(10 * MIN);
        controller.note_input(5 * MIN);
        controller.note_input(10 * MIN + 500);
        controller.connected(10 * MIN + 500);
        assert_eq!(auto_away(20 * MIN, 10 * MIN).0, Show::Away);
        assert!(controller.refresh(19 * MIN).is_empty());
        assert_eq!(announced(&controller.refresh(20 * MIN))[0].show, Show::Away);
    }

    #[test]
    fn the_status_message_is_trimmed_and_blank_removes_it() {
        let mut controller = connected();
        let effects = controller.set_status_text("  heads down  ", 0);
        assert_eq!(announced(&effects)[0].status.as_deref(), Some("heads down"));
        let effects = controller.set_status_text("   ", 0);
        assert_eq!(announced(&effects)[0].status, None);
    }

    #[test]
    fn the_idle_stamp_is_utc_seconds() {
        let presence = OwnPresence {
            show: Show::Away,
            idle_since_ms: Some(1_700_000_000_000),
            status: None,
        };
        assert_eq!(
            presence.idle_since().as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }

    #[test]
    fn wire_shows_round_trip() {
        for show in [
            Show::Available,
            Show::Chat,
            Show::Away,
            Show::ExtendedAway,
            Show::DoNotDisturb,
        ] {
            assert_eq!(Show::from_wire(show.wire()), show);
        }
        assert_eq!(Show::from_wire(Some("unknown")), Show::Available);
        assert!(Show::DoNotDisturb.is_here() && !Show::Away.is_here());
    }

    #[test]
    fn the_picker_maps_modes_both_ways_and_shows_free_to_chat_as_online() {
        for (index, (_, mode)) in PresenceMode::PICKER.iter().enumerate() {
            let index = u32::try_from(index).expect("small");
            assert_eq!(mode.picker_index(), index);
            assert_eq!(PresenceMode::from_picker_index(index), Some(*mode));
        }
        assert_eq!(
            PresenceMode::Manual(ManualStatus::Chat).picker_index(),
            PresenceMode::Manual(ManualStatus::Available).picker_index()
        );
        assert_eq!(PresenceMode::from_picker_index(9), None);
    }

    #[test]
    fn modes_round_trip_through_the_wire_preference() {
        for (_, mode) in PresenceMode::PICKER {
            assert_eq!(PresenceMode::from_wire(mode.to_wire()), mode);
        }
        let chat = PresenceMode::Manual(ManualStatus::Chat);
        assert_eq!(PresenceMode::from_wire(chat.to_wire()), chat);
    }

    #[test]
    fn reset_forgets_the_account_but_not_that_it_happened() {
        let mut controller = connected();
        controller.set_mode(AWAY, 0);
        controller.reset();
        assert_eq!(controller.mode(), PresenceMode::Automatic);
        assert!(controller.refresh(0).is_empty());
    }
}
