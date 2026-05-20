//! Durable per-(user, conversation) activity projection backing the
//! XEP-0513 `<active/>` push filter.
//!
//! The projection records, for each conversation a user participates
//! in, the most recent typed signal that the user was *currently
//! active*: a XEP-0085 chat-state update, a XEP-0490 read-marker
//! advance, an outbound message commit, or a XEP-0045 presence
//! join/leave. The T1 push-gate evaluator consults this projection to
//! suppress `ActiveChannelMention` candidates whose recipient is no
//! longer recent enough to satisfy the XEP-0513 `<active/>` filter.
//!
//! Slice 2b — wires
//! [`crate::notification_outbox::SuppressedReason::Xep0513ActiveMiss`]
//! (the reserved variant from slice 2a) to the evaluator and lands the
//! projection store + reader trait + writer surface used by the
//! ingestion sites (XEP-0085 / XEP-0490 / outbound commit / XEP-0045).
//!
//! Cold-start expectation: on first deploy the projection table is
//! empty. Every [`crate::notification_outbox::NotificationClass::ActiveChannelMention`]
//! candidate that reaches the T1 drain will suppress with
//! [`crate::notification_outbox::SuppressedReason::Xep0513ActiveMiss`]
//! until users start sending chat-states, advancing read markers,
//! committing outbound messages, or emitting MUC presence. The metric
//! `waddle_push_suppressed_total{reason="xep0513_active_miss"}` will
//! therefore ramp up from zero to a baseline as the projection fills.
//! That is expected behavior, not a regression.

use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;

use crate::db::{Database, DatabaseError, IntoParams};

/// CHECK constraint name on `notification_activity.last_chat_state`.
pub(crate) const NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_NAME: &str =
    "notification_activity_last_chat_state_check";

/// Closed-set XEP-0085 chat-state db-values accepted by the
/// `notification_activity.last_chat_state` column. Parallel to the
/// [`NotificationChatState`] enum so the schema CHECK, the typed enum,
/// and the migration matcher stay in lockstep without three
/// independently-edited lists.
pub(crate) const NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES: [&str; 5] =
    ["active", "composing", "paused", "inactive", "gone"];

/// SQL fragment matching [`NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES`].
/// Inlined into the table DDL and the migration's `ADD CONSTRAINT`
/// statement; both sides MUST stay in lockstep with the parallel
/// `_VALUES` array — the
/// [`notification_activity_last_chat_state_constraint_matches_expected`]
/// matcher enforces that.
pub(crate) const NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_SQL: &str =
    "last_chat_state IS NULL OR last_chat_state IN ('active', 'composing', 'paused', 'inactive', 'gone')";

/// CHECK constraint name on `notification_activity.presence_show`.
pub(crate) const NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_NAME: &str =
    "notification_activity_presence_show_check";

/// Closed-set XEP-0045/RFC 6121 §4.7.2.1 `<show/>` db-values accepted
/// by the `notification_activity.presence_show` column. Parallel to
/// the [`NotificationPresenceShow`] enum so the schema CHECK, the
/// typed enum, and the migration matcher stay in lockstep without
/// three independently-edited lists.
pub(crate) const NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES: [&str; 4] =
    ["away", "chat", "dnd", "xa"];

/// SQL fragment matching [`NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES`].
/// Inlined into the table DDL and the migration's `ADD CONSTRAINT`
/// statement; both sides MUST stay in lockstep with the parallel
/// `_VALUES` array — the
/// [`notification_activity_presence_show_constraint_matches_expected`]
/// matcher enforces that.
pub(crate) const NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_SQL: &str =
    "presence_show IS NULL OR presence_show IN ('away', 'chat', 'dnd', 'xa')";

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

/// Returns `true` iff `definition` quotes `value` as a SQL string
/// literal — see
/// [`crate::notification_outbox`]'s
/// `constraint_definition_quotes_value` for the rationale (Postgres'
/// `pg_get_constraintdef` and SQLite's `sqlite_master.sql` both render
/// IN-list literals with single quotes around each enum value, and
/// matching against the quoted form prevents substring false positives).
fn constraint_definition_quotes_value(definition: &str, value: &str) -> bool {
    let mut needle = String::with_capacity(value.len() + 2);
    needle.push('\'');
    needle.push_str(value);
    needle.push('\'');
    definition.contains(&needle)
}

