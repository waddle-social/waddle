use super::*;
use crate::bootstrap::NS_CLIENT;
use crate::ClientError;
use std::str::FromStr;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::ConnectionConfig;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use futures::{SinkExt, StreamExt};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::net::TcpListener;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::accept_hdr_async;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::tungstenite::Message;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use url::Url;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn config() -> ClientConfig {
    ClientConfig::new(
        ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
        WebSocketConfig::new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap(),
        OAuthBearerConfig::new(
            BareJid::from_str("alice@example.com").unwrap(),
            ClientResource::new("macbook").unwrap(),
            AccessToken::new("token"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback error type as a large ErrorResponse"
)]
fn assert_xmpp_subprotocol(
    request: &Request,
    mut response: Response,
) -> Result<Response, ErrorResponse> {
    assert_eq!(
        request
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok()),
        Some("xmpp")
    );
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "xmpp".parse().unwrap());
    Ok(response)
}

#[test]
fn transport_event_maps_close_messages_to_closing() {
    assert_eq!(
        TransportEvent::MessageReceived(TransportMessage::Close(StreamClose)).transport_state(),
        TransportState::Closing
    );
    assert_eq!(
        TransportEvent::MessageSent(TransportMessage::Close(StreamClose)).transport_state(),
        TransportState::Closing
    );
}

#[test]
fn open_frames_round_trip_through_rfc7395_codec() {
    let message = TransportMessage::Open(StreamOpen {
        to: Some(BareJid::from_str("waddle.example").unwrap()),
        from: Some(BareJid::from_str("chat.example.com").unwrap()),
        id: Some(StreamId::new("stream-1")),
        language: Some("en".to_string()),
        version: StreamOpen::VERSION,
    });

    let encoded = encode_message(&message).unwrap();
    let decoded = decode_message(&encoded).unwrap();

    assert_eq!(decoded, message);
}

