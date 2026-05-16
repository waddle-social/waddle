//! XEP-0428: Fallback Indication — dedicated conformance test suite.
//!
//! Spec source: https://xmpp.org/extensions/xep-0428.html (v0.2.1, 2024-03-20).
//!
//! The audit found three gaps that this suite locks down:
//!
//! 1. **`<subject/>` children**. Earlier impl silently ignored fallbacks
//!    that targeted only the subject; spec §2.2 says subjects are valid
//!    fallback regions on equal footing with bodies.
//! 2. **Optional `for=` attribute**. Spec schema marks it optional; a
//!    bare `<fallback xmlns='urn:xmpp:fallback:0'/>` is shorthand for
//!    "every body and subject in this message is fallback". Earlier
//!    impl rejected such elements.
//! 3. **No dedicated test file**. Per CLAUDE.md "Every implemented XEP
//!    MUST have a dedicated Rust custom test suite" hard rule.
//!
//! The tests below exercise the public API published by
//! `waddle_xmpp::xep::xep0428` — building, parsing, set/strip
//! lifecycles — and pin the spec-required shape on the wire.

use minidom::Element;
use waddle_xmpp::xep::xep0428::{
    build_fallback_element, is_fallback_element, parse_fallbacks_from_message,
    set_fallback_payloads, strip_fallback_ranges, FallbackIndication, FallbackRange,
    FallbackRegion, NS_FALLBACK,
};
use xmpp_parsers::message::Message;

fn parse_message(xml: &str) -> Message {
    Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message")
}

// ── §1: container shape and namespace pinning ─────────────────────────────

#[test]
fn xep_0428_namespace_constant_matches_spec() {
    // The container namespace is mandatory per §2.2.
    assert_eq!(NS_FALLBACK, "urn:xmpp:fallback:0");
}

#[test]
fn xep_0428_is_fallback_element_namespaces_strictly() {
    let elem = Element::builder("fallback", NS_FALLBACK).build();
    assert!(is_fallback_element(&elem));
    let wrong_ns = Element::builder("fallback", "other:ns").build();
    assert!(!is_fallback_element(&wrong_ns));
    let wrong_name = Element::builder("fallbacks", NS_FALLBACK).build();
    assert!(!is_fallback_element(&wrong_name));
}

// ── §2.2: `for=` attribute (optional) ─────────────────────────────────────

#[test]
fn xep_0428_for_attribute_present_round_trips() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'/>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].for_ns.as_deref(), Some("urn:xmpp:reply:0"));
}

#[test]
fn xep_0428_for_attribute_absent_means_whole_message_per_spec() {
    // §2.2: "If the <fallback/> element does not have any childs, it is
    // assumed to apply to every message <body/> and <subject/> present
    // in the message." The `for=` attribute is OPTIONAL on the wire —
    // a bare element is a valid spec form and must NOT be dropped.
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0'/>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(
        fallbacks.len(),
        1,
        "spec-conformant bare <fallback/> must parse, not be dropped"
    );
    assert!(fallbacks[0].for_ns.is_none());
    assert!(fallbacks[0].body.is_none());
    assert!(fallbacks[0].subject.is_none());
}

#[test]
fn xep_0428_for_attribute_empty_string_treated_as_absent() {
    // Defensive: a `for=""` is functionally indistinguishable from
    // `for=` being absent. Treat as absent rather than retaining an
    // empty string.
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for=''/>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert!(fallbacks[0].for_ns.is_none());
}

// ── §2.2: <body/> children (whole + ranges) ───────────────────────────────

#[test]
fn xep_0428_body_with_offsets_marks_a_range() {
    // The §2.2 canonical example exactly.
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='33'/>\
            </fallback>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].for_ns.as_deref(), Some("urn:xmpp:reply:0"));
    assert_eq!(
        fallbacks[0].body,
        Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 0,
            end: 33
        }]))
    );
    assert!(fallbacks[0].subject.is_none());
}

#[test]
fn xep_0428_body_without_offsets_marks_the_whole_body() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:sfs:0'>\
                <body/>\
            </fallback>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].body, Some(FallbackRegion::Whole));
}

#[test]
fn xep_0428_multiple_body_ranges_are_all_carried() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
                <body start='20' end='30'/>\
            </fallback>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(
        fallbacks[0].body,
        Some(FallbackRegion::Ranges(vec![
            FallbackRange { start: 0, end: 10 },
            FallbackRange { start: 20, end: 30 },
        ]))
    );
}

