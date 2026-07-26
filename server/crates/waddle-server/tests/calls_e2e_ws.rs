//! XEP-0353 + XEP-0166 + waddle-livekit-transport wire-conformance
//! tests against a live waddle-server with `LIVEKIT_*` envs set.
//!
//! Scope: the JMI ring layer (alice → bob propose/proceed/reject),
//! the Jingle session-initiate transport-rewrite path, and the
//! XEP-0272 Muji MUC-presence reflection path that drives the
//! per-room "call ongoing" indicator. The full engine-level
//! connect-to-LiveKit step is exercised by manual smoke tests in
//! the browser; this suite covers everything the server is
//! responsible for on the wire.

use waddle_ws_test_support as ws_common;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::json;
use sha2::{Digest, Sha256};
use ws_common::{disco_info_query, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";
const NS_CALL_THREAD: &str = "urn:waddle:call-thread:0";
const NS_FASTEN: &str = "urn:xmpp:fasten:0";
const NS_SID: &str = "urn:xmpp:sid:0";
const NODE_GROUP_DM_CREATE: &str = "urn:waddle:group-dm:create:0";

const DOMAIN: &str = "localhost";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const CAROL: &str = "carol";
const ALICE_PW: &str = "alice-pw-12345";
const BOB_PW: &str = "bob-pw-12345";
const CAROL_PW: &str = "carol-pw-12345";
const LIVEKIT_WEBHOOK_SECRET: &str = "test-webhook-secret-with-at-least-32-bytes";

// The harness rejects api-secrets shorter than 32 bytes; this one is
// exactly 32 ASCII characters. The other LIVEKIT_* values are
// well-formed but never reach a real LiveKit server in this test —
// no LiveKit connect happens, only JWT mint + transport rewrite.
fn livekit_test_envs() -> Vec<(&'static str, &'static str)> {
    vec![
        ("LIVEKIT_API_KEY", "APItestkeyworkshop"),
        (
            "LIVEKIT_API_SECRET",
            "test-secret-with-at-least-32-bytes-of-payload",
        ),
        ("LIVEKIT_WS_URL", "wss://livekit.example.test"),
        ("LIVEKIT_TURN_HOST", "turn.example.test"),
        (
            "LIVEKIT_TURN_SHARED_SECRET",
            "turn-shared-secret-value-also-long-enough",
        ),
        ("LIVEKIT_WEBHOOK_SECRET", LIVEKIT_WEBHOOK_SECRET),
    ]
}

async fn start_pair() -> (TestServer, WsXmppClient, WsXmppClient) {
    let server = TestServer::start_with_extra_envs(
        &[(ALICE, ALICE_PW), (BOB, BOB_PW)],
        &livekit_test_envs(),
    );
    let alice = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ALICE, ALICE_PW, "ax")
        .await
        .expect("alice connect");
    let bob = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, BOB, BOB_PW, "bx")
        .await
        .expect("bob connect");
    (server, alice, bob)
}

#[tokio::test]
async fn server_disco_advertises_call_features_when_livekit_enabled() {
    let server = TestServer::start_with_extra_envs(&[], &livekit_test_envs());
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "ax",
    )
    .await
    .expect("admin connect");

    let resp = disco_info_query(&mut admin, DOMAIN, "disco-call-1")
        .await
        .expect("disco#info");

    for ns in [
        "urn:xmpp:jingle:1",
        "urn:xmpp:jingle:apps:rtp:1",
        "urn:xmpp:jingle:apps:rtp:audio",
        "urn:xmpp:jingle:apps:rtp:video",
        "urn:xmpp:jingle-message:0",
        "urn:xmpp:extdisco:2",
        "urn:waddle:transports:livekit:0",
        // XEP-0272 (Muji) — MUC group call presence advertisement +
        // Jingle session-initiate join surface to the SFU mixer.
        // Backed by the Muji branch in `JingleHandler`; the legacy
        // `urn:waddle:muc-call:0` IQ surface and `MucCallHandler`
        // have been removed.
        "urn:xmpp:jingle:muji:0",
    ] {
        assert!(
            resp.contains(ns),
            "disco#info must advertise {ns}; got: {resp}"
        );
    }
}

#[tokio::test]
async fn mixer_jid_disco_info_advertises_muji_and_focus_identity() {
    // XEP-0030 / XEP-0272 §Discovery: a strict client SHOULD be
    // able to disco#info the `calls.<domain>` mixer and find a
    // `<identity category='conference' type='audio-video'/>` plus
    // the `urn:xmpp:jingle:muji:0` feature BEFORE sending a
    // session-initiate. Without this round-trip, a Muji-compliant
    // peer can't safely discover the mixer.
    let server = TestServer::start_with_extra_envs(&[], &livekit_test_envs());
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "ax",
    )
    .await
    .expect("admin connect");

    let resp = disco_info_query(&mut admin, &format!("calls.{DOMAIN}"), "mixer-disco-1")
        .await
        .expect("mixer disco#info");

    assert!(
        resp.contains("category='conference'") && resp.contains("type='audio-video'"),
        "mixer must advertise the XEP-0272 / av-conferences identity; got: {resp}"
    );
    assert!(
        resp.contains("urn:xmpp:jingle:muji:0"),
        "mixer must advertise the Muji feature; got: {resp}"
    );
    assert!(
        resp.contains("urn:xmpp:jingle:1"),
        "mixer must advertise base Jingle feature; got: {resp}"
    );
    assert!(
        resp.contains("urn:waddle:transports:livekit:0"),
        "mixer must advertise the LiveKit transport feature; got: {resp}"
    );
}

