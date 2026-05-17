//! XEP-0492 push-enforcement gate conformance suite.
//!
//! XEP-0492 (Chat Notification Settings) defines three notification
//! levels — `<always/>`, `<on-mention/>`, `<never/>` — that the server
//! MUST consult before fanning a message out to the recipient's
//! registered XEP-0357 Push Service nodes. The wire format and the
//! durable per-`(owner, conversation)` projection are covered by
//! `waddle-xmpp/tests/xep0492_chat_notification_settings.rs` (parsing,
//! validation, round-trip) and by the projection unit tests in
//! `waddle_server::notification_settings_projection`.
//!
//! This file covers the *enforcement* boundary that those two suites
//! deliberately do NOT exercise: the pure reducer
//! [`waddle_server::notification_settings_projection::PushDispatchDecision::evaluate`]
//! that the offline-delivery interpret arm consults immediately before
//! invoking the push provider. The reducer is the single decision point
//! shared by every conversation kind (`Direct`, `PrivateGroup`,
//! `PublicGroup`); the wire-level DM regression suite in
//! `server/crates/waddle-server/src/server/routes/websocket/tests/messages.rs`
//! drives the same reducer through the full `QueueOfflineDelivery`
//! → push fan-out path with real projection-store writes for the DM
//! arm. MUC fan-out is not yet wired through `QueueOfflineDelivery`
//! (groupchat occupant push is a separate landing surface); the MUC
//! cases here lock the reducer behaviour today so that when MUC push
//! lands it cannot regress the matrix.
//!
//! Matrix covered (3 levels × 2 mention states × 3 conversation kinds
//! = 18 deterministic cases) plus self-consistency invariants on the
//! typed `PushDispatchDecision` payload — per the CLAUDE.md hard rule
//! every dispatch outcome flows as a typed enum, never as a string.
//!
//! The dedicated wire-level DM matrix (3 levels × 2 mention states)
//! lives next to the existing offline-delivery harness in
//! `tests/messages.rs` — see `xep0492_direct_chat_*` tests there.

use waddle_server::notification_settings_projection::{ConversationKind, PushDispatchDecision};
use waddle_xmpp::xep::NotificationLevel;

// ---------------------------------------------------------------------------
// `<always/>` — XEP-0492 §3 fallback. Every conformant deployment MUST
// deliver every push regardless of mention state.
// ---------------------------------------------------------------------------

#[test]
fn xep0492_always_delivers_without_mention() {
    assert_eq!(
        PushDispatchDecision::evaluate(NotificationLevel::Always, false),
        PushDispatchDecision::Deliver
    );
}

#[test]
fn xep0492_always_delivers_with_mention() {
    assert_eq!(
        PushDispatchDecision::evaluate(NotificationLevel::Always, true),
        PushDispatchDecision::Deliver
    );
}

// ---------------------------------------------------------------------------
// `<on-mention/>` — XEP-0492 §3. Push is delivered only when the
// recipient is the target of an XEP-0513 explicit mention. The
// suppressed branch MUST carry the typed `OnMention` reason so audit
// logs (and adversarial tests) can distinguish "muted because of level"
// from "muted because of `<never/>`".
// ---------------------------------------------------------------------------

#[test]
fn xep0492_on_mention_suppresses_without_mention_with_typed_reason() {
    assert_eq!(
        PushDispatchDecision::evaluate(NotificationLevel::OnMention, false),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::OnMention
        }
    );
}

#[test]
fn xep0492_on_mention_delivers_with_mention() {
    assert_eq!(
        PushDispatchDecision::evaluate(NotificationLevel::OnMention, true),
        PushDispatchDecision::Deliver
    );
}

// ---------------------------------------------------------------------------
// `<never/>` — XEP-0492 §3. Push MUST be suppressed regardless of
// mention state. The typed reason MUST be `Never` so operators can
// distinguish from `OnMention` mutes.
// ---------------------------------------------------------------------------

#[test]
fn xep0492_never_suppresses_without_mention_with_typed_reason() {
    assert_eq!(
        PushDispatchDecision::evaluate(NotificationLevel::Never, false),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::Never
        }
    );
}

#[test]
fn xep0492_never_suppresses_with_mention_with_typed_reason() {
    assert_eq!(
        PushDispatchDecision::evaluate(NotificationLevel::Never, true),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::Never
        }
    );
}

