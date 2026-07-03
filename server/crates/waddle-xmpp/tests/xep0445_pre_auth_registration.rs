//! XEP-0445: Pre-Authenticated In-Band Registration — dedicated suite.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:pars:0` (xep-0445.xml pins the
//!   `<preauth/>` element in exactly this namespace),
//! - the `<preauth token='...'/>` wire shape used inside the XEP-0077
//!   `jabber:iq:register` query,
//! - the extraction rules: missing element, missing/empty `token`
//!   attribute, and wrong-namespace lookalikes are all rejected,
//! - `PreauthValidation` error-surface semantics (only `Valid` passes;
//!   every failure variant carries a human-readable message).

use minidom::Element;
use waddle_xmpp::xep::xep0445::{
    build_preauth_element, extract_preauth, has_preauth, is_preauth_element, PreauthToken,
    PreauthValidation, NS_PARS,
};

const NS_REGISTER: &str = "jabber:iq:register";

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0445_namespace_matches_spec() {
    // xep-0445.xml: `<preauth xmlns='urn:xmpp:pars:0' token='TOKEN'/>`.
    // Servers dispatch registration preauth on this string.
    assert_eq!(NS_PARS, "urn:xmpp:pars:0");
}

// ── Wire-shape round-trip ────────────────────────────────────────────

#[test]
fn xep0445_builder_round_trips_through_serialization() {
    let elem = build_preauth_element("invite-token-abc");
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_preauth_element(&reparsed));
    assert_eq!(reparsed.name(), "preauth");
    assert_eq!(reparsed.ns(), NS_PARS);
    assert_eq!(reparsed.attr("token"), Some("invite-token-abc"));
}

#[test]
fn xep0445_extracts_token_from_spec_shaped_register_query() {
    // The XEP-0077 registration set carrying the invite token — the
    // exact shape a fresh client submits when following an invite URI.
    let query: Element = "<query xmlns='jabber:iq:register'>\
            <username>newuser</username>\
            <password>secret</password>\
            <preauth xmlns='urn:xmpp:pars:0' token='invite-token-abc'/>\
        </query>"
        .parse()
        .expect("valid register query");

    assert!(has_preauth(&query));
    let token = extract_preauth(&query).expect("token present");
    assert_eq!(token, PreauthToken::new("invite-token-abc"));
    assert_eq!(token.to_string(), "invite-token-abc");
}

#[test]
fn xep0445_extract_round_trips_builder_output_inside_query() {
    let mut query = Element::builder("query", NS_REGISTER).build();
    query.append_child(build_preauth_element("tok-round-trip"));

    let xml = String::from(&query);
    let reparsed: Element = xml.parse().expect("query reparses");

    let token = extract_preauth(&reparsed).expect("token survives round-trip");
    assert_eq!(token.token, "tok-round-trip");
}

// ── Extraction robustness ────────────────────────────────────────────

#[test]
fn xep0445_query_without_preauth_yields_none() {
    let query: Element = "<query xmlns='jabber:iq:register'>\
            <username>plain</username>\
        </query>"
        .parse()
        .expect("valid query");

    assert!(!has_preauth(&query));
    assert!(extract_preauth(&query).is_none());
}

#[test]
fn xep0445_empty_token_attribute_is_rejected() {
    // `token=''` carries no invite. Accepting it would let an empty
    // string reach the token validator and potentially match a
    // degenerate stored token.
    let query: Element =
        "<query xmlns='jabber:iq:register'><preauth xmlns='urn:xmpp:pars:0' token=''/></query>"
            .parse()
            .expect("valid xml");
    assert!(extract_preauth(&query).is_none());
}

#[test]
fn xep0445_missing_token_attribute_is_rejected() {
    let query: Element =
        "<query xmlns='jabber:iq:register'><preauth xmlns='urn:xmpp:pars:0'/></query>"
            .parse()
            .expect("valid xml");
    assert!(extract_preauth(&query).is_none());
}

#[test]
fn xep0445_wrong_namespace_preauth_is_not_recognized() {
    // A `<preauth>` in a foreign namespace must not be treated as a
    // XEP-0445 token — the namespace is the dispatch key.
    let query: Element =
        "<query xmlns='jabber:iq:register'><preauth xmlns='urn:xmpp:evil:0' token='x'/></query>"
            .parse()
            .expect("valid xml");

    assert!(!has_preauth(&query));
    assert!(extract_preauth(&query).is_none());

    let lookalike = Element::builder("preauth", "urn:xmpp:pars:1").build();
    assert!(!is_preauth_element(&lookalike));
}

#[test]
fn xep0445_wrong_element_name_in_pars_namespace_is_not_recognized() {
    let wrong = Element::builder("token", NS_PARS).build();
    assert!(!is_preauth_element(&wrong));
}

#[test]
fn xep0445_first_preauth_wins_when_duplicated() {
    // Duplicate `<preauth/>` children are already malformed input;
    // the extractor's contract is deterministic first-match so the
    // server validates exactly one token.
    let query: Element = "<query xmlns='jabber:iq:register'>\
            <preauth xmlns='urn:xmpp:pars:0' token='first'/>\
            <preauth xmlns='urn:xmpp:pars:0' token='second'/>\
        </query>"
        .parse()
        .expect("valid xml");

    let token = extract_preauth(&query).expect("token present");
    assert_eq!(token.token, "first");
}

// ── Validation surface ───────────────────────────────────────────────

#[test]
fn xep0445_only_valid_outcome_permits_registration() {
    assert!(PreauthValidation::Valid.is_valid());
    for outcome in [
        PreauthValidation::InvalidToken,
        PreauthValidation::AlreadyUsed,
        PreauthValidation::Expired,
        PreauthValidation::Required,
    ] {
        assert!(!outcome.is_valid(), "{outcome:?} must not validate");
    }
}

#[test]
fn xep0445_every_failure_variant_has_an_error_message() {
    assert!(PreauthValidation::Valid.error_message().is_none());
    for outcome in [
        PreauthValidation::InvalidToken,
        PreauthValidation::AlreadyUsed,
        PreauthValidation::Expired,
        PreauthValidation::Required,
    ] {
        let msg = outcome
            .error_message()
            .unwrap_or_else(|| panic!("{outcome:?} must carry an error message"));
        assert!(!msg.is_empty());
    }
}
