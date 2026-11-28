use minidom::Element;

use super::config;
use crate::muc_invites::MucInvitationEvent;
use crate::{ClientEvent, XmppRuntime};

#[test]
fn mediated_invite_and_decline_are_not_chat_messages() {
    let mut runtime = XmppRuntime::try_new(config()).unwrap();
    let [invite, decline] = [
        "<message xmlns='jabber:client' from='coven@chat.example'>\
            <body>Fallback invitation text</body>\
            <x xmlns='http://jabber.org/protocol/muc#user'>\
                <invite from='alice@example.com'/></x></message>",
        "<message xmlns='jabber:client' type='normal' from='coven@chat.example'>\
            <x xmlns='http://jabber.org/protocol/muc#user'>\
                <decline from='bob@example.com'><reason>Busy</reason></decline></x></message>",
    ]
    .map(|xml| runtime.handle_app_stanza(&xml.parse::<Element>().unwrap()));
    assert!(matches!(
        invite.as_slice(),
        [ClientEvent::MucInvitation(MucInvitationEvent::Invited(_))]
    ));
    assert!(
        matches!(decline.as_slice(), [ClientEvent::MucInvitation(MucInvitationEvent::Declined(decline))]
        if decline.room.to_string() == "coven@chat.example"
            && decline.invitee.to_string() == "bob@example.com"
            && decline.reason.as_ref().is_some_and(|reason| reason.0 == "Busy"))
    );
}

#[test]
fn received_carbon_dispatches_the_invite_without_a_chat_message() {
    let mut runtime = XmppRuntime::try_new(config()).unwrap();
    let stanza =
        "<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/macbook'>\
      <received xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'>\
        <message xmlns='jabber:client' from='coven@chat.example' to='alice@example.com/phone'>\
          <x xmlns='http://jabber.org/protocol/muc#user'><invite from='bob@example.com'/></x>\
        </message></forwarded></received></message>"
            .parse()
            .unwrap();
    assert!(matches!(
        runtime.handle_app_stanza(&stanza).as_slice(),
        [ClientEvent::MucInvitation(MucInvitationEvent::Invited(_))]
    ));
}
