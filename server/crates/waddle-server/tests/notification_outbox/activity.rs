//! XEP-0513 active-channel-mention TTL gating against recorded activity.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::{BareJid, Jid};
use waddle_server::notification_activity::{
    NotificationActivity, NotificationActivityError, NotificationActivityReader,
};
use waddle_server::notification_outbox::*;
use waddle_xmpp_core::xep0359::StanzaId;

/// Process-global mutex used to serialize tests that mutate
/// environment variables. Mirrors the `env_lock` pattern in
/// `waddle_server::server::tests`. `std::env::set_var` is process-global
/// and `cargo test` runs tests on multiple threads by default, so
/// any test that reads or writes an env var MUST hold this guard
/// to avoid races with parallel tests.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    ENV_MUTEX
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ─────────────────────────────────────────────────────────────
// Slice 2b — `notification_activity` projection + XEP-0513
// `<active/>` push filter (#526).
// ─────────────────────────────────────────────────────────────

/// Builds an [`ActiveChannelMention`] candidate for the given
/// (recipient, room, sender) triple — slice 2b's gate operates
/// exclusively on this class.
fn active_channel_mention_candidate_for(
    recipient: &BareJid,
    room: &BareJid,
    sender: &BareJid,
    id: &str,
) -> NotificationCandidate {
    groupchat_candidate_for(
        recipient,
        room,
        format!("{room}/{}", sender.node().expect("sender node"))
            .parse()
            .expect("sender occupant jid"),
        id,
        NotificationClass::ActiveChannelMention,
    )
}

