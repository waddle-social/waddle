//! XEP-0308: Last Message Correction — dedicated conformance suite.
//!
//! Pins the audit-level invariants at the public-API boundary:
//!
//! - §6 namespace string `urn:xmpp:message-correct:0`,
//! - §"Discovering Support" advertisement on `server_features()`
//!   and every MUC room configuration,
//! - §4 wire shape: `<replace id='ORIGINAL_ID' xmlns='…'/>` with
//!   `id` REQUIRED,
//! - §3 "the correction message MUST have a new unique id and a
//!   <body/>" — the helper `build_correction_message` upholds both
//!   invariants,
//! - parser robustness: missing `id`, empty `id`, wrong-ns
//!   payloads, and an attacker stacking multiple `<replace/>`
//!   elements,
//! - §3 "last wins" semantics for the mutator: `set_correction`
//!   replaces any prior `<replace/>`, never appends.

use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::xep0308::{
    build_correction_message, build_replace_element, extract_correction_from_message,
    extract_replaces_id, is_correction_message, is_replace_element, parse_correction_from_message,
    set_correction, strip_correction, Correction, CorrectionCarrier, CorrectionError,
    NS_MESSAGE_CORRECT,
};
use xmpp_parsers::message::Message;

// ── §6 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0308_namespace_matches_spec() {
    // §6 pins the namespace URI exactly. Clients dispatch on it
    // to merge corrections back onto the original message in
    // their timeline; a typo silently drops every correction.
    assert_eq!(NS_MESSAGE_CORRECT, "urn:xmpp:message-correct:0");
}

// ── §"Discovering Support" advertisement ────────────────────────────

#[test]
fn xep0308_server_features_advertise_correction() {
    let feats = server_features();
    let target = Feature::message_correction();
    assert_eq!(target.0, NS_MESSAGE_CORRECT);
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:xmpp:message-correct:0`"
    );
}

#[test]
fn xep0308_muc_rooms_advertise_correction_in_every_configuration() {
    let target = Feature::message_correction();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         must advertise `urn:xmpp:message-correct:0`"
                    );
                }
            }
        }
    }
}

// ── §4 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0308_classifier_accepts_spec_shape_only() {
    let canonical = build_replace_element("msg-1");
    assert!(is_replace_element(&canonical));

    let wrong_ns = Element::builder("replace", "wrong:ns")
        .attr("id", "msg-1")
        .build();
    assert!(!is_replace_element(&wrong_ns));

    let wrong_name = Element::builder("correct", NS_MESSAGE_CORRECT)
        .attr("id", "msg-1")
        .build();
    assert!(!is_replace_element(&wrong_name));
}

#[test]
fn xep0308_build_replace_element_pins_namespace_and_id() {
    // §4 example: `<replace xmlns='urn:xmpp:message-correct:0'
    // id='orig-msg-id'/>`. The element is a leaf with a single
    // required `id` attribute.
    let elem = build_replace_element("origin-id-1");
    assert_eq!(elem.name(), "replace");
    assert_eq!(elem.ns(), NS_MESSAGE_CORRECT);
    assert_eq!(elem.attr("id"), Some("origin-id-1"));
    assert_eq!(
        elem.children().count(),
        0,
        "<replace/> is a leaf element per §4"
    );
}

// ── §3 correction-message helper ─────────────────────────────────────

#[test]
fn xep0308_build_correction_message_assigns_new_unique_id() {
    // §3 mandates: "The correction message MUST have a new unique
    // id." A correction that reused the original id would be
    // ambiguous to MAM consumers and to clients with messages
    // keyed by stanza id.
    let to: jid::Jid = "lord@capulet.example".parse().expect("jid");
    let from: jid::Jid = "juliet@example.com/web".parse().expect("jid");

    let first = build_correction_message(Some(to.clone()), Some(from.clone()), "fixed", "orig-1");
    let second = build_correction_message(Some(to), Some(from), "fixed again", "orig-1");

    let first_id = first.id.as_deref().expect("correction has its own id");
    let second_id = second.id.as_deref().expect("correction has its own id");
    assert_ne!(
        first_id, second_id,
        "consecutive corrections MUST get distinct ids (§3 \"new unique id\")"
    );
    assert_ne!(
        first_id, "orig-1",
        "correction's own id MUST NOT reuse the corrected message's id"
    );
}

