//! XEP-0452: MUC Mention Notifications dedicated suite.
//!
//! Pins the conformant `<mentions><forwarded><message/></forwarded></mentions>`
//! shape, outer room-bare sender, forwarded message extraction, and mention
//! counter bookkeeping.

use minidom::Element;
use waddle_xmpp::xep::xep0452::{
    build_mention_notification_element, build_mention_notification_message,
    extract_mention_notification, has_mention_notification, is_mention_notification_element,
    set_mention_notification, strip_mention_notification, MentionCounter,
    MentionNotificationCarrier, NS_FORWARD, NS_MENTION_NOTIFICATION,
};
use xmpp_parsers::message::{Message, MessageType};

#[test]
fn xep0452_namespace_matches_spec() {
    assert_eq!(NS_MENTION_NOTIFICATION, "urn:xmpp:mmn:0");
    assert_eq!(NS_FORWARD, "urn:xmpp:forward:0");
}

#[test]
fn xep0452_notification_element_wraps_original_forwarded_message() {
    let original = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat' id='msg-42' \
             from='coven@chat.shakespeare.lit/firstwitch' to='coven@chat.shakespeare.lit'>\
           <body>hag66: Thrice the brinded cat hath mew'd.</body>\
           <reference xmlns='urn:xmpp:reference:0' type='mention' begin='0' end='5' uri='xmpp:hag66@shakespeare.lit'/>\
           <stanza-id xmlns='urn:xmpp:sid:0' id='sid-42' by='coven@chat.shakespeare.lit'/>\
         </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    let elem = build_mention_notification_element(&original);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_mention_notification_element(&reparsed));
    assert_eq!(reparsed.name(), "mentions");
    assert_eq!(reparsed.attr("id"), None);
    assert_eq!(reparsed.attr("by"), None);

    let forwarded = reparsed
        .get_child("forwarded", NS_FORWARD)
        .expect("forwarded child");
    let message = forwarded
        .children()
        .find(|child| child.name() == "message")
        .expect("forwarded message");
    assert_eq!(message.attr("id"), Some("msg-42"));
    assert_eq!(message.attr("type"), Some("groupchat"));
    assert_eq!(
        message.attr("from"),
        Some("coven@chat.shakespeare.lit/firstwitch")
    );
    assert_eq!(message.attr("to"), Some("coven@chat.shakespeare.lit"));
    assert_eq!(
        message
            .get_child("body", "jabber:client")
            .map(|body| body.text()),
        Some("hag66: Thrice the brinded cat hath mew'd.".to_owned())
    );
    assert!(message
        .get_child("reference", "urn:xmpp:reference:0")
        .is_some());
    assert!(message.get_child("stanza-id", "urn:xmpp:sid:0").is_some());
}

#[test]
fn xep0452_message_round_trip_recovers_room_from_outer_from() {
    let to: jid::Jid = "alice@example.com".parse().expect("valid jid");
    let original = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat' id='msg-1' \
             from='coven@chat.shakespeare.lit/bob' to='coven@chat.shakespeare.lit'>\
           <body>@alice hey!</body>\
         </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");

    let msg =
        build_mention_notification_message(to.clone(), &original).expect("original has room jid");
    let elem = Element::from(msg);
    let reparsed = Message::try_from(String::from(&elem).parse::<Element>().expect("reparses"))
        .expect("valid message");

    assert_eq!(reparsed.to, Some(to));
    assert_eq!(
        reparsed.from,
        Some("coven@chat.shakespeare.lit".parse().expect("valid room"))
    );
    assert_eq!(reparsed.type_, MessageType::Chat);
    assert!(reparsed.bodies.is_empty());
    assert!(reparsed.has_mention_notification());

    let mentions = reparsed
        .payloads
        .iter()
        .find(|payload| is_mention_notification_element(payload))
        .expect("mentions payload");
    let forwarded_message = mentions
        .get_child("forwarded", NS_FORWARD)
        .and_then(|forwarded| forwarded.children().find(|child| child.name() == "message"))
        .expect("forwarded message");
    assert_eq!(forwarded_message.attr("type"), Some("groupchat"));
    assert_eq!(
        forwarded_message
            .get_child("body", "jabber:client")
            .map(|body| body.text()),
        Some("@alice hey!".to_owned())
    );

    let extracted = reparsed.mention_notification().expect("notification");
    assert_eq!(extracted.message_id, "msg-1");
    assert_eq!(
        extracted.mentioned_by.as_deref(),
        Some("coven@chat.shakespeare.lit/bob")
    );
    assert_eq!(
        extracted.room_jid.as_deref(),
        Some("coven@chat.shakespeare.lit")
    );
}

