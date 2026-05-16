//! XEP-0300: Use of Cryptographic Hash Functions — dedicated suite.
//!
//! Pins the audit-level invariants:
//!
//! - §3 namespace string `urn:xmpp:hashes:2`,
//! - §"Recommended Hash Functions" algorithm strings on the wire
//!   (`sha-1` / `sha-256` / `sha-512` — the `-` is part of the
//!   spec ID, not a typo),
//! - §4 wire shape: `<hash xmlns='urn:xmpp:hashes:2' algo='…'>BASE64</hash>`,
//! - hash determinism against the spec-required output lengths
//!   and well-known test vectors (catches algorithm swap or
//!   wrong-encoding bugs),
//! - parser rejects wrong-ns, wrong-name, missing/unknown algo,
//!   and bad base64,
//! - `verify_hash` correctly compares hashes against data.

use minidom::Element;
use waddle_xmpp::xep::xep0300::{
    build_hash_element, compute_hash, parse_hash_element, sha1_hex, sha256_base64, sha256_hex,
    verify_hash, HashAlgo, HashError, HashValue, Hashable, NS_HASHES,
};

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0300_namespace_matches_spec_v2() {
    // §3 pins `urn:xmpp:hashes:2`. The `:2` versioning is the
    // post-MD5 / post-SHA-1-deprecation revision — clients
    // dispatch on the version, so a typo silently drops every
    // hash payload.
    assert_eq!(NS_HASHES, "urn:xmpp:hashes:2");
}

// ── §"Recommended Hash Functions" algorithm strings ─────────────────

#[test]
fn xep0300_algorithm_enum_serialises_to_spec_strings() {
    // §"Recommended Hash Functions" pins the strings: `sha-1`,
    // `sha-256`, `sha-512`. The hyphen is part of the spec
    // identifier (not `sha1`/`sha256`). A typo here breaks every
    // peer's hash-name dispatch.
    assert_eq!(HashAlgo::Sha1.as_str(), "sha-1");
    assert_eq!(HashAlgo::Sha256.as_str(), "sha-256");
    assert_eq!(HashAlgo::Sha512.as_str(), "sha-512");
}

#[test]
fn xep0300_algorithm_enum_round_trips_every_supported_string() {
    for algo in [HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Sha512] {
        assert_eq!(
            HashAlgo::from_algo_str(algo.as_str()).expect("round-trips"),
            algo
        );
    }
}

#[test]
fn xep0300_algorithm_enum_rejects_unknown_strings() {
    // Unknown algos surface as None so callers can return
    // `UnsupportedAlgorithm` — the spec requires graceful
    // handling of unsupported hash names, not silent acceptance.
    assert_eq!(HashAlgo::from_algo_str(""), None);
    assert_eq!(HashAlgo::from_algo_str("md5"), None);
    assert_eq!(HashAlgo::from_algo_str("SHA-256"), None); // case-sensitive per spec
    assert_eq!(HashAlgo::from_algo_str("sha256"), None); // hyphen required
}

#[test]
fn xep0300_algorithm_output_lengths_match_spec_octet_sizes() {
    // The output-length invariants are what consumers use to
    // validate a parsed hash payload — a hash whose byte length
    // doesn't match the named algo is corrupt.
    assert_eq!(HashAlgo::Sha1.output_len(), 20);
    assert_eq!(HashAlgo::Sha256.output_len(), 32);
    assert_eq!(HashAlgo::Sha512.output_len(), 64);
}

// ── Hash determinism (known test vectors) ───────────────────────────

