//! XEP-0272 Muji FFI test suite: pins the exact stanzas the three
//! group-call verbs emit (presence variants, mixer session-initiate,
//! mixer session-terminate) plus their typed failure behavior — the
//! same fixture style as `calls_tests.rs`.

use jid::FullJid;
use waddle_xmpp_client::messaging::SessionId;

use crate::muji::{
    muji_mixer_jid, muji_presence_stanza, muji_session_initiate_iq, muji_session_terminate_iq,
    WaddleInCallPresenceFlags,
};

const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";
const NS_JINGLE: &str = "urn:xmpp:jingle:1";
const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";
const NS_IN_CALL: &str = "urn:waddle:in-call:0";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";

fn full(value: &str) -> FullJid {
    value.parse().expect("valid full JID")
}

fn sid(value: &str) -> SessionId {
    SessionId(value.to_owned())
}

fn no_flags() -> WaddleInCallPresenceFlags {
    WaddleInCallPresenceFlags {
        hand_raised: false,
        muted: false,
    }
}

#[test]
fn mixer_jid_is_calls_dot_domain() {
    assert_eq!(muji_mixer_jid("waddle.test"), "calls.waddle.test");
}

/// XEP-0272 §Joining phase 1: the `<preparing/>` presence carries no
/// contents and no in-call flags, but always the caps advertisement.
#[test]
fn preparing_presence_carries_only_the_marker() {
    let presence = muji_presence_stanza(
        "room@muc.waddle.test",
        "alice",
        false,
        true,
        false,
        WaddleInCallPresenceFlags {
            hand_raised: true,
            muted: true,
        },
    );
    assert_eq!(presence.name(), "presence");
    assert_eq!(presence.attr("to"), Some("room@muc.waddle.test/alice"));
    let muji = presence
        .get_child("muji", NS_MUJI)
        .expect("muji child present");
    assert!(muji.get_child("preparing", NS_MUJI).is_some());
    assert_eq!(muji.children().filter(|c| c.name() == "content").count(), 0);
    // Flags ride only the active variant here — preparing publishes
    // them too per the shared builder (in_call is meaningful while
    // preparing or active).
    assert!(presence.get_child("c", NS_CAPS).is_some());
}

/// XEP-0272 §Joining phase 2: the active presence declares the audio
/// content always and video only on request; in-call flags ride
/// alongside `<muji/>`, never inside it.
#[test]
fn active_presence_declares_contents_and_flags() {
    let presence = muji_presence_stanza(
        "room@muc.waddle.test",
        "alice",
        true,
        false,
        true,
        WaddleInCallPresenceFlags {
            hand_raised: false,
            muted: true,
        },
    );
    let muji = presence
        .get_child("muji", NS_MUJI)
        .expect("muji child present");
    assert!(muji.get_child("preparing", NS_MUJI).is_none());
    let contents: Vec<_> = muji.children().filter(|c| c.name() == "content").collect();
    assert_eq!(contents.len(), 2, "audio + requested video");
    for content in &contents {
        let desc = content
            .get_child("description", NS_JINGLE_RTP)
            .expect("RTP description");
        assert!(desc.attr("media") == Some("audio") || desc.attr("media") == Some("video"));
    }
    let in_call = presence
        .get_child("in-call", NS_IN_CALL)
        .expect("in-call sibling present");
    assert!(in_call.get_child("muted", NS_IN_CALL).is_some());
    assert!(in_call.get_child("hand-raised", NS_IN_CALL).is_none());
    assert!(muji.get_child("in-call", NS_IN_CALL).is_none());
}

/// Audio-only active presence: exactly one content, no in-call child
/// when neither flag is set.
#[test]
fn audio_only_presence_has_single_content_and_no_flags_child() {
    let presence = muji_presence_stanza(
        "room@muc.waddle.test",
        "alice",
        true,
        false,
        false,
        no_flags(),
    );
    let muji = presence.get_child("muji", NS_MUJI).expect("muji child");
    assert_eq!(muji.children().filter(|c| c.name() == "content").count(), 1);
    assert!(presence.get_child("in-call", NS_IN_CALL).is_none());
}

