//! RFC 7395 §3.8 keepalive over a real WebSocket (issue #1090).
//!
//! End-to-end conformance: the server sends `Ping` frames on an
//! inbound-idle interval, an idle-but-responsive client survives
//! indefinitely, a silent (dead) peer is closed after the miss limit
//! with a graceful close handshake, and a keepalive close takes the
//! XEP-0198 detach-for-resume path.
//!
//! The interval is shrunk to the 1s config floor via
//! `WADDLE_WS_KEEPALIVE_INTERVAL_SECS`; the pure policy timing lives in
//! the clock-free suite at `waddle-xmpp/tests/rfc7395_keepalive.rs`.

mod ws_common;

use std::time::Duration;

use futures::StreamExt;
use tokio_tungstenite::tungstenite;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";

/// Receive one raw WebSocket frame — control frames included.
///
/// This suite observes server-initiated `Ping` frames, which the
/// harness's `recv_timeout` deliberately treats as errors. Polling the
/// stream also lets tokio-tungstenite flush its automatic `Pong`
/// replies, so a client driven by this helper behaves like a healthy
/// browser. `Ok(None)` means the stream ended. Local to this suite (on
/// the harness it would be dead code in every other test binary).
async fn recv_raw_timeout(
    client: &mut WsXmppClient,
    dur: Duration,
) -> Result<Option<tungstenite::Message>, String> {
    match tokio::time::timeout(dur, client.ws.next()).await {
        Ok(Some(Ok(message))) => Ok(Some(message)),
        Ok(Some(Err(e))) => Err(format!("WebSocket error: {e}")),
        Ok(None) => Ok(None),
        Err(_) => Err("Timeout waiting for raw frame".to_string()),
    }
}

fn keepalive_server(interval_secs: &str, miss_limit: &str) -> TestServer {
    TestServer::start_with_extra_envs(
        &[],
        &[
            ("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", interval_secs),
            ("WADDLE_WS_KEEPALIVE_MISS_LIMIT", miss_limit),
        ],
    )
}

async fn connect(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("{resource}-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection")
}

