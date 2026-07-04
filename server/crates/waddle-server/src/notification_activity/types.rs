//! Typed activity model: chat-state and presence tokens with closed-set
//! db codecs, the activity row, and the reader trait.

use super::*;

/// XEP-0085 chat-state token persisted on
/// `notification_activity.last_chat_state`.
///
/// Closed set — one variant per XEP-0085 state. Mirrors
/// [`waddle_xmpp::xep::xep0085::ChatState`] but is owned by the server
/// crate so the audit shape doesn't depend on the upstream
/// notification-shape changes; conversion is provided in both
/// directions via [`NotificationChatState::from_xep0085`] and
/// [`NotificationChatState::to_xep0085`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationChatState {
    Active,
    Composing,
    Paused,
    Inactive,
    Gone,
}

impl NotificationChatState {
    /// Every variant of the closed set, in declaration order. Exposed
    /// so the startup invariant traversal and the round-trip test can
    /// iterate the typed values without re-declaring them.
    pub const ALL: &'static [NotificationChatState] = &[
        Self::Active,
        Self::Composing,
        Self::Paused,
        Self::Inactive,
        Self::Gone,
    ];

    pub(crate) fn as_db_value(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Composing => "composing",
            Self::Paused => "paused",
            Self::Inactive => "inactive",
            Self::Gone => "gone",
        }
    }

    pub(crate) fn from_db_value(value: &str) -> Result<Self, NotificationActivityError> {
        Self::ALL
            .iter()
            .copied()
            .find(|variant| variant.as_db_value() == value)
            .ok_or_else(|| NotificationActivityError::InvalidChatState(value.to_string()))
    }

    /// Convert from the canonical XEP-0085 typed shape.
    pub fn from_xep0085(state: waddle_xmpp::xep::xep0085::ChatState) -> Self {
        use waddle_xmpp::xep::xep0085::ChatState;
        match state {
            ChatState::Active => Self::Active,
            ChatState::Composing => Self::Composing,
            ChatState::Paused => Self::Paused,
            ChatState::Inactive => Self::Inactive,
            ChatState::Gone => Self::Gone,
        }
    }

    /// Convert into the canonical XEP-0085 typed shape.
    pub fn to_xep0085(self) -> waddle_xmpp::xep::xep0085::ChatState {
        use waddle_xmpp::xep::xep0085::ChatState;
        match self {
            Self::Active => ChatState::Active,
            Self::Composing => ChatState::Composing,
            Self::Paused => ChatState::Paused,
            Self::Inactive => ChatState::Inactive,
            Self::Gone => ChatState::Gone,
        }
    }
}

/// XEP-0045 / RFC 6121 §4.7.2.1 `<show/>` token persisted on
/// `notification_activity.presence_show`.
///
/// Closed set — one variant per RFC 6121 `<show/>` value. Mirrors
/// [`xmpp_parsers::presence::Show`] but is owned by the server crate
/// so the audit shape doesn't depend on upstream parser changes;
/// conversion is provided in both directions via
/// [`NotificationPresenceShow::from_xep0045`] and
/// [`NotificationPresenceShow::to_xep0045`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationPresenceShow {
    Away,
    Chat,
    Dnd,
    Xa,
}

impl NotificationPresenceShow {
    /// Every variant of the closed set, in declaration order. Exposed
    /// so the startup invariant traversal and the round-trip test can
    /// iterate the typed values without re-declaring them.
    pub const ALL: &'static [NotificationPresenceShow] =
        &[Self::Away, Self::Chat, Self::Dnd, Self::Xa];

    pub(crate) fn as_db_value(self) -> &'static str {
        match self {
            Self::Away => "away",
            Self::Chat => "chat",
            Self::Dnd => "dnd",
            Self::Xa => "xa",
        }
    }

    pub(crate) fn from_db_value(value: &str) -> Result<Self, NotificationActivityError> {
        Self::ALL
            .iter()
            .copied()
            .find(|variant| variant.as_db_value() == value)
            .ok_or_else(|| NotificationActivityError::InvalidPresenceShow(value.to_string()))
    }

    /// Convert from the canonical XEP-0045 / RFC 6121 typed shape.
    pub fn from_xep0045(show: xmpp_parsers::presence::Show) -> Self {
        use xmpp_parsers::presence::Show;
        match show {
            Show::Away => Self::Away,
            Show::Chat => Self::Chat,
            Show::Dnd => Self::Dnd,
            Show::Xa => Self::Xa,
        }
    }

    /// Convert into the canonical XEP-0045 / RFC 6121 typed shape.
    pub fn to_xep0045(self) -> xmpp_parsers::presence::Show {
        use xmpp_parsers::presence::Show;
        match self {
            Self::Away => Show::Away,
            Self::Chat => Show::Chat,
            Self::Dnd => Show::Dnd,
            Self::Xa => Show::Xa,
        }
    }
}

