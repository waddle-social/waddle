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

use chrono::Duration;
use minidom::Element;
use std::sync::Arc;
use waddle_sfu::{
    ApiKey, ApiSecret, CallId, Identity, LiveKitSfu, SfuConfig, SfuService, TurnHost,
    TurnSharedSecret, WebsocketUrl,
};
use waddle_xmpp::protocol::event::{OutboundEvent, StanzaContext};
use waddle_xmpp::protocol::handlers::jingle::{calls_mixer_jid, JingleHandler};
use waddle_xmpp::protocol::traits::IqHandler;
use waddle_xmpp::xep::xep0167::{opus_audio_description, MediaKind};
use waddle_xmpp::xep::xep0272::{find_muji, Creator, Muji, MujiContent, MujiParseError, NS_MUJI};
use waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::{
    Action, Content, ContentId, Creator as JingleCreator, Jingle, SessionId, Transport,
};
use xmpp_parsers::stanza_error::DefinedCondition;

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
fn rtp_description_emits_canonical_opus_payload_type_for_audio() {
    // XEP-0167 §3.1 / XEP-0272 §Joining examples include
    // `<payload-type/>` children. Waddle emits the canonical
    // LiveKit-supported Opus codec (PT 111, 48 kHz, 2 channels)
    // so a strict Muji peer reading the presence sees a valid
    // codec offer. LiveKit still dictates the actual encoding
    // negotiation below the XMPP signaling layer.
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
    let payload = desc
        .children()
        .find(|c| c.name() == "payload-type")
        .expect("description must advertise at least one payload-type per XEP-0167 §3.1 examples");
    assert_eq!(payload.attr("name"), Some("opus"));
    assert_eq!(payload.attr("id"), Some("111"));
    assert_eq!(payload.attr("clockrate"), Some("48000"));
    assert_eq!(payload.attr("channels"), Some("2"));
}

#[test]
fn rtp_description_emits_canonical_vp8_payload_type_for_video() {
    let muji = Muji::with_contents(vec![MujiContent::new(
        "video",
        Creator::Initiator,
        MediaKind::Video,
    )]);
    let elem = muji.to_element();
    let content = elem
        .children()
        .find(|c| c.name() == "content")
        .expect("content present");
    let desc = content
        .children()
        .find(|c| c.name() == "description")
        .expect("description present");
    let payload = desc
        .children()
        .find(|c| c.name() == "payload-type")
        .expect("video description must advertise VP8 payload-type");
    assert_eq!(payload.attr("name"), Some("VP8"));
    assert_eq!(payload.attr("id"), Some("96"));
    assert_eq!(payload.attr("clockrate"), Some("90000"));
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

// ── JingleHandler integration: Muji entry path through the federation guard ─

fn fixture_sfu() -> Arc<dyn SfuService> {
    let cfg = SfuConfig {
        api_key: ApiKey::new("APIxxxxxxxx"),
        api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
        turn_host: TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    };
    Arc::new(LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
}

const TEST_DOMAIN: &str = "waddle.test";
const TEST_INITIATOR: &str = "alice@waddle.test/desktop";

fn test_full_jid() -> jid::FullJid {
    TEST_INITIATOR.parse().unwrap()
}

fn test_occupant_session() -> OccupancySessionGeneration {
    static SESSION: std::sync::OnceLock<OccupancySessionGeneration> = std::sync::OnceLock::new();
    *SESSION.get_or_init(OccupancySessionGeneration::mint)
}

fn ctx<'a>(jid: &'a jid::FullJid) -> StanzaContext<'a> {
    // Mirrors the grants the websocket layer's Muji gate derives for
    // a voiced (role ≥ participant) occupant.
    StanzaContext {
        domain: TEST_DOMAIN,
        full_jid: jid,
        occupant_session: Some(test_occupant_session()),
        media_capabilities: Some(waddle_sfu::MediaCapabilities::from_muc_voice(
            waddle_xmpp_core::types::Voice::Voiced,
        )),
    }
}

