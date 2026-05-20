//! XEP-0372: References — dedicated conformance suite.
//!
//! Pins the audit-level invariants at the public-API boundary:
//!
//! - §"Protocol Namespace" namespace string `urn:xmpp:reference:0`,
//! - §"Discovering Support" advertisement on `server_features()`
//!   and every MUC room configuration,
//! - §"Protocol" wire shape: `<reference xmlns='…' type='mention|data'
//!   uri='…' begin='…' end='…'/>` — `type` + `uri` REQUIRED;
//!   begin/end optional,
//! - parser robustness: missing `type`, missing `uri`, malformed
//!   begin/end range,
//! - `xmpp:` URI extraction for the mention-routing fast path
//!   (`mentions_jid` is the consumer surface for XEP-0513-style
//!   per-recipient mentions when XEP-0372 is the carrier).

use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::xep0372::{
    add_reference, build_reference_element, extract_mention_uris, extract_mentioned_jids,
    extract_references_from_message, has_references, is_reference_element, parse_reference_element,
    strip_references, Reference, ReferenceCarrier, ReferenceError, ReferenceType, NS_REFERENCE,
};
use xmpp_parsers::message::Message;

// ── §"Protocol Namespace" ────────────────────────────────────────────

#[test]
fn xep0372_namespace_matches_spec() {
    // §"Protocol Namespace" pins the URI. Clients dispatch on it
    // for mention rendering; a typo silently drops every
    // mention into "unknown payload" routing.
    assert_eq!(NS_REFERENCE, "urn:xmpp:reference:0");
}

#[test]
fn xep0372_reference_type_enum_matches_spec_strings() {
    // §"Reference Types" defines `mention` and `data`. The wire
    // strings are the textual contract.
    assert_eq!(ReferenceType::Mention.as_str(), "mention");
    assert_eq!(ReferenceType::Data.as_str(), "data");
    assert_eq!(
        ReferenceType::from_str_attr("mention"),
        Some(ReferenceType::Mention)
    );
    assert_eq!(
        ReferenceType::from_str_attr("data"),
        Some(ReferenceType::Data)
    );
    assert_eq!(ReferenceType::from_str_attr("bogus"), None);
}

// ── §"Discovering Support" advertisement ────────────────────────────

