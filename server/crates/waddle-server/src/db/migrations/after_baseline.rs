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

/// Version of the migration that admits offline call notification
/// candidates.
pub const OFFLINE_CALL_NOTIFICATIONS_VERSION: i64 = 2001;

/// Version of the deployed migration for missed-call alerts.
pub const MISSED_CALL_NOTIFICATIONS_VERSION: i64 = 2002;

/// Version of the migration that creates the host-grant authority tables.
pub const HOST_GRANT_AUTHORITY_VERSION: i64 = 2003;

/// Version of the migration that binds the host-grant authority incarnation
/// to the declared restore epoch and the database lineage.
pub const HOST_GRANT_RESTORE_INCARNATION_VERSION: i64 = 2004;

/// Version of the migration that creates the S2S IQ correlation table.
pub const S2S_IQ_CORRELATIONS_VERSION: i64 = 2005;

/// Version of the migration that creates the S2S remote room occupancy
/// table and its room verdict cache.
pub const S2S_REMOTE_ROOMS_VERSION: i64 = 2006;

/// Version of the migration that creates the host-grant revocation fence's
/// replica leases and acknowledgements.
pub const HOST_GRANT_REVOCATION_FENCE_VERSION: i64 = 2007;

/// Version of the migration that creates the ledger of retired account
/// JIDs.
pub const RETIRED_ACCOUNT_JIDS_VERSION: i64 = 2008;

/// Version of the migration that creates the table of the seats remote
/// occupants hold in local rooms.
pub const S2S_LOCAL_ROOM_SEATS_VERSION: i64 = 2009;

/// Version of the migration that adds the federated setting to the channel
/// catalog.
pub const CHANNEL_FEDERATED_VERSION: i64 = 2010;

/// Version of the migration that creates the stores of S2S account
/// traffic: pending subscription requests from remote senders and the
/// remote PubSub nodes local accounts subscribed to.
pub const S2S_ACCOUNT_TRAFFIC_VERSION: i64 = 2011;

/// Version of the migration that creates the offline admissions of
/// messages from remote senders that are not the account's contacts.
pub const S2S_REMOTE_OFFLINE_ADMISSIONS_VERSION: i64 = 2012;

/// Version of the migration that creates the table of the Spaces open to
/// accounts at other servers.
pub const S2S_FEDERATED_SPACES_VERSION: i64 = 2013;

/// Version of the migration that creates the table of the subscriptions
/// remote accounts hold to local Spaces.
pub const S2S_SPACE_SUBSCRIPTIONS_VERSION: i64 = 2014;

/// Version of the migration that creates the requests to end a retired
/// account's open sessions, the replicas' leases and their
/// acknowledgements.
pub const ACCOUNT_SESSION_ENDINGS_VERSION: i64 = 2015;
pub const HOST_GRANT_SPACES_VERSION: i64 = 2016;
pub const S2S_SPACE_SUBSCRIPTION_GRANTS_VERSION: i64 = 2017;
pub const HOST_GRANT_SPACE_BOOKMARK_VERSIONS_VERSION: i64 = 2018;
pub const LOCAL_SPACE_SUBSCRIPTIONS_VERSION: i64 = 2019;
pub const LOCAL_SPACE_NOTIFICATION_ORDER_VERSION: i64 = 2020;
pub const HOST_GRANT_SPACE_CONFIGURATION_VERSION: i64 = 2021;
pub const HOST_GRANT_SPACE_DELETIONS_VERSION: i64 = 2022;

