//! T1 candidate drain worker: coalescing, malformed rows, room policy, batch continuation.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::{BareJid, Jid};
use waddle_server::notification_outbox::*;
use waddle_xmpp::push::PushSubscriptionStore;

/// Test stub that returns `Ok(None)` — the "room not currently
/// live" signal that the T1 evaluator must treat as `Unknown`
/// (defer) rather than `Public` (default-OnMention). The counter
/// proves the per-batch cache short-circuits repeat lookups.
struct UnknownRoomPolicy {
    calls: std::sync::atomic::AtomicUsize,
}

impl UnknownRoomPolicy {
    fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl RoomPolicyStore for UnknownRoomPolicy {
    async fn room_members_only(
        &self,
        _room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(None)
    }
}

/// Test stub that returns `Ok(Some(false))` (public) but counts
/// the calls so the per-batch cache can be asserted as effective.
struct CountingPublicRoomPolicy {
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingPublicRoomPolicy {
    fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl RoomPolicyStore for CountingPublicRoomPolicy {
    async fn room_members_only(
        &self,
        _room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(false))
    }
}

#[tokio::test]
async fn candidate_insert_is_idempotent_and_worker_coalesces_distinct_messages() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let first = candidate("archive-1");
    let duplicate = candidate("archive-1");
    let second = candidate("archive-2");
    register_push_target(&push_store, first.recipient_bare_jid(), &target).await;

    assert_eq!(
        store.insert_candidate(&first).await.expect("first insert"),
        NotificationCandidateInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .insert_candidate(&duplicate)
            .await
            .expect("duplicate insert"),
        NotificationCandidateInsertOutcome::Duplicate
    );
    assert_eq!(
        store
            .insert_candidate(&second)
            .await
            .expect("second insert"),
        NotificationCandidateInsertOutcome::Inserted
    );

    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pre-worker jobs")
        .is_empty());
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        2
    );
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 2);
    assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
    assert_eq!(jobs[0].class(), NotificationClass::DirectMessage);
}

#[tokio::test]
async fn candidate_without_first_party_registration_is_suppressed_and_observable() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let candidate = candidate("archive-no-registration");
    store
        .insert_candidate(&candidate)
        .await
        .expect("candidate insert");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidate without registration"),
        1,
    );
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());

    let mut rows = store
        .query(
            "SELECT outboxed_at_ms, suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-no-registration"],
        )
        .await
        .expect("candidate audit query");
    let row = rows
        .next()
        .await
        .expect("candidate audit row")
        .expect("candidate audit row");
    assert!(row.get::<Option<i64>>(0).expect("outboxed_at_ms").is_some());
    assert_eq!(
        row.get::<Option<String>>(1)
            .expect("suppressed_reason")
            .as_deref(),
        Some("xep0357_no_registration"),
    );
    assert_eq!(
        metrics.counter_sum(
            "xmpp.push.suppressed",
            &[("reason", "xep0357_no_registration")]
        ),
        Some(1),
    );
}