/// Build a Muji-bearing Jingle session-initiate IQ. The wire shape
/// matches XEP-0272 §Joining: a standard `<jingle action='session-
/// initiate'>` carrying a `<content/>` plus a `<muji room='…'/>`
/// sibling that pins it to a specific MUC room.
fn muji_session_initiate_iq(initiator: &str, responder: &str, room: &str, sid: &str) -> Iq {
    let mut content = Content::new(JingleCreator::Initiator, ContentId("audio".into()));
    content.description = Some(xmpp_parsers::jingle::Description::Rtp(
        opus_audio_description(),
    ));
    content.transport = Some(Transport::Unknown(
        WaddleLiveKitTransport::Request.to_element(),
    ));
    let mut jingle = Jingle::new(Action::SessionInitiate, SessionId(sid.into()));
    jingle.initiator = Some(initiator.parse().unwrap());
    jingle.contents.push(content);
    let muji = Muji {
        room: Some(room.parse().expect("test room is a valid bare JID")),
        preparing: false,
        contents: vec![MujiContent::new(
            "audio",
            Creator::Initiator,
            MediaKind::Audio,
        )],
    };
    // xmpp_parsers' Jingle struct exposes no API for arbitrary child
    // elements, so we serialise to a minidom Element and graft the
    // <muji/> child in. This produces the exact wire shape the chat
    // client emits via the wasm bridge.
    let mut jingle_elem: Element = jingle.into();
    jingle_elem.append_child(muji.to_element());
    Iq::Set {
        from: Some(initiator.parse().unwrap()),
        to: Some(responder.parse().unwrap()),
        id: format!("muji-{sid}"),
        payload: jingle_elem,
    }
}

fn muji_session_terminate_iq(initiator: &str, responder: &str, room: &str, sid: &str) -> Iq {
    let mut jingle = Jingle::new(Action::SessionTerminate, SessionId(sid.into()));
    jingle.initiator = Some(initiator.parse().unwrap());
    // XEP-0272 only mandates the `<muji room='…'/>` marker; no
    // contents required for terminate.
    let muji = Muji {
        room: Some(room.parse().expect("test room is a valid bare JID")),
        preparing: false,
        contents: vec![],
    };
    let mut jingle_elem: Element = jingle.into();
    jingle_elem.append_child(muji.to_element());
    Iq::Set {
        from: Some(initiator.parse().unwrap()),
        to: Some(responder.parse().unwrap()),
        id: format!("muji-term-{sid}"),
        payload: jingle_elem,
    }
}

fn has_feature_not_implemented(events: &[OutboundEvent]) -> bool {
    events.iter().any(|ev| match ev {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => matches!(
                reply.as_ref(),
                Iq::Error { error, .. }
                    if error.defined_condition == DefinedCondition::FeatureNotImplemented
            ),
            _ => false,
        },
        _ => false,
    })
}

fn first_error_condition(events: &[OutboundEvent]) -> Option<DefinedCondition> {
    events.iter().find_map(|ev| {
        let OutboundEvent::SendStanza(stanza) = ev else {
            return None;
        };
        let Stanza::Iq(reply) = stanza.as_ref() else {
            return None;
        };
        let Iq::Error { error, .. } = reply.as_ref() else {
            return None;
        };
        Some(error.defined_condition.clone())
    })
}

fn first_error_application_condition(events: &[OutboundEvent]) -> Option<&Element> {
    events.iter().find_map(|ev| {
        let OutboundEvent::SendStanza(stanza) = ev else {
            return None;
        };
        let Stanza::Iq(reply) = stanza.as_ref() else {
            return None;
        };
        let Iq::Error { error, .. } = reply.as_ref() else {
            return None;
        };
        error.other.as_ref()
    })
}

fn has_session_accept_to(events: &[OutboundEvent], expected_to: &str) -> bool {
    session_accept_payload_to(events, expected_to).is_some()
}

fn session_accept_payload_to<'a>(
    events: &'a [OutboundEvent],
    expected_to: &str,
) -> Option<&'a Element> {
    events.iter().find_map(|ev| {
        let OutboundEvent::SendStanza(stanza) = ev else {
            return None;
        };
        let Stanza::Iq(boxed) = stanza.as_ref() else {
            return None;
        };
        let Iq::Set { payload, to, .. } = boxed.as_ref() else {
            return None;
        };
        (to.as_ref().is_some_and(|j| j.to_string() == expected_to)
            && payload.name() == "jingle"
            && payload.attr("action") == Some("session-accept"))
        .then_some(payload)
    })
}

#[test]
fn local_muji_mixer_jid_passes_federation_guard() {
    // XEP-0272 §Joining: a Jingle session-initiate addressed to the
    // local SFU mixer (`calls.<server-domain>`, Waddle's synthetic
    // component JID for disambiguating Muji from P2P on the
    // dispatcher) must reach `handle_muji_session_initiate`. The
    // server is the conference focus and replies with a
    // server-initiated session-accept per XEP-0166 §6.3 (IQ-result
    // ack on the initiate IQ, separate `<iq type='set'>` carrying
    // the session-accept).
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-1",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(&iq, &ctx(&jid));

    assert!(
        !has_feature_not_implemented(&events),
        "local mixer must not be misclassified as federation: {events:?}",
    );
    assert!(
        has_session_accept_to(&events, TEST_INITIATOR),
        "expected Muji session-accept addressed to initiator: {events:?}",
    );
}