// ---------------------------------------------------------------------------
// Conversation-kind defaults (XEP-0492 §3.2). The reducer is unaware of
// `ConversationKind` directly — its inputs are a *resolved*
// `NotificationLevel` plus the mention bit. The
// `ConversationKind::default_notification_setting` mapping is the
// upstream input to the reducer; lock the contract at the type level
// here so a regression to the defaults (e.g. public MUC silently
// flipping to `Always`) is caught.
// ---------------------------------------------------------------------------

#[test]
fn xep0492_direct_default_level_is_always() {
    assert_eq!(
        ConversationKind::Direct.default_notification_setting(),
        NotificationLevel::Always,
        "DMs default to <always/> per XEP-0492 §3.2"
    );
}

#[test]
fn xep0492_private_group_default_level_is_always() {
    assert_eq!(
        ConversationKind::PrivateGroup.default_notification_setting(),
        NotificationLevel::Always,
        "private MUCs default to <always/> per XEP-0492 §3.2"
    );
}

#[test]
fn xep0492_public_group_default_level_is_on_mention() {
    assert_eq!(
        ConversationKind::PublicGroup.default_notification_setting(),
        NotificationLevel::OnMention,
        "public MUCs default to <on-mention/> per XEP-0492 §3.2"
    );
}

// ---------------------------------------------------------------------------
// Composed default-driven matrices — exercise the (kind → default level)
// → reducer chain for every (kind, mention) pair. These are the cases
// that will fire when MUC push fan-out lands without a per-conversation
// override in the projection store.
// ---------------------------------------------------------------------------

#[test]
fn xep0492_direct_default_path_delivers_without_mention() {
    let level = ConversationKind::Direct.default_notification_setting();
    assert_eq!(
        PushDispatchDecision::evaluate(level, false),
        PushDispatchDecision::Deliver
    );
}

#[test]
fn xep0492_direct_default_path_delivers_with_mention() {
    let level = ConversationKind::Direct.default_notification_setting();
    assert_eq!(
        PushDispatchDecision::evaluate(level, true),
        PushDispatchDecision::Deliver
    );
}

#[test]
fn xep0492_private_group_default_path_delivers_without_mention() {
    let level = ConversationKind::PrivateGroup.default_notification_setting();
    assert_eq!(
        PushDispatchDecision::evaluate(level, false),
        PushDispatchDecision::Deliver
    );
}

#[test]
fn xep0492_private_group_default_path_delivers_with_mention() {
    let level = ConversationKind::PrivateGroup.default_notification_setting();
    assert_eq!(
        PushDispatchDecision::evaluate(level, true),
        PushDispatchDecision::Deliver
    );
}

#[test]
fn xep0492_public_group_default_path_suppresses_without_mention() {
    let level = ConversationKind::PublicGroup.default_notification_setting();
    assert_eq!(
        PushDispatchDecision::evaluate(level, false),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::OnMention
        }
    );
}

#[test]
fn xep0492_public_group_default_path_delivers_with_mention() {
    let level = ConversationKind::PublicGroup.default_notification_setting();
    assert_eq!(
        PushDispatchDecision::evaluate(level, true),
        PushDispatchDecision::Deliver
    );
}

// ---------------------------------------------------------------------------
// Typed-payload invariants. `PushDispatchDecision` MUST be a closed
// enum whose suppression arm carries a typed `NotificationLevel`. A
// regression that flips `reason` to a `String` (or removes the typed
// variant entirely) would silently break observability and is the
// failure mode the CLAUDE.md typed-payloads rule exists to catch.
// ---------------------------------------------------------------------------

#[test]
fn xep0492_decision_should_deliver_matches_variant() {
    assert!(PushDispatchDecision::Deliver.should_deliver());
    assert!(!PushDispatchDecision::Suppressed {
        reason: NotificationLevel::Never
    }
    .should_deliver());
    assert!(!PushDispatchDecision::Suppressed {
        reason: NotificationLevel::OnMention
    }
    .should_deliver());
}

#[test]
fn xep0492_suppression_reason_never_distinct_from_on_mention() {
    let never = PushDispatchDecision::evaluate(NotificationLevel::Never, false);
    let on_mention = PushDispatchDecision::evaluate(NotificationLevel::OnMention, false);
    assert_ne!(
        never, on_mention,
        "<never/> and <on-mention/> MUST surface distinct typed suppression reasons"
    );
}
