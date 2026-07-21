//! Typed T1 suppression audit: XEP-0492, XEP-0513, XEP-0334 hints, XEP-0191 audit, DND.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::{BareJid, Jid};
use waddle_server::notification_outbox::*;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::NS_DATA_FORMS;
use waddle_xmpp_core::xep0359::StanzaId;

/// `DndReader` test double that mirrors the shape #367 will land
/// for the real `urn:waddle:dnd:0` PEP-backed reader: a per-user
/// persisted set of "currently DnD-active" recipients, queried
/// fresh at T1 and returning [`DndState::Active`] iff the user's
/// PEP item is present.
///
/// When #367 lands, only the implementation swaps — the trait
/// contract this mock exercises (per-user lookup → typed `DndState`,
/// async + `BareJid`-keyed) is the load-bearing surface and is
/// locked in slice 2a. Tests using this mock therefore verify the
/// integration contract independently of #367's persistence layer.
struct MockPepDndReader {
    active_users: std::sync::Mutex<std::collections::BTreeSet<BareJid>>,
}

impl MockPepDndReader {
    fn new() -> Self {
        Self {
            active_users: std::sync::Mutex::new(std::collections::BTreeSet::new()),
        }
    }
    fn set_active(&self, user: BareJid) {
        self.active_users
            .lock()
            .expect("active_users lock")
            .insert(user);
    }
}

#[async_trait::async_trait]
impl DndReader for MockPepDndReader {
    async fn dnd_state(&self, user: &BareJid) -> Result<DndState, NotificationOutboxError> {
        let active = self
            .active_users
            .lock()
            .expect("active_users lock")
            .contains(user);
        Ok(if active {
            DndState::Active
        } else {
            DndState::Inactive
        })
    }
}

/// Assert the inbox witness seeded by [`seed_inbox_witness`] is
/// still present and byte-identical — proves no rollback / no
/// cross-table write happened during the candidate emission /
/// drain. Implicit corollary: any other upstream artifact
/// (XEP-0313 MAM row, XEP-0160 pending_delivery row, RFC 6121
/// online-resource routing effect) that the test SETS UP BEFORE
/// the candidate emission is preserved by symmetry — the outbox
/// layer touches only its own two tables.
async fn assert_inbox_witness_unchanged(
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

/// Upsert a per-conversation rich-payload opt-in for a DM so the
/// drain's T1 evaluator resolves rich XEP-0357 summaries.
async fn opt_in_rich_payload(
    projection: &waddle_server::notification_settings_projection::NotificationSettingsProjectionStore,
    recipient: &BareJid,
    conversation: &BareJid,
) {
    projection
        .upsert(&waddle_server::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: conversation.clone(),
            conversation_kind:
                waddle_server::notification_settings_projection::ConversationKind::Direct,
            mode: waddle_xmpp::xep::NotificationLevel::Always,
            rich_payload_opt_in: true,
            source_version: 1,
            updated_at_ms: 1,
            source:
                waddle_server::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: conversation.clone(),
        })
        .await
        .expect("opt-in upsert");
}

/// XEP-0492 `<never/>` suppression at T1 MUST persist
/// `Xep0492Never` onto the candidate row's `suppressed_reason`
/// column, NOT enqueue a job, and increment the metric counter
/// labeled by the typed db value.
#[tokio::test]
async fn t1_xep0492_never_records_typed_suppressed_reason() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");

    // Persist a `<never/>` notification setting for the recipient
    // against `sender`'s DM conversation.
    projection
        .upsert(&waddle_server::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind:
                waddle_server::notification_settings_projection::ConversationKind::Direct,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            source:
                waddle_server::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: sender.clone(),
            updated_at_ms: 1,
            rich_payload_opt_in: false,
            source_version: 1,
        })
        .await
        .expect("seed never level");

    let candidate = candidate_for(&recipient, &sender, "t1-never");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["t1-never"],
        )
        .await
        .expect("query suppressed_reason");
    let row = rows.next().await.expect("row").expect("row exists");
    let reason: Option<String> = row.get(0).expect("reason");
    let outboxed: Option<i64> = row.get(1).expect("outboxed_at_ms");
    assert_eq!(reason.as_deref(), Some("xep0492_never"));
    assert!(
        outboxed.is_some(),
        "T1 suppression must mark candidate outboxed"
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "T1 suppression MUST NOT enqueue a job",
    );

    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0492_never")]),
        Some(1),
        "metric counter for xep0492_never must increment",
    );
}