#[test]
fn local_muji_session_accept_uses_focus_responder_without_initiator_attr() {
    // XEP-0166 only recommends echoing `initiator` on actions sent
    // by the responder. The Muji focus is the authenticated sender
    // of the server-initiated session-accept, so the conformant and
    // least ambiguous identity signal is `responder='calls.<domain>'`.
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-focus-responder",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(&iq, &ctx(&jid));
    let accept = session_accept_payload_to(&events, TEST_INITIATOR)
        .unwrap_or_else(|| panic!("expected Muji session-accept: {events:?}"));

    assert_eq!(
        accept.attr("responder"),
        Some("calls.waddle.test"),
        "Muji session-accept must identify the conference focus as responder",
    );
    assert_eq!(
        accept.attr("initiator"),
        None,
        "Muji session-accept should not copy the client's initiator attr",
    );
}

#[test]
fn local_muji_mixer_jid_with_resource_passes_federation_guard() {
    // The mixer JID's bare form is the policy gate. A `to=` with a
    // resource (`calls.<domain>/anything`) must still be accepted —
    // both `is_local_jingle_peer` and the downstream Muji-handler
    // mixer check use `.to_bare()` to compare, so the resource is
    // semantically irrelevant.
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        "calls.waddle.test/focus",
        "room@muc.waddle.test",
        "muji-resourced",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(&iq, &ctx(&jid));

    assert!(
        !has_feature_not_implemented(&events),
        "mixer JID with resource must be treated as the bare mixer: {events:?}",
    );
    assert!(
        has_session_accept_to(&events, TEST_INITIATOR),
        "expected Muji session-accept: {events:?}",
    );
}

#[test]
fn foreign_mixer_jid_is_rejected_as_federation() {
    // The local-mixer carve-out is scoped to `ctx.domain`. A
    // `calls.<other-host>` JID is a remote-server mixer and must
    // still be rejected by the federation guard — otherwise a local
    // user could mint LiveKit tokens scoped to an attacker-supplied
    // SFU room id by addressing a foreign-looking mixer.
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        "calls.other.test",
        "room@muc.other.test",
        "muji-foreign",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(&iq, &ctx(&jid));

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::FeatureNotImplemented),
        "foreign-domain mixer JID must remain rejected: {events:?}",
    );
}

#[test]
fn muji_with_foreign_room_jid_is_rejected_as_bad_request() {
    // Payload gate (distinct from the peer-JID federation guard):
    // even when the IQ is correctly addressed to the LOCAL mixer,
    // the `<muji room='…'/>` JID must reference a room on this
    // server's MUC service (`muc.<ctx.domain>`). Otherwise a local
    // user could pollute our LiveKit namespace with arbitrary
    // attacker-supplied call ids and register themselves into a
    // participant registry keyed by a foreign room JID.
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.other.test",
        "muji-foreign-room",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(&iq, &ctx(&jid));

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::BadRequest),
        "Muji with non-local room must be rejected with bad-request: {events:?}",
    );
}

#[test]
fn muji_session_terminate_to_local_mixer_passes_federation_guard() {
    // Session-terminate on the Muji branch must traverse the same
    // relaxed federation guard. The handler responds with an
    // IQ-result per XEP-0166 §6.7 — not an error.
    let iq = muji_session_terminate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-1",
    );
    let jid = test_full_jid();
    let sfu = fixture_sfu();
    let call_id = CallId::new("room@muc.waddle.test").expect("valid room call id");
    sfu.register_call_participant(&call_id, &waddle_sfu::Identity::from_jid(jid.clone()));
    let handler = JingleHandler::new(sfu);
    let events = handler.handle(&iq, &ctx(&jid));

    assert!(
        !has_feature_not_implemented(&events),
        "session-terminate to local mixer must not trip federation guard: {events:?}",
    );
    // The terminate path emits an IQ-result ack; assert at least one
    // SendStanza that is NOT an error reply.
    let saw_non_error_reply = events.iter().any(|ev| {
        let OutboundEvent::SendStanza(stanza) = ev else {
            return false;
        };
        let Stanza::Iq(reply) = stanza.as_ref() else {
            return false;
        };
        !matches!(reply.as_ref(), Iq::Error { .. })
    });
    assert!(
        saw_non_error_reply,
        "expected non-error reply to session-terminate: {events:?}",
    );
}