/// Aggregated activity snapshot for a single (owner, conversation)
/// row in `notification_activity`.
///
/// All timestamps are wall-clock Unix-millis (`crate::time::now_ms`).
/// `last_active_at_ms` is the maximum of every typed signal so the
/// XEP-0513 `<active/>` filter at T1 can compare it against the
/// configured TTL window. Per-source columns (`last_chat_state`,
/// `last_read_at_ms`, `presence_show`) are preserved so future slices
/// (presence-aware DnD, typing-aware fanout) can read individual
/// signals without re-instrumenting ingestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationActivity {
    pub last_active_at_ms: i64,
    pub last_chat_state: Option<NotificationChatState>,
    pub last_read_at_ms: Option<i64>,
    /// XEP-0045 / RFC 6121 §4.7.2.1 `<show/>` token. Closed set:
    /// `Away`, `Chat`, `Dnd`, `Xa`. The DB layer enforces the closed
    /// set via a CHECK constraint, and the typed enum guarantees
    /// bounds-by-construction on the writer path.
    pub presence_show: Option<NotificationPresenceShow>,
}

#[derive(Debug, Error)]
pub enum NotificationActivityError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("invalid chat-state token: {0}")]
    InvalidChatState(String),
    #[error("invalid presence <show/> token: {0}")]
    InvalidPresenceShow(String),
    #[error("invalid owner bare JID in notification_activity: {0}")]
    InvalidOwnerBareJid(String),
    #[error("invalid conversation JID in notification_activity: {0}")]
    InvalidConversationJid(String),
}

/// T1 lookup of the recipient's recent activity for a given
/// conversation, used by the XEP-0513 `<active/>` push filter.
///
/// Returning `Ok(None)` means *no activity has ever been recorded for
/// this (owner, conversation) pair*. The evaluator treats this as a
/// miss — there is no signal that the recipient is currently active,
/// so the XEP-0513 `<active/>` filter suppresses with
/// [`crate::notification_outbox::SuppressedReason::Xep0513ActiveMiss`].
#[async_trait]
pub trait NotificationActivityReader: Send + Sync {
    async fn read_activity(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError>;
}

/// Default [`NotificationActivityReader`] that reports every
/// (user, conversation) as having no recorded activity.
///
/// Used at test call sites and at T0 emission sites that do not
/// consult the projection. Returning `Ok(None)` means the evaluator
/// treats the recipient as inactive — but the T0 emission stage
/// deliberately skips the activity check (current activity is a T1
/// read), so this only affects the T1 drain path when no real reader
/// is wired.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopActivityReader;

#[async_trait]
impl NotificationActivityReader for NoopActivityReader {
    async fn read_activity(
        &self,
        _owner: &BareJid,
        _conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_activity::schema::{
        NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES, NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES,
    };

    /// Closed-set audit: every [`NotificationChatState`] variant MUST
    /// round-trip through `as_db_value` / `from_db_value`. Iterates
    /// `ALL` so future enum extensions join this test automatically.
    #[test]
    fn notification_chat_state_round_trip_covers_every_variant() {
        assert_eq!(
            NotificationChatState::ALL.len(),
            NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES.len(),
            "variant count must match CHECK constraint value list",
        );
        for variant in NotificationChatState::ALL.iter().copied() {
            let db = variant.as_db_value();
            assert!(
                NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES.contains(&db),
                "variant {variant:?} db value {db} missing from CHECK list",
            );
            let decoded = NotificationChatState::from_db_value(db).expect("decode");
            assert_eq!(
                decoded, variant,
                "round-trip failed for {variant:?} (db value {db})"
            );
            // XEP-0085 bidirectional conversion stays in lockstep.
            assert_eq!(
                NotificationChatState::from_xep0085(variant.to_xep0085()),
                variant
            );
        }
        assert!(matches!(
            NotificationChatState::from_db_value("not-a-state"),
            Err(NotificationActivityError::InvalidChatState(_))
        ));
    }

    /// Closed-set audit: every [`NotificationPresenceShow`] variant
    /// MUST round-trip through `as_db_value` / `from_db_value`.
    /// Iterates `ALL` so future enum extensions join this test
    /// automatically.
    #[test]
    fn notification_presence_show_round_trip_covers_every_variant() {
        assert_eq!(
            NotificationPresenceShow::ALL.len(),
            NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES.len(),
            "variant count must match CHECK constraint value list",
        );
        for variant in NotificationPresenceShow::ALL.iter().copied() {
            let db = variant.as_db_value();
            assert!(
                NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES.contains(&db),
                "variant {variant:?} db value {db} missing from CHECK list",
            );
            let decoded = NotificationPresenceShow::from_db_value(db).expect("decode");
            assert_eq!(
                decoded, variant,
                "round-trip failed for {variant:?} (db value {db})"
            );
        }
        assert!(matches!(
            NotificationPresenceShow::from_db_value("not-a-show"),
            Err(NotificationActivityError::InvalidPresenceShow(_))
        ));
    }

    /// XEP-0045 / RFC 6121 bidirectional conversion stays in lockstep
    /// with the typed db-values. Locks the
    /// `xmpp_parsers::presence::Show` <-> `NotificationPresenceShow`
    /// shape so a parser-side enum reshuffle is detected immediately.
    #[test]
    fn notification_presence_show_xep0045_conversion_round_trip() {
        for variant in NotificationPresenceShow::ALL.iter().copied() {
            let xep = variant.to_xep0045();
            let back = NotificationPresenceShow::from_xep0045(xep.clone());
            assert_eq!(
                back, variant,
                "xep0045 round-trip failed for {variant:?} (xep variant {xep:?})",
            );
        }
    }
}
