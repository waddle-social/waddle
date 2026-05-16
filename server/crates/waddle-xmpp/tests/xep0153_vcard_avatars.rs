//! XEP-0153: vCard-Based Avatars — dedicated conformance suite.
//!
//! XEP-0153 broadcasts a vCard PHOTO hash in presence via
//! `<x xmlns='vcard-temp:x:update'>` so clients can detect avatar
//! changes without polling vCards. This is the legacy companion
//! to XEP-0084 / XEP-0292 PEP avatars — separately advertised on
//! `vcard-temp` (XEP-0054), so the presence broadcast has no
//! disco feature of its own.
//!
//! Pinning the audit-level invariants:
//!
//! - §"Protocol" namespace string `vcard-temp:x:update`,
//! - §4 wire shape: `<x><photo>HEX-SHA1</photo></x>`,
//! - §4.1 "no avatar" sentinel: an empty `<photo/>` element means
//!   the user has explicitly cleared their avatar (distinct from
//!   "haven't computed yet"),
//! - SHA-1 hash determinism + RFC 3174 test vector pinning,
//! - parser robustness against wrong-ns / wrong-name / empty
//!   photo.

use minidom::Element;
use waddle_xmpp::xep::xep0153::{
    build_vcard_update_element, compute_photo_hash, compute_photo_hash_from_base64,
    has_vcard_update, parse_vcard_update, NS_VCARD_UPDATE,
};

// ── §"Protocol" namespace ───────────────────────────────────────────

#[test]
fn xep0153_namespace_matches_spec() {
    // XEP-0153 §"Protocol" pins this namespace. The
    // `vcard-temp:x:update` string is unusual — it's namespaced
    // under XEP-0054's `vcard-temp` rather than under
    // `urn:xmpp:*` — pinning the literal protects against a
    // future maintainer "modernising" the URI scheme and breaking
    // every legacy client.
    assert_eq!(NS_VCARD_UPDATE, "vcard-temp:x:update");
}

// ── §4 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0153_classifier_accepts_spec_shape_only() {
    let canonical = build_vcard_update_element(Some("deadbeef"));
    assert!(has_vcard_update(&canonical));

    let wrong_ns = Element::builder("x", "wrong:ns").build();
    assert!(!has_vcard_update(&wrong_ns));

    let wrong_name = Element::builder("update", NS_VCARD_UPDATE).build();
    assert!(!has_vcard_update(&wrong_name));
}

#[test]
fn xep0153_build_emits_x_wrapper_with_photo_child() {
    // §4 example: `<x xmlns='vcard-temp:x:update'><photo>HEX</photo></x>`.
    // The builder must pin the wrapper name + ns AND emit the
    // photo child under the same namespace (a `<photo>` in some
    // other ns would not match the spec's containment rules).
    let elem = build_vcard_update_element(Some("0123456789abcdef0123456789abcdef01234567"));

    assert_eq!(elem.name(), "x");
    assert_eq!(elem.ns(), NS_VCARD_UPDATE);

    let photo = elem
        .children()
        .find(|c| c.name() == "photo" && c.ns() == NS_VCARD_UPDATE)
        .expect("<photo> child present and namespaced under XEP-0153");
    assert_eq!(photo.text(), "0123456789abcdef0123456789abcdef01234567");
}

#[test]
fn xep0153_round_trip_preserves_hash() {
    let hash_in = "deadbeef01234567890abcdef012345678901234";
    let parsed = parse_vcard_update(&build_vcard_update_element(Some(hash_in)));
    assert_eq!(parsed.as_deref(), Some(hash_in));
}

// ── §4.1 "no avatar" sentinel ───────────────────────────────────────