// ── Admin-evict bridge (XMPP teardown → LiveKit RemoveParticipant) ───────

/// Recording admin client: captures every admin invocation so the
/// test can assert the XMPP teardown path correctly mirrors itself
/// onto LiveKit's REST surface. The production
/// [`waddle_sfu::LiveKitSfu::new`] uses a reqwest-backed client;
/// tests swap that out via [`waddle_sfu::LiveKitSfu::with_admin`].
#[derive(Default)]
struct RecordingAdmin {
    remove_calls: std::sync::Mutex<Vec<(waddle_sfu::CallId, waddle_sfu::Identity)>>,
    delete_calls: std::sync::Mutex<Vec<waddle_sfu::CallId>>,
}

impl RecordingAdmin {
    fn remove_snapshot(&self) -> Vec<(waddle_sfu::CallId, waddle_sfu::Identity)> {
        self.remove_calls.lock().expect("recording lock").clone()
    }

    fn delete_snapshot(&self) -> Vec<waddle_sfu::CallId> {
        self.delete_calls.lock().expect("recording lock").clone()
    }
}

impl waddle_sfu::LiveKitAdmin for RecordingAdmin {
    fn list_rooms(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Vec<waddle_sfu::ListedRoom>, waddle_sfu::SfuError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn remove_participant<'a>(
        &'a self,
        room: &'a waddle_sfu::CallId,
        identity: &'a waddle_sfu::Identity,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), waddle_sfu::SfuError>> + Send + 'a>,
    > {
        let room = room.clone();
        let identity = identity.clone();
        Box::pin(async move {
            self.remove_calls
                .lock()
                .expect("recording lock")
                .push((room, identity));
            Ok(())
        })
    }

    fn delete_room<'a>(
        &'a self,
        room: &'a waddle_sfu::CallId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), waddle_sfu::SfuError>> + Send + 'a>,
    > {
        let room = room.clone();
        Box::pin(async move {
            self.delete_calls.lock().expect("recording lock").push(room);
            Ok(())
        })
    }

    fn update_participant<'a>(
        &'a self,
        _room: &'a waddle_sfu::CallId,
        _identity: &'a waddle_sfu::Identity,
        _capabilities: waddle_sfu::MediaCapabilities,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), waddle_sfu::SfuError>> + Send + 'a>,
    > {
        // These Muji teardown tests don't exercise mid-call grant
        // updates; accept and ignore.
        Box::pin(async move { Ok(()) })
    }

    fn room_occupancy<'a>(
        &'a self,
        _room: &'a waddle_sfu::CallId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<waddle_sfu::RoomOccupancy, waddle_sfu::SfuError>,
                > + Send
                + 'a,
        >,
    > {
        // These Muji teardown tests don't exercise the reconciliation
        // backstop; report an empty room.
        Box::pin(async move { Ok(waddle_sfu::RoomOccupancy::default()) })
    }
}

fn fixture_sfu_with_admin(admin: Arc<RecordingAdmin>) -> Arc<dyn SfuService> {
    let cfg = SfuConfig {
        api_key: ApiKey::new("APIxxxxxxxx"),
        api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
        turn_host: TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    };
    Arc::new(LiveKitSfu::with_admin(
        cfg,
        admin as Arc<dyn waddle_sfu::LiveKitAdmin>,
    ))
}