#[tokio::test]
async fn jmi_propose_is_forwarded_to_peer() {
    let (_server, mut alice, mut bob) = start_pair().await;

    // Alice rings bob with an audio+video JMI propose.
    let sid = "ce2e1";
    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" to="bob@localhost">
                 <propose xmlns="urn:xmpp:jingle-message:0" id="{sid}">
                   <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                   <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="video"/>
                 </propose>
               </message>"#
        ))
        .await
        .expect("alice sends propose");

    let received = bob
        .recv_matching(|frame| frame.contains("<propose") && frame.contains(sid))
        .await
        .expect("bob receives propose");
    assert!(
        received.contains(r#"from='alice@localhost"#)
            || received.contains(r#"from="alice@localhost"#),
        "from missing: {received}"
    );
    assert!(received.contains("urn:xmpp:jingle-message:0"));
    assert!(received.contains(r#"media='audio'"#));
    assert!(received.contains(r#"media='video'"#));
}

#[tokio::test]
async fn jmi_proceed_round_trips_back_to_caller() {
    let (_server, mut alice, mut bob) = start_pair().await;

    let alice_full = alice.full_jid.clone().expect("alice bound");
    let sid = "ce2e2";
    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" to="bob@localhost">
                 <propose xmlns="urn:xmpp:jingle-message:0" id="{sid}">
                   <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                 </propose>
               </message>"#
        ))
        .await
        .expect("alice propose");
    bob.recv_matching(|frame| frame.contains("<propose") && frame.contains(sid))
        .await
        .expect("bob sees propose");

    // XEP-0353 §0.6: proceed addressed to the *full* JID of the
    // initiator's accepting resource, not the bare JID, so only the
    // ringing resource stops ringing.
    bob.send(&format!(
        r#"<message xmlns="jabber:client" to="{alice_full}">
             <proceed xmlns="urn:xmpp:jingle-message:0" id="{sid}"/>
           </message>"#
    ))
    .await
    .expect("bob proceeds");

    let proceed = alice
        .recv_matching(|frame| frame.contains("<proceed") && frame.contains(sid))
        .await
        .expect("alice receives proceed");
    assert!(
        proceed.contains(r#"from='bob@localhost"#) || proceed.contains(r#"from="bob@localhost"#),
        "from missing: {proceed}"
    );
    assert!(proceed.contains("urn:xmpp:jingle-message:0"));
}

/// XEP-0191 incoming-block must apply to peer-routed Jingle IQs:
/// an authenticated user who has bob@domain on their blocklist
/// must not receive bob's session-initiate. The JingleHandler
/// rewrites the transport regardless (it cannot see the
/// recipient's blocklist), but the recipient's state machine
/// drops the IQ before writing to the wire.
#[tokio::test]
async fn session_initiate_from_blocked_peer_is_dropped() {
    let (_server, mut alice, mut bob) = start_pair().await;

    let alice_full = alice.full_jid.clone().expect("alice bound");
    let bob_full = bob.full_jid.clone().expect("bob bound");

    // Bob blocks alice via XEP-0191. Wait for the IQ result so we
    // know the block is committed before alice rings.
    bob.send(
        r#"<iq xmlns="jabber:client" type="set" id="block-1">
             <block xmlns="urn:xmpp:blocking">
               <item jid="alice@localhost"/>
             </block>
           </iq>"#,
    )
    .await
    .expect("bob blocks alice");
    bob.recv_matching(|f| f.contains(r#"id='block-1'"#))
        .await
        .expect("block ack");

    let sid = "blocked-call";
    alice
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="ji-{sid}" to="{bob_full}">
                 <jingle xmlns="urn:xmpp:jingle:1" action="session-initiate"
                         initiator="{alice_full}" sid="{sid}">
                   <content creator="initiator" name="audio">
                     <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio">
                       <payload-type id="111" name="opus" clockrate="48000" channels="2"/>
                     </description>
                     <transport xmlns="urn:waddle:transports:livekit:0"/>
                   </content>
                 </jingle>
               </iq>"#
        ))
        .await
        .expect("alice session-initiate");

    let dropped = bob
        .recv_matching(|frame| {
            frame.contains("session-initiate") && frame.contains(&format!(r#"sid='{sid}'"#))
                || frame.contains(&format!(r#"sid='{sid}'"#))
        })
        .await;
    assert!(
        dropped.is_err(),
        "blocked peer's session-initiate must not reach bob; got: {dropped:?}"
    );
}

#[tokio::test]
async fn session_initiate_rewrites_empty_waddle_transport() {
    let (_server, mut alice, mut bob) = start_pair().await;

    let sid = "ce2e3";
    let alice_full = alice.full_jid.clone().expect("alice bound");
    let bob_full = bob.full_jid.clone().expect("bob bound");

    // Send the bare-minimum session-initiate shape: one audio content
    // with an empty Waddle LiveKit transport that the server is
    // required to populate before forwarding to bob.
    alice
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="ji-{sid}" to="{bob_full}">
                 <jingle xmlns="urn:xmpp:jingle:1" action="session-initiate"
                         initiator="{alice_full}" sid="{sid}">
                   <content creator="initiator" name="audio">
                     <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio">
                       <payload-type id="111" name="opus" clockrate="48000" channels="2"/>
                     </description>
                     <transport xmlns="urn:waddle:transports:livekit:0"/>
                   </content>
                 </jingle>
               </iq>"#
        ))
        .await
        .expect("alice session-initiate");

    // Wait for the *populated* (server-rewritten) transport. The
    // sid match alone is ambiguous because the original empty
    // transport would also satisfy it; the `url=` attribute is only
    // present after the JingleHandler stamps it.
    let forwarded = bob
        .recv_matching(|frame| {
            (frame.contains(&format!(r#"sid='{sid}'"#))
                || frame.contains(&format!(r#"sid='{sid}'"#)))
                && frame.contains("session-initiate")
                && frame.contains("url=")
        })
        .await
        .expect("bob receives rewritten session-initiate");

    // The forwarded transport MUST be a populated one: url/room/
    // identity attributes plus a non-empty <token/> child. The
    // server scopes the room id by initiator bare JID so an
    // attacker can't pick a colliding sid; assert that scoping is
    // present.
    // The `url::Url` parser normalises the websocket URL with a
    // trailing slash on the empty path, so accept either form.
    assert!(
        forwarded.contains("livekit.example.test"),
        "url mismatch; got: {forwarded}"
    );
    assert!(
        forwarded.contains(&format!(r#"room='alice@localhost::{sid}'"#)),
        "room must be scoped by initiator bare jid; got: {forwarded}"
    );
    assert!(
        forwarded.contains(&format!(r#"identity='{bob_full}'"#))
            || forwarded.contains(&format!(r#"identity='{bob_full}'"#)),
        "identity mismatch; got: {forwarded}"
    );
    // <token> is a child element, not an attribute — look for an
    // opening tag and a non-trivial body. JWT segments are
    // base64url-encoded so dots between header/payload/sig are a
    // reliable structural marker.
    assert!(
        forwarded.contains("<token") && forwarded.matches('.').count() > 2,
        "token child must be a populated JWT; got: {forwarded}"
    );
}

// ── XEP-0272 §Joining: sibling-resource Muji reflection ───────────────────

/// Send a XEP-0045 join presence to `room/nick` and drain frames until
/// the room's historical-subject ack arrives (the last frame in the
/// join sequence). Mirrors the helper in `xep0045_kick_presence_broadcast_ws.rs`
/// but local to this test file so we don't have to share mutable state
/// across integration test binaries.
async fn muji_join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{nick}"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses including subject ack");
}

async fn try_join_room(client: &mut WsXmppClient, room: &str, nick: &str) -> String {
    client
        .send(&format!(
            r#"<presence xmlns="jabber:client" to="{room}/{nick}"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_matching(|frame| {
            frame.contains("<presence")
                && (frame.contains(&format!("from='{room}/{nick}'"))
                    || frame.contains(&format!("from=\"{room}/{nick}\"")))
        })
        .await
        .expect("join response")
}

fn parse_presence(frame: &str) -> Element {
    frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"))
}

fn muc_user_has_status_110(presence: &Element) -> bool {
    let Some(x) = presence
        .children()
        .find(|c| c.name() == "x" && c.ns() == NS_MUC_USER)
    else {
        return false;
    };
    x.children()
        .filter(|c| c.name() == "status" && c.ns() == NS_MUC_USER)
        .any(|s| s.attr("code") == Some("110"))
}

fn muji_child(presence: &Element) -> Option<&Element> {
    presence
        .children()
        .find(|c| c.name() == "muji" && c.ns() == NS_MUJI)
}

fn call_thread_child(message: &Element) -> Option<&Element> {
    message
        .children()
        .find(|c| c.name() == "call-thread" && c.ns() == NS_CALL_THREAD)
}

fn apply_to_child(message: &Element) -> Option<&Element> {
    message
        .children()
        .find(|c| c.name() == "apply-to" && c.ns() == NS_FASTEN)
}

fn call_thread_ended_child(apply_to: &Element) -> Option<&Element> {
    apply_to
        .children()
        .find(|c| c.name() == "call-thread-ended" && c.ns() == NS_CALL_THREAD)
}

fn room_stanza_id(message: &Element, room: &str) -> Option<String> {
    message
        .children()
        .find(|c| c.name() == "stanza-id" && c.ns() == NS_SID && c.attr("by") == Some(room))
        .and_then(|c| c.attr("id"))
        .map(ToOwned::to_owned)
}

fn origin_id(message: &Element) -> Option<String> {
    message
        .children()
        .find(|c| c.name() == "origin-id" && c.ns() == NS_SID)
        .and_then(|c| c.attr("id"))
        .map(ToOwned::to_owned)
}

fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    String::from_utf8(bytes).expect("XML serialization is UTF-8")
}

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
}

fn text_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "text-single")
        .append(Element::builder("value", NS_DATA).append(value).build())
        .build()
}

fn hidden_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(Element::builder("value", NS_DATA).append(value).build())
        .build()
}

fn list_multi_field(var: &str, values: &[&str]) -> Element {
    let mut field = Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "list-multi");
    for value in values {
        field = field.append(Element::builder("value", NS_DATA).append(*value).build());
    }
    field.build()
}

fn extract_field(frame: &str, var: &str) -> Option<String> {
    let marker_dq = format!(r#"var="{var}""#);
    let marker_sq = format!(r#"var='{var}'"#);
    let idx = frame.find(&marker_dq).or_else(|| frame.find(&marker_sq))?;
    let after = &frame[idx..];
    let open = after.find("<value>")?;
    let inner = &after[open + "<value>".len()..];
    let close = inner.find("</value>")?;
    Some(inner[..close].to_string())
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type='result'"#) || frame.contains(r#"type="result""#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) || frame.contains(r#"type="error""#)
}

async fn send_command(client: &mut WsXmppClient, node: &str, id: &str, form: Element) -> String {
    let command = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(form)
        .build();
    let iq = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
        .append(command)
        .build();
    client.send(&element_to_xml(iq)).await.expect("send iq");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("iq response")
}

async fn create_group_dm(
    client: &mut WsXmppClient,
    id: &str,
    name: &str,
    members: &[&str],
) -> String {
    let resp = send_command(
        client,
        NODE_GROUP_DM_CREATE,
        id,
        Element::builder("x", NS_DATA)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(hidden_field("FORM_TYPE", NODE_GROUP_DM_CREATE))
            .append(text_field("name", name))
            .append(list_multi_field("member_jids", members))
            .build(),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected group-DM create result, got: {resp}"
    );
    extract_field(&resp, "room_jid").expect("room_jid in create response")
}

async fn recv_muji_leave_reflection(client: &mut WsXmppClient, room: &str, nick: &str) -> Element {
    let from_attr_single = format!("from='{room}/{nick}'");
    let from_attr_double = format!("from=\"{room}/{nick}\"");
    let frame = client
        .recv_matching(|frame| {
            frame.contains("<presence")
                && (frame.contains(&from_attr_single) || frame.contains(&from_attr_double))
                && !frame.contains("<muji")
        })
        .await
        .unwrap_or_else(|err| panic!("{nick} leave reflection must arrive: {err}"));
    let presence = parse_presence(&frame);
    assert!(
        muji_child(&presence).is_none(),
        "XEP-0272 §Leaving wire shape: muji child must be stripped: {frame}"
    );
    presence
}

async fn assert_no_call_thread_ended(client: &mut WsXmppClient, context: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(750);
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let frame = client.recv_timeout(remaining).await;
        match frame {
            Ok(frame) => {
                assert!(
                    !(frame.contains("<call-thread-ended") && frame.contains("<apply-to")),
                    "{context}: call-thread-ended arrived too early: {frame}"
                );
            }
            Err(err) if err.contains("Timeout waiting for message") => break,
            Err(err) => panic!("{context}: failed while checking for premature call end: {err}"),
        }
    }
}

fn livekit_webhook_auth(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    let claims = json!({
        "sha256": BASE64_STANDARD.encode(digest),
        "exp": (chrono::Utc::now() + chrono::Duration::seconds(60)).timestamp(),
        "iat": chrono::Utc::now().timestamp(),
    });
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(LIVEKIT_WEBHOOK_SECRET.as_bytes()),
    )
    .expect("sign LiveKit webhook");
    format!("Bearer {token}")
}

async fn post_participant_left_webhook(server: &TestServer, room: &str, identity: &str) {
    let body = serde_json::to_vec(&json!({
        "id": format!("EV_{}", uuid::Uuid::new_v4()),
        "event": "participant_left",
        "room": { "name": room },
        "participant": { "identity": identity },
    }))
    .expect("webhook body");
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/livekit/webhook", server.http_base_url()))
        .header("Authorization", livekit_webhook_auth(&body))
        .body(body)
        .send()
        .await
        .expect("post LiveKit webhook");
    assert!(
        response.status().is_success(),
        "LiveKit webhook failed: {}",
        response.status()
    );
}

async fn send_muji_session_initiate_request(client: &mut WsXmppClient, room: &str, sid: &str) {
    let full_jid = client.full_jid.clone().expect("client bound");
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="muji-join-{sid}" to="calls.{DOMAIN}">
                 <jingle xmlns="urn:xmpp:jingle:1" action="session-initiate"
                         initiator="{full_jid}" sid="{sid}">
                   <muji xmlns="{NS_MUJI}" room="{room}"/>
                   <content creator="initiator" name="audio">
                     <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio">
                       <payload-type id="111" name="opus" clockrate="48000" channels="2"/>
                     </description>
                     <transport xmlns="urn:waddle:transports:livekit:0"/>
                   </content>
                 </jingle>
               </iq>"#
        ))
        .await
        .expect("send Muji session-initiate");
}

async fn send_muji_session_initiate(client: &mut WsXmppClient, room: &str, sid: &str) {
    send_muji_session_initiate_request(client, room, sid).await;
    client
        .recv_matching(|frame| {
            (frame.contains("type='result'") && frame.contains(&format!("id='muji-join-{sid}'")))
                || (frame.contains("type=\"result\"")
                    && frame.contains(&format!("id=\"muji-join-{sid}\"")))
        })
        .await
        .expect("Muji session-initiate ack");
    client
        .recv_matching(|frame| {
            frame.contains("session-accept")
                && (frame.contains(&format!("sid='{sid}'"))
                    || frame.contains(&format!("sid=\"{sid}\"")))
        })
        .await
        .expect("Muji session-accept");
}

async fn send_muji_session_initiate_expect_forbidden(
    client: &mut WsXmppClient,
    room: &str,
    sid: &str,
) {
    send_muji_session_initiate_request(client, room, sid).await;

    let response = client
        .recv_matching(|frame| {
            frame.contains(&format!("id='muji-join-{sid}'"))
                || frame.contains(&format!("id=\"muji-join-{sid}\""))
        })
        .await
        .expect("Muji session-initiate forbidden response");
    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "non-occupant Muji join must return an IQ error: {response}"
    );
    assert!(
        response.contains("<forbidden"),
        "non-occupant Muji join must be forbidden: {response}"
    );
    assert!(
        !response.contains("session-accept")
            && !response.contains("urn:waddle:transports:livekit:0")
            && !response.contains("token="),
        "forbidden Muji join must not mint or return LiveKit credentials: {response}"
    );

    let late_accept = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        client
            .recv_matching(|frame| {
                frame.contains("session-accept")
                    && (frame.contains(&format!("sid='{sid}'"))
                        || frame.contains(&format!("sid=\"{sid}\""))
                        || frame.contains("urn:waddle:transports:livekit:0")
                        || frame.contains("token="))
            })
            .await
    })
    .await;
    if let Ok(Ok(frame)) = late_accept {
        panic!(
            "forbidden Muji join must not be followed by a token-bearing session-accept: {frame}"
        );
    }
}

async fn recv_until_muji_and_anchor(client: &mut WsXmppClient) -> Vec<String> {
    let mut frames = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let frame = client
                .recv()
                .await
                .expect("receive frame while waiting for muji reflection and call-thread anchor");
            frames.push(frame);
            let has_muji = frames.iter().any(|frame| {
                frame.contains("<presence")
                    && (frame.contains("<muji ") || frame.contains("<muji>"))
            });
            let has_anchor = frames
                .iter()
                .any(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD));
            if has_muji && has_anchor {
                return frames;
            }
        }
    })
    .await
    .expect("muji reflection and call-thread anchor arrive before timeout")
}

