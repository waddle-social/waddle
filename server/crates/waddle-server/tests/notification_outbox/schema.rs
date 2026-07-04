//! Storage schema creation, migration, and round-trip tests for the notification outbox.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::Jid;
use waddle_server::db::Database;
use waddle_server::notification_outbox::*;
use waddle_xmpp_core::xep0359::StanzaId;

#[tokio::test]
async fn store_initialization_rejects_candidate_schema_without_sender_provenance_column() {
    let db = Database::in_memory("notification-outbox-missing-candidate-sender")
        .await
        .unwrap();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        r#"
        CREATE TABLE notification_candidates (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL,
            reason TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms INTEGER,
            outboxed_at_ms INTEGER,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        )
        "#,
        (),
    )
    .await
    .expect("create incompatible candidate table");
    drop(conn);

    match NotificationOutboxStore::new(db.clone()).await {
        Ok(_) => panic!("store must not add missing sender provenance candidate columns"),
        Err(error) => assert!(
            error.to_string().contains("sender_jid"),
            "unexpected schema error: {error}"
        ),
    }
}

#[tokio::test]
async fn store_initialization_migrates_legacy_candidate_reason_check() {
    let db = Database::in_memory("notification-outbox-legacy-candidate-reason")
        .await
        .unwrap();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        r#"
        CREATE TABLE notification_candidates (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
            reason TEXT NOT NULL CHECK (reason IN ('offline_dm')),
            created_at_ms INTEGER NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms INTEGER,
            outboxed_at_ms INTEGER,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        )
        "#,
        (),
    )
    .await
    .expect("create legacy candidate table");
    conn.execute(
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
        ) VALUES (
            'bob@example.com',
            'alice@example.com',
            'alice@example.com/web',
            '',
            'bob@example.com',
            'legacy-direct',
            'dm',
            'offline_dm',
            1,
            0,
            NULL,
            NULL
        )
        "#,
        (),
    )
    .await
    .expect("insert legacy direct candidate");
    drop(conn);

    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("store initializes and migrates legacy reason check");
    let recipient = bare("charlie@example.com");
    let room = bare("legacy-reason@muc.example.com");
    let groupchat = groupchat_candidate_for(
        &recipient,
        &room,
        "legacy-reason@muc.example.com/alice"
            .parse()
            .expect("room sender jid"),
        "legacy-group",
        NotificationClass::ChannelMention,
    );

    assert_eq!(
        store
            .insert_candidate(&groupchat)
            .await
            .expect("insert groupchat"),
        NotificationCandidateInsertOutcome::Inserted
    );
    let mut rows = db_query(
        &db,
        "SELECT reason FROM notification_candidates ORDER BY stanza_id",
        (),
    )
    .await
    .expect("query migrated candidates");
    let first = rows
        .next()
        .await
        .expect("first row query")
        .expect("legacy row");
    let second = rows
        .next()
        .await
        .expect("second row query")
        .expect("group row");
    assert_eq!(first.get::<String>(0).expect("legacy reason"), "offline_dm");
    assert_eq!(
        second.get::<String>(0).expect("group reason"),
        "groupchat_channel_mention"
    );
}

/// Regression for slice 1 of #526: a database created before the
/// `dm_mention` class variant existed still has the legacy class
/// CHECK constraint (`dm`/`personal_mention`/`channel_mention`/
/// `active_channel_mention`/`notify_all`). After
/// `NotificationOutboxStore::new` runs the class-constraint
/// migration, the new `dm_mention` variant must be insertable.
#[tokio::test]
async fn store_initialization_migrates_legacy_candidate_class_check() {
    let db = Database::in_memory("notification-outbox-legacy-candidate-class")
        .await
        .unwrap();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        r#"
        CREATE TABLE notification_candidates (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
            reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
            created_at_ms INTEGER NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms INTEGER,
            outboxed_at_ms INTEGER,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        )
        "#,
        (),
    )
    .await
    .expect("create legacy candidate table");
    conn.execute(
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
        ) VALUES (
            'bob@example.com',
            'alice@example.com',
            'alice@example.com/web',
            '',
            'bob@example.com',
            'legacy-direct',
            'dm',
            'offline_dm',
            1,
            0,
            NULL,
            NULL
        )
        "#,
        (),
    )
    .await
    .expect("insert legacy direct candidate");
    drop(conn);

    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("store initializes and migrates legacy class check");
    let recipient = bare("bob@example.com");
    let sender_bare = bare("alice@example.com");
    let mention_candidate = NotificationCandidate::direct_message(
        recipient.clone(),
        "alice@example.com/web".parse().expect("full sender jid"),
        StanzaId::new("post-migration-mention", Jid::from(recipient.clone())),
        true,
    )
    .expect("dm_mention candidate after migration");
    assert_eq!(
        mention_candidate.class(),
        NotificationClass::DirectMessageMention
    );
    assert_eq!(
        store
            .insert_candidate(&mention_candidate)
            .await
            .expect("insert dm_mention candidate post-migration"),
        NotificationCandidateInsertOutcome::Inserted
    );
    let mut rows = db_query(
        &db,
        "SELECT class FROM notification_candidates ORDER BY stanza_id",
        (),
    )
    .await
    .expect("query migrated candidates");
    let first = rows
        .next()
        .await
        .expect("first row query")
        .expect("legacy row");
    let second = rows
        .next()
        .await
        .expect("second row query")
        .expect("dm_mention row");
    assert_eq!(first.get::<String>(0).expect("legacy class"), "dm");
    assert_eq!(
        second.get::<String>(0).expect("dm_mention class"),
        "dm_mention"
    );
    // Touch unused fields to keep them documented as required
    // identity inputs for the candidate row.
    let _ = sender_bare;
}

