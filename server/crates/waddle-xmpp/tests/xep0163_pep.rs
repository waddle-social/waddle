//! XEP-0163: Personal Eventing Protocol — dedicated conformance suite.
//!
//! XEP-0163 is a PEP profile of XEP-0060 PubSub: every user has an
//! implicit PubSub service hosted at their bare JID. The audit
//! invariants live across the disco + identity + classifier
//! surfaces:
//!
//! - §6 disco advertisement: `http://jabber.org/protocol/pubsub#pep`
//!   on the user's bare-JID `disco#info`,
//! - §3 identity: `<identity category='pubsub' type='pep'/>`,
//! - §"PEP Request" classifier: a PubSub IQ addressed to the
//!   user's bare JID (or no `to` at all) is a PEP request; one
//!   addressed elsewhere is a peer-PEP request,
//! - §4 well-known PEP nodes recognised as such,
//! - §5 default access model for non-bookmark nodes is `presence`
//!   (XEP-0060 §16.4.3 access-model), with bookmarks defaulting
//!   to `whitelist` (XEP-0402 §1).

use minidom::Element;
use waddle_xmpp::pubsub::{
    build_pep_identity, is_pep_request, is_pep_request_to, pep_features, PepHandler,
    PEP_NODE_AVATAR_DATA, PEP_NODE_AVATAR_METADATA, PEP_NODE_BOOKMARKS,
};
use waddle_xmpp_core::disco::Feature;
use waddle_xmpp_core::pubsub::node::AccessModel;
use xmpp_parsers::iq::{Iq, IqType};

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

// ── §6 PEP disco advertisement ──────────────────────────────────────

#[test]
fn xep0163_pep_features_advertise_pubsub_pep_per_section_6() {
    // XEP-0163 §6: "When a server supports PEP, it MUST include
    // the `http://jabber.org/protocol/pubsub#pep` feature in its
    // responses to service discovery information (disco#info)
    // requests sent to the user's bare JID." Without this advert
    // clients fall back to per-node `+notify` probing or skip
    // PEP-driven features entirely.
    let feats = pep_features();
    let target = Feature::new("http://jabber.org/protocol/pubsub#pep");
    assert!(
        feats.iter().any(|f| f == &target),
        "pep_features() MUST advertise `http://jabber.org/protocol/pubsub#pep` per XEP-0163 §6"
    );
}

#[test]
fn xep0163_pep_features_also_advertise_base_pubsub() {
    // §3: PEP IS PubSub. The base `pubsub` feature MUST appear
    // alongside the `pubsub#pep` profile flag so a PEP-naive
    // pubsub client can still discover the service.
    let feats = pep_features();
    assert!(
        feats
            .iter()
            .any(|f| f.0 == "http://jabber.org/protocol/pubsub"),
        "pep_features() MUST advertise base `pubsub` alongside the `pep` profile flag"
    );
}

// ── §3 PEP identity ─────────────────────────────────────────────────

#[test]
fn xep0163_identity_uses_spec_category_and_type() {
    // §3: `<identity category='pubsub' type='pep'/>`. The
    // category/type pair is the canonical PEP service-discovery
    // marker; clients dispatch on it alongside the feature.
    let identity = build_pep_identity();
    assert_eq!(identity.category, "pubsub");
    assert_eq!(identity.type_, "pep");
    assert!(
        identity.name.is_some(),
        "spec allows but doesn't require a friendly name; Waddle ships one for human-facing disco"
    );
}

// ── §"PEP Request" classifier ───────────────────────────────────────

fn make_pubsub_iq(to: Option<&str>) -> Iq {
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("items", NS_PUBSUB)
                .attr("node", "urn:xmpp:avatar:metadata")
                .build(),
        )
        .build();
    Iq {
        from: Some("alice@example.com/web".parse().expect("valid jid")),
        to: to.map(|s| s.parse().expect("valid jid")),
        id: "p-1".to_owned(),
        payload: IqType::Get(pubsub),
    }
}

#[test]
fn xep0163_self_pep_request_has_no_to_or_to_eq_self() {
    // §"PEP Request": a PubSub IQ to the user's own PEP service
    // is either `to=user-bare-jid` OR `to=` is absent (the
    // implicit-self routing).
    let alice: jid::BareJid = "alice@example.com".parse().expect("valid jid");

    let absent = make_pubsub_iq(None);
    assert!(
        is_pep_request(&absent, &alice),
        "absent `to=` MUST be treated as a self-PEP request"
    );

    let explicit_self = make_pubsub_iq(Some("alice@example.com"));
    assert!(
        is_pep_request(&explicit_self, &alice),
        "explicit `to=` equal to user's bare JID is a self-PEP request"
    );
}

#[test]
fn xep0163_self_pep_classifier_ignores_full_jid_resource() {
    // §"PEP Request" matches against the bare JID regardless of
    // the requester's resource — Carbons and multi-device PEP
    // both depend on this lenient matching.
    let alice: jid::BareJid = "alice@example.com".parse().expect("valid jid");
    let with_resource = make_pubsub_iq(Some("alice@example.com/balcony"));
    assert!(is_pep_request(&with_resource, &alice));
}

