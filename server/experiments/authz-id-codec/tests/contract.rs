use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jid::BareJid;
use proptest::arbitrary::any;
use proptest::collection::vec;
use proptest::prelude::ProptestConfig;
use proptest::{prop_assert, prop_assert_eq, proptest};
use waddle_authz_id_codec_experiment::{
    CodecError, OpaqueId, ValidatedUserJid, MAX_ID_BYTES, MAX_WIRE_BYTES,
};

fn raw_wire(value: &str) -> String {
    format!("v2_{}", URL_SAFE_NO_PAD.encode(value))
}

#[test]
fn fixed_vectors_and_legacy_collisions_remain_distinct() {
    assert_eq!(OpaqueId::new("a").unwrap().to_wire().as_str(), "v2_YQ");
    assert_eq!(OpaqueId::new("ab").unwrap().to_wire().as_str(), "v2_YWI");
    assert_eq!(OpaqueId::new("abc").unwrap().to_wire().as_str(), "v2_YWJj");
    for (left, right) in [("a@b", "a|b"), ("a.b", "a=b"), ("v2_YQ", "a")] {
        let left = OpaqueId::new(left).unwrap();
        let right = OpaqueId::new(right).unwrap();
        assert_ne!(left.to_wire(), right.to_wire());
        assert_eq!(OpaqueId::from_wire(left.to_wire().as_str()).unwrap(), left);
        assert_eq!(
            OpaqueId::from_wire(right.to_wire().as_str()).unwrap(),
            right
        );
    }

    // Both survive JID parsing unchanged, but legacy '.' -> '=' aliases them.
    let dotted = ValidatedUserJid::parse_exact("a.b@example.org").unwrap();
    let equals = ValidatedUserJid::parse_exact("a=b@example.org").unwrap();
    assert_ne!(dotted.to_wire(), equals.to_wire());
    assert_eq!(
        ValidatedUserJid::from_wire(dotted.to_wire().as_str()).unwrap(),
        dotted
    );
    assert_eq!(
        ValidatedUserJid::from_wire(equals.to_wire().as_str()).unwrap(),
        equals
    );
}

#[test]
fn opaque_values_preserve_unicode_case_and_delimiters() {
    let values = [
        "Room",
        "room",
        "é",
        "e\u{301}",
        "a|b=c@d/e#f:g",
        "\0\n ",
        "🦆",
        "*",
    ];
    let mut encoded = std::collections::HashSet::new();
    for value in values {
        let id = OpaqueId::new(value).unwrap();
        assert!(encoded.insert(id.to_wire()));
        assert_eq!(
            OpaqueId::from_wire(id.to_wire().as_str()).unwrap().as_str(),
            value
        );
    }
    // A userset relation is not split out of an opaque object's identifier.
    assert_ne!(
        OpaqueId::new("space#member").unwrap().to_wire(),
        OpaqueId::new("space").unwrap().to_wire()
    );
}

#[test]
fn lengths_are_bounded_in_bytes_before_and_after_encoding() {
    for value in ["a".repeat(MAX_ID_BYTES), format!("{}a", "é".repeat(382))] {
        let id = OpaqueId::new(value.clone()).unwrap();
        assert_eq!(value.len(), MAX_ID_BYTES);
        assert_eq!(id.to_wire().as_str().len(), 1023);
        assert_eq!(OpaqueId::from_wire(id.to_wire().as_str()).unwrap(), id);
    }
    for value in ["a".repeat(MAX_ID_BYTES + 1), "é".repeat(383)] {
        assert_eq!(OpaqueId::new(&value), Err(CodecError::TooLong));
        assert_eq!(
            OpaqueId::from_wire(&raw_wire(&value)),
            Err(CodecError::TooLong)
        );
    }
    assert_eq!(OpaqueId::new(""), Err(CodecError::Empty));
    assert!(OpaqueId::from_wire(&"a".repeat(MAX_WIRE_BYTES)).is_err());
    assert_eq!(
        OpaqueId::from_wire(&"a".repeat(MAX_WIRE_BYTES + 1)),
        Err(CodecError::TooLong)
    );
}

#[test]
fn strict_decoding_rejects_alternate_spellings_and_legacy_bytes() {
    for value in ["", "a|b=c", "v1_YQ", "V2_YQ", "v3_YQ", "v2YQ"] {
        assert_eq!(
            OpaqueId::from_wire(value),
            Err(CodecError::UnsupportedVersion)
        );
    }
    for value in [
        "v2_YQ==", "v2_YQ=", "v2_YR", "v2_YWJ", "v2_A", "v2_YQ\n", "v2_ YQ", "v2_YQ/", "v2_+w",
        "v2_é", "v2_YQ\0",
    ] {
        assert_eq!(OpaqueId::from_wire(value), Err(CodecError::InvalidEncoding));
    }
    assert_eq!(OpaqueId::from_wire("v2_"), Err(CodecError::Empty));
    assert_eq!(OpaqueId::from_wire("v2__w"), Err(CodecError::InvalidUtf8));
}