/// Matcher used by the Postgres and SQLite migration paths to detect a
/// stale `last_chat_state` CHECK. The matcher MUST require every
/// quoted typed db-value — otherwise a partial definition that
/// advertises only a subset of the closed-set could be accepted as
/// "current", leaving the column unable to round-trip future inserts.
pub(crate) fn notification_activity_last_chat_state_constraint_matches_expected(
    definition: &str,
) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("last_chat_state")
        && NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES
            .iter()
            .all(|value| constraint_definition_quotes_value(&normalized, value))
}

/// Matcher used by the Postgres and SQLite migration paths to detect a
/// stale `presence_show` CHECK. Same substring-safety guarantees as
/// [`notification_activity_last_chat_state_constraint_matches_expected`]:
/// every typed db-value MUST appear as a quoted SQL literal.
pub(crate) fn notification_activity_presence_show_constraint_matches_expected(
    definition: &str,
) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("presence_show")
        && NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES
            .iter()
            .all(|value| constraint_definition_quotes_value(&normalized, value))
}

fn notification_activity_table_sql(i64_type: &str, if_not_exists: bool) -> String {
    let if_not_exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        r#"
        CREATE TABLE {if_not_exists}notification_activity (
            owner_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            last_active_at_ms {i64_type} NOT NULL,
            last_chat_state TEXT NULL CONSTRAINT {NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_NAME} CHECK ({NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_SQL}),
            last_read_at_ms {i64_type} NULL,
            presence_show TEXT NULL CONSTRAINT {NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_NAME} CHECK ({NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_SQL}),
            created_at_ms {i64_type} NOT NULL,
            updated_at_ms {i64_type} NOT NULL,
            PRIMARY KEY (owner_bare_jid, conversation_jid)
        )
        "#
    )
}

#[derive(Clone)]
pub struct NotificationActivityStore {
    db: Database,
}

