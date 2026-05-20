//! XEP-0084: User Avatar — dedicated conformance suite.
//!
//! XEP-0084 doesn't carry its own disco feature — avatar visibility
//! is gated by the XEP-0163 PEP `+notify` filter (`urn:xmpp:avatar:metadata+notify`)
//! which clients advertise in CAPS, not by a server-side
//! `Feature::*()`. So this suite focuses on the wire-shape and
//! semantic invariants:
//!
//! - §4 namespace strings for the two PEP nodes,
//! - §4.2.1 metadata wire shape: `<metadata>` wrapping one
//!   `<info id=… type=…/>` with optional width/height/bytes/url,
//! - §4.2 data wire shape: `<data xmlns='urn:xmpp:avatar:data'>`
//!   carrying base64 text content,
//! - §4.2.1 SHA-1 hash determinism (item-id derivation), pinned
//!   against a known test vector,
//! - parser robustness against wrong-ns metadata, missing `<info>`
//!   (the §5 removal signal), and missing required attributes.

use minidom::Element;
use waddle_xmpp::xep::xep0084::{
    build_avatar_data, build_avatar_metadata, compute_avatar_hash, is_avatar_data_node,
    is_avatar_metadata_node, parse_avatar_data, parse_avatar_metadata, AvatarInfo,
    NODE_AVATAR_DATA, NODE_AVATAR_METADATA, NS_AVATAR_DATA, NS_AVATAR_METADATA,
};

// ── §4 namespace + node identifiers ─────────────────────────────────

#[test]
fn xep0084_namespace_constants_match_spec() {
    // §4 fixes both namespace URIs and the PEP node names; they
    // happen to coincide (which is fine, but worth pinning both
    // separately so a future rename of either trips a test).
    assert_eq!(NS_AVATAR_DATA, "urn:xmpp:avatar:data");
    assert_eq!(NS_AVATAR_METADATA, "urn:xmpp:avatar:metadata");
    assert_eq!(NODE_AVATAR_DATA, "urn:xmpp:avatar:data");
    assert_eq!(NODE_AVATAR_METADATA, "urn:xmpp:avatar:metadata");
}

#[test]
fn xep0084_node_classifiers_distinguish_data_and_metadata() {
    assert!(is_avatar_data_node("urn:xmpp:avatar:data"));
    assert!(is_avatar_metadata_node("urn:xmpp:avatar:metadata"));
    assert!(!is_avatar_data_node("urn:xmpp:avatar:metadata"));
    assert!(!is_avatar_metadata_node("urn:xmpp:avatar:data"));
    // Sanity: wrong-ns sibling nodes don't accidentally classify.
    assert!(!is_avatar_data_node("urn:xmpp:vcard4"));
}

// ── §4.2.1 SHA-1 item-id derivation ─────────────────────────────────

#[test]
fn xep0084_sha1_hash_matches_known_vector() {
    // §4.2.1: "the value of the 'id' attribute MUST be the SHA-1
    // hash of the image data." Pin the computation against
    // RFC 3174's "abc" test vector so an accidental swap of hash
    // algo (or hex encoding) trips a test.
    let hash = compute_avatar_hash(b"abc");
    assert_eq!(
        hash, "a9993e364706816aba3e25717850c26c9cd0d89d",
        "SHA-1(\"abc\") MUST match the RFC 3174 test vector"
    );
}

#[test]
fn xep0084_sha1_hash_is_lowercase_hex_40_chars() {
    // The PEP `<item id="…">` carrying the avatar bytes uses this
    // hash verbatim as its id. Clients dispatch on it for cache
    // lookup, so the textual form (lowercase, no separators, 40
    // chars) must stay deterministic.
    let hash = compute_avatar_hash(b"any-bytes");
    assert_eq!(hash.len(), 40, "SHA-1 hex is 40 chars");
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "hash MUST be lowercase hex: got {hash}"
    );
}

#[test]
fn xep0084_sha1_hash_is_deterministic() {
    // Same bytes → same id. Defence against a future migration to
    // a non-deterministic content-addressable scheme that would
    // break the §4.2.1 implicit contract.
    let bytes = vec![0xC0, 0xFF, 0xEE];
    assert_eq!(compute_avatar_hash(&bytes), compute_avatar_hash(&bytes));
}

