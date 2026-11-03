//! RFC 7395 stream hints and SASL1 OAUTHBEARER identity, over the real router.
#![cfg(feature = "test-support")]

use axum::serve::Listener;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use minidom::Element;
use std::future;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use waddle_server::test_support::{create_test_session, router, websocket_state, WebSocketState};
use waddle_xmpp::auth::{parse_oauthbearer, OAuthBearerResult};
use waddle_xmpp_client::{AccessToken, OAuthBearerRequest};

const TIMEOUT: Duration = Duration::from_secs(5);
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
const NS_STREAM: &str = "http://etherx.jabber.org/streams";
const NS_STREAM_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-streams";

type Peer = WebSocketStream<DuplexStream>;

struct OneConnection(Option<DuplexStream>);
impl Listener for OneConnection {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(io) => (io, ()),
            None => future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

async fn connect(state: Arc<WebSocketState>) -> (Peer, JoinHandle<()>) {
    let (server_io, peer_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        axum::serve(OneConnection(Some(server_io)), router(state))
            .await
            .expect("serve");
    });
    let (peer, response) = tokio_tungstenite::client_async("ws://localhost/ws", peer_io)
        .await
        .expect("websocket upgrade");
    assert_eq!(response.status(), 101);
    (peer, server)
}

fn open(from: Option<&str>) -> Element {
    let mut builder = Element::builder("open", NS_FRAMING)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), "example.com")
        .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0");
    if let Some(from) = from {
        builder = builder.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    builder.build()
}

fn oauth(data: &[u8]) -> Element {
    Element::builder("auth", NS_SASL)
        .attr(
            minidom::rxml::xml_ncname!("mechanism").to_owned(),
            "OAUTHBEARER",
        )
        .append(BASE64_STANDARD.encode(data))
        .build()
}

async fn send(peer: &mut Peer, element: Element) {
    peer.send(Message::Text(String::from(&element).into()))
        .await
        .expect("send frame");
}

