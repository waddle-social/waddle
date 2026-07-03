//! Behavior tests for the notification activity store: recording chat
//! states, read markers, presence, and outbound messages, plus the
//! reader trait contract.
//!
//! Extracted from the former inline `mod tests` in
//! `src/notification_activity.rs`. Closed-set db-value round-trip and
//! CHECK-constraint matcher tests stay inline next to the enums they
//! pin (they exercise `pub(crate)`/private codec internals).

use jid::BareJid;
use waddle_server::db::Database;
use waddle_server::notification_activity::*;

fn bare(value: &str) -> BareJid {
    value.parse().expect("valid bare jid")
}

async fn store() -> NotificationActivityStore {
    NotificationActivityStore::new(
        Database::in_memory("notification-activity-test")
            .await
            .expect("in-memory db"),
    )
    .await
    .expect("activity store")
}

/// Recording a chat-state event persists the typed token and bumps
/// `last_active_at_ms`. Re-recording overrides previous columns
/// per the `ON CONFLICT DO UPDATE` semantics.
#[tokio::test]
async fn record_chat_state_persists_typed_token_and_advances_activity() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    store
        .record_chat_state(
            &owner,
            &conversation,
            NotificationChatState::Composing,
            1_000,
        )
        .await
        .expect("record chat-state");

    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 1_000);
    assert_eq!(
        activity.last_chat_state,
        Some(NotificationChatState::Composing)
    );
    assert!(activity.last_read_at_ms.is_none());
    assert!(activity.presence_show.is_none());

    // Re-recording at a later timestamp advances activity and
    // updates the chat-state token.
    store
        .record_chat_state(&owner, &conversation, NotificationChatState::Paused, 2_000)
        .await
        .expect("record again");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 2_000);
    assert_eq!(
        activity.last_chat_state,
        Some(NotificationChatState::Paused)
    );
}

/// Monotonic invariant: a stale chat-state write whose `now_ms` is
/// older than the projection's stored `last_active_at_ms` MUST NOT
/// regress either `last_active_at_ms` or `last_chat_state`. This
/// guards against concurrent UPSERT races where a slow writer
/// commits AFTER a fresh writer with a smaller event timestamp.
#[tokio::test]
async fn record_chat_state_does_not_regress_on_stale_write() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    // Fresh write at t=2000.
    store
        .record_chat_state(&owner, &conversation, NotificationChatState::Active, 2_000)
        .await
        .expect("fresh chat-state");
    // Stale write at t=1000 arrives second.
    store
        .record_chat_state(
            &owner,
            &conversation,
            NotificationChatState::Inactive,
            1_000,
        )
        .await
        .expect("stale chat-state");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        activity.last_active_at_ms, 2_000,
        "stale chat-state MUST NOT regress last_active_at_ms"
    );
    assert_eq!(
        activity.last_chat_state,
        Some(NotificationChatState::Active),
        "stale chat-state MUST NOT overwrite the fresh chat-state token"
    );
}

/// XEP-0085 `<gone/>` is an explicit inactivity signal. The writer
/// MUST zero `last_active_at_ms` regardless of how recent the prior
/// activity was so the T1 XEP-0513 `<active/>` filter immediately
/// stops treating the user as engaged in the conversation. The
/// chat-state token is preserved as `gone` for diagnostics.
#[tokio::test]
async fn record_chat_state_gone_zeroes_last_active_unconditionally() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    // Seed a fresh active state at t=5000.
    store
        .record_chat_state(&owner, &conversation, NotificationChatState::Active, 5_000)
        .await
        .expect("seed active");
    let seeded = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read seed")
        .expect("row");
    assert_eq!(seeded.last_active_at_ms, 5_000);
    // <gone/> at t=6000 MUST zero last_active_at_ms even though
    // the prior write is more recent than this would otherwise be
    // allowed under the monotonic clamp.
    store
        .record_chat_state_gone(&owner, &conversation, 6_000)
        .await
        .expect("record gone");
    let after = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read post-gone")
        .expect("row");
    assert_eq!(
        after.last_active_at_ms, 0,
        "<gone/> MUST unconditionally regress last_active_at_ms to 0",
    );
    assert_eq!(
        after.last_chat_state,
        Some(NotificationChatState::Gone),
        "<gone/> MUST preserve the typed chat-state token for diagnostics",
    );
}

/// `record_chat_state_gone` works as an UPSERT against a row that
/// does not yet exist for the (owner, conversation) pair — the
/// first signal we ever see for this user in this conversation can
/// legitimately be a `<gone/>` (a client sending its departure on
/// disconnect without ever having sent another chat-state).
#[tokio::test]
async fn record_chat_state_gone_upserts_missing_row() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    store
        .record_chat_state_gone(&owner, &conversation, 1_000)
        .await
        .expect("record gone");
    let after = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(after.last_active_at_ms, 0);
    assert_eq!(after.last_chat_state, Some(NotificationChatState::Gone));
}

