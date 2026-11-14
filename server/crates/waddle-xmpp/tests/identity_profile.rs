//! The `waddle-identity-v1` identity profile: canonical domains, refusal of
//! spellings that preparation would fold, and its enforcement on decoded
//! C2S frames before typed parsing.

use jid::{BareJid, Jid};
use waddle_xmpp::auth::{parse_oauthbearer, OAuthBearerParseError};
use waddle_xmpp::identity::{
    canonical_jid, parse_bare_jid, parse_full_jid, parse_jid, parse_localpart, parse_resourcepart,
    AccountLocalpart, CanonicalBareJid, CanonicalDomain, DomainKind, IdentityError,
    ACCOUNT_LOCALPART_MAX_BYTES,
};
use waddle_xmpp::protocol::frame::{parse_frame, InboundFrame, ParseError};
use waddle_xmpp::Stanza;

fn domain(text: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(text).expect("canonical spelling")
}

#[test]
fn one_trailing_dot_is_removed_before_anything_else() {
    assert_eq!(domain("example.org."), domain("example.org"));
    assert_eq!(domain("example.org.").as_str(), "example.org");
    assert_eq!(domain("localhost.").as_str(), "localhost");
    for input in ["example.org..", ".", "", "..example.org"] {
        assert!(CanonicalDomain::parse_exact(input).is_err(), "{input:?}");
    }
}

#[test]
fn u_label_and_a_label_spellings_are_one_domain_in_a_label_form() {
    let unicode = domain("bücher.example");
    let ascii = domain("xn--bcher-kva.example");
    assert_eq!(unicode, ascii);
    assert_eq!(unicode.as_str(), "xn--bcher-kva.example");
    assert_eq!(unicode.to_unicode(), "bücher.example");
    assert_eq!(unicode.kind(), DomainKind::Dns);
    // Labels may mix forms; each must be one of the two spellings.
    assert_eq!(
        domain("muc.bücher.example."),
        domain("muc.xn--bcher-kva.example")
    );
    assert_eq!(
        parse_jid("alice@bücher.example./phone")
            .expect("canonical address")
            .as_str(),
        "alice@xn--bcher-kva.example/phone"
    );
}

#[test]
fn idna2008_keeps_domains_apart_that_the_pinned_parser_folds() {
    // Nameprep (IDNA2003) maps `ß` to `ss`; IDNA2008 keeps them distinct.
    let parser_only = BareJid::new("a@faß.example").expect("parser accepts");
    assert_eq!(parser_only.as_str(), "a@fass.example");
    let sharp = domain("faß.example");
    assert_eq!(sharp.as_str(), "xn--fa-hia.example");
    assert_ne!(sharp, domain("fass.example"));
    assert_eq!(
        parse_bare_jid("a@faß.example").expect("canonical").as_str(),
        "a@xn--fa-hia.example"
    );
}

#[test]
fn domain_spellings_preparation_would_change_are_refused() {
    for input in [
        "Example.org",
        "EXAMPLE.ORG.",
        "ｅｘａｍｐｌｅ.org",
        "XN--BCHER-KVA.example",
        "Bücher.example",
        "bücher。example",
        "exa_mple.org",
    ] {
        assert!(
            matches!(
                CanonicalDomain::parse_exact(input),
                Err(IdentityError::NonCanonicalDomain | IdentityError::InvalidDomain)
            ),
            "{input:?}"
        );
    }
    // Configuration entries are prepared instead, with the same result.
    assert_eq!(
        CanonicalDomain::prepare("EXAMPLE.org.").expect("prepared"),
        domain("example.org")
    );
    assert_eq!(
        CanonicalDomain::prepare("Bücher.Example").expect("prepared"),
        domain("xn--bcher-kva.example")
    );
    assert!(CanonicalDomain::prepare("exa_mple.org").is_err());
}

#[test]
fn ip_literals_have_one_canonical_text() {
    assert_eq!(domain("127.0.0.1").kind(), DomainKind::Ipv4);
    assert_eq!(domain("[::1]").kind(), DomainKind::Ipv6);
    assert_eq!(domain("127.0.0.1.").as_str(), "127.0.0.1");
    for input in [
        "[0:0:0:0:0:0:0:1]",
        "[::1",
        "01.2.3.4",
        "1.2.3",
        "[::FFFF:1.2.3.4]",
    ] {
        assert!(CanonicalDomain::parse_exact(input).is_err(), "{input:?}");
    }
    assert_eq!(
        CanonicalDomain::prepare("[0:0:0:0:0:0:0:1]")
            .expect("prepared")
            .as_str(),
        "[::1]"
    );
}

