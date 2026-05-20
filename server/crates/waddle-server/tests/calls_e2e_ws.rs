//! XEP-0353 + XEP-0166 + waddle-livekit-transport wire-conformance
//! tests against a live waddle-server with `LIVEKIT_*` envs set.
//!
//! Scope: the JMI ring layer (alice → bob propose/proceed/reject)
//! and the Jingle session-initiate transport-rewrite path. The full
//! engine-level connect-to-LiveKit step is exercised by manual smoke
//! tests in the browser; this suite covers everything the server is
//! responsible for.

mod ws_common;

use ws_common::{disco_info_query, TestServer, WsXmppClient};

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