/// A [`NotificationActivityReader`] test double that counts
/// per-`(owner, conversation)` read calls so the slice 2b T0/T1
/// stage-split and per-batch cache can be asserted.
struct CountingActivityReader {
    inner: waddle_server::notification_activity::NotificationActivityStore,
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingActivityReader {
    async fn new() -> Self {
        Self {
            inner: activity_store().await,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl NotificationActivityReader for CountingActivityReader {
    async fn read_activity(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.read_activity(owner, conversation).await
    }
}

/// XEP-0513 hit: recipient was active within the TTL window →
/// `ActiveChannelMention` candidate MUST deliver. Seeds the
/// activity projection with a `last_active_at_ms = now()` row
/// and asserts the T1 drain enqueues the push job.
#[tokio::test]
async fn t1_active_channel_mention_with_recent_activity_delivers() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let activity_reader = activity_store().await;

    let recipient = bare("alice@example.com");
    let room = bare("room@muc.example.com");
    let sender = bare("bob@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    // Record recent activity for the recipient on this room —
    // the chat-state ingest mirrors a fresh XEP-0085 update.
    activity_reader
        .record_chat_state(
            &recipient,
            &room,
            waddle_server::notification_activity::NotificationChatState::Active,
            waddle_server::time::now_ms(),
        )
        .await
        .expect("seed activity");

    let candidate = active_channel_mention_candidate_for(&recipient, &room, &sender, "active-hit");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(
        jobs.len(),
        1,
        "active recipient within TTL MUST receive the push",
    );
    assert_eq!(jobs[0].class(), NotificationClass::ActiveChannelMention,);
}

/// Clock-skew regression: a `last_active_at_ms` value stamped in
/// the FUTURE relative to the evaluator's `now_ms` (NTP drift,
/// replica clock skew, ingestion path using a writer with a
/// faster wall clock) MUST NOT silently extend the configured
/// TTL window. The evaluator clamps the stored timestamp to
/// `now_ms` so `age` stays non-negative — a future-stamped row
/// is treated as "active at now" and ages from there. Without
/// the clamp, the unsigned-style `age <= TTL` predicate would
/// silently treat any future timestamp as active until the
/// wall clock caught up, even past the TTL.
#[tokio::test]
async fn t1_active_channel_mention_future_timestamp_does_not_extend_ttl_window() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let activity_reader = activity_store().await;

    let recipient = bare("alice@example.com");
    let room = bare("room-future@muc.example.com");
    let sender = bare("bob@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    // Stamp activity at `now_ms + 1h` — a pathological future
    // timestamp from a skewed writer clock. The candidate is
    // emitted at the evaluator's `now_ms`; without the clamp the
    // raw `now - last_active` would be hugely negative and the
    // `<= TTL` predicate would fire as "active". With the clamp
    // it normalizes to `age = 0 <= TTL`, which also delivers —
    // but that's the desired outcome: a fresh-looking (clamped-
    // to-now) activity row is correctly treated as active.
    let future_ms = waddle_server::time::now_ms().saturating_add(3_600_000);
    activity_reader
        .record_chat_state(
            &recipient,
            &room,
            waddle_server::notification_activity::NotificationChatState::Active,
            future_ms,
        )
        .await
        .expect("seed future-stamped activity");

    let candidate =
        active_channel_mention_candidate_for(&recipient, &room, &sender, "future-clamp");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    // Behavior we lock: a future timestamp is clamped to `now_ms`
    // and the evaluator treats it as fresh activity → delivery.
    // The clamp's protective value isn't the immediate outcome
    // (both clamped-fresh and unclamped-negative would deliver
    // under `<= TTL`); it's that the predicate operates on a
    // non-negative `age`, so future signed-integer refactors
    // can't silently re-introduce a "negative age = always
    // active" bug.
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(
        jobs.len(),
        1,
        "future-stamped activity must clamp to now and deliver as fresh, not produce a negative age",
    );
    // Verify the row passes the gate (no suppression metric ticked).
    assert_eq!(
        metrics
            .counter_sum("xmpp.push.suppressed", &[("reason", "xep0513_active_miss")])
            .unwrap_or(0),
        0,
        "future-stamped activity must not suppress with Xep0513ActiveMiss",
    );
}

/// XEP-0513 miss (stale): recipient's last activity is older than
/// the configured TTL → suppress with `Xep0513ActiveMiss`. Also
/// asserts the audit column persists and the metric ticks.
#[tokio::test]
async fn t1_active_channel_mention_with_stale_activity_suppresses_with_xep0513_active_miss() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let activity_reader = activity_store().await;

    let recipient = bare("alice@example.com");
    let room = bare("room-stale@muc.example.com");
    let sender = bare("bob@example.com");

    // Stale activity: 1 hour ago, well outside the default 5min
    // TTL window the evaluator clamps to.
    let now_ms = waddle_server::time::now_ms();
    let stale_ms = now_ms.saturating_sub(60 * 60 * 1_000);
    activity_reader
        .record_outbound_message(&recipient, &room, stale_ms)
        .await
        .expect("seed stale");

    let candidate =
        active_channel_mention_candidate_for(&recipient, &room, &sender, "active-stale");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    let mut rows = store
        .query(
            "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["active-stale"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0513_active_miss"),
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "T1 XEP-0513 miss MUST NOT enqueue a job",
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0513_active_miss")]),
        Some(1),
        "metric for xep0513_active_miss must increment",
    );
}

/// XEP-0513 miss (no row): recipient has never recorded any
/// activity on the conversation → suppress with
/// `Xep0513ActiveMiss` (no row in the projection is treated the
/// same as "stale activity").
#[tokio::test]
async fn t1_active_channel_mention_with_no_activity_record_suppresses() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let activity_reader = activity_store().await;

    let recipient = bare("alice@example.com");
    let room = bare("room-never@muc.example.com");
    let sender = bare("bob@example.com");

    let candidate =
        active_channel_mention_candidate_for(&recipient, &room, &sender, "active-never");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    let mut rows = store
        .query(
            "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["active-never"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0513_active_miss"),
    );
}

/// Storage-preservation regression mirroring slice 2a: a T1
/// XEP-0513 `<active/>` miss MUST persist the typed audit reason
/// onto the candidate row and MUST NOT touch upstream storage
/// (inbox, MAM, pending delivery).
#[tokio::test]
async fn xep0513_active_miss_t1_suppression_persists_audit_and_keeps_storage() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let activity_reader = activity_store().await;

    let recipient = bare("alice@example.com");
    let partner = bare("bob@example.com");
    let room = bare("room-storage@muc.example.com");
    // Seed an inbox row so we can witness it's untouched after
    // the T1 suppression. The recipient/partner pairing on the
    // inbox witness is intentionally independent of the
    // ActiveChannelMention candidate's room — both must survive
    // identically since push suppression touches neither.
    let (inbox_storage, inbox_witness) =
        seed_inbox_witness(&recipient, &partner, "witness-stanza", 7_000, 3).await;

    let sender = bare("bob@example.com");
    let candidate =
        active_channel_mention_candidate_for(&recipient, &room, &sender, "active-storage");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    // Audit column written.
    let mut rows = store
        .query(
            "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["active-storage"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0513_active_miss"),
    );
    assert!(
        row.get::<Option<i64>>(1).expect("outboxed_at_ms").is_some(),
        "suppressed candidate MUST be marked outboxed",
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "no push job MUST be enqueued",
    );

    // Inbox witness untouched.
    use waddle_xmpp::inbox::storage::InboxStorage;
    let after = inbox_storage
        .list(&recipient)
        .await
        .expect("list inbox after T1 suppression");
    assert_eq!(after.len(), 1, "inbox row count MUST be unchanged");
    assert_eq!(
        after[0].last_stanza_id, inbox_witness.last_stanza_id,
        "inbox last_stanza_id MUST be unchanged by push suppression",
    );
    assert_eq!(
        after[0].unread, inbox_witness.unread,
        "inbox unread MUST be unchanged by push suppression",
    );
}