/// XEP-0492 `<on-mention/>` setting with a non-mention candidate
/// (DM without explicit mention) MUST suppress at T1 with the
/// typed `Xep0492OnMentionMiss` audit reason.
#[tokio::test]
async fn t1_xep0492_on_mention_miss_records_typed_suppressed_reason() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");

    projection
        .upsert(&waddle_server::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind:
                waddle_server::notification_settings_projection::ConversationKind::Direct,
            mode: waddle_xmpp::xep::NotificationLevel::OnMention,
            source:
                waddle_server::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: sender.clone(),
            updated_at_ms: 1,
            rich_payload_opt_in: false,
            source_version: 1,
        })
        .await
        .expect("seed on-mention level");

    let candidate = candidate_for(&recipient, &sender, "t1-on-mention");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            waddle_server::db_params!["t1-on-mention"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    let reason: Option<String> = row.get(0).expect("reason");
    assert_eq!(reason.as_deref(), Some("xep0492_on_mention_miss"));

    assert_eq!(
        metrics.counter_sum(
            "xmpp.push.suppressed",
            &[("reason", "xep0492_on_mention_miss")]
        ),
        Some(1),
    );
}

/// XEP-0191 blocking at T1 MUST record `Xep0191Blocked` onto the
/// candidate row before marking it outboxed-without-job.
#[tokio::test]
async fn t1_xep0191_blocked_records_typed_suppressed_reason() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");

    // Block the sender on the recipient's blocklist.
    blocking.set_blocklist(recipient.clone(), vec![sender.clone()]);

    let candidate = candidate_for(&recipient, &sender, "t1-blocked");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            waddle_server::db_params!["t1-blocked"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    let reason: Option<String> = row.get(0).expect("reason");
    assert_eq!(reason.as_deref(), Some("xep0191_blocked"));
    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0191_blocked")]),
        Some(1),
    );
}

/// XEP-0513 `<noping/>` carried on the candidate row MUST suppress
/// at T1 with the typed `Xep0513Noping` reason. Tests the
/// message-frozen path: candidate is constructed with the noping
/// bit set, persisted, then the drain reads it back and suppresses.
#[tokio::test]
async fn t1_noping_records_typed_suppressed_reason() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("t1-noping", Jid::from(recipient.clone())),
        false,
        NotificationMessageHints::none().with_noping(true),
    )
    .expect("candidate");
    assert!(candidate.noping());
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, noping FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["t1-noping"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0513_noping")
    );
    assert_eq!(row.get::<i64>(1).expect("noping"), 1);
    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0513_noping")]),
        Some(1),
    );
}

// #780: a reaction-only message's candidate persists at T0 and is
// suppressed at T1 with the typed `xep0444_reaction` reason + labeled
// metric — no outbox job is created.
#[tokio::test]
async fn t1_xep0444_reaction_records_typed_suppressed_reason() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("t1-reaction", Jid::from(recipient.clone())),
        false,
        NotificationMessageHints::none().with_reaction(true),
    )
    .expect("candidate");
    assert!(candidate.reaction());
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, reaction FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["t1-reaction"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0444_reaction")
    );
    assert_eq!(row.get::<i64>(1).expect("reaction"), 1);
    assert!(
        store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty(),
        "a reaction-only candidate must not produce an outbox job"
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0444_reaction")]),
        Some(1),
    );
}