/// #719 migration regression: a legacy `notification_outbox` table
/// with a stale `class` CHECK (pre-`dm_mention`) and WITHOUT the
/// rich-summary columns (`summary_sender_jid`, `summary_body`) must upgrade
/// cleanly. Store init runs the class-constraint rebuild followed by
/// the column ALTERs (the documented ordering); a legacy queued row
/// must remain decodable afterwards with a minimal rich summary.
#[tokio::test]
async fn store_initialization_migrates_legacy_outbox_without_rich_summary_columns() {
    let db = Database::in_memory("notification-outbox-legacy-rich-summary")
        .await
        .unwrap();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        r#"
        CREATE TABLE notification_outbox (
            job_id TEXT PRIMARY KEY,
            recipient_bare_jid TEXT NOT NULL,
            push_service_jid TEXT NOT NULL,
            node TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            sender_jids TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
            message_count INTEGER NOT NULL,
            context_xml TEXT NOT NULL,
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
        )
        "#,
        (),
    )
    .await
    .expect("create legacy outbox table");
    conn.execute(
        r#"
        INSERT INTO notification_outbox (
            job_id, recipient_bare_jid, push_service_jid, node,
            conversation_jid, sender_jid, sender_jids, thread_id,
            class, message_count, context_xml, status,
            attempt_count, policy_error_count, last_error,
            next_attempt_at_ms, claimed_at_ms, claim_token,
            created_at_ms, updated_at_ms, published_at_ms
        ) VALUES (
            'legacy-job', 'bob@example.com', 'push.example.com', 'web-node',
            'alice@example.com', 'alice@example.com/web',
            '["alice@example.com/web"]', '',
            'dm', 1,
            '<notification xmlns=''urn:xmpp:push:0''/>', 'queued',
            0, 0, NULL, NULL, NULL, NULL, 1, 1, NULL
        )
        "#,
        (),
    )
    .await
    .expect("insert legacy outbox row");
    drop(conn);

    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("store initializes and migrates legacy outbox");

    // The legacy row decodes through the new SELECT/decoder, which
    // reads `summary_sender_jid`/`summary_body` at the appended indices —
    // proving the columns were added by migration.
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_id().as_str(), "legacy-job");
    assert_eq!(jobs[0].rich_summary(), &RichSummary::minimal());
}

/// Round-trip regression for the new `DirectMessageMention` class /
/// `OfflineDirectMessageMention` reason variants introduced in
/// slice 1 of #526. Inserts a DM candidate with `is_mention=true`
/// and asserts the persisted row carries the typed `dm_mention`
/// class and `offline_dm_mention` reason values.
#[tokio::test]
async fn direct_message_mention_class_round_trips_through_storage() {
    let db = Database::in_memory("notification-outbox-dm-mention-roundtrip")
        .await
        .unwrap();
    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("store init");
    let recipient = bare("bob@example.com");
    let plain = NotificationCandidate::direct_message(
        recipient.clone(),
        "alice@example.com/web".parse().expect("full sender jid"),
        StanzaId::new("plain-dm", Jid::from(recipient.clone())),
        false,
    )
    .expect("plain dm candidate");
    let mention = NotificationCandidate::direct_message(
        recipient.clone(),
        "alice@example.com/web".parse().expect("full sender jid"),
        StanzaId::new("mention-dm", Jid::from(recipient.clone())),
        true,
    )
    .expect("dm_mention candidate");
    assert_eq!(plain.class(), NotificationClass::DirectMessage);
    assert_eq!(plain.reason(), NotificationReason::OfflineDirectMessage);
    assert_eq!(mention.class(), NotificationClass::DirectMessageMention);
    assert_eq!(
        mention.reason(),
        NotificationReason::OfflineDirectMessageMention
    );
    assert_eq!(
        store.insert_candidate(&plain).await.expect("insert plain"),
        NotificationCandidateInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .insert_candidate(&mention)
            .await
            .expect("insert mention"),
        NotificationCandidateInsertOutcome::Inserted
    );
    let mut rows = db_query(
        &db,
        "SELECT class, reason FROM notification_candidates ORDER BY stanza_id",
        (),
    )
    .await
    .expect("query round-trip candidates");
    let mention_row = rows
        .next()
        .await
        .expect("first row query")
        .expect("mention row");
    let plain_row = rows
        .next()
        .await
        .expect("second row query")
        .expect("plain row");
    assert_eq!(
        mention_row.get::<String>(0).expect("mention class"),
        "dm_mention"
    );
    assert_eq!(
        mention_row.get::<String>(1).expect("mention reason"),
        "offline_dm_mention"
    );
    assert_eq!(plain_row.get::<String>(0).expect("plain class"), "dm");
    assert_eq!(
        plain_row.get::<String>(1).expect("plain reason"),
        "offline_dm"
    );
}

