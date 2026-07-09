//! Durable schema DDL, CHECK-constraint matching, and startup migrations
//! for `notification_candidates` and `notification_outbox`.

use super::*;

const NOTIFICATION_CANDIDATES_REASON_CHECK_NAME: &str = "notification_candidates_reason_check";
const NOTIFICATION_CANDIDATES_REASON_VALUES: [&str; 6] = [
    "offline_dm",
    "offline_dm_mention",
    "groupchat_personal_mention",
    "groupchat_channel_mention",
    "groupchat_active_channel_mention",
    "groupchat_notify_all",
];
const NOTIFICATION_CANDIDATES_REASON_CHECK_SQL: &str = "reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')";
const NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME: &str = "notification_candidates_class_check";
const NOTIFICATION_CANDIDATES_CLASS_VALUES: [&str; 6] = [
    "dm",
    "dm_mention",
    "personal_mention",
    "channel_mention",
    "active_channel_mention",
    "notify_all",
];
const NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL: &str = "class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')";
const NOTIFICATION_OUTBOX_CLASS_CHECK_NAME: &str = "notification_outbox_class_check";
const NOTIFICATION_OUTBOX_CLASS_VALUES: [&str; 6] = [
    "dm",
    "dm_mention",
    "personal_mention",
    "channel_mention",
    "active_channel_mention",
    "notify_all",
];
const NOTIFICATION_OUTBOX_CLASS_CHECK_SQL: &str = "class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')";
const NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME: &str =
    "notification_candidates_suppressed_reason_check";
pub(super) const NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES: [&str; 13] = [
    "xep0357_self",
    "xep0357_no_registration",
    "xep0357_registration_disabled",
    "xep0492_never",
    "xep0492_on_mention_miss",
    "xep0191_blocked",
    "xep0513_noping",
    "xep0513_active_miss",
    "waddle_dnd",
    "provider_rejected",
    "provider_token_expired",
    "xep0357_push_service_degraded",
    "unread_zero_at_publish",
];
const NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_SQL: &str = "suppressed_reason IS NULL OR suppressed_reason IN ('xep0357_self', 'xep0357_no_registration', 'xep0357_registration_disabled', 'xep0492_never', 'xep0492_on_mention_miss', 'xep0191_blocked', 'xep0513_noping', 'xep0513_active_miss', 'waddle_dnd', 'provider_rejected', 'provider_token_expired', 'xep0357_push_service_degraded', 'unread_zero_at_publish')";
const NOTIFICATION_CANDIDATES_INDEXES: [&str; 4] = [
    "idx_notification_candidates_recipient_created",
    "idx_notification_candidates_identity",
    "idx_notification_candidates_pending_worker",
    "idx_notification_candidates_outboxed_prune",
];
const NOTIFICATION_OUTBOX_INDEXES: [&str; 4] = [
    "idx_notification_outbox_queued_coalesce",
    "idx_notification_outbox_conversation_status",
    "idx_notification_outbox_status_next_attempt",
    "idx_notification_outbox_retention_prune",
];

fn notification_candidates_table_sql(i64_type: &str, if_not_exists: bool) -> String {
    let if_not_exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        r#"
        CREATE TABLE {if_not_exists}notification_candidates (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CONSTRAINT {NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME} CHECK ({NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL}),
            reason TEXT NOT NULL CONSTRAINT {NOTIFICATION_CANDIDATES_REASON_CHECK_NAME} CHECK ({NOTIFICATION_CANDIDATES_REASON_CHECK_SQL}),
            created_at_ms {i64_type} NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms {i64_type},
            outboxed_at_ms {i64_type},
            suppressed_reason TEXT CONSTRAINT {NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME} CHECK ({NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_SQL}),
            noping INTEGER NOT NULL DEFAULT 0,
            no_store INTEGER NOT NULL DEFAULT 0,
            no_permanent_store INTEGER NOT NULL DEFAULT 0,
            last_message_body TEXT,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        )
        "#
    )
}

fn notification_outbox_table_sql(i64_type: &str, if_not_exists: bool) -> String {
    let if_not_exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        r#"
        CREATE TABLE {if_not_exists}notification_outbox (
            job_id TEXT PRIMARY KEY,
            recipient_bare_jid TEXT NOT NULL,
            push_service_jid TEXT NOT NULL,
            node TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            sender_jids TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            class TEXT NOT NULL CONSTRAINT {NOTIFICATION_OUTBOX_CLASS_CHECK_NAME} CHECK ({NOTIFICATION_OUTBOX_CLASS_CHECK_SQL}),
            message_count INTEGER NOT NULL,
            context_xml TEXT NOT NULL,
            -- #719 rich XEP-0357 §5.4 summary fields. Both nullable and
            -- written together: `summary_sender_jid` is the
            -- `last-message-sender` (present iff the recipient opted in),
            -- `summary_body` the (hint-stripped) `last-message-body`.
            -- Stored explicitly rather than inferred from the routing
            -- `sender_jid`, so `RichSummary` round-trips 1:1.
            summary_sender_jid TEXT,
            summary_body TEXT,
            status TEXT NOT NULL CHECK (status IN ('queued', 'in-progress', 'published', 'failed')),
            attempt_count INTEGER NOT NULL DEFAULT 0,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            next_attempt_at_ms {i64_type},
            claimed_at_ms {i64_type},
            claim_token TEXT,
            created_at_ms {i64_type} NOT NULL,
            updated_at_ms {i64_type} NOT NULL,
            published_at_ms {i64_type}
        )
        "#
    )
}