#[test]
fn xep0153_empty_photo_signals_no_avatar() {
    // XEP-0153 §4.1: "If the user has no avatar (or wishes to
    // remove their avatar), they MUST send an empty <photo/>
    // element." The parser MUST distinguish this from "no
    // element at all" — the parent caller treats empty as
    // "remove cached avatar" rather than "no signal."
    let elem = build_vcard_update_element(None);

    // The wrapper is present (the broadcast carries it)…
    assert!(has_vcard_update(&elem));

    let photo = elem
        .children()
        .find(|c| c.name() == "photo" && c.ns() == NS_VCARD_UPDATE)
        .expect("<photo> child present even when empty");
    assert!(
        photo.text().is_empty(),
        "no-avatar signal MUST be an empty <photo/>, not an absent one"
    );

    // …but `parse_vcard_update` surfaces the empty text as None.
    // Consumers MUST interpret a None on a still-`has_vcard_update`
    // wrapper as "remove the cached avatar".
    assert!(
        parse_vcard_update(&elem).is_none(),
        "empty <photo/> parses to None (the removal signal)"
    );
}

#[test]
fn xep0153_classifier_distinguishes_empty_from_absent() {
    // Two distinct semantics worth pinning:
    // - element present + hash text → "this is the current hash"
    // - element present + empty photo → §4.1 "no avatar"
    // - element absent entirely → "no signal in this presence"
    //
    // `has_vcard_update` is the "is there a signal at all?" check;
    // `parse_vcard_update` is the "is there a hash to display?"
    // check. The pair must stay distinguishable for the consumer
    // to react correctly.
    let with_hash = build_vcard_update_element(Some("abc"));
    let no_avatar = build_vcard_update_element(None);
    let absent = Element::builder("presence", "jabber:client").build();

    assert!(has_vcard_update(&with_hash));
    assert!(has_vcard_update(&no_avatar));
    assert!(!has_vcard_update(&absent));

    assert_eq!(parse_vcard_update(&with_hash).as_deref(), Some("abc"));
    assert_eq!(parse_vcard_update(&no_avatar), None);
    assert_eq!(parse_vcard_update(&absent), None);
}

// ── SHA-1 hash determinism ──────────────────────────────────────────

#[test]
fn xep0153_sha1_hash_matches_known_vector() {
    // §4.1: the photo hash is the SHA-1 of the image bytes. Pin
    // against the RFC 3174 "abc" test vector so a future hash-algo
    // swap (or hex-encoding bug) trips a test.
    assert_eq!(
        compute_photo_hash(b"abc"),
        "a9993e364706816aba3e25717850c26c9cd0d89d",
    );
}

#[test]
fn xep0153_sha1_hash_is_lowercase_hex_40_chars() {
    // The hash is broadcast verbatim in every presence stanza;
    // string form must stay stable (lowercase, 40 chars, hex)
    // because peer clients use textual equality to detect avatar
    // changes.
    let hash = compute_photo_hash(b"any-bytes");
    assert_eq!(hash.len(), 40, "SHA-1 hex is 40 chars");
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "MUST be lowercase hex: got {hash}"
    );
}

#[test]
fn xep0153_sha1_hash_from_base64_matches_direct_hash() {
    // The base64 helper is what callers use when the source is
    // XEP-0054 PHOTO/BINVAL (which is base64-encoded). It must
    // produce the SAME hash as hashing the decoded bytes directly,
    // otherwise PEP avatar sync would disagree with vCard-temp
    // avatar sync about whether the avatar changed.
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = b"image bytes";
    let encoded = STANDARD.encode(bytes);

    let from_b64 = compute_photo_hash_from_base64(&encoded).expect("decodable");
    let from_raw = compute_photo_hash(bytes);
    assert_eq!(from_b64, from_raw);
}

#[test]
fn xep0153_sha1_hash_from_invalid_base64_returns_none() {
    // Defence: malformed base64 (e.g. embedded `!!!`) MUST NOT
    // panic and MUST NOT produce a hash of the raw bytes (which
    // would silently produce a wrong fingerprint). Return None
    // so the caller can decide how to react (skip, log, etc).
    assert!(compute_photo_hash_from_base64("not valid base64!!!").is_none());
}
