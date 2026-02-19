//! XEP-0215: External Service Discovery dedicated suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, extdisco_services_query,
    extdisco_services_set_query, init_test_env, RawXmppClient, TestServer,
};

#[tokio::test]
async fn xep0215_services_query_returns_stun_and_turn() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0215", "client")
        .await
        .expect("bind session");

    let response = extdisco_services_query(&mut client, "localhost", "xep0215-get")
        .await
        .expect("extdisco response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected IQ result for extdisco query, got: {}",
        response
    );
    assert!(
        response.contains("<services") && response.contains("urn:xmpp:extdisco:2"),
        "Expected extdisco services payload, got: {}",
        response
    );
    assert!(
        response.contains("type='stun'") || response.contains("type=\"stun\""),
        "Expected STUN service in extdisco response, got: {}",
        response
    );
    assert!(
        response.contains("type='turn'") || response.contains("type=\"turn\""),
        "Expected TURN service in extdisco response, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0215_invalid_services_set_request_returns_error() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0215neg", "client")
        .await
        .expect("bind session");

    let response = extdisco_services_set_query(&mut client, "localhost", "xep0215-set")
        .await
        .expect("extdisco error response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected IQ error for extdisco set request, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable for extdisco set request, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0215_feature_advertisement_matches_handler_availability() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0215consistency", "client")
        .await
        .expect("bind session");

    let disco = disco_info_query(&mut client, "localhost", "xep0215-disco")
        .await
        .expect("disco#info response");
    let extdisco = extdisco_services_query(&mut client, "localhost", "xep0215-services")
        .await
        .expect("extdisco response");

    let feature_advertised = disco.contains("var='urn:xmpp:extdisco:2'")
        || disco.contains("var=\"urn:xmpp:extdisco:2\"");
    let handler_responded = extdisco.contains("<services")
        && (extdisco.contains("type='result'") || extdisco.contains("type=\"result\""));

    assert!(
        feature_advertised && handler_responded,
        "Expected extdisco feature advertisement and working handler, disco: {}, extdisco: {}",
        disco,
        extdisco
    );
}