/// T1 race-window regression: a candidate inserted while the
/// recipient's XEP-0492 setting said "deliver" must still be
/// suppressed at drain time if the setting flipped to `<never/>`
/// between T0 emission and T1 dispatch.
///
/// The T0 emission gate (in `offline_delivery.rs` /
/// `groupchat_inbox.rs`) catches the common case where the
/// setting was already `<never/>` at message-arrival time and
/// short-circuits the insert — that case is covered by
/// `xep0492_direct_chat_*_persists_no_candidate_row*` over in
/// `tests/messages.rs`. This test exercises the *other*
/// invocation moment of the same shared evaluator function: a
/// row already exists, and the recipient's effective level has
/// since changed.
///
/// Expected behaviour: `drain_pending_candidates_into_outbox`
/// re-evaluates against the fresh projection, marks the row
/// outboxed without enqueuing a job, and returns `processed = 1`.
/// The row exists only briefly during the race window — push
/// output is preserved per the locked Q2 design.
#[tokio::test]
async fn t1_drain_reevaluates_xep0492_when_projection_changes_after_insert() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let candidate = candidate("archive-t1-race-window");
    register_push_target(&push_store, candidate.recipient_bare_jid(), &target).await;
    // Insert with no projection — defaults to `<always/>`, so a
    // T0 evaluator at this moment would say "deliver". The
    // candidate row gets persisted.
    store
        .insert_candidate(&candidate)
        .await
        .expect("candidate insert");
    assert_eq!(
        store
            .count_all_candidates()
            .await
            .expect("post-insert row count"),
        1,
        "T0 must have persisted the candidate row when projection said deliver"
    );

    // Race window: between T0 emission and T1 drain, the
    // recipient's XEP-0492 setting flips to `<never/>`.
    projection
        .upsert(&waddle_server::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: candidate.recipient_bare_jid().clone(),
            conversation_jid: candidate.conversation_jid().clone(),
            conversation_kind:
                waddle_server::notification_settings_projection::ConversationKind::Direct,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: waddle_server::time::now_ms(),
            source:
                waddle_server::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: candidate.conversation_jid().clone(),
        })
        .await
        .expect("flip xep-0492 setting to <never/>");

    // T1 drain re-evaluates against the now-`<never/>`
    // projection and suppresses the candidate.
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        1,
        "T1 race-window guard MUST count the candidate as processed via suppression"
    );

    // No outbox job — the suppression path goes
    // `mark_candidate_outboxed_tx` WITHOUT `enqueue_outbox_job_tx`.
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
    // The candidate row still exists, now marked outboxed —
    // this is the documented race-window exception. The
    // compliance rule is "no row for the common case"; the
    // race-window row is acceptable per locked Q2.
    let mut rows = store
        .query(
            "SELECT outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-t1-race-window"],
        )
        .await
        .expect("candidate marker query");
    let row = rows
        .next()
        .await
        .expect("candidate marker row")
        .expect("candidate marker row");
    assert!(
        row.get::<Option<i64>>(0)
            .expect("outboxed marker")
            .is_some(),
        "T1 race-window suppression MUST mark the candidate outboxed"
    );
}

#[tokio::test]
async fn candidate_worker_marks_malformed_bare_sender_candidate_terminal() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;
    let candidate = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-malformed-candidate-bare-sender",
    );
    store
        .insert_candidate(&candidate)
        .await
        .expect("candidate insert");
    store
        .execute(
            "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
            waddle_server::db_params!["bob@example.com", "archive-malformed-candidate-bare-sender"],
        )
        .await
        .expect("make candidate sender malformed");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        0
    );

    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
    assert!(store
        .pending_candidates(16)
        .await
        .expect("pending candidates")
        .is_empty());
    let mut rows = store
        .query(
            "SELECT outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-malformed-candidate-bare-sender"],
        )
        .await
        .expect("candidate marker query");
    let row = rows
        .next()
        .await
        .expect("candidate marker row")
        .expect("candidate marker row");
    assert!(row
        .get::<Option<i64>>(0)
        .expect("outboxed marker")
        .is_some());
}

#[tokio::test]
async fn candidate_worker_marks_empty_sender_candidate_terminal_without_conversation_fallback() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;
    let candidate = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-malformed-candidate-empty-sender",
    );
    store
        .insert_candidate(&candidate)
        .await
        .expect("candidate insert");
    store
        .execute(
            "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
            waddle_server::db_params!["", "archive-malformed-candidate-empty-sender"],
        )
        .await
        .expect("make candidate sender empty");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        0
    );

    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
    assert!(store
        .pending_candidates(16)
        .await
        .expect("pending candidates")
        .is_empty());
    let mut rows = store
        .query(
            "SELECT outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-malformed-candidate-empty-sender"],
        )
        .await
        .expect("candidate marker query");
    let row = rows
        .next()
        .await
        .expect("candidate marker row")
        .expect("candidate marker row");
    assert!(row
        .get::<Option<i64>>(0)
        .expect("outboxed marker")
        .is_some());
}

