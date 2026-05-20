use super::*;
use crate::xep::xep0085::{build_chat_state_element, ChatState};
use crate::xep::xep0334::{add_hint, Hint};
use xmpp_parsers::message::MessageType;

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn dm(from: &str, to: &str, message_type: MessageType, body: Option<&str>) -> Message {
    let mut m = Message::new(Some(to.parse::<Jid>().expect("valid jid")));
    m.from = Some(from.parse::<Jid>().expect("valid jid"));
    m.type_ = message_type;
    if let Some(body_text) = body {
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), body_text.to_string());
    }
    m
}

fn one_resource_online(priority: i8) -> OnlineResources {
    OnlineResources::from_pairs([(full("alice@example.com/web"), priority)])
}

// ── XEP-0191 blocking ───────────────────────────────────────────

#[test]
fn blocked_sender_drops_entire_routing() {
    let msg = dm(
        "blocked@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let block = Blocklist::new([bare("blocked@elsewhere")]);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &block);
    assert_eq!(routing, DmRouting::dropped());
}

// ── XEP-0160 §3 storage trigger ─────────────────────────────────

#[test]
fn chat_to_offline_recipient_creates_archived_pending() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
    assert_eq!(routing.live, LiveDecision::None);
    assert_eq!(
        routing.inbox,
        InboxDecision::UpdateUnread {
            has_archive_ref: true
        }
    );
}

#[test]
fn negative_priority_resources_count_as_offline_for_storage() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &one_resource_online(-1), &Blocklist::empty());
    assert_eq!(routing.pending, PendingDecision::Archived);
    assert_eq!(routing.live, LiveDecision::None);
}

#[test]
fn chat_to_online_recipient_skips_pending_routes_live() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
}

#[test]
fn full_jid_target_to_negative_priority_resource_is_still_live() {
    // RFC 6121 §8.5.3: full-JID-addressed messages go to the
    // specific resource regardless of priority. The negative-
    // priority filter is for bare-JID delivery (§8.5.2) only.
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com/web",
        MessageType::Chat,
        Some("hi"),
    );
    // Resource is online but with negative priority.
    let routing = classify_dm_intake(&msg, &one_resource_online(-1), &Blocklist::empty());
    assert_eq!(routing.live, LiveDecision::DeliverToFull);
    assert_eq!(routing.pending, PendingDecision::None);
}

#[test]
fn full_jid_target_to_disconnected_resource_falls_back_to_bare_jid_routing() {
    // RFC 6121 §8.5.3: if the addressed resource is not connected,
    // the server SHOULD treat the stanza "as if it had been
    // addressed to the user's bare JID" — i.e. fall back to
    // §8.5.2 fanout to non-negative-priority resources, NOT
    // jump straight to offline storage.
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com/laptop",
        MessageType::Chat,
        Some("hi"),
    );
    // /web is online with priority=1; /laptop is not connected.
    // §8.5.3 fallback should send live to /web, not store offline.
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.pending, PendingDecision::None);
    // The classifier's `live` reflects the wire `to` shape (the
    // routing layer below this is responsible for resolving the
    // bare-JID-fallback fanout), so we still see DeliverToFull.
    // What matters for §8.5.3 conformance is that we did NOT
    // store offline and DID emit a live decision.
    assert_eq!(routing.live, LiveDecision::DeliverToFull);
}

#[test]
fn full_jid_target_to_fully_offline_user_stores_offline() {
    // No resources online at all → standard §8.5.2 offline path.
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com/laptop",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.live, LiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn full_jid_target_uses_deliver_to_full() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com/web",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.live, LiveDecision::DeliverToFull);
}

// ── XEP-0160 §4 type matrix ─────────────────────────────────────

#[test]
fn normal_type_is_storable() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Normal,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn headline_type_skips_storage_and_pending() {
    let msg = dm(
        "system@example.com",
        "alice@example.com",
        MessageType::Headline,
        Some("notif"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
}

#[test]
fn groupchat_is_skipped_by_dm_classifier() {
    let msg = dm(
        "room@conf.example.com/bob",
        "alice@example.com",
        MessageType::Groupchat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing, {
        let mut r = DmRouting::dropped();
        r.carbons = CarbonsDecision::Suppressed;
        r
    });
}

#[test]
fn error_type_is_silently_dropped_when_offline() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Error,
        None,
    );
    // <store/> hint must be ignored on error per XEP-0334 §6 ¶3.
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn error_type_when_online_routes_live_only() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Error,
        None,
    );
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
}

// ── XEP-0085 chat-state-only ────────────────────────────────────

#[test]
fn chat_state_only_is_not_stored_or_queued() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        None,
    );
    msg.payloads
        .push(build_chat_state_element(ChatState::Composing));
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

// ── XEP-0334 hint matrix ────────────────────────────────────────