async fn query_room_mam(client: &mut WsXmppClient, room: &str, id: &str) -> Vec<String> {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{room}">
                 <query xmlns="urn:xmpp:mam:2">
                   <set xmlns="http://jabber.org/protocol/rsm"><max>50</max></set>
                 </query>
               </iq>"#
        ))
        .await
        .expect("send room MAM query");
    client
        .recv_until(|frame| frame.contains("urn:xmpp:mam:2") && frame.contains("<fin"))
        .await
        .expect("MAM query completes")
}

/// XEP-0272 §Joining + XEP-0045 §7.1: when a multi-session occupant
/// (the user's own bare JID joined twice with two resources) updates
/// presence with a `<muji/>` content advertisement, the reflection
/// MUST reach the *sibling* WebSocket session, not just the sending
/// one. This is what powers the cross-instance "call ongoing"
/// indicator: a user starting a call on their desktop client should
/// see the chip light up in their phone client without any extra
/// round-trip.
///
/// Regression test for the bug where bare-JID `is_self` equality was
/// used to pick the delivery channel — pushing reflections to *every*
/// same-bare recipient onto the sender's `responses` vec and never
/// dispatching them to the sibling's WebSocket via the connection
/// registry.
#[tokio::test]
async fn muji_presence_reflects_to_senders_sibling_resource() {
    let server = TestServer::start_with_extra_envs(&[], &livekit_test_envs());
    let admin_pass = server.fixed_account_password().to_string();

    // Two WebSocket sessions for the SAME bare JID — "admin" with the
    // server-owner localpart so the instant-room join below succeeds.
    let mut web =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "admin", &admin_pass, "muji-web")
            .await
            .expect("admin/web connects");
    let mut mobile = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "muji-mobile",
    )
    .await
    .expect("admin/mobile connects");

    // Use a UUID prefix that does NOT contain the substring "muji" —
    // recv_matching's predicate filters by `<muji` literally below,
    // and a room-JID-embedded "muji" substring would collide with it.
    let room = format!("sibling-call-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let nick = "admin";

    // Web joins first (instant room creation, admin is owner). Then
    // mobile joins the same room with the same nick — XEP-0045 §7.2
    // multi-session join: shared occupant entry, two underlying full
    // JIDs in `occupant_sessions`.
    muji_join_room(&mut web, &room, nick).await;
    muji_join_room(&mut mobile, &room, nick).await;

    // Web emits a XEP-0272 §Joining content presence (active call).
    // Use a minimal `<muji>` body that the typed extractor accepts:
    // one `<content>` with an RTP `<description media='audio'/>`.
    let muji_active = format!(
        r#"<presence to="{room}/{nick}">
             <x xmlns="{NS_MUC}"/>
             <muji xmlns="{NS_MUJI}">
               <content creator="initiator" name="audio">
                 <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
               </content>
             </muji>
           </presence>"#
    );
    web.send(&muji_active)
        .await
        .expect("web sends active muji presence");

    // Mobile MUST receive a reflected presence — from the room/nick,
    // carrying the `<muji>` child with `<content>` intact, plus
    // XEP-0045 §7.1 `<status code='110'/>` because mobile shares the
    // sender's bare JID and is therefore a "self" session for the
    // status-code stamping purpose.
    // Match on the `<muji` element start tag, NOT the substring "muji"
    // which could collide with the room JID. Both open-tag forms
    // (with attributes / namespace) are accepted.
    let active_frames = recv_until_muji_and_anchor(&mut mobile).await;
    let muji_frame_index = active_frames
        .iter()
        .position(|frame| {
            frame.contains("<presence") && (frame.contains("<muji ") || frame.contains("<muji>"))
        })
        .expect("muji frame index");
    let anchor_frame_index = active_frames
        .iter()
        .position(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD))
        .expect("anchor frame index");
    assert!(
        muji_frame_index < anchor_frame_index,
        "active Muji presence should be queued before the call-thread anchor: {active_frames:?}"
    );
    let mobile_active = active_frames
        .iter()
        .find(|frame| {
            frame.contains("<presence") && (frame.contains("<muji ") || frame.contains("<muji>"))
        })
        .unwrap_or_else(|| {
            panic!(
                "mobile must receive reflected muji presence on its own WebSocket: {active_frames:?}"
            )
        });
    let element = parse_presence(mobile_active);
    assert_eq!(
        element.name(),
        "presence",
        "expected <presence>: {mobile_active}"
    );
    assert_eq!(
        element.attr("from"),
        Some(format!("{room}/{nick}").as_str()),
        "reflection must be from room/nick: {mobile_active}"
    );
    let muji = muji_child(&element)
        .unwrap_or_else(|| panic!("mobile reflection must carry <muji/>: {mobile_active}"));
    assert!(
        muji.children().any(|c| c.name() == "content"),
        "mobile reflection must preserve the <content/> child (active call shape): {mobile_active}"
    );
    assert!(
        muc_user_has_status_110(&element),
        "XEP-0045 §7.1: sibling session of the sender must receive <status code='110'/>: {mobile_active}"
    );

    let anchor = active_frames
        .iter()
        .find(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD))
        .expect("mobile receives call-thread anchor");
    let anchor_element: Element = anchor
        .parse()
        .unwrap_or_else(|err| panic!("anchor must parse as XML: {err}; frame={anchor}"));
    assert_eq!(
        anchor_element.name(),
        "message",
        "expected message: {anchor}"
    );
    assert_eq!(
        anchor_element.attr("type"),
        Some("groupchat"),
        "anchor must be groupchat: {anchor}"
    );
    assert_eq!(
        anchor_element.attr("from"),
        Some(room.as_str()),
        "anchor must be authored by the room bare JID: {anchor}"
    );
    assert!(
        anchor_element
            .get_child("thread", "jabber:client")
            .is_some()
            || anchor_element.get_child("thread", "").is_some(),
        "anchor must carry XEP-0201 <thread/>: {anchor}"
    );
    let marker = call_thread_child(&anchor_element)
        .unwrap_or_else(|| panic!("anchor must carry typed call-thread marker: {anchor}"));
    assert_eq!(marker.attr("kind"), Some("muc"));
    assert_eq!(marker.attr("media"), Some("audio"));
    assert_eq!(marker.attr("initiator"), Some("admin@localhost"));
    assert!(
        marker.attr("sid").is_some(),
        "marker must carry session id: {anchor}"
    );
    assert!(
        marker.attr("started").is_some(),
        "marker must carry RFC3339 started timestamp: {anchor}"
    );
    assert!(
        anchor_element
            .get_child("store", "urn:xmpp:hints")
            .is_some(),
        "anchor must carry XEP-0334 <store/> hint: {anchor}"
    );

    web.send(&muji_active)
        .await
        .expect("web repeats active muji presence");
    mobile
        .recv_matching(|frame| {
            frame.contains("<presence") && (frame.contains("<muji ") || frame.contains("<muji>"))
        })
        .await
        .expect("mobile receives repeated active muji reflection");

    let mam_frames = query_room_mam(&mut mobile, &room, "call-thread-mam-1").await;
    let archived_anchors: Vec<&String> = mam_frames
        .iter()
        .filter(|frame| frame.contains("<forwarded") && frame.contains("<call-thread"))
        .collect();
    assert_eq!(
        archived_anchors.len(),
        1,
        "MAM must archive exactly one anchor per active call session: {mam_frames:?}"
    );
    let archived_anchor = archived_anchors[0];
    assert!(
        archived_anchor.contains("<thread>") && archived_anchor.contains("</thread>"),
        "MAM anchor must round-trip XEP-0201 <thread/>: {archived_anchor}"
    );
    assert!(
        archived_anchor.contains("<store xmlns='urn:xmpp:hints'/>")
            || archived_anchor.contains("<store xmlns=\"urn:xmpp:hints\"/>"),
        "MAM anchor must round-trip <store/> hint: {archived_anchor}"
    );

    // Web emits the XEP-0272 §Leaving marker — an empty `<muji/>`
    // element. The room actor clears the call state; the reflection
    // strips the `<muji/>` child, so the mobile sibling sees a plain
    // available presence (which the chat-side store reads as "no
    // active muji" and clears the indicator).
    let muji_leave = format!(
        r#"<presence to="{room}/{nick}">
             <x xmlns="{NS_MUC}"/>
             <muji xmlns="{NS_MUJI}"/>
           </presence>"#
    );
    web.send(&muji_leave)
        .await
        .expect("web sends empty muji leave marker");

    // Mobile MUST receive a follow-up presence from room/nick WITHOUT
    // the `<muji/>` child. The match predicate is narrow on purpose:
    // there may be unrelated frames buffered (caps, etc.); we want
    // the next presence FROM room/nick that has no `<muji/>` element.
    let from_attr_single = format!("from='{room}/{nick}'");
    let from_attr_double = format!("from=\"{room}/{nick}\"");
    let mobile_leave = mobile
        .recv_matching(|frame| {
            // Skip the active-muji frame we already consumed and the
            // initial join echoes; we only want a presence whose XML
            // body has no `<muji` element. Accept both attribute
            // quoting styles to stay robust against a future
            // minidom/rxml serializer swap (other tests in this file
            // already guard both forms).
            frame.contains("<presence")
                && (frame.contains(&from_attr_single) || frame.contains(&from_attr_double))
                && !frame.contains("<muji")
        })
        .await
        .expect("mobile receives leave reflection without <muji/>");
    let leave_element = parse_presence(&mobile_leave);
    assert!(
        muji_child(&leave_element).is_none(),
        "XEP-0272 §Leaving wire shape: muji child must be stripped: {mobile_leave}"
    );
    assert!(
        muc_user_has_status_110(&leave_element),
        "XEP-0045 §7.1: sibling session still gets <status code='110'/> on a self-driven update: {mobile_leave}"
    );
}

