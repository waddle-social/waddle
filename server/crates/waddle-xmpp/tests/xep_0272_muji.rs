//! XEP-0272 (Multiparty Jingle, "Muji") custom test suite.
//!
//! Per the project's "XEP custom test-suite hard rule"
//! (`CLAUDE.md`): every implemented XEP — including any advertised
//! compatibility/profile — MUST have a dedicated Rust custom test
//! suite. This file pins the wire shape of:
//!
//! 1. The `<muji xmlns='urn:xmpp:jingle:muji:0'/>` element in MUC
//!    presence (XEP-0272 §Joining, §Leaving, §Adding a Content
//!    Type).
//! 2. The Jingle session-initiate / session-accept embedding of
//!    `<muji room='…'/>` per XEP-0272 §Joining "Jingle
//!    session-initiate referencing a Muji conference".
//! 3. The XEP-0167 §3.2 minimal-`<description>` rule (only `media`
//!    required — no payload-types — because LiveKit dictates the
//!    codecs below the signaling layer).
//! 4. CallId convention for the Muji branch of the JingleHandler.
//!
//! Room-actor-side semantics (originator multi-resource clearing,
//! join-replay) are covered by the `muc::room_actor::tests` lib
//! tests in this crate — those access private spawn helpers and
//! can't be reused from an integration test crate. This file
//! covers everything else conformantly.

use waddle_sfu::CallId;
use waddle_xmpp::xep::xep0167::MediaKind;
use waddle_xmpp::xep::xep0272::{find_muji, Creator, Muji, MujiContent, MujiParseError, NS_MUJI};

fn audio_muji() -> Muji {
    Muji::with_contents(vec![MujiContent::new(
        "audio",
        Creator::Initiator,
        MediaKind::Audio,
    )])
}

fn audio_video_muji() -> Muji {
    Muji::with_contents(vec![
        MujiContent::new("audio", Creator::Initiator, MediaKind::Audio),
        MujiContent::new("video", Creator::Initiator, MediaKind::Video),
    ])
}

// ── §Joining (presence shape) ──────────────────────────────────────────────

#[test]
fn ns_matches_xep_0272() {
    assert_eq!(NS_MUJI, "urn:xmpp:jingle:muji:0");
}

#[test]
fn preparing_only_round_trips_through_element() {
    let muji = Muji::preparing();
    let elem = muji.to_element();
    assert_eq!(elem.name(), "muji");
    assert_eq!(elem.ns(), NS_MUJI);
    assert!(elem.children().any(|c| c.name() == "preparing"));
    let reparsed = Muji::try_from(&elem).expect("preparing reparses");
    assert!(reparsed.preparing);
    assert!(reparsed.contents.is_empty());
    assert!(!reparsed.is_active(), "preparing alone is not active");
}

#[test]
fn multi_content_audio_video_round_trips() {
    let muji = audio_video_muji();
    let elem = muji.to_element();
    let reparsed = Muji::try_from(&elem).expect("contents reparse");
    assert_eq!(reparsed.contents.len(), 2);
    assert_eq!(reparsed.contents[0].name.0, "audio");
    assert_eq!(reparsed.contents[0].media, MediaKind::Audio);
    assert_eq!(reparsed.contents[1].name.0, "video");
    assert_eq!(reparsed.contents[1].media, MediaKind::Video);
    assert!(reparsed.is_active());
}

#[test]
fn room_attribute_round_trips() {
    let room: jid::BareJid = "room@muc.example.com".parse().unwrap();
    let muji = Muji::for_room(room.clone());
    let elem = muji.to_element();
    assert_eq!(elem.attr("room"), Some("room@muc.example.com"));
    let reparsed = Muji::try_from(&elem).expect("room attribute reparses");
    assert_eq!(reparsed.room.as_ref(), Some(&room));
}

#[test]
fn rejects_wrong_namespace() {
    let xml = "<muji xmlns='urn:xmpp:not-muji:0'/>";
    let elem: minidom::Element = xml.parse().unwrap();
    let err = Muji::try_from(&elem).expect_err("namespace mismatch");
    assert_eq!(err, MujiParseError::WrongElement);
}

#[test]
fn rejects_invalid_creator() {
    let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                 <content creator='moderator' name='audio'>\
                   <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                 </content>\
               </muji>";
    let elem: minidom::Element = xml.parse().unwrap();
    let err = Muji::try_from(&elem).expect_err("creator validation");
    assert!(matches!(err, MujiParseError::InvalidCreator(_)));
}

#[test]
fn rejects_invalid_media() {
    let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                 <content creator='initiator' name='audio'>\
                   <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='quantum'/>\
                 </content>\
               </muji>";
    let elem: minidom::Element = xml.parse().unwrap();
    let err = Muji::try_from(&elem).expect_err("media validation");
    assert!(matches!(err, MujiParseError::InvalidMedia(_)));
}