/// #719: with the rich-payload opt-in set and no XEP-0334 storage
/// hint, the drained push carries the full XEP-0357 §5.4 summary —
/// both `last-message-sender` and `last-message-body`.
#[tokio::test]
async fn t1_opt_in_without_hint_emits_rich_summary() {
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
    opt_in_rich_payload(&projection, &recipient, &conversation).await;
    register_push_target(&push_store, &recipient, &target()).await;
    let candidate = NotificationCandidate::direct_message(
        recipient.clone(),
        sender_jid.clone(),
        StanzaId::new("t1-rich", Jid::from(recipient.clone())),
        false,
    )
    .expect("candidate")
    .with_last_message_body(Some("Wherefore art thou, Romeo?".to_string()));
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].rich_summary().sender.as_ref(), Some(&sender_jid));
    assert_eq!(
        jobs[0].rich_summary().body.as_deref(),
        Some("Wherefore art thou, Romeo?")
    );
    // End-to-end: the dispatched XEP-0357 §5.4 wire shape carries
    // both optional fields.
    let item = jobs[0].to_xep0357_pubsub_item();
    let payload = item.payload.expect("payload");
    let summary = payload
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .expect("summary form");
    assert!(summary
        .children()
        .any(|field| field.attr("var") == Some("last-message-sender")));
    assert!(summary
        .children()
        .any(|field| field.attr("var") == Some("last-message-body")));
}

/// #719 privacy invariant: for a groupchat, `last-message-sender`
/// is the room-occupant JID (`room@muc/nick`), never a real JID.
/// The candidate constructor enforces
/// `sender_jid.to_bare() == conversation_jid`, so the summary cannot
/// leak a real JID to the push gateway regardless of room visibility.
#[tokio::test]
async fn t1_groupchat_rich_sender_is_occupant_jid() {
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let room = bare("team@muc.example.com");
    let occupant: Jid = "team@muc.example.com/bob".parse().expect("occupant jid");
    // StubRoomPolicy resolves the room as public; a PersonalMention
    // is a mention, and the stored Always row delivers regardless.
    opt_in_rich_payload(&projection, &recipient, &room).await;
    register_push_target(&push_store, &recipient, &target()).await;
    let candidate = NotificationCandidate::groupchat(
        recipient.clone(),
        room.clone(),
        occupant.clone(),
        NotificationThreadId::root(),
        StanzaId::new("gc-rich", Jid::from(room.clone())),
        NotificationClass::PersonalMention,
    )
    .expect("groupchat candidate")
    .with_last_message_body(Some("hi team".to_string()));
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
    assert_eq!(jobs.len(), 1);
    let sender = jobs[0].rich_summary().sender.as_ref().expect("sender");
    assert_eq!(sender, &occupant);
    assert_eq!(
        sender.to_bare(),
        room,
        "sender must be the room-occupant JID"
    );
}

/// #719 / XEP-0334 §3 precedence: even with the rich-payload opt-in
/// set, a `<no-store/>` candidate delivers a push whose summary
/// carries NO `last-message-body` — the sender is preserved, and the
/// body is never persisted onto the candidate row.
#[tokio::test]
async fn t1_no_store_strips_body_but_still_delivers_with_opt_in() {
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
    opt_in_rich_payload(&projection, &recipient, &conversation).await;
    register_push_target(&push_store, &recipient, &target()).await;
    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("t1-no-store", Jid::from(recipient.clone())),
        false,
        NotificationMessageHints::none().with_xep0334(true, false),
    )
    .expect("candidate")
    .with_last_message_body(Some("secret".to_string()));
    // Storage conformance: the body is never even persisted.
    assert_eq!(candidate.last_message_body(), None);
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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

    // The push is delivered (no suppression recorded)...
    let mut rows = store
        .query(
            "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["t1-no-store"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        None,
        "<no-store/> must NOT suppress the push under #719",
    );
    // ...but the summary carries the sender and NOT the body.
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].rich_summary().sender.is_some());
    assert_eq!(jobs[0].rich_summary().body, None);
}

