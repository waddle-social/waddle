//! XEP-0054: vcard-temp — dedicated conformance suite.
//!
//! Pins the audit-level invariants for the live XEP-0054 wire path. The
//! in-crate `xep::xep0054::tests` module covers helper internals
//! (element parsing, builder coverage, error display); this file pins
//! what the spec and Waddle's project rules require to hold at the
//! public-API boundary:
//!
//! - §3 namespace string.
//! - XEP-0054 advertisement obligation: every server's disco#info MUST
//!   list `vcard-temp` (the spec is universally implemented and clients
//!   probe for it).
//! - §3.1 / §3.2 IQ shapes:
//!   - `is_vcard_get` accepts ONLY `iq/type='get'` with `<vCard
//!     xmlns='vcard-temp'/>` child; rejects `set` and wrong shapes.
//!   - `is_vcard_set` accepts ONLY `iq/type='set'` with same child;
//!     rejects `get` and wrong shapes.
//!   - `build_vcard_success` returns the §3.2 acknowledgement: an
//!     `iq/type='result'` with the original id, no payload.

use minidom::Element;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::xep::xep0054::{build_vcard_success, is_vcard_get, is_vcard_set, NS_VCARD};
use xmpp_parsers::iq::{Iq, IqType};

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0054_namespace_matches_spec() {
    // XEP-0054 §3 fixes the namespace string. Clients dispatch on
    // this exact literal; deviation drops the IQ into "unknown
    // payload" routing.
    assert_eq!(NS_VCARD, "vcard-temp");
}

// ── Server disco advertisement ───────────────────────────────────────

#[test]
fn xep0054_server_features_advertise_vcard_temp() {
    // XEP-0054 is a near-universal compatibility surface: clients
    // probe `vcard-temp` to know whether the server stores vCards at
    // all. Without the advert, the spec-correct PEP-only client
    // (XEP-0292) and the legacy client both fail to discover the
    // capability.
    let feats = server_features();
    let target = Feature::vcard();
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `vcard-temp` for XEP-0054 compatibility"
    );
}

#[test]
fn xep0054_feature_constructor_pins_namespace_string() {
    // Defence-in-depth against a future constructor rename that
    // accidentally changes the wire string.
    assert_eq!(Feature::vcard().0, "vcard-temp");
}

// ── §3.1 IQ get classifier ───────────────────────────────────────────

fn vcard_payload() -> Element {
    Element::builder("vCard", NS_VCARD).build()
}

#[test]
fn xep0054_is_vcard_get_accepts_spec_shape() {
    // XEP-0054 §3.1: client retrieves its own vCard via
    //   <iq type='get'><vCard xmlns='vcard-temp'/></iq>
    let iq = Iq {
        from: None,
        to: None,
        id: "v1".into(),
        payload: IqType::Get(vcard_payload()),
    };
    assert!(is_vcard_get(&iq));
    assert!(!is_vcard_set(&iq));
}

#[test]
fn xep0054_is_vcard_get_rejects_wrong_iq_type() {
    // A set with the same payload is the §3.2 update path, not get.
    let iq = Iq {
        from: None,
        to: None,
        id: "v2".into(),
        payload: IqType::Set(vcard_payload()),
    };
    assert!(!is_vcard_get(&iq));
    assert!(is_vcard_set(&iq));
}

#[test]
fn xep0054_classifier_rejects_wrong_payload_namespace() {
    // Wrong-ns `vCard` (e.g. an attacker putting an XEP-0054-shaped
    // element under a different namespace to confuse the routing).
    let elem = Element::builder("vCard", "wrong:namespace").build();
    let iq = Iq {
        from: None,
        to: None,
        id: "v3".into(),
        payload: IqType::Get(elem),
    };
    assert!(!is_vcard_get(&iq));
    assert!(!is_vcard_set(&iq));
}

#[test]
fn xep0054_classifier_rejects_wrong_payload_element_name() {
    // Right ns, wrong local name (a `<query/>` in `vcard-temp` is
    // not the spec-defined element — XEP-0054 §3 uses `<vCard/>`).
    let elem = Element::builder("query", NS_VCARD).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "v4".into(),
        payload: IqType::Get(elem),
    };
    assert!(!is_vcard_get(&iq));
    assert!(!is_vcard_set(&iq));
}

#[test]
fn xep0054_classifier_rejects_iq_result_and_error_types() {
    // Only `type=get` / `type=set` are request shapes per §3; result
    // and error stanzas must not be misclassified as requests.
    let result_iq = Iq {
        from: None,
        to: None,
        id: "v5".into(),
        payload: IqType::Result(Some(vcard_payload())),
    };
    assert!(!is_vcard_get(&result_iq));
    assert!(!is_vcard_set(&result_iq));
}

// ── §3.2 vcard set acknowledgement ──────────────────────────────────

#[test]
fn xep0054_build_vcard_success_is_empty_result_iq() {
    // XEP-0054 §3.2: server acknowledges a successful update with an
    // empty IQ result that echoes the request id, swaps from/to.
    // Anything else (e.g. embedding the stored vCard) would violate
    // the spec's "MUST" minimal-acknowledgement requirement.
    let request = Iq {
        from: Some(
            "alice@example.com/web"
                .parse()
                .expect("valid full jid for from"),
        ),
        to: Some(
            "server.example.com"
                .parse()
                .expect("valid bare jid for to"),
        ),
        id: "set-vcard-1".into(),
        payload: IqType::Set(vcard_payload()),
    };

    let response = build_vcard_success(&request);

    assert_eq!(
        response.id, "set-vcard-1",
        "result MUST echo the request id (RFC 6120 §8.2.3)"
    );
    assert_eq!(
        response.from, request.to,
        "result from = request to (origin-flip)"
    );
    assert_eq!(
        response.to, request.from,
        "result to = request from (origin-flip)"
    );
    assert!(
        matches!(response.payload, IqType::Result(None)),
        "XEP-0054 §3.2 acknowledgement carries no payload"
    );
}