/// Returns `true` iff `definition` quotes `value` as a SQL string literal,
/// e.g. `'value'`.
///
/// Postgres' `pg_get_constraintdef` and SQLite's `sqlite_master.sql` both
/// render IN-list literals with single quotes around each enum value
/// (Postgres adds a `::character varying` cast but the leading `'value'`
/// token is the same). Matching against the quoted form prevents false
/// positives where one enum value is a substring of another — e.g.
/// `'dm_mention'` contains the substring `dm`, but the bare token `'dm'`
/// is absent from a CHECK list that only allows `dm_mention`.
fn constraint_definition_quotes_value(definition: &str, value: &str) -> bool {
    let mut needle = String::with_capacity(value.len() + 2);
    needle.push('\'');
    needle.push_str(value);
    needle.push('\'');
    definition.contains(&needle)
}

fn notification_candidates_reason_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("reason")
        && NOTIFICATION_CANDIDATES_REASON_VALUES
            .iter()
            .all(|reason| constraint_definition_quotes_value(&normalized, reason))
}

fn notification_candidates_class_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("class")
        && NOTIFICATION_CANDIDATES_CLASS_VALUES
            .iter()
            .all(|class| constraint_definition_quotes_value(&normalized, class))
}

fn notification_outbox_class_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("class")
        && NOTIFICATION_OUTBOX_CLASS_VALUES
            .iter()
            .all(|class| constraint_definition_quotes_value(&normalized, class))
}

fn notification_candidates_suppressed_reason_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    if !normalized.contains("suppressed_reason") {
        return false;
    }
    let all_present = NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES
        .iter()
        .all(|reason| constraint_definition_quotes_value(&normalized, reason));
    // Reject a stale *superset* constraint. The audit set can SHRINK
    // (e.g. the removed `xep0334_no_store`/`xep0334_no_permanent_store`
    // variants), and a subset-only check would treat an old definition
    // that still lists a dropped label as up-to-date — leaving the DB
    // willing to store `suppressed_reason` values the Rust enum no
    // longer understands. The only single-quoted tokens in the CHECK
    // are the reason values (SQLite `'v'` and Postgres `'v'::...`), so
    // an exact token count pins the constraint to exactly the current
    // set and forces a rebuild when it diverges in either direction.
    let quoted_value_count = normalized.matches('\'').count() / 2;

    all_present && quoted_value_count == NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES.len()
}