#[tokio::test]
async fn candidate_worker_marks_mismatched_sender_candidate_terminal() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;
    let candidate = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-malformed-candidate-mismatch",
    );
    store
        .insert_candidate(&candidate)
        .await
        .expect("candidate insert");
    store
        .execute(
            "UPDATE notification_candidates SET conversation_jid = ? WHERE stanza_id = ?",
            waddle_server::db_params!["carol@example.com", "archive-malformed-candidate-mismatch"],
        )
        .await
        .expect("make candidate sender mismatch conversation");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        0
    );

    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
    assert!(store
        .pending_candidates(16)
        .await
        .expect("pending candidates")
        .is_empty());
}

#[tokio::test]
async fn candidate_worker_marks_malformed_conversation_candidate_terminal_and_continues() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let malformed_recipient = bare("alice@example.com");
    let valid_recipient = bare("carol@example.com");
    register_push_target(&push_store, &malformed_recipient, &target).await;
    register_push_target(&push_store, &valid_recipient, &target).await;
    let malformed = candidate_for_sender_jid(
        &malformed_recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-malformed-candidate-conversation",
    );
    let valid = candidate_for_sender_jid(
        &valid_recipient,
        "dave@example.com/phone".parse().expect("valid sender"),
        "archive-valid-after-malformed-candidate",
    );
    store
        .insert_candidate(&malformed)
        .await
        .expect("malformed candidate insert");
    store
        .insert_candidate(&valid)
        .await
        .expect("valid candidate insert");
    store
        .execute(
            "UPDATE notification_candidates SET conversation_jid = ? WHERE stanza_id = ?",
            waddle_server::db_params!["not a jid", "archive-malformed-candidate-conversation"],
        )
        .await
        .expect("make candidate conversation malformed");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        1
    );

    assert!(store
        .pending_candidates(16)
        .await
        .expect("pending candidates")
        .is_empty());
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &valid_recipient);
}

#[tokio::test]
async fn candidate_worker_coalesces_distinct_sender_resources_into_one_bare_conversation_job() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-bob-phone",
    );
    let laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-bob-laptop",
    );
    assert_eq!(
        store.insert_candidate(&phone).await.expect("phone insert"),
        NotificationCandidateInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert"),
        NotificationCandidateInsertOutcome::Inserted
    );

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        2
    );

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
    assert_eq!(jobs[0].message_count(), 2);
    let mut sender_jids = jobs[0]
        .sender_jids()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    sender_jids.sort();
    assert_eq!(
        sender_jids,
        vec![
            "bob@example.com/laptop".to_string(),
            "bob@example.com/phone".to_string(),
        ]
    );
}

#[tokio::test]
async fn candidate_worker_fails_malformed_coalesced_job_before_requeueing_exact_sender() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-malformed-existing-phone",
    );
    store.insert_candidate(&phone).await.expect("phone insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain first candidate"),
        1
    );
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params!["bob@example.com", "[]"],
        )
        .await
        .expect("make queued job malformed");

    let laptop_sender = "bob@example.com/laptop".parse().expect("laptop sender");
    let laptop = candidate_for_sender_jid(
        &recipient,
        laptop_sender,
        "archive-malformed-existing-laptop",
    );
    store
        .insert_candidate(&laptop)
        .await
        .expect("laptop insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain second candidate"),
        1
    );

    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 1);
    assert_eq!(
        jobs[0].sender_jids(),
        &["bob@example.com/laptop"
            .parse::<Jid>()
            .expect("laptop sender")]
    );
}

#[tokio::test]
async fn candidate_worker_fails_malformed_sender_jids_json_before_requeueing_exact_sender() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-malformed-json-phone",
    );
    store.insert_candidate(&phone).await.expect("phone insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain first candidate"),
        1
    );
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params!["bob@example.com/phone", "not-json"],
        )
        .await
        .expect("make queued job sender_jids malformed");

    let laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-malformed-json-laptop",
    );
    store
        .insert_candidate(&laptop)
        .await
        .expect("laptop insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain second candidate"),
        1
    );

    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 1);
    assert_eq!(
        jobs[0].sender_jids(),
        &["bob@example.com/laptop"
            .parse::<Jid>()
            .expect("laptop sender")]
    );
}

