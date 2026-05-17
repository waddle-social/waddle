//! XEP-0292: vCard4 Over XMPP — dedicated conformance suite.
//!
//! Pins:
//! - the §3 namespace and PEP node identifiers,
//! - the §8 MUST disco feature on both server-wide and PEP-domain
//!   advertisements (Waddle's `<vcard>` storage is PEP-hosted, so the
//!   "supports vCard4" claim has to appear on both surfaces clients
//!   actually query),
//! - the parse/build round-trip for an item modelled on the §3.1
//!   spec example, and the `is_vcard4_element` classifier that gates
//!   PEP item dispatch.
//!
//! Custom Waddle behaviour (avatar URI references via PEP `urn:xmpp:avatar:data`)
//! lives in `waddle-server::profile::vcard_rmw`; the wire-shape contract
//! lives here.

use minidom::Element;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::pubsub::pep_features;
use waddle_xmpp::xep::xep0292::{
    build_vcard4_element, is_vcard4_element, parse_vcard4, VCard4, NS_VCARD4, PEP_NODE_VCARD4,
};

// ── §3 namespace + PEP node identifiers ──────────────────────────────

#[test]
fn xep0292_namespace_matches_rfc6350_xml_binding() {
    // RFC 6351 / XEP-0292 §3 pin this exact namespace URI; clients
    // dispatch on the string, so a typo here silently breaks every
    // vCard4 consumer.
    assert_eq!(NS_VCARD4, "urn:ietf:params:xml:ns:vcard-4.0");
}

#[test]
fn xep0292_pep_node_matches_spec() {
    // XEP-0292 §3: vCard4 items are published to the PEP node
    // "urn:xmpp:vcard4". Subscribers (rosters + `+notify` clients)
    // listen on this exact node name.
    assert_eq!(PEP_NODE_VCARD4, "urn:xmpp:vcard4");
}

// ── §8 MUST disco advertisement ──────────────────────────────────────

