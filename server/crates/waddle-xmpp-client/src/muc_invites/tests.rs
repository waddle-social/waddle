use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::muc::user::Reason;

use super::{
    build_muc_decline_message, build_muc_invite_message, parse_muc_invitation_events,
    MucInvitationEvent, MucInvitationPassword,
};

#[test]
fn xep0045_outbound_invite_and_decline_address_the_room() {
    let room = "coven@chat.example".parse().unwrap();
    let peer: Jid = "bob@example.com/phone".parse().unwrap();
    let invite = build_muc_invite_message(&room, &peer, Some(Reason("<&>".into())));
    let decline = build_muc_decline_message(&room, &peer, Some(Reason("Busy".into())));
    let silent_decline = build_muc_decline_message(&room, &peer, None);

    for (message, payload) in [
        (
            invite,
            "<x xmlns='http://jabber.org/protocol/muc#user'>\
                <invite to='bob@example.com/phone'><reason>&lt;&amp;&gt;</reason></invite></x>",
        ),
        (
            decline,
            "<x xmlns='http://jabber.org/protocol/muc#user'>\
                <decline to='bob@example.com/phone'><reason>Busy</reason></decline></x>",
        ),
        (
            silent_decline,
            "<x xmlns='http://jabber.org/protocol/muc#user'>\
                <decline to='bob@example.com/phone'/></x>",
        ),
    ] {
        let stanza = Element::from(message);
        let message = Message::try_from(stanza).unwrap();
        assert_eq!(message.to, Some(Jid::from(room.clone())));
        assert_eq!(message.type_, MessageType::Normal);
        assert!(message.id.is_some());
        assert!(message.from.is_none());
        assert!(message.bodies.is_empty());
        assert_eq!(message.payloads, vec![payload.parse::<Element>().unwrap()]);
    }
}

#[test]
fn xep0045_continuation_keeps_empty_flag_and_thread_id() {
    let stanza: Element = "<message xmlns='jabber:client' from='coven@chat.example'>\
      <x xmlns='http://jabber.org/protocol/muc#user'>\
        <invite from='alice@example.com/desktop'><reason>Join us</reason><continue thread='t1'/></invite>\
        <invite from='coven@chat.example/nick'><continue/></invite>\
        <invite from='bob@example.net' to='me@example.com'>\
          <history-access xmlns='urn:waddle:group-dm:0'/>\
        </invite>\
        <password>room-secret</password>\
      </x></message>"
        .parse()
        .unwrap();
    let events = parse_muc_invitation_events(&stanza);
    let [MucInvitationEvent::Invited(threaded), MucInvitationEvent::Invited(continued), MucInvitationEvent::Invited(plain)] =
        events.as_slice()
    else {
        panic!("expected three mediated invites, got {events:?}");
    };
    assert_eq!(
        threaded.room,
        "coven@chat.example".parse::<BareJid>().unwrap()
    );
    assert_eq!(
        threaded.inviter,
        "alice@example.com/desktop".parse::<Jid>().unwrap()
    );
    assert_eq!(threaded.reason, Some(Reason("Join us".into())));
    assert_eq!(
        threaded.password,
        Some(MucInvitationPassword("room-secret".into()))
    );
    assert_eq!(
        threaded.continuation.as_ref().unwrap().thread.as_deref(),
        Some("t1")
    );
    assert_eq!(
        continued.inviter,
        "coven@chat.example/nick".parse::<Jid>().unwrap()
    );
    assert_eq!(continued.continuation.as_ref().unwrap().thread, None);
    assert_eq!(continued.reason, None);
    assert!(plain.continuation.is_none());
}

#[test]
fn xep0045_non_mediated_messages_do_not_produce_invitation_events() {
    for xml in [
        "<message xmlns='jabber:client' from='coven@chat.example/nick' type='chat'>\
            <body>Private message</body><x xmlns='http://jabber.org/protocol/muc#user'/></message>",
        "<message xmlns='jabber:client' from='coven@chat.example'>\
            <x xmlns='http://jabber.org/protocol/muc#user'><invite to='bob@example.com'/></x></message>",
        "<message xmlns='jabber:client' from='coven@chat.example'>\
            <x xmlns='http://jabber.org/protocol/muc#user'><decline from=''/></x></message>",
        "<message xmlns='jabber:client' from='coven@chat.example/nick'>\
            <x xmlns='http://jabber.org/protocol/muc#user'><invite from='bob@example.com'/></x></message>",
        "<message xmlns='jabber:client' from='coven@chat.example' type='error'>\
            <x xmlns='http://jabber.org/protocol/muc#user'><invite from='bob@example.com'/></x></message>",
        "<message xmlns='jabber:client'>\
            <x xmlns='http://jabber.org/protocol/muc#user'><invite from='bob@example.com'/></x></message>",
        "<message xmlns='jabber:client' from='coven@chat.example'>\
            <x xmlns='urn:example:unrelated'><invite from='bob@example.com'/></x></message>",
    ] {
        assert!(parse_muc_invitation_events(&xml.parse().unwrap()).is_empty(), "{xml}");
    }
}