async fn receive(peer: &mut Peer) -> Element {
    match timeout(TIMEOUT, peer.next()).await.expect("frame timeout") {
        Some(Ok(Message::Text(text))) => text.parse().expect("XML frame"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn start_stream(peer: &mut Peer, from: Option<&str>) -> Element {
    send(peer, open(from)).await;
    let response = receive(peer).await;
    assert_eq!(
        (response.name(), response.ns()),
        ("open", NS_FRAMING.to_owned())
    );
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
    response
}

fn bearer(token: &str, authzid: Option<&str>) -> Vec<u8> {
    let mut payload = match authzid {
        Some(value) => format!("n,a={value},").into_bytes(),
        None => b"n,,".to_vec(),
    };
    payload.push(1);
    payload.extend_from_slice(b"auth=Bearer ");
    payload.extend_from_slice(token.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    payload
}

#[tokio::test]
async fn open_replies_with_to_only_for_the_current_hint() {
    let state = websocket_state().await;
    let (mut peer, server) = connect(state).await;
    let first = start_stream(&mut peer, Some("Alice@EXAMPLE.com")).await;
    assert_eq!(first.attr("to"), Some("alice@example.com"));
    let second = start_stream(&mut peer, None).await;
    assert_eq!(second.attr("to"), None);
    let third = start_stream(&mut peer, Some("bob@example.com")).await;
    assert_eq!(third.attr("to"), Some("bob@example.com"));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn invalid_open_from_gets_separate_open_error_close_and_websocket_close() {
    let state = websocket_state().await;
    for invalid in [
        "",
        "example.com",
        "alice@example.com/device",
        "@example.com",
    ] {
        let (mut peer, server) = connect(Arc::clone(&state)).await;
        send(&mut peer, open(Some(invalid))).await;
        let server_open = receive(&mut peer).await;
        assert_eq!(
            (server_open.name(), server_open.ns()),
            ("open", NS_FRAMING.to_owned())
        );
        assert_eq!(server_open.attr("to"), None);
        let error = receive(&mut peer).await;
        assert_eq!((error.name(), error.ns()), ("error", NS_STREAM.to_owned()));
        assert!(error.get_child("invalid-from", NS_STREAM_ERRORS).is_some());
        let framing_close = receive(&mut peer).await;
        assert_eq!(
            (framing_close.name(), framing_close.ns()),
            ("close", NS_FRAMING.to_owned())
        );
        assert!(matches!(
            timeout(TIMEOUT, peer.next()).await.expect("close timeout"),
            Some(Ok(Message::Close(_)))
        ));
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn invalid_open_after_sasl_restart_gets_open_error_and_close_frames() {
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, Some("alice@example.com")).await;
    send(&mut peer, oauth(&bearer(&session.id, None))).await;
    assert_eq!(receive(&mut peer).await.name(), "success");

    send(&mut peer, open(Some("alice@example.com/device"))).await;
    let server_open = receive(&mut peer).await;
    assert_eq!(
        (server_open.name(), server_open.ns()),
        ("open", NS_FRAMING.to_owned())
    );
    assert_eq!(server_open.attr("to"), None);
    let error = receive(&mut peer).await;
    assert_eq!((error.name(), error.ns()), ("error", NS_STREAM.to_owned()));
    assert!(error.get_child("invalid-from", NS_STREAM_ERRORS).is_some());
    let close = receive(&mut peer).await;
    assert_eq!((close.name(), close.ns()), ("close", NS_FRAMING.to_owned()));
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close timeout"),
        Some(Ok(Message::Close(_)))
    ));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn post_auth_open_accepts_only_the_authenticated_principal() {
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    for from in [None, Some("alice@example.com")] {
        let (mut peer, server) = connect(Arc::clone(&state)).await;
        start_stream(&mut peer, None).await;
        send(&mut peer, oauth(&bearer(&session.id, None))).await;
        assert_eq!(receive(&mut peer).await.name(), "success");
        let reopened = start_stream(&mut peer, from).await;
        assert_eq!(reopened.attr("to"), from);
        server.abort();
        let _ = server.await;
    }

    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, None).await;
    send(&mut peer, oauth(&bearer(&session.id, None))).await;
    assert_eq!(receive(&mut peer).await.name(), "success");
    send(&mut peer, open(Some("bob@example.com"))).await;
    let response = receive(&mut peer).await;
    assert_eq!(
        (response.name(), response.ns()),
        ("open", NS_FRAMING.to_owned())
    );
    assert_eq!(response.attr("to"), Some("bob@example.com"));
    let error = receive(&mut peer).await;
    assert_eq!((error.name(), error.ns()), ("error", NS_STREAM.to_owned()));
    assert!(error.get_child("invalid-from", NS_STREAM_ERRORS).is_some());
    let close = receive(&mut peer).await;
    assert_eq!((close.name(), close.ns()), ("close", NS_FRAMING.to_owned()));
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close timeout"),
        Some(Ok(Message::Close(_)))
    ));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn oauth_accepts_y_flag_and_mixed_case_bearer_with_multiple_spaces() {
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, Some("alice@example.com")).await;
    let mut payload = b"y,,\x01auth=bEaReR   ".to_vec();
    payload.extend_from_slice(session.id.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    send(&mut peer, oauth(&payload)).await;
    assert_eq!(receive(&mut peer).await.name(), "success");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn oauth_checks_supplied_authzid_without_committing_failed_attempts() {
    use waddle_server::db::actor::DbExecute;
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "UPDATE sessions SET last_used_at = ? WHERE id = ?".to_owned(),
            params: vec![
                "2000-01-01T00:00:00+00:00".into(),
                session.id.clone().into(),
            ],
        })
        .await
        .expect("set sentinel timestamp");
    let sessions = &state.deps.auth_state.session_manager;
    let original = sessions
        .get_session(&session.id)
        .await
        .expect("load session")
        .expect("session exists")
        .last_used_at;
    let (mut peer, server) = connect(Arc::clone(&state)).await;
    start_stream(&mut peer, Some("alice@example.com")).await;
    send(
        &mut peer,
        oauth(&bearer(&session.id, Some("bob@example.com"))),
    )
    .await;
    let failed = receive(&mut peer).await;
    assert_eq!(
        (failed.name(), failed.ns()),
        ("failure", NS_SASL.to_owned())
    );
    assert!(failed.get_child("invalid-authzid", NS_SASL).is_some());
    assert_eq!(
        sessions
            .get_session(&session.id)
            .await
            .expect("load session")
            .expect("session exists")
            .last_used_at,
        original
    );
    send(
        &mut peer,
        oauth(&bearer(&session.id, Some("alice@example.com"))),
    )
    .await;
    let success = receive(&mut peer).await;
    assert_eq!(
        (success.name(), success.ns()),
        ("success", NS_SASL.to_owned())
    );
    assert_ne!(
        sessions
            .get_session(&session.id)
            .await
            .expect("load session")
            .expect("session exists")
            .last_used_at,
        original
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn oauth_accepts_absent_authzid_but_rejects_wrong_stream_hint() {
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(Arc::clone(&state)).await;
    start_stream(&mut peer, None).await;
    send(&mut peer, oauth(&bearer(&session.id, None))).await;
    assert_eq!(receive(&mut peer).await.name(), "success");
    server.abort();
    let _ = server.await;

    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, Some("alice@example.com")).await;
    start_stream(&mut peer, Some("bob@example.com")).await;
    send(&mut peer, oauth(&bearer(&session.id, None))).await;
    let failed = receive(&mut peer).await;
    assert!(failed.get_child("not-authorized", NS_SASL).is_some());
    assert_eq!(receive(&mut peer).await.name(), "close");
    assert!(matches!(
        timeout(TIMEOUT, peer.next()).await.expect("close timeout"),
        Some(Ok(Message::Close(_)))
    ));
    server.abort();
    let _ = server.await;
}

#[test]
fn client_gs2_encoding_round_trips_a_typed_authzid() {
    let account = "al,ice=test@example.com".parse().expect("bare JID");
    let request = OAuthBearerRequest {
        account,
        access_token: AccessToken::new("token"),
    };
    let wire = request.to_element();
    let payload = BASE64_STANDARD.decode(wire.text()).expect("base64");
    assert!(payload.starts_with(b"n,a=al=2Cice=3Dtest@example.com,"));
    let OAuthBearerResult::Credentials(parsed) = parse_oauthbearer(&payload).expect("GS2") else {
        panic!("expected credentials");
    };
    assert_eq!(
        parsed.authzid.expect("authzid").to_string(),
        "al,ice=test@example.com"
    );
}

#[tokio::test]
async fn malformed_authzid_and_invalid_token_use_expected_sasl_failures() {
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, None).await;
    send(
        &mut peer,
        oauth(&bearer(&session.id, Some("alice=ZZ@example.com"))),
    )
    .await;
    assert!(receive(&mut peer)
        .await
        .get_child("invalid-authzid", NS_SASL)
        .is_some());
    send(
        &mut peer,
        oauth(&bearer("invalid-token", Some("bob@example.com"))),
    )
    .await;
    assert!(receive(&mut peer)
        .await
        .get_child("not-authorized", NS_SASL)
        .is_some());
    send(&mut peer, oauth(&bearer(&session.id, None))).await;
    assert_eq!(receive(&mut peer).await.name(), "success");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn oauth_rejects_inconsistent_session_jid_and_localpart() {
    use waddle_server::db::actor::DbExecute;
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "UPDATE users SET xmpp_localpart = ? WHERE jid = ?".to_owned(),
            params: vec!["bob".into(), session.user_jid.clone().into()],
        })
        .await
        .expect("update localpart");
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, None).await;
    send(&mut peer, oauth(&bearer(&session.id, None))).await;
    let failed = receive(&mut peer).await;
    assert!(failed.get_child("not-authorized", NS_SASL).is_some());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn scram_proof_is_checked_against_the_current_stream_hint() {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};
    use waddle_server::auth::{NativeUserStore, RegisterRequest};
    use waddle_server::test_support::seed_local_account;

    let state = websocket_state().await;
    seed_local_account(&state, "alice").await;
    NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone())
        .register(RegisterRequest {
            username: "alice".to_owned(),
            domain: "example.com".to_owned(),
            password: "test-password".to_owned(),
            email: None,
        })
        .await
        .expect("register SCRAM account");

    for (from, should_succeed, prefixed, malformed) in [
        ("alice@example.com", true, false, false),
        ("bob@example.com", false, false, false),
        ("alice@example.com", true, true, false),
        ("alice@example.com", false, true, true),
    ] {
        let (mut peer, server) = connect(Arc::clone(&state)).await;
        start_stream(&mut peer, Some(from)).await;
        let bare = "n=alice,r=clientnonce";
        let initial = format!("n,,{bare}");
        let mut auth = Element::builder("auth", NS_SASL);
        if prefixed {
            auth = auth
                .prefix(Some("s".to_owned()), NS_SASL)
                .expect("unique prefix");
        }
        let auth = auth
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(BASE64_STANDARD.encode(initial))
            .build();
        if prefixed {
            assert!(String::from(&auth).starts_with("<s:auth"));
        }
        send(&mut peer, auth.clone()).await;
        let challenge = receive(&mut peer).await;
        assert_eq!(challenge.name(), "challenge");
        let server_first = String::from_utf8(
            BASE64_STANDARD
                .decode(challenge.text())
                .expect("challenge base64"),
        )
        .expect("challenge utf8");
        let fields: std::collections::HashMap<_, _> = server_first
            .split(',')
            .filter_map(|part| part.split_once('='))
            .collect();
        let nonce = fields.get("r").expect("nonce");
        let salt = BASE64_STANDARD
            .decode(fields.get("s").expect("salt"))
            .expect("salt base64");
        let iterations: u32 = fields.get("i").expect("iterations").parse().expect("count");
        let salted = pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(b"test-password", &salt, iterations);
        let mut client_key_mac = Hmac::<Sha256>::new_from_slice(&salted).expect("key");
        client_key_mac.update(b"Client Key");
        let client_key = client_key_mac.finalize().into_bytes();
        let stored_key = Sha256::digest(client_key);
        let final_without_proof = format!("c=biws,r={nonce}");
        let auth_message = format!("{bare},{server_first},{final_without_proof}");
        let mut signature_mac = Hmac::<Sha256>::new_from_slice(&stored_key).expect("key");
        signature_mac.update(auth_message.as_bytes());
        let signature = signature_mac.finalize().into_bytes();
        let proof: Vec<u8> = client_key
            .iter()
            .zip(signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let final_message = format!("{final_without_proof},p={}", BASE64_STANDARD.encode(proof));
        let mut response = Element::builder("response", NS_SASL);
        if prefixed {
            response = response
                .prefix(Some("s".to_owned()), NS_SASL)
                .expect("unique prefix");
        }
        let response = response.append(BASE64_STANDARD.encode(final_message));
        let response = if malformed {
            response.append(Element::builder("unexpected", NS_SASL).build())
        } else {
            response
        }
        .build();
        if prefixed {
            assert!(String::from(&response).starts_with("<s:response"));
        }
        if prefixed && !malformed {
            let wrong_namespace = Element::builder("response", "urn:wrong")
                .prefix(Some("s".to_owned()), "urn:wrong")
                .expect("unique prefix")
                .append("eA==")
                .build();
            send(&mut peer, wrong_namespace).await;
        }
        send(&mut peer, response).await;
        let result = receive(&mut peer).await;
        if malformed {
            assert!(result.get_child("malformed-request", NS_SASL).is_some());
            send(&mut peer, auth).await;
            assert_eq!(receive(&mut peer).await.name(), "challenge");
        } else if should_succeed {
            assert_eq!(result.name(), "success");
        } else {
            assert!(result.get_child("not-authorized", NS_SASL).is_some());
            assert_eq!(receive(&mut peer).await.name(), "close");
            assert!(matches!(
                timeout(TIMEOUT, peer.next()).await.expect("close timeout"),
                Some(Ok(Message::Close(_)))
            ));
        }
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn malformed_xml_and_wrong_namespace_send_open_error_close_as_separate_frames() {
    let state = websocket_state().await;
    for (wire, condition) in [
        (
            "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='alice",
            "not-well-formed",
        ),
        ("<open xmlns='jabber:client'/>", "invalid-namespace"),
        (
            "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'>text</open>",
            "invalid-xml",
        ),
        (
            "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'><child/></open>",
            "invalid-xml",
        ),
    ] {
        let (mut peer, server) = connect(Arc::clone(&state)).await;
        peer.send(Message::Text(wire.into())).await.expect("send");
        let server_open = receive(&mut peer).await;
        assert_eq!(
            (server_open.name(), server_open.ns()),
            ("open", NS_FRAMING.to_owned())
        );
        let error = receive(&mut peer).await;
        assert_eq!((error.name(), error.ns()), ("error", NS_STREAM.to_owned()));
        assert!(error.get_child(condition, NS_STREAM_ERRORS).is_some());
        let close = receive(&mut peer).await;
        assert_eq!((close.name(), close.ns()), ("close", NS_FRAMING.to_owned()));
        assert!(matches!(
            timeout(TIMEOUT, peer.next()).await.expect("close timeout"),
            Some(Ok(Message::Close(_)))
        ));
        server.abort();
        let _ = server.await;
    }
}
