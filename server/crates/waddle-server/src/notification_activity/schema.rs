//! Durable schema DDL, CHECK-constraint matching, and migrations for
//! `notification_activity`.

use super::*;

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

/// Extract every single-quoted literal token from a CHECK constraint
/// definition. Both Postgres' `pg_get_constraintdef` and SQLite's
/// `sqlite_master.sql` render IN-list values as single-quoted
/// literals; this helper returns the **set** of those literals so a
/// caller can verify the constraint's value set is **exactly** the
/// closed enum — neither missing values (substring-safety) nor
/// permitting extras (exclusivity).
///
/// The XEP-conformance contract here is one-way: a hand-modified or
/// older CHECK that admits `'online'` alongside the canonical four
/// XEP-0045 tokens would round-trip a stored `'online'` row, and the
/// typed [`NotificationPresenceShow::from_db_value`] decode in
/// [`NotificationActivityStore::read`] would then surface
/// [`NotificationActivityError::InvalidPresenceShow`] into the T1
/// push-gate evaluator. Catching the over-permissive CHECK at the
/// matcher tier keeps that decode path tight by construction — the
/// migration will rewrite the constraint to the canonical closed
/// set before any stale row can be observed.
fn extract_single_quoted_literals(definition: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let bytes = definition.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'\'' {
                end += 1;
            }
            if end < bytes.len() && start < end {
                // ASCII-only by the closed-set construction; the
                // definition has already been lowercased by callers
                // when they need case-insensitive comparison.
                out.insert(String::from_utf8_lossy(&bytes[start..end]).into_owned());
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Matcher used by the Postgres and SQLite migration paths to detect a
/// stale `presence_show` CHECK. Stricter than the original
/// substring-safety guarantee: the matcher now verifies **exclusivity**
/// — the constraint's quoted-literal set MUST equal the typed closed
/// set [`NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES`]. A CHECK that
/// allows extra values (e.g. a hand-modified definition with
/// `'online'`) is flagged stale and the migration rewrites it.
/// Without exclusivity, an over-permissive CHECK would silently
/// round-trip out-of-enum values which then fail
/// [`NotificationPresenceShow::from_db_value`] at read time.
pub(crate) fn notification_activity_presence_show_constraint_matches_expected(
    definition: &str,
) -> bool {
    let normalized = definition.to_ascii_lowercase();
    if !normalized.contains("presence_show") {
        return false;
    }
    let extracted = extract_single_quoted_literals(&normalized);
    let expected: std::collections::BTreeSet<String> = NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES
        .iter()
        .map(|s| s.to_string())
        .collect();
    extracted == expected
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

impl NotificationActivityStore {
    pub(super) async fn initialize(&self) -> Result<(), NotificationActivityError> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Exclusivity guarantee: a CHECK definition that allows the full
    /// XEP-0045 closed set PLUS an extra value (e.g. `'online'` from a
    /// hand-modified deployment or an older schema variant) MUST be
    /// flagged stale so the migration rewrites it. Without this, an
    /// over-permissive CHECK would round-trip out-of-enum rows that
    /// then fail [`NotificationPresenceShow::from_db_value`] at read
    /// time, surfacing
    /// [`NotificationActivityError::InvalidPresenceShow`] into the T1
    /// push-gate evaluator. Locks the contract from day one so a
    /// matcher refactor cannot silently re-introduce the gap.
    #[test]
    fn notification_activity_presence_show_constraint_match_rejects_over_permissive_definition() {
        // Postgres-shape: the four canonical tokens plus an extra
        // `'online'` token a hand-modified deployment might add.
        let pg_extra = "CHECK (((presence_show IS NULL) OR \
            ((presence_show)::text = ANY ((ARRAY['away'::character varying, \
            'chat'::character varying, 'dnd'::character varying, \
            'xa'::character varying, 'online'::character varying])::text[]))))";
        assert!(
            !notification_activity_presence_show_constraint_matches_expected(pg_extra),
            "Postgres-shape over-permissive CHECK must be flagged stale",
        );

        // SQLite-shape variant of the same defect.
        let sqlite_extra =
            "CHECK (presence_show IS NULL OR presence_show IN ('away','chat','dnd','xa','online'))";
        assert!(
            !notification_activity_presence_show_constraint_matches_expected(sqlite_extra),
            "SQLite-shape over-permissive CHECK must be flagged stale",
        );

        // Sanity: the canonical closed set still passes both shapes
        // (regression guard against accidentally over-tightening).
        let pg_canonical = "CHECK (((presence_show IS NULL) OR \
            ((presence_show)::text = ANY ((ARRAY['away'::character varying, \
            'chat'::character varying, 'dnd'::character varying, \
            'xa'::character varying])::text[]))))";
        assert!(
            notification_activity_presence_show_constraint_matches_expected(pg_canonical),
            "canonical Postgres definition must still match",
        );
        let sqlite_canonical =
            "CHECK (presence_show IS NULL OR presence_show IN ('away', 'chat', 'dnd', 'xa'))";
        assert!(
            notification_activity_presence_show_constraint_matches_expected(sqlite_canonical),
            "canonical SQLite definition must still match",
        );
    }
}