#[test]
fn xep0292_server_features_advertise_vcard4_per_section_8_must() {
    // XEP-0292 §8: "If an XMPP client or server supports the vCard4
    // namespace, it MUST advertise that fact in its responses to
    // XEP-0030 information ('disco#info') requests by returning a
    // feature of `urn:ietf:params:xml:ns:vcard-4.0`."
    //
    // Waddle stores and serves vCard4 PEP items via
    // `waddle-server::profile::vcard_rmw`, so the server-wide
    // disco#info MUST list this feature.
    let feats = server_features();
    let target = Feature::vcard4();
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:ietf:params:xml:ns:vcard-4.0` per XEP-0292 §8 MUST"
    );
}

#[test]
fn xep0292_pep_features_advertise_vcard4_per_section_8_must() {
    // §8's "client or server" obligation also covers the PEP service
    // that owns the `urn:xmpp:vcard4` node: a remote peer querying
    // the per-user PEP disco surface must see the feature to know
    // vCard4 publication is supported here.
    let feats = pep_features();
    let target = Feature::vcard4();
    assert!(
        feats.iter().any(|f| f == &target),
        "pep_features() must advertise `urn:ietf:params:xml:ns:vcard-4.0` per XEP-0292 §8 MUST"
    );
}

#[test]
fn xep0292_feature_constructor_pins_namespace_string() {
    // Defence-in-depth: even if a future refactor changes how the
    // server/pep feature lists are assembled, the value of
    // Feature::vcard4() itself must stay anchored to the spec URI.
    let feat = Feature::vcard4();
    assert_eq!(feat.0, "urn:ietf:params:xml:ns:vcard-4.0");
}

// ── Element classifier ───────────────────────────────────────────────

#[test]
fn xep0292_classifier_accepts_correct_namespace_and_element() {
    let elem = Element::builder("vcard", NS_VCARD4).build();
    assert!(is_vcard4_element(&elem));
}

#[test]
fn xep0292_classifier_rejects_wrong_namespace() {
    // The legacy XEP-0054 element shares the local name `vCard`
    // (different casing); a `<vcard xmlns='vcard-temp'>` would be
    // neither valid XEP-0054 (wrong case) nor XEP-0292 (wrong ns).
    // Either way, the classifier must not accept it as vCard4.
    let wrong_ns = Element::builder("vcard", "vcard-temp").build();
    assert!(!is_vcard4_element(&wrong_ns));
}

#[test]
fn xep0292_classifier_rejects_wrong_element_name() {
    // XEP-0292 §3 fixes the element name to lowercase `vcard`.
    // RFC 6351 uses `<vcards>` for collection wrappers; a stray
    // `<vcards>` payload must not be misclassified as a single item.
    let wrong_name = Element::builder("vcards", NS_VCARD4).build();
    assert!(!is_vcard4_element(&wrong_name));
}

// ── §3.1 example round-trip ──────────────────────────────────────────

#[test]
fn xep0292_parses_spec_example_shape() {
    // Modelled on XEP-0292 §3.1: a PEP-published vCard4 item with FN,
    // nickname, email, URL, and a photo URI. The parser must surface
    // each child as the corresponding typed field.
    let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                  <fn><text>Peter Saint-Andre</text></fn>\
                  <nickname><text>stpeter</text></nickname>\
                  <email><text>stpeter@stpeter.im</text></email>\
                  <url><uri>https://stpeter.im/</uri></url>\
                  <photo><uri>https://stpeter.im/images/stpeter.jpg</uri></photo>\
              </vcard>";
    let elem: Element = xml.parse().expect("valid vCard4 XML");
    let parsed = parse_vcard4(&elem);

    assert_eq!(parsed.full_name.as_deref(), Some("Peter Saint-Andre"));
    assert_eq!(parsed.nickname.as_deref(), Some("stpeter"));
    assert_eq!(parsed.email.as_deref(), Some("stpeter@stpeter.im"));
    assert_eq!(parsed.url.as_deref(), Some("https://stpeter.im/"));
    assert_eq!(
        parsed.photo_uri.as_deref(),
        Some("https://stpeter.im/images/stpeter.jpg")
    );
}

#[test]
fn xep0292_builder_emits_namespaced_property_grandchildren() {
    // XEP-0292 §3 follows the RFC 6351 mapping: each property is a
    // namespaced child of `<vcard>`, and the property's value lives
    // in a typed grandchild (`<text>` for text properties, `<uri>`
    // for URIs). Builder output must match so any conformant peer
    // parser can read what we publish.
    let vcard = VCard4::new()
        .with_full_name("Juliet Capulet")
        .with_email("juliet@example.com")
        .with_url("https://juliet.example.com");
    let elem = build_vcard4_element(&vcard);

    assert_eq!(elem.name(), "vcard");
    assert_eq!(elem.ns(), NS_VCARD4);

    let fn_child = elem
        .children()
        .find(|c| c.name() == "fn" && c.ns() == NS_VCARD4)
        .expect("<fn> child present");
    let fn_text = fn_child
        .children()
        .find(|c| c.name() == "text" && c.ns() == NS_VCARD4)
        .expect("<fn> wraps <text> per RFC 6351 mapping");
    assert_eq!(fn_text.text(), "Juliet Capulet");

    let url_child = elem
        .children()
        .find(|c| c.name() == "url" && c.ns() == NS_VCARD4)
        .expect("<url> child present");
    let url_uri = url_child
        .children()
        .find(|c| c.name() == "uri" && c.ns() == NS_VCARD4)
        .expect("<url> wraps <uri> for URI-valued properties");
    assert_eq!(url_uri.text(), "https://juliet.example.com");
}

#[test]
fn xep0292_round_trip_preserves_every_supported_property() {
    let original = VCard4::new()
        .with_full_name("Romeo Montague")
        .with_nickname("Romeo")
        .with_email("romeo@montague.example")
        .with_note("Star-crossed lover")
        .with_org("House Montague")
        .with_title("Heir")
        .with_url("https://romeo.example.com")
        .with_photo("https://example.com/romeo.jpg")
        .with_pronouns("he/him");

    let elem = build_vcard4_element(&original);
    let reparsed = parse_vcard4(&elem);

    assert_eq!(reparsed.full_name, original.full_name);
    assert_eq!(reparsed.nickname, original.nickname);
    assert_eq!(reparsed.email, original.email);
    assert_eq!(reparsed.note, original.note);
    assert_eq!(reparsed.org, original.org);
    assert_eq!(reparsed.title, original.title);
    assert_eq!(reparsed.url, original.url);
    assert_eq!(reparsed.photo_uri, original.photo_uri);
    assert_eq!(reparsed.pronouns, original.pronouns);
}

#[test]
fn xep0292_empty_vcard_builds_an_empty_namespaced_element() {
    // A vCard with no properties is still a valid §3 payload (a peer
    // may publish a placeholder before populating fields). The
    // element must keep its name and namespace pinned, and carry
    // zero children.
    let elem = build_vcard4_element(&VCard4::new());
    assert_eq!(elem.name(), "vcard");
    assert_eq!(elem.ns(), NS_VCARD4);
    assert_eq!(elem.children().count(), 0);
    assert!(is_vcard4_element(&elem));
}

#[test]
fn xep0292_parse_tolerates_missing_optional_properties() {
    // All vCard4 properties beyond `FN` are optional in RFC 6350.
    // Even `FN` may be absent from a partial PEP item if a publisher
    // separates it from the avatar push. The parser must not panic
    // and must report `None` for absent fields rather than empty
    // strings (an empty `FN` would falsely satisfy "has full_name").
    let xml =
        "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'><nickname><text>Mercutio</text></nickname></vcard>";
    let elem: Element = xml.parse().expect("valid xml");
    let parsed = parse_vcard4(&elem);

    assert_eq!(parsed.nickname.as_deref(), Some("Mercutio"));
    assert_eq!(parsed.full_name, None);
    assert_eq!(parsed.email, None);
    assert_eq!(parsed.photo_uri, None);
    assert_eq!(parsed.pronouns, None);
}

// ── RFC 6350 §6.2.7 / RFC 9554 PRONOUNS round-trip ───────────────────

#[test]
fn xep0292_parses_pronouns_text_child_per_rfc6350() {
    // RFC 6350 §6.2 (extended by RFC 9554 §3.1) defines `PRONOUNS` as
    // a text property. The XEP-0292 XML binding wraps every text
    // property's value in a `<text>` grandchild — `<pronouns>` is no
    // different. A conformant client publishes
    // `<pronouns><text>they/them</text></pronouns>`; the parser must
    // surface the value as `vcard.pronouns`.
    let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                  <fn><text>Sam</text></fn>\
                  <pronouns><text>they/them</text></pronouns>\
              </vcard>";
    let elem: Element = xml.parse().expect("valid vCard4 XML");
    let parsed = parse_vcard4(&elem);

    assert_eq!(parsed.full_name.as_deref(), Some("Sam"));
    assert_eq!(parsed.pronouns.as_deref(), Some("they/them"));
}

#[test]
fn xep0292_pronouns_round_trip_preserves_value() {
    // Build a vCard4 with only pronouns set, serialize, reparse, and
    // confirm the value survives. Guards against accidental dropping
    // of the property by either the builder or the parser.
    let original = VCard4::new().with_pronouns("xe/xem");
    let elem = build_vcard4_element(&original);
    let reparsed = parse_vcard4(&elem);

    assert_eq!(reparsed.pronouns.as_deref(), Some("xe/xem"));
    assert_eq!(reparsed.full_name, None);
}

#[test]
fn xep0292_pronouns_builder_emits_namespaced_text_grandchild() {
    // Wire-shape contract: the builder MUST emit
    // `<pronouns xmlns='urn:ietf:params:xml:ns:vcard-4.0'>
    //    <text xmlns='urn:ietf:params:xml:ns:vcard-4.0'>...</text>
    //  </pronouns>` — the same shape every other RFC 6350 text
    // property uses. A peer parser following the spec will look for
    // exactly this nesting.
    let vcard = VCard4::new().with_pronouns("she/they");
    let elem = build_vcard4_element(&vcard);

    let pronouns_child = elem
        .children()
        .find(|c| c.name() == "pronouns" && c.ns() == NS_VCARD4)
        .expect("<pronouns> child present");
    let pronouns_text = pronouns_child
        .children()
        .find(|c| c.name() == "text" && c.ns() == NS_VCARD4)
        .expect("<pronouns> wraps <text> per RFC 6351 mapping");
    assert_eq!(pronouns_text.text(), "she/they");
}