async fn drain_spawned_admin_tasks() {
    // The spawn from `LiveKitSfu::unregister_call_participant` lands
    // on the current tokio runtime; a handful of cooperative yields
    // is more than enough to let the recording mock complete its
    // mutex pushes.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn muji_session_terminate_evicts_participant_via_livekit_admin() {
    // XMPP teardown must mirror onto LiveKit's REST admin surface,
    // not rely on the SFU's DTLS read-timeout to detect the dead
    // PeerConnection. Without this, kubectl logs show "dtls timeout"
    // warnings on every hangup and the room object lingers until
    // `empty_timeout` (LK default 5 min) — visible in production as
    // a stuck call indicator in the chat UI.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = fixture_sfu_with_admin(Arc::clone(&admin));

    // Pre-register so the call is non-empty at terminate time — the
    // sequence under test is "join, then terminate", which is what
    // the chat client does via session-initiate + session-terminate.
    let room_jid_str = "room@muc.waddle.test";
    let call_id = CallId::new(room_jid_str).expect("valid call id");
    let initiator = test_full_jid();
    sfu.register_call_participant(&call_id, &waddle_sfu::Identity::from_jid(initiator.clone()));

    let iq = muji_session_terminate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-1",
    );
    let handler = JingleHandler::new(sfu);
    let _events = handler.handle(&iq, &ctx(&initiator));
    drain_spawned_admin_tasks().await;

    let removes = admin.remove_snapshot();
    assert_eq!(
        removes.len(),
        1,
        "session-terminate must trigger exactly one LiveKit RemoveParticipant; got {removes:?}",
    );
    assert_eq!(removes[0].0.as_str(), room_jid_str);
    assert_eq!(
        removes[0].1.as_livekit_identity(),
        TEST_INITIATOR,
        "RemoveParticipant identity must match the Jingle session-terminate sender",
    );

    // The initiator was the only registered participant, so the call
    // transitions to `CallState::Ended` and DeleteRoom should also
    // fire — collapsing LK's empty-room TTL window to zero.
    let deletes = admin.delete_snapshot();
    assert_eq!(
        deletes.len(),
        1,
        "last-participant terminate must also trigger DeleteRoom; got {deletes:?}",
    );
    assert_eq!(deletes[0].as_str(), room_jid_str);
}

#[tokio::test]
async fn muji_session_terminate_skips_delete_room_when_call_still_has_participants() {
    // Bob's terminate while alice is still in the call must NOT
    // trigger DeleteRoom — only the per-participant evict.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = fixture_sfu_with_admin(Arc::clone(&admin));

    let room_jid_str = "room@muc.waddle.test";
    let call_id = CallId::new(room_jid_str).expect("valid call id");
    let alice = test_full_jid();
    let bob: jid::FullJid = "bob@waddle.test/desktop".parse().unwrap();
    sfu.register_call_participant(&call_id, &waddle_sfu::Identity::from_jid(alice.clone()));
    sfu.register_call_participant(&call_id, &waddle_sfu::Identity::from_jid(bob.clone()));

    let iq = muji_session_terminate_iq(
        &bob.to_string(),
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-2",
    );
    let handler = JingleHandler::new(sfu);
    let _events = handler.handle(&iq, &ctx(&bob));
    drain_spawned_admin_tasks().await;

    let removes = admin.remove_snapshot();
    assert_eq!(removes.len(), 1);
    assert_eq!(removes[0].1.as_livekit_identity(), bob.to_string());

    let deletes = admin.delete_snapshot();
    assert!(
        deletes.is_empty(),
        "DeleteRoom must not fire while the call still has participants; got {deletes:?}",
    );
}

#[tokio::test]
async fn muji_unknown_participant_terminate_returns_unknown_session_without_admin_calls() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = fixture_sfu_with_admin(Arc::clone(&admin));
    let room_jid_str = "room@muc.waddle.test";
    let call_id = CallId::new(room_jid_str).expect("valid call id");
    let alice = test_full_jid();
    let bob: jid::FullJid = "bob@waddle.test/desktop".parse().unwrap();

    sfu.register_call_participant(&call_id, &waddle_sfu::Identity::from_jid(alice.clone()));

    let iq = muji_session_terminate_iq(
        &bob.to_string(),
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-unknown-terminate",
    );
    let handler = JingleHandler::new(sfu);
    let events = handler.handle(&iq, &ctx(&bob));
    drain_spawned_admin_tasks().await;

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::ItemNotFound),
        "unknown Muji terminator must get item-not-found/unknown-session: {events:?}",
    );
    assert!(
        first_error_application_condition(&events).is_some_and(|condition| {
            condition.name() == "unknown-session"
                && condition.ns() == waddle_xmpp::xep::xep0166::NS_JINGLE_ERRORS
        }),
        "unknown Muji terminate must include the XEP-0166 unknown-session application condition"
    );
    assert!(
        admin.remove_snapshot().is_empty(),
        "unknown Muji terminator must not schedule RemoveParticipant",
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "unknown Muji terminator must not schedule DeleteRoom",
    );
}

// ── Role-derived media grants in the minted LiveKit token ──────────

/// Decoded `video` grant claim of a minted LiveKit join JWT.
#[derive(serde::Deserialize)]
struct DecodedVideoGrant {
    #[serde(rename = "canPublish")]
    can_publish: bool,
    #[serde(rename = "canSubscribe")]
    can_subscribe: bool,
    #[serde(rename = "canPublishData")]
    can_publish_data: bool,
}