#[test]
fn xep0372_server_features_advertise_references() {
    let feats = server_features();
    let target = Feature::references();
    assert_eq!(target.0, NS_REFERENCE);
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:xmpp:reference:0`"
    );
}

#[test]
fn xep0372_muc_rooms_advertise_references_in_every_configuration() {
    let target = Feature::references();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         must advertise `urn:xmpp:reference:0`"
                    );
                }
            }
        }
    }
}

// ── §"Protocol" wire shape ──────────────────────────────────────────

#[test]
fn xep0372_classifier_accepts_spec_shape_only() {
    let canonical = build_reference_element(&Reference::mention("xmpp:alice@example.com"));
    assert!(is_reference_element(&canonical));

    let wrong_ns = Element::builder("reference", "wrong:ns").build();
    assert!(!is_reference_element(&wrong_ns));

    let wrong_name = Element::builder("ref", NS_REFERENCE).build();
    assert!(!is_reference_element(&wrong_name));
}

#[test]
fn xep0372_build_mention_reference_emits_full_spec_shape() {
    // §"Protocol" example:
    //   <reference xmlns='urn:xmpp:reference:0' type='mention'
    //              begin='72' end='78' uri='xmpp:alice@example.com'/>
    let mention = Reference::mention_at(72, 78, "xmpp:alice@example.com");
    let elem = build_reference_element(&mention);

    assert_eq!(elem.name(), "reference");
    assert_eq!(elem.ns(), NS_REFERENCE);
    assert_eq!(elem.attr("type"), Some("mention"));
    assert_eq!(elem.attr("uri"), Some("xmpp:alice@example.com"));
    assert_eq!(elem.attr("begin"), Some("72"));
    assert_eq!(elem.attr("end"), Some("78"));
}

#[test]
fn xep0372_build_data_reference_uses_data_type_string() {
    let data = Reference::data("https://files.example.com/foo.jpg");
    let elem = build_reference_element(&data);
    assert_eq!(elem.attr("type"), Some("data"));
    assert_eq!(elem.attr("uri"), Some("https://files.example.com/foo.jpg"));
    // begin/end optional — absent on whole-message references.
    assert!(elem.attr("begin").is_none());
    assert!(elem.attr("end").is_none());
}

#[test]
fn xep0372_round_trip_preserves_every_field() {
    let original = Reference::mention_at(0, 5, "xmpp:bob@example.com").with_anchor("@bob");
    let elem = build_reference_element(&original);
    let parsed = parse_reference_element(&elem).expect("round-trips");

    assert_eq!(parsed.ref_type, ReferenceType::Mention);
    assert_eq!(parsed.begin, Some(0));
    assert_eq!(parsed.end, Some(5));
    assert_eq!(parsed.uri, "xmpp:bob@example.com");
    assert_eq!(parsed.anchor.as_deref(), Some("@bob"));
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0372_parse_rejects_reference_missing_type_attribute() {
    // §"Protocol" makes `type` REQUIRED. Without it the consumer
    // can't tell a mention from a data reference; parser MUST
    // surface MissingType as an error.
    let elem = Element::builder("reference", NS_REFERENCE)
        .attr(
            minidom::rxml::xml_ncname!("uri").to_owned(),
            "xmpp:alice@example.com",
        )
        .build();
    let err = parse_reference_element(&elem).expect_err("missing type");
    assert!(matches!(err, ReferenceError::MissingType));
}

#[test]
fn xep0372_parse_rejects_reference_missing_uri_attribute() {
    // §"Protocol" makes `uri` REQUIRED — it's the entire point of
    // the reference. Missing means "reference to nothing."
    let elem = Element::builder("reference", NS_REFERENCE)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "mention")
        .build();
    let err = parse_reference_element(&elem).expect_err("missing uri");
    assert!(matches!(err, ReferenceError::MissingUri));
}

#[test]
fn xep0372_parse_rejects_inverted_range() {
    // §"Protocol" begin/end define a body span; `begin > end`
    // would either crash a substring slicer or render the
    // mention at the wrong position. Parser MUST flag inverted
    // ranges instead of letting them propagate to renderers.
    let elem = Element::builder("reference", NS_REFERENCE)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "mention")
        .attr(
            minidom::rxml::xml_ncname!("uri").to_owned(),
            "xmpp:alice@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("begin").to_owned(), "10")
        .attr(minidom::rxml::xml_ncname!("end").to_owned(), "5")
        .build();
    let err = parse_reference_element(&elem).expect_err("inverted range");
    assert!(matches!(
        err,
        ReferenceError::InvalidRange { begin: 10, end: 5 }
    ));
}

#[test]
fn xep0372_extract_skips_malformed_references_silently() {
    // The lenient `extract_*` helper swallows parse errors and
    // surfaces only the valid references. This is the
    // best-effort path for inbox preview / mention-detection:
    // a single bad payload mustn't drop every reference on the
    // message.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads
        .push(Element::builder("reference", NS_REFERENCE).build()); // missing both required attrs
    msg.payloads
        .push(build_reference_element(&Reference::mention(
            "xmpp:alice@example.com",
        )));

    let extracted = extract_references_from_message(&msg);
    assert_eq!(extracted.len(), 1, "only the valid reference surfaces");
    assert_eq!(extracted[0].uri, "xmpp:alice@example.com");
}

// ── Mention-routing helpers ─────────────────────────────────────────

#[test]
fn xep0372_extract_mention_uris_filters_to_mentions() {
    // `extract_mention_uris` is the fast path consumers use to
    // route mention notifications — it MUST filter out data
    // references (which point at files/media, not users).
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));
    add_reference(
        &mut msg,
        &Reference::data("https://files.example.com/avatar.png"),
    );
    add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));

    let uris = extract_mention_uris(&msg);
    assert_eq!(uris.len(), 2);
    assert!(uris.contains(&"xmpp:alice@example.com".to_string()));
    assert!(uris.contains(&"xmpp:bob@example.com".to_string()));
    assert!(!uris.iter().any(|u| u.contains("files.example.com")));
}

#[test]
fn xep0372_extract_mentioned_jids_strips_xmpp_scheme() {
    // For the mention-notification path, consumers want JIDs
    // (`alice@example.com`), not URIs (`xmpp:alice@example.com`).
    // The helper strips the `xmpp:` scheme so downstream code
    // can match against bare JIDs directly.
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));
    add_reference(&mut msg, &Reference::mention("https://not-a-jid.example/"));

    let jids = extract_mentioned_jids(&msg);
    assert_eq!(jids, vec!["alice@example.com".to_owned()]);
}

#[test]
fn xep0372_carrier_trait_mentions_jid_matches_full_uri() {
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));

    assert!(msg.has_mentions());
    assert!(msg.mentions_jid("alice@example.com"));
    assert!(!msg.mentions_jid("bob@example.com"));
}

// ── Mutator semantics ───────────────────────────────────────────────

#[test]
fn xep0372_add_reference_accumulates_payloads() {
    // Unlike correction (§3 "last wins"), references stack — a
    // single message can mention multiple users.
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:a@example.com"));
    add_reference(&mut msg, &Reference::mention("xmpp:b@example.com"));
    add_reference(&mut msg, &Reference::mention("xmpp:c@example.com"));

    assert_eq!(extract_references_from_message(&msg).len(), 3);
    assert!(has_references(&msg));
}

#[test]
fn xep0372_strip_references_clears_every_namespaced_payload() {
    let mut msg = Message::new(None::<jid::Jid>);
    add_reference(&mut msg, &Reference::mention("xmpp:a@example.com"));
    add_reference(&mut msg, &Reference::data("https://example/img.png"));
    strip_references(&mut msg);

    assert!(!has_references(&msg));
    assert!(extract_references_from_message(&msg).is_empty());
}