#[test]
fn xep0300_sha1_matches_rfc3174_abc_vector() {
    // RFC 3174 test vector — guards against an accidental hash
    // algo swap or wrong-encoding bug.
    let hash = compute_hash(HashAlgo::Sha1, b"abc");
    assert_eq!(hash.to_hex(), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(hash.bytes.len(), HashAlgo::Sha1.output_len());
}

#[test]
fn xep0300_sha256_matches_fips_180_2_vectors() {
    // SHA-256("") is one of the most-cited cryptographic constants.
    let empty = compute_hash(HashAlgo::Sha256, b"");
    assert_eq!(
        empty.to_hex(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
    assert_eq!(empty.bytes.len(), HashAlgo::Sha256.output_len());

    // SHA-256("abc") from FIPS 180-2 Annex A.
    let abc = compute_hash(HashAlgo::Sha256, b"abc");
    assert_eq!(
        abc.to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
}

#[test]
fn xep0300_sha512_matches_fips_180_2_vector() {
    let abc = compute_hash(HashAlgo::Sha512, b"abc");
    assert_eq!(abc.to_hex(),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f");
    assert_eq!(abc.bytes.len(), HashAlgo::Sha512.output_len());
}

#[test]
fn xep0300_sha256_helpers_are_consistent_with_compute_hash() {
    // The convenience helpers `sha256_base64` / `sha256_hex` /
    // `sha1_hex` are shortcuts MAM/avatar/file-sharing code uses
    // directly. They must produce identical output to going
    // through `compute_hash`.
    let bytes = b"hello world";

    assert_eq!(
        sha256_hex(bytes),
        compute_hash(HashAlgo::Sha256, bytes).to_hex()
    );
    assert_eq!(
        sha256_base64(bytes),
        compute_hash(HashAlgo::Sha256, bytes).to_base64()
    );
    assert_eq!(
        sha1_hex(bytes),
        compute_hash(HashAlgo::Sha1, bytes).to_hex()
    );
}

#[test]
fn xep0300_hashable_trait_covers_common_byte_carriers() {
    // The Hashable trait is implemented for &[u8], &str, String,
    // Vec<u8>. Callers should be able to hash any of those
    // without explicit conversion.
    let expected = compute_hash(HashAlgo::Sha256, b"data");

    assert_eq!(b"data".as_slice().sha256(), expected);
    assert_eq!("data".sha256(), expected);
    assert_eq!(String::from("data").sha256(), expected);
    assert_eq!(b"data".to_vec().sha256(), expected);
}

// ── §4 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0300_build_hash_element_emits_spec_shape() {
    // §4 example: `<hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>BASE64</hash>`.
    // The algorithm name is an attribute; the value is the
    // base64-encoded bytes as the element's text content.
    let hash = compute_hash(HashAlgo::Sha256, b"hello");
    let elem = build_hash_element(&hash);

    assert_eq!(elem.name(), "hash");
    assert_eq!(elem.ns(), NS_HASHES);
    assert_eq!(elem.attr("algo"), Some("sha-256"));
    assert_eq!(elem.text(), hash.to_base64());
}

#[test]
fn xep0300_hash_element_round_trips() {
    let original = compute_hash(HashAlgo::Sha512, b"audit-pass");
    let elem = build_hash_element(&original);
    let parsed = parse_hash_element(&elem).expect("round-trips");

    assert_eq!(parsed.algo, HashAlgo::Sha512);
    assert_eq!(parsed.bytes, original.bytes);
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0300_parse_rejects_wrong_namespace() {
    let elem = Element::builder("hash", "attacker:ns")
        .attr("algo", "sha-256")
        .build();
    assert!(matches!(
        parse_hash_element(&elem),
        Err(HashError::UnsupportedAlgorithm(_))
    ));
}

#[test]
fn xep0300_parse_rejects_wrong_element_name() {
    let elem = Element::builder("digest", NS_HASHES)
        .attr("algo", "sha-256")
        .build();
    assert!(matches!(
        parse_hash_element(&elem),
        Err(HashError::UnsupportedAlgorithm(_))
    ));
}

#[test]
fn xep0300_parse_rejects_missing_algo_attribute() {
    // §4 makes `algo` REQUIRED. Without it the consumer can't
    // verify the hash; parser MUST refuse.
    let elem = Element::builder("hash", NS_HASHES).build();
    assert!(matches!(
        parse_hash_element(&elem),
        Err(HashError::UnsupportedAlgorithm(_))
    ));
}

#[test]
fn xep0300_parse_rejects_unknown_algo_attribute() {
    let elem = Element::builder("hash", NS_HASHES)
        .attr("algo", "md5") // §"Recommended Hash Functions" excludes MD5
        .build();
    assert!(matches!(
        parse_hash_element(&elem),
        Err(HashError::UnsupportedAlgorithm(name)) if name == "md5"
    ));
}

#[test]
fn xep0300_parse_rejects_invalid_base64_text() {
    // Hash bytes are base64-encoded per §4. Malformed base64
    // (e.g. `!!!`) MUST NOT panic; surface as InvalidBase64 so
    // the caller can fall through to a "treat as no hash" path.
    let elem = Element::builder("hash", NS_HASHES)
        .attr("algo", "sha-256")
        .append("!!!not-base64!!!")
        .build();
    assert!(matches!(
        parse_hash_element(&elem),
        Err(HashError::InvalidBase64(_))
    ));
}

// ── Verification ────────────────────────────────────────────────────

#[test]
fn xep0300_verify_hash_returns_true_for_matching_data() {
    let hash = compute_hash(HashAlgo::Sha256, b"correct data");
    assert!(verify_hash(&hash, b"correct data"));
}

#[test]
fn xep0300_verify_hash_returns_false_for_mismatching_data() {
    let hash = compute_hash(HashAlgo::Sha256, b"original data");
    // Even a single-byte change must produce a different hash —
    // this is the integrity guarantee that XEP-0300 hashes
    // exist to provide.
    assert!(!verify_hash(&hash, b"original Data"));
    assert!(!verify_hash(&hash, b""));
}

#[test]
fn xep0300_verify_hash_uses_the_named_algo_not_the_byte_length() {
    // A 32-byte hash labelled sha-256 and a 32-byte hash labelled
    // sha-512 (truncated, in practice) are DIFFERENT verifications.
    // `verify_hash` MUST hash with the carried algo, not just
    // compare bytes.
    let sha256 = compute_hash(HashAlgo::Sha256, b"data");
    let lied = HashValue::new(HashAlgo::Sha512, sha256.bytes.clone());
    assert!(!verify_hash(&lied, b"data"));
}

// ── Base64 round-trip ───────────────────────────────────────────────

#[test]
fn xep0300_hash_value_base64_round_trip() {
    let original = compute_hash(HashAlgo::Sha256, b"some bytes");
    let b64 = original.to_base64();
    let restored = HashValue::from_base64(HashAlgo::Sha256, &b64).expect("decodes");
    assert_eq!(restored.bytes, original.bytes);
}

#[test]
fn xep0300_hash_value_from_base64_rejects_invalid_input() {
    assert!(matches!(
        HashValue::from_base64(HashAlgo::Sha256, "not valid base64!!!"),
        Err(HashError::InvalidBase64(_))
    ));
}
