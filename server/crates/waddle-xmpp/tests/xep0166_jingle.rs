//! XEP-0166: Jingle (core session signaling) — dedicated test suite.
//!
//! Spec: <https://xmpp.org/extensions/xep-0166.html> (v1.1.2, Final).
//!
//! Pins the wire shapes Waddle accepts and emits for the four
//! Jingle actions that drive a LiveKit-backed call:
//!
//! * `session-initiate` — XEP-0166 §6.2
//! * `session-accept`   — XEP-0166 §6.4
//! * `session-terminate` — XEP-0166 §6.7
//! * `transport-info`   — XEP-0166 §6.8 (used by mid-session
//!   negotiation; routed unchanged with a sanitised `from` by the
//!   server)
//!
//! Construction goes through typed Rust values
//! ([`xmpp_parsers::jingle::Jingle`], [`Action`], [`Content`],
//! [`SessionId`]) and [`minidom::Element::builder`] — no `format!`,
//! no string concatenation (CLAUDE.md XML hard rule).

use minidom::Element;
use waddle_xmpp::xep::xep0166::{
    reason_element, session_terminate, Action, Content, ContentId, Creator, Jingle, Reason,
    SessionId, Transport, NS_JINGLE,
};

const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_WADDLE_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";

fn audio_content_with_waddle_transport() -> Content {
    let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
    // Empty description in the RTP namespace — codec list rides
    // session-initiate proper; this is the per-content media tag.
    let description = Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), "audio")
        .build();
    content.description = Some(xmpp_parsers::jingle::Description::Rtp(
        xmpp_parsers::jingle_rtp::Description::try_from(description).expect("rtp desc"),
    ));
    let transport = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build();
    content.transport = Some(Transport::Unknown(transport));
    content
}

fn parse_jingle(xml: &str) -> Jingle {
    let elem: Element = xml.parse().expect("element parses");
    Jingle::try_from(elem).expect("Jingle parses")
}

// ── §3 namespace ──────────────────────────────────────────────────────

#[test]
fn xep_0166_namespace_matches_spec() {
    // XEP-0166 §3: Jingle stanzas are qualified by `urn:xmpp:jingle:1`.
    assert_eq!(NS_JINGLE, "urn:xmpp:jingle:1");
    assert_eq!(NS_JINGLE, xmpp_parsers::ns::JINGLE);
}

// ── §6.2 session-initiate ─────────────────────────────────────────────

#[test]
fn xep_0166_session_initiate_round_trips_through_typed_surface() {
    // §6.2: session-initiate carries `action='session-initiate'`,
    // `initiator='<full-jid>'`, `sid='<opaque>'` and at least one
    // `<content/>` child. Parses through xmpp-parsers' typed
    // `Jingle::try_from` and the action lands as the typed
    // `Action::SessionInitiate` variant.
    let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                         action='session-initiate'
                         initiator='alice@waddle.test/desktop'
                         sid='c1'>
                   <content creator='initiator' name='audio'>
                     <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                     <transport xmlns='urn:waddle:transports:livekit:0'/>
                   </content>
                 </jingle>"#;
    let jingle = parse_jingle(xml);
    assert_eq!(jingle.action, Action::SessionInitiate);
    assert_eq!(jingle.sid.0, "c1");
    assert_eq!(
        jingle.initiator.as_ref().map(|j| j.to_string()),
        Some("alice@waddle.test/desktop".to_string())
    );
    assert_eq!(jingle.contents.len(), 1);
    let content = &jingle.contents[0];
    assert_eq!(content.creator, Creator::Initiator);
    assert_eq!(content.name.0, "audio");
}

#[test]
fn xep_0166_session_initiate_builder_emits_initiator_attr_via_minidom() {
    // Builders MUST use typed `minidom::Element::builder` per the
    // CLAUDE.md XML hard rule. Round-trip a hand-built session-initiate
    // through the typed `Jingle::try_from` pass to prove that the
    // builder-emitted shape parses back into the same typed value.
    let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c2".into()));
    jingle.initiator = Some("alice@waddle.test/desktop".parse().expect("valid jid"));
    jingle.contents.push(audio_content_with_waddle_transport());

    let elem: Element = jingle.into();
    assert_eq!(elem.name(), "jingle");
    assert_eq!(elem.ns(), NS_JINGLE);
    assert_eq!(elem.attr("action"), Some("session-initiate"));
    assert_eq!(elem.attr("sid"), Some("c2"));
    assert_eq!(elem.attr("initiator"), Some("alice@waddle.test/desktop"));

    // Round-trip the serialised form back through the parser.
    let reparsed = Jingle::try_from(elem).expect("reparses");
    assert_eq!(reparsed.action, Action::SessionInitiate);
    assert_eq!(reparsed.sid.0, "c2");
    assert_eq!(reparsed.contents.len(), 1);
}

// ── §6.4 session-accept ───────────────────────────────────────────────

#[test]
fn xep_0166_session_accept_carries_responder_attribute() {
    // §6.4: session-accept identifies the accepting party via
    // `responder='<full-jid>'`. The initiator attribute mirrors the
    // original session-initiate so the server can re-derive the call
    // scope.
    let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                         action='session-accept'
                         initiator='alice@waddle.test/desktop'
                         responder='bob@waddle.test/desktop'
                         sid='c1'>
                   <content creator='initiator' name='audio'>
                     <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                     <transport xmlns='urn:waddle:transports:livekit:0'/>
                   </content>
                 </jingle>"#;
    let jingle = parse_jingle(xml);
    assert_eq!(jingle.action, Action::SessionAccept);
    assert_eq!(
        jingle.responder.as_ref().map(|j| j.to_string()),
        Some("bob@waddle.test/desktop".to_string())
    );
    assert_eq!(
        jingle.initiator.as_ref().map(|j| j.to_string()),
        Some("alice@waddle.test/desktop".to_string())
    );
}

