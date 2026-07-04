//! Shared fixtures for the notification outbox unit tests.

use super::drain::{enqueue_outbox_job_tx, mark_candidate_outboxed_tx};
use super::publish::OUTBOX_CLAIM_TIMEOUT_MS;
use super::*;
use crate::db::Database;

pub(super) fn bare(raw: &str) -> BareJid {
    raw.parse().expect("bare jid")
}

pub(super) fn candidate(id: &str) -> NotificationCandidate {
    candidate_for(&bare("alice@example.com"), &bare("bob@example.com"), id)
}

pub(super) fn candidate_for(
    recipient: &BareJid,
    sender: &BareJid,
    id: &str,
) -> NotificationCandidate {
    candidate_for_sender_jid(
        recipient,
        format!("{sender}/test-resource")
            .parse()
            .expect("full sender jid"),
        id,
    )
}

pub(super) fn candidate_for_sender_jid(
    recipient: &BareJid,
    sender_jid: Jid,
    id: &str,
) -> NotificationCandidate {
    NotificationCandidate::direct_message(
        recipient.clone(),
        sender_jid,
        StanzaId::new(id, Jid::from(recipient.clone())),
        false,
    )
    .expect("candidate")
}

pub(super) fn groupchat_candidate_for(
    recipient: &BareJid,
    room: &BareJid,
    sender_jid: Jid,
    id: &str,
    class: NotificationClass,
) -> NotificationCandidate {
    NotificationCandidate::groupchat(
        recipient.clone(),
        room.clone(),
        sender_jid,
        NotificationThreadId::root(),
        StanzaId::new(id, Jid::from(room.clone())),
        class,
    )
    .expect("groupchat candidate")
}

pub(super) fn target() -> NotificationOutboxTarget {
    target_named("web-node")
}

pub(super) fn target_named(node: &str) -> NotificationOutboxTarget {
    NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::new(node).expect("node"),
    )
}

pub(super) async fn store() -> NotificationOutboxStore {
    NotificationOutboxStore::new(Database::in_memory("notification-outbox").await.unwrap())
        .await
        .expect("store")
}

/// Default activity-reader for tests that do not exercise the
/// XEP-0513 `<active/>` filter. Returns `Ok(None)` for every
/// lookup so the T1 evaluator treats every recipient as inactive
/// — but the XEP-0513 gate is only consulted for the
/// `ActiveChannelMention` class, so other class tests are
/// unaffected.
///
/// Static so a `&NoopActivityReader` borrow stays valid for the
/// duration of every call site without needing a `let binding =`
/// dance at each invocation.
pub(super) static NOOP_ACTIVITY_READER: crate::notification_activity::NoopActivityReader =
    crate::notification_activity::NoopActivityReader;
pub(super) fn noop_activity_reader() -> &'static crate::notification_activity::NoopActivityReader {
    &NOOP_ACTIVITY_READER
}

/// Cache triple held by every direct-evaluator test call site.
/// Extracted as a `type` alias to keep clippy's `type_complexity`
/// lint quiet without leaking `#[allow]` into the codebase.
pub(super) type FreshEvalCaches = (
    std::collections::BTreeMap<BareJid, RoomPolicyCacheEntry>,
    std::collections::BTreeMap<BareJid, DndState>,
    std::collections::BTreeMap<(BareJid, BareJid), Option<NotificationActivity>>,
);

/// Build a fresh [`PushEvalDeps`] / [`PushEvalCaches`] pair for
/// unit-testing the typed evaluator function directly. Returns
/// owned caches so each call site can hold them and pass `&mut`.
pub(super) fn fresh_eval_caches() -> FreshEvalCaches {
    (
        std::collections::BTreeMap::new(),
        std::collections::BTreeMap::new(),
        std::collections::BTreeMap::new(),
    )
}

pub(super) fn eval_deps_for_test<'a>(
    settings_projection:
        &'a crate::notification_settings_projection::NotificationSettingsProjectionStore,
    room_policy: &'a dyn RoomPolicyStore,
    dnd_reader: &'a dyn DndReader,
    activity_reader: &'a dyn NotificationActivityReader,
) -> PushEvalDeps<'a> {
    PushEvalDeps {
        settings_projection,
        room_policy,
        dnd_reader,
        activity_reader,
        active_mention_ttl_ms: 5 * 60 * 1_000,
    }
}