#[test]
fn subdomain_coverage_is_label_by_label() {
    let parent = domain("example.net");
    assert!(domain("example.net").is_within(&parent));
    assert!(domain("muc.example.net").is_within(&parent));
    assert!(domain("a.b.example.net.").is_within(&parent));
    assert!(!domain("badexample.net").is_within(&parent));
    assert!(!domain("example.net.evil").is_within(&parent));
    assert!(!domain("[::1]").is_within(&domain("localhost")));
}

#[test]
fn localpart_aliasing_refusals() {
    for (input, why) in [
        ("Alice", "case"),
        ("ALICE", "case"),
        ("ａｌｉｃｅ", "width"),
        ("\u{fb01}x", "compatibility ligature"),
        ("\u{2167}", "compatibility numeral"),
        ("\u{210c}ello", "compatibility letter"),
        ("fußball", "sharp s"),
        ("λόγος", "final sigma"),
        ("ali\u{ad}ce", "soft hyphen mapped to nothing"),
        ("ali\u{200d}ce", "zero-width joiner mapped to nothing"),
        ("e\u{301}", "decomposed sequence"),
    ] {
        assert_eq!(
            parse_localpart(input),
            Err(IdentityError::NonCanonicalLocalpart),
            "{why}: {input:?}"
        );
        let address = format!("{input}@example.org");
        assert_eq!(
            parse_jid(&address),
            Err(IdentityError::NonCanonicalLocalpart),
            "{why}"
        );
    }
    for input in ["alice", "josé", "strasse", "alice+work"] {
        assert_eq!(parse_localpart(input).expect("canonical").as_str(), input);
    }
    for input in ["", "a b", "a@b", "a:b"] {
        assert!(parse_localpart(input).is_err(), "{input:?}");
    }
}

#[test]
fn resourceparts_must_be_unchanged_by_preparation() {
    for input in ["phone", "My Phone", "ws-1", "user@host"] {
        assert_eq!(
            parse_resourcepart(input).expect("canonical").as_str(),
            input
        );
    }
    for input in ["ｗｅｂ", "\u{fb01}le", "ph\u{ad}one"] {
        assert_eq!(
            parse_resourcepart(input),
            Err(IdentityError::NonCanonicalResourcepart),
            "{input:?}"
        );
    }
    assert_eq!(
        parse_full_jid("alice@example.org").err(),
        Some(IdentityError::NotFull)
    );
    assert_eq!(
        parse_bare_jid("alice@example.org/phone").err(),
        Some(IdentityError::NotBare)
    );
}

#[test]
fn canonical_bare_jids_have_exactly_one_accepted_text() {
    let jid = CanonicalBareJid::parse_exact("alice@example.org").expect("canonical");
    assert_eq!(jid.as_str(), "alice@example.org");
    for input in [
        "alice@example.org.",
        "alice@bücher.example",
        "Alice@example.org",
    ] {
        assert!(CanonicalBareJid::parse_exact(input).is_err(), "{input:?}");
    }
    // A JID the parser already prepared keeps a trailing dot and U-labels in
    // its text; `new` refuses them and `canonicalize` maps them.
    let parsed = BareJid::new("alice@bücher.example.").expect("parser accepts");
    assert!(CanonicalBareJid::new(parsed.clone()).is_err());
    assert_eq!(
        CanonicalBareJid::canonicalize(&parsed)
            .expect("canonical form")
            .as_str(),
        "alice@xn--bcher-kva.example"
    );
    let full: Jid = "alice@example.org./phone".parse().expect("parser accepts");
    assert_eq!(
        canonical_jid(&full).expect("canonical").as_str(),
        "alice@example.org/phone"
    );
}

#[test]
fn account_profile_is_a_fixed_ascii_set() {
    let domain = domain("example.org");
    for input in ["alice", "alice+work", "a.b_c-d", "x", "0"] {
        let localpart = AccountLocalpart::parse(input).expect("account profile");
        assert_eq!(
            localpart.account_jid(&domain).as_str(),
            format!("{input}@example.org")
        );
    }
    let longest = "a".repeat(ACCOUNT_LOCALPART_MAX_BYTES);
    assert!(AccountLocalpart::parse(&longest).is_ok());
    for input in [
        String::new(),
        "a".repeat(ACCOUNT_LOCALPART_MAX_BYTES + 1),
        "Alice".to_owned(),
        "josé".to_owned(),
        "a b".to_owned(),
        "a\\40b".to_owned(),
        "a!b".to_owned(),
    ] {
        assert_eq!(
            AccountLocalpart::parse(&input),
            Err(IdentityError::OutsideAccountProfile),
            "{input:?}"
        );
    }
}