#[test]
fn no_store_hint_suppresses_archive_and_pending() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("ephemeral"),
    );
    add_hint(&mut msg, Hint::NoStore);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn no_permanent_store_hint_uses_transient_pending() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("off the record"),
    );
    add_hint(&mut msg, Hint::NoPermanentStore);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::Transient);
    // Locked Q10b: Transient leaves no inbox trace.
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn no_permanent_store_when_recipient_online_skips_pending() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("otr"),
    );
    add_hint(&mut msg, Hint::NoPermanentStore);
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
}

#[test]
fn store_hint_overrides_default_skip_for_headline() {
    let mut msg = dm(
        "system@example.com",
        "alice@example.com",
        MessageType::Headline,
        None,
    );
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    // XEP-0334 §5.4 says <store/> SHOULD store; XEP-0160 §4 says
    // headline SHOULD NOT be stored offline. The two SHOULDs
    // conflict; the project rule (see module header) chooses
    // <store/> wins, so headline+<store/> goes to BOTH MAM (Mam)
    // and offline storage (Archived) — pending derives from
    // `archive == ArchiveDecision::Mam`.
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn store_hint_does_not_override_error_type() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Error,
        None,
    );
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    // §6 ¶3: hints in error stanzas are ignored.
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
}

#[test]
fn store_overrides_no_store_more_restrictive_loses() {
    // XEP-0334 §5.4: <store/> SHOULD store. Project rule: <store/>
    // wins over conflicting <no-store/>.
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    add_hint(&mut msg, Hint::NoStore);
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn no_copy_on_full_jid_suppresses_carbons() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com/web",
        MessageType::Chat,
        Some("secret"),
    );
    add_hint(&mut msg, Hint::NoCopy);
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.carbons, CarbonsDecision::Suppressed);
}

#[test]
fn no_copy_on_bare_jid_does_not_suppress_carbons() {
    // XEP-0334 §5.3 ¶2: no-copy MUST NOT override bare-JID fanout.
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("secret"),
    );
    add_hint(&mut msg, Hint::NoCopy);
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.carbons, CarbonsDecision::Eligible);
}

#[test]
fn no_copy_in_error_stanza_is_ignored() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com/web",
        MessageType::Error,
        None,
    );
    add_hint(&mut msg, Hint::NoCopy);
    // §6 ¶3: hints in error stanzas are ignored — so no-copy doesn't
    // suppress carbons. But error-type itself is not carbons-eligible
    // by XEP-0280. Our classifier sets Suppressed for errors anyway,
    // but the route is via the type guard, not the hint.
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.carbons, CarbonsDecision::Suppressed);
}

// ── DmRouting helpers ───────────────────────────────────────────

#[test]
fn dropped_routing_has_all_sinks_off() {
    let r = DmRouting::dropped();
    assert_eq!(r.archive, ArchiveDecision::None);
    assert_eq!(r.pending, PendingDecision::None);
    assert_eq!(r.live, LiveDecision::None);
    assert_eq!(r.inbox, InboxDecision::None);
}

// ── Issue #209 body-less chat DMs ─────────────────────────────────

#[test]
fn body_less_chat_to_offline_recipient_is_transient_not_archived() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        None,
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::Transient);
    assert_eq!(routing.live, LiveDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn body_less_chat_with_store_hint_archives_to_mam() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        None,
    );
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn body_less_chat_to_online_recipient_does_not_archive_or_queue() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        None,
    );
    let routing = classify_dm_intake(&msg, &one_resource_online(1), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::DeliverToBareWithFanout);
    assert_eq!(routing.inbox, InboxDecision::None);
}

// ── XEP-0280 carbon wrappers ─────────────────────────────────────

#[test]
fn carbon_sent_wrapper_is_dropped_not_classified_as_dm() {
    use minidom::Element;
    use waddle_xmpp_core::carbons::CARBONS_NS;

    let mut msg = dm(
        "alice@example.com",
        "alice@example.com/web",
        MessageType::Chat,
        None,
    );
    msg.payloads
        .push(Element::builder("sent", CARBONS_NS).build());
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing, DmRouting::dropped());
}

#[test]
fn carbon_received_wrapper_is_dropped_not_classified_as_dm() {
    use minidom::Element;
    use waddle_xmpp_core::carbons::CARBONS_NS;

    let mut msg = dm(
        "alice@example.com",
        "alice@example.com/web",
        MessageType::Chat,
        None,
    );
    msg.payloads
        .push(Element::builder("received", CARBONS_NS).build());
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing, DmRouting::dropped());
}

#[test]
fn online_resources_priority_threshold() {
    let none = OnlineResources::empty();
    let neg = OnlineResources::from_pairs([(full("a@b/c"), -1)]);
    let zero = OnlineResources::from_pairs([(full("a@b/c"), 0)]);
    let pos = OnlineResources::from_pairs([(full("a@b/c"), 5)]);
    assert!(!none.has_non_negative_priority());
    assert!(!neg.has_non_negative_priority());
    assert!(zero.has_non_negative_priority());
    assert!(pos.has_non_negative_priority());
}
