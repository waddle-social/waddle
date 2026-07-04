//! Shared helpers for the notification outbox integration suite.

use std::ops::Deref;

use jid::{BareJid, Jid};
use waddle_server::db::Database;
use waddle_server::notification_outbox::*;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};
use waddle_xmpp_core::xep0359::StanzaId;

/// A [`NotificationOutboxStore`] paired with the [`Database`] handle it
/// was opened on, so tests can make raw SQL assertions against the
/// durable schema while driving the store through its public API.
///
/// `Deref`s to the store so call sites read exactly like production
/// call sites (`store.insert_candidate(..)`, `store.claim_due_outbox_jobs(..)`).
pub struct TestStore {
    pub db: Database,
    pub store: NotificationOutboxStore,
}

impl Deref for TestStore {
    type Target = NotificationOutboxStore;

    fn deref(&self) -> &NotificationOutboxStore {
        &self.store
    }
}

impl TestStore {
    pub async fn query(
        &self,
        sql: &str,
        params: impl waddle_server::db::IntoParams,
    ) -> Result<waddle_server::db::Rows, waddle_server::db::DatabaseError> {
        self.db.guard().await?.query(sql, params).await
    }

    pub async fn execute(
        &self,
        sql: &str,
        params: impl waddle_server::db::IntoParams,
    ) -> Result<u64, waddle_server::db::DatabaseError> {
        self.db.guard().await?.execute(sql, params).await
    }
}

/// Raw SQL read against a test-owned [`Database`] handle, for schema
/// tests that construct their own store from a prepared database.
pub async fn db_query(
    db: &Database,
    sql: &str,
    params: impl waddle_server::db::IntoParams,
) -> Result<waddle_server::db::Rows, waddle_server::db::DatabaseError> {
    db.guard().await?.query(sql, params).await
}
pub async fn store() -> TestStore {
    let db = Database::in_memory("notification-outbox")
        .await
        .expect("in-memory db");
    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("store");
    TestStore { db, store }
}

pub fn bare(raw: &str) -> BareJid {
    raw.parse().expect("bare jid")
}

pub fn candidate(id: &str) -> NotificationCandidate {
    candidate_for(&bare("alice@example.com"), &bare("bob@example.com"), id)
}

pub fn candidate_for(recipient: &BareJid, sender: &BareJid, id: &str) -> NotificationCandidate {
    candidate_for_sender_jid(
        recipient,
        format!("{sender}/test-resource")
            .parse()
            .expect("full sender jid"),
        id,
    )
}

pub fn candidate_for_sender_jid(
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

pub fn groupchat_candidate_for(
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

pub async fn failed_outbox_jobs_count(store: &TestStore) -> i64 {
    let mut rows = store
        .query(
            // `failed` is the durable schema's terminal status value; the
            // typed enum round-trip is pinned by the inline unit tests
            // next to `NotificationOutboxStatus`.
            "SELECT COUNT(*) FROM notification_outbox WHERE status = ?",
            waddle_server::db_params!["failed"],
        )
        .await
        .expect("failed outbox count query");
    rows.next()
        .await
        .expect("failed outbox count row")
        .expect("failed outbox count")
        .get(0)
        .expect("failed outbox count")
}

pub fn target() -> NotificationOutboxTarget {
    target_named("web-node")
}

pub fn target_named(node: &str) -> NotificationOutboxTarget {
    NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::new(node).expect("node"),
    )
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
pub static NOOP_ACTIVITY_READER: waddle_server::notification_activity::NoopActivityReader =
    waddle_server::notification_activity::NoopActivityReader;

pub fn noop_activity_reader() -> &'static waddle_server::notification_activity::NoopActivityReader {
    &NOOP_ACTIVITY_READER
}

/// Convenience constructor for [`NotificationDrainDeps`] that
/// wires the default no-op activity reader. Used by every slice
/// 2a test whose recipient class is not
/// `ActiveChannelMention` — those tests do not exercise the
/// XEP-0513 `<active/>` filter, so a noop reader is the correct
/// dependency.
pub fn drain_deps_with_noop_activity<'a>(
    room_policy: &'a dyn RoomPolicyStore,
    dnd_reader: &'a dyn DndReader,
    activity_reader: &'a waddle_server::notification_activity::NoopActivityReader,
) -> NotificationDrainDeps<'a> {
    NotificationDrainDeps::new(room_policy, dnd_reader, activity_reader)
}