impl NotificationOutboxStore {
    pub(super) async fn initialize(&self) -> Result<(), NotificationOutboxError> {
        // Startup invariant: every closed-set typed `SuppressedReason`
        // db value MUST round-trip through `from_db_value`. This is a
        // typed sanity check that the enum, its db values, the schema
        // CHECK constraint, and the prometheus labels are in lockstep.
        // A mismatched build fails fast at process start rather than
        // surfacing as a confusing `CHECK violation` during the first
        // real suppression.
        for reason in SuppressedReason::ALL.iter().copied() {
            let db = reason.as_db_value();
            let decoded = SuppressedReason::from_db_value(db)?;
            if decoded != reason {
                return Err(NotificationOutboxError::InvalidSuppressedReason(format!(
                    "round-trip mismatch for {db}: decoded {decoded:?}",
                )));
            }
        }
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        self.execute(&notification_candidates_table_sql(i64_type, true), ())
            .await?;
        self.query("SELECT sender_jid FROM notification_candidates LIMIT 0", ())
            .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "policy_error_count INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        let candidate_next_attempt_column = format!("next_attempt_at_ms {i64_type}");
        self.add_column_if_missing("notification_candidates", &candidate_next_attempt_column)
            .await?;
        // Reason/class CHECK migrations rebuild the table from a legacy
        // schema; they MUST run before the slice-2a columns are added
        // because the rebuild INSERT only copies the original column
        // set. Adding the slice-2a columns afterward then either creates
        // the column for-the-first-time (legacy upgrade) or is a no-op
        // (cold init, since `notification_candidates_table_sql` already
        // declares them).
        self.migrate_notification_candidates_reason_constraint(i64_type)
            .await?;
        self.migrate_notification_candidates_class_constraint(i64_type)
            .await?;
        self.add_column_if_missing("notification_candidates", "suppressed_reason TEXT")
            .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "noping INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "no_store INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "no_permanent_store INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        // #719: message-body snapshot for the optional XEP-0357 §5.4
        // `last-message-body`. Nullable — dropped when a XEP-0334
        // storage hint applies.
        self.add_column_if_missing("notification_candidates", "last_message_body TEXT")
            .await?;
        self.migrate_notification_candidates_suppressed_reason_constraint(i64_type)
            .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_recipient_created \
             ON notification_candidates (recipient_bare_jid, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_notification_candidates_identity \
             ON notification_candidates (recipient_bare_jid, conversation_jid, thread_id, stanza_id, class)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_pending_worker \
             ON notification_candidates (created_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) \
             WHERE outboxed_at_ms IS NULL",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_outboxed_prune \
             ON notification_candidates (outboxed_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) \
             WHERE outboxed_at_ms IS NOT NULL",
            (),
        )
        .await?;
        self.execute(&notification_outbox_table_sql(i64_type, true), ())
            .await?;
        self.query(
            "SELECT sender_jid, sender_jids FROM notification_outbox LIMIT 0",
            (),
        )
        .await?;
        self.add_column_if_missing("notification_outbox", "claim_token TEXT")
            .await?;
        self.add_column_if_missing(
            "notification_outbox",
            "policy_error_count INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.migrate_notification_outbox_class_constraint(i64_type)
            .await?;
        // #719: T1-resolved XEP-0357 §5.4 rich summary fields.
        // `summary_sender_jid` is the `last-message-sender` (NULL unless
        // the recipient opted in); `summary_body` is the (hint-stripped)
        // `last-message-body`.
        //
        // These ALTERs run AFTER the class-constraint rebuild: that
        // rebuild's INSERT…SELECT only copies the original column set,
        // so columns added before it would be silently dropped on a
        // legacy-CHECK DB (same ordering rule the candidates side
        // documents above).
        self.add_column_if_missing("notification_outbox", "summary_sender_jid TEXT")
            .await?;
        self.add_column_if_missing("notification_outbox", "summary_body TEXT")
            .await?;
        self.execute(
            "DROP INDEX IF EXISTS idx_notification_outbox_queued_coalesce",
            (),
        )
        .await?;
        self.execute(
            "DROP INDEX IF EXISTS idx_notification_outbox_active_coalesce",
            (),
        )
        .await?;
        self.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_notification_outbox_queued_coalesce \
             ON notification_outbox (recipient_bare_jid, push_service_jid, node, conversation_jid, thread_id, class) \
             WHERE status = 'queued'",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_conversation_status \
             ON notification_outbox (recipient_bare_jid, conversation_jid, thread_id, status)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_status_next_attempt \
             ON notification_outbox (status, next_attempt_at_ms, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_retention_prune \
             ON notification_outbox (status, updated_at_ms, job_id) \
             WHERE status IN ('published', 'failed')",
            (),
        )
        .await?;
        Ok(())
    }

