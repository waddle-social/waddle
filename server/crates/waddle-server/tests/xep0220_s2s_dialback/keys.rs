//! XEP-0185 dialback keys: the recommended algorithm (XEP-0185 §2) checked
//! against the worked example of XEP-0185 §3 and the examples of XEP-0220
//! §2, and constant verification.

use waddle_server::s2s::dialback::DialbackSecret;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{DialbackKey, DialbackKeys, StreamId};

fn domain(text: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(text).expect("a domain")
}

fn id(text: &str) -> StreamId {
    StreamId::parse(text).expect("a stream ID")
}

fn key(secret: &DialbackSecret, receiving: &str, originating: &str, stream: &str) -> String {
    secret
        .key(&domain(receiving), &domain(originating), &id(stream))
        .expect("a key")
        .as_str()
        .to_owned()
}

#[test]
fn keys_match_the_specification_vectors() {
    let capulet = DialbackSecret::new(b"s3cr3tf0rd14lb4ck");
    // XEP-0185 §3.
    assert_eq!(
        key(&capulet, "xmpp.example.com", "example.org", "D60000229F"),
        "37c69b1cf07a3f67c04a5ef5902fa5114f2c76fe4a2686482ba5b89323075643"
    );
    // XEP-0220 §2.1.1, Example 1.
    assert_eq!(
        key(
            &capulet,
            "montague.example",
            "capulet.example",
            "D60000229F"
        ),
        "b4835385f37fe2895af6c196b59097b16862406db80559900d96bf6fa7d23df3"
    );
    // XEP-0220 §2.2.2, the Authoritative Server's computation.
    let montague = DialbackSecret::new(b"d14lb4ck43v3r");
    assert_eq!(
        key(&montague, "capulet.example", "montague.example", "417GAF25"),
        "225cc5aa6a071133249d25fef42ae516fc7a86c523aa1c6980a7f73e784c972d"
    );
}

#[test]
fn verification_accepts_only_the_issued_key() {
    let secret = DialbackSecret::new(b"s3cr3tf0rd14lb4ck");
    let (receiving, originating, stream) = (
        domain("xmpp.example.com"),
        domain("example.org"),
        id("D60000229F"),
    );
    let issued = secret
        .key(&receiving, &originating, &stream)
        .expect("a key");
    assert!(secret.verify(&receiving, &originating, &stream, &issued));
    // Hexadecimal is case-insensitive.
    let upper = DialbackKey::parse(&issued.as_str().to_ascii_uppercase()).expect("a key");
    assert!(secret.verify(&receiving, &originating, &stream, &upper));
    // Another secret, stream ID or domain, or the domains swapped.
    let other = DialbackSecret::new(b"another deployment's secret value");
    assert!(!other.verify(&receiving, &originating, &stream, &issued));
    assert!(!secret.verify(&receiving, &originating, &id("D60000229G"), &issued));
    assert!(!secret.verify(&domain("xmpp.example.net"), &originating, &stream, &issued));
    assert!(!secret.verify(&originating, &receiving, &stream, &issued));
    // A truncated key or one with an extra character.
    let short = DialbackKey::parse(&issued.as_str()[..63]).expect("a key");
    assert!(!secret.verify(&receiving, &originating, &stream, &short));
    let long = DialbackKey::parse(&format!("{}0", issued.as_str())).expect("a key");
    assert!(!secret.verify(&receiving, &originating, &stream, &long));
}

#[test]
fn a_key_is_bounded_visible_ascii() {
    assert!(DialbackKey::parse("").is_none());
    assert!(DialbackKey::parse("  \n ").is_none());
    assert!(DialbackKey::parse("ab cd").is_none());
    assert!(DialbackKey::parse("clé").is_none());
    assert!(DialbackKey::parse(&"a".repeat(1025)).is_none());
    assert_eq!(
        DialbackKey::parse("\n  abc123\n").map(|key| key.as_str().to_owned()),
        Some("abc123".to_owned())
    );
}