// ── §6.7 session-terminate ────────────────────────────────────────────

#[test]
fn xep_0166_session_terminate_with_success_reason() {
    // §6.7: session-terminate MAY carry a `<reason/>` child taking
    // one of the XEP-0166 §7.4 conditions. The typed
    // `xmpp_parsers::jingle::Reason::Success` flows through the
    // dedicated reason helper rather than `format!`.
    let term = session_terminate(SessionId("c1".into()), Reason::Success);
    let elem: Element = term.into();
    assert_eq!(elem.name(), "jingle");
    assert_eq!(elem.ns(), NS_JINGLE);
    assert_eq!(elem.attr("action"), Some("session-terminate"));
    assert_eq!(elem.attr("sid"), Some("c1"));

    let reason_child = elem
        .children()
        .find(|c| c.name() == "reason")
        .expect("<reason/> child required when reason supplied");
    assert_eq!(reason_child.ns(), NS_JINGLE);
    let condition = reason_child
        .children()
        .next()
        .expect("reason has condition child");
    assert_eq!(condition.name(), "success");
}

#[test]
fn xep_0166_session_terminate_carries_no_content_per_spec() {
    // §6.7: "the <jingle/> element of a session-terminate action
    // MUST NOT contain any <content/> elements." Waddle's typed
    // `session_terminate` helper enforces this by construction —
    // round-trip through the parser confirms it on the wire.
    let term = session_terminate(SessionId("c1".into()), Reason::Success);
    let elem: Element = term.into();
    let contents: Vec<&Element> = elem.children().filter(|c| c.name() == "content").collect();
    assert!(
        contents.is_empty(),
        "session-terminate MUST NOT carry <content/> children"
    );
}

#[test]
fn xep_0166_session_terminate_reason_helper_uses_typed_condition() {
    // The `reason_element` helper builds a typed
    // `xmpp_parsers::jingle::ReasonElement` directly — proving the
    // typed-payloads rule (no `&str` for the condition) — and
    // serialises to the canonical `<reason><$cond/></reason>` shape.
    for (reason, cond_name) in [
        (Reason::Success, "success"),
        (Reason::Decline, "decline"),
        (Reason::Busy, "busy"),
        (Reason::Cancel, "cancel"),
        (Reason::ConnectivityError, "connectivity-error"),
        (Reason::Expired, "expired"),
    ] {
        let mut jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into()));
        jingle = jingle.set_reason(reason_element(reason.clone()));
        let elem: Element = jingle.into();
        let reason_child = elem
            .children()
            .find(|c| c.name() == "reason")
            .expect("reason present");
        let cond = reason_child
            .children()
            .next()
            .expect("reason has condition");
        assert_eq!(
            cond.name(),
            cond_name,
            "reason {reason:?} must serialise to <{cond_name}/>"
        );
    }
}

// ── §6.8 transport-info ───────────────────────────────────────────────

#[test]
fn xep_0166_transport_info_round_trips() {
    // §6.8: transport-info carries the SAME sid as the running session
    // and one `<content/>` per transport-update. Waddle forwards these
    // unchanged with a sanitised `from` — the wire shape MUST survive
    // through the typed `Jingle::try_from` parse.
    let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                         action='transport-info'
                         initiator='alice@waddle.test/desktop'
                         sid='c1'>
                   <content creator='initiator' name='audio'>
                     <transport xmlns='urn:waddle:transports:livekit:0'/>
                   </content>
                 </jingle>"#;
    let jingle = parse_jingle(xml);
    assert_eq!(jingle.action, Action::TransportInfo);
    assert_eq!(jingle.sid.0, "c1");
    assert_eq!(jingle.contents.len(), 1);
}

// ── Negative cases ────────────────────────────────────────────────────

#[test]
fn xep_0166_non_jingle_namespace_is_rejected() {
    // A `<jingle/>` element in a non-Jingle namespace MUST NOT parse
    // as `Jingle`. Waddle's handler relies on this to reject
    // hand-rolled lookalikes that try to bypass the typed contract.
    let elem = Element::builder("jingle", "urn:waddle:not-jingle")
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "c1")
        .build();
    assert!(Jingle::try_from(elem).is_err());
}

#[test]
fn xep_0166_unknown_action_is_rejected() {
    // §7.2: the `action` attribute is constrained to the 14 values
    // enumerated in the spec. xmpp-parsers' typed `Action` enum will
    // refuse to parse anything else.
    let elem = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-warp",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "c1")
        .build();
    assert!(Jingle::try_from(elem).is_err());
}

#[test]
fn xep_0166_session_terminate_typed_helper_round_trips() {
    // The `session_terminate` helper builds via the typed
    // `Jingle::new(...).set_reason(...)` chain. Round-trip the
    // serialised form through the parser to prove no information is
    // lost across the typed/wire boundary.
    let term = session_terminate(SessionId("c-roundtrip".into()), Reason::Cancel);
    let elem: Element = term.into();
    let reparsed = Jingle::try_from(elem).expect("reparses");
    assert_eq!(reparsed.action, Action::SessionTerminate);
    assert_eq!(reparsed.sid.0, "c-roundtrip");
    let reason = reparsed.reason.expect("reason preserved");
    assert!(matches!(reason.reason, Reason::Cancel));
}