#[derive(serde::Deserialize)]
struct DecodedClaims {
    video: DecodedVideoGrant,
}

/// Extract the issued LiveKit token from the Muji session-accept and
/// decode its `video` grant with the fixture signing secret.
fn accepted_video_grant(events: &[OutboundEvent]) -> DecodedVideoGrant {
    let accept = session_accept_payload_to(events, TEST_INITIATOR)
        .unwrap_or_else(|| panic!("expected Muji session-accept: {events:?}"));
    let jingle = Jingle::try_from(accept.clone()).expect("session-accept parses as jingle");
    let content = jingle.contents.first().expect("accept carries content");
    let Some(Transport::Unknown(elem)) = &content.transport else {
        panic!("content missing Waddle transport");
    };
    let WaddleLiveKitTransport::Issued(issued) =
        WaddleLiveKitTransport::try_from(elem).expect("transport parses")
    else {
        panic!("server must issue the transport");
    };
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_required_spec_claims::<&str>(&[]);
    let key = jsonwebtoken::DecodingKey::from_secret(b"super-secret-secret-32-bytes-min");
    jsonwebtoken::decode::<DecodedClaims>(issued.token.as_str(), &key, &validation)
        .expect("issued token decodes with fixture secret")
        .claims
        .video
}

fn ctx_with_caps<'a>(
    jid: &'a jid::FullJid,
    caps: Option<waddle_sfu::MediaCapabilities>,
) -> StanzaContext<'a> {
    StanzaContext {
        domain: TEST_DOMAIN,
        full_jid: jid,
        occupant_session: Some(test_occupant_session()),
        media_capabilities: caps,
    }
}

fn ctx_with_caps_and_session<'a>(
    jid: &'a jid::FullJid,
    caps: Option<waddle_sfu::MediaCapabilities>,
    occupant_session: Option<OccupancySessionGeneration>,
) -> StanzaContext<'a> {
    StanzaContext {
        domain: TEST_DOMAIN,
        full_jid: jid,
        occupant_session,
        media_capabilities: caps,
    }
}

/// XEP-0045 voice → SFU grant conformance: a voiced occupant's
/// authorization (derived by the websocket-layer Muji gate) mints a
/// token that may publish.
#[test]
fn muji_initiate_with_participant_grants_mints_publishing_token() {
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-grants-participant",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(
        &iq,
        &ctx_with_caps(
            &jid,
            Some(waddle_sfu::MediaCapabilities::from_muc_voice(
                waddle_xmpp_core::types::Voice::Voiced,
            )),
        ),
    );

    let grant = accepted_video_grant(&events);
    assert!(grant.can_publish && grant.can_subscribe);
    assert!(!grant.can_publish_data);
}

#[test]
fn muji_initiate_with_context_generation_binds_the_sfu_registration() {
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-occupant-session-bound",
    );
    let jid = test_full_jid();
    let call_id = CallId::new("room@muc.waddle.test").expect("valid room call id");
    let identity = Identity::from_jid(jid.clone());
    let occupant_session = OccupancySessionGeneration::mint();
    let sfu = Arc::new(
        LiveKitSfu::new({
            SfuConfig {
                api_key: ApiKey::new("APIxxxxxxxx"),
                api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                    .expect("test secret meets min length"),
                webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                    .expect("test secret meets min length"),
                ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
                turn_host: TurnHost::new("turn.test"),
                turn_tls_port: 443,
                turn_udp_port: 3478,
                turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
                token_ttl: Duration::seconds(3600),
                turn_ttl: Duration::seconds(3600),
            }
        })
        .expect("LiveKitSfu init in test"),
    );
    let handler = JingleHandler::new(Arc::clone(&sfu) as Arc<dyn SfuService>);

    let events = handler.handle(
        &iq,
        &ctx_with_caps_and_session(
            &jid,
            Some(waddle_sfu::MediaCapabilities::from_muc_voice(
                waddle_xmpp_core::types::Voice::Voiced,
            )),
            Some(occupant_session),
        ),
    );

    assert!(
        first_error_condition(&events).is_none(),
        "a generation-bound Muji initiate must succeed: {events:?}",
    );
    assert_eq!(
        sfu.participant_occupant_session(&call_id, &identity),
        Some(occupant_session)
    );
}

