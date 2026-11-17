//! Schema changes made after the schema baseline.
//!
//! The baseline (version 2000, [`super::baseline`]) builds the whole schema
//! as it stood when the migration history was retired. Every later change is
//! a migration here, numbered from 2001 upwards, with SQL for both engines,
//! and is applied after the baseline on a fresh database and on a stamped
//! one alike. The baseline itself is never edited: a changed checksum would
//! fail every database that recorded it.
//!
//! A change to the schema must keep the two engines in lockstep; see
//! `docs/planning/2046-schema-baseline.md`, "Keeping the engines in lockstep
//! after the cut".

use super::Migration;

/// The migrations after the baseline, in version order.
pub fn all() -> Vec<Migration> {
    vec![Migration {
        version: 2001,
        description: "offline call notification candidates".to_owned(),
        sql_sqlite: CALL_NOTIFICATIONS_SQLITE,
        sql_postgres: CALL_NOTIFICATIONS_POSTGRES,
    }]
}

const CALL_NOTIFICATIONS_POSTGRES: &str = r#"
ALTER TABLE notification_candidates
    DROP CONSTRAINT IF EXISTS notification_candidates_class_check,
    ADD CONSTRAINT notification_candidates_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call')),
    DROP CONSTRAINT IF EXISTS notification_candidates_reason_check,
    ADD CONSTRAINT notification_candidates_reason_check CHECK (reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all', 'offline_call')),
    ADD COLUMN call_sid TEXT,
    ADD COLUMN call_video INTEGER;

ALTER TABLE notification_outbox
    DROP CONSTRAINT IF EXISTS notification_outbox_class_check,
    ADD CONSTRAINT notification_outbox_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call'));
"#;

const CALL_NOTIFICATIONS_SQLITE: &str = r#"
CREATE TABLE notification_candidates_call_2001 (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CONSTRAINT notification_candidates_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call')),
            reason TEXT NOT NULL CONSTRAINT notification_candidates_reason_check CHECK (reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all', 'offline_call')),
            created_at_ms INTEGER NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms INTEGER,
            outboxed_at_ms INTEGER,
            suppressed_reason TEXT CONSTRAINT notification_candidates_suppressed_reason_check CHECK (suppressed_reason IS NULL OR suppressed_reason IN ('xep0357_self', 'xep0357_no_registration', 'xep0357_registration_disabled', 'xep0492_never', 'xep0492_on_mention_miss', 'xep0191_blocked', 'xep0513_noping', 'xep0513_active_miss', 'waddle_dnd', 'provider_rejected', 'provider_token_expired', 'xep0357_push_service_degraded', 'unread_zero_at_publish', 'policy_retries_exhausted', 'xep0444_reaction')),
            noping INTEGER NOT NULL DEFAULT 0,
            no_store INTEGER NOT NULL DEFAULT 0,
            no_permanent_store INTEGER NOT NULL DEFAULT 0,
            last_message_body TEXT,
            reaction INTEGER NOT NULL DEFAULT 0,
            delivery_id TEXT,
            quarantined_at_ms INTEGER,
            quarantined_suppressed_reason TEXT,
            call_sid TEXT,
            call_video INTEGER,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        );

INSERT INTO notification_candidates_call_2001 (recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, reason, created_at_ms, policy_error_count, next_attempt_at_ms, outboxed_at_ms, suppressed_reason, noping, no_store, no_permanent_store, last_message_body, reaction, delivery_id, quarantined_at_ms, quarantined_suppressed_reason)
    SELECT recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, reason, created_at_ms, policy_error_count, next_attempt_at_ms, outboxed_at_ms, suppressed_reason, noping, no_store, no_permanent_store, last_message_body, reaction, delivery_id, quarantined_at_ms, quarantined_suppressed_reason FROM notification_candidates;

DROP TABLE notification_candidates;

ALTER TABLE notification_candidates_call_2001 RENAME TO notification_candidates;

CREATE TABLE notification_outbox_call_2001 (
            job_id TEXT PRIMARY KEY,
            recipient_bare_jid TEXT NOT NULL,
            push_service_jid TEXT NOT NULL,
            node TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            sender_jids TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            class TEXT NOT NULL CONSTRAINT notification_outbox_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call')),
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
            approved_payload_xml TEXT,
            approved_publish_options_xml TEXT,
            queue_acceptance_may_exist INTEGER NOT NULL DEFAULT 1 CHECK (queue_acceptance_may_exist IN (0, 1)),
            status TEXT NOT NULL CHECK (status IN ('queued', 'in-progress', 'published', 'failed')),
            attempt_count INTEGER NOT NULL DEFAULT 0,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            next_attempt_at_ms INTEGER,
            claimed_at_ms INTEGER,
            claim_token TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            published_at_ms INTEGER
        );

INSERT INTO notification_outbox_call_2001 (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, status, attempt_count, policy_error_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms)
    SELECT job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, status, attempt_count, policy_error_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms FROM notification_outbox;

DROP TABLE notification_outbox;

ALTER TABLE notification_outbox_call_2001 RENAME TO notification_outbox;

CREATE UNIQUE INDEX idx_notification_candidates_identity ON notification_candidates (recipient_bare_jid, conversation_jid, thread_id, stanza_id, class);

CREATE INDEX idx_notification_candidates_outboxed_prune ON notification_candidates (outboxed_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) WHERE outboxed_at_ms IS NOT NULL;

CREATE INDEX idx_notification_candidates_pending_worker ON notification_candidates (created_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) WHERE outboxed_at_ms IS NULL;

CREATE INDEX idx_notification_candidates_recipient_created ON notification_candidates (recipient_bare_jid, created_at_ms);

CREATE INDEX idx_notification_outbox_conversation_status ON notification_outbox (recipient_bare_jid, conversation_jid, thread_id, status);

CREATE UNIQUE INDEX "idx_notification_outbox_queued_coalesce" ON "notification_outbox" (recipient_bare_jid, push_service_jid, node, conversation_jid, thread_id, class) WHERE status = 'queued' AND approved_payload_xml IS NULL AND queue_acceptance_may_exist = 0;

CREATE INDEX idx_notification_outbox_retention_prune ON notification_outbox (status, updated_at_ms, job_id) WHERE status IN ('published', 'failed');

CREATE INDEX idx_notification_outbox_status_next_attempt ON notification_outbox (status, next_attempt_at_ms, created_at_ms);
"#;

#[cfg(test)]
mod tests {
    use super::super::BASELINE_VERSION;
    use super::all;

    #[test]
    fn later_migrations_follow_the_baseline_in_order() {
        let versions: Vec<i64> = all().iter().map(|migration| migration.version).collect();
        assert!(versions.iter().all(|version| *version > BASELINE_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