#[test]
fn xep0308_build_correction_message_carries_body_and_replace_reference() {
    // §3: "The correction message MUST … have a `<body/>`."
    // Without a body, the correction would be indistinguishable
    // from a state-only update for clients that key off body
    // presence (most do).
    let msg = build_correction_message(
        Some("lord@capulet.example".parse::<jid::Jid>().expect("jid")),
        Some("juliet@example.com/web".parse::<jid::Jid>().expect("jid")),
        "I corrected this text",
        "original-msg-1",
    );

    assert!(
        is_correction_message(&msg),
        "built correction must classify as one"
    );
    let body = msg
        .bodies
        .get("")
        .map(|b| b.0.as_str())
        .expect("body present");
    assert_eq!(body, "I corrected this text");

    let reference = extract_correction_from_message(&msg).expect("correction reference extracts");
    assert_eq!(reference.replaces_id, "original-msg-1");
}

// ── §"Last Wins" mutator semantics ──────────────────────────────────

#[test]
fn xep0308_set_correction_replaces_prior_replace_payload() {
    // §3 / §4: there's exactly one `<replace/>` per correction
    // message. The mutator MUST replace, never append; otherwise
    // a consumer would have to pick which prior id is "the real"
    // one when multiple `<replace/>` payloads stack up.
    let mut msg = Message::new(None::<jid::Jid>);
    set_correction(&mut msg, "first-target");
    set_correction(&mut msg, "second-target");

    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| is_replace_element(e))
            .count(),
        1,
        "exactly one <replace/> payload survives the second set"
    );
    let extracted = extract_correction_from_message(&msg).expect("present");
    assert_eq!(extracted.replaces_id, "second-target");
}

#[test]
fn xep0308_strip_correction_removes_every_namespaced_payload() {
    // Defence: even if a prior bug let multiple `<replace/>`
    // elements through, `strip_correction` MUST clear them all.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(build_replace_element("a"));
    msg.payloads.push(build_replace_element("b"));
    strip_correction(&mut msg);
    assert!(!is_correction_message(&msg));
    assert!(extract_correction_from_message(&msg).is_none());
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0308_parse_returns_ok_none_when_no_replace_payload() {
    // No correction in the message — `parse_correction_from_message`
    // distinguishes "no correction" (Ok(None)) from "malformed
    // correction" (Err). Consumers MUST treat the two differently:
    // a malformed correction surfaces as a protocol error, an
    // absent one is just an ordinary message.
    let msg = Message::new(None::<jid::Jid>);
    let result = parse_correction_from_message(&msg).expect("ok");
    assert!(result.is_none());
}

#[test]
fn xep0308_parse_returns_err_when_replace_id_missing() {
    // §4 makes `id` REQUIRED on `<replace/>`. Without it the
    // consumer has no key to merge against; the parser MUST
    // surface MissingId as an error rather than silently
    // ignoring the payload.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads
        .push(Element::builder("replace", NS_MESSAGE_CORRECT).build());
    let err = parse_correction_from_message(&msg).expect_err("MissingId");
    assert!(matches!(err, CorrectionError::MissingId));
}

#[test]
fn xep0308_parse_returns_err_when_replace_id_empty() {
    // `id=""` is just as broken as no id — a phantom target.
    // Treat it as malformed for the same reason as missing.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder("replace", NS_MESSAGE_CORRECT)
            .attr("id", "")
            .build(),
    );
    let err = parse_correction_from_message(&msg).expect_err("MissingId");
    assert!(matches!(err, CorrectionError::MissingId));
}

#[test]
fn xep0308_extract_returns_none_for_malformed_payload() {
    // The lenient `extract_*` helpers swallow the parse error
    // (they're for "best-effort surface a usable correction" use
    // cases — e.g. inbox preview). `parse_correction_from_message`
    // is the strict version for protocol-error reporting.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads
        .push(Element::builder("replace", NS_MESSAGE_CORRECT).build());
    assert!(extract_correction_from_message(&msg).is_none());
    assert!(extract_replaces_id(&msg).is_none());
}

// ── Carrier-trait surface ───────────────────────────────────────────

#[test]
fn xep0308_carrier_trait_surfaces_correction_and_replaces_id() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_correction(&mut msg, "target-msg");

    assert!(msg.is_correction());
    assert_eq!(
        msg.replaces_id().as_deref(),
        Some("target-msg"),
        "trait shortcut MUST match the typed extractor"
    );
    let typed = msg.correction().expect("typed value");
    assert_eq!(typed, Correction::new("target-msg"));
}
