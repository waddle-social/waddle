//! Notification activity store writers (XEP-0085, XEP-0490, XEP-0045, outbound messages).
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use waddle_server::notification_activity::NotificationActivityReader;

use crate::support::*;

/// XEP-0085 ingestion: a writer call persists the typed chat
/// state and is readable via the projection store's reader trait.
/// Per CLAUDE.md per-XEP test discipline.
#[tokio::test]
async fn xep0085_chat_state_writer_persists_typed_token() {
    let store = activity_store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    store
        .record_chat_state(
            &owner,
            &conversation,
            waddle_server::notification_activity::NotificationChatState::Composing,
            42,
        )
        .await
        .expect("record chat-state");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 42);
    assert_eq!(
        activity.last_chat_state,
        Some(waddle_server::notification_activity::NotificationChatState::Composing),
    );
}

/// XEP-0490 ingestion: a read-marker writer persists the typed
/// last_read_at_ms timestamp and updates `last_active_at_ms`.
/// Per CLAUDE.md per-XEP test discipline.
#[tokio::test]
async fn xep0490_read_marker_writer_persists_typed_timestamp() {
    let store = activity_store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");
    store
        .record_read_marker(&owner, &conversation, 11_000)
        .await
        .expect("record marker");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 11_000);
    assert_eq!(activity.last_read_at_ms, Some(11_000));
}

/// Outbound message commit: writer call updates the sender's
/// activity row for the conversation. Per CLAUDE.md per-XEP test
/// discipline.
#[tokio::test]
async fn outbound_message_writer_persists_activity_for_sender() {
    let store = activity_store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("bob@example.com");
    store
        .record_outbound_message(&owner, &conversation, 9_999)
        .await
        .expect("record outbound");
    let activity = store
        .read_activity(&owner, &conversation)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(activity.last_active_at_ms, 9_999);
}

/// XEP-0045 ingestion: presence available + unavailable both bump
/// `last_active_at_ms`; the show is preserved on available and
/// cleared on unavailable. Per CLAUDE.md per-XEP test discipline.
#[tokio::test]
async fn xep0045_presence_writer_persists_show_and_clears_on_unavailable() {
    let store = activity_store().await;
    let owner = bare("alice@example.com");
    let room = bare("room@muc.example.com");
    store
        .record_presence_available(
            &owner,
            &room,
            Some(waddle_server::notification_activity::NotificationPresenceShow::Dnd),
            1_000,
        )
        .await
        .expect("available");
    let after_available = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(after_available.last_active_at_ms, 1_000);
    assert_eq!(
        after_available.presence_show,
        Some(waddle_server::notification_activity::NotificationPresenceShow::Dnd)
    );

    store
        .record_presence_unavailable(&owner, &room, 2_000)
        .await
        .expect("unavailable");
    let after_unavailable = store
        .read_activity(&owner, &room)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(after_unavailable.last_active_at_ms, 2_000);
    assert!(after_unavailable.presence_show.is_none());
}