/// #719 / XEP-0334 §3 precedence for `<no-permanent-store/>`: same
/// as `<no-store/>` — body stripped, push delivered.
#[tokio::test]
async fn t1_no_permanent_store_strips_body_but_still_delivers_with_opt_in() {
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
    opt_in_rich_payload(&projection, &recipient, &conversation).await;
    register_push_target(&push_store, &recipient, &target()).await;
    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("t1-no-perm-store", Jid::from(recipient.clone())),
        false,
        NotificationMessageHints::none().with_xep0334(false, true),
    )
    .expect("candidate")
    .with_last_message_body(Some("secret".to_string()));
    assert_eq!(candidate.last_message_body(), None);
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            waddle_server::db_params!["t1-no-perm-store"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        None,
        "<no-permanent-store/> must NOT suppress the push under #719",
    );
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].rich_summary().sender.is_some());
    assert_eq!(jobs[0].rich_summary().body, None);
}

/// `NoopDndReader` MUST report every user as `Inactive` so slice
/// 2a's defaulted call sites never trigger DnD suppression while
/// the real impl is still pending (#367).
#[tokio::test]
async fn noop_dnd_reader_reports_inactive() {
    let reader = NoopDndReader;
    let user = bare("alice@example.com");
    let state = reader.dnd_state(&user).await.expect("noop dnd");
    assert_eq!(state, DndState::Inactive);
}

/// A `DndReader` that reports `Active` MUST suppress at T1 with
/// `WaddleDnd`, even when the recipient has no other suppressors
/// in play.
#[tokio::test]
async fn t1_active_dnd_suppresses_with_typed_reason() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;

    struct ActiveDndReader;
    #[async_trait::async_trait]
    impl DndReader for ActiveDndReader {
        async fn dnd_state(&self, _user: &BareJid) -> Result<DndState, NotificationOutboxError> {
            Ok(DndState::Active)
        }
    }

    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = ActiveDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");
    let candidate = candidate_for(&recipient, &sender, "t1-dnd");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            waddle_server::db_params!["t1-dnd"],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("waddle_dnd"),
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "waddle_dnd")]),
        Some(1),
    );
}

/// XEP-0191 blocking suppresses at T1: the candidate row IS
/// persisted (so the audit row exists), then the drain marks it
/// outboxed-without-job with `xep0191_blocked`. Upstream storage
/// (here: pre-existing inbox row) MUST be intact.
#[tokio::test]
async fn xep0191_blocked_t1_suppression_keeps_pending_delivery_intact() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");

    let (inbox, witness) =
        seed_inbox_witness(&recipient, &sender, "archive-blocked-witness", 11, 2).await;

    blocking.set_blocklist(recipient.clone(), vec![sender.clone()]);

    let candidate = candidate_for(&recipient, &sender, "blocked-t1");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["blocked-t1"],
        )
        .await
        .expect("query suppressed_reason");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0191_blocked"),
    );
    assert!(
        row.get::<Option<i64>>(1).expect("outboxed").is_some(),
        "T1 suppression must mark candidate outboxed",
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "T1 XEP-0191 suppression MUST NOT enqueue a job",
    );

    assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0191_blocked")]),
        Some(1),
        "metric counter for xep0191_blocked must increment",
    );
}

/// XEP-0513 `<noping/>` is a message-frozen hint suppressed at
/// T1 (per the f898e54c stage-split): the candidate row persists
/// with the noping bit, then T1 records `xep0513_noping`. Upstream
/// storage is preserved across this audit-only suppression.
#[tokio::test]
async fn xep0513_noping_t1_suppression_persists_candidate_and_keeps_storage() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

    let (inbox, witness) =
        seed_inbox_witness(&recipient, &sender, "archive-noping-witness", 13, 1).await;

    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("noping-t1", Jid::from(recipient.clone())),
        true,
        NotificationMessageHints::none().with_noping(true),
    )
    .expect("candidate");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, outboxed_at_ms, noping FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["noping-t1"],
        )
        .await
        .expect("query suppressed_reason");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("xep0513_noping"),
    );
    assert!(
        row.get::<Option<i64>>(1).expect("outboxed").is_some(),
        "T1 noping suppression must mark candidate outboxed",
    );
    assert_eq!(
        row.get::<i64>(2).expect("noping"),
        1,
        "candidate row must persist the noping hint bit",
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "T1 XEP-0513 noping suppression MUST NOT enqueue a job",
    );

    assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "xep0513_noping")]),
        Some(1),
        "metric counter for xep0513_noping must increment",
    );
}

