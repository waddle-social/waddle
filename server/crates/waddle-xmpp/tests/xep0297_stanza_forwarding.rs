//! XEP-0297: Stanza Forwarding — dedicated conformance suite.
//!
//! XEP-0297 is a *substrate*: it defines the `<forwarded/>` wrapper
//! that Carbons (XEP-0280), MAM (XEP-0313), and general message
//! forwarding all reuse. It has no disco feature of its own — the
//! profiles that build on it advertise their own. So this suite
//! focuses on the wire-shape invariants the substrate guarantees:
//!
//! - §3 namespace string `urn:xmpp:forward:0`,
//! - §3 element shape: `<forwarded xmlns='urn:xmpp:forward:0'>` with
//!   an embedded stanza child and an optional XEP-0203 `<delay/>`,
//! - round-trip stability for `Message` payloads with bodies, types,
//!   and from/to addressing,
//! - classifier robustness against wrong-ns / wrong-name near-misses.

use chrono::{DateTime, Utc};
use minidom::Element;
use waddle_xmpp::xep::xep0297::{
    build_forwarded_element, build_forwarded_now, build_forwarded_with_delay,
    extract_forwarded_from_message, is_forwarded_element, parse_forwarded_element,
    ForwardedMessage, ForwardingCarrier, NS_FORWARD,
};
use xmpp_parsers::message::{Message, MessageType};

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0297_namespace_matches_spec() {
    // XEP-0297 §3 pins this exact URI. Carbons and MAM dispatch on
    // it; a typo silently drops every forwarded stanza into
    // "unknown payload" routing.
    assert_eq!(NS_FORWARD, "urn:xmpp:forward:0");
}

// ── §3 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0297_classifier_accepts_spec_shape_only() {
    let canonical = Element::builder("forwarded", NS_FORWARD).build();
    assert!(is_forwarded_element(&canonical));

    let wrong_ns = Element::builder("forwarded", "wrong:ns").build();
    assert!(!is_forwarded_element(&wrong_ns));

    let wrong_name = Element::builder("forward", NS_FORWARD).build();
    assert!(!is_forwarded_element(&wrong_name));
}

#[test]
fn xep0297_builder_emits_namespaced_forwarded_with_inner_message() {
    // The §3 example wraps the original stanza verbatim inside
    // `<forwarded>`. The builder MUST emit the wrapper namespace
    // and let the inner message keep its own namespace/identity.
    let mut original = Message::new(
        "juliet@capulet.example/balcony"
            .parse::<jid::Jid>()
            .expect("valid jid"),
    );
    original.from = Some(
        "romeo@montague.example/orchard"
            .parse::<jid::Jid>()
            .expect("valid jid"),
    );
    original.type_ = MessageType::Chat;
    original.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "O Romeo, Romeo!".to_owned(),
    );

    let elem = build_forwarded_element(&ForwardedMessage::new(original));
    assert_eq!(elem.name(), "forwarded");
    assert_eq!(elem.ns(), NS_FORWARD);

    let inner = elem
        .children()
        .find(|c| c.name() == "message")
        .expect("inner <message> child");
    assert!(
        inner
            .children()
            .any(|child| child.name() == "body" && child.text() == "O Romeo, Romeo!"),
        "inner message body MUST be preserved verbatim"
    );
}

// ── §3 round-trip ────────────────────────────────────────────────────

fn spec_message() -> Message {
    let mut msg = Message::new(
        "juliet@capulet.example/chamber"
            .parse::<jid::Jid>()
            .expect("valid jid"),
    );
    msg.from = Some(
        "romeo@montague.example/orchard"
            .parse::<jid::Jid>()
            .expect("valid jid"),
    );
    msg.type_ = MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "Hello".to_owned());
    msg
}

fn fixed_stamp() -> DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("fixed test stamp")
}

#[test]
fn xep0297_round_trip_preserves_body_and_addressing() {
    // The minimum round-trip guarantee: a forwarded message
    // arrives at the consumer with the original `from`, `to`, and
    // body intact. Anything weaker breaks MAM replay (which reads
    // an archived message back through the forwarded wrapper).
    let original = spec_message();
    let elem = build_forwarded_element(&ForwardedMessage::new(original));
    let parsed = parse_forwarded_element(&elem).expect("forwarded parses");

    assert_eq!(parsed.message.type_, MessageType::Chat);
    assert_eq!(
        parsed.message.from.as_ref().map(ToString::to_string),
        Some("romeo@montague.example/orchard".to_owned()),
    );
    assert_eq!(
        parsed.message.to.as_ref().map(ToString::to_string),
        Some("juliet@capulet.example/chamber".to_owned()),
    );
    assert_eq!(
        parsed.message.bodies.get("").cloned(),
        Some("Hello".to_owned())
    );
}