#[test]
fn xep0452_notification_message_without_preview_has_no_forwarded_body() {
    let to: jid::Jid = "alice@example.com".parse().expect("valid");
    let original = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat' id='m-2' \
             from='room@muc.example.com/nick' to='room@muc.example.com'/>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    let msg = build_mention_notification_message(to, &original).expect("original has room jid");
    assert!(msg.bodies.is_empty());
    assert!(has_mention_notification(&msg));

    let mentions = msg
        .payloads
        .iter()
        .find(|payload| is_mention_notification_element(payload))
        .expect("mentions");
    let forwarded_message = mentions
        .get_child("forwarded", NS_FORWARD)
        .and_then(|forwarded| forwarded.children().find(|child| child.name() == "message"))
        .expect("forwarded message");
    assert!(forwarded_message
        .get_child("body", "jabber:client")
        .is_none());
}

#[test]
fn xep0452_extract_requires_forwarded_message_id() {
    for forwarded_message in [
        "<message xmlns='jabber:client' type='groupchat' from='room@muc/bob' to='room@muc'/>",
        "<message xmlns='jabber:client' type='groupchat' id='' from='room@muc/bob' to='room@muc'/>",
    ] {
        let msg = Message::try_from(
            format!(
                "<message xmlns='jabber:client' from='room@muc'>\
                   <mentions xmlns='urn:xmpp:mmn:0'>\
                     <forwarded xmlns='urn:xmpp:forward:0'>{forwarded_message}</forwarded>\
                   </mentions>\
                 </message>"
            )
            .parse::<Element>()
            .expect("valid xml"),
        )
        .expect("valid message");
        assert!(extract_mention_notification(&msg).is_none());
    }
}

#[test]
fn xep0452_extract_rejects_forwarded_room_mismatch() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' from='room@muc'>\
           <mentions xmlns='urn:xmpp:mmn:0'>\
             <forwarded xmlns='urn:xmpp:forward:0'>\
               <message xmlns='jabber:client' type='groupchat' id='m-1' from='other@muc/bob' to='other@muc'/>\
             </forwarded>\
           </mentions>\
         </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    assert!(extract_mention_notification(&msg).is_none());
}

#[test]
fn xep0452_wrong_namespace_or_name_is_not_recognized() {
    let wrong_ns = Element::builder("mentions", "urn:xmpp:mmn:1").build();
    assert!(!is_mention_notification_element(&wrong_ns));

    let wrong_name = Element::builder("mention", NS_MENTION_NOTIFICATION).build();
    assert!(!is_mention_notification_element(&wrong_name));
}

#[test]
fn xep0452_set_replaces_prior_notification_keeping_exactly_one() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.from = Some("room@muc.example.com".parse().expect("valid room"));
    let first = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat' id='m-1' \
             from='room@muc.example.com/nick' to='room@muc.example.com'/>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    let second = Message::try_from(
        "<message xmlns='jabber:client' type='groupchat' id='m-2' \
             from='room@muc.example.com/nick' to='room@muc.example.com'/>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    set_mention_notification(&mut msg, &first);
    set_mention_notification(&mut msg, &second);

    assert_eq!(
        msg.payloads
            .iter()
            .filter(|payload| payload.ns() == NS_MENTION_NOTIFICATION)
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
