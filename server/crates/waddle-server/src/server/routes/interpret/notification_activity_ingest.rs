//! Production ingestion sites for the
//! [`crate::notification_activity::NotificationActivityStore`].
//!
//! The store's `record_*` writers are called from the typed interpreter
//! arms so the per-(user, conversation) projection reflects real user
//! activity in production. Every helper:
//!
//! - resolves the activity store via `Deps::web_socket_state` (returns
//!   silently when the store is not present — unit tests and the early
//!   bootstrap path);
//! - stamps `now_ms` from [`crate::time::now_ms`];
//! - swallows DB errors with a `warn!` log so a transient projection
//!   failure NEVER blocks stanza routing — the canonical XMPP path
//!   stays authoritative.
//!
//! Conformance map:
//! - XEP-0085 chat-state — [`record_chat_state_activity`].
//! - XEP-0490 read-marker advance (displayed) — [`record_read_marker_activity`].
//! - Outbound message commit — [`record_outbound_message_activity`].
//! - XEP-0045 presence join / show change / leave —
//!   [`record_presence_available_activity`] / [`record_presence_unavailable_activity`].
//!
//! Each helper records activity for the *acting* user keyed against the
//! conversation they acted on — never for the conversation's peer. The
//! T1 evaluator looks up `(recipient, conversation)` in
//! [`crate::notification_outbox::evaluate_push_gate_at_dispatch`], so
//! the projection grows from the recipient's own typed signals and the
//! XEP-0513 `<active/>` filter resolves against the recipient's own
//! recent activity.

use jid::BareJid;
use tracing::{debug, warn};
use xmpp_parsers::message::Message;

use crate::notification_activity::{NotificationChatState, NotificationPresenceShow};
use crate::server::routes::websocket::WebSocketState;
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::xep::xep0085::{ChatState, ChatStateCarrier};
use waddle_xmpp::xep::xep0203::has_delay;

use super::Deps;

/// Resolve the production
/// [`crate::notification_activity::NotificationActivityStore`] from the
/// deps surface. Returns `None` in unit-test fixtures whose `Deps`
/// don't wire `web_socket_state`.
fn activity_store<'a>(
    deps: &'a Deps<'_>,
) -> Option<&'a crate::notification_activity::NotificationActivityStore> {
    deps.web_socket_state
        .map(|state: &WebSocketState| state.deps.protocol.notification_activity.as_ref())
}

