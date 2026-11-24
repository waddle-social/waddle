//! Per-conversation notification settings (XEP-0492) and the decision
//! whether a message earns a desktop notification.
//!
//! The wire work is the `WaddleClient` bookmark verbs; this module holds the
//! state they produce and the rules for using it, so none of it lives in a
//! widget. Rooms store their mode in the account's XEP-0402 bookmark and DMs
//! in the Waddle DM-bookmark node, which is sparse: a DM with no stored mode
//! uses the XEP-0492 section 3 default.

use std::collections::{HashMap, HashSet};

use waddle_xmpp_client_ffi::{
    WaddleBookmarkItem, WaddleDmBookmarkItem, WaddleNotifyMode, WaddleSetDmNotificationModeOutcome,
    WaddleSetRoomNotificationModeOutcome,
};

use crate::session::Kind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyMode {
    /// Every message.
    Always,
    /// Only messages that mention this account or the room.
    OnMention,
    /// Nothing: the conversation is muted.
    Never,
}

impl NotifyMode {
    pub const ALL: [Self; 3] = [Self::Always, Self::OnMention, Self::Never];

    /// The id used as the `win.notify-mode` action's target.
    pub fn id(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::OnMention => "on-mention",
            Self::Never => "never",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.id() == id)
    }

    pub fn ffi(self) -> WaddleNotifyMode {
        match self {
            Self::Always => WaddleNotifyMode::Always,
            Self::OnMention => WaddleNotifyMode::OnMention,
            Self::Never => WaddleNotifyMode::Never,
        }
    }

    fn from_ffi(mode: WaddleNotifyMode) -> Self {
        match mode {
            WaddleNotifyMode::Always => Self::Always,
            WaddleNotifyMode::OnMention => Self::OnMention,
            WaddleNotifyMode::Never => Self::Never,
        }
    }

    /// The icon of the header's notification button.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Never => "notifications-disabled-symbolic",
            _ => "preferences-system-notifications-symbolic",
        }
    }
}

/// Whether a message in a conversation with this mode is announced.
pub fn should_notify(mode: NotifyMode, mentions_me: bool) -> bool {
    match mode {
        NotifyMode::Always => true,
        NotifyMode::OnMention => mentions_me,
        NotifyMode::Never => false,
    }
}

/// What the account stores for one conversation. `mode` is `None` when the
/// conversation uses the default; `rich` is the Waddle push-summary opt-in,
/// which a mode change must send back unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stored {
    pub mode: Option<NotifyMode>,
    pub rich: bool,
}

/// All notification state of the signed-in window.
#[derive(Debug, Default)]
pub struct Notifications {
    rooms: HashMap<String, Stored>,
    directs: HashMap<String, Stored>,
    /// Rooms whose discovery says members-only: they default to `Always`.
    members_only: HashSet<String>,
    /// Conversation keys with a desktop notification on screen.
    shown: HashSet<String>,
}

impl Notifications {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Replace everything the account stores, after a connect.
    pub fn load(
        &mut self,
        bookmarks: &[WaddleBookmarkItem],
        directs: &[WaddleDmBookmarkItem],
        members_only: HashSet<String>,
    ) {
        self.rooms = bookmarks
            .iter()
            .map(|item| (item.jid.clone(), stored_from_room(item)))
            .collect();
        self.directs = directs
            .iter()
            .map(|item| (item.jid.clone(), stored_from_dm(item)))
            .collect();
        self.members_only = members_only;
    }

    fn entries(&self, kind: Kind) -> &HashMap<String, Stored> {
        match kind {
            Kind::Room => &self.rooms,
            Kind::Direct => &self.directs,
        }
    }

    pub fn stored(&self, kind: Kind, jid: &str) -> Stored {
        self.entries(kind).get(jid).copied().unwrap_or_default()
    }

    /// XEP-0492 section 3: chats notify for everything, and so do
    /// members-only rooms; other rooms only for mentions.
    pub fn default_mode(&self, kind: Kind, jid: &str) -> NotifyMode {
        match kind {
            Kind::Direct => NotifyMode::Always,
            Kind::Room if self.members_only.contains(jid) => NotifyMode::Always,
            Kind::Room => NotifyMode::OnMention,
        }
    }