#[tokio::test]
async fn muji_session_initiate_from_non_occupant_is_forbidden() {
    let server = TestServer::start_with_extra_envs(&[(BOB, BOB_PW)], &livekit_test_envs());
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin_web = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "gate-admin",
    )
    .await
    .expect("admin connects");
    let mut bob = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, BOB, BOB_PW, "gate-bob")
        .await
        .expect("bob connects");
    let mut admin_mobile = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "gate-observer",
    )
    .await
    .expect("admin observer connects");
    let room = format!("forbidden-call-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    muji_join_room(&mut admin_web, &room, "admin").await;
    muji_join_room(&mut admin_mobile, &room, "admin").await;

    let active = Element::builder("presence", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            format!("{room}/admin"),
        )
        .append(Element::builder("x", NS_MUC).build())
        .append(
            Element::builder("muji", NS_MUJI)
                .append(
                    Element::builder("content", NS_MUJI)
                        .attr(
                            minidom::rxml::xml_ncname!("creator").to_owned(),
                            "initiator",
                        )
                        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "audio")
                        .append(
                            Element::builder("description", "urn:xmpp:jingle:apps:rtp:1")
                                .attr(minidom::rxml::xml_ncname!("media").to_owned(), "audio")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let active = element_to_xml(active);
    admin_web
        .send(&active)
        .await
        .expect("admin/web sends active muji");
    let active_frames = recv_until_muji_and_anchor(&mut admin_mobile).await;
    assert!(
        active_frames
            .iter()
            .any(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD)),
        "observer must receive a call-thread anchor before participant checks: {active_frames:?}"
    );

    send_muji_session_initiate_expect_forbidden(&mut bob, &room, "bob-forbidden").await;
    send_muji_session_initiate(&mut admin_web, &room, "admin-allowed").await;

    let admin_web_full_jid = admin_web.full_jid.clone().expect("admin/web has full jid");
    post_participant_left_webhook(&server, &room, &admin_web_full_jid).await;
    admin_mobile
        .recv_matching(|frame| frame.contains("<call-thread-ended") && frame.contains("<apply-to"))
        .await
        .expect("observer receives ended fastening when the only accepted participant leaves");
}

#[tokio::test]
async fn group_dm_muji_call_lifecycle_uses_existing_muc_gate() {
    let server = TestServer::start_with_extra_envs(
        &[(BOB, BOB_PW), (CAROL, CAROL_PW)],
        &livekit_test_envs(),
    );
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin_web = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "group-dm-call-web",
    )
    .await
    .expect("admin connects");
    let mut bob =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, BOB, BOB_PW, "group-dm-call-bob")
            .await
            .expect("bob connects");
    let mut carol = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        CAROL,
        CAROL_PW,
        "group-dm-call-carol",
    )
    .await
    .expect("carol connects");

    let room = create_group_dm(
        &mut admin_web,
        "group-dm-call-create",
        "Launch crew",
        &["admin@localhost", "bob@localhost"],
    )
    .await;
    muji_join_room(&mut admin_web, &room, "admin").await;
    muji_join_room(&mut bob, &room, "bob").await;

    let active = Element::builder("presence", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            format!("{room}/admin"),
        )
        .append(Element::builder("x", NS_MUC).build())
        .append(
            Element::builder("muji", NS_MUJI)
                .append(
                    Element::builder("content", NS_MUJI)
                        .attr(
                            minidom::rxml::xml_ncname!("creator").to_owned(),
                            "initiator",
                        )
                        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "audio")
                        .append(
                            Element::builder("description", "urn:xmpp:jingle:apps:rtp:1")
                                .attr(minidom::rxml::xml_ncname!("media").to_owned(), "audio")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    admin_web
        .send(&element_to_xml(active))
        .await
        .expect("admin sends active group-DM Muji");

    let bob_frames = recv_until_muji_and_anchor(&mut bob).await;
    assert!(
        bob_frames.iter().any(|frame| {
            frame.contains("<presence")
                && (frame.contains("<muji ") || frame.contains("<muji>"))
                && frame.contains(&format!("{room}/admin"))
        }),
        "group-DM member must see active-call Muji presence: {bob_frames:?}"
    );
    assert!(
        bob_frames
            .iter()
            .any(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD)),
        "group-DM member must receive the call-thread anchor: {bob_frames:?}"
    );

    let carol_join = try_join_room(&mut carol, &room, "carol").await;
    assert!(
        is_error(&carol_join) && carol_join.contains("registration-required"),
        "non-member must not be able to enter members-only group-DM room: {carol_join}"
    );
    send_muji_session_initiate_expect_forbidden(&mut carol, &room, "carol-forbidden").await;
    send_muji_session_initiate(&mut admin_web, &room, "admin-group-dm-allowed").await;
    send_muji_session_initiate(&mut bob, &room, "bob-group-dm-allowed").await;

    let admin_full_jid = admin_web.full_jid.clone().expect("admin full jid");
    post_participant_left_webhook(&server, &room, &admin_full_jid).await;
    recv_muji_leave_reflection(&mut bob, &room, "admin").await;
    assert_no_call_thread_ended(
        &mut bob,
        "first group-DM participant left while bob remained active",
    )
    .await;

    send_muji_session_initiate(&mut admin_web, &room, "admin-group-dm-rejoin").await;

    let bob_full_jid = bob.full_jid.clone().expect("bob full jid");
    post_participant_left_webhook(&server, &room, &bob_full_jid).await;
    recv_muji_leave_reflection(&mut admin_web, &room, "bob").await;
    assert_no_call_thread_ended(
        &mut admin_web,
        "bob left while rejoined admin remained active",
    )
    .await;

    post_participant_left_webhook(&server, &room, &admin_full_jid).await;
    admin_web
        .recv_matching(|frame| frame.contains("<call-thread-ended") && frame.contains("<apply-to"))
        .await
        .expect("group-DM call ends cleanly when the last participant leaves");
}