#[test]
fn users_require_unchanged_bare_account_jids_on_both_boundaries() {
    for value in [
        "example.org",
        "alice@example.org/phone",
        "@example.org",
        "*",
    ] {
        assert_eq!(
            ValidatedUserJid::parse_exact(value),
            Err(CodecError::InvalidUserJid)
        );
        assert_eq!(
            ValidatedUserJid::from_wire(&raw_wire(value)),
            Err(CodecError::InvalidUserJid)
        );
    }
    for value in ["Alice@EXAMPLE.ORG", "e\u{301}@example.org"] {
        assert_eq!(
            ValidatedUserJid::parse_exact(value),
            Err(CodecError::UserJidChangedByParser)
        );
        assert_eq!(
            ValidatedUserJid::from_wire(&raw_wire(value)),
            Err(CodecError::UserJidChangedByParser)
        );
    }
    let parsed = BareJid::new("Alice@EXAMPLE.ORG").unwrap();
    let validated = ValidatedUserJid::new(parsed).unwrap();
    assert_eq!(validated.jid().as_str(), "alice@example.org");
    assert_eq!(
        ValidatedUserJid::from_wire(validated.to_wire().as_str()).unwrap(),
        validated
    );
    assert_eq!(
        ValidatedUserJid::new(BareJid::new("example.org").unwrap()),
        Err(CodecError::InvalidUserJid)
    );
}

#[test]
fn domain_aliases_remain_distinct_without_authoritative_canonicalization() {
    // These assertions expose the parser's limitation, not an authorization
    // policy: accepted, unequal spellings need not represent different users.
    for (left, right) in [
        ("a@example.org", "a@example.org."),
        ("a@bücher.example", "a@xn--bcher-kva.example"),
    ] {
        let left = ValidatedUserJid::parse_exact(left).unwrap();
        let right = ValidatedUserJid::parse_exact(right).unwrap();
        assert_ne!(left.jid(), right.jid());
        assert_ne!(left, right);
        assert_ne!(left.to_wire(), right.to_wire());
        assert_eq!(
            ValidatedUserJid::from_wire(left.to_wire().as_str()).unwrap(),
            left
        );
        assert_eq!(
            ValidatedUserJid::from_wire(right.to_wire().as_str()).unwrap(),
            right
        );
    }
}

#[test]
fn valid_jids_beyond_the_codec_limit_are_explicitly_unsupported() {
    // Both localparts satisfy the JID parser; only the first fits this codec.
    let accepted = BareJid::new(&format!("{}@x", "a".repeat(MAX_ID_BYTES - 2))).unwrap();
    let rejected = BareJid::new(&format!("{}@x", "a".repeat(MAX_ID_BYTES - 1))).unwrap();
    let accepted = ValidatedUserJid::new(accepted).unwrap();
    assert_eq!(accepted.to_wire().as_str().len(), 1023);
    assert_eq!(
        ValidatedUserJid::new(rejected.clone()),
        Err(CodecError::TooLong)
    );
    assert_eq!(
        ValidatedUserJid::parse_exact(rejected.as_str()),
        Err(CodecError::TooLong)
    );
    assert_eq!(
        ValidatedUserJid::from_wire(&raw_wire(rejected.as_str())),
        Err(CodecError::TooLong)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn unicode_roundtrips_and_encoding_is_injective(
        left in vec(any::<char>(), 1..192),
        right in vec(any::<char>(), 1..192),
    ) {
        let left = OpaqueId::new(left.into_iter().collect::<String>()).unwrap();
        let right = OpaqueId::new(right.into_iter().collect::<String>()).unwrap();
        let encoded = left.to_wire();
        prop_assert!(encoded.as_str().len() <= MAX_WIRE_BYTES);
        prop_assert!(encoded.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
        prop_assert_eq!(OpaqueId::from_wire(encoded.as_str()).unwrap(), left.clone());
        prop_assert_eq!(encoded == right.to_wire(), left == right);
    }

    #[test]
    fn validated_user_representations_roundtrip(localpart in "[a-z0-9][a-z0-9.=|_-]{0,60}") {
        let jid = format!("{localpart}@example.org");
        let user = ValidatedUserJid::parse_exact(&jid).unwrap();
        prop_assert_eq!(ValidatedUserJid::from_wire(user.to_wire().as_str()).unwrap(), user);
    }

    #[test]
    fn generated_wires_reject_padding_whitespace_and_other_versions(value in vec(any::<char>(), 1..192)) {
        let id = OpaqueId::new(value.into_iter().collect::<String>()).unwrap();
        let wire = id.to_wire();
        for suffix in ["=", "\n", " "] {
            let malformed = format!("{}{suffix}", wire.as_str());
            prop_assert!(OpaqueId::from_wire(&malformed).is_err());
        }
        let unknown_version = wire.as_str().replacen("v2_", "v3_", 1);
        prop_assert_eq!(OpaqueId::from_wire(&unknown_version), Err(CodecError::UnsupportedVersion));
    }
}