    pub fn mode(&self, kind: Kind, jid: &str) -> NotifyMode {
        self.stored(kind, jid)
            .mode
            .unwrap_or_else(|| self.default_mode(kind, jid))
    }

    pub fn is_muted(&self, kind: Kind, jid: &str) -> bool {
        self.mode(kind, jid) == NotifyMode::Never
    }

    pub fn wants(&self, kind: Kind, jid: &str, mentions_me: bool) -> bool {
        should_notify(self.mode(kind, jid), mentions_me)
    }

    /// Remember what the server confirmed for a conversation.
    pub fn record(&mut self, kind: Kind, jid: &str, stored: Stored) {
        let entries = match kind {
            Kind::Room => &mut self.rooms,
            Kind::Direct => &mut self.directs,
        };
        entries.insert(jid.to_string(), stored);
    }

    pub fn shown(&mut self, key: &str) {
        self.shown.insert(key.to_string());
    }

    pub fn dismissed(&mut self, key: &str) {
        self.shown.remove(key);
    }

    /// Shown notifications whose conversation is no longer unread (read
    /// here, or on another device). They are forgotten as they are returned.
    pub fn take_read(&mut self, has_unread: impl Fn(&str) -> bool) -> Vec<String> {
        let read: Vec<String> = self
            .shown
            .iter()
            .filter(|key| !has_unread(key))
            .cloned()
            .collect();
        for key in &read {
            self.shown.remove(key);
        }
        read
    }
}

fn stored_from_room(item: &WaddleBookmarkItem) -> Stored {
    Stored {
        mode: item.notify_mode.map(NotifyMode::from_ffi),
        rich: item.rich_payload_opt_in,
    }
}

fn stored_from_dm(item: &WaddleDmBookmarkItem) -> Stored {
    Stored {
        mode: item.notify_mode.map(NotifyMode::from_ffi),
        rich: item.rich_payload_opt_in,
    }
}

/// What a confirmed room change leaves stored, or `None` if it failed.
pub fn room_outcome(outcome: WaddleSetRoomNotificationModeOutcome) -> Option<Stored> {
    match outcome {
        WaddleSetRoomNotificationModeOutcome::Ok { item } => Some(stored_from_room(&item)),
        _ => None,
    }
}

/// What a confirmed DM change leaves stored, or `None` if it failed. A DM
/// returned to its default has its item retracted, which stores nothing.
pub fn dm_outcome(outcome: WaddleSetDmNotificationModeOutcome) -> Option<Stored> {
    match outcome {
        WaddleSetDmNotificationModeOutcome::Ok { item } => Some(stored_from_dm(&item)),
        WaddleSetDmNotificationModeOutcome::Removed { .. } => Some(Stored::default()),
        _ => None,
    }
}