/// Test double for [`RoomPolicyStore`] that pretends every room is
/// public (`members_only = false`). Slice 1's tests do not exercise
/// private-room dispatch policy; when slice 2 adds those paths it
/// will grow this stub (or replace it with a richer fixture).
pub(super) struct StubRoomPolicy;

impl StubRoomPolicy {
    pub(super) fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl RoomPolicyStore for StubRoomPolicy {
    async fn room_members_only(
        &self,
        _room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        Ok(Some(false))
    }
}

/// Test stub that returns `Ok(None)` — the "room not currently
/// live" signal that the T1 evaluator must treat as `Unknown`
/// (defer) rather than `Public` (default-OnMention). The counter
/// proves the per-batch cache short-circuits repeat lookups.
pub(super) struct UnknownRoomPolicy {
    calls: std::sync::atomic::AtomicUsize,
}

impl UnknownRoomPolicy {
    pub(super) fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
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

/// Test stub that always returns a typed `RoomPolicyLookup` error
/// — models actor mailbox / transport failures. Counts the calls
/// so we can assert the per-batch cache short-circuits subsequent
/// lookups (one error → one cache entry → one warn → many silent
/// reuses, never re-asking the failing dependency).
pub(super) struct ErroringRoomPolicy {
    calls: std::sync::atomic::AtomicUsize,
}

impl ErroringRoomPolicy {
    pub(super) fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(super) fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl RoomPolicyStore for ErroringRoomPolicy {
    async fn room_members_only(
        &self,
        room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(NotificationOutboxError::RoomPolicyLookup {
            room: room.clone(),
            message: "test-fixture: simulated actor mailbox failure".to_string(),
        })
    }
}

pub(super) async fn settings_projection(
) -> crate::notification_settings_projection::NotificationSettingsProjectionStore {
    let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("settings pubsub storage");
    crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
        storage.database(),
    )
}

/// Witness fixture for upstream-storage preservation: an
/// [`InMemoryInboxStorage`] entry that the test seeds BEFORE the
/// candidate emission / T1 drain runs, captured as a snapshot.
/// The notification outbox layer only ever writes to
/// `notification_candidates` and `notification_outbox`; the
/// upstream XEP-0430 inbox (and by symmetry XEP-0313 MAM /
/// XEP-0160 pending delivery / RFC 6121 routing) MUST be untouched
/// when push is suppressed at T0 or T1.
///
/// This helper seeds one inbox entry and returns both the storage
/// handle and the entry-as-snapshot so the test can assert the row
/// is identical (same `last_stanza_id`, `unread`, `last_updated`)
/// after the candidate-emission code path runs.
pub(super) async fn seed_inbox_witness(
    recipient: &BareJid,
    partner: &BareJid,
    stanza_id: &str,
    last_updated: i64,
    unread: u32,
) -> (
    waddle_xmpp::inbox::storage::InMemoryInboxStorage,
    waddle_xmpp::inbox::InboxEntry,
) {
    let storage = waddle_xmpp::inbox::storage::InMemoryInboxStorage::new();
    // Bring the unread count up to `unread` via repeated
    // `increment_unread=true` upserts so the stored row matches
    // what a real XEP-0430 projection would persist (the in-memory
    // adapter ignores `with_unread` on first insert and instead
    // sets unread = 1 when increment is true).
    use waddle_xmpp::inbox::storage::InboxStorage;
    let entry_template = waddle_xmpp::inbox::InboxEntry::new(
        partner.clone(),
        waddle_xmpp::inbox::ConversationKind::Direct,
        stanza_id.to_string(),
        last_updated,
    );
    let mut last = None;
    for _ in 0..unread {
        last = Some(
            storage
                .upsert(recipient, entry_template.clone(), true)
                .await
                .expect("seed inbox witness increment"),
        );
    }
    let witness = match last {
        Some(entry) => entry,
        None => storage
            .upsert(recipient, entry_template.clone(), false)
            .await
            .expect("seed inbox witness (no unread)"),
    };
    (storage, witness)
}

/// Assert the inbox witness seeded by [`seed_inbox_witness`] is
/// still present and byte-identical — proves no rollback / no
/// cross-table write happened during the candidate emission /
/// drain. Implicit corollary: any other upstream artifact
/// (XEP-0313 MAM row, XEP-0160 pending_delivery row, RFC 6121
/// online-resource routing effect) that the test SETS UP BEFORE
/// the candidate emission is preserved by symmetry — the outbox
/// layer touches only its own two tables.
pub(super) async fn assert_inbox_witness_unchanged(
    storage: &waddle_xmpp::inbox::storage::InMemoryInboxStorage,
    recipient: &BareJid,
    expected: &waddle_xmpp::inbox::InboxEntry,
) {
    use waddle_xmpp::inbox::storage::InboxStorage;
    let entries = storage.list(recipient).await.expect("list inbox witness");
    assert_eq!(
        entries.len(),
        1,
        "inbox witness must have exactly one entry after suppression; got {entries:?}",
    );
    assert_eq!(
        &entries[0], expected,
        "suppression code path must not mutate upstream inbox row",
    );
}

// ─────────────────────────────────────────────────────────────
// Slice 2b — `notification_activity` projection + XEP-0513
// `<active/>` push filter (#526).
// ─────────────────────────────────────────────────────────────

/// Builds an [`ActiveChannelMention`] candidate for the given
/// (recipient, room, sender) triple — slice 2b's gate operates
/// exclusively on this class.
pub(super) fn active_channel_mention_candidate_for(
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

pub(super) async fn activity_store() -> crate::notification_activity::NotificationActivityStore {
    crate::notification_activity::NotificationActivityStore::new(
        Database::in_memory("notification-activity-eval")
            .await
            .expect("activity db"),
    )
    .await
    .expect("activity store")
}

/// A [`NotificationActivityReader`] test double that counts
/// per-`(owner, conversation)` read calls so the slice 2b T0/T1
/// stage-split and per-batch cache can be asserted.
pub(super) struct CountingActivityReader {
    inner: crate::notification_activity::NotificationActivityStore,
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingActivityReader {
    pub(super) async fn new() -> Self {
        Self {
            inner: activity_store().await,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(super) fn call_count(&self) -> usize {
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

pub(super) async fn enqueue_jobs_for_test(
    store: &NotificationOutboxStore,
    candidate: &NotificationCandidate,
    targets: &[NotificationOutboxTarget],
) {
    let _ = store
        .insert_candidate(candidate)
        .await
        .expect("insert candidate");
    let now_ms = crate::time::now_ms();
    let context = build_waddle_context(candidate);
    let mut tx = store.db.begin().await.expect("begin tx");
    mark_candidate_outboxed_tx(&mut tx, candidate, now_ms)
        .await
        .expect("mark candidate outboxed");
    for target in targets {
        enqueue_outbox_job_tx(
            &mut tx,
            candidate,
            target,
            &context,
            &RichSummary::minimal(),
            now_ms,
        )
        .await
        .expect("enqueue outbox job");
    }
    tx.commit().await.expect("commit tx");
}

pub(super) async fn inbox_with_unread(
    recipient: &BareJid,
    conversation: &BareJid,
    unread: u32,
) -> waddle_xmpp::inbox::storage::InMemoryInboxStorage {
    let inbox = waddle_xmpp::inbox::storage::InMemoryInboxStorage::new();
    for n in 0..unread {
        inbox
            .upsert(
                recipient,
                waddle_xmpp::inbox::InboxEntry::new(
                    conversation.clone(),
                    waddle_xmpp::inbox::ConversationKind::Direct,
                    format!("archive-{n}"),
                    i64::from(n),
                ),
                true,
            )
            .await
            .expect("upsert inbox entry");
    }
    inbox
}

pub(super) async fn reclaim_stale_job(
    store: &NotificationOutboxStore,
) -> (NotificationOutboxJob, NotificationOutboxJob) {
    let stale_claim = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    let stale_claimed_at_ms = crate::time::now_ms()
        .saturating_sub(OUTBOX_CLAIM_TIMEOUT_MS)
        .saturating_sub(1);
    store
        .execute(
            "UPDATE notification_outbox SET claimed_at_ms = ? WHERE job_id = ?",
            crate::db_params![stale_claimed_at_ms, stale_claim.job_id().as_str()],
        )
        .await
        .expect("make claim stale");
    let fresh_claim = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("reclaim")
        .into_iter()
        .next()
        .expect("reclaimed job");
    (stale_claim, fresh_claim)
}