#[tokio::test]
async fn store_initialization_rejects_outbox_schema_without_sender_provenance_columns() {
    let db = Database::in_memory("notification-outbox-missing-job-senders")
        .await
        .unwrap();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        r#"
        CREATE TABLE notification_outbox (
            job_id TEXT PRIMARY KEY,
            recipient_bare_jid TEXT NOT NULL,
            push_service_jid TEXT NOT NULL,
            node TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            class TEXT NOT NULL,
            message_count INTEGER NOT NULL,
            context_xml TEXT NOT NULL,
            status TEXT NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            next_attempt_at_ms INTEGER,
            claimed_at_ms INTEGER,
            claim_token TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            published_at_ms INTEGER
        )
        "#,
        (),
    )
    .await
    .expect("create incompatible outbox table");
    drop(conn);

    match NotificationOutboxStore::new(db.clone()).await {
        Ok(_) => panic!("store must not add missing sender provenance outbox columns"),
        Err(error) => assert!(
            error.to_string().contains("sender_jid"),
            "unexpected schema error: {error}"
        ),
    }
}

/// Legacy upgrade regression: a database created with an
/// older schema that lacks the `suppressed_reason`, `noping`,
/// `no_store`, and `no_permanent_store` columns MUST upgrade
/// cleanly when `NotificationOutboxStore::new` runs the
/// `add_column_if_missing` + suppressed-reason migration.
/// Both legacy rows AND newly-inserted rows are insertable
/// and decodable after migration.
#[tokio::test]
async fn legacy_schema_upgrade_adds_suppressed_reason_and_hints() {
    let db = Database::in_memory("notification-outbox-legacy-suppressed")
        .await
        .unwrap();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        r#"
        CREATE TABLE notification_candidates (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
            reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
            created_at_ms INTEGER NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms INTEGER,
            outboxed_at_ms INTEGER,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        )
        "#,
        (),
    )
    .await
    .expect("create legacy candidate table");
    conn.execute(
        r#"
        INSERT INTO notification_candidates (
            recipient_bare_jid, conversation_jid, sender_jid, thread_id,
            stanza_id_by, stanza_id, class, reason, created_at_ms,
            policy_error_count, next_attempt_at_ms, outboxed_at_ms
        ) VALUES (
            'alice@example.com', 'bob@example.com', 'bob@example.com/web', '',
            'alice@example.com', 'legacy-row', 'dm', 'offline_dm', 1, 0, NULL, NULL
        )
        "#,
        (),
    )
    .await
    .expect("insert legacy candidate");
    drop(conn);

    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("store migrates legacy schema");
    // Insert a new candidate with the noping bit set; the column
    // must exist and accept the value.
    let recipient = bare("alice@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
    let new_candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("post-upgrade", Jid::from(recipient.clone())),
        true,
        NotificationMessageHints::none()
            .with_noping(true)
            .with_xep0334(true, true),
    )
    .expect("post-upgrade candidate");
    store
        .insert_candidate(&new_candidate)
        .await
        .expect("insert post-upgrade candidate");
    let mut rows = db_query(
            &db,
            "SELECT suppressed_reason, noping, no_store, no_permanent_store FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["post-upgrade"],
        )
        .await
        .expect("query post-upgrade row");
    let row = rows.next().await.expect("row").expect("row exists");
    let reason: Option<String> = row.get(0).expect("reason");
    assert!(reason.is_none());
    assert_eq!(row.get::<i64>(1).expect("noping"), 1);
    assert_eq!(row.get::<i64>(2).expect("no_store"), 1);
    assert_eq!(row.get::<i64>(3).expect("no_permanent_store"), 1);

    // Legacy row's hint columns must default to 0.
    let mut rows = db_query(
            &db,
            "SELECT noping, no_store, no_permanent_store, suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["legacy-row"],
        )
        .await
        .expect("query legacy row");
    let row = rows.next().await.expect("row").expect("legacy row");
    assert_eq!(row.get::<i64>(0).expect("noping"), 0);
    assert_eq!(row.get::<i64>(1).expect("no_store"), 0);
    assert_eq!(row.get::<i64>(2).expect("no_permanent_store"), 0);
    assert!(row.get::<Option<String>>(3).expect("reason").is_none());
}
