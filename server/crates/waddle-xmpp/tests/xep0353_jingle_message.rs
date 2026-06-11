//! XEP-0353: Jingle Message Initiation (JMI) — dedicated test suite.
//!
//! Spec: <https://xmpp.org/extensions/xep-0353.html> (v0.7.0, Draft).
//!
//! JMI is the pre-call ringing layer: the initiator broadcasts a
//! `<propose/>` to every resource of the responder so any one of them
//! can ring before Jingle session-initiate commits to a single
//! resource. Waddle uses JMI as the canonical 1:1 call ring signal.
//!
//! This file pins the **server-side typed builder library** under
//! [`waddle_xmpp::xep::xep0353`]:
//!
//! * `<propose/>` carries one `<description/>` per offered media type
//!   (XEP-0353 §3) — a single combined description is non-conformant.
//! * `<finish/>` MAY carry a `<reason/>` child taking a typed XEP-0166
//!   Jingle reason (XEP-0353 §3.5 SHOULD).
//! * The wrapping `<message/>` envelope MUST be `type='chat'` and MUST
//!   carry a XEP-0334 `<store/>` hint so MAM archives keep the call
//!   timeline reconstructible even when no body is present (XEP-0353
//!   §3). The envelope is built via the typed
//!   [`xmpp_parsers::message::Message`] surface and the typed
//!   [`MessageType::Chat`] variant — no ad-hoc string attributes.
//!
//! All construction goes through typed Rust values and
//! [`minidom::Element::builder`]; no `format!`, no string
//! concatenation (CLAUDE.md XML hard rule).

use minidom::Element;
use waddle_xmpp::xep::xep0353::{
    build_finish, build_proceed, build_propose, build_reject, build_retract, build_ringing,
    CallOffer, JingleMI, NS_JINGLE_MESSAGE,
};
use xmpp_parsers::jingle::{Reason as JingleReason, SessionId};
use xmpp_parsers::message::{Message, MessageType};

const NS_JINGLE: &str = "urn:xmpp:jingle:1";
const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_HINTS: &str = "urn:xmpp:hints";

fn store_hint_element() -> Element {
    Element::builder("store", NS_HINTS).build()
}

/// XEP-0353 §3 envelope: every JMI body (propose / proceed / reject /
/// retract / finish) MUST ride a `<message type='chat'/>` envelope
/// carrying a XEP-0334 `<store/>` hint. Built via the typed
/// `Message::new_with_type(MessageType::Chat, …)` surface so
/// `type='chat'` flows through the dedicated `MessageType` variant
/// rather than an ad-hoc attribute string.
fn jmi_envelope(to: &jid::Jid, body: Element) -> Element {
    let mut msg = Message::new_with_type(MessageType::Chat, Some(to.clone()));
    msg.payloads.push(store_hint_element());
    msg.payloads.push(body);
    Element::from(msg)
}

// ── §3 namespace ──────────────────────────────────────────────────────

#[test]
fn xep_0353_namespace_matches_spec() {
    assert_eq!(NS_JINGLE_MESSAGE, "urn:xmpp:jingle-message:0");
    assert_eq!(NS_JINGLE_MESSAGE, xmpp_parsers::ns::JINGLE_MESSAGE);
}

// ── §3 propose — one <description/> per offered media kind ───────────