#[test]
fn decode_injects_client_namespace_for_stanzas() {
    let message =
        decode_message(r#"<iq type="get" id="ping-1"><ping xmlns="urn:xmpp:ping"/></iq>"#).unwrap();

    let TransportMessage::Element(element) = message else {
        panic!("expected element transport message");
    };

    assert_eq!(element.name(), "iq");
    assert_eq!(element.ns(), NS_CLIENT);
}

#[test]
fn decode_accepts_stream_error_as_single_websocket_frame() {
    let message = decode_message(
        r#"<stream:error xmlns:stream="http://etherx.jabber.org/streams"><undefined-condition xmlns="urn:ietf:params:xml:ns:xmpp-streams"/><handled-count-too-high xmlns="urn:xmpp:sm:3" h="3" send-count="2"/></stream:error>"#,
    )
    .unwrap();

    let TransportMessage::Element(element) = message else {
        panic!("expected stream error element");
    };

    assert_eq!(element.name(), "error");
    assert_eq!(element.ns(), "http://etherx.jabber.org/streams");
    assert!(element
        .get_child("handled-count-too-high", "urn:xmpp:sm:3")
        .is_some());
}

#[test]
fn decode_rejects_tcp_style_stream_close_in_websocket_frame() {
    let error = decode_message(
        r#"<stream:error xmlns:stream="http://etherx.jabber.org/streams"><undefined-condition xmlns="urn:ietf:params:xml:ns:xmpp-streams"/><handled-count-too-high xmlns="urn:xmpp:sm:3" h="3" send-count="2"/></stream:error></stream:stream>"#,
    )
    .unwrap_err();

    assert!(matches!(error, ClientError::InvalidTransportFrame));
}

#[test]
fn decode_allows_stream_close_literal_inside_message_cdata() {
    let message = decode_message(
        r#"<message xmlns="jabber:client"><body><![CDATA[</stream:stream>]]></body></message>"#,
    )
    .unwrap();

    let TransportMessage::Element(element) = message else {
        panic!("expected message element");
    };

    assert_eq!(element.name(), "message");
    let body = element.get_child("body", NS_CLIENT).expect("message body");
    assert_eq!(body.text(), "</stream:stream>");
}

#[test]
fn decode_rejects_multiple_top_level_websocket_elements() {
    let error =
        decode_message(r#"<message xmlns="jabber:client"/><presence xmlns="jabber:client"/>"#)
            .unwrap_err();

    assert!(matches!(error, ClientError::InvalidTransportFrame));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn concrete_transport_connects_and_emits_typed_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, assert_xmpp_subprotocol)
            .await
            .unwrap();

        let outbound = websocket.next().await.unwrap().unwrap();
        let Message::Text(text) = outbound else {
            panic!("expected text frame from client");
        };
        assert!(matches!(
            decode_message(text.as_ref()).unwrap(),
            TransportMessage::Open(_)
        ));

        websocket
            .send(
                Message::Text(
                    r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" from="waddle.example" id="stream-1" version="1.0"/>"#
                        .into(),                ),
            )
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                r#"<iq type="result" id="ping-1"><ping xmlns="urn:xmpp:ping"/></iq>"#.into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let mut live_config = config();
    live_config.transport =
        WebSocketConfig::new(Url::parse(&format!("ws://{address}/xmpp")).unwrap()).unwrap();

    let factory = DefaultTransportFactory;
    let mut transport = factory.connect(&live_config).await.unwrap();

    assert_eq!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::StateChanged(TransportState::Connecting))
    );
    assert_eq!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::StateChanged(TransportState::Open))
    );

    let outbound = TransportMessage::Open(StreamOpen::from_config(&live_config));
    transport.send(outbound.clone()).await.unwrap();
    assert!(matches!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::MessageSent(message)) if message == outbound
    ));

    assert!(matches!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::MessageReceived(TransportMessage::Open(open)))
            if open.id.as_ref().map(StreamId::as_str) == Some("stream-1")
    ));
    assert!(matches!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::MessageReceived(TransportMessage::Element(element)))
            if element.name() == "iq" && element.ns() == NS_CLIENT
    ));
    assert_eq!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::StateChanged(TransportState::Closing))
    );
    assert_eq!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::MessageReceived(TransportMessage::Close(
            StreamClose
        )))
    );
    assert_eq!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::StateChanged(TransportState::Closed))
    );
    assert_eq!(
        transport.next_event().await.unwrap(),
        Some(TransportEvent::Closed)
    );

    server.await.unwrap();
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[test]
fn transport_pins_the_rustls_connector() {
    use tokio_tungstenite::Connector;

    // Guards against regressing to tokio-tungstenite's auto-connector: with
    // whole-workspace feature unification the auto-connector would prefer
    // `native-tls` (OpenSSL on Android) whenever another workspace member
    // enables it.
    let connector = super::native::rustls_connector().unwrap();
    assert!(matches!(connector, Connector::Rustls(_)));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn concrete_transport_rejects_self_signed_tls_certificates() {
    use std::sync::Arc;
    use tokio_rustls::TlsAcceptor;

    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_der = certified.cert.der().clone();
    let key_der = rustls::pki_types::PrivateKeyDer::from(
        rustls::pki_types::PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()),
    );

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // The TLS handshake must fail: the client rejects the untrusted
        // self-signed certificate against its bundled webpki roots.
        assert!(acceptor.accept(stream).await.is_err());
    });

    let mut live_config = config();
    live_config.transport = WebSocketConfig::new(
        Url::parse(&format!("wss://localhost:{}/xmpp", address.port())).unwrap(),
    )
    .unwrap();

    let error = match DefaultTransportFactory.connect(&live_config).await {
        Err(error) => error,
        Ok(_) => panic!("expected certificate verification to fail"),
    };
    assert!(matches!(error, ClientError::WebSocket(_)));

    server.await.unwrap();
}