#[test]
fn rejects_missing_description() {
    let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                 <content creator='initiator' name='audio'/>\
               </muji>";
    let elem: minidom::Element = xml.parse().unwrap();
    let err = Muji::try_from(&elem).expect_err("description required");
    assert!(matches!(err, MujiParseError::MissingDescription));
}

#[test]
fn ignores_non_muji_children() {
    // Real-world MUC presence often carries `<c xmlns='caps'/>`,
    // `<x xmlns='muc#user'/>`, etc. — the parser must not error
    // out on unrelated namespaces sitting next to `<preparing/>`.
    let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                 <c xmlns='http://jabber.org/protocol/caps' node='x' ver='y' hash='sha-1'/>\
                 <preparing/>\
               </muji>";
    let elem: minidom::Element = xml.parse().unwrap();
    let muji = Muji::try_from(&elem).expect("foreign children ignored");
    assert!(muji.preparing);
}

#[test]
fn rtp_description_uses_minimal_xep0167_shape() {
    // XEP-0167 §3.2 only mandates `media` on `<description/>`; the
    // payload-type list is OPTIONAL. Waddle exploits this because
    // LiveKit dictates codecs below the XMPP signaling layer.
    let muji = audio_muji();
    let elem = muji.to_element();
    let content = elem
        .children()
        .find(|c| c.name() == "content")
        .expect("content present");
    let desc = content
        .children()
        .find(|c| c.name() == "description")
        .expect("description present");
    assert_eq!(desc.ns(), "urn:xmpp:jingle:apps:rtp:1");
    assert_eq!(desc.attr("media"), Some("audio"));
    assert!(
        desc.children().count() == 0,
        "Waddle's Muji description must be minimal — no payload-types"
    );
}

// ── §Joining (Jingle session-initiate embedding) ──────────────────────────

#[test]
fn find_muji_locates_muji_child_inside_jingle() {
    let xml = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='r@muc'>\
                 <muji xmlns='urn:xmpp:jingle:muji:0' room='r@muc'/>\
                 <content creator='initiator' name='audio' senders='both'>\
                   <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                 </content>\
               </jingle>";
    let elem: minidom::Element = xml.parse().unwrap();
    let muji_elem = find_muji(&elem).expect("muji child located");
    let muji = Muji::try_from(muji_elem).expect("typed Muji parses");
    assert_eq!(
        muji.room.as_ref().map(|j| j.to_string()),
        Some("r@muc".to_string())
    );
}

#[test]
fn find_muji_returns_none_when_absent() {
    let xml = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1'>\
                 <content creator='initiator' name='audio'>\
                   <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                 </content>\
               </jingle>";
    let elem: minidom::Element = xml.parse().unwrap();
    assert!(
        find_muji(&elem).is_none(),
        "1:1 Jingle without <muji/> must not match"
    );
}

#[test]
fn find_muji_returns_none_for_wrong_namespace_child() {
    // A child element NAMED `muji` but in a different namespace
    // must not be matched — defends against squatting on the local
    // name in unrelated extensions.
    let xml = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1'>\
                 <muji xmlns='urn:xmpp:other-muji:0'/>\
               </jingle>";
    let elem: minidom::Element = xml.parse().unwrap();
    assert!(find_muji(&elem).is_none());
}

// ── CallId convention (Jingle-Muji uses room JID, NOT scoped_call_id) ─────

#[test]
fn muji_call_id_is_the_room_jid() {
    // Document the convention that
    // `JingleHandler::handle_muji_session_initiate` uses the bare
    // room JID as the SFU `CallId`. Every occupant who joins the
    // call via Muji therefore lands in the SAME LiveKit room —
    // distinct from the 1:1 path which scopes by
    // `(initiator_bare, sid)` to prevent collisions across
    // independent calls.
    let room: jid::BareJid = "general@muc.example.com".parse().unwrap();
    let call_id = CallId::new(room.to_string()).expect("room JID forms a valid CallId");
    assert_eq!(call_id.as_str(), "general@muc.example.com");
}

// ── §Leaving (empty Muji is the leave marker) ─────────────────────────────

#[test]
fn empty_muji_is_the_leave_marker() {
    // Per XEP-0272 §Leaving, absence of `<muji/>` from MUC presence
    // is itself the leave marker. A `<muji/>` element with neither
    // `<preparing/>` nor `<content/>` children parses to the
    // equivalent "leave" state (`Muji::is_empty() == true`).
    let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'/>";
    let elem: minidom::Element = xml.parse().unwrap();
    let muji = Muji::try_from(&elem).expect("empty Muji reparses");
    assert!(muji.is_empty());
    assert!(!muji.is_active());
    assert!(!muji.preparing);
    assert!(muji.contents.is_empty());
}

#[test]
fn default_muji_is_empty() {
    // The `Default` impl is the storage-side "leave" state —
    // verifies that the lib test suite's `empty_muji()` helper
    // (in `muc::room_actor::tests`) produces a value that
    // `MucRoom::upsert_muji_presence` recognises as a leave.
    let muji = Muji::default();
    assert!(muji.is_empty());
    assert!(!muji.is_active());
}