fn stanza(frame: &str) -> Stanza {
    match parse_frame(frame).expect("frame parses") {
        InboundFrame::Stanza(stanza) => *stanza,
        other => panic!("expected a stanza, got {other:?}"),
    }
}

fn refusal(frame: &str) -> (&'static str, IdentityError) {
    match parse_frame(frame) {
        Err(ParseError::IdentityProfile { kind, error }) => (kind, error),
        other => panic!("expected an identity refusal, got {other:?}"),
    }
}

#[test]
fn decoded_stanza_addresses_are_canonicalized_before_typed_parsing() {
    let Stanza::Iq(iq) = stanza(
        "<iq type='get' id='q1' to='pubsub.b&#xFC;cher.example.'><query xmlns='http://jabber.org/protocol/disco#info'/></iq>",
    ) else {
        panic!("expected an IQ");
    };
    assert_eq!(
        iq.to().map(Jid::as_str),
        Some("pubsub.xn--bcher-kva.example")
    );
    let Stanza::Message(message) =
        stanza("<message to='bob@example.org.' type='chat'><body>hi</body></message>")
    else {
        panic!("expected a message");
    };
    assert_eq!(
        message.to.as_ref().map(Jid::as_str),
        Some("bob@example.org")
    );
}

#[test]
fn decoded_stanza_addresses_outside_the_profile_refuse_the_stanza() {
    assert_eq!(
        refusal("<message to='Bob@example.org' type='chat'><body>hi</body></message>"),
        ("message", IdentityError::NonCanonicalLocalpart)
    );
    // A character reference decodes before the check, so it cannot hide a
    // spelling the parser would fold.
    assert_eq!(
        refusal("<presence to='stra&#xDF;e@example.org'/>"),
        ("presence", IdentityError::NonCanonicalLocalpart)
    );
    assert_eq!(
        refusal("<iq type='get' id='q2' to='EXAMPLE.org'><ping xmlns='urn:xmpp:ping'/></iq>"),
        ("iq", IdentityError::NonCanonicalDomain)
    );
    assert_eq!(
        refusal("<iq type='set' id='q3' to='room@muc.example.org/&#xFF57;eb'><query xmlns='jabber:iq:version'/></iq>"),
        ("iq", IdentityError::NonCanonicalResourcepart)
    );
    assert_eq!(
        refusal("<message from='ALICE@example.org/phone' to='bob@example.org'/>"),
        ("message", IdentityError::NonCanonicalLocalpart)
    );
}

#[test]
fn payload_jid_attributes_are_held_to_the_same_profile() {
    assert_eq!(
        refusal(
            "<iq type='set' id='r1'><query xmlns='jabber:iq:roster'><item jid='Bob@example.org'/></query></iq>"
        ),
        ("iq", IdentityError::NonCanonicalLocalpart)
    );
    assert_eq!(
        refusal(
            "<iq type='set' id='m1' to='room@muc.example.org'><query xmlns='http://jabber.org/protocol/muc#admin'><item affiliation='member' jid='fu&#xDF;ball@example.org'/></query></iq>"
        ),
        ("iq", IdentityError::NonCanonicalLocalpart)
    );
    let Stanza::Iq(iq) = stanza(
        "<iq type='set' id='r2'><query xmlns='jabber:iq:roster'><item jid='bob@b&#xFC;cher.example.'/></query></iq>",
    ) else {
        panic!("expected an IQ");
    };
    let xmpp_parsers::iq::Iq::Set { payload, .. } = *iq else {
        panic!("expected an IQ set");
    };
    let item = payload
        .get_child("item", "jabber:iq:roster")
        .expect("roster item");
    assert_eq!(item.attr("jid"), Some("bob@xn--bcher-kva.example"));
}

#[test]
fn oauthbearer_authzid_must_be_a_canonical_account_jid() {
    for authzid in [
        "Alice@example.org",
        "alice@EXAMPLE.org",
        "stra\u{df}e@example.org",
    ] {
        let mut payload = format!("n,a={authzid},").into_bytes();
        payload.extend_from_slice(b"\x01auth=Bearer token\x01\x01");
        assert!(
            matches!(
                parse_oauthbearer(&payload),
                Err(OAuthBearerParseError::InvalidAuthzid)
            ),
            "{authzid:?}"
        );
    }
}