#[tokio::test]
async fn muji_session_initiate_is_gated_by_requesting_full_jid() {
    let server = TestServer::start_with_extra_envs(&[], &livekit_test_envs());
    let admin_pass = server.fixed_account_password().to_string();
    let mut web =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "admin", &admin_pass, "gate-web")
            .await
            .expect("admin/web connects");
    let mut mobile = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "gate-mobile",
    )
    .await
    .expect("admin/mobile connects");
    let room = format!("resource-gated-call-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    muji_join_room(&mut web, &room, "admin").await;

    send_muji_session_initiate_expect_forbidden(&mut mobile, &room, "mobile-forbidden").await;
    send_muji_session_initiate(&mut web, &room, "web-allowed").await;
}

#[tokio::test]
async fn livekit_last_participant_left_fastens_ended_summary_to_call_thread_anchor() {
    let server = TestServer::start_with_extra_envs(&[], &livekit_test_envs());
    let admin_pass = server.fixed_account_password().to_string();
    let mut web =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "admin", &admin_pass, "end-web")
            .await
            .expect("admin/web connects");
    let mut mobile = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "end-mobile",
    )
    .await
    .expect("admin/mobile connects");

    let room = format!("ended-call-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let nick = "admin";
    muji_join_room(&mut web, &room, nick).await;
    muji_join_room(&mut mobile, &room, nick).await;

    let active = format!(
        r#"<presence to="{room}/{nick}">
             <x xmlns="{NS_MUC}"/>
             <muji xmlns="{NS_MUJI}">
               <content creator="initiator" name="audio">
                 <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
               </content>
             </muji>
           </presence>"#
    );
    web.send(&active).await.expect("web sends active muji");

    let active_frames = recv_until_muji_and_anchor(&mut mobile).await;
    let anchor = active_frames
        .iter()
        .find(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD))
        .expect("mobile receives call-thread anchor");
    let anchor_element: Element = anchor
        .parse()
        .unwrap_or_else(|err| panic!("anchor must parse as XML: {err}; frame={anchor}"));
    let anchor_stanza_id = room_stanza_id(&anchor_element, &room)
        .unwrap_or_else(|| panic!("anchor must carry room-assigned XEP-0359 stanza-id: {anchor}"));
    assert!(!anchor_stanza_id.is_empty());
    let anchor_origin_id = origin_id(&anchor_element)
        .unwrap_or_else(|| panic!("anchor must carry origin-id: {anchor}"));

    mobile
        .send(&active)
        .await
        .expect("mobile also sends active muji");
    web.recv_matching(|frame| {
        frame.contains("<presence") && (frame.contains("<muji ") || frame.contains("<muji>"))
    })
    .await
    .expect("web receives mobile active muji reflection");
    send_muji_session_initiate(&mut web, &room, "web-ended").await;
    send_muji_session_initiate(&mut mobile, &room, "mobile-ended").await;

    let web_full_jid = web.full_jid.clone().expect("web has full jid");
    post_participant_left_webhook(&server, &room, &web_full_jid).await;
    let no_early_ended = tokio::time::timeout(std::time::Duration::from_millis(750), async {
        mobile
            .recv_matching(|frame| {
                frame.contains("<call-thread-ended") && frame.contains("<apply-to")
            })
            .await
    })
    .await;
    assert!(
        no_early_ended.is_err(),
        "first participant leaving must not emit call-thread-ended while another SFU participant remains"
    );

    let mobile_full_jid = mobile.full_jid.clone().expect("mobile has full jid");
    post_participant_left_webhook(&server, &room, &mobile_full_jid).await;

    let ended_frame = web
        .recv_matching(|frame| frame.contains("<call-thread-ended") && frame.contains("<apply-to"))
        .await
        .expect("remaining room occupant receives call-thread-ended fastening");
    let ended_message: Element = ended_frame
        .parse()
        .unwrap_or_else(|err| panic!("ended frame must parse as XML: {err}; frame={ended_frame}"));
    assert_eq!(ended_message.name(), "message");
    assert_eq!(ended_message.attr("type"), Some("groupchat"));
    assert_eq!(ended_message.attr("from"), Some(room.as_str()));
    let apply_to = apply_to_child(&ended_message)
        .unwrap_or_else(|| panic!("ended message must carry apply-to: {ended_frame}"));
    assert_eq!(apply_to.attr("id"), Some(anchor_origin_id.as_str()));
    let ended = call_thread_ended_child(apply_to)
        .unwrap_or_else(|| panic!("apply-to must carry call-thread-ended: {ended_frame}"));
    assert!(
        ended.attr("ended").is_some(),
        "ended marker must carry RFC3339 ended timestamp: {ended_frame}"
    );
    assert!(ended
        .attr("duration")
        .is_some_and(|value| value.starts_with("PT")));
    assert!(
        ended_message.get_child("store", "urn:xmpp:hints").is_some(),
        "ended fastening must carry XEP-0334 <store/> hint: {ended_frame}"
    );

    let mam_frames = query_room_mam(&mut mobile, &room, "call-thread-ended-mam-1").await;
    let archived_ended: Vec<&String> = mam_frames
        .iter()
        .filter(|frame| frame.contains("<forwarded") && frame.contains("<call-thread-ended"))
        .collect();
    assert_eq!(
        archived_ended.len(),
        1,
        "MAM must archive exactly one ended fastening: {mam_frames:?}"
    );
    assert!(
        archived_ended[0].contains(&format!("id='{anchor_origin_id}'"))
            || archived_ended[0].contains(&format!("id=\"{anchor_origin_id}\"")),
        "archived ended fastening must target the anchor origin id: {}",
        archived_ended[0]
    );
}