// ── §4.2.1 metadata wire shape ──────────────────────────────────────

#[test]
fn xep0084_build_metadata_emits_full_spec_shape() {
    // §4.2.1 example shape:
    //   <metadata xmlns='urn:xmpp:avatar:metadata'>
    //     <info id='…' type='image/png' width='64' height='64'
    //           bytes='12345' url='https://…'/>
    //   </metadata>
    // The builder must pin element names, namespaces, and attribute
    // serialisation (numeric attrs as decimal strings).
    let info = AvatarInfo {
        id: "111f4b3c50d7b0df729d299bc6f8e9ef9066971f".to_owned(),
        mime_type: "image/png".to_owned(),
        width: Some(64),
        height: Some(64),
        bytes: Some(12_345),
        url: Some("https://avatars.example/64.png".to_owned()),
    };
    let elem = build_avatar_metadata(&info);

    assert_eq!(elem.name(), "metadata");
    assert_eq!(elem.ns(), NS_AVATAR_METADATA);

    let inner = elem
        .children()
        .find(|c| c.name() == "info" && c.ns() == NS_AVATAR_METADATA)
        .expect("<info> child present");
    assert_eq!(
        inner.attr("id"),
        Some("111f4b3c50d7b0df729d299bc6f8e9ef9066971f")
    );
    assert_eq!(inner.attr("type"), Some("image/png"));
    assert_eq!(inner.attr("width"), Some("64"));
    assert_eq!(inner.attr("height"), Some("64"));
    assert_eq!(inner.attr("bytes"), Some("12345"));
    assert_eq!(inner.attr("url"), Some("https://avatars.example/64.png"));
}

#[test]
fn xep0084_build_metadata_omits_optional_attrs_when_not_set() {
    // §4.2.1: width/height/bytes/url are OPTIONAL. The builder
    // MUST honour absence rather than emitting `width=""`
    // placeholders that would mislead consumers about dimensions.
    let info = AvatarInfo {
        id: "abc".to_owned(),
        mime_type: "image/png".to_owned(),
        width: None,
        height: None,
        bytes: None,
        url: None,
    };
    let elem = build_avatar_metadata(&info);
    let inner = elem
        .children()
        .find(|c| c.name() == "info")
        .expect("<info> present");
    assert!(inner.attr("width").is_none());
    assert!(inner.attr("height").is_none());
    assert!(inner.attr("bytes").is_none());
    assert!(inner.attr("url").is_none());
}

#[test]
fn xep0084_metadata_round_trip_preserves_every_field() {
    let original = AvatarInfo {
        id: "deadbeef".to_owned(),
        mime_type: "image/jpeg".to_owned(),
        width: Some(96),
        height: Some(96),
        bytes: Some(7_654),
        url: Some("https://avatars.example/96.jpg".to_owned()),
    };
    let parsed =
        parse_avatar_metadata(&build_avatar_metadata(&original)).expect("metadata round-trips");
    assert_eq!(parsed.id, "deadbeef");
    assert_eq!(parsed.mime_type, "image/jpeg");
    assert_eq!(parsed.width, Some(96));
    assert_eq!(parsed.height, Some(96));
    assert_eq!(parsed.bytes, Some(7_654));
    assert_eq!(
        parsed.url.as_deref(),
        Some("https://avatars.example/96.jpg")
    );
}

// ── §4.2 data wire shape ────────────────────────────────────────────

#[test]
fn xep0084_data_round_trip_preserves_base64_payload() {
    // §4.2: the data element's text content is the base64-encoded
    // image bytes. Round-trip pinning protects against a future
    // builder swap to e.g. an attribute-based shape.
    let elem = build_avatar_data("iVBORw0KGgoAAAANSUhEUgAA");
    assert_eq!(elem.name(), "data");
    assert_eq!(elem.ns(), NS_AVATAR_DATA);

    let recovered = parse_avatar_data(&elem).expect("data parses");
    assert_eq!(recovered, "iVBORw0KGgoAAAANSUhEUgAA");
}

