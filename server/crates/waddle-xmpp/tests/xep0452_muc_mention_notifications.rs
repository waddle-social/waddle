//! XEP-0452: MUC Mention Notifications — dedicated suite.
//!
//! Pins the registrar namespace `urn:xmpp:mmn:0`, the notification
//! element's extraction rules (REQUIRED `id`, optional `by`, room JID
//! inferred from the stanza `from`), the notification-message builder
//! shape, and the per-room `MentionCounter` bookkeeping.
//!
//! Known spec divergence (reported, not pinned as conformant):
//! xep-0452.xml wraps the notification in
//! `<mentions xmlns='urn:xmpp:mmn:0'><forwarded/></mentions>` sent
//! from the room's bare JID, whereas this module models a flat
//! `<mention id by/>` payload. The tests below pin the module's actual
//! behaviour; reconciling the wire shape with §"Notifying the mentioned
//! user" is production work outside this suite's scope.

use minidom::Element;
use waddle_xmpp::xep::xep0452::{
    build_mention_notification_element, build_mention_notification_message,
    extract_mention_notification, has_mention_notification, is_mention_notification_element,
    set_mention_notification, strip_mention_notification, MentionCounter, MentionNotification,
    MentionNotificationCarrier, NS_MENTION_NOTIFICATION,
};
use xmpp_parsers::message::{Message, MessageType};

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0452_namespace_matches_spec() {
    // xep-0452.xml registrar entry: `urn:xmpp:mmn:0`.
    assert_eq!(NS_MENTION_NOTIFICATION, "urn:xmpp:mmn:0");
}

// ── Round-trips ──────────────────────────────────────────────────────

#[test]
fn xep0452_notification_survives_serialize_reparse_round_trip() {
    let notif = MentionNotification::new("msg-42").with_by("coven@chat.shakespeare.lit/firstwitch");
    let elem = build_mention_notification_element(&notif);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_mention_notification_element(&reparsed));
    assert_eq!(reparsed.attr("id"), Some("msg-42"));
    assert_eq!(
        reparsed.attr("by"),
        Some("coven@chat.shakespeare.lit/firstwitch")
    );
}

#[test]
fn xep0452_message_round_trip_recovers_room_from_stanza_from() {
    let to: jid::Jid = "alice@example.com".parse().expect("valid jid");
    let room: jid::Jid = "coven@chat.shakespeare.lit".parse().expect("valid jid");
    let notif = MentionNotification::new("msg-1").with_by("coven@chat.shakespeare.lit/bob");

    let msg =
        build_mention_notification_message(to.clone(), room.clone(), &notif, Some("@alice hey!"));
    let elem = Element::from(msg);
    let reparsed = Message::try_from(String::from(&elem).parse::<Element>().expect("reparses"))
        .expect("valid message");

    assert_eq!(reparsed.to, Some(to));
    assert_eq!(reparsed.type_, MessageType::Groupchat);
    assert!(reparsed.has_mention_notification());

    let extracted = reparsed.mention_notification().expect("notification");
    assert_eq!(extracted.message_id, "msg-1");
    assert_eq!(
        extracted.mentioned_by.as_deref(),
        Some("coven@chat.shakespeare.lit/bob")
    );
    // The room is not an attribute — it is recovered from the stanza
    // `from`, which the builder sets to the room JID.
    assert_eq!(
        extracted.room_jid.as_deref(),
        Some("coven@chat.shakespeare.lit")
    );
}

#[test]
fn xep0452_builder_omits_by_attribute_when_unset() {
    let elem = build_mention_notification_element(&MentionNotification::new("m-1"));
    assert_eq!(elem.attr("id"), Some("m-1"));
    assert_eq!(elem.attr("by"), None, "unset by must not emit by=''");
}

#[test]
fn xep0452_notification_message_without_preview_has_no_body() {
    let to: jid::Jid = "alice@example.com".parse().expect("valid");
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid");
    let msg = build_mention_notification_message(to, room, &MentionNotification::new("m-2"), None);
    assert!(msg.bodies.is_empty());
    assert!(has_mention_notification(&msg));
}

// ── Extraction robustness ────────────────────────────────────────────

#[test]
fn xep0452_extract_requires_non_empty_id() {
    for mention in [
        "<mention xmlns='urn:xmpp:mmn:0'/>",
        "<mention xmlns='urn:xmpp:mmn:0' id=''/>",
    ] {
        let msg = Message::try_from(
            format!("<message xmlns='jabber:client' type='groupchat'>{mention}</message>")
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");
        assert!(
            extract_mention_notification(&msg).is_none(),
            "id is REQUIRED; `{mention}` must not extract"
        );
    }
}

#[test]
fn xep0452_empty_by_attribute_becomes_none() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat'>\
            <mention xmlns='urn:xmpp:mmn:0' id='m-1' by=''/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    let notif = extract_mention_notification(&msg).expect("id present");
    assert_eq!(notif.mentioned_by, None);
}

#[test]
fn xep0452_wrong_namespace_mention_is_not_recognized() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat'>\
            <mention xmlns='urn:xmpp:mmn:1' id='m-1'/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    assert!(!has_mention_notification(&msg));
    assert!(extract_mention_notification(&msg).is_none());

    let wrong_name = Element::builder("mentions", NS_MENTION_NOTIFICATION).build();
    assert!(!is_mention_notification_element(&wrong_name));
}

// ── Mutation semantics ───────────────────────────────────────────────

#[test]
fn xep0452_set_replaces_prior_notification_keeping_exactly_one() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_mention_notification(&mut msg, &MentionNotification::new("m-1"));
    set_mention_notification(&mut msg, &MentionNotification::new("m-2"));

    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| e.ns() == NS_MENTION_NOTIFICATION)
            .count(),
        1
    );
    assert_eq!(
        extract_mention_notification(&msg)
            .expect("notification")
            .message_id,
        "m-2"
    );

    strip_mention_notification(&mut msg);
    assert!(!has_mention_notification(&msg));
}

// ── Counter bookkeeping ──────────────────────────────────────────────

#[test]
fn xep0452_counter_tracks_per_room_and_totals() {
    let mut counter = MentionCounter::new();
    counter.increment("room1@muc");
    counter.increment("room1@muc");
    counter.increment("room2@muc");

    assert_eq!(counter.count("room1@muc"), 2);
    assert_eq!(counter.count("room2@muc"), 1);
    assert_eq!(counter.count("room3@muc"), 0);
    assert_eq!(counter.total(), 3);

    let mut rooms = counter.rooms_with_mentions();
    rooms.sort_by_key(|(room, _)| room.to_owned());
    assert_eq!(rooms, vec![("room1@muc", 2), ("room2@muc", 1)]);
}

#[test]
fn xep0452_counter_clear_room_and_clear_all() {
    let mut counter = MentionCounter::new();
    counter.increment("a@muc");
    counter.increment("b@muc");

    counter.clear_room("a@muc");
    assert_eq!(counter.count("a@muc"), 0);
    assert_eq!(counter.total(), 1);

    counter.clear_all();
    assert_eq!(counter.total(), 0);
    assert!(counter.rooms_with_mentions().is_empty());
}