#[test]
fn xep0297_round_trip_preserves_xep0203_delay_stamp_and_from() {
    // XEP-0297 §3 uses XEP-0203 `<delay/>` to carry the original
    // send timestamp. MAM relies on this — the archived message's
    // delay stamp is the only source of "when did this originally
    // happen" that the consumer sees. Loss here corrupts the
    // archive timeline.
    let stamp = fixed_stamp();
    let elem = build_forwarded_with_delay(&spec_message(), stamp, "room@conf.example/relay");
    let parsed = parse_forwarded_element(&elem).expect("forwarded parses");

    assert_eq!(parsed.stamp, Some(stamp));
    assert_eq!(
        parsed.delay_from.as_deref(),
        Some("room@conf.example/relay")
    );
}

#[test]
fn xep0297_build_forwarded_now_stamps_the_current_time() {
    // The Carbons-style "no delay specified" helper: the resulting
    // forwarded element still carries a `<delay/>` so the recipient
    // can place the carbon on the timeline. The stamp must be
    // present and within a sane window of now.
    let before = Utc::now();
    let elem = build_forwarded_now(&spec_message());
    let after = Utc::now();
    let parsed = parse_forwarded_element(&elem).expect("forwarded parses");

    let stamp = parsed.stamp.expect("now-stamped forwarded carries delay");
    assert!(
        stamp >= before && stamp <= after,
        "build_forwarded_now MUST stamp the current time, got {stamp} outside [{before}, {after}]"
    );
}

#[test]
fn xep0297_round_trip_omits_delay_when_not_set() {
    // `ForwardedMessage::with_stamp(_, None)` isn't a thing —
    // construction via `with_stamp` / `new` always sets a stamp.
    // The escape hatch is a directly-constructed `ForwardedMessage`
    // with `stamp: None`, used by some Carbons flows that defer
    // stamping to the recipient. Builder MUST honour that by
    // omitting the `<delay/>` child rather than inserting an
    // empty-stamp placeholder.
    let fwd = ForwardedMessage {
        message: spec_message(),
        stamp: None,
        delay_from: None,
    };
    let elem = build_forwarded_element(&fwd);
    assert!(
        elem.children().find(|c| c.name() == "delay").is_none(),
        "absent stamp MUST NOT be invented"
    );

    let parsed = parse_forwarded_element(&elem).expect("forwarded parses");
    assert_eq!(parsed.stamp, None);
    assert_eq!(parsed.delay_from, None);
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0297_parse_rejects_wrong_wrapper_namespace() {
    // A `<forwarded>` in some other namespace isn't a XEP-0297
    // wrapper. Accepting it would let an attacker smuggle stanzas
    // through carbons/MAM under a co-opted element name.
    let bogus = Element::builder("forwarded", "attacker:ns")
        .append(
            Element::builder("message", "jabber:client")
                .append(
                    Element::builder("body", "jabber:client")
                        .append("hi")
                        .build(),
                )
                .build(),
        )
        .build();
    assert!(parse_forwarded_element(&bogus).is_none());
}

#[test]
fn xep0297_parse_returns_none_when_inner_message_missing() {
    // §3 requires the embedded stanza. A `<forwarded>` without an
    // inner `<message>` (or `<presence>`/`<iq>`, though Waddle's
    // Carbons/MAM consumers only care about messages) has no
    // content to surface; the parser MUST report None rather than
    // fabricate an empty `Message`.
    let empty = Element::builder("forwarded", NS_FORWARD).build();
    assert!(parse_forwarded_element(&empty).is_none());
}

// ── Carrier-trait surface ───────────────────────────────────────────

#[test]
fn xep0297_carrier_trait_surfaces_forwarded_from_message_payload() {
    // The end-to-end consumer pattern: Carbons (XEP-0280) and MAM
    // (XEP-0313) wrap a parent `<message>` around the forwarded
    // element. `extract_forwarded_from_message` finds the inner
    // forwarded payload from the wrapping `Message::payloads`.
    let elem = build_forwarded_element(&ForwardedMessage::new(spec_message()));
    let mut wrapper = Message::new(Some(
        "alice@example.com".parse::<jid::Jid>().expect("valid jid"),
    ));
    wrapper.payloads.push(elem);

    let fwd = extract_forwarded_from_message(&wrapper).expect("forwarded surfaces");
    assert_eq!(
        fwd.message.bodies.get("").cloned(),
        Some("Hello".to_owned())
    );
    assert!(wrapper.has_forwarded());
}