impl NotificationActivityStore {
    pub async fn new(db: Database) -> Result<Self, NotificationActivityError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
    }

    async fn initialize(&self) -> Result<(), NotificationActivityError> {
        // Startup invariant: every closed-set typed `NotificationChatState`
        // db-value MUST round-trip through `from_db_value`. Mismatched
        // builds fail fast at process start rather than at first
        // insert-time CHECK violation.
        for state in NotificationChatState::ALL.iter().copied() {
            let db = state.as_db_value();
            let decoded = NotificationChatState::from_db_value(db)?;
            if decoded != state {
                return Err(NotificationActivityError::InvalidChatState(format!(
                    "round-trip mismatch for {db}: decoded {decoded:?}",
                )));
            }
        }
        // Same invariant for the closed-set typed `NotificationPresenceShow`
        // db-values — mismatched builds fail fast at process start.
        for show in NotificationPresenceShow::ALL.iter().copied() {
            let db = show.as_db_value();
            let decoded = NotificationPresenceShow::from_db_value(db)?;
            if decoded != show {
                return Err(NotificationActivityError::InvalidPresenceShow(format!(
                    "round-trip mismatch for {db}: decoded {decoded:?}",
                )));
            }
        }
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        self.execute(&notification_activity_table_sql(i64_type, true), ())
            .await?;
        // Migrate the Postgres CHECK constraint if a previous build
        // shipped a stale variant set. SQLite enforces CHECK at row
        // write time and has no `pg_constraint`-equivalent walker, so
        // we instead rebuild the table on a stale-schema detection.
        self.migrate_last_chat_state_constraint(i64_type).await?;
        // Same migration shape for the closed-set `presence_show` CHECK
        // introduced by the typed `NotificationPresenceShow` enum: any
        // legacy build whose `presence_show` column lacked a CHECK (or
        // shipped a stale variant set) gets repaired here before the
        // first write attempts to honour the closed set.
        self.migrate_presence_show_constraint(i64_type).await?;
        // Per-conversation active-recipient lookup index. Slice 2b only
        // reads per-(user, conversation), but the trailing
        // `last_active_at_ms` lets future slices range-scan all
        // "recently active" recipients of a conversation without a
        // table scan.
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_activity_conversation_active \
             ON notification_activity (conversation_jid, last_active_at_ms)",
            (),
        )
        .await?;
        Ok(())
    }

    async fn migrate_last_chat_state_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationActivityError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => self.migrate_postgres_last_chat_state().await,
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_last_chat_state(i64_type).await
            }
        }
    }

    /// Postgres path: walk `pg_constraint` for every CHECK on the
    /// `last_chat_state` column, drop any whose definition does NOT
    /// match the typed closed set, then ensure the named CHECK is in
    /// place. Mirrors the
    /// `migrate_postgres_check_constraint_on_column` helper in
    /// `notification_outbox` — including the anonymous-CHECK drop so
    /// legacy inline `CHECK (col IN (...))` definitions can be
    /// replaced safely.
    async fn migrate_postgres_last_chat_state(&self) -> Result<(), NotificationActivityError> {
        let existing = self
            .postgres_check_constraints_on_column("notification_activity", "last_chat_state")
            .await?;
        let mut current_named_present = false;
        let mut to_drop: Vec<String> = Vec::new();
        for (conname, definition) in &existing {
            if conname == NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_NAME
                && notification_activity_last_chat_state_constraint_matches_expected(definition)
            {
                current_named_present = true;
            } else {
                to_drop.push(conname.clone());
            }
        }
        if current_named_present && to_drop.is_empty() {
            return Ok(());
        }
        for conname in &to_drop {
            self.execute(
                &format!(
                    "ALTER TABLE notification_activity DROP CONSTRAINT IF EXISTS \"{conname}\""
                ),
                (),
            )
            .await?;
        }
        if !current_named_present {
            self.execute(
                &format!(
                    "ALTER TABLE notification_activity \
                     ADD CONSTRAINT {NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_NAME} \
                     CHECK ({NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_SQL})"
                ),
                (),
            )
            .await?;
        }
        Ok(())
    }

    async fn postgres_check_constraints_on_column(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Vec<(String, String)>, NotificationActivityError> {
        let mut rows = self
            .query(
                r#"
                SELECT c.conname,
                       pg_get_constraintdef(c.oid)
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = ?
                WHERE c.conrelid = (? :: regclass)
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![column, table],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let conname: String = row.get(0)?;
            let definition: String = row.get(1)?;
            out.push((conname, definition));
        }
        Ok(out)
    }

    async fn migrate_sqlite_last_chat_state(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationActivityError> {
        if !self.sqlite_last_chat_state_constraint_is_stale().await? {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        tx.execute(
            "DROP INDEX IF EXISTS idx_notification_activity_conversation_active",
            (),
        )
        .await?;
        tx.execute(
            "ALTER TABLE notification_activity RENAME TO notification_activity_old_chat_state_check",
            (),
        )
        .await?;
        tx.execute(&notification_activity_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            )
            SELECT
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            FROM notification_activity_old_chat_state_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_activity_old_chat_state_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_last_chat_state_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationActivityError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_activity'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_activity_last_chat_state_constraint_matches_expected(&create_sql))
    }

    async fn migrate_presence_show_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationActivityError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => self.migrate_postgres_presence_show().await,
            crate::db::DatabaseDriver::Sqlite => self.migrate_sqlite_presence_show(i64_type).await,
        }
    }

    /// Postgres path: same shape as [`Self::migrate_postgres_last_chat_state`].
    /// Walk `pg_constraint` for every CHECK on the `presence_show`
    /// column, drop any whose definition does NOT match the typed
    /// closed set, then ensure the named CHECK is in place.
    async fn migrate_postgres_presence_show(&self) -> Result<(), NotificationActivityError> {
        let existing = self
            .postgres_check_constraints_on_column("notification_activity", "presence_show")
            .await?;
        let mut current_named_present = false;
        let mut to_drop: Vec<String> = Vec::new();
        for (conname, definition) in &existing {
            if conname == NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_NAME
                && notification_activity_presence_show_constraint_matches_expected(definition)
            {
                current_named_present = true;
            } else {
                to_drop.push(conname.clone());
            }
        }
        if current_named_present && to_drop.is_empty() {
            return Ok(());
        }
        for conname in &to_drop {
            self.execute(
                &format!(
                    "ALTER TABLE notification_activity DROP CONSTRAINT IF EXISTS \"{conname}\""
                ),
                (),
            )
            .await?;
        }
        if !current_named_present {
            self.execute(
                &format!(
                    "ALTER TABLE notification_activity \
                     ADD CONSTRAINT {NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_NAME} \
                     CHECK ({NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_SQL})"
                ),
                (),
            )
            .await?;
        }
        Ok(())
    }

    async fn migrate_sqlite_presence_show(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationActivityError> {
        if !self.sqlite_presence_show_constraint_is_stale().await? {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        tx.execute(
            "DROP INDEX IF EXISTS idx_notification_activity_conversation_active",
            (),
        )
        .await?;
        tx.execute(
            "ALTER TABLE notification_activity RENAME TO notification_activity_old_presence_show_check",
            (),
        )
        .await?;
        tx.execute(&notification_activity_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            )
            SELECT
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            FROM notification_activity_old_presence_show_check
            "#,
            (),
        )
        .await?;
        tx.execute(
            "DROP TABLE notification_activity_old_presence_show_check",
            (),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_presence_show_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationActivityError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_activity'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_activity_presence_show_constraint_matches_expected(&create_sql))
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, NotificationActivityError> {
        let conn = self.db.guard().await?;
        Ok(conn.execute(sql, params).await?)
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, NotificationActivityError> {
        let conn = self.db.guard().await?;
        Ok(conn.query(sql, params).await?)
    }

    /// Record a XEP-0085 chat-state change as activity for the user
    /// on the named conversation.
    ///
    /// Idempotent: re-applying the same `(owner, conversation, state)`
    /// at a later time advances both `last_active_at_ms` and
    /// `updated_at_ms`. Concurrent writers race to `INSERT … ON
    /// CONFLICT DO UPDATE`; the row reflects the most recent commit.
    pub async fn record_chat_state(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        chat_state: NotificationChatState,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, ?, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                last_chat_state = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.last_chat_state
                    ELSE notification_activity.last_chat_state
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms > notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                chat_state.as_db_value(),
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Mark `(owner, conversation)` as no longer active. Used for the
    /// XEP-0085 `<gone/>` signal: the user has ended participation in
    /// the conversation, so any prior activity window must be
    /// invalidated regardless of how recent it was. Bypasses the
    /// monotonic clamp on `last_active_at_ms` — `<gone/>` is the only
    /// path that legitimately regresses activity, because semantically
    /// it tells us the recipient is *not* currently engaged. The T1
    /// XEP-0513 `<active/>` filter then sees `now_ms - 0` which is
    /// huge, so the `ActiveChannelMention` is suppressed with
    /// `Xep0513ActiveMiss`. The audit trail preserves the chat-state
    /// token as `gone` for diagnostics (Codex review on PR #731).
    pub async fn record_chat_state_gone(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, 0, ?, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = 0,
                last_chat_state = excluded.last_chat_state,
                updated_at_ms = excluded.updated_at_ms
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                NotificationChatState::Gone.as_db_value(),
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record a XEP-0490 read-marker advance as activity for the user
    /// on the named conversation. Updates both `last_read_at_ms` and
    /// `last_active_at_ms` — a read-marker advance is by definition a
    /// currently-engaged signal. Both timestamp columns advance
    /// monotonically: a late-arriving stale write CANNOT regress
    /// either column (XEP-0490 read-marker invariant + general
    /// projection monotonicity under concurrent writers).
    pub async fn record_read_marker(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, ?, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                last_read_at_ms = CASE
                    WHEN notification_activity.last_read_at_ms IS NULL THEN excluded.last_read_at_ms
                    WHEN excluded.last_read_at_ms > notification_activity.last_read_at_ms
                    THEN excluded.last_read_at_ms
                    ELSE notification_activity.last_read_at_ms
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms > notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                now_ms,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record an outbound message commit as activity for the sender
    /// on the named conversation. Sending a message is the strongest
    /// "currently active" signal we have.
    pub async fn record_outbound_message(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms > notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record a XEP-0045 presence event (join or available `<show/>`
    /// change) for the user against the given MUC room.
    ///
    /// A `None` `show` is the canonical default-`available` token (no
    /// `<show/>` child); the column accepts it and the read path
    /// preserves the distinction. The typed
    /// [`NotificationPresenceShow`] enum guarantees the persisted
    /// value is one of the four RFC 6121 §4.7.2.1 tokens — no
    /// truncation or sanitisation needed at the writer.
    pub async fn record_presence_available(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        show: Option<NotificationPresenceShow>,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        let show_db_value: Option<&'static str> = show.map(NotificationPresenceShow::as_db_value);
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, NULL, ?, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                presence_show = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.presence_show
                    ELSE notification_activity.presence_show
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms > notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                show_db_value,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record a XEP-0045 `<presence type='unavailable'/>` event. Per
    /// the brief, an explicit leave still counts as recent activity
    /// (so we bump `last_active_at_ms`) but clears the `<show/>` value
    /// — there is no longer an available presence to report.
    pub async fn record_presence_unavailable(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                presence_show = CASE
                    WHEN excluded.last_active_at_ms > notification_activity.last_active_at_ms
                    THEN NULL
                    ELSE notification_activity.presence_show
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms > notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    async fn read(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
        let mut rows = self
            .query(
                r#"
                SELECT last_active_at_ms,
                       last_chat_state,
                       last_read_at_ms,
                       presence_show
                FROM notification_activity
                WHERE owner_bare_jid = ?
                  AND conversation_jid = ?
                "#,
                crate::db_params![owner.to_string(), conversation.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let last_active_at_ms: i64 = row.get(0)?;
        let last_chat_state_raw: Option<String> = row.get(1)?;
        let last_read_at_ms: Option<i64> = row.get(2)?;
        let presence_show_raw: Option<String> = row.get(3)?;
        let last_chat_state = last_chat_state_raw
            .map(|raw| NotificationChatState::from_db_value(&raw))
            .transpose()?;
        let presence_show = presence_show_raw
            .map(|raw| NotificationPresenceShow::from_db_value(&raw))
            .transpose()?;
        Ok(Some(NotificationActivity {
            last_active_at_ms,
            last_chat_state,
            last_read_at_ms,
            presence_show,
        }))
    }
}

#[async_trait]
impl NotificationActivityReader for NotificationActivityStore {
    async fn read_activity(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
        self.read(owner, conversation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare jid")
    }

    async fn store() -> NotificationActivityStore {
        NotificationActivityStore::new(
            Database::in_memory("notification-activity-test")
                .await
                .expect("in-memory db"),
        )
        .await
        .expect("activity store")
    }

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

    /// Regression: the constraint matcher MUST reject a definition
    /// that contains a typed value only as a substring of another
    /// value — the substring guard caught a gap in slice 1 / 2a and
    /// MUST be enforced here from day one.
    ///
    /// `inactive` is a substring of every full variant name only at
    /// the token boundary, so we exercise the more interesting case:
    /// `active` is a literal substring of `inactive`. A bare-token
    /// definition advertising only `inactive` (without `'active'`)
    /// MUST be flagged stale.
    #[test]
    fn notification_activity_last_chat_state_constraint_match_rejects_substring_only_definition() {
        // Definition advertises `inactive` (which contains the
        // substring `active`) but not the standalone `'active'`
        // literal. The matcher MUST flag this stale.
        let substring_only = "CHECK (last_chat_state IS NULL OR last_chat_state IN ('inactive'))";
        assert!(
            !notification_activity_last_chat_state_constraint_matches_expected(substring_only),
            "matcher MUST require every variant as a quoted literal, not as a substring",
        );

        // Sanity: the full closed-set definition is accepted.
        let full = "CHECK (last_chat_state IS NULL OR last_chat_state IN \
                    ('active', 'composing', 'paused', 'inactive', 'gone'))";
        assert!(notification_activity_last_chat_state_constraint_matches_expected(full));
    }

    /// Recording a chat-state event persists the typed token and bumps
    /// `last_active_at_ms`. Re-recording overrides previous columns
    /// per the `ON CONFLICT DO UPDATE` semantics.
    #[tokio::test]
    async fn record_chat_state_persists_typed_token_and_advances_activity() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        store
            .record_chat_state(
                &owner,
                &conversation,
                NotificationChatState::Composing,
                1_000,
            )
            .await
            .expect("record chat-state");

        let activity = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 1_000);
        assert_eq!(
            activity.last_chat_state,
            Some(NotificationChatState::Composing)
        );
        assert!(activity.last_read_at_ms.is_none());
        assert!(activity.presence_show.is_none());

        // Re-recording at a later timestamp advances activity and
        // updates the chat-state token.
        store
            .record_chat_state(&owner, &conversation, NotificationChatState::Paused, 2_000)
            .await
            .expect("record again");
        let activity = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 2_000);
        assert_eq!(
            activity.last_chat_state,
            Some(NotificationChatState::Paused)
        );
    }

    /// Monotonic invariant: a stale chat-state write whose `now_ms` is
    /// older than the projection's stored `last_active_at_ms` MUST NOT
    /// regress either `last_active_at_ms` or `last_chat_state`. This
    /// guards against concurrent UPSERT races where a slow writer
    /// commits AFTER a fresh writer with a smaller event timestamp.
    #[tokio::test]
    async fn record_chat_state_does_not_regress_on_stale_write() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        // Fresh write at t=2000.
        store
            .record_chat_state(&owner, &conversation, NotificationChatState::Active, 2_000)
            .await
            .expect("fresh chat-state");
        // Stale write at t=1000 arrives second.
        store
            .record_chat_state(
                &owner,
                &conversation,
                NotificationChatState::Inactive,
                1_000,
            )
            .await
            .expect("stale chat-state");
        let activity = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(
            activity.last_active_at_ms, 2_000,
            "stale chat-state MUST NOT regress last_active_at_ms"
        );
        assert_eq!(
            activity.last_chat_state,
            Some(NotificationChatState::Active),
            "stale chat-state MUST NOT overwrite the fresh chat-state token"
        );
    }

    /// XEP-0085 `<gone/>` is an explicit inactivity signal. The writer
    /// MUST zero `last_active_at_ms` regardless of how recent the prior
    /// activity was so the T1 XEP-0513 `<active/>` filter immediately
    /// stops treating the user as engaged in the conversation. The
    /// chat-state token is preserved as `gone` for diagnostics.
    #[tokio::test]
    async fn record_chat_state_gone_zeroes_last_active_unconditionally() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        // Seed a fresh active state at t=5000.
        store
            .record_chat_state(&owner, &conversation, NotificationChatState::Active, 5_000)
            .await
            .expect("seed active");
        let seeded = store
            .read(&owner, &conversation)
            .await
            .expect("read seed")
            .expect("row");
        assert_eq!(seeded.last_active_at_ms, 5_000);
        // <gone/> at t=6000 MUST zero last_active_at_ms even though
        // the prior write is more recent than this would otherwise be
        // allowed under the monotonic clamp.
        store
            .record_chat_state_gone(&owner, &conversation, 6_000)
            .await
            .expect("record gone");
        let after = store
            .read(&owner, &conversation)
            .await
            .expect("read post-gone")
            .expect("row");
        assert_eq!(
            after.last_active_at_ms, 0,
            "<gone/> MUST unconditionally regress last_active_at_ms to 0",
        );
        assert_eq!(
            after.last_chat_state,
            Some(NotificationChatState::Gone),
            "<gone/> MUST preserve the typed chat-state token for diagnostics",
        );
    }

    /// `record_chat_state_gone` works as an UPSERT against a row that
    /// does not yet exist for the (owner, conversation) pair — the
    /// first signal we ever see for this user in this conversation can
    /// legitimately be a `<gone/>` (a client sending its departure on
    /// disconnect without ever having sent another chat-state).
    #[tokio::test]
    async fn record_chat_state_gone_upserts_missing_row() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        store
            .record_chat_state_gone(&owner, &conversation, 1_000)
            .await
            .expect("record gone");
        let after = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(after.last_active_at_ms, 0);
        assert_eq!(after.last_chat_state, Some(NotificationChatState::Gone));
    }

    /// XEP-0490 read-marker writes persist `last_read_at_ms` alongside
    /// `last_active_at_ms` and leave other columns untouched.
    #[tokio::test]
    async fn record_read_marker_persists_last_read_and_active() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        // Seed a chat-state row first so we can witness that the
        // read-marker write leaves `last_chat_state` intact.
        store
            .record_chat_state(
                &owner,
                &conversation,
                NotificationChatState::Composing,
                1_000,
            )
            .await
            .expect("seed");
        store
            .record_read_marker(&owner, &conversation, 2_000)
            .await
            .expect("record marker");

        let activity = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 2_000);
        assert_eq!(activity.last_read_at_ms, Some(2_000));
        assert_eq!(
            activity.last_chat_state,
            Some(NotificationChatState::Composing),
            "read-marker MUST NOT overwrite chat-state",
        );
    }

    /// XEP-0490 monotonic invariant: a stale read-marker write whose
    /// `now_ms` is older than the stored `last_read_at_ms` MUST NOT
    /// regress the marker. XEP-0490 §3 mandates monotonic advance of
    /// the displayed marker; the projection enforces it at the
    /// UPSERT layer so out-of-order arrivals (network reorder,
    /// concurrent writers) cannot violate the wire-level invariant.
    #[tokio::test]
    async fn record_read_marker_does_not_regress_on_stale_write() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        store
            .record_read_marker(&owner, &conversation, 2_000)
            .await
            .expect("fresh marker");
        store
            .record_read_marker(&owner, &conversation, 1_000)
            .await
            .expect("stale marker");
        let activity = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(
            activity.last_active_at_ms, 2_000,
            "stale read-marker MUST NOT regress last_active_at_ms"
        );
        assert_eq!(
            activity.last_read_at_ms,
            Some(2_000),
            "stale read-marker MUST NOT regress last_read_at_ms (XEP-0490 monotonicity)"
        );
    }

    /// Outbound message commit bumps `last_active_at_ms` but leaves
    /// other columns untouched (no chat-state, no read marker, no
    /// presence change).
    #[tokio::test]
    async fn record_outbound_message_advances_active_only() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        store
            .record_outbound_message(&owner, &conversation, 3_000)
            .await
            .expect("record outbound");
        let activity = store
            .read(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 3_000);
        assert!(activity.last_chat_state.is_none());
        assert!(activity.last_read_at_ms.is_none());
        assert!(activity.presence_show.is_none());
    }

    /// XEP-0045 presence: an available presence persists the
    /// `<show/>` token; a subsequent unavailable clears it but still
    /// bumps `last_active_at_ms`.
    #[tokio::test]
    async fn record_presence_available_then_unavailable_keeps_recent_activity() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let room = bare("room@muc.example.com");
        store
            .record_presence_available(&owner, &room, Some(NotificationPresenceShow::Away), 4_000)
            .await
            .expect("available");
        let activity = store.read(&owner, &room).await.expect("read").expect("row");
        assert_eq!(activity.last_active_at_ms, 4_000);
        assert_eq!(activity.presence_show, Some(NotificationPresenceShow::Away));

        // Unavailable bumps activity but clears the show.
        store
            .record_presence_unavailable(&owner, &room, 5_000)
            .await
            .expect("unavailable");
        let activity = store.read(&owner, &room).await.expect("read").expect("row");
        assert_eq!(activity.last_active_at_ms, 5_000);
        assert!(
            activity.presence_show.is_none(),
            "unavailable MUST clear `<show/>`",
        );
    }

    /// `NotificationActivityReader` impl on the store matches the
    /// inherent `read` method — exercises the trait surface that the
    /// T1 evaluator consults.
    #[tokio::test]
    async fn reader_trait_returns_recorded_activity() {
        let store = store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        store
            .record_outbound_message(&owner, &conversation, 7_000)
            .await
            .expect("record");
        let activity = NotificationActivityReader::read_activity(&store, &owner, &conversation)
            .await
            .expect("reader")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 7_000);
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

    /// Regression: the `presence_show` constraint matcher MUST reject
    /// a definition that contains a typed value only as a substring of
    /// another value — same substring-safety guarantee as the
    /// `last_chat_state` matcher (slice 2a). Lock from day one so a
    /// matcher refactor can't silently re-introduce the gap.
    ///
    /// We exercise `xa` as a substring of the longer `Xavier`-style
    /// token — a definition advertising only `xavier` MUST NOT be
    /// accepted as containing the standalone `'xa'` literal.
    #[test]
    fn notification_activity_presence_show_constraint_match_rejects_substring_only_definition() {
        let substring_only = "CHECK (presence_show IS NULL OR presence_show IN ('xavier'))";
        assert!(
            !notification_activity_presence_show_constraint_matches_expected(substring_only),
            "matcher MUST require every variant as a quoted literal, not as a substring",
        );

        // Sanity: the full closed-set definition is accepted.
        let full = "CHECK (presence_show IS NULL OR presence_show IN \
                    ('away', 'chat', 'dnd', 'xa'))";
        assert!(notification_activity_presence_show_constraint_matches_expected(full));
    }

    /// `NoopActivityReader` returns `None` for every (owner,
    /// conversation) — the evaluator treats this as a miss.
    #[tokio::test]
    async fn noop_reader_reports_no_activity() {
        let reader = NoopActivityReader;
        let owner = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        let activity = reader
            .read_activity(&owner, &conversation)
            .await
            .expect("noop reader");
        assert!(activity.is_none());
    }
}