// ── XEP-0045 role → LiveKit media grants, end to end ──────────────────────

/// The `video` grant of a LiveKit join JWT, decoded with the test
/// deployment's signing secret.
#[derive(serde::Deserialize)]
struct E2eVideoGrant {
    #[serde(rename = "canPublish")]
    can_publish: bool,
    #[serde(rename = "canSubscribe")]
    can_subscribe: bool,
    #[serde(rename = "canPublishData")]
    can_publish_data: bool,
}

#[derive(serde::Deserialize)]
struct E2eClaims {
    video: E2eVideoGrant,
}

/// Pull the `<token/>` out of an issued Waddle LiveKit transport and
/// decode its grants.
fn decode_issued_grant(frame: &str) -> E2eVideoGrant {
    let elem: Element = frame.parse().expect("frame parses as XML");
    let jingle = elem
        .children()
        .find(|child| child.name() == "jingle")
        .expect("iq carries <jingle/>");
    let content = jingle
        .children()
        .find(|child| child.name() == "content")
        .expect("jingle carries <content/>");
    let transport = content
        .children()
        .find(|child| child.name() == "transport")
        .expect("content carries <transport/>");
    let token = transport
        .children()
        .find(|child| child.name() == "token")
        .expect("issued transport carries <token/>")
        .text();

    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_required_spec_claims::<&str>(&[]);
    validation.validate_exp = false;
    let key = jsonwebtoken::DecodingKey::from_secret(
        "test-secret-with-at-least-32-bytes-of-payload".as_bytes(),
    );
    jsonwebtoken::decode::<E2eClaims>(&token, &key, &validation)
        .expect("issued token decodes with the deployment signing secret")
        .claims
        .video
}