/// A visitor (occupant without voice) gets a listen-only token: the
/// SFU itself rejects publish attempts regardless of client behaviour.
#[test]
fn muji_initiate_with_visitor_grants_mints_listen_only_token() {
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-grants-visitor",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(
        &iq,
        &ctx_with_caps(
            &jid,
            Some(waddle_sfu::MediaCapabilities::from_muc_voice(
                waddle_xmpp_core::types::Voice::Muted,
            )),
        ),
    );

    let grant = accepted_video_grant(&events);
    assert!(grant.can_subscribe, "a visitor may still listen");
    assert!(!grant.can_publish);
    assert!(!grant.can_publish_data);
}

/// Fail closed: a Muji mint reached without gate-derived capabilities
/// must issue NO token at all. A subscribe-only JWT is still a usable
/// credential for the room's media, so an unverified caller must not
/// receive one — the request is refused instead.
#[test]
fn muji_initiate_without_gate_capabilities_is_refused() {
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-grants-ungated",
    );
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let events = handler.handle(&iq, &ctx_with_caps(&jid, None));

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::Forbidden),
        "an ungated Muji join must be forbidden, not granted a listener token: {events:?}",
    );
    assert!(
        session_accept_payload_to(&events, TEST_INITIATOR).is_none(),
        "no session-accept (and therefore no token) may be issued: {events:?}",
    );
}

#[test]
fn muji_initiate_without_context_generation_is_refused_and_does_not_register() {
    let iq = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        "room@muc.waddle.test",
        "muji-occupant-session-missing",
    );
    let jid = test_full_jid();
    let call_id = CallId::new("room@muc.waddle.test").expect("valid room call id");
    let identity = Identity::from_jid(jid.clone());
    let sfu = Arc::new(
        LiveKitSfu::new({
            SfuConfig {
                api_key: ApiKey::new("APIxxxxxxxx"),
                api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                    .expect("test secret meets min length"),
                webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                    .expect("test secret meets min length"),
                ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
                turn_host: TurnHost::new("turn.test"),
                turn_tls_port: 443,
                turn_udp_port: 3478,
                turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
                token_ttl: Duration::seconds(3600),
                turn_ttl: Duration::seconds(3600),
            }
        })
        .expect("LiveKitSfu init in test"),
    );
    let handler = JingleHandler::new(Arc::clone(&sfu) as Arc<dyn SfuService>);

    let events = handler.handle(
        &iq,
        &ctx_with_caps_and_session(
            &jid,
            Some(waddle_sfu::MediaCapabilities::from_muc_voice(
                waddle_xmpp_core::types::Voice::Voiced,
            )),
            None,
        ),
    );

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::InternalServerError),
        "a Muji initiate missing its occupant generation must fail closed: {events:?}",
    );
    assert!(
        session_accept_payload_to(&events, TEST_INITIATOR).is_none(),
        "a failed-closed Muji initiate must not mint a session-accept: {events:?}",
    );
    assert!(
        !sfu.has_call_participant(&call_id, &identity),
        "a failed-closed Muji initiate must not register the participant"
    );
}

// ── #1445: cross-replica routing of Muji signalling ──────────────────

/// The per-JID `session-initiate` rate limit must cover the Muji
/// branch, not just 1:1 Jingle. Before #1445 the Muji branch returned
/// before the limiter ran, so an unlimited stream of Muji initiates
/// could mint JWTs and grow the SFU registry — and since #1445 each
/// one also costs a claim-store lookup and, for a foreign-owned room,
/// a cross-node relay.
#[test]
fn muji_session_initiate_is_rate_limited_per_bare_jid() {
    let jid = test_full_jid();
    let handler = JingleHandler::new(fixture_sfu());
    let mixer = calls_mixer_jid(TEST_DOMAIN).to_string();

    // The limiter's default budget is 5 initiates per 30s; the sixth
    // in the same window must be rejected.
    let mut conditions = Vec::new();
    for attempt in 0..7 {
        let iq = muji_session_initiate_iq(
            TEST_INITIATOR,
            &mixer,
            "room@muc.waddle.test",
            &format!("muji-rate-{attempt}"),
        );
        let events = handler.handle(&iq, &ctx(&jid));
        conditions.push(first_error_condition(&events));
    }

    assert!(
        conditions.contains(&Some(DefinedCondition::PolicyViolation)),
        "a sustained Muji session-initiate burst must hit the rate limit: {conditions:?}",
    );
    assert!(
        conditions[0].is_none(),
        "the first initiate in the window must still succeed: {:?}",
        conditions[0],
    );
}

// ── Session-terminate sid binding (#1608) ─────────────────────────────