/// #719 regression: a `<no-store/>` candidate is NOT push-suppressed
/// — the candidate row persists with the no_store bit, T1 records NO
/// `suppressed_reason`, an outbox job is enqueued, and upstream
/// storage is untouched. With the default (opt-out) the summary stays
/// minimal.
#[tokio::test]
async fn xep0334_no_store_delivers_minimal_push_and_keeps_storage() {
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

    let (inbox, witness) =
        seed_inbox_witness(&recipient, &sender, "archive-no-store-witness", 17, 1).await;
    register_push_target(&push_store, &recipient, &target()).await;

    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("no-store-t1", Jid::from(recipient.clone())),
        false,
        NotificationMessageHints::none().with_xep0334(true, false),
    )
    .expect("candidate")
    .with_last_message_body(Some("secret".to_string()));
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, outboxed_at_ms, no_store, last_message_body FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["no-store-t1"],
        )
        .await
        .expect("query suppressed_reason");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        None,
        "<no-store/> must not push-suppress under #719",
    );
    assert!(row.get::<Option<i64>>(1).expect("outboxed").is_some());
    assert_eq!(row.get::<i64>(2).expect("no_store"), 1);
    // Off-the-record body was never persisted onto the candidate.
    assert_eq!(row.get::<Option<String>>(3).expect("body"), None);

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1, "the minimal push must still be enqueued");
    // Default opt-out → minimal summary, no rich fields.
    assert_eq!(jobs[0].rich_summary(), &RichSummary::minimal());

    assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;
}

/// #719 parallel of the `<no-store/>` regression for
/// `<no-permanent-store/>`: delivered, not suppressed, storage
/// preserved.
#[tokio::test]
async fn xep0334_no_permanent_store_delivers_minimal_push_and_keeps_storage() {
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = NoopDndReader;
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");
    let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

    let (inbox, witness) =
        seed_inbox_witness(&recipient, &sender, "archive-no-perm-store-witness", 23, 1).await;
    register_push_target(&push_store, &recipient, &target()).await;

    let candidate = NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid,
        StanzaId::new("no-perm-store-t1", Jid::from(recipient.clone())),
        false,
        NotificationMessageHints::none().with_xep0334(false, true),
    )
    .expect("candidate")
    .with_last_message_body(Some("secret".to_string()));
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, no_permanent_store, last_message_body FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["no-perm-store-t1"],
        )
        .await
        .expect("query suppressed_reason");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        None,
        "<no-permanent-store/> must not push-suppress under #719",
    );
    assert_eq!(row.get::<i64>(1).expect("no_permanent_store"), 1);
    assert_eq!(row.get::<Option<String>>(2).expect("body"), None);

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].rich_summary(), &RichSummary::minimal());

    assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;
}

/// Waddle DnD suppression at T1 via the `DndReader` trait. Uses
/// the [`MockPepDndReader`] fixture that mirrors #367's
/// PEP-backed shape (per-user `Active`/`Inactive` lookup against
/// persisted state). Upstream inbox witness is preserved across
/// the DnD-driven audit.
#[tokio::test]
async fn waddle_dnd_t1_suppression_persists_audit_and_keeps_storage() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = MockPepDndReader::new();
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");
    dnd_reader.set_active(recipient.clone());

    let (inbox, witness) =
        seed_inbox_witness(&recipient, &sender, "archive-dnd-witness", 29, 5).await;

    let candidate = candidate_for(&recipient, &sender, "dnd-t1");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
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
            "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["dnd-t1"],
        )
        .await
        .expect("query suppressed_reason");
    let row = rows.next().await.expect("row").expect("row exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("reason").as_deref(),
        Some("waddle_dnd"),
    );
    assert!(row.get::<Option<i64>>(1).expect("outboxed").is_some());
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "T1 Waddle DnD suppression MUST NOT enqueue a job",
    );

    assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "waddle_dnd")]),
        Some(1),
        "metric counter for waddle_dnd must increment",
    );
}