/// The migrations after the baseline, in version order.
pub fn all() -> Vec<Migration> {
    vec![
        Migration {
            version: OFFLINE_CALL_NOTIFICATIONS_VERSION,
            description: "offline call notification candidates".to_owned(),
            sql_sqlite: CALL_NOTIFICATIONS_SQLITE,
            sql_postgres: CALL_NOTIFICATIONS_POSTGRES,
        },
        Migration {
            version: MISSED_CALL_NOTIFICATIONS_VERSION,
            description: "missed call alert notifications".to_owned(),
            sql_sqlite: MISSED_CALL_NOTIFICATIONS_SQLITE,
            sql_postgres: MISSED_CALL_NOTIFICATIONS_POSTGRES,
        },
        Migration {
            version: HOST_GRANT_AUTHORITY_VERSION,
            description: "host-grant authority".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2003_host_grant_authority.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2003_host_grant_authority.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_RESTORE_INCARNATION_VERSION,
            description: "host-grant restore incarnation".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2004_host_grant_restore_incarnation.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2004_host_grant_restore_incarnation.postgres.sql"
            ),
        },
        Migration {
            version: S2S_IQ_CORRELATIONS_VERSION,
            description: "s2s iq correlations".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2005_s2s_iq_correlations.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2005_s2s_iq_correlations.postgres.sql"
            ),
        },
        Migration {
            version: S2S_REMOTE_ROOMS_VERSION,
            description: "s2s remote rooms".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2006_s2s_remote_rooms.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2006_s2s_remote_rooms.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_REVOCATION_FENCE_VERSION,
            description: "host-grant revocation fence".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2007_host_grant_revocation_fence.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2007_host_grant_revocation_fence.postgres.sql"
            ),
        },
        Migration {
            version: RETIRED_ACCOUNT_JIDS_VERSION,
            description: "retired account jids".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2008_retired_account_jids.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2008_retired_account_jids.postgres.sql"
            ),
        },
        Migration {
            version: S2S_LOCAL_ROOM_SEATS_VERSION,
            description: "s2s local room seats".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2009_s2s_local_room_seats.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2009_s2s_local_room_seats.postgres.sql"
            ),
        },
        Migration {
            version: CHANNEL_FEDERATED_VERSION,
            description: "channel federated setting".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2010_channel_federated.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2010_channel_federated.postgres.sql"
            ),
        },
        Migration {
            version: S2S_ACCOUNT_TRAFFIC_VERSION,
            description: "s2s account traffic".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2011_s2s_account_traffic.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2011_s2s_account_traffic.postgres.sql"
            ),
        },
        Migration {
            version: S2S_REMOTE_OFFLINE_ADMISSIONS_VERSION,
            description: "s2s remote offline admissions".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2012_s2s_remote_offline_admissions.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2012_s2s_remote_offline_admissions.postgres.sql"
            ),
        },
        Migration {
            version: S2S_FEDERATED_SPACES_VERSION,
            description: "s2s federated spaces".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2013_s2s_federated_spaces.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2013_s2s_federated_spaces.postgres.sql"
            ),
        },
        Migration {
            version: S2S_SPACE_SUBSCRIPTIONS_VERSION,
            description: "s2s space subscriptions".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2014_s2s_space_subscriptions.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2014_s2s_space_subscriptions.postgres.sql"
            ),
        },
        Migration {
            version: ACCOUNT_SESSION_ENDINGS_VERSION,
            description: "account session endings".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2015_account_session_endings.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2015_account_session_endings.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_SPACES_VERSION,
            description: "host grant spaces".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2016_host_grant_spaces.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2016_host_grant_spaces.postgres.sql"
            ),
        },
        Migration {
            version: S2S_SPACE_SUBSCRIPTION_GRANTS_VERSION,
            description: "s2s space subscription grants".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2017_s2s_space_subscription_grants.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2017_s2s_space_subscription_grants.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_SPACE_BOOKMARK_VERSIONS_VERSION,
            description: "host grant space bookmark versions".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2018_host_grant_space_bookmark_versions.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2018_host_grant_space_bookmark_versions.postgres.sql"
            ),
        },
        Migration {
            version: LOCAL_SPACE_SUBSCRIPTIONS_VERSION,
            description: "local enrolled Space subscriptions".to_owned(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2019_local_space_subscriptions.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2019_local_space_subscriptions.postgres.sql"
            ),
        },
        Migration {
            version: LOCAL_SPACE_NOTIFICATION_ORDER_VERSION,
            description: "local Space notification order".to_owned(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2020_local_space_notification_order.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2020_local_space_notification_order.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_SPACE_CONFIGURATION_VERSION,
            description: "host grant Space configuration".to_owned(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2021_host_grant_space_configuration.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2021_host_grant_space_configuration.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_SPACE_DELETIONS_VERSION,
            description: "host grant Space deletions".to_owned(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2022_host_grant_space_deletions.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2022_host_grant_space_deletions.postgres.sql"
            ),
        },
    ]
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

const MISSED_CALL_NOTIFICATIONS_POSTGRES: &str = r#"
ALTER TABLE notification_candidates
    DROP CONSTRAINT notification_candidates_class_check,
    ADD CONSTRAINT notification_candidates_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call', 'missed_call')),
    DROP CONSTRAINT notification_candidates_reason_check,
    ADD CONSTRAINT notification_candidates_reason_check CHECK (reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all', 'offline_call', 'call_retracted', 'call_unanswered'));

ALTER TABLE notification_outbox
    DROP CONSTRAINT notification_outbox_class_check,
    ADD CONSTRAINT notification_outbox_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call', 'missed_call'));

CREATE UNIQUE INDEX idx_notification_candidates_missed_call
    ON notification_candidates (recipient_bare_jid, conversation_jid, call_sid)
    WHERE class = 'missed_call';
"#;

const MISSED_CALL_NOTIFICATIONS_SQLITE: &str = r#"
CREATE TABLE notification_candidates_missed_call_2002 (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CONSTRAINT notification_candidates_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call', 'missed_call')),
            reason TEXT NOT NULL CONSTRAINT notification_candidates_reason_check CHECK (reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all', 'offline_call', 'call_retracted', 'call_unanswered')),
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

INSERT INTO notification_candidates_missed_call_2002 (recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, reason, created_at_ms, policy_error_count, next_attempt_at_ms, outboxed_at_ms, suppressed_reason, noping, no_store, no_permanent_store, last_message_body, reaction, delivery_id, quarantined_at_ms, quarantined_suppressed_reason, call_sid, call_video)
    SELECT recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, reason, created_at_ms, policy_error_count, next_attempt_at_ms, outboxed_at_ms, suppressed_reason, noping, no_store, no_permanent_store, last_message_body, reaction, delivery_id, quarantined_at_ms, quarantined_suppressed_reason, call_sid, call_video FROM notification_candidates;

DROP TABLE notification_candidates;

ALTER TABLE notification_candidates_missed_call_2002 RENAME TO notification_candidates;

CREATE TABLE notification_outbox_missed_call_2002 (
            job_id TEXT PRIMARY KEY,
            recipient_bare_jid TEXT NOT NULL,
            push_service_jid TEXT NOT NULL,
            node TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            sender_jids TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            class TEXT NOT NULL CONSTRAINT notification_outbox_class_check CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all', 'call', 'missed_call')),
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

INSERT INTO notification_outbox_missed_call_2002 (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, status, attempt_count, policy_error_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms)
    SELECT job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, status, attempt_count, policy_error_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms FROM notification_outbox;

DROP TABLE notification_outbox;

ALTER TABLE notification_outbox_missed_call_2002 RENAME TO notification_outbox;

CREATE UNIQUE INDEX idx_notification_candidates_identity ON notification_candidates (recipient_bare_jid, conversation_jid, thread_id, stanza_id, class);

CREATE INDEX idx_notification_candidates_outboxed_prune ON notification_candidates (outboxed_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) WHERE outboxed_at_ms IS NOT NULL;

CREATE INDEX idx_notification_candidates_pending_worker ON notification_candidates (created_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) WHERE outboxed_at_ms IS NULL;

CREATE INDEX idx_notification_candidates_recipient_created ON notification_candidates (recipient_bare_jid, created_at_ms);

CREATE INDEX idx_notification_outbox_conversation_status ON notification_outbox (recipient_bare_jid, conversation_jid, thread_id, status);

CREATE UNIQUE INDEX "idx_notification_outbox_queued_coalesce" ON "notification_outbox" (recipient_bare_jid, push_service_jid, node, conversation_jid, thread_id, class) WHERE status = 'queued' AND approved_payload_xml IS NULL AND queue_acceptance_may_exist = 0;

CREATE INDEX idx_notification_outbox_retention_prune ON notification_outbox (status, updated_at_ms, job_id) WHERE status IN ('published', 'failed');

CREATE INDEX idx_notification_outbox_status_next_attempt ON notification_outbox (status, next_attempt_at_ms, created_at_ms);

CREATE UNIQUE INDEX idx_notification_candidates_missed_call
    ON notification_candidates (recipient_bare_jid, conversation_jid, call_sid)
    WHERE class = 'missed_call';
"#;

#[cfg(test)]
mod tests {
    use super::super::BASELINE_VERSION;
    use super::{
        all, ACCOUNT_SESSION_ENDINGS_VERSION, CHANNEL_FEDERATED_VERSION,
        HOST_GRANT_AUTHORITY_VERSION, HOST_GRANT_RESTORE_INCARNATION_VERSION,
        HOST_GRANT_REVOCATION_FENCE_VERSION, HOST_GRANT_SPACES_VERSION,
        HOST_GRANT_SPACE_BOOKMARK_VERSIONS_VERSION, HOST_GRANT_SPACE_CONFIGURATION_VERSION,
        HOST_GRANT_SPACE_DELETIONS_VERSION, LOCAL_SPACE_NOTIFICATION_ORDER_VERSION,
        LOCAL_SPACE_SUBSCRIPTIONS_VERSION, MISSED_CALL_NOTIFICATIONS_VERSION,
        OFFLINE_CALL_NOTIFICATIONS_VERSION, RETIRED_ACCOUNT_JIDS_VERSION,
        S2S_ACCOUNT_TRAFFIC_VERSION, S2S_FEDERATED_SPACES_VERSION, S2S_IQ_CORRELATIONS_VERSION,
        S2S_LOCAL_ROOM_SEATS_VERSION, S2S_REMOTE_OFFLINE_ADMISSIONS_VERSION,
        S2S_REMOTE_ROOMS_VERSION, S2S_SPACE_SUBSCRIPTIONS_VERSION,
        S2S_SPACE_SUBSCRIPTION_GRANTS_VERSION,
    };

    #[test]
    fn later_migrations_follow_the_baseline_in_order() {
        let versions: Vec<i64> = all().iter().map(|migration| migration.version).collect();
        assert!(versions.iter().all(|version| *version > BASELINE_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
        // The runner refuses a database that recorded a later version but
        // not an earlier one (`VersionGap`), so a skipped number could never
        // be used once a later migration has shipped: the versions follow
        // the baseline without a gap.
        assert_eq!(versions.first(), Some(&(BASELINE_VERSION + 1)));
        assert!(versions.windows(2).all(|pair| pair[1] == pair[0] + 1));
        assert_eq!(
            versions,
            vec![
                OFFLINE_CALL_NOTIFICATIONS_VERSION,
                MISSED_CALL_NOTIFICATIONS_VERSION,
                HOST_GRANT_AUTHORITY_VERSION,
                HOST_GRANT_RESTORE_INCARNATION_VERSION,
                S2S_IQ_CORRELATIONS_VERSION,
                S2S_REMOTE_ROOMS_VERSION,
                HOST_GRANT_REVOCATION_FENCE_VERSION,
                RETIRED_ACCOUNT_JIDS_VERSION,
                S2S_LOCAL_ROOM_SEATS_VERSION,
                CHANNEL_FEDERATED_VERSION,
                S2S_ACCOUNT_TRAFFIC_VERSION,
                S2S_REMOTE_OFFLINE_ADMISSIONS_VERSION,
                S2S_FEDERATED_SPACES_VERSION,
                S2S_SPACE_SUBSCRIPTIONS_VERSION,
                ACCOUNT_SESSION_ENDINGS_VERSION,
                HOST_GRANT_SPACES_VERSION,
                S2S_SPACE_SUBSCRIPTION_GRANTS_VERSION,
                HOST_GRANT_SPACE_BOOKMARK_VERSIONS_VERSION,
                LOCAL_SPACE_SUBSCRIPTIONS_VERSION,
                LOCAL_SPACE_NOTIFICATION_ORDER_VERSION,
                HOST_GRANT_SPACE_CONFIGURATION_VERSION,
                HOST_GRANT_SPACE_DELETIONS_VERSION,
            ]
        );
    }
}
