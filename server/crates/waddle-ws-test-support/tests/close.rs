use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_hdr_async, WebSocketStream};
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::minidom::Element;

const CLOSE: &str = "<close xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>";
const REQUEST: &str = "<r xmlns='urn:xmpp:sm:3'/>";

struct XmppSubprotocol;

impl Callback for XmppSubprotocol {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "xmpp".parse().expect("protocol header"),
        );
        Ok(response)
    }
}

async fn accept_xmpp(listener: TcpListener) -> WebSocketStream<TcpStream> {
    let (socket, _) = listener.accept().await.expect("accept client");
    accept_hdr_async(socket, XmppSubprotocol)
        .await
        .expect("WebSocket handshake")
}

async fn scripted_peer(
    frames: Vec<&'static str>,
    send_close_ack: bool,
) -> (WsXmppClient, JoinHandle<Option<Message>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let url = format!("ws://{}", listener.local_addr().expect("peer address"));
    let peer = tokio::spawn(async move {
        let mut ws = accept_xmpp(listener).await;
        let close = ws.next().await.expect("client close").expect("close frame");
        let Message::Text(close) = close else {
            panic!("expected XMPP close, got {close:?}");
        };
        let close: Element = close.parse().expect("valid client close");
        assert!(close.is("close", "urn:ietf:params:xml:ns:xmpp-framing"));
        for frame in frames {
            ws.send(Message::text(frame))
                .await
                .expect("send scripted frame");
        }
        if send_close_ack {
            ws.send(Message::text(CLOSE)).await.expect("send close ack");
        }
        let next = timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("client stops waiting")
            .and_then(Result::ok);
        if matches!(next, Some(Message::Close(_))) {
            ws.flush().await.expect("reply to WebSocket close");
        }
        next
    });
    let client = WsXmppClient::connect(&url).await.expect("connect client");
    (client, peer)
}

#[tokio::test]
async fn close_drains_only_in_flight_sm_requests_without_replying() {
    let (client, peer) = scripted_peer(
        vec![REQUEST, "<sm:r xmlns:sm='urn:xmpp:sm:3'></sm:r>", REQUEST],
        true,
    )
    .await;
    client
        .close()
        .await
        .expect("complete both close handshakes");
    assert!(
        matches!(peer.await.expect("peer task"), Some(Message::Close(_))),
        "the client must send a WebSocket close, not an SM acknowledgement after its XMPP close"
    );
}

#[tokio::test]
async fn close_rejects_unknown_or_malformed_frames_after_an_sm_request() {
    for frame in [
        "<r xmlns='urn:wrong'/>",
        "<request xmlns='urn:xmpp:sm:3'/>",
        "<r xmlns='urn:xmpp:sm:3' unexpected='true'/>",
        "<r xmlns='urn:xmpp:sm:3'><unexpected/></r>",
        "<r xmlns='urn:xmpp:sm:3'>unexpected</r>",
        "<r xmlns='urn:xmpp:sm:3'>",
        "<r xmlns='urn:xmpp:sm:3'/>trailing",
        "<r xmlns='urn:xmpp:sm:3'/><unexpected xmlns='urn:wrong'/>",
    ] {
        let (client, peer) = scripted_peer(vec![REQUEST, frame], false).await;
        let error = client.close().await.expect_err(frame);
        assert_eq!(
            error,
            format!("Unexpected text frame while waiting for XMPP close acknowledgement: {frame}")
        );
        peer.await.expect("peer task");
    }
}

#[tokio::test]
async fn close_rejects_duplicate_namespace_declarations() {
    for frame in [
        "<r xmlns='urn:wrong' xmlns='urn:xmpp:sm:3'/>",
        "<r xmlns='urn:xmpp:sm:3' xmlns='urn:xmpp:sm:3'/>",
        "<sm:r xmlns:sm='urn:wrong' xmlns:sm='urn:xmpp:sm:3'/>",
        "<sm:r xmlns:sm='urn:xmpp:sm:3' xmlns:sm='urn:xmpp:sm:3'/>",
    ] {
        let (client, peer) = scripted_peer(vec![REQUEST, frame], false).await;
        let error = client.close().await.expect_err(frame);
        assert_eq!(
            error,
            format!("Unexpected text frame while waiting for XMPP close acknowledgement: {frame}")
        );
        peer.await.expect("peer task");
    }
}

#[tokio::test]
async fn close_still_rejects_stream_errors_after_an_sm_request() {
    let error_frame = "<stream:error xmlns:stream='http://etherx.jabber.org/streams'><unexpected-request xmlns='urn:ietf:params:xml:ns:xmpp-streams'/></stream:error>";
    let (client, peer) = scripted_peer(vec![REQUEST, error_frame], false).await;
    assert_eq!(
        client.close().await.expect_err("stream error must surface"),
        format!("Stream error during close handshake: {error_frame}")
    );
    peer.await.expect("peer task");
}

#[tokio::test]
async fn close_rejects_sm_requests_after_the_peer_closed_its_xmpp_stream() {
    let (client, peer) = scripted_peer(vec![CLOSE, REQUEST], false).await;
    assert_eq!(
        client
            .close()
            .await
            .expect_err("SM request after peer close"),
        format!("Unexpected text frame after WebSocket close request: {REQUEST}")
    );
    peer.await.expect("peer task");
}

#[tokio::test]
async fn close_requires_xmpp_ack_before_websocket_close() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let url = format!("ws://{}", listener.local_addr().expect("peer address"));
    let peer = tokio::spawn(async move {
        let mut ws = accept_xmpp(listener).await;
        ws.next().await.expect("client close").expect("close frame");
        ws.send(Message::text(REQUEST))
            .await
            .expect("send SM request");
        ws.close(None).await.expect("close WebSocket prematurely");
    });
    let client = WsXmppClient::connect(&url).await.expect("connect client");
    assert_eq!(
        client
            .close()
            .await
            .expect_err("XMPP close ack is required"),
        "WebSocket closed before XMPP close acknowledgement"
    );
    peer.await.expect("peer task");
}

#[tokio::test]
async fn repeated_sm_requests_do_not_extend_the_close_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
    let url = format!("ws://{}", listener.local_addr().expect("peer address"));
    let peer = tokio::spawn(async move {
        let mut ws = accept_xmpp(listener).await;
        ws.next().await.expect("client close").expect("close frame");
        let mut requests = interval(Duration::from_millis(40));
        timeout(Duration::from_secs(4), async {
            loop {
                tokio::select! {
                    _ = requests.tick() => {
                        if ws.send(Message::text(REQUEST)).await.is_err() {
                            break;
                        }
                    }
                    message = ws.next() => {
                        assert!(
                            message.is_none() || matches!(message, Some(Err(_))),
                            "the client must not answer SM requests after its XMPP close: {message:?}"
                        );
                        break;
                    }
                }
            }
        })
        .await
        .expect("client disconnects after its bounded close wait");
    });
    let client = WsXmppClient::connect(&url).await.expect("connect client");
    let error = timeout(Duration::from_secs(3), client.close())
        .await
        .expect("overall two-second deadline is not renewed by requests")
        .expect_err("requests do not replace a close acknowledgement");
    assert_eq!(error, "Timed out waiting for XMPP close acknowledgement");
    peer.await.expect("peer task");
}
