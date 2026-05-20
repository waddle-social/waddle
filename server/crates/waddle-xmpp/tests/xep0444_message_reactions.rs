//! XEP-0444: Message Reactions — dedicated conformance suite.
//!
//! In-crate `xep::xep0444::tests` covers helper internals (the
//! dedup / trim normaliser, etc.). This file pins the audit-level
//! invariants at the public-API boundary:
//!
//! - §3 namespace string `urn:xmpp:reactions:0`,
//! - §"Determining Support" advertisement on both server and every
//!   MUC room configuration,
//! - §3 wire shape: `<reactions id='target' xmlns='urn:xmpp:reactions:0'>`
//!   with zero-or-more `<reaction>emoji</reaction>` children,
//! - §3.2 "empty reactions set is a removal" semantics,
//! - §3.1 emoji-set uniqueness (the same emoji from the same sender
//!   collapses to one),
//! - parser robustness against empty `id=""`, wrong-ns, and
//!   missing-id payloads.

use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::xep0444::{
    build_reaction_element, build_reaction_message, build_reactions_element,
    extract_reactions_from_message, is_reaction_message, is_reactions_element, set_reactions,
    strip_reactions, ReactionCarrier, ReactionSet, NS_REACTIONS,
};
use xmpp_parsers::message::{Message, MessageType};

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0444_namespace_matches_spec() {
    // XEP-0444 §3 pins the namespace URI. Clients dispatch on it
    // for reaction-state projection; a typo silently drops every
    // reaction into "unknown payload" routing.
    assert_eq!(NS_REACTIONS, "urn:xmpp:reactions:0");
}

// ── §"Determining Support" advertisement ────────────────────────────

