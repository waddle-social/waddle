//! Legacy Session routing over actual WebSockets and SCRAM-bound resources.
//! This does not add Session negotiation or change the local no-op behavior.

use minidom::Element;
use waddle_ws_test_support::iq::{bound_jid as full_jid, receive_iq, round_trip, send_iq};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::ns::SESSION;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";

fn request(target: Option<&str>, set: bool, id: &str) -> Iq {
    let to = target.map(|target| target.parse().expect("Session target"));
    let payload = Element::builder("session", SESSION).build();
    if set {
        Iq::Set {
            from: None,
            to,
            id: id.into(),
            payload,
        }
    } else {
        Iq::Get {
            from: None,
            to,
            id: id.into(),
            payload,
        }
    }
}

#[tokio::test]
async fn foreign_session_denials_preserve_local_noop_and_real_peer_forwarding() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "session-fixture-password")]);
    let mut origin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "session-origin",
    )
    .await
    .expect("origin SCRAM/bind");
    let bound = full_jid(&origin);
    for set in [false, true] {
        for target in [
            "remote.test",
            "bob@remote.test/phone",
            "room@muc.remote.test/nick",
        ] {
            let mut iq = request(Some(target), set, "foreign-session<&");
            *iq.from_mut() = Some(
                "forged@elsewhere.test/resource"
                    .parse()
                    .expect("forged from"),
            );
            let reply = round_trip(&mut origin, iq.clone()).await;
            assert_eq!(reply.id(), iq.id());
            assert_eq!(reply.from(), iq.to());
            assert_eq!(reply.to(), Some(&bound));
            let Iq::Error { error, payload, .. } = reply else {
                panic!("foreign Session cannot return local success")
            };
            assert_eq!(error.type_, ErrorType::Cancel);
            assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
            assert_eq!(error.by, Some(DOMAIN.parse().expect("home domain")));
            assert!(error
                .texts
                .values()
                .any(|text| text == "Remote Session routing is not allowed by this server."));
            let original = match iq {
                Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
                _ => unreachable!(),
            };
            assert_eq!(payload, Some(original));
        }
        for target in [None, Some(DOMAIN)] {
            let iq = request(target, set, "local-session");
            let reply = round_trip(&mut origin, iq.clone()).await;
            assert!(matches!(reply, Iq::Result { payload: None, .. }));
            assert_eq!(reply.id(), iq.id());
            assert_eq!(reply.from(), iq.to());
        }
    }

    let mut peer = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "peer",
        "session-fixture-password",
        "session-peer",
    )
    .await
    .expect("peer SCRAM/bind");
    for set in [false, true] {
        let mut iq = request(Some(full_jid(&peer).as_str()), set, "peer-session");
        *iq.from_mut() = Some(
            "forged@elsewhere.test/resource"
                .parse()
                .expect("forged from"),
        );
        send_iq(&mut origin, iq.clone()).await;
        let forwarded = receive_iq(&mut peer, iq.id()).await;
        *iq.from_mut() = Some(bound.clone());
        assert_eq!(forwarded, iq);
    }
    // A later local request still succeeds after foreign errors and peer routing.
    assert!(matches!(
        round_trip(&mut origin, request(None, true, "local-after-peer")).await,
        Iq::Result { payload: None, .. }
    ));
    origin.close().await.expect("close origin");
    peer.close().await.expect("close peer");
}