// ── §2.2: <subject/> children ─────────────────────────────────────────────
//
// Spec §2.2 is explicit that subject AND body may both be fallback
// regions: "The <fallback/> element may have one or multiple <body/> or
// <subject/> child elements". Earlier impl silently dropped subject-only
// elements.

#[test]
fn xep_0428_subject_with_offsets_marks_a_range() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <subject start='0' end='4'/>\
            </fallback>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert!(fallbacks[0].body.is_none());
    assert_eq!(
        fallbacks[0].subject,
        Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 0,
            end: 4
        }]))
    );
}

#[test]
fn xep_0428_subject_without_offsets_marks_the_whole_subject() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <subject/>\
            </fallback>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].subject, Some(FallbackRegion::Whole));
}

#[test]
fn xep_0428_body_and_subject_can_both_be_targeted() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <subject start='0' end='4'/>\
                <body start='5' end='20'/>\
            </fallback>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(
        fallbacks[0].subject,
        Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 0,
            end: 4
        }]))
    );
    assert_eq!(
        fallbacks[0].body,
        Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 5,
            end: 20
        }]))
    );
}

// ── Defensive parsing: malformed shapes are rejected ──────────────────────

#[test]
fn xep_0428_mixed_whole_body_plus_range_is_rejected() {
    // Mixing `<body/>` (whole) with `<body start=… end=…/>` (range) in
    // the same fallback indication is ambiguous; reject the whole
    // indication rather than guess.
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
                <body/>\
            </fallback>\
        </message>",
    );
    assert!(parse_fallbacks_from_message(&msg).is_empty());
}

#[test]
fn xep_0428_half_specified_range_is_rejected() {
    // `start=` without `end=` is malformed.
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0'/>\
            </fallback>\
        </message>",
    );
    assert!(parse_fallbacks_from_message(&msg).is_empty());
}

#[test]
fn xep_0428_end_before_start_is_rejected() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='10' end='3'/>\
            </fallback>\
        </message>",
    );
    assert!(parse_fallbacks_from_message(&msg).is_empty());
}

#[test]
fn xep_0428_non_integer_offsets_are_rejected() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='abc' end='123'/>\
            </fallback>\
        </message>",
    );
    assert!(parse_fallbacks_from_message(&msg).is_empty());
}

// ── Multiple <fallback/> payloads on a single message ─────────────────────
//
// Spec doesn't bound the count — every payload is independent.

#[test]
fn xep_0428_multiple_fallback_payloads_each_parse_independently() {
    let msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
            </fallback>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:sfs:0'/>\
        </message>",
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 2);
    assert_eq!(fallbacks[0].for_ns.as_deref(), Some("urn:xmpp:reply:0"));
    assert_eq!(fallbacks[1].for_ns.as_deref(), Some("urn:xmpp:sfs:0"));
}

// ── Builder round-trips and emits canonical shape ─────────────────────────

#[test]
fn xep_0428_builder_emits_for_attribute_and_body_offsets() {
    let elem = build_fallback_element(&FallbackIndication::for_range("urn:xmpp:reply:0", 0, 17));
    assert_eq!(elem.ns(), NS_FALLBACK);
    assert_eq!(elem.attr("for"), Some("urn:xmpp:reply:0"));
    let body = elem
        .children()
        .find(|c| c.name() == "body")
        .expect("body child");
    assert_eq!(body.ns(), NS_FALLBACK);
    assert_eq!(body.attr("start"), Some("0"));
    assert_eq!(body.attr("end"), Some("17"));
}

#[test]
fn xep_0428_builder_omits_for_when_indication_is_whole_message() {
    let elem = build_fallback_element(&FallbackIndication::whole_message());
    assert_eq!(elem.ns(), NS_FALLBACK);
    assert!(
        elem.attr("for").is_none(),
        "whole-message indication has no `for` attribute on the wire"
    );
    assert!(
        elem.children().next().is_none(),
        "whole-message indication has no children on the wire"
    );
}