/// Send a Muji `session-initiate` for `room` and return the focus's
/// `session-accept` frame carrying the issued transport.
async fn muji_session_initiate(client: &mut WsXmppClient, room: &str, sid: &str) -> String {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="mji-{sid}" to="calls.localhost">
                 <jingle xmlns="urn:xmpp:jingle:1" action="session-initiate" sid="{sid}">
                   <content creator="initiator" name="audio">
                     <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio">
                       <payload-type id="111" name="opus" clockrate="48000" channels="2"/>
                     </description>
                     <transport xmlns="urn:waddle:transports:livekit:0"/>
                   </content>
                   <muji xmlns="{NS_MUJI}" room="{room}"/>
                 </jingle>
               </iq>"#
        ))
        .await
        .expect("muji session-initiate");
    client
        .recv_matching(|frame| frame.contains("session-accept") && frame.contains("<token"))
        .await
        .expect("focus replies with a session-accept carrying an issued token")
}

/// End-to-end pin for the websocket gate → sans-I/O mint seam: a
/// voiced occupant's Muji join must mint a token that may publish.
/// This is the ONLY test that exercises the real
/// `handlers/iq/sans_io.rs` wiring which carries the gate's derived
/// capabilities into the mint — unit tests either call the gate
/// directly or build `StanzaContext` by hand.
#[tokio::test]
async fn muji_join_by_voiced_occupant_mints_publishing_token_end_to_end() {
    let server = TestServer::start_with_extra_envs(&[], &livekit_test_envs());
    let admin_pass = server.fixed_account_password().to_string();
    // The server-owner localpart is required for instant-room creation.
    let mut owner = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "grants-owner",
    )
    .await
    .expect("owner connects");
    let room = format!("grants-voiced-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    muji_join_room(&mut owner, &room, "admin").await;

    let accept = muji_session_initiate(&mut owner, &room, "grant-e2e-voiced").await;
    let grant = decode_issued_grant(&accept);

    assert!(
        grant.can_publish,
        "an occupant with voice must be able to publish: {accept}"
    );
    assert!(grant.can_subscribe);
    assert!(grant.can_publish_data);
}