#[tokio::test]
async fn muji_session_terminate_with_stale_sid_does_not_tear_down_current_session() {
    // The #1608 race: a late-flushed terminate for an OLD call (old
    // Jingle sid) arrives after the user rejoined the same room with
    // a NEW session. The room-scoped teardown used to end the new
    // call; the sid binding recorded at session-initiate must reject
    // the stale terminate instead.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = fixture_sfu_with_admin(Arc::clone(&admin));
    let room_jid_str = "room@muc.waddle.test";
    let call_id = CallId::new(room_jid_str).expect("valid call id");
    let jid = test_full_jid();

    // Join with the CURRENT session's sid via the real initiate path
    // so the binding is recorded exactly where production records it.
    let initiate = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-current",
    );
    let handler = JingleHandler::new(Arc::clone(&sfu));
    let events = handler.handle(&initiate, &ctx(&jid));
    assert!(
        first_error_condition(&events).is_none(),
        "fixture initiate must succeed: {events:?}",
    );

    let stale_terminate = muji_session_terminate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-stale",
    );
    let events = handler.handle(&stale_terminate, &ctx(&jid));
    drain_spawned_admin_tasks().await;

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::ItemNotFound),
        "stale-sid Muji terminate must get item-not-found/unknown-session: {events:?}",
    );
    assert!(
        first_error_application_condition(&events).is_some_and(|condition| {
            condition.name() == "unknown-session"
                && condition.ns() == waddle_xmpp::xep::xep0166::NS_JINGLE_ERRORS
        }),
        "stale-sid Muji terminate must carry the XEP-0166 unknown-session condition",
    );
    assert!(
        sfu.has_call_participant(&call_id, &waddle_sfu::Identity::from_jid(jid.clone())),
        "stale-sid terminate must not unregister the current session's participant",
    );
    assert!(
        admin.remove_snapshot().is_empty(),
        "stale-sid terminate must not schedule RemoveParticipant",
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "stale-sid terminate must not schedule DeleteRoom",
    );
}

#[tokio::test]
async fn muji_session_terminate_with_matching_sid_tears_down_as_today() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = fixture_sfu_with_admin(Arc::clone(&admin));
    let room_jid_str = "room@muc.waddle.test";
    let call_id = CallId::new(room_jid_str).expect("valid call id");
    let jid = test_full_jid();

    let initiate = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-current",
    );
    let handler = JingleHandler::new(Arc::clone(&sfu));
    let events = handler.handle(&initiate, &ctx(&jid));
    assert!(
        first_error_condition(&events).is_none(),
        "fixture initiate must succeed: {events:?}",
    );

    let terminate = muji_session_terminate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        "muji-current",
    );
    let events = handler.handle(&terminate, &ctx(&jid));
    drain_spawned_admin_tasks().await;

    assert!(
        first_error_condition(&events).is_none(),
        "matching-sid Muji terminate must succeed: {events:?}",
    );
    assert!(
        !sfu.has_call_participant(&call_id, &waddle_sfu::Identity::from_jid(jid.clone())),
        "matching-sid terminate must unregister the participant",
    );
    assert_eq!(admin.remove_snapshot().len(), 1);
}

#[test]
fn muji_session_initiate_with_unbindable_sid_is_rejected_before_registering() {
    // Codex review P2 on #1626: an accepted initiate whose sid cannot
    // be bound (over the 256-byte SessionBinding cap — xmpp_parsers
    // puts no limit on Jingle sids) would leave the registration
    // unbound and thus unprotected by the #1608 stale-terminate gate.
    // Reject the initiate outright instead: no token, no registration.
    let sfu = fixture_sfu();
    let room_jid_str = "room@muc.waddle.test";
    let call_id = CallId::new(room_jid_str).expect("valid call id");
    let jid = test_full_jid();
    let oversized_sid = "s".repeat(300);

    let initiate = muji_session_initiate_iq(
        TEST_INITIATOR,
        &calls_mixer_jid(TEST_DOMAIN).to_string(),
        room_jid_str,
        &oversized_sid,
    );
    let handler = JingleHandler::new(Arc::clone(&sfu));
    let events = handler.handle(&initiate, &ctx(&jid));

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::BadRequest),
        "unbindable-sid Muji initiate must be rejected with bad-request: {events:?}",
    );
    assert!(
        !sfu.has_call_participant(&call_id, &waddle_sfu::Identity::from_jid(jid.clone())),
        "rejected initiate must not register the participant",
    );
    assert!(
        !has_session_accept_to(&events, TEST_INITIATOR),
        "rejected initiate must not mint a session-accept",
    );
}