#[test]
fn xep_0428_builder_emits_subject_alongside_body() {
    let indication = FallbackIndication {
        for_ns: Some("urn:xmpp:reply:0".to_owned()),
        body: Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 0,
            end: 10,
        }])),
        subject: Some(FallbackRegion::Whole),
    };
    let elem = build_fallback_element(&indication);
    let subject = elem
        .children()
        .find(|c| c.name() == "subject")
        .expect("subject child must be emitted");
    assert_eq!(subject.ns(), NS_FALLBACK);
    assert!(subject.attr("start").is_none());
    assert!(subject.attr("end").is_none());
    let body = elem
        .children()
        .find(|c| c.name() == "body")
        .expect("body child must be emitted");
    assert_eq!(body.attr("start"), Some("0"));
    assert_eq!(body.attr("end"), Some("10"));
}

#[test]
fn xep_0428_round_trip_preserves_full_indication_shape() {
    let original = FallbackIndication {
        for_ns: Some("urn:xmpp:message-retract:1".to_owned()),
        body: Some(FallbackRegion::Whole),
        subject: Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 4,
            end: 9,
        }])),
    };
    let elem = build_fallback_element(&original);
    let wrapper_xml = format!(
        "<message xmlns='jabber:client'>{}</message>",
        String::from(&elem)
    );
    let msg = parse_message(&wrapper_xml);
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0], original);
}

// ── set_fallback_payloads replaces previous fallback set ──────────────────

#[test]
fn xep_0428_set_fallback_payloads_replaces_existing_indications() {
    let mut msg = parse_message(
        "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:old'/>\
        </message>",
    );
    set_fallback_payloads(
        &mut msg,
        &[FallbackIndication::for_range("urn:xmpp:reply:0", 2, 5)],
    );
    let fallbacks = parse_fallbacks_from_message(&msg);
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].for_ns.as_deref(), Some("urn:xmpp:reply:0"));
    assert_eq!(
        fallbacks[0].body,
        Some(FallbackRegion::Ranges(vec![FallbackRange {
            start: 2,
            end: 5
        }]))
    );
}

// ── strip_fallback_ranges UTF-16 model ────────────────────────────────────

#[test]
fn xep_0428_strip_fallback_ranges_drops_specified_substrings() {
    let stripped = strip_fallback_ranges(
        "> quoted\n\nreply body",
        &[FallbackRange { start: 0, end: 10 }],
    );
    assert_eq!(stripped, "reply body");
}

#[test]
fn xep_0428_strip_fallback_ranges_handles_overlapping_and_non_bmp() {
    // "👋 hi\n\n" is a 7-UTF-16-unit prefix (surrogate pair counts as 2).
    let body = "👋 hi\n\nreply";
    let prefix_units = "👋 hi\n\n".encode_utf16().count();
    let stripped = strip_fallback_ranges(
        body,
        &[FallbackRange {
            start: 0,
            end: prefix_units,
        }],
    );
    assert_eq!(stripped, "reply");
}

#[test]
fn xep_0428_strip_fallback_ranges_saturates_out_of_bounds() {
    let stripped = strip_fallback_ranges(
        "short",
        &[FallbackRange {
            start: 2,
            end: 9999,
        }],
    );
    assert_eq!(stripped, "sh");
}

// ── Convenience constructors ──────────────────────────────────────────────

#[test]
fn xep_0428_whole_body_constructor_matches_canonical_wire_form() {
    let elem = build_fallback_element(&FallbackIndication::whole_body("urn:xmpp:sfs:0"));
    assert_eq!(elem.attr("for"), Some("urn:xmpp:sfs:0"));
    let body = elem.get_child("body", NS_FALLBACK).expect("body child");
    assert!(body.attr("start").is_none());
    assert!(body.attr("end").is_none());
    assert!(elem.get_child("subject", NS_FALLBACK).is_none());
}

#[test]
fn xep_0428_whole_subject_constructor_emits_subject_only() {
    let elem = build_fallback_element(&FallbackIndication::whole_subject("urn:xmpp:reply:0"));
    assert_eq!(elem.attr("for"), Some("urn:xmpp:reply:0"));
    assert!(elem.get_child("subject", NS_FALLBACK).is_some());
    assert!(elem.get_child("body", NS_FALLBACK).is_none());
}

#[test]
fn xep_0428_empty_range_list_canonicalises_to_whole_body() {
    let indication =
        FallbackIndication::for_ranges("urn:xmpp:sfs:0", std::iter::empty::<FallbackRange>());
    assert_eq!(indication.body, Some(FallbackRegion::Whole));
}