#[test]
fn xep0084_parse_data_rejects_wrong_namespace_or_name() {
    // A `<data>` in some other namespace isn't a XEP-0084 avatar
    // payload. Accepting it would let arbitrary base64 from a
    // co-opted element name reach the avatar-cache writer.
    let wrong_ns = Element::builder("data", "attacker:ns")
        .append(minidom::Node::Text("AAAA".to_owned()))
        .build();
    assert!(parse_avatar_data(&wrong_ns).is_none());

    let wrong_name = Element::builder("payload", NS_AVATAR_DATA)
        .append(minidom::Node::Text("AAAA".to_owned()))
        .build();
    assert!(parse_avatar_data(&wrong_name).is_none());
}

#[test]
fn xep0084_parse_data_rejects_empty_payload() {
    // Empty text content is meaningless — there's no avatar to
    // cache. Surface as None rather than caching an empty string.
    let empty = Element::builder("data", NS_AVATAR_DATA).build();
    assert!(parse_avatar_data(&empty).is_none());
}

// ── Parser robustness ──────────────────────────────────────────────

#[test]
fn xep0084_parse_metadata_rejects_wrong_wrapper_namespace() {
    // A `<metadata>` in some other namespace isn't XEP-0084.
    // Accepting it would let an attacker inject a synthetic
    // avatar id that consumers would then trust for cache lookup.
    let wrong = Element::builder("metadata", "attacker:ns")
        .append(
            Element::builder("info", "attacker:ns")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "fake")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
                .build(),
        )
        .build();
    assert!(parse_avatar_metadata(&wrong).is_none());
}

#[test]
fn xep0084_parse_metadata_with_no_info_child_signals_no_avatar() {
    // §5 removal signal: an empty `<metadata/>` (no `<info>`
    // children) means "I have removed my avatar." `parse_avatar_metadata`
    // surfaces this as None — consumers MUST treat None on a
    // `<metadata>` whose ns/name match as "remove cached avatar"
    // rather than "leave the prior avatar in place".
    let empty = Element::builder("metadata", NS_AVATAR_METADATA).build();
    assert!(parse_avatar_metadata(&empty).is_none());
}

#[test]
fn xep0084_parse_metadata_rejects_info_without_id() {
    // §4.2.1 makes `id` REQUIRED. Without it, the PEP item has no
    // content-addressable identity; the parser MUST drop it.
    let no_id = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(
            Element::builder("info", NS_AVATAR_METADATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
                .build(),
        )
        .build();
    assert!(parse_avatar_metadata(&no_id).is_none());
}

#[test]
fn xep0084_parse_metadata_defaults_unspecified_type_to_image_png() {
    // §4.2.1 declares `type` REQUIRED, but the spec example uses
    // `image/png` as the default. Waddle's parser tolerates a
    // missing type by defaulting to `image/png` — pinning this so
    // a future strictening (rejecting type-less metadata)
    // surfaces as a test failure that prompts a deliberate
    // decision rather than a silent behavioural change.
    let no_type = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(
            Element::builder("info", NS_AVATAR_METADATA)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
                .build(),
        )
        .build();
    let info = parse_avatar_metadata(&no_type).expect("type-less metadata is accepted");
    assert_eq!(info.mime_type, "image/png");
}

#[test]
fn xep0084_parse_metadata_drops_malformed_numeric_attrs() {
    // `width="not-a-number"` etc. MUST NOT panic and MUST NOT
    // produce a `Some(0)` (which would mislead consumers about
    // dimensions). Surface as None for the malformed field while
    // keeping the rest of the parse intact.
    let elem = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(
            Element::builder("info", NS_AVATAR_METADATA)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
                .attr(minidom::rxml::xml_ncname!("width").to_owned(), "wide")
                .attr(minidom::rxml::xml_ncname!("height").to_owned(), "")
                .attr(minidom::rxml::xml_ncname!("bytes").to_owned(), "lots")
                .build(),
        )
        .build();
    let info = parse_avatar_metadata(&elem).expect("parses despite bad numeric attrs");
    assert_eq!(info.id, "abc");
    assert_eq!(info.width, None);
    assert_eq!(info.height, None);
    assert_eq!(info.bytes, None);
}