    async fn migrate_notification_candidates_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_candidates_reason_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_candidates_reason_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_candidates_reason_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_candidates",
            "reason",
            NOTIFICATION_CANDIDATES_REASON_CHECK_NAME,
            NOTIFICATION_CANDIDATES_REASON_CHECK_SQL,
            notification_candidates_reason_constraint_matches_expected,
        )
        .await
    }

    async fn migrate_sqlite_notification_candidates_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_candidates_reason_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_CANDIDATES_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_old_reason_check",
            (),
        )
        .await?;
        tx.execute(&notification_candidates_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
            r#"
            INSERT INTO notification_candidates (
                recipient_bare_jid,
                conversation_jid,
                sender_jid,
                thread_id,
                stanza_id_by,
                stanza_id,
                class,
                reason,
                created_at_ms,
                policy_error_count,
                next_attempt_at_ms,
                outboxed_at_ms
            )
            SELECT
                recipient_bare_jid,
                conversation_jid,
                sender_jid,
                thread_id,
                stanza_id_by,
                stanza_id,
                class,
                reason,
                created_at_ms,
                policy_error_count,
                next_attempt_at_ms,
                outboxed_at_ms
            FROM notification_candidates_old_reason_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_candidates_old_reason_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_candidates_reason_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_candidates_reason_constraint_matches_expected(
            &create_sql,
        ))
    }

    async fn migrate_notification_candidates_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_candidates_class_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_candidates_class_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_candidates_class_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_candidates",
            "class",
            NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME,
            NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL,
            notification_candidates_class_constraint_matches_expected,
        )
        .await
    }

    async fn migrate_sqlite_notification_candidates_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_candidates_class_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_CANDIDATES_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_old_class_check",
            (),
        )
        .await?;
        tx.execute(&notification_candidates_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
            r#"
            INSERT INTO notification_candidates (
                recipient_bare_jid,
                conversation_jid,
                sender_jid,
                thread_id,
                stanza_id_by,
                stanza_id,
                class,
                reason,
                created_at_ms,
                policy_error_count,
                next_attempt_at_ms,
                outboxed_at_ms
            )
            SELECT
                recipient_bare_jid,
                conversation_jid,
                sender_jid,
                thread_id,
                stanza_id_by,
                stanza_id,
                class,
                reason,
                created_at_ms,
                policy_error_count,
                next_attempt_at_ms,
                outboxed_at_ms
            FROM notification_candidates_old_class_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_candidates_old_class_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_candidates_class_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_candidates_class_constraint_matches_expected(
            &create_sql,
        ))
    }

    async fn migrate_notification_candidates_suppressed_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_candidates_suppressed_reason_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_candidates_suppressed_reason_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_candidates_suppressed_reason_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_candidates",
            "suppressed_reason",
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME,
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_SQL,
            notification_candidates_suppressed_reason_constraint_matches_expected,
        )
        .await
    }

    /// SQLite does not enforce CHECK constraints added after CREATE
    /// TABLE for existing rows, and adding a new CHECK requires a
    /// rebuild. Following the existing pattern, when the current
    /// schema text does not advertise the expected suppressed_reason
    /// CHECK we rebuild via rename-old → create-new → copy.
    async fn migrate_sqlite_notification_candidates_suppressed_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_candidates_suppressed_reason_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_CANDIDATES_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_old_suppressed_reason_check",
            (),
        )
        .await?;
        tx.execute(&notification_candidates_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
            r#"
            INSERT INTO notification_candidates (
                recipient_bare_jid,
                conversation_jid,
                sender_jid,
                thread_id,
                stanza_id_by,
                stanza_id,
                class,
                reason,
                created_at_ms,
                policy_error_count,
                next_attempt_at_ms,
                outboxed_at_ms,
                suppressed_reason,
                noping,
                no_store,
                no_permanent_store
            )
            SELECT
                recipient_bare_jid,
                conversation_jid,
                sender_jid,
                thread_id,
                stanza_id_by,
                stanza_id,
                class,
                reason,
                created_at_ms,
                policy_error_count,
                next_attempt_at_ms,
                outboxed_at_ms,
                suppressed_reason,
                noping,
                no_store,
                no_permanent_store
            FROM notification_candidates_old_suppressed_reason_check
            "#,
            (),
        )
        .await?;
        tx.execute(
            "DROP TABLE notification_candidates_old_suppressed_reason_check",
            (),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_candidates_suppressed_reason_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_candidates_suppressed_reason_constraint_matches_expected(&create_sql))
    }

    async fn migrate_notification_outbox_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_outbox_class_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_outbox_class_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_outbox_class_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_outbox",
            "class",
            NOTIFICATION_OUTBOX_CLASS_CHECK_NAME,
            NOTIFICATION_OUTBOX_CLASS_CHECK_SQL,
            notification_outbox_class_constraint_matches_expected,
        )
        .await
    }

    /// Drops every CHECK constraint on `table.column` whose definition
    /// does NOT match the expected value set, and ensures a single
    /// named CHECK constraint is in place.
    ///
    /// Old schemas (created before this PR via inline
    /// `CHECK (column IN (...))` literals in `CREATE TABLE`) carry
    /// **anonymous** CHECK constraints with autogenerated names like
    /// `notification_candidates_class_check1`. Dropping only the named
    /// constraint we own would leave those anonymous ones in place,
    /// rejecting any newly-added enum value indefinitely. Walking
    /// `pg_constraint` + `pg_attribute` and dropping every
    /// non-matching CHECK on the column closes that gap.
    async fn migrate_postgres_check_constraint_on_column(
        &self,
        table: &str,
        column: &str,
        expected_name: &str,
        expected_check_sql: &str,
        matches_expected: fn(&str) -> bool,
    ) -> Result<(), NotificationOutboxError> {
        let existing = self
            .postgres_check_constraints_on_column(table, column)
            .await?;
        let mut current_named_present = false;
        let mut to_drop: Vec<String> = Vec::new();
        for (conname, definition) in &existing {
            if conname == expected_name && matches_expected(definition) {
                current_named_present = true;
            } else {
                to_drop.push(conname.clone());
            }
        }
        if current_named_present && to_drop.is_empty() {
            return Ok(());
        }
        for conname in &to_drop {
            // Identifier-safe: conname comes from `pg_constraint` and
            // matches the Postgres identifier rules; we additionally
            // quote it to defend against unexpected characters.
            self.execute(
                &format!("ALTER TABLE {table} DROP CONSTRAINT IF EXISTS \"{conname}\""),
                (),
            )
            .await?;
        }
        if !current_named_present {
            self.execute(
                &format!(
                    "ALTER TABLE {table} ADD CONSTRAINT {expected_name} CHECK ({expected_check_sql})"
                ),
                (),
            )
            .await?;
        }
        Ok(())
    }

    /// Returns every CHECK constraint on `table` that references
    /// `column` exclusively, as `(conname, pg_get_constraintdef)`.
    ///
    /// The `conkey = ARRAY[<attnum>]::int2[]` filter narrows to
    /// single-column CHECKs against the target column — multi-column
    /// CHECKs covering other columns are deliberately out of scope
    /// since they encode different invariants.
    async fn postgres_check_constraints_on_column(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Vec<(String, String)>, NotificationOutboxError> {
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

    async fn migrate_sqlite_notification_outbox_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_outbox_class_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_OUTBOX_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_outbox RENAME TO notification_outbox_old_class_check",
            (),
        )
        .await?;
        tx.execute(&notification_outbox_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
            r#"
            INSERT INTO notification_outbox (
                job_id,
                recipient_bare_jid,
                push_service_jid,
                node,
                conversation_jid,
                sender_jid,
                sender_jids,
                thread_id,
                class,
                message_count,
                context_xml,
                status,
                attempt_count,
                policy_error_count,
                last_error,
                next_attempt_at_ms,
                claimed_at_ms,
                claim_token,
                created_at_ms,
                updated_at_ms,
                published_at_ms
            )
            SELECT
                job_id,
                recipient_bare_jid,
                push_service_jid,
                node,
                conversation_jid,
                sender_jid,
                sender_jids,
                thread_id,
                class,
                message_count,
                context_xml,
                status,
                attempt_count,
                policy_error_count,
                last_error,
                next_attempt_at_ms,
                claimed_at_ms,
                claim_token,
                created_at_ms,
                updated_at_ms,
                published_at_ms
            FROM notification_outbox_old_class_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_outbox_old_class_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_outbox_class_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_outbox'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_outbox_class_constraint_matches_expected(
            &create_sql,
        ))
    }

    async fn add_column_if_missing(
        &self,
        table: &str,
        column_def: &str,
    ) -> Result<(), NotificationOutboxError> {
        let alter_sql = match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {column_def}")
            }
            crate::db::DatabaseDriver::Sqlite => {
                format!("ALTER TABLE {table} ADD COLUMN {column_def}")
            }
        };
        if let Err(error) = self.execute(&alter_sql, ()).await {
            let msg = error.to_string().to_lowercase();
            if msg.contains("duplicate column") || msg.contains("already exists") {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_outbox::drain::record_candidate_suppressed_reason_tx;
    use crate::notification_outbox::test_support::*;

    #[test]
    fn postgres_reason_constraint_match_accepts_current_definition() {
        let postgres_definition = "CHECK (((reason)::text = ANY ((ARRAY['offline_dm'::character varying, 'offline_dm_mention'::character varying, 'groupchat_personal_mention'::character varying, 'groupchat_channel_mention'::character varying, 'groupchat_active_channel_mention'::character varying, 'groupchat_notify_all'::character varying])::text[])))";
        assert!(notification_candidates_reason_constraint_matches_expected(
            postgres_definition
        ));
    }

    #[test]
    fn postgres_reason_constraint_match_rejects_legacy_definition() {
        let postgres_definition = "CHECK (((reason)::text = 'offline_dm'::character varying))";
        assert!(!notification_candidates_reason_constraint_matches_expected(
            postgres_definition
        ));
    }

    #[test]
    fn postgres_class_constraint_match_accepts_current_definition() {
        let postgres_definition = "CHECK (((class)::text = ANY ((ARRAY['dm'::character varying, 'dm_mention'::character varying, 'personal_mention'::character varying, 'channel_mention'::character varying, 'active_channel_mention'::character varying, 'notify_all'::character varying])::text[])))";
        assert!(notification_candidates_class_constraint_matches_expected(
            postgres_definition
        ));
        assert!(notification_outbox_class_constraint_matches_expected(
            postgres_definition
        ));
    }

    #[test]
    fn postgres_class_constraint_match_rejects_legacy_definition() {
        let postgres_definition = "CHECK (((class)::text = ANY ((ARRAY['dm'::character varying, 'personal_mention'::character varying])::text[])))";
        assert!(!notification_candidates_class_constraint_matches_expected(
            postgres_definition
        ));
    }

    /// Regression: a constraint definition that contains the substring
    /// `dm` only because the longer value `dm_mention` is present (i.e.
    /// `'dm'` is NOT a quoted literal in the IN-list) must be flagged
    /// stale. Earlier code used `definition.contains("dm")` which
    /// false-positively accepted such a constraint and skipped the
    /// migration — leaving a stale CHECK that rejects new
    /// `'dm'` inserts.
    #[test]
    fn postgres_class_constraint_match_rejects_substring_only_definition() {
        let postgres_definition = "CHECK (((class)::text = ANY ((ARRAY['dm_mention'::character varying, 'personal_mention'::character varying, 'channel_mention'::character varying, 'active_channel_mention'::character varying, 'notify_all'::character varying])::text[])))";
        assert!(
            !notification_candidates_class_constraint_matches_expected(postgres_definition),
            "stale constraint missing 'dm' must NOT be treated as current",
        );
        assert!(
            !notification_outbox_class_constraint_matches_expected(postgres_definition),
            "stale outbox constraint missing 'dm' must NOT be treated as current",
        );
    }

    /// Regression: a SQLite `CREATE TABLE` body that contains the
    /// substring `offline_dm` only because the longer value
    /// `offline_dm_mention` is present must be flagged stale for the
    /// reason CHECK migration.
    #[test]
    fn sqlite_reason_constraint_match_rejects_substring_only_definition() {
        let sqlite_create_sql = "CREATE TABLE notification_candidates (reason TEXT NOT NULL CHECK (reason IN ('offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')))";
        assert!(
            !notification_candidates_reason_constraint_matches_expected(sqlite_create_sql),
            "stale reason constraint missing 'offline_dm' must NOT be treated as current",
        );
    }

    /// Regression for the substring-only defect class extended to the
    /// slice 2a `suppressed_reason` matcher. The `SuppressedReason`
    /// enum has overlapping value families — `provider_rejected` is a
    /// substring of `provider_token_expired`, and `xep0492_never` /
    /// `xep0492_on_mention_miss` share the `xep0492_` prefix. A naïve
    /// `definition.contains(value)` matcher would false-positively
    /// accept a CHECK definition that only allows the longer variant
    /// while claiming to cover the shorter, skipping the migration
    /// and leaving inserts of the missing variant to fail at runtime.
    /// The quoted-literal matcher introduced in slice 1
    /// (commit 3f2b2dcd) must catch this for `suppressed_reason` too.
    #[test]
    fn postgres_suppressed_reason_constraint_match_rejects_substring_only_definition() {
        // Stale Postgres-shape definition that lists ONLY the longer
        // overlapping variants (`provider_token_expired`,
        // `xep0492_on_mention_miss`, `xep0357_no_registration`,
        // `xep0357_registration_disabled`) — every shorter prefix
        // (`provider_rejected`, `xep0492_never`, `xep0357_self`, ...)
        // would substring-match falsely under a naïve `contains`.
        let postgres_definition = "CHECK ((((suppressed_reason)::text = ANY ((ARRAY['xep0492_on_mention_miss'::character varying, 'xep0357_no_registration'::character varying, 'xep0357_registration_disabled'::character varying, 'provider_token_expired'::character varying])::text[]))))";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(postgres_definition),
            "stale Postgres suppressed_reason constraint missing shorter overlapping values must NOT be treated as current",
        );
    }

    /// SQLite-shape parallel of the substring-only regression. A
    /// `CREATE TABLE` body that only quotes the longer overlapping
    /// `SuppressedReason` variants must be flagged stale.
    #[test]
    fn sqlite_suppressed_reason_constraint_match_rejects_substring_only_definition() {
        let sqlite_create_sql = "CREATE TABLE notification_candidates (suppressed_reason TEXT CHECK (suppressed_reason IS NULL OR suppressed_reason IN ('xep0492_on_mention_miss', 'xep0357_no_registration', 'xep0357_registration_disabled', 'provider_token_expired')))";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(sqlite_create_sql),
            "stale SQLite suppressed_reason constraint missing shorter overlapping values must NOT be treated as current",
        );
    }

    /// Postgres-only regression for the anonymous CHECK constraint bug:
    /// schemas created before this PR via inline
    /// `CHECK (class IN (...))` literals in `CREATE TABLE` end up with
    /// **anonymous** CHECK constraints whose name is autogenerated
    /// (e.g. `notification_candidates_class_check1`). The migration
    /// must walk `pg_constraint` and drop every CHECK on the target
    /// column — not just the named one we own — otherwise the
    /// anonymous CHECK keeps rejecting newly-added enum values
    /// (`dm_mention` here) on upgraded deployments.
    ///
    /// Opt-in via `WADDLE_TEST_POSTGRES_URL` since the project's
    /// default test backend is SQLite (which uses a different
    /// CREATE-TABLE-rebuild migration path).
    #[tokio::test]
    async fn store_initialization_drops_anonymous_postgres_class_check_constraint() {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!(
                "skipping: WADDLE_TEST_POSTGRES_URL not set \
                 (postgres-only regression for anonymous CHECK drop)"
            );
            return;
        };

        // Use a UUID-suffixed table name so concurrent runs against
        // the same Postgres do not clobber each other.
        let table_suffix = uuid::Uuid::new_v4().simple().to_string();
        let table = format!("notification_candidates_{table_suffix}");
        let scoped_url = database_url;
        let db = Database::from_config(
            "notification-outbox-anonymous-pg-check",
            &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, scoped_url),
        )
        .await
        .expect("connect postgres");
        let conn = db.guard().await.expect("db guard");

        // Anonymous CHECK: no `CONSTRAINT name` clause means Postgres
        // generates one (e.g. `<table>_class_check`). The legacy class
        // set deliberately excludes `'dm_mention'` to mirror the
        // pre-#526 schema.
        let create_sql = format!(
            r#"
            CREATE TABLE "{table}" (
                recipient_bare_jid TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                sender_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
                created_at_ms BIGINT NOT NULL,
                policy_error_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at_ms BIGINT,
                outboxed_at_ms BIGINT,
                PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
            )
            "#
        );
        conn.execute(&create_sql, ())
            .await
            .expect("create scoped table");

        // Cleanup on test exit: scope-guard pattern via a closure
        // that captures `&db` (we can't use `Drop` because it would
        // require async cleanup).
        let cleanup_table = table.clone();
        let cleanup_db = db.clone();
        let cleanup = async move {
            let conn = cleanup_db.guard().await.expect("cleanup db guard");
            let _ = conn
                .execute(&format!(r#"DROP TABLE IF EXISTS "{cleanup_table}""#), ())
                .await;
        };

        // Sanity check: pre-migration, the anonymous CHECK exists
        // and is named (any non-empty `conname` qualifies — Postgres
        // never emits truly nameless constraints, but autogenerated
        // names are still NOT ours).
        let mut rows = conn
            .query(
                r#"
                SELECT c.conname
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = 'class'
                WHERE c.conrelid = ($1 :: regclass)
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![table.as_str()],
            )
            .await
            .expect("pre-migration check constraint query");
        let mut found_anonymous = false;
        while let Some(row) = rows.next().await.expect("row") {
            let conname: String = row.get(0).expect("conname");
            if conname != NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME {
                found_anonymous = true;
            }
        }
        assert!(
            found_anonymous,
            "test fixture must produce an anonymous (non-canonical-name) CHECK on the class column"
        );
        drop(conn);
        drop(rows);

        // Drive the migration helpers directly against our scoped
        // table. This sidesteps the full `NotificationOutboxStore::new`
        // initialization (which targets the hard-coded
        // `notification_candidates` table name). We migrate BOTH the
        // class and reason anonymous CHECK constraints because the
        // post-migration insert below carries `dm_mention` (new class
        // value) AND `offline_dm_mention` (new reason value) — Postgres
        // enforces every constraint on the row, so leaving either
        // anonymous CHECK in place will reject the insert.
        let store = NotificationOutboxStore { db: db.clone() };
        let class_migrate = store
            .migrate_postgres_check_constraint_on_column(
                &table,
                "class",
                NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME,
                NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL,
                notification_candidates_class_constraint_matches_expected,
            )
            .await;
        if let Err(error) = &class_migrate {
            cleanup.await;
            panic!("class migration failed: {error}");
        }
        let reason_migrate = store
            .migrate_postgres_check_constraint_on_column(
                &table,
                "reason",
                NOTIFICATION_CANDIDATES_REASON_CHECK_NAME,
                NOTIFICATION_CANDIDATES_REASON_CHECK_SQL,
                notification_candidates_reason_constraint_matches_expected,
            )
            .await;
        if let Err(error) = &reason_migrate {
            cleanup.await;
            panic!("reason migration failed: {error}");
        }

        // Post-migration: only the canonical named CHECK should
        // remain on the class column, and it should accept the new
        // `dm_mention` value.
        let conn = db.guard().await.expect("db guard");
        let mut rows = conn
            .query(
                r#"
                SELECT c.conname
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = 'class'
                WHERE c.conrelid = ($1 :: regclass)
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![table.as_str()],
            )
            .await
            .expect("post-migration check constraint query");
        let mut remaining: Vec<String> = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            remaining.push(row.get(0).expect("conname"));
        }
        let canonical_present = remaining
            .iter()
            .any(|n| n == NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME);
        let anonymous_present = remaining
            .iter()
            .any(|n| n != NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME);
        let dm_mention_insert = conn
            .execute(
                &format!(
                    r#"
                    INSERT INTO "{table}" (
                        recipient_bare_jid,
                        conversation_jid,
                        sender_jid,
                        thread_id,
                        stanza_id_by,
                        stanza_id,
                        class,
                        reason,
                        created_at_ms,
                        policy_error_count
                    ) VALUES (
                        'bob@example.com',
                        'alice@example.com',
                        'alice@example.com/web',
                        '',
                        'bob@example.com',
                        'post-migration-mention',
                        'dm_mention',
                        'offline_dm_mention',
                        1,
                        0
                    )
                    "#
                ),
                (),
            )
            .await;
        cleanup.await;
        assert!(
            canonical_present,
            "named CHECK constraint must remain after migration; saw {remaining:?}"
        );
        assert!(
            !anonymous_present,
            "anonymous CHECK constraint(s) must be dropped by migration; saw {remaining:?}"
        );
        dm_mention_insert.expect("dm_mention insert must succeed post-migration");
    }

    #[test]
    fn postgres_suppressed_reason_constraint_match_accepts_current_definition() {
        let postgres_definition = "CHECK (((suppressed_reason IS NULL) OR ((suppressed_reason)::text = ANY ((ARRAY['xep0357_self'::character varying, 'xep0357_no_registration'::character varying, 'xep0357_registration_disabled'::character varying, 'xep0492_never'::character varying, 'xep0492_on_mention_miss'::character varying, 'xep0191_blocked'::character varying, 'xep0513_noping'::character varying, 'xep0513_active_miss'::character varying, 'waddle_dnd'::character varying, 'provider_rejected'::character varying, 'provider_token_expired'::character varying, 'xep0357_push_service_degraded'::character varying, 'unread_zero_at_publish'::character varying])::text[]))))";
        assert!(
            notification_candidates_suppressed_reason_constraint_matches_expected(
                postgres_definition
            )
        );
    }

    #[test]
    fn sqlite_suppressed_reason_constraint_match_rejects_partial_definition() {
        // A schema that advertises only some of the typed reasons must
        // be flagged stale so the migration rebuilds the CHECK.
        let sqlite_create_sql = "CREATE TABLE notification_candidates (suppressed_reason TEXT CHECK (suppressed_reason IS NULL OR suppressed_reason IN ('xep0492_never')))";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(
                sqlite_create_sql
            ),
        );
    }

    #[test]
    fn suppressed_reason_constraint_match_rejects_stale_superset_definition() {
        // #719 / Codex review: when the audit set SHRINKS, a legacy
        // CHECK that still lists a removed label (here the dropped
        // `xep0334_no_store`/`xep0334_no_permanent_store`) must be
        // flagged stale — otherwise the DB keeps accepting
        // `suppressed_reason` values the Rust enum no longer
        // understands. A subset-only matcher would wrongly accept this.
        let superset = "suppressed_reason IS NULL OR suppressed_reason IN ('xep0357_self', 'xep0357_no_registration', 'xep0357_registration_disabled', 'xep0492_never', 'xep0492_on_mention_miss', 'xep0191_blocked', 'xep0513_noping', 'xep0513_active_miss', 'xep0334_no_store', 'xep0334_no_permanent_store', 'waddle_dnd', 'provider_rejected', 'provider_token_expired', 'xep0357_push_service_degraded')";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(superset),
            "a CHECK listing removed labels must be treated as stale so the rebuild fires"
        );
    }

    /// On a fresh store, cold-init MUST advertise the
    /// `suppressed_reason` column with the CHECK constraint that
    /// accepts the full closed-set of typed reasons. A direct INSERT
    /// using each typed db value MUST succeed; an INSERT with a
    /// nonsense value MUST be rejected by the CHECK.
    #[tokio::test]
    async fn suppressed_reason_check_constraint_accepts_every_typed_value() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        // Iterate the closed-set `ALL` array so a future enum extension
        // joins this audit automatically — no hand-maintained parallel
        // list to drift.
        for (idx, reason) in SuppressedReason::ALL.iter().enumerate() {
            let stanza_id = format!("audit-{idx}");
            let candidate = NotificationCandidate::direct_message(
                recipient.clone(),
                sender_jid.clone(),
                StanzaId::new(stanza_id.clone(), Jid::from(recipient.clone())),
                false,
            )
            .expect("candidate");
            assert_eq!(
                store
                    .insert_candidate(&candidate)
                    .await
                    .expect("insert candidate"),
                NotificationCandidateInsertOutcome::Inserted,
            );
            let mut tx = store.db.begin().await.expect("begin tx");
            record_candidate_suppressed_reason_tx(&mut tx, &candidate, *reason)
                .await
                .expect("record reason");
            tx.commit().await.expect("commit");
        }

        // A nonsense value MUST be rejected by the CHECK.
        let insert_result = store
            .execute(
                r#"
                INSERT INTO notification_candidates (
                    recipient_bare_jid, conversation_jid, sender_jid, thread_id,
                    stanza_id_by, stanza_id, class, reason, created_at_ms,
                    policy_error_count, next_attempt_at_ms, outboxed_at_ms,
                    suppressed_reason, noping, no_store, no_permanent_store
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, 0, 0, 0)
                "#,
                crate::db_params![
                    "alice@example.com",
                    "bob@example.com",
                    "bob@example.com/web",
                    "",
                    "alice@example.com",
                    "bad-value",
                    "dm",
                    "offline_dm",
                    1_i64,
                    0_i64,
                    "not-a-real-reason",
                ],
            )
            .await;
        assert!(
            insert_result.is_err(),
            "CHECK constraint must reject nonsense suppressed_reason"
        );
    }

    /// Schema regression: cold-init MUST produce a
    /// `notification_candidates.suppressed_reason` column with the
    /// named CHECK constraint accepting every typed db value.
    #[tokio::test]
    async fn cold_init_creates_suppressed_reason_column_and_check() {
        let store = store().await;
        // Insert a row, then update suppressed_reason to every typed
        // db value in turn — all MUST succeed.
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new("schema-probe", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");
        for reason_db in NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES {
            store
                .execute(
                    "UPDATE notification_candidates SET suppressed_reason = ? WHERE stanza_id = ?",
                    crate::db_params![reason_db, "schema-probe"],
                )
                .await
                .expect("update suppressed_reason");
        }
        // Reset to NULL also OK (unsuppressed/delivered shape).
        store
            .execute(
                "UPDATE notification_candidates SET suppressed_reason = NULL WHERE stanza_id = ?",
                crate::db_params!["schema-probe"],
            )
            .await
            .expect("reset to NULL");
    }
}
