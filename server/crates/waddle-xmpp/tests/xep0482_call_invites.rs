#![recursion_limit = "256"]

//! XEP-0482: Call Invites integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};
use minidom::Element;
use waddle_xmpp::xep::{extract_call_action, CallAction, CallJoinMethod};
use xmpp_parsers::message::Message;

#[tokio::test]
async fn xep0482_call_invite_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "calls@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "calls@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends a call invite
    alice
        .send(
            "<message type='groupchat' to='calls@muc.localhost' id='call-1' xmlns='jabber:client'>\
                <invite xmlns='urn:xmpp:call-invites:0'>\
                    <external uri='https://meet.example.com/room'/>\
                </invite>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("call-invites", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("urn:xmpp:call-invites:0"),
        "Bob should receive call invite namespace, got: {}",
        bob_response
    );
}

#[test]
fn xep0482_invite_supports_multi_method_semantics() {
    let xml = "<message xmlns='jabber:client'>\
                 <invite xmlns='urn:xmpp:call-invites:0'>\
                   <muji/>\
                   <jingle sid='sid-1' jid='alice@localhost/desktop'/>\
                   <external uri='https://meet.example.com/room'/>\
                 </invite>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    match extract_call_action(&msg).expect("call action") {
        CallAction::Invite(invite) => {
            assert_eq!(invite.methods.len(), 3);
            assert!(invite.methods.contains(&CallJoinMethod::Muji));
            assert!(invite.methods.iter().any(|method| {
                matches!(
                    method,
                    CallJoinMethod::Jingle { sid, jid }
                    if sid == "sid-1" && jid.as_deref() == Some("alice@localhost/desktop")
                )
            }));
            assert!(invite.methods.iter().any(|method| {
                matches!(
                    method,
                    CallJoinMethod::External { uri } if uri == "https://meet.example.com/room"
                )
            }));
        }
        _ => panic!("expected invite action"),
    }
}

#[test]
fn xep0482_invite_without_join_method_is_rejected() {
    let xml = "<message xmlns='jabber:client'>\
                 <invite xmlns='urn:xmpp:call-invites:0'/>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(extract_call_action(&msg).is_none());
}

#[test]
fn xep0482_accept_requires_invite_id_and_valid_method() {
    let missing_id = "<message xmlns='jabber:client'>\
                        <accept xmlns='urn:xmpp:call-invites:0'>\
                          <external uri='https://meet.example.com/room'/>\
                        </accept>\
                      </message>";
    let missing_method = "<message xmlns='jabber:client'>\
                            <accept xmlns='urn:xmpp:call-invites:0' id='call-1'/>\
                          </message>";

    let msg_missing_id = Message::try_from(missing_id.parse::<Element>().expect("valid xml"))
        .expect("valid message");
    let msg_missing_method =
        Message::try_from(missing_method.parse::<Element>().expect("valid xml"))
            .expect("valid message");

    assert!(extract_call_action(&msg_missing_id).is_none());
    assert!(extract_call_action(&msg_missing_method).is_none());
}
