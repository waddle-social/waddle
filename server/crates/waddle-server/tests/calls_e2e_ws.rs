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

mod ws_common;

use ws_common::{disco_info_query, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";
const NS_CALL_THREAD: &str = "urn:waddle:call-thread:0";

const DOMAIN: &str = "localhost";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const ALICE_PW: &str = "alice-pw-12345";
const BOB_PW: &str = "bob-pw-12345";

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

async fn recv_until_muji_and_anchor(client: &mut WsXmppClient) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        let frame = client
            .recv()
            .await
            .expect("receive frame while waiting for muji reflection and call-thread anchor");
        frames.push(frame);
        let has_muji = frames.iter().any(|frame| {
            frame.contains("<presence") && (frame.contains("<muji ") || frame.contains("<muji>"))
        });
        let has_anchor = frames
            .iter()
            .any(|frame| frame.contains("<call-thread") && frame.contains(NS_CALL_THREAD));
        if has_muji && has_anchor {
            return frames;
        }
    }
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