#[test]
fn xep0292_pronouns_omitted_when_unset() {
    // Absent pronouns MUST NOT serialize to a ghost
    // `<pronouns/>` element — an empty element would parse back to
    // `Some("")` on some implementations and falsely indicate the
    // user has declared pronouns when they have not.
    let vcard = VCard4::new().with_full_name("Anonymous");
    let elem = build_vcard4_element(&vcard);

    assert!(
        elem.children().find(|c| c.name() == "pronouns").is_none(),
        "unset pronouns must not emit a <pronouns/> element"
    );

    // And the parser side: an empty `<text/>` under `<pronouns>`
    // MUST round-trip to `None`, not `Some("")` — the parser already
    // filters empty text values; this test pins that behaviour for
    // pronouns specifically.
    let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                  <fn><text>Anon</text></fn>\
                  <pronouns><text></text></pronouns>\
              </vcard>";
    let parsed = parse_vcard4(&xml.parse::<Element>().expect("valid xml"));
    assert_eq!(parsed.pronouns, None);
}

#[test]
fn xep0292_pronouns_freeform_value_preserved_verbatim() {
    // RFC 9554 §3.1 explicitly leaves the value freeform — no
    // enumeration, no normalisation. The parser/builder must not
    // mangle whitespace, case, or punctuation in the user's chosen
    // string.
    let raw = "He/Him  ·  They/Them (formal)";
    let vcard = VCard4::new().with_pronouns(raw);
    let elem = build_vcard4_element(&vcard);
    let parsed = parse_vcard4(&elem);
    assert_eq!(parsed.pronouns.as_deref(), Some(raw));
}
