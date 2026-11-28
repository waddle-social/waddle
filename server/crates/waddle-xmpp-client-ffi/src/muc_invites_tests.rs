use std::sync::Mutex;

use waddle_xmpp_client::muc_invites::parse_muc_invitation_events;
use waddle_xmpp_client::ClientEvent;

use crate::convert::dispatch_event;
use crate::test_support::offline_client;
use crate::{WaddleClientEvent, WaddleError, WaddleEventListener, WaddleMucInvitationEvent};

#[tokio::test]
async fn xep0045_verbs_validate_jids_before_requiring_a_connection() {
    let client = offline_client();
    assert_eq!(
        client
            .invite_to_room(
                "coven@chat.example/nick".into(),
                "alice@example.com".into(),
                None
            )
            .await,
        Err(WaddleError::InvalidJid)
    );
    assert_eq!(
        client
            .decline_room_invitation("coven@chat.example".into(), String::new(), None)
            .await,
        Err(WaddleError::InvalidJid)
    );
    assert_eq!(
        client
            .invite_to_room(
                "coven@chat.example".into(),
                "alice@example.com".into(),
                None
            )
            .await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client
            .decline_room_invitation(
                "coven@chat.example".into(),
                "alice@example.com/phone".into(),
                None
            )
            .await,
        Err(WaddleError::NotConnected)
    );
}

#[derive(Default)]
struct Listener(Mutex<Vec<WaddleClientEvent>>);

impl WaddleEventListener for Listener {
    fn on_event(&self, event: WaddleClientEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[test]
fn xep0045_invite_and_decline_reach_the_ffi_listener() {
    let listener = Listener::default();
    for xml in [
        "<message xmlns='jabber:client' from='coven@chat.example'>\
            <x xmlns='http://jabber.org/protocol/muc#user'>\
                <invite from='alice@example.com/phone'><reason>Join us</reason>\
                    <continue thread='t1'/></invite><password>magic</password>\
            </x></message>",
        "<message xmlns='jabber:client' from='coven@chat.example'>\
            <x xmlns='http://jabber.org/protocol/muc#user'>\
                <decline from='bob@example.com'><reason>Busy</reason></decline>\
            </x></message>",
    ] {
        for event in parse_muc_invitation_events(&xml.parse().unwrap()) {
            dispatch_event(
                ClientEvent::MucInvitation(event),
                "user@example.com",
                &listener,
            );
        }
    }
    let events = listener.0.into_inner().unwrap();
    let [WaddleClientEvent::MucInvitation {
        event: WaddleMucInvitationEvent::Invited { invitation },
    }, WaddleClientEvent::MucInvitation {
        event: WaddleMucInvitationEvent::Declined { decline },
    }] = events.as_slice()
    else {
        panic!("expected an invite followed by a decline");
    };
    assert_eq!(invitation.room.to_string(), "coven@chat.example");
    assert_eq!(invitation.inviter.to_string(), "alice@example.com/phone");
    assert_eq!(invitation.reason.as_deref(), Some("Join us"));
    assert_eq!(invitation.password.as_deref(), Some("magic"));
    assert_eq!(
        invitation.continuation.as_ref().unwrap().thread.as_deref(),
        Some("t1")
    );
    assert_eq!(decline.room.to_string(), "coven@chat.example");
    assert_eq!(decline.invitee.to_string(), "bob@example.com");
    assert_eq!(decline.reason.as_deref(), Some("Busy"));
}
