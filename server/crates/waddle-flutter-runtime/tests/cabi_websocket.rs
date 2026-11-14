//! The actual C boundary over a local WebSocket, with typed XMPP bootstrap and stanza builders.

use std::ffi::{CStr, CString};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use minidom::Element;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{accept_hdr_async, WebSocketStream};
use waddle_flutter_runtime::{
    waddle_flutter_create, waddle_flutter_destroy, waddle_flutter_free_json, waddle_flutter_poll,
    waddle_flutter_submit,
};
use waddle_xmpp_client::bootstrap::{NS_BIND, NS_CLIENT, NS_SASL, NS_SM, NS_STREAMS};
use waddle_xmpp_client::stream_management::SmState;
use waddle_xmpp_client::transport::{
    decode_message, encode_message, StreamClose, StreamOpen, TransportMessage,
};
use xmpp_parsers::bind::BindResponse;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

type Socket = WebSocketStream<TcpStream>;

async fn receive(socket: &mut Socket) -> TransportMessage {
    loop {
        let frame = timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("receive deadline")
            .expect("socket frame")
            .expect("valid frame");
        match frame {
            WsMessage::Text(text) => {
                return decode_message(text.as_str()).expect("typed XMPP frame")
            }
            WsMessage::Close(_) => return TransportMessage::Close(StreamClose),
            _ => {}
        }
    }
}

async fn send(socket: &mut Socket, frame: TransportMessage) {
    socket
        .send(WsMessage::Text(
            encode_message(&frame).expect("encode typed frame").into(),
        ))
        .await
        .expect("send frame");
}

async fn element(socket: &mut Socket, value: Element) {
    send(socket, TransportMessage::Element(value)).await;
}