#[tokio::test]
async fn candidate_worker_fails_sender_set_missing_scalar_before_requeueing_exact_sender() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-missing-scalar-phone",
    );
    store.insert_candidate(&phone).await.expect("phone insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain first candidate"),
        1
    );
    store
        .execute(
            "UPDATE notification_outbox SET sender_jids = ?",
            waddle_server::db_params!["[\"bob@example.com/laptop\"]"],
        )
        .await
        .expect("make queued job sender_jids omit scalar sender");

    let laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-missing-scalar-laptop",
    );
    store
        .insert_candidate(&laptop)
        .await
        .expect("laptop insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain second candidate"),
        1
    );

    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 1);
    assert_eq!(
        jobs[0].sender_jids(),
        &["bob@example.com/laptop"
            .parse::<Jid>()
            .expect("laptop sender")]
    );
}

#[tokio::test]
async fn candidate_worker_fails_semantically_invalid_sender_jids_before_requeueing_exact_sender() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-invalid-sender-jids-phone",
    );
    store.insert_candidate(&phone).await.expect("phone insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain first candidate"),
        1
    );
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params!["bob@example.com/phone", "[\"carol@example.com/phone\"]"],
        )
        .await
        .expect("make queued job sender_jids semantically invalid");

    let laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-invalid-sender-jids-laptop",
    );
    store
        .insert_candidate(&laptop)
        .await
        .expect("laptop insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain second candidate"),
        1
    );

    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 1);
    assert_eq!(
        jobs[0].sender_jids(),
        &["bob@example.com/laptop"
            .parse::<Jid>()
            .expect("laptop sender")]
    );
}

#[tokio::test]
async fn candidate_worker_fails_mismatched_scalar_sender_before_requeueing_exact_sender() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-invalid-scalar-phone",
    );
    store.insert_candidate(&phone).await.expect("phone insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain first candidate"),
        1
    );
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?",
            waddle_server::db_params!["carol@example.com/phone"],
        )
        .await
        .expect("make queued job scalar sender invalid");

    let laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-invalid-scalar-laptop",
    );
    store
        .insert_candidate(&laptop)
        .await
        .expect("laptop insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain second candidate"),
        1
    );

    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 1);
    assert_eq!(
        jobs[0].sender_jids(),
        &["bob@example.com/laptop"
            .parse::<Jid>()
            .expect("laptop sender")]
    );
}

#[tokio::test]
async fn candidate_worker_skips_malformed_registration_and_continues_batch() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let alice = bare("alice@example.com");
    let carol = bare("carol@example.com");
    let bob = bare("bob@example.com");
    let bad_candidate = candidate_for(&alice, &bob, "archive-bad-target");
    let good_candidate = candidate_for(&carol, &bob, "archive-good-target");

    push_store
        .register(waddle_xmpp::push::PushSubscription {
            user_jid: alice.to_string(),
            service_jid: "push.example.com".to_string(),
            node: Some(String::new()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("register malformed push target");
    register_push_target(&push_store, &carol, &target).await;

    assert_eq!(
        store
            .insert_candidate(&bad_candidate)
            .await
            .expect("bad candidate insert"),
        NotificationCandidateInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .insert_candidate(&good_candidate)
            .await
            .expect("good candidate insert"),
        NotificationCandidateInsertOutcome::Inserted
    );

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        2
    );

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &carol);
    assert_eq!(jobs[0].message_count(), 1);
}