#[test]
fn xep0163_peer_pep_request_addresses_another_bare_jid() {
    // The cross-user PEP path: querying a contact's PEP avatar.
    // `is_pep_request_to` returns true only when the IQ is
    // addressed to the named contact — required to avoid leaking
    // OWN PEP items via a peer-path classifier.
    let bob: jid::BareJid = "bob@example.com".parse().expect("valid jid");
    let to_bob = make_pubsub_iq(Some("bob@example.com"));
    assert!(is_pep_request_to(&to_bob, &bob));

    let to_someone_else = make_pubsub_iq(Some("carol@example.com"));
    assert!(!is_pep_request_to(&to_someone_else, &bob));

    let no_to = make_pubsub_iq(None);
    assert!(
        !is_pep_request_to(&no_to, &bob),
        "absent `to=` is NEVER a peer-PEP request — that would let a sender's self-PEP \
         query accidentally surface as a query against bob"
    );
}

#[test]
fn xep0163_non_pubsub_iq_is_never_a_pep_request() {
    // The classifier gates on `is_pubsub_iq` first — a roster IQ
    // (even one addressed to the user's bare JID) MUST NOT
    // classify as PEP.
    let roster = Iq {
        from: Some("alice@example.com/web".parse().expect("valid jid")),
        to: Some("alice@example.com".parse().expect("valid jid")),
        id: "r-1".into(),
        payload: IqType::Get(Element::builder("query", "jabber:iq:roster").build()),
    };
    let alice: jid::BareJid = "alice@example.com".parse().expect("valid jid");
    assert!(!is_pep_request(&roster, &alice));
    assert!(!is_pep_request_to(&roster, &alice));
}

// ── §4 well-known PEP nodes ─────────────────────────────────────────

#[test]
fn xep0163_well_known_node_classifier_covers_registered_nodes() {
    // §4 nominates these as the "well-known" PEP nodes that
    // clients SHOULD treat as semantically meaningful. Pinning
    // the set keeps Waddle's dispatch table aligned with the
    // protocol-registry registrations.
    let known = [
        PEP_NODE_BOOKMARKS,
        PEP_NODE_AVATAR_DATA,
        PEP_NODE_AVATAR_METADATA,
        "http://jabber.org/protocol/nick",
        "http://jabber.org/protocol/mood",
        "http://jabber.org/protocol/activity",
        "http://jabber.org/protocol/tune",
        "http://jabber.org/protocol/geoloc",
        "urn:xmpp:microblog:0",
        // XEP-0490 Message Displayed Synchronization PEP node.
        "urn:xmpp:mds:displayed:0",
    ];
    for node in known {
        assert!(
            PepHandler::is_well_known_node(node),
            "{node} should be classified as well-known"
        );
    }
}

#[test]
fn xep0163_unknown_pep_nodes_are_not_misclassified_as_well_known() {
    assert!(!PepHandler::is_well_known_node(""));
    assert!(!PepHandler::is_well_known_node("urn:xmpp:bookmarks:0"));
    assert!(!PepHandler::is_well_known_node("urn:waddle:custom:0"));
    assert!(!PepHandler::is_well_known_node(
        "eu.siacs.conversations.axolotl.devicelist"
    ));
}

#[test]
fn xep0163_pep_node_constants_match_official_namespaces() {
    // Bookmarks (XEP-0402), Avatar Data / Metadata (XEP-0084) all
    // use the `urn:xmpp:*` namespace registered with the XSF.
    // Pinning the literals so a future "modernise" temptation
    // doesn't break peer interop.
    assert_eq!(PEP_NODE_BOOKMARKS, "urn:xmpp:bookmarks:1");
    assert_eq!(PEP_NODE_AVATAR_DATA, "urn:xmpp:avatar:data");
    assert_eq!(PEP_NODE_AVATAR_METADATA, "urn:xmpp:avatar:metadata");
}

// ── §5 default access models ────────────────────────────────────────

#[test]
fn xep0163_default_access_model_for_bookmarks_is_whitelist() {
    // XEP-0402 §1: bookmarks default to whitelist access model
    // — they're private to the owner, not fanned out to roster
    // contacts.
    assert_eq!(
        PepHandler::default_access_model_for_node(PEP_NODE_BOOKMARKS),
        AccessModel::Whitelist
    );
}

#[test]
fn xep0163_default_access_model_for_other_nodes_is_presence() {
    // XEP-0163 §5: "PEP-defined nodes by default use the
    // 'presence' access model" — items get pushed to every
    // roster contact whose presence is currently online.
    for node in [
        PEP_NODE_AVATAR_DATA,
        PEP_NODE_AVATAR_METADATA,
        "http://jabber.org/protocol/nick",
        "urn:waddle:custom:0",
        "",
    ] {
        assert_eq!(
            PepHandler::default_access_model_for_node(node),
            AccessModel::Presence,
            "{node} should default to `presence`"
        );
    }
}