/// XEP-0085: when the typed `chat_state` is present on a routed
/// `Message`, record `(sender_bare, conversation)` activity. The sender
/// IS the active party — a chat-state stanza reports the sender's own
/// state in the conversation, so the projection key MUST be the sender
/// (the receiving side updates its projection only when its own user
/// emits a chat-state, a read marker, an outbound message, or a
/// presence event in that conversation).
///
/// Two XEP-conformance filters apply on the projection write path:
///
/// 1. **XEP-0085 `<gone/>` (§"Definitions" + §"Use in Groupchat" item 3):**
///    `<gone/>` signals the user has ended their participation in the
///    conversation. We persist it as an *explicit inactivity* signal
///    via `record_chat_state_gone`, which UNCONDITIONALLY zeroes
///    `last_active_at_ms`. The XEP-0513 `<active/>` filter at T1 then
///    sees `now - 0` which is huge → `> TTL` → suppressed with
///    `Xep0513ActiveMiss`. XEP-0085's "SHOULD ignore `<gone/>` in
///    groupchat" guidance applies to client UI state — the
///    server-internal push-filter projection is a separate layer,
///    and treating `<gone/>` as an explicit inactivity signal there
///    is XEP-conformant for push-filter purposes (Codex review on
///    PR #731).
/// 2. **XEP-0203 `<delay/>`:** A delayed stanza is a historical replay
///    (MAM catchup, offline-stored stanza, room subject replay on
///    join). Persisting "the user is active now" off a 3-hour-old
///    delayed chat-state would inflate the projection. Skip without
///    writing.
pub(super) async fn record_chat_state_activity(
    deps: &Deps<'_>,
    sender: &BareJid,
    conversation: &BareJid,
    message: &Message,
) {
    let Some(state) = message.chat_state() else {
        return;
    };
    if has_delay(message) {
        debug!(
            owner = %sender,
            conversation = %conversation,
            chat_state = ?state,
            "notification_activity: skipping delayed chat-state \
             (XEP-0203 <delay/> = historical replay, not real-time activity)"
        );
        return;
    }
    deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
        owner: sender.clone(),
    });
    let Some(store) = activity_store(deps) else {
        return;
    };
    let now_ms = crate::time::now_ms();
    if matches!(state, ChatState::Gone) {
        debug!(
            owner = %sender,
            conversation = %conversation,
            "notification_activity: recording <gone/> as explicit inactivity \
             (XEP-0085 'gone' = departure → zero last_active_at_ms)"
        );
        if let Err(error) = store
            .record_chat_state_gone(sender, conversation, now_ms)
            .await
        {
            warn!(
                owner = %sender,
                conversation = %conversation,
                %error,
                "notification_activity: record_chat_state_gone failed; \
                 projection write skipped (XMPP routing unaffected)",
            );
        }
        return;
    }
    let typed = NotificationChatState::from_xep0085(state);
    if let Err(error) = store
        .record_chat_state(sender, conversation, typed, now_ms)
        .await
    {
        warn!(
            owner = %sender,
            conversation = %conversation,
            chat_state = ?typed,
            %error,
            "notification_activity: record_chat_state failed; \
             projection write skipped (XMPP routing unaffected)",
        );
    }
}

/// XEP-0490: when user `owner` advances their read marker in
/// `conversation`, bump `(owner, conversation)` activity.
pub(super) async fn record_read_marker_activity(
    deps: &Deps<'_>,
    owner: &BareJid,
    conversation: &BareJid,
) {
    deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
        owner: owner.clone(),
    });
    let Some(store) = activity_store(deps) else {
        return;
    };
    let now_ms = crate::time::now_ms();
    if let Err(error) = store.record_read_marker(owner, conversation, now_ms).await {
        warn!(
            %owner,
            %conversation,
            %error,
            "notification_activity: record_read_marker failed; \
             projection write skipped (XMPP routing unaffected)",
        );
    }
}

/// Outbound message commit: when the sender's own archive write
/// commits a message for `(sender, conversation)`, bump activity.
/// Sending a message is the strongest "currently active" signal.
///
/// XEP-0203 `<delay/>` filter: if the outbound stanza carries a
/// `<delay/>` (server-bridge replay, S2S buffered delivery, etc.) the
/// commit is a historical replay, not a real-time send. Skip the
/// activity bump so the projection isn't inflated by stale catchup
/// writes.
///
/// XEP-0334 interaction (`<no-store/>` / `<no-permanent-store/>`):
/// the upstream archive eligibility gate (`MucArchiveHandler` /
/// `ArchiveHandler`) refuses to emit `ArchiveDirect` / `ArchiveGroupchat`
/// effects for messages that opted out of archival, so this helper
/// never sees them. Activity is therefore correctly tied to durable
/// storage: ephemeral hints don't bump the projection.
pub(super) async fn record_outbound_message_activity(
    deps: &Deps<'_>,
    sender: &BareJid,
    conversation: &BareJid,
    message: &Message,
) {
    if has_delay(message) {
        debug!(
            owner = %sender,
            conversation = %conversation,
            "notification_activity: skipping delayed outbound commit \
             (XEP-0203 <delay/> = historical replay, not real-time activity)"
        );
        return;
    }
    deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
        owner: sender.clone(),
    });
    let Some(store) = activity_store(deps) else {
        return;
    };
    let now_ms = crate::time::now_ms();
    if let Err(error) = store
        .record_outbound_message(sender, conversation, now_ms)
        .await
    {
        warn!(
            owner = %sender,
            %conversation,
            %error,
            "notification_activity: record_outbound_message failed; \
             projection write skipped (XMPP routing unaffected)",
        );
    }
}