/// Test double for [`RoomPolicyStore`] that pretends every room is
/// public (`members_only = false`). Slice 1's tests do not exercise
/// private-room dispatch policy; when slice 2 adds those paths it
/// will grow this stub (or replace it with a richer fixture).
pub struct StubRoomPolicy;

impl StubRoomPolicy {
    pub fn new() -> Self {
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

pub async fn settings_projection(
) -> waddle_server::notification_settings_projection::NotificationSettingsProjectionStore {
    let storage = waddle_server::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("settings pubsub storage");
    waddle_server::notification_settings_projection::NotificationSettingsProjectionStore::new(
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
///
/// [`InMemoryInboxStorage`]: waddle_xmpp::inbox::storage::InMemoryInboxStorage
pub async fn seed_inbox_witness(
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

/// Enqueues one outbox job per target for `candidate` by driving the
/// public T1 drain with a throwaway XEP-0357 registration per target
/// and permissive gate dependencies. This is the same enqueue path
/// production uses (`drain_pending_candidates_into_outbox` →
/// coalescing insert/merge), so coalescing semantics — merge into a
/// queued job, fresh job after claim — are exercised for real.
///
/// The former inline helper reached into the store's private
/// transaction API to shortcut the drain; the observable rows are
/// identical.
pub async fn enqueue_jobs_for_test(
    store: &TestStore,
    candidate: &NotificationCandidate,
    targets: &[NotificationOutboxTarget],
) {
    let _ = store
        .insert_candidate(candidate)
        .await
        .expect("insert candidate");
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    for target in targets {
        register_push_target(&push_store, candidate.recipient_bare_jid(), target).await;
    }
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
    let mut service_jids: Vec<&BareJid> = targets
        .iter()
        .map(NotificationOutboxTarget::push_service_jid)
        .collect();
    service_jids.dedup();
    assert_eq!(
        service_jids.len(),
        1,
        "enqueue_jobs_for_test drains once; pass targets on a single push service"
    );
    let drained = store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            service_jids[0],
            16,
        )
        .await
        .expect("drain candidate into outbox");
    assert!(drained > 0, "candidate must reach the outbox");
}

pub async fn register_push_target(
    push_store: &waddle_xmpp::push::InMemoryPushStore,
    recipient: &BareJid,
    target: &NotificationOutboxTarget,
) {
    push_store
        .register(waddle_xmpp::push::PushSubscription {
            user_jid: recipient.to_string(),
            service_jid: target.push_service_jid().to_string(),
            node: Some(target.node().as_str().to_string()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("register push target");
}

pub async fn inbox_with_unread(
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

#[derive(Debug, thiserror::Error)]
#[error("blocking storage unavailable")]
pub struct BlockingStorageUnavailable;

pub struct FailingBlockingStorage;

#[async_trait::async_trait]
impl BlockingStorage for FailingBlockingStorage {
    async fn list_blocked_jids(
        &self,
        _user: &BareJid,
    ) -> Result<Vec<BareJid>, BlockingStorageError> {
        Err(BlockingStorageError::new(BlockingStorageUnavailable))
    }
}

pub async fn activity_store() -> waddle_server::notification_activity::NotificationActivityStore {
    waddle_server::notification_activity::NotificationActivityStore::new(
        Database::in_memory("notification-activity-eval")
            .await
            .expect("activity db"),
    )
    .await
    .expect("activity store")
}