/// End-to-end pin for the security property: an occupant who has been
/// devoiced to `visitor` in a moderated room joins the call as a
/// listener — the SFU token itself forbids publishing, so no client
/// cooperation is required.
#[tokio::test]
async fn muji_join_by_devoiced_visitor_mints_listen_only_token_end_to_end() {
    let server = TestServer::start_with_extra_envs(&[(BOB, BOB_PW)], &livekit_test_envs());
    let admin_pass = server.fixed_account_password().to_string();
    // The owner creates the room (and is therefore its moderator);
    // bob joins as a regular occupant and is then devoiced.
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_pass,
        "grants-owner",
    )
    .await
    .expect("owner connects");
    let mut bob = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, BOB, BOB_PW, "bx")
        .await
        .expect("bob connects");
    let room = format!("grants-devoiced-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    muji_join_room(&mut alice, &room, "admin").await;
    muji_join_room(&mut bob, &room, "bob").await;

    // Make the room moderated so the visitor role actually withholds
    // voice (XEP-0045 §Terminology), then devoice bob.
    alice
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="cfg-moderated" to="{room}">
                 <query xmlns="http://jabber.org/protocol/muc#owner">
                   <x xmlns="{NS_DATA}" type="submit">
                     <field var="FORM_TYPE"><value>http://jabber.org/protocol/muc#roomconfig</value></field>
                     <field var="muc#roomconfig_moderatedroom"><value>1</value></field>
                   </x>
                 </query>
               </iq>"#
        ))
        .await
        .expect("owner config submit");
    let _ = alice
        .recv_matching(|frame| frame.contains("id='cfg-moderated'"))
        .await
        .expect("room config result");

    alice
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="devoice-bob" to="{room}">
                 <query xmlns="http://jabber.org/protocol/muc#admin">
                   <item nick="bob" role="visitor"/>
                 </query>
               </iq>"#
        ))
        .await
        .expect("devoice bob");
    let _ = alice
        .recv_matching(|frame| frame.contains("id='devoice-bob'"))
        .await
        .expect("devoice result");

    let accept = muji_session_initiate(&mut bob, &room, "grant-e2e-visitor").await;
    let grant = decode_issued_grant(&accept);

    assert!(
        !grant.can_publish,
        "a devoiced visitor must NOT receive publish rights: {accept}"
    );
    assert!(
        !grant.can_publish_data,
        "a devoiced visitor must NOT receive data-publish rights: {accept}"
    );
    assert!(
        grant.can_subscribe,
        "a visitor may still listen and watch: {accept}"
    );
}