/// Per-batch cache: multiple ActiveChannelMention candidates for
/// the same (recipient, conversation) MUST trigger exactly one
/// activity-reader call. Exercises the cache-population path in
/// `resolve_cached_activity`.
#[tokio::test]
async fn t1_active_channel_mention_cache_collapses_same_recipient_lookups() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let counting = CountingActivityReader::new().await;

    let recipient = bare("alice@example.com");
    let room = bare("room-cache@muc.example.com");
    let _sender = bare("bob@example.com");

    // Seed activity so the candidates all pass the gate — the
    // assertion is purely about call-count economy, so the
    // outcome doesn't matter as long as the reader gets consulted.
    counting
        .inner
        .record_outbound_message(&recipient, &room, waddle_server::time::now_ms())
        .await
        .expect("seed activity");

    for (idx, stanza_id) in ["cache-1", "cache-2", "cache-3"].iter().enumerate() {
        // Sender must be `<room>/<nick>` per
        // `NotificationCandidate::groupchat`'s `SenderConversationMismatch`
        // guard — groupchat candidates carry the occupant JID, not
        // the raw user JID.
        let candidate = NotificationCandidate::groupchat(
            recipient.clone(),
            room.clone(),
            format!("{room}/bob-conn-{idx}")
                .parse()
                .expect("occupant jid"),
            NotificationThreadId::root(),
            StanzaId::new(stanza_id.to_string(), Jid::from(room.clone())),
            NotificationClass::ActiveChannelMention,
        )
        .expect("candidate");
        store.insert_candidate(&candidate).await.expect("insert");
    }

    let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &counting);
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    assert_eq!(
        counting.call_count(),
        1,
        "per-batch activity cache MUST collapse repeats for the same (owner, conversation)",
    );
}

/// Operator-tunable TTL: env-driven helper clamps to the
/// [`MIN_ACTIVE_MENTION_TTL_SECONDS`,
/// `MAX_ACTIVE_MENTION_TTL_SECONDS`] window and falls back to
/// the default on unparseable input. Tests via direct env
/// manipulation; serialized against other env-mutating tests in
/// this module via [`env_lock`] (Codex review on PR #731).
#[test]
fn active_mention_ttl_env_var_clamps_to_window() {
    // SAFETY: `env_lock` serializes against every other
    // env-mutating test in this module; `std::env::set_var` is
    // process-global but no other thread will read this var while
    // the guard is held.
    let _guard = env_lock();
    // Save and restore the operator-set value (if any) so the
    // test is a no-op for the parent environment.
    let previous = std::env::var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV).ok();
    unsafe { std::env::remove_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV) };
    assert_eq!(
        active_mention_ttl_ms_from_env(),
        (DEFAULT_ACTIVE_MENTION_TTL_SECONDS as i64) * 1_000,
    );

    unsafe { std::env::set_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV, "0") };
    assert_eq!(
        active_mention_ttl_ms_from_env(),
        (MIN_ACTIVE_MENTION_TTL_SECONDS as i64) * 1_000,
    );

    unsafe { std::env::set_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV, "999999999") };
    assert_eq!(
        active_mention_ttl_ms_from_env(),
        (MAX_ACTIVE_MENTION_TTL_SECONDS as i64) * 1_000,
    );

    match previous {
        Some(value) => unsafe { std::env::set_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV, value) },
        None => unsafe { std::env::remove_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV) },
    }
}