/// The notification's title: the sender for a DM, sender and room for a room.
pub fn title(kind: Kind, sender: &str, room: &str) -> String {
    match kind {
        Kind::Direct => sender.to_string(),
        Kind::Room => format!("{sender} in #{room}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room_item(jid: &str, mode: Option<WaddleNotifyMode>, rich: bool) -> WaddleBookmarkItem {
        WaddleBookmarkItem {
            jid: jid.into(),
            name: None,
            autojoin: true,
            notify_mode: mode,
            rich_payload_opt_in: rich,
        }
    }

    #[test]
    fn the_mode_decides_what_is_announced() {
        assert!(should_notify(NotifyMode::Always, false));
        assert!(should_notify(NotifyMode::Always, true));
        assert!(!should_notify(NotifyMode::OnMention, false));
        assert!(should_notify(NotifyMode::OnMention, true));
        assert!(!should_notify(NotifyMode::Never, false));
        assert!(!should_notify(NotifyMode::Never, true));
    }

    #[test]
    fn ids_round_trip() {
        for mode in NotifyMode::ALL {
            assert_eq!(NotifyMode::from_id(mode.id()), Some(mode));
            assert_eq!(NotifyMode::from_ffi(mode.ffi()), mode);
        }
        assert_eq!(NotifyMode::from_id("loud"), None);
    }

    #[test]
    fn defaults_follow_xep_0492_section_3() {
        let mut notifications = Notifications::default();
        notifications.load(&[], &[], HashSet::from(["private@rooms".to_string()]));
        assert_eq!(
            notifications.mode(Kind::Direct, "ann@example.com"),
            NotifyMode::Always
        );
        assert_eq!(
            notifications.mode(Kind::Room, "open@rooms"),
            NotifyMode::OnMention
        );
        assert_eq!(
            notifications.mode(Kind::Room, "private@rooms"),
            NotifyMode::Always
        );
        assert!(notifications.wants(Kind::Direct, "ann@example.com", false));
        assert!(!notifications.wants(Kind::Room, "open@rooms", false));
        assert!(notifications.wants(Kind::Room, "open@rooms", true));
    }

    #[test]
    fn stored_modes_beat_defaults_and_mute_wins_over_mentions() {
        let mut notifications = Notifications::default();
        notifications.load(
            &[
                room_item("open@rooms", Some(WaddleNotifyMode::Never), true),
                room_item("quiet@rooms", Some(WaddleNotifyMode::Always), false),
                room_item("plain@rooms", None, false),
            ],
            &[WaddleDmBookmarkItem {
                jid: "bob@example.com".into(),
                notify_mode: Some(WaddleNotifyMode::OnMention),
                rich_payload_opt_in: false,
            }],
            HashSet::new(),
        );
        assert!(notifications.is_muted(Kind::Room, "open@rooms"));
        assert!(!notifications.wants(Kind::Room, "open@rooms", true));
        assert!(notifications.stored(Kind::Room, "open@rooms").rich);
        assert!(notifications.wants(Kind::Room, "quiet@rooms", false));
        assert_eq!(
            notifications.mode(Kind::Room, "plain@rooms"),
            NotifyMode::OnMention
        );
        assert!(!notifications.wants(Kind::Direct, "bob@example.com", false));
        assert!(notifications.wants(Kind::Direct, "bob@example.com", true));
        // The same JID in the other carrier is a different conversation.
        assert!(!notifications.is_muted(Kind::Direct, "open@rooms"));
    }

    #[test]
    fn outcomes_say_what_is_now_stored() {
        let item = room_item("open@rooms", Some(WaddleNotifyMode::Never), true);
        assert_eq!(
            room_outcome(WaddleSetRoomNotificationModeOutcome::Ok { item }),
            Some(Stored {
                mode: Some(NotifyMode::Never),
                rich: true
            })
        );
        assert_eq!(
            room_outcome(WaddleSetRoomNotificationModeOutcome::NodeConfigMismatch),
            None
        );
        assert_eq!(
            room_outcome(WaddleSetRoomNotificationModeOutcome::Error),
            None
        );
        assert_eq!(
            dm_outcome(WaddleSetDmNotificationModeOutcome::Removed {
                jid: "bob@example.com".into()
            }),
            Some(Stored::default())
        );
        assert_eq!(dm_outcome(WaddleSetDmNotificationModeOutcome::Error), None);
    }

    #[test]
    fn notifications_are_withdrawn_once_their_conversation_is_read() {
        let mut notifications = Notifications::default();
        notifications.shown("room@rooms");
        notifications.shown("ann@example.com");
        let read = notifications.take_read(|key| key == "ann@example.com");
        assert_eq!(read, ["room@rooms"]);
        // Withdrawn ones are forgotten; the unread one stays.
        assert!(notifications.take_read(|_| true).is_empty());
        notifications.dismissed("ann@example.com");
        assert!(notifications.take_read(|_| false).is_empty());
    }

    #[test]
    fn titles_name_the_room_only_for_rooms() {
        assert_eq!(title(Kind::Direct, "ann", "x"), "ann");
        assert_eq!(title(Kind::Room, "ann", "lobby"), "ann in #lobby");
    }
}
