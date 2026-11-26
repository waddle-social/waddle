//! XEP-0494 Client Access Management over the real router: an account owner
//! lists and revokes the OAuth grants (XEP-0493) that give clients access.
#![cfg(feature = "test-support")]

use futures::SinkExt;
use minidom::Element;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;
use waddle_server::auth::oauth_grants::OAuthGrantStore;
use waddle_server::auth::Session;
use waddle_server::test_support::{create_test_session, websocket_state, WebSocketState};
use waddle_xmpp::xep::xep0493::OAuthScope;
use waddle_xmpp::xep::xep0494::NS_CAM;

mod oauth_ws_support;
use oauth_ws_support::{bearer, connect, oauth, receive, send, start_stream, Peer};

const NS_CLIENT: &str = "jabber:client";

async fn send_xml(peer: &mut Peer, xml: &str) {
    peer.send(Message::Text(xml.to_owned().into()))
        .await
        .expect("send frame");
}

/// Authenticate with `token`, restart the stream and bind `resource`.
async fn authenticate_and_bind_as(peer: &mut Peer, token: &str, resource: &str) {
    start_stream(peer, None).await;
    send(peer, oauth(&bearer(token, None))).await;
    assert_eq!(receive(peer).await.name(), "success");
    start_stream(peer, None).await;
    send_xml(
        peer,
        &format!("<iq xmlns='jabber:client' type='set' id='bind'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>{resource}</resource></bind></iq>"),
    )
    .await;
    let bound = receive(peer).await;
    assert_eq!(bound.attr("type"), Some("result"), "{bound:?}");
}

async fn authenticate_and_bind(peer: &mut Peer, token: &str) {
    authenticate_and_bind_as(peer, token, "web").await;
}

async fn cam_iq(peer: &mut Peer, id: &str, kind: &str, payload: &str) -> Element {
    send_xml(
        peer,
        &format!("<iq xmlns='jabber:client' type='{kind}' id='{id}'>{payload}</iq>"),
    )
    .await;
    loop {
        let frame = receive(peer).await;
        if frame.attr("id") == Some(id) {
            return frame;
        }
    }
}

async fn list(peer: &mut Peer, id: &str) -> Element {
    cam_iq(peer, id, "get", &format!("<list xmlns='{NS_CAM}'/>")).await
}

/// A credential for `username` as a completed authorization-code exchange
/// issues it: a session of its own plus the grant record.
async fn grant(state: &WebSocketState, username: &str) -> (Session, String) {
    let session = create_test_session(state, username).await;
    let grants = OAuthGrantStore::new(&state.deps.auth_state.session_manager);
    let id = grants
        .create(&session.id, &session.user_jid, &[OAuthScope::ClientNormal])
        .await
        .expect("grant");
    (session, id)
}