#[test]
fn xep_0353_propose_audio_video_emits_two_descriptions() {
    // §3: "the <propose/> element MUST contain one or more
    // <description/> child elements". Each description names a
    // single media kind; combining audio+video into one description
    // is non-conformant — the responder can't distinguish audio-only
    // from audio+video at ring time.
    let elem = build_propose(SessionId("c1".into()), CallOffer::audio_video());
    assert_eq!(elem.name(), "propose");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert_eq!(elem.attr("id"), Some("c1"));

    let descs: Vec<&Element> = elem
        .children()
        .filter(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
        .collect();
    assert_eq!(
        descs.len(),
        2,
        "audio+video propose carries exactly two descriptions"
    );
    let media: Vec<&str> = descs.iter().filter_map(|c| c.attr("media")).collect();
    assert_eq!(media, vec!["audio", "video"]);
}

#[test]
fn xep_0353_propose_audio_only_carries_single_audio_description() {
    let elem = build_propose(SessionId("c2".into()), CallOffer::audio_only());
    let descs: Vec<&Element> = elem
        .children()
        .filter(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
        .collect();
    assert_eq!(descs.len(), 1, "audio-only carries exactly one description");
    assert_eq!(descs[0].attr("media"), Some("audio"));
    // §3 example: the description is empty at JMI time; codec list
    // rides the subsequent session-initiate (XEP-0167).
    assert!(
        descs[0].children().next().is_none(),
        "JMI <description/> bodies are empty per §3 example"
    );
}

// ── §3.x proceed / reject / retract — typed surface ──────────────────

#[test]
fn xep_0353_proceed_round_trips_through_typed_jingle_mi() {
    // proceed/reject/retract carry no children — xmpp-parsers'
    // typed `JingleMI` enum covers them directly. Round-trip via
    // `Element` to prove the wire shape matches the typed enum.
    let proceed = build_proceed(SessionId("c1".into()));
    let elem: Element = proceed.into();
    assert_eq!(elem.name(), "proceed");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert_eq!(elem.attr("id"), Some("c1"));

    let reparsed = JingleMI::try_from(elem).expect("proceed reparses");
    assert!(matches!(reparsed, JingleMI::Proceed(_)));
}

#[test]
fn xep_0353_ringing_reports_device_ring_state() {
    let elem = build_ringing(SessionId("c1".into()));
    assert_eq!(elem.name(), "ringing");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert_eq!(elem.attr("id"), Some("c1"));
    assert!(elem.children().next().is_none());
}

#[test]
fn xep_0353_reject_round_trips_through_typed_jingle_mi() {
    let elem: Element = build_reject(SessionId("c1".into())).into();
    assert_eq!(elem.name(), "reject");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert!(matches!(
        JingleMI::try_from(elem).expect("reject reparses"),
        JingleMI::Reject(_)
    ));
}

#[test]
fn xep_0353_retract_round_trips_through_typed_jingle_mi() {
    let elem: Element = build_retract(SessionId("c1".into())).into();
    assert_eq!(elem.name(), "retract");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert!(matches!(
        JingleMI::try_from(elem).expect("retract reparses"),
        JingleMI::Retract(_)
    ));
}

// ── §3.5 finish — optional typed reason child ─────────────────────────

#[test]
fn xep_0353_finish_without_reason_emits_no_reason_child() {
    // §3.5: `<finish/>` SHOULD carry a `<reason/>` child, but the
    // SHOULD is informational — omitting it is still valid wire
    // shape. Pin both branches so a future tightening of the SHOULD
    // doesn't silently strip the omit-path.
    let elem = build_finish(SessionId("c1".into()), None);
    assert_eq!(elem.name(), "finish");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert_eq!(elem.attr("id"), Some("c1"));
    assert!(
        elem.children().all(|c| c.name() != "reason"),
        "no reason child when caller passes None"
    );
}

#[test]
fn xep_0353_finish_with_reason_emits_typed_jingle_reason_child() {
    // §3.5: `<finish/>` MAY carry a `<reason/>` element qualified
    // by `urn:xmpp:jingle:1` (NOT `urn:xmpp:jingle-message:0`) and
    // containing a single typed XEP-0166 §7.4 condition. The typed
    // [`xmpp_parsers::jingle::Reason`] enum is the typed-payloads
    // contract — a `&str` condition would be rejected per CLAUDE.md.
    let cases = [
        (JingleReason::Success, "success"),
        (JingleReason::Decline, "decline"),
        (JingleReason::Busy, "busy"),
        (JingleReason::Cancel, "cancel"),
        (JingleReason::ConnectivityError, "connectivity-error"),
        (JingleReason::Expired, "expired"),
    ];
    for (reason, cond_name) in cases {
        let elem = build_finish(SessionId("c1".into()), Some(reason.clone()));
        let reason_child = elem
            .children()
            .find(|c| c.name() == "reason")
            .unwrap_or_else(|| panic!("<reason/> required for reason {reason:?}"));
        // The reason wrapper is XEP-0166 qualified (§3.5 carries the
        // reason "as it is in the Jingle session"), not the JMI
        // namespace. Pin this — it's an easy detail to mis-emit.
        assert_eq!(
            reason_child.ns(),
            NS_JINGLE,
            "<reason/> wrapper is XEP-0166 qualified, not JMI"
        );
        let cond = reason_child
            .children()
            .next()
            .unwrap_or_else(|| panic!("<reason/> has condition child for {reason:?}"));
        assert_eq!(
            cond.name(),
            cond_name,
            "reason {reason:?} serialises to <{cond_name}/>"
        );
    }
}

// ── §3 envelope — type='chat' + XEP-0334 store hint ──────────────────

fn assert_jmi_envelope_conformant(name: &str, body_builder: impl Fn() -> Element) {
    // §3: every JMI body rides a `<message type='chat'/>` envelope
    // carrying a XEP-0334 `<store/>` hint. Without `type='chat'` the
    // stanza ships as `type='normal'` (the XMPP default) and gets
    // routed only to the highest-priority resource — the whole
    // point of JMI is to fork to ALL resources. Without `<store/>`
    // the body doesn't make it into MAM and call history breaks.
    let to: jid::Jid = "bob@waddle.test".parse().expect("valid jid");
    let stanza = jmi_envelope(&to, body_builder());

    assert_eq!(stanza.name(), "message", "{name}: envelope is <message/>");
    assert_eq!(
        stanza.attr("type"),
        Some("chat"),
        "{name}: §3 MUST type='chat'"
    );
    assert_eq!(
        stanza.attr("to"),
        Some("bob@waddle.test"),
        "{name}: 'to' preserved"
    );

    let store = stanza
        .children()
        .find(|c| c.name() == "store" && c.ns() == NS_HINTS)
        .unwrap_or_else(|| panic!("{name}: §3 MUST carry a XEP-0334 <store/> hint"));
    assert!(
        store.children().next().is_none(),
        "{name}: <store/> hint is empty"
    );
    assert!(
        store.attrs().iter().next().is_none(),
        "{name}: <store/> hint carries no attributes"
    );

    // The JMI body rides along — the responder's parser still finds
    // it as a non-hint child in the JMI namespace.
    assert!(
        stanza.children().any(|c| c.ns() == NS_JINGLE_MESSAGE),
        "{name}: JMI body preserved through envelope"
    );
}

#[test]
fn xep_0353_propose_envelope_is_chat_with_store_hint() {
    assert_jmi_envelope_conformant("propose", || {
        build_propose(SessionId("c1".into()), CallOffer::audio_video())
    });
}

#[test]
fn xep_0353_proceed_envelope_is_chat_with_store_hint() {
    assert_jmi_envelope_conformant("proceed", || build_proceed(SessionId("c1".into())).into());
}

#[test]
fn xep_0353_ringing_envelope_is_chat_with_store_hint() {
    assert_jmi_envelope_conformant("ringing", || build_ringing(SessionId("c1".into())));
}

#[test]
fn xep_0353_reject_envelope_is_chat_with_store_hint() {
    assert_jmi_envelope_conformant("reject", || build_reject(SessionId("c1".into())).into());
}

#[test]
fn xep_0353_retract_envelope_is_chat_with_store_hint() {
    assert_jmi_envelope_conformant("retract", || build_retract(SessionId("c1".into())).into());
}

#[test]
fn xep_0353_finish_envelope_is_chat_with_store_hint() {
    assert_jmi_envelope_conformant("finish", || {
        build_finish(SessionId("c1".into()), Some(JingleReason::Success))
    });
}

// ── Negative cases ────────────────────────────────────────────────────

#[test]
fn xep_0353_jingle_mi_rejects_unknown_envelope_element() {
    // A child in the JMI namespace whose name isn't one of the
    // canonical {propose, proceed, reject, retract, finish} MUST
    // NOT parse as `JingleMI`.
    let elem = Element::builder("bogus", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "c1")
        .build();
    assert!(JingleMI::try_from(elem).is_err());
}

#[test]
fn xep_0353_jingle_mi_rejects_wrong_namespace() {
    // A `<proceed/>` element in some other namespace MUST NOT parse
    // as a JMI proceed.
    let elem = Element::builder("proceed", "urn:waddle:not-jmi")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "c1")
        .build();
    assert!(JingleMI::try_from(elem).is_err());
}
