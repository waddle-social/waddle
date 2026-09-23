//! XEP-0199 client FFI regression tests.

use std::future::pending;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use minidom::Element;
use waddle_xmpp_client::{ClientError, StanzaError, StanzaErrorType};
use xmpp_parsers::{iq::Iq, ns};

use crate::error::client_error_to_waddle;
use crate::ping::{build_ping_iq, send_ping};
use crate::{WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener};

#[derive(Clone, Default)]
struct RecordingListener {
    events: Arc<Mutex<Vec<WaddleClientEvent>>>,
}

impl RecordingListener {
    fn events(&self) -> Vec<WaddleClientEvent> {
        self.events
            .lock()
            .expect("test event mutex poisoned")
            .clone()
    }
}

impl WaddleEventListener for RecordingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        self.events
            .lock()
            .expect("test event mutex poisoned")
            .push(event);
    }
}

fn test_client(listener: RecordingListener) -> Arc<WaddleClient> {
    WaddleClient::new(
        WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_owned(),
            jid: "alice@waddle.test".to_owned(),
            access_token: "token".to_owned(),
            resource: "test".to_owned(),
        },
        Box::new(listener),
    )
}

fn empty_iq_result() -> Element {
    Iq::Result {
        from: None,
        to: None,
        id: "ping-test".to_owned(),
        payload: None,
    }
    .into()
}

fn stanza_error() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "service-unavailable".to_owned(),
        text: None,
        application_condition: None,
    })
}

#[test]
fn xep0199_ping_iq_is_a_get_with_empty_ping_payload_and_unique_id() {
    let first = build_ping_iq();
    let second = build_ping_iq();

    assert_eq!(first.name(), "iq");
    assert_eq!(first.attr("type"), Some("get"));
    let first_id = first.attr("id").expect("ping IQ has an id");
    let second_id = second.attr("id").expect("second ping IQ has an id");
    assert!(!first_id.is_empty());
    assert!(!second_id.is_empty());
    assert_ne!(first_id, second_id);

    let ping = first
        .get_child("ping", ns::PING)
        .expect("XEP-0199 ping payload uses the standard namespace");
    assert_eq!(ping.children().count(), 0);
}

#[tokio::test]
async fn xep0199_success_iq_response_is_liveness() {
    let result = send_ping(
        |iq| async move {
            assert_eq!(iq.name(), "iq");
            Ok(empty_iq_result())
        },
        Duration::from_secs(1),
    )
    .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn xep0199_stanza_error_response_is_liveness() {
    let result = send_ping(|_| async { Err(stanza_error()) }, Duration::from_secs(1)).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn xep0199_disconnect_and_transport_failures_keep_typed_mappings() {
    let disconnected = send_ping(
        |_| async { Err::<Element, _>(ClientError::Disconnected) },
        Duration::from_secs(1),
    )
    .await
    .expect_err("a disconnected session cannot answer a ping");
    assert_eq!(
        client_error_to_waddle(&disconnected),
        WaddleError::NotConnected
    );

    let transport = send_ping(
        |_| async { Err::<Element, _>(ClientError::TransportClosed) },
        Duration::from_secs(1),
    )
    .await
    .expect_err("a closed transport cannot answer a ping");
    assert_eq!(client_error_to_waddle(&transport), WaddleError::Transport);
}

#[tokio::test]
async fn xep0199_timeout_maps_to_typed_timeout() {
    let timeout = Duration::from_millis(1);
    let error = send_ping(|_| pending::<Result<Element, ClientError>>(), timeout)
        .await
        .expect_err("a ping without a reply times out");

    assert!(matches!(error, ClientError::IqTimeout { timeout: value } if value == timeout));
    assert_eq!(client_error_to_waddle(&error), WaddleError::Timeout);
}

#[tokio::test]
async fn ping_server_reports_not_connected_when_handle_is_absent() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());

    let result = client.ping_server().await;

    assert_eq!(result, Err(WaddleError::NotConnected));
    assert!(matches!(
        listener.events().as_slice(),
        [WaddleClientEvent::Error { description }] if description == "Not connected"
    ));
}