/// XEP-0272 §Leaving: neither active nor preparing yields the bare
/// presence with NO `<muji/>` child (and no in-call flags), still with
/// caps.
#[test]
fn leave_presence_is_bare() {
    let presence = muji_presence_stanza(
        "room@muc.waddle.test",
        "alice",
        false,
        false,
        false,
        WaddleInCallPresenceFlags {
            hand_raised: true,
            muted: true,
        },
    );
    assert!(presence.get_child("muji", NS_MUJI).is_none());
    assert!(presence.get_child("in-call", NS_IN_CALL).is_none());
    assert!(presence.get_child("c", NS_CAPS).is_some());
}

/// The mixer session-initiate: IQ-set to `calls.<domain>`, `<muji
/// room=…/>` first, contents with the empty LiveKit transport
/// placeholder the server rewrites.
#[test]
fn session_initiate_targets_mixer_with_muji_room_and_livekit_placeholder() {
    let iq = muji_session_initiate_iq(
        "calls.waddle.test",
        &sid("attempt-1"),
        &full("alice@waddle.test/phone"),
        "room@muc.waddle.test",
        false,
    );
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("calls.waddle.test"));
    let jingle = iq.get_child("jingle", NS_JINGLE).expect("jingle child");
    assert_eq!(jingle.attr("action"), Some("session-initiate"));
    assert_eq!(jingle.attr("sid"), Some("attempt-1"));
    assert_eq!(jingle.attr("initiator"), Some("alice@waddle.test/phone"));
    let muji = jingle.get_child("muji", NS_MUJI).expect("muji marker");
    assert_eq!(muji.attr("room"), Some("room@muc.waddle.test"));
    let contents: Vec<_> = jingle
        .children()
        .filter(|c| c.name() == "content")
        .collect();
    assert_eq!(contents.len(), 1, "audio only unless video requested");
    let transport = contents[0]
        .get_child("transport", NS_LIVEKIT_TRANSPORT)
        .expect("LiveKit transport placeholder");
    assert_eq!(
        transport.attrs().into_iter().count(),
        0,
        "placeholder is empty"
    );
}

/// Video request adds the second content, both transport-bearing.
#[test]
fn session_initiate_with_video_adds_second_content() {
    let iq = muji_session_initiate_iq(
        "calls.waddle.test",
        &sid("attempt-2"),
        &full("alice@waddle.test/phone"),
        "room@muc.waddle.test",
        true,
    );
    let jingle = iq.get_child("jingle", NS_JINGLE).expect("jingle child");
    let contents: Vec<_> = jingle
        .children()
        .filter(|c| c.name() == "content")
        .collect();
    assert_eq!(contents.len(), 2);
    assert!(contents
        .iter()
        .all(|c| c.get_child("transport", NS_LIVEKIT_TRANSPORT).is_some()));
}

/// The mixer session-terminate: `<muji room=…/>` +
/// `<reason><success/></reason>` (XEP-0166 §6.7).
#[test]
fn session_terminate_carries_room_and_success_reason() {
    let iq = muji_session_terminate_iq(
        "calls.waddle.test",
        "room@muc.waddle.test",
        &sid("attempt-1"),
    );
    assert_eq!(iq.attr("to"), Some("calls.waddle.test"));
    let jingle = iq.get_child("jingle", NS_JINGLE).expect("jingle child");
    assert_eq!(jingle.attr("action"), Some("session-terminate"));
    assert_eq!(jingle.attr("sid"), Some("attempt-1"));
    let muji = jingle.get_child("muji", NS_MUJI).expect("muji marker");
    assert_eq!(muji.attr("room"), Some("room@muc.waddle.test"));
    let reason = jingle.get_child("reason", NS_JINGLE).expect("reason");
    assert!(reason.get_child("success", NS_JINGLE).is_some());
}