async fn bootstrap(socket: &mut Socket) {
    assert!(matches!(receive(socket).await, TransportMessage::Open(_)));
    let open = || {
        TransportMessage::Open(StreamOpen::from_server(
            "example.test".parse().expect("domain"),
        ))
    };
    send(socket, open()).await;
    element(
        socket,
        Element::builder("features", NS_STREAMS)
            .append(
                Element::builder("mechanisms", NS_SASL)
                    .append(
                        Element::builder("mechanism", NS_SASL)
                            .append("OAUTHBEARER")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    let TransportMessage::Element(auth) = receive(socket).await else {
        panic!("expected SASL authentication");
    };
    assert_eq!(auth.name(), "auth");
    assert_eq!(auth.attr("mechanism"), Some("OAUTHBEARER"));
    element(socket, Element::builder("success", NS_SASL).build()).await;
    assert!(matches!(receive(socket).await, TransportMessage::Open(_)));
    send(socket, open()).await;
    element(
        socket,
        Element::builder("features", NS_STREAMS)
            .append(Element::builder("bind", NS_BIND).build())
            .append(Element::builder("sm", NS_SM).build())
            .build(),
    )
    .await;
    let TransportMessage::Element(bind) = receive(socket).await else {
        panic!("expected resource bind");
    };
    let request = Iq::try_from(bind).expect("typed bind IQ");
    let response = Iq::from_result(
        request.id(),
        Some(
            BindResponse::try_from(
                Element::builder("bind", NS_BIND)
                    .append(
                        Element::builder("jid", NS_BIND)
                            .append("alice@example.test/flutter")
                            .build(),
                    )
                    .build(),
            )
            .expect("typed bind response"),
        ),
    );
    element(socket, response.into()).await;
    let TransportMessage::Element(enable) = receive(socket).await else {
        panic!("expected SM enable");
    };
    assert_eq!((enable.name(), enable.ns().as_str()), ("enable", NS_SM));
    assert_ne!(enable.attr("resume"), Some("true"));
    element(socket, Element::builder("enabled", NS_SM).build()).await;
}

struct XmppHandshake;

impl Callback for XmppHandshake {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        assert_eq!(
            request
                .headers()
                .get("Sec-WebSocket-Protocol")
                .expect("subprotocol"),
            "xmpp"
        );
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "xmpp".parse().expect("header"));
        Ok(response)
    }
}

async fn fake_server(listener: TcpListener) {
    let (stream, _) = listener.accept().await.expect("accept client");
    server_socket(stream).await;
}

async fn server_socket(stream: TcpStream) {
    let mut socket = accept_hdr_async(stream, XmppHandshake)
        .await
        .expect("WebSocket upgrade");
    bootstrap(&mut socket).await;
    let mut incoming = Message::new_with_type(
        MessageType::Chat,
        "alice@example.test/flutter"
            .parse::<jid::Jid>()
            .expect("recipient"),
    );
    incoming.from = Some("bob@example.test/phone".parse().expect("sender"));
    incoming.id = Some(Id("incoming-1".into()));
    incoming
        .bodies
        .insert(Lang(String::new()), "hello Flutter".into());
    element(&mut socket, incoming.into()).await;
    loop {
        match receive(&mut socket).await {
            TransportMessage::Element(stanza) if stanza.is("message", NS_CLIENT) => {
                let message = Message::try_from(stanza).expect("typed outbound message");
                assert_eq!(
                    message.to.as_ref().map(ToString::to_string).as_deref(),
                    Some("bob@example.test")
                );
                assert_eq!(message.bodies.get("").expect("body"), "outbound Flutter");
                assert_eq!(
                    message.id.as_ref().map(|id| id.0.as_str()),
                    Some("outgoing-1")
                );
                element(&mut socket, SmState::build_ack(1)).await;
            }
            TransportMessage::Element(stanza) if stanza.is("r", NS_SM) => {
                element(&mut socket, SmState::build_ack(1)).await;
            }
            TransportMessage::Close(_) => {
                let _ = socket.close(None).await;
                break;
            }
            _ => {}
        }
    }
}

struct Client {
    handle: u64,
    frames: Vec<Value>,
}

impl Client {
    fn new() -> Self {
        let handle = waddle_flutter_create();
        assert_ne!(handle, 0);
        Self {
            handle,
            frames: Vec::new(),
        }
    }

    fn rpc(&self, method: &str, args: Value) -> u64 {
        let request = CString::new(json!({"method": method, "args": args}).to_string())
            .expect("JSON CString");
        // SAFETY: CString remains alive and readable for the nonblocking submit call.
        let id = unsafe { waddle_flutter_submit(self.handle, request.as_ptr()) };
        assert_ne!(id, 0);
        id
    }

    fn poll(&mut self) {
        let pointer = waddle_flutter_poll(self.handle);
        assert!(!pointer.is_null());
        // SAFETY: poll returns an owned, valid NUL-terminated buffer, accessed before it is freed.
        let text = unsafe { CStr::from_ptr(pointer) }.to_str().expect("UTF8");
        let frames: Vec<Value> = serde_json::from_str(text).expect("JSON frames");
        // SAFETY: This returns the exact allocation once, after its contents were copied.
        unsafe { waddle_flutter_free_json(pointer) };
        self.frames.extend(frames);
    }

    async fn wait(&mut self, predicate: impl Fn(&Value) -> bool) {
        timeout(Duration::from_secs(5), async {
            loop {
                self.poll();
                if self.frames.iter().any(&predicate) {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("expected native event/result before deadline");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        waddle_flutter_destroy(self.handle);
    }
}

#[tokio::test]
async fn c_abi_connects_binds_delivers_messages_acknowledges_and_retires() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let address = listener.local_addr().expect("bound address");
    let server = tokio::spawn(fake_server(listener));
    let mut client = Client::new();
    let connect = client.rpc("connect", json!({"server_url": format!("ws://{address}/xmpp"), "jid": "alice@example.test", "access_token": "local-test-token", "resource": "flutter", "resumable": false}));
    client
        .wait(|frame| frame["event"]["type"] == "connected")
        .await;
    client
        .wait(|frame| frame["event"]["type"] == "message")
        .await;
    let incoming = client
        .frames
        .iter()
        .find(|frame| frame["event"]["type"] == "message")
        .expect("incoming");
    assert_eq!(incoming["generation"], 1);
    assert_eq!(incoming["event"]["data"]["body"], "hello Flutter");
    assert_eq!(incoming["event"]["data"]["from"], "bob@example.test/phone");
    assert_eq!(incoming["event"]["data"]["id"], "incoming-1");
    client.wait(|frame| frame["request_id"] == connect).await;
    let options = waddle_xmpp_client_ffi::WaddleSendOptions {
        stanza_id: Some("outgoing-1".into()),
        ..Default::default()
    };
    let send = client.rpc(
        "send_chat_message",
        json!({"peer_jid": "bob@example.test", "body": "outbound Flutter", "options": options}),
    );
    client.wait(|frame| frame["request_id"] == send).await;
    let result = client
        .frames
        .iter()
        .find(|frame| frame["request_id"] == send)
        .expect("send result");
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["type"], "sent");
    assert_eq!(result["data"]["stanza_id"], "outgoing-1");
    client
        .wait(|frame| {
            frame["event"]["type"] == "deliveryAcked"
                && frame["event"]["data"]["stanza_id"] == "outgoing-1"
        })
        .await;
    let shutdown = client.rpc("disconnect", json!({}));
    client.wait(|frame| frame["request_id"] == shutdown).await;
    let stopped = client
        .frames
        .iter()
        .find(|frame| frame["request_id"] == shutdown)
        .expect("shutdown result");
    assert_eq!(stopped["ok"], true);
    let handle = client.handle;
    drop(client);
    timeout(Duration::from_secs(5), server)
        .await
        .expect("disconnect closes socket")
        .expect("server task");
    let mut retired = Client {
        handle,
        frames: Vec::new(),
    };
    retired.poll();
    assert!(retired.frames.is_empty());
}

#[tokio::test]
async fn shutdown_and_successor_wait_for_a_blocked_connect_to_finish_and_close() {
    let first = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("first listener");
    let first_address = first.local_addr().expect("address");
    let second = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("second listener");
    let second_address = second.local_addr().expect("address");
    let (accepted, acceptance) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let old_server = tokio::spawn(async move {
        let (stream, _) = first.accept().await.expect("first connection");
        accepted.send(()).expect("acceptance signal");
        released.await.expect("release handshake");
        let mut socket = accept_hdr_async(stream, XmppHandshake)
            .await
            .expect("first upgrade");
        while !matches!(receive(&mut socket).await, TransportMessage::Close(_)) {}
        let _ = socket.close(None).await;
    });
    let (new_accept, mut new_accepted) = oneshot::channel();
    let new_server = tokio::spawn(async move {
        let (stream, _) = second.accept().await.expect("successor connection");
        new_accept.send(()).expect("successor acceptance");
        server_socket(stream).await;
    });
    let mut client = Client::new();
    let args = |address| json!({"server_url": format!("ws://{address}/xmpp"), "jid":"alice@example.test", "access_token":"local-test-token", "resource":"flutter", "resumable":false});
    client.rpc("connect", args(first_address));
    timeout(Duration::from_secs(5), acceptance)
        .await
        .expect("first TCP connection")
        .expect("signal");
    let shutdown = client.rpc("disconnect", json!({}));
    sleep(Duration::from_millis(25)).await;
    client.poll();
    assert!(!client
        .frames
        .iter()
        .any(|frame| frame["request_id"] == shutdown));
    client.rpc("connect", args(second_address));
    sleep(Duration::from_millis(25)).await;
    client.poll();
    assert!(!client
        .frames
        .iter()
        .any(|frame| frame["event"]["type"] == "connected"));
    assert!(matches!(
        new_accepted.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    release.send(()).expect("unblock old handshake");
    timeout(Duration::from_secs(5), old_server)
        .await
        .expect("old socket closed")
        .expect("old server");
    client
        .wait(|frame| frame["event"]["type"] == "connected" && frame["generation"] == 3)
        .await;
    let shutdown = client.rpc("disconnect", json!({}));
    client
        .wait(|frame| frame["request_id"] == shutdown && frame["ok"] == true)
        .await;
    timeout(Duration::from_secs(5), new_server)
        .await
        .expect("new socket closed")
        .expect("new server");
}