#[tokio::test]
async fn candidate_worker_defers_candidates_fail_closed_when_blocklist_load_fails() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;
    store
        .insert_candidate(&candidate_for(
            &recipient,
            &bare("bob@example.com"),
            "archive-policy-error-1",
        ))
        .await
        .expect("first insert");
    store
        .insert_candidate(&candidate_for(
            &recipient,
            &bare("carol@example.com"),
            "archive-policy-error-2",
        ))
        .await
        .expect("second insert");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &FailingBlockingStorage,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        0
    );

    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
    assert!(store
        .pending_candidates(16)
        .await
        .expect("backed-off pending candidates")
        .is_empty());

    let mut rows = store
        .query(
            "SELECT policy_error_count FROM notification_candidates ORDER BY stanza_id",
            (),
        )
        .await
        .expect("policy count query");
    let mut policy_error_counts = Vec::new();
    while let Some(row) = rows.next().await.expect("policy count row") {
        policy_error_counts.push(row.get::<i64>(0).expect("policy count"));
    }
    assert_eq!(policy_error_counts, vec![1, 1]);

    store
        .execute(
            "UPDATE notification_candidates SET next_attempt_at_ms = NULL",
            (),
        )
        .await
        .expect("release backoff");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("retry drain candidates"),
        2
    );
    assert_eq!(
        store
            .pending_outbox_jobs()
            .await
            .expect("retried pending jobs")
            .len(),
        2
    );
}

/// Regression for the unknown-room-policy deferral behavior. When
/// the [`RoomPolicyStore`] returns `Ok(None)` (room actor not
/// currently live), the T1 evaluator MUST defer the candidate via
/// the policy-error backoff rather than silently defaulting to
/// public — see [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`].
/// Dropped pushes for members-only rooms (`Always` default level
/// → `NotifyAll` candidates SHOULD push) would otherwise be the
/// blast radius.
#[tokio::test]
async fn unknown_room_policy_defers_groupchat_candidate_at_t1() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = UnknownRoomPolicy::new();
    let recipient = bare("alice@example.com");
    let room = bare("team@muc.example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let candidate = groupchat_candidate_for(
        &recipient,
        &room,
        "team@muc.example.com/bob".parse().expect("room occupant"),
        "archive-unknown-policy",
        NotificationClass::NotifyAll,
    );
    store
        .insert_candidate(&candidate)
        .await
        .expect("groupchat insert");

    // Drain returns 0 processed because the candidate deferred
    // (not marked outboxed, not enqueued).
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        0,
        "unknown room policy must NOT count as a processed candidate",
    );

    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "unknown room policy must NOT enqueue a push job",
    );
    // The candidate is still un-outboxed but has its
    // policy_error_count incremented and next_attempt_at_ms set
    // in the future, so it is NOT pending right now but WILL be
    // retried by the next drain pass after the backoff elapses.
    let mut rows = store
        .query(
            "SELECT policy_error_count, next_attempt_at_ms, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-unknown-policy"],
        )
        .await
        .expect("candidate row query");
    let row = rows
        .next()
        .await
        .expect("candidate row read")
        .expect("candidate row");
    let policy_error_count: i64 = row.get(0).expect("policy_error_count");
    let next_attempt: Option<i64> = row.get(1).expect("next_attempt_at_ms");
    let outboxed: Option<i64> = row.get(2).expect("outboxed_at_ms");
    assert_eq!(
        policy_error_count, 1,
        "deferral must bump policy_error_count",
    );
    assert!(
        next_attempt.is_some(),
        "deferral must schedule a retry via next_attempt_at_ms",
    );
    assert!(
        outboxed.is_none(),
        "deferral must NOT mark the candidate outboxed",
    );
}