/// Integration shape that #367 will fulfill: the real
/// `urn:waddle:dnd:0` PEP-backed `DndReader` is queried per-user
/// at T1 with the recipient's `BareJid`, and the typed
/// `DndState::Active` / `Inactive` outcome decides suppression.
/// This test exercises the contract with [`MockPepDndReader`]
/// (a per-user persisted set of "active" recipients) — once
/// #367 ships, only the reader implementation swaps; the trait
/// surface this test pins is locked in slice 2a.
///
/// Scenario: two DM candidates drain in one batch — Alice (DnD
/// Active) MUST be suppressed with `waddle_dnd`, Bob (DnD
/// Inactive) MUST be delivered through to a job. Metric counter
/// MUST tick by exactly one (Alice's row only).
#[tokio::test]
async fn dnd_integration_with_pep_shaped_reader_suppresses_push_only() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let dnd_reader = MockPepDndReader::new();
    let alice = bare("alice@example.com");
    let bob = bare("bob@example.com");
    let carol = bare("carol@example.com");
    let push_service_jid = bare("push.example.com");

    // Mirrors #367: Alice has a `urn:waddle:dnd:0` PEP item set
    // (modelled here as membership in the active_users set);
    // Bob does not.
    dnd_reader.set_active(alice.clone());

    // Register a push device for each recipient so the
    // non-suppressed candidate can enqueue a real outbox job
    // (proves the suppression scope is per-recipient, not global).
    for recipient in [&alice, &bob] {
        push_store
            .register(waddle_xmpp::push::PushSubscription {
                user_jid: recipient.to_string(),
                service_jid: push_service_jid.to_string(),
                node: Some(format!("{recipient}-node")),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("register push subscription");
    }

    let alice_candidate = candidate_for(&alice, &carol, "dnd-integration-alice");
    let bob_candidate = candidate_for(&bob, &carol, "dnd-integration-bob");
    store
        .insert_candidate(&alice_candidate)
        .await
        .expect("insert alice candidate");
    store
        .insert_candidate(&bob_candidate)
        .await
        .expect("insert bob candidate");

    let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
    store
        .drain_pending_candidates_into_outbox(
            &push_store,
            &blocking,
            &projection,
            deps,
            &push_service_jid,
            16,
        )
        .await
        .expect("drain candidates");

    // Alice's candidate is suppressed with the typed audit.
    let mut alice_rows = store
        .query(
            "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["dnd-integration-alice"],
        )
        .await
        .expect("query alice");
    let alice_row = alice_rows
        .next()
        .await
        .expect("alice row")
        .expect("alice exists");
    assert_eq!(
        alice_row
            .get::<Option<String>>(0)
            .expect("alice reason")
            .as_deref(),
        Some("waddle_dnd"),
        "Alice (DnD Active) MUST be suppressed with the typed waddle_dnd audit",
    );
    assert!(
        alice_row
            .get::<Option<i64>>(1)
            .expect("alice outboxed")
            .is_some(),
        "Alice's candidate must be marked outboxed-without-job",
    );

    // Bob's candidate is delivered through to a real outbox job.
    let mut bob_rows = store
        .query(
            "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["dnd-integration-bob"],
        )
        .await
        .expect("query bob");
    let bob_row = bob_rows.next().await.expect("bob row").expect("bob exists");
    assert!(
        bob_row
            .get::<Option<String>>(0)
            .expect("bob reason")
            .is_none(),
        "Bob (DnD Inactive) MUST NOT be suppressed",
    );

    let jobs = store
        .pending_outbox_jobs()
        .await
        .expect("pending outbox jobs");
    assert_eq!(
        jobs.len(),
        1,
        "exactly one outbox job — Bob's. Alice's DnD suppression MUST be per-recipient",
    );
    assert_eq!(
        jobs[0].recipient_bare_jid(),
        &bob,
        "the surviving job belongs to Bob",
    );

    assert_eq!(
        metrics.counter_sum("xmpp.push.suppressed", &[("reason", "waddle_dnd")]),
        Some(1),
        "metric for waddle_dnd must increment by exactly 1 (Alice only)",
    );
}