/// AC: "Server sends WS ping frames on an idle interval" + "Idle
/// sessions survive past the current 5-minute window". With a 1s
/// interval, 12s of idling spans 12 intervals — the scaled equivalent
/// of far more than 5 minutes at the production 45s interval. The
/// client only polls (tokio-tungstenite auto-pongs, like a browser);
/// it must observe repeated server pings and remain fully functional
/// afterwards.
#[tokio::test]
async fn idle_session_receives_pings_and_survives() {
    let server = keepalive_server("1", "2");
    let mut client = connect(&server, "keepalive-idle").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let mut pings = 0u32;
    while tokio::time::Instant::now() < deadline && pings < 3 {
        match recv_raw_timeout(&mut client, Duration::from_secs(12)).await {
            Ok(Some(tungstenite::Message::Ping(_))) => pings += 1,
            Ok(Some(_)) => {} // unrelated frame (SM nudges, etc.)
            Ok(None) => panic!("server closed an idle-but-responsive connection"),
            Err(e) => panic!("keepalive ping never arrived: {e}"),
        }
    }
    assert!(
        pings >= 3,
        "expected repeated server-initiated pings on an idle connection; saw {pings}"
    );

    // The session must still be fully functional after many idle
    // intervals: an XMPP ping IQ round-trips.
    client
        .send(r#"<iq type="get" id="post-idle-ping"><ping xmlns="urn:xmpp:ping"/></iq>"#)
        .await
        .expect("send xmpp ping after idling");
    let reply = client
        .recv_matching(|frame| frame.contains("post-idle-ping"))
        .await
        .expect("xmpp ping reply after idling");
    assert!(
        reply.contains(r#"type="result""#) || reply.contains("type='result'"),
        "idle-surviving session must answer IQs: {reply}"
    );
}

/// AC: consecutive unanswered probes close the connection. A client
/// that stops polling never flushes tokio-tungstenite's auto-pongs —
/// from the server's perspective it is a dead peer. With interval=1s
/// and miss_limit=1 the close lands ~3s after the last inbound frame;
/// the client must then find a graceful `Close` (or clean EOF), not a
/// hung socket.
#[tokio::test]
async fn silent_peer_is_closed_after_miss_limit() {
    let server = keepalive_server("1", "1");
    let mut client = connect(&server, "keepalive-dead").await;

    // Go dark: no polling, so no pongs leave this side.
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Resume polling: buffered pings may surface first, then the
    // server's close must arrive. A live server would emit nothing but
    // pings here, so a bounded loop distinguishes closed from hung.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while tokio::time::Instant::now() < deadline {
        match recv_raw_timeout(&mut client, Duration::from_secs(10)).await {
            Ok(Some(tungstenite::Message::Close(_))) | Ok(None) => {
                closed = true;
                break;
            }
            Ok(Some(_)) => {}
            // A reset after the server tore down also proves closure,
            // but the graceful path should have delivered Close first.
            Err(e) => {
                assert!(
                    e.contains("WebSocket error"),
                    "expected close/EOF/error, got timeout: {e}"
                );
                closed = true;
                break;
            }
        }
    }
    assert!(
        closed,
        "server must close a silent peer after the miss limit"
    );
}

/// Locked design decision on #1090: "timer runs from WS upgrade so
/// wedged pre-bind connections get reaped too." A socket that upgrades
/// but never authenticates keeps auto-ponging at the WS layer
/// (tokio-tungstenite here, the browser network process in real
/// clients), so probe answers alone must not keep it alive — the
/// negotiation deadline (NEGOTIATION_TICK_LIMIT ticks) closes it.
#[tokio::test]
async fn unauthenticated_socket_is_reaped_despite_auto_pongs() {
    let server = keepalive_server("1", "2");
    // Connect the raw WebSocket only — no SASL, no bind.
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("raw websocket connect");

    // Poll continuously so every server ping is auto-ponged — this is
    // the "wedged app, live network stack" shape. Expect the server to
    // close ~3-4s in (negotiation limit tick 3 at the 1s interval).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut closed = false;
    while tokio::time::Instant::now() < deadline {
        match recv_raw_timeout(&mut client, Duration::from_secs(15)).await {
            Ok(Some(tungstenite::Message::Close(_))) | Ok(None) => {
                closed = true;
                break;
            }
            Ok(Some(_)) => {}
            Err(e) => {
                assert!(
                    e.contains("WebSocket error"),
                    "expected close/EOF/error, got timeout: {e}"
                );
                closed = true;
                break;
            }
        }
    }
    assert!(
        closed,
        "an unauthenticated auto-ponging socket must be reaped by the negotiation deadline"
    );
}

/// The keepalive close must ride the XEP-0198 detach-for-resume path:
/// a client whose network stalled (missed pongs → server closed) can
/// come back and `<resume/>` its stream instead of losing the session.
#[tokio::test]
async fn keepalive_close_detaches_for_sm_resume() {
    let server = keepalive_server("1", "1");
    let mut client = connect(&server, "keepalive-resume").await;

    client
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("enable stream management");
    let enabled = client
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("stream management enabled");
    let stream_id = ws_common::extract_attr_after(&enabled, "<enabled", "id")
        .unwrap_or_else(|| panic!("enabled missing id: {enabled}"));

    // Go dark until the keepalive reaps the connection (~3s at 1s/1
    // miss; generous margin for CI).
    tokio::time::sleep(Duration::from_secs(6)).await;

    let mut resumed = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("reconnect after keepalive close");
    resumed
        .authenticate(DOMAIN, USERNAME, server.fixed_account_password())
        .await
        .expect("re-authenticate after keepalive close");
    resumed
        .send(&format!(
            r#"<resume xmlns="urn:xmpp:sm:3" previd="{stream_id}" h="0"/>"#
        ))
        .await
        .expect("send resume");
    let resumption = resumed
        .recv_matching(|frame| frame.contains("<resumed") || frame.contains("<failed"))
        .await
        .expect("resume reply");
    assert!(
        resumption.contains("<resumed"),
        "keepalive close must detach for resume, not fully clean up: {resumption}"
    );
}