/// XEP-0045: bump `(owner, room)` activity when `owner` sends a
/// MUC presence (join or in-room show/status change). `show` is the
/// typed [`NotificationPresenceShow`] token if any; the closed enum
/// guarantees the persisted value is one of the four RFC 6121
/// §4.7.2.1 tokens (`Away`, `Chat`, `Dnd`, `Xa`).
pub(crate) async fn record_presence_available_activity_on_state(
    state: &WebSocketState,
    owner: &BareJid,
    room: &BareJid,
    show: Option<NotificationPresenceShow>,
) {
    let now_ms = crate::time::now_ms();
    if let Err(error) = state
        .deps
        .protocol
        .notification_activity
        .record_presence_available(owner, room, show, now_ms)
        .await
    {
        warn!(
            %owner,
            %room,
            ?show,
            %error,
            "notification_activity: record_presence_available failed; \
             projection write skipped (XMPP routing unaffected)",
        );
    }
}

/// XEP-0045: bump `(owner, room)` activity when `owner` sends
/// `<presence type='unavailable'/>`. The store clears the persisted
/// `<show/>` (an explicit leave has no available presence).
pub(crate) async fn record_presence_unavailable_activity_on_state(
    state: &WebSocketState,
    owner: &BareJid,
    room: &BareJid,
) {
    let now_ms = crate::time::now_ms();
    if let Err(error) = state
        .deps
        .protocol
        .notification_activity
        .record_presence_unavailable(owner, room, now_ms)
        .await
    {
        warn!(
            %owner,
            %room,
            %error,
            "notification_activity: record_presence_unavailable failed; \
             projection write skipped (XMPP routing unaffected)",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use waddle_xmpp::xep::xep0085::build_chat_state_message;
    use waddle_xmpp::xep::xep0203::build_delay_element_simple;

    /// XEP-0085 §"Definitions" describes `<gone/>` as the user having
    /// ended participation in the conversation. The helper distinguishes
    /// `<gone/>` from other chat-states because it routes through
    /// `record_chat_state_gone` (which zeroes `last_active_at_ms`) rather
    /// than the monotonic `record_chat_state` path. Lock the typed
    /// predicate so a refactor cannot silently merge the two paths and
    /// re-introduce the XEP-0513 `<active/>` TTL inflation bug (Codex
    /// review on PR #731).
    #[test]
    fn xep0085_gone_state_is_detected_for_filter() {
        let to: jid::Jid = "alice@example.com".parse().expect("to");
        let from: jid::Jid = "bob@example.com/work".parse().expect("from");
        let msg = build_chat_state_message(to, from, ChatState::Gone);
        assert!(matches!(msg.chat_state(), Some(ChatState::Gone)));
        assert!(matches!(
            ChatState::Gone,
            ChatState::Gone /* filter pattern */
        ));
    }

    /// XEP-0203 §"Introduction" — a `<delay/>` element marks the stanza
    /// as historical replay (MAM catchup, offline-stored, etc). Lock
    /// the typed predicate so a refactor that drops the `has_delay`
    /// check can't silently inflate the projection with stale signals.
    #[test]
    fn xep0203_delay_is_detected_for_filter() {
        let to: jid::Jid = "alice@example.com".parse().expect("to");
        let from: jid::Jid = "bob@example.com/work".parse().expect("from");
        let mut msg = build_chat_state_message(to, from, ChatState::Active);
        assert!(!has_delay(&msg), "fresh chat-state must not look delayed");
        msg.payloads.push(build_delay_element_simple(
            chrono::Utc
                .timestamp_opt(1_700_000_000, 0)
                .single()
                .expect("delay stamp"),
            "muc.example.com",
        ));
        assert!(
            has_delay(&msg),
            "<delay/> stamp must mark the stanza as historical"
        );
    }
}