#[tokio::test]
async fn policy_deferral_cap_dead_letters_candidate_and_unblocks_fresh_work() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = UnknownRoomPolicy::new();
    let recipient = bare("alice@example.com");
    let room = bare("team@muc.example.com");
    register_push_target(&push_store, &recipient, &target).await;

    let stuck_candidate = groupchat_candidate_for(
        &recipient,
        &room,
        "team@muc.example.com/bob".parse().expect("room occupant"),
        "archive-policy-cap",
        NotificationClass::NotifyAll,
    );
    store
        .insert_candidate(&stuck_candidate)
        .await
        .expect("groupchat insert");
    store
        .execute(
            "UPDATE notification_candidates SET policy_error_count = 47, next_attempt_at_ms = NULL WHERE stanza_id = ?",
            waddle_server::db_params!["archive-policy-cap"],
        )
        .await
        .expect("prime final policy attempt");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain capped candidate"),
        1,
        "terminal policy retry exhaustion must count as processed",
    );

    let mut rows = store
        .query(
            "SELECT policy_error_count, outboxed_at_ms, suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-policy-cap"],
        )
        .await
        .expect("candidate row query");
    let row = rows
        .next()
        .await
        .expect("candidate row read")
        .expect("candidate row");
    assert_eq!(row.get::<i64>(0).expect("policy_error_count"), 48);
    assert!(
        row.get::<Option<i64>>(1).expect("outboxed_at_ms").is_some(),
        "retry-exhausted candidate must be marked outboxed for retention pruning",
    );
    assert_eq!(
        row.get::<Option<String>>(2)
            .expect("suppressed_reason")
            .as_deref(),
        Some("policy_retries_exhausted"),
    );
    assert_eq!(
        metrics.counter_sum(
            "xmpp.push.suppressed",
            &[("reason", "policy_retries_exhausted")]
        ),
        Some(1),
        "policy retry exhaustion counter must increment",
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "dead-lettering must not enqueue a push job",
    );

    let fresh_candidate = candidate_for(
        &recipient,
        &bare("bob@example.com"),
        "archive-fresh-after-policy-cap",
    );
    store
        .insert_candidate(&fresh_candidate)
        .await
        .expect("fresh candidate insert");
    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain fresh candidate"),
        1,
        "fresh candidate must drain after the stuck row is dead-lettered",
    );
    let jobs = store.pending_outbox_jobs().await.expect("fresh jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &recipient);
}

/// The per-batch room-policy cache MUST collapse repeat lookups
/// for the same room into a single [`RoomPolicyStore`]
/// round-trip. With one room and N candidates, only one actor
/// call is permitted.
#[tokio::test]
async fn room_policy_lookup_is_cached_within_drain_batch() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = CountingPublicRoomPolicy::new();
    let recipient = bare("alice@example.com");
    let room = bare("team@muc.example.com");
    register_push_target(&push_store, &recipient, &target).await;

    // Three distinct groupchat candidates for the *same* room
    // (different archive ids, all PersonalMention so they hit
    // the room-policy path).
    for id in ["arc-1", "arc-2", "arc-3"] {
        let candidate = groupchat_candidate_for(
            &recipient,
            &room,
            "team@muc.example.com/bob".parse().expect("room occupant"),
            id,
            NotificationClass::PersonalMention,
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("groupchat insert");
    }

    let _ = store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    assert_eq!(
        room_policy.call_count(),
        1,
        "per-batch room-policy cache must collapse repeat lookups for the same room",
    );
}

/// The per-batch deferral cache MUST short-circuit
/// `RoomPolicyCacheEntry::Unknown` once observed — subsequent
/// candidates for the same room in the same batch SHOULD reuse
/// the deferral outcome instead of re-asking a failing actor.
#[tokio::test]
async fn unknown_room_policy_lookup_is_cached_within_drain_batch() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = UnknownRoomPolicy::new();
    let recipient = bare("alice@example.com");
    let room = bare("team@muc.example.com");
    register_push_target(&push_store, &recipient, &target).await;

    for id in ["arc-1", "arc-2", "arc-3"] {
        let candidate = groupchat_candidate_for(
            &recipient,
            &room,
            "team@muc.example.com/bob".parse().expect("room occupant"),
            id,
            NotificationClass::NotifyAll,
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("groupchat insert");
    }

    let _ = store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain candidates");

    assert_eq!(
        room_policy.call_count(),
        1,
        "deferral outcomes must be cached per-batch — failing actor must NOT be re-queried per candidate",
    );
}