#[test]
fn xep0444_server_features_advertise_reactions() {
    // §"Determining Support": a service that supports reactions
    // SHOULD advertise `urn:xmpp:reactions:0` in disco#info.
    // Waddle routes reactions on both DMs and groupchats, so the
    // advert is mandatory.
    let feats = server_features();
    let target = Feature::reactions();
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:xmpp:reactions:0`"
    );
}

#[test]
fn xep0444_muc_rooms_advertise_reactions_in_every_configuration() {
    let target = Feature::reactions();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features(persistent={persistent}, members_only={members_only}, \
                         moderated={moderated}, forum={forum}) must advertise \
                         `urn:xmpp:reactions:0`"
                    );
                }
            }
        }
    }
}

#[test]
fn xep0444_feature_constructor_pins_namespace_string() {
    assert_eq!(Feature::reactions().0, "urn:xmpp:reactions:0");
}

// ── §3 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0444_classifier_accepts_spec_shape_only() {
    let canonical = Element::builder("reactions", NS_REACTIONS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "msg-1")
        .build();
    assert!(is_reactions_element(&canonical));

    let wrong_ns = Element::builder("reactions", "wrong:ns").build();
    assert!(!is_reactions_element(&wrong_ns));

    let wrong_name = Element::builder("reaction", NS_REACTIONS).build();
    assert!(!is_reactions_element(&wrong_name));
}

#[test]
fn xep0444_builder_emits_namespaced_reactions_with_reaction_children() {
    // §3 example shape: `<reactions id='target'>` wrapping one
    // `<reaction>emoji</reaction>` per emoji. The builder MUST
    // pin the namespace, the `id`, and the per-emoji child shape.
    let elem = build_reactions_element("target-msg", &["👍", "❤️"]);
    assert_eq!(elem.name(), "reactions");
    assert_eq!(elem.ns(), NS_REACTIONS);
    assert_eq!(elem.attr("id"), Some("target-msg"));

    let emojis: Vec<String> = elem
        .children()
        .filter(|c| c.name() == "reaction" && c.ns() == NS_REACTIONS)
        .map(|c| c.text())
        .collect();
    assert_eq!(emojis, vec!["👍".to_owned(), "❤️".to_owned()]);
}

#[test]
fn xep0444_build_reaction_element_pins_namespace() {
    // Defence in depth — a stray `<reaction>` in some other
    // namespace (or `<reactions>` with wrong-ns children) would
    // confuse the parser. Builder must always emit the spec ns.
    let elem = build_reaction_element("👍");
    assert_eq!(elem.name(), "reaction");
    assert_eq!(elem.ns(), NS_REACTIONS);
    assert_eq!(elem.text(), "👍");
}

// ── §3.1 uniqueness + §3.2 removal semantics ────────────────────────

#[test]
fn xep0444_duplicate_emojis_collapse_per_section_31() {
    // §3.1: "Reactions are uniquely identified by the unicode
    // character or sequence." A sender posting `["👍", "👍"]` is
    // expressing one reaction, not two; the normaliser must dedup
    // so the rendered count is 1.
    let set = ReactionSet::new("msg-1", vec!["👍".into(), "👍".into(), "❤️".into()]);
    assert_eq!(set.emojis, vec!["👍".to_owned(), "❤️".to_owned()]);
}

#[test]
fn xep0444_empty_reaction_set_is_a_removal() {
    // §3.2: "When sending an empty reactions element, the sender
    // is asking to remove all previous reactions." The carrier
    // type surfaces this as the `is_removal()` flag — consumers
    // shouldn't have to introspect `.emojis.is_empty()` directly.
    let removal = ReactionSet::new("msg-1", Vec::new());
    assert!(removal.is_removal());
    assert!(removal.emojis.is_empty());

    let active = ReactionSet::new("msg-1", vec!["👍".into()]);
    assert!(!active.is_removal());
}

#[test]
fn xep0444_empty_set_round_trip_serializes_as_removal_element() {
    // Round-trip the §3.2 removal shape:
    //   <reactions id='target' xmlns='urn:xmpp:reactions:0'/>
    // No reaction children — the empty element itself is the
    // removal signal.
    let elem = build_reactions_element("target-msg", &[]);
    assert_eq!(elem.attr("id"), Some("target-msg"));
    assert_eq!(
        elem.children().filter(|c| c.name() == "reaction").count(),
        0,
        "empty set MUST NOT emit any `<reaction>` children"
    );

    let mut msg = Message::new(None::<jid::Jid>);
    msg.type_ = MessageType::Chat;
    msg.payloads.push(elem);
    let set = extract_reactions_from_message(&msg).expect("extracted");
    assert_eq!(set.message_id, "target-msg");
    assert!(set.is_removal());
}

// ── Whole-message helpers ───────────────────────────────────────────

#[test]
fn xep0444_build_reaction_message_round_trip_via_carrier_trait() {
    // End-to-end: build a body-less reaction message, classify it
    // via the carrier trait, and recover the original target +
    // emoji set.
    let msg = build_reaction_message(
        "lord@capulet.example".parse::<jid::Jid>().ok(),
        "juliet@example.com/web".parse::<jid::Jid>().ok(),
        "victim-msg-id",
        &["👀", "🔥"],
        MessageType::Chat,
    );
    assert!(
        is_reaction_message(&msg),
        "built reaction must classify as one"
    );
    assert!(
        msg.bodies.is_empty(),
        "§Implementation Notes: reactions are body-less; the helper MUST NOT inject one"
    );

    let set = msg.reactions().expect("via carrier trait");
    assert_eq!(set.message_id, "victim-msg-id");
    assert_eq!(set.emojis, vec!["👀".to_owned(), "🔥".to_owned()]);
}

#[test]
fn xep0444_set_reactions_replaces_prior_reactions_payload() {
    // §3 update semantics: a fresh reactions payload replaces the
    // sender's prior reaction state — sender keeps exactly one
    // `<reactions>` per target. If the mutator added a second
    // payload, consumers would have to merge or pick one.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.type_ = MessageType::Chat;
    set_reactions(&mut msg, "target-1", &["👍"]);
    set_reactions(&mut msg, "target-1", &["❤️"]);

    let payloads: Vec<_> = msg
        .payloads
        .iter()
        .filter(|e| is_reactions_element(e))
        .collect();
    assert_eq!(
        payloads.len(),
        1,
        "exactly one <reactions> payload survives the second set"
    );
    let set = extract_reactions_from_message(&msg).expect("extracted");
    assert_eq!(set.emojis, vec!["❤️".to_owned()]);
}

#[test]
fn xep0444_strip_reactions_removes_all_namespaced_payloads() {
    // The strip mutator clears every reactions payload (defensive
    // against duplicates that might have slipped in via a prior
    // bug). After strip, the carrier classifier MUST report false.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(build_reactions_element("t-1", &["👍"]));
    msg.payloads.push(build_reactions_element("t-1", &["❤️"]));
    strip_reactions(&mut msg);
    assert!(!is_reaction_message(&msg));
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0444_extract_rejects_payload_with_empty_id() {
    // `<reactions id="">` would route the sender's reaction-state
    // update against a phantom message; consumers must treat it
    // as malformed.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder("reactions", NS_REACTIONS)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "")
            .append(build_reaction_element("👍"))
            .build(),
    );
    assert!(extract_reactions_from_message(&msg).is_none());
}

#[test]
fn xep0444_extract_rejects_payload_missing_id_attribute() {
    // §3 makes `id` REQUIRED on `<reactions>`. Without it the
    // payload doesn't reference any target; parser MUST drop it.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder("reactions", NS_REACTIONS)
            .append(build_reaction_element("👍"))
            .build(),
    );
    assert!(extract_reactions_from_message(&msg).is_none());
}

#[test]
fn xep0444_extract_ignores_reaction_children_in_wrong_namespace() {
    // A `<reactions xmlns='urn:xmpp:reactions:0'>` carrying
    // `<reaction xmlns='attacker:ns'>` children: the spec ns
    // gates per-emoji elements separately from the wrapper. The
    // parser MUST only count namespaced `<reaction>` children;
    // foreign-ns children are dropped, not added to the set.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder("reactions", NS_REACTIONS)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "t-1")
            .append(
                Element::builder("reaction", "attacker:ns")
                    .append("☠️")
                    .build(),
            )
            .append(build_reaction_element("👍"))
            .build(),
    );

    let set = extract_reactions_from_message(&msg).expect("wrapper still parses");
    assert_eq!(
        set.emojis,
        vec!["👍".to_owned()],
        "foreign-ns `<reaction>` children MUST be ignored"
    );
}