/// Tie-handling invariant: two writes that land in the same
/// millisecond MUST both apply (the later writer wins on its
/// paired columns). The strict-`>` comparison previously dropped
/// the second write silently — e.g. a join+leave pair in the same
/// ms left the join's `<show/>` token persisted even though the
/// leave should have cleared it (Codex/Copilot review on PR #731).
#[tokio::test]
async fn record_presence_tie_writes_apply_latest_writer() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let room = bare("room@muc.example.com");
    store
        .record_presence_available(&owner, &room, Some(NotificationPresenceShow::Chat), 1_000)
        .await
        .expect("available");
    store
        .record_presence_unavailable(&owner, &room, 1_000)
        .await
        .expect("unavailable at same ms");
    let after = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(after.last_active_at_ms, 1_000);
    assert!(
        after.presence_show.is_none(),
        "same-ms unavailable MUST clear `<show/>` (tie goes to latest writer)",
    );
}

/// Tie-handling invariant for `record_chat_state`: a second
/// chat-state at the same ms MUST overwrite the prior token.
#[tokio::test]
async fn record_chat_state_tie_writes_apply_latest_writer() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    store
        .record_chat_state(&owner, &conversation, NotificationChatState::Active, 1_000)
        .await
        .expect("first");
    store
        .record_chat_state(
            &owner,
            &conversation,
            NotificationChatState::Composing,
            1_000,
        )
        .await
        .expect("tie write");
    let after = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(after.last_active_at_ms, 1_000);
    assert_eq!(
        after.last_chat_state,
        Some(NotificationChatState::Composing),
        "tie goes to the latest writer",
    );
}

/// XEP-0490 read-marker writes persist `last_read_at_ms` alongside
/// `last_active_at_ms` and leave other columns untouched.
#[tokio::test]
async fn record_read_marker_persists_last_read_and_active() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    // Seed a chat-state row first so we can witness that the
    // read-marker write leaves `last_chat_state` intact.
    store
        .record_chat_state(
            &owner,
            &conversation,
            NotificationChatState::Composing,
            1_000,
        )
        .await
        .expect("seed");
    store
        .record_read_marker(&owner, &conversation, 2_000)
        .await
        .expect("record marker");

    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 2_000);
    assert_eq!(activity.last_read_at_ms, Some(2_000));
    assert_eq!(
        activity.last_chat_state,
        Some(NotificationChatState::Composing),
        "read-marker MUST NOT overwrite chat-state",
    );
}

/// XEP-0490 monotonic invariant: a stale read-marker write whose
/// `now_ms` is older than the stored `last_read_at_ms` MUST NOT
/// regress the marker. XEP-0490 §3 mandates monotonic advance of
/// the displayed marker; the projection enforces it at the
/// UPSERT layer so out-of-order arrivals (network reorder,
/// concurrent writers) cannot violate the wire-level invariant.
#[tokio::test]
async fn record_read_marker_does_not_regress_on_stale_write() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    store
        .record_read_marker(&owner, &conversation, 2_000)
        .await
        .expect("fresh marker");
    store
        .record_read_marker(&owner, &conversation, 1_000)
        .await
        .expect("stale marker");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        activity.last_active_at_ms, 2_000,
        "stale read-marker MUST NOT regress last_active_at_ms"
    );
    assert_eq!(
        activity.last_read_at_ms,
        Some(2_000),
        "stale read-marker MUST NOT regress last_read_at_ms (XEP-0490 monotonicity)"
    );
}

/// Outbound message commit bumps `last_active_at_ms` but leaves
/// other columns untouched (no chat-state, no read marker, no
/// presence change).
#[tokio::test]
async fn record_outbound_message_advances_active_only() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    store
        .record_outbound_message(&owner, &conversation, 3_000)
        .await
        .expect("record outbound");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 3_000);
    assert!(activity.last_chat_state.is_none());
    assert!(activity.last_read_at_ms.is_none());
    assert!(activity.presence_show.is_none());
}

/// XEP-0045 presence: an available presence persists the
/// `<show/>` token; a subsequent unavailable clears it but still
/// bumps `last_active_at_ms`.
#[tokio::test]
async fn record_presence_available_then_unavailable_keeps_recent_activity() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let room = bare("room@muc.example.com");
    store
        .record_presence_available(&owner, &room, Some(NotificationPresenceShow::Away), 4_000)
        .await
        .expect("available");
    let activity = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 4_000);
    assert_eq!(activity.presence_show, Some(NotificationPresenceShow::Away));

    // Unavailable bumps activity but clears the show.
    store
        .record_presence_unavailable(&owner, &room, 5_000)
        .await
        .expect("unavailable");
    let activity = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 5_000);
    assert!(
        activity.presence_show.is_none(),
        "unavailable MUST clear `<show/>`",
    );
}

/// `NotificationActivityReader` impl on the store matches the
/// inherent `read` method — exercises the trait surface that the
/// T1 evaluator consults.
#[tokio::test]
async fn reader_trait_returns_recorded_activity() {
    let store = store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    store
        .record_outbound_message(&owner, &conversation, 7_000)
        .await
        .expect("record");
    let activity = NotificationActivityReader::read_activity(&store, &owner, &conversation)
        .await
        .expect("reader")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 7_000);
}

/// `NoopActivityReader` returns `None` for every (owner,
/// conversation) — the evaluator treats this as a miss.
#[tokio::test]
async fn noop_reader_reports_no_activity() {
    let reader = NoopActivityReader;
    let owner = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    let activity = reader
        .read_activity(&owner, &conversation)
        .await
        .expect("noop reader");
    assert!(activity.is_none());
}