#[tokio::test]
async fn owner_lists_and_revokes_a_grant_and_the_credential_stops_working() {
    let state = websocket_state().await;
    // The owner's own login, and a third-party client's grant for the same
    // account: separate credentials.
    let owner = create_test_session(&state, "alice").await;
    let (client_session, grant_id) = grant(&state, "alice").await;
    assert_ne!(owner.id, client_session.id);

    let (mut peer, server) = connect(Arc::clone(&state)).await;
    authenticate_and_bind(&mut peer, &owner.id).await;

    let listed = list(&mut peer, "list-1").await;
    assert_eq!(listed.attr("type"), Some("result"));
    let clients = listed.get_child("clients", NS_CAM).expect("clients");
    let entries: Vec<_> = clients.children().collect();
    assert_eq!(entries.len(), 1, "the owner's own login is not a grant");
    let client = entries[0];
    assert_eq!(client.attr("id"), Some(grant_id.as_str()));
    assert_eq!(client.attr("connected"), Some("false"));
    assert_eq!(client.attr("type"), Some("access"));
    assert!(client
        .get_child("auth", NS_CAM)
        .and_then(|auth| auth.get_child("grant", NS_CAM))
        .is_some());
    assert_eq!(
        client
            .get_child("permission", NS_CAM)
            .and_then(|permission| permission.attr("status")),
        Some("normal")
    );
    assert!(client.get_child("first-seen", NS_CAM).is_some());
    assert!(client.get_child("last-seen", NS_CAM).is_some());
    // The listed id is not the bearer credential.
    assert!(!String::from(client).contains(&client_session.id));

    // The credential works until revoked.
    let (mut third_party, third_party_server) = connect(Arc::clone(&state)).await;
    authenticate_and_bind_as(&mut third_party, &client_session.id, "app").await;
    let listed = list(&mut peer, "list-2").await;
    assert_eq!(
        listed
            .get_child("clients", NS_CAM)
            .and_then(|clients| clients.children().next())
            .and_then(|client| client.attr("type")),
        Some("session"),
        "a grant that has authenticated is a session"
    );
    third_party_server.abort();
    let _ = third_party_server.await;

    let revoked = cam_iq(
        &mut peer,
        "revoke-1",
        "set",
        &format!("<revoke xmlns='{NS_CAM}' id='{grant_id}'/>"),
    )
    .await;
    assert_eq!(revoked.attr("type"), Some("result"), "{revoked:?}");
    assert_eq!(revoked.children().count(), 0);

    let listed = list(&mut peer, "list-3").await;
    assert_eq!(
        listed
            .get_child("clients", NS_CAM)
            .map(|clients| clients.children().count()),
        Some(0)
    );

    // The revoked credential no longer authenticates.
    let (mut revoked_peer, revoked_server) = connect(Arc::clone(&state)).await;
    start_stream(&mut revoked_peer, None).await;
    send(&mut revoked_peer, oauth(&bearer(&client_session.id, None))).await;
    assert_eq!(receive(&mut revoked_peer).await.name(), "challenge");
    revoked_server.abort();
    let _ = revoked_server.await;

    // Revoking again, or an id that was never issued, finds nothing.
    for id in [grant_id.as_str(), "unknown"] {
        let missing = cam_iq(
            &mut peer,
            "revoke-missing",
            "set",
            &format!("<revoke xmlns='{NS_CAM}' id='{id}'/>"),
        )
        .await;
        assert_eq!(missing.attr("type"), Some("error"));
        assert!(missing
            .get_child("error", NS_CLIENT)
            .and_then(
                |error| error.get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
            )
            .is_some());
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn an_account_cannot_see_or_revoke_another_accounts_grants() {
    let state = websocket_state().await;
    let owner = create_test_session(&state, "alice").await;
    let (_, bobs_grant) = grant(&state, "bob").await;
    let _ = create_test_session(&state, "bob").await;

    let (mut peer, server) = connect(Arc::clone(&state)).await;
    authenticate_and_bind(&mut peer, &owner.id).await;

    let listed = list(&mut peer, "list").await;
    assert_eq!(
        listed
            .get_child("clients", NS_CAM)
            .map(|clients| clients.children().count()),
        Some(0)
    );
    let revoke = cam_iq(
        &mut peer,
        "revoke",
        "set",
        &format!("<revoke xmlns='{NS_CAM}' id='{bobs_grant}'/>"),
    )
    .await;
    assert_eq!(revoke.attr("type"), Some("error"));
    let grants = OAuthGrantStore::new(&state.deps.auth_state.session_manager);
    assert_eq!(
        grants.list("bob@example.com").await.expect("list").len(),
        1,
        "the grant survived"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn a_client_normal_credential_cannot_manage_account_access() {
    let state = websocket_state().await;
    let (client_session, grant_id) = grant(&state, "alice").await;

    let (mut peer, server) = connect(Arc::clone(&state)).await;
    authenticate_and_bind(&mut peer, &client_session.id).await;
    for (kind, payload) in [
        ("get", format!("<list xmlns='{NS_CAM}'/>")),
        ("set", format!("<revoke xmlns='{NS_CAM}' id='{grant_id}'/>")),
    ] {
        let response = cam_iq(&mut peer, "cam", kind, &payload).await;
        assert_eq!(response.attr("type"), Some("error"), "{kind}");
        assert!(response
            .get_child("error", NS_CLIENT)
            .and_then(|error| error.get_child("forbidden", "urn:ietf:params:xml:ns:xmpp-stanzas"))
            .is_some());
    }
    let grants = OAuthGrantStore::new(&state.deps.auth_state.session_manager);
    assert_eq!(
        grants.list("alice@example.com").await.expect("list").len(),
        1
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn malformed_requests_are_bad_requests() {
    let state = websocket_state().await;
    let owner = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    authenticate_and_bind(&mut peer, &owner.id).await;
    for (kind, payload) in [
        ("set", format!("<revoke xmlns='{NS_CAM}'/>")),
        ("set", format!("<list xmlns='{NS_CAM}'/>")),
        ("get", format!("<revoke xmlns='{NS_CAM}' id='x'/>")),
    ] {
        let response = cam_iq(&mut peer, "bad", kind, &payload).await;
        assert_eq!(response.attr("type"), Some("error"), "{payload}");
        assert!(response
            .get_child("error", NS_CLIENT)
            .and_then(|error| error.get_child("bad-request", "urn:ietf:params:xml:ns:xmpp-stanzas"))
            .is_some());
    }
    server.abort();
    let _ = server.await;
}
