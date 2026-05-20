//! XEP-0030: Service Discovery — dedicated conformance suite.
//!
//! XEP-0030 is the discovery substrate every other XEP rides on.
//! The `disco#info` query/response surfaces identities, features,
//! and XEP-0128 extension forms, and `disco#items` enumerates the
//! child JIDs and node hierarchy.
//!
//! The audit pins:
//!
//! - §3 disco#info namespace and §4 disco#items namespace,
//! - §3 disco#info query shape (IQ get with namespaced `<query/>`,
//!   optional `node=` attribute),
//! - §3 response shape (`<identity>` with `category`/`type`/optional
//!   `name`/optional `xml:lang`, and `<feature var=>`),
//! - §4 disco#items response shape (`<item jid=… name=… node=…/>`),
//! - server-wide disco#info advertises the disco itself (so a
//!   client can confirm the server supports XEP-0030 by querying
//!   for the protocol it just used),
//! - XEP-0115 §5.4 ill-formed detection: duplicate identity
//!   tuples, duplicate features, and multi-FORM_TYPE extension
//!   forms all flag `ill_formed = true` so XEP-0115 caps caching
//!   is correctly invalidated.

use minidom::Element;
use waddle_xmpp::disco::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    build_disco_items_response, is_disco_info_query, is_disco_items_query, muc_room_features,
    muc_service_features, parse_disco_info_query, parse_disco_items_query, server_features,
    upload_service_features, DiscoItem, Feature, Identity, DISCO_INFO_NS, DISCO_ITEMS_NS,
};
use waddle_xmpp_core::disco::info::parse_disco_info_response;
use xmpp_parsers::iq::Iq;

const NS_DATA_FORMS: &str = "jabber:x:data";

// ── §3 / §4 namespace pins ──────────────────────────────────────────

#[test]
fn xep0030_namespace_constants_match_spec() {
    // §3 + §4 pin these exact URIs. Every other XEP that
    // advertises its own feature is keyed on the disco#info
    // namespace — a typo here breaks the entire discovery surface.
    assert_eq!(DISCO_INFO_NS, "http://jabber.org/protocol/disco#info");
    assert_eq!(DISCO_ITEMS_NS, "http://jabber.org/protocol/disco#items");
}

// ── §3 disco#info query shape ───────────────────────────────────────

#[test]
fn xep0030_info_classifier_accepts_iq_get_with_namespaced_query() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "i-1".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };
    assert!(is_disco_info_query(&iq));
}

#[test]
fn xep0030_info_classifier_rejects_iq_set_or_wrong_namespace() {
    // §3 fixes the verb as `get`. A `set` carrying the same
    // payload is malformed.
    let set_iq = Iq::Set {
        from: None,
        to: None,
        id: "i-2".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };
    assert!(!is_disco_info_query(&set_iq));

    let wrong_ns = Iq::Get {
        from: None,
        to: None,
        id: "i-3".into(),
        payload: Element::builder("query", "wrong:ns").build(),
    };
    assert!(!is_disco_info_query(&wrong_ns));
}

#[test]
fn xep0030_info_parser_captures_optional_node_attribute() {
    // §3.2: clients can scope the query to a specific node. The
    // parser surfaces `node=` so the handler can dispatch
    // node-scoped disco lookups (e.g. XEP-0115 caps, XEP-0050
    // command nodes, XEP-0503 space metadata).
    let iq_with_node = Iq::Get {
        from: None,
        to: None,
        id: "i-4".into(),
        payload: Element::builder("query", DISCO_INFO_NS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "https://waddle.social/caps#hash",
            )
            .build(),
    };
    let parsed = parse_disco_info_query(&iq_with_node).expect("parses");
    assert_eq!(
        parsed.node.as_deref(),
        Some("https://waddle.social/caps#hash")
    );

    let iq_no_node = Iq::Get {
        from: None,
        to: None,
        id: "i-5".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };
    let parsed = parse_disco_info_query(&iq_no_node).expect("parses");
    assert!(parsed.node.is_none());
}

// ── §3 disco#info response shape ────────────────────────────────────

#[test]
fn xep0030_build_info_response_emits_spec_shape() {
    // §3.1 example shape:
    //   <iq type='result' …>
    //     <query xmlns='http://jabber.org/protocol/disco#info'>
    //       <identity category='server' type='im' name='Waddle'/>
    //       <feature var='http://jabber.org/protocol/disco#info'/>
    //       …
    //     </query>
    //   </iq>
    let original = Iq::Get {
        from: Some("user@example.com/web".parse().expect("jid")),
        to: Some("waddle.example.com".parse().expect("jid")),
        id: "disco-1".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };

    let identities = [Identity::server(Some("Waddle Server"))];
    let features = [
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::caps(),
    ];

    let response = build_disco_info_response(&original, &identities, &features, None);

    // Origin-flip: response from = request to, response to = request from.
    assert_eq!(
        response.from().map(ToString::to_string).as_deref(),
        Some("waddle.example.com")
    );
    assert_eq!(
        response.to().map(ToString::to_string).as_deref(),
        Some("user@example.com/web")
    );
    assert_eq!(response.id(), "disco-1");

    let Iq::Result {
        payload: Some(query),
        ..
    } = response
    else {
        panic!("response must be iq type='result' with query payload");
    };
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), DISCO_INFO_NS);

    let identity = query
        .children()
        .find(|c| c.name() == "identity" && c.ns() == DISCO_INFO_NS)
        .expect("<identity> child present");
    assert_eq!(identity.attr("category"), Some("server"));
    assert_eq!(identity.attr("type"), Some("im"));
    assert_eq!(identity.attr("name"), Some("Waddle Server"));

    let feature_vars: Vec<_> = query
        .children()
        .filter(|c| c.name() == "feature" && c.ns() == DISCO_INFO_NS)
        .filter_map(|c| c.attr("var").map(str::to_owned))
        .collect();
    assert!(feature_vars.contains(&DISCO_INFO_NS.to_owned()));
    assert!(feature_vars.contains(&"http://jabber.org/protocol/disco#items".to_owned()));
}

#[test]
fn xep0030_identity_constructors_match_spec_disco_categories() {
    // §3 §"Categories" pins the (category, type) pairs for each
    // service kind. Waddle's constructors must produce the
    // standard tuples so cross-server disco lookups
    // canonicalise correctly.
    assert_eq!(Identity::server(None).category, "server");
    assert_eq!(Identity::server(None).type_, "im");

    assert_eq!(Identity::muc_service(None).category, "conference");
    assert_eq!(Identity::muc_service(None).type_, "text");

    assert_eq!(Identity::muc_room(None).category, "conference");
    assert_eq!(Identity::muc_room(None).type_, "text");

    assert_eq!(Identity::upload_service(None).category, "store");
    assert_eq!(Identity::upload_service(None).type_, "file");

    assert_eq!(Identity::pubsub_service(None).category, "pubsub");
    assert_eq!(Identity::pubsub_service(None).type_, "service");

    assert_eq!(Identity::pubsub_leaf(None).category, "pubsub");
    assert_eq!(Identity::pubsub_leaf(None).type_, "leaf");
}

#[test]
fn xep0030_identity_optional_xml_lang_round_trips() {
    // §3.4: identity names can be tagged with `xml:lang` so
    // servers can advertise localised names. The builder + parser
    // must round-trip the attribute or multilingual deployments
    // lose their localisations.
    let identity = Identity::new("server", "im", Some("Le serveur")).with_lang(Some("fr"));
    let original = Iq::Get {
        from: None,
        to: None,
        id: "i-6".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };
    let response = build_disco_info_response(&original, &[identity], &[], None);
    let Iq::Result {
        payload: Some(query),
        ..
    } = response
    else {
        panic!();
    };
    let id_elem = query
        .children()
        .find(|c| c.name() == "identity")
        .expect("present");
    // minidom 0.18 keys attrs by (Namespace, NcName); xml:lang lives in
    // the XML namespace, not the default one. Read it via attr_ns.
    assert_eq!(
        id_elem.attr_ns(&minidom::rxml::Namespace::XML, "lang"),
        Some("fr")
    );
    assert_eq!(id_elem.attr("name"), Some("Le serveur"));
}

// ── §4 disco#items query shape ──────────────────────────────────────

#[test]
fn xep0030_items_classifier_accepts_iq_get_with_namespaced_query() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "n-1".into(),
        payload: Element::builder("query", DISCO_ITEMS_NS).build(),
    };
    assert!(is_disco_items_query(&iq));
}

#[test]
fn xep0030_items_classifier_rejects_wrong_ns() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "n-2".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };
    assert!(!is_disco_items_query(&iq));
}

#[test]
fn xep0030_items_parser_captures_optional_node_attribute() {
    // §4: items can be scoped to a node, e.g. a XEP-0503 Space's
    // member bookmark listing or a XEP-0050 ad-hoc command's
    // sub-commands.
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "n-3".into(),
        payload: Element::builder("query", DISCO_ITEMS_NS)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
            .build(),
    };
    let parsed = parse_disco_items_query(&iq).expect("parses");
    assert_eq!(parsed.node.as_deref(), Some("general"));
}

// ── §4 disco#items response shape ───────────────────────────────────

#[test]
fn xep0030_build_items_response_emits_spec_shape() {
    // §4.1 example: `<query xmlns='http://jabber.org/protocol/disco#items'>
    //                  <item jid='…' name='…' node='…'/>…</query>`.
    let original = Iq::Get {
        from: Some("user@example.com/web".parse().expect("jid")),
        to: Some("waddle.example.com".parse().expect("jid")),
        id: "disco-items-1".into(),
        payload: Element::builder("query", DISCO_ITEMS_NS).build(),
    };
    let items = vec![
        DiscoItem::muc_service("muc.example.com", Some("Chat")),
        DiscoItem::upload_service("upload.example.com", Some("Files")),
        DiscoItem::spaces_node("spaces.example.com", "general", Some("General Space")),
    ];
    let response = build_disco_items_response(&original, &items, None);

    let Iq::Result {
        payload: Some(query),
        ..
    } = response
    else {
        panic!();
    };
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), DISCO_ITEMS_NS);

    let item_elems: Vec<_> = query
        .children()
        .filter(|c| c.name() == "item" && c.ns() == DISCO_ITEMS_NS)
        .collect();
    assert_eq!(item_elems.len(), 3);

    // First item: MUC service (no node).
    assert_eq!(item_elems[0].attr("jid"), Some("muc.example.com"));
    assert_eq!(item_elems[0].attr("name"), Some("Chat"));
    assert!(item_elems[0].attr("node").is_none());

    // Third item: spaces node (node= attribute carries the space id).
    assert_eq!(item_elems[2].attr("jid"), Some("spaces.example.com"));
    assert_eq!(item_elems[2].attr("node"), Some("general"));
    assert_eq!(item_elems[2].attr("name"), Some("General Space"));
}

// ── Disco-itself advertisement ─────────────────────────────────────

#[test]
fn xep0030_server_advertises_disco_info_and_disco_items_features() {
    // A discovery-supporting server MUST advertise the disco
    // protocols themselves in its own disco#info — otherwise a
    // client can't confirm via the spec what protocol it just
    // succeeded in using. This is the convention in every major
    // XMPP server.
    let feats = server_features();
    assert!(feats.iter().any(|f| f.0 == DISCO_INFO_NS));
    assert!(feats.iter().any(|f| f.0 == DISCO_ITEMS_NS));
}

#[test]
fn xep0030_muc_service_advertises_both_disco_features() {
    let feats = muc_service_features();
    assert!(feats.iter().any(|f| f.0 == DISCO_INFO_NS));
    assert!(feats.iter().any(|f| f.0 == DISCO_ITEMS_NS));
}

#[test]
fn xep0030_muc_rooms_advertise_disco_info() {
    // Each room is itself a disco#info-supporting entity (clients
    // probe the room JID to enumerate its advertised features).
    // §"Discovering Support" requires the room to advertise the
    // discovery protocol it answers to.
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f.0 == DISCO_INFO_NS),
                        "room ({persistent}, {members_only}, {moderated}, {forum}) \
                         must advertise disco#info"
                    );
                }
            }
        }
    }
}

#[test]
fn xep0030_upload_service_advertises_disco_info() {
    let feats = upload_service_features();
    assert!(feats.iter().any(|f| f.0 == DISCO_INFO_NS));
}

// ── XEP-0115 §5.4 ill-formed disco#info detection ──────────────────

fn build_response_query(extensions: &[Element]) -> Element {
    let mut q = Element::builder("query", DISCO_INFO_NS);
    q = q
        .append(
            Element::builder("identity", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("category").to_owned(), "server")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "im")
                .build(),
        )
        .append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), DISCO_INFO_NS)
                .build(),
        );
    for ext in extensions {
        q = q.append(ext.clone());
    }
    q.build()
}

#[test]
fn xep0030_parse_response_well_formed_returns_ill_formed_false() {
    // Baseline: a well-formed disco#info response with one
    // identity + one feature parses as `ill_formed = false`.
    // XEP-0115 §5.4 hash verification is only meaningful against
    // a well-formed response.
    let q = build_response_query(&[]);
    let response = parse_disco_info_response(&q).expect("parses");
    assert!(!response.ill_formed);
    assert_eq!(response.identities.len(), 1);
    assert_eq!(response.features.len(), 1);
}

#[test]
fn xep0030_parse_response_flags_duplicate_identity_tuples() {
    // XEP-0115 §5.4 step 2.4: identities with identical
    // (category, type, xml:lang, name) tuples make the response
    // ill-formed. A caps-cache that hashed such a response would
    // disagree with a peer that dedup'd them, causing cache
    // misses.
    let mut q = Element::builder("query", DISCO_INFO_NS)
        .append(
            Element::builder("identity", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("category").to_owned(), "server")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "im")
                .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Waddle")
                .build(),
        )
        .append(
            Element::builder("identity", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("category").to_owned(), "server")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "im")
                .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Waddle")
                .build(),
        );
    q = q.append(
        Element::builder("feature", DISCO_INFO_NS)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), DISCO_INFO_NS)
            .build(),
    );
    let response = parse_disco_info_response(&q.build()).expect("parses");
    assert!(
        response.ill_formed,
        "duplicate identity tuples MUST set ill_formed=true"
    );
}

#[test]
fn xep0030_parse_response_flags_duplicate_features() {
    // XEP-0115 §5.4 step 2.4: duplicate feature vars are
    // ill-formed for the same caps-cache reason.
    let q = Element::builder("query", DISCO_INFO_NS)
        .append(
            Element::builder("identity", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("category").to_owned(), "server")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "im")
                .build(),
        )
        .append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), DISCO_INFO_NS)
                .build(),
        )
        .append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), DISCO_INFO_NS)
                .build(),
        )
        .build();
    let response = parse_disco_info_response(&q).expect("parses");
    assert!(
        response.ill_formed,
        "duplicate feature vars MUST set ill_formed=true"
    );
}

#[test]
fn xep0030_parse_response_preserves_xep0128_extension_forms_verbatim() {
    // §3.5 (XEP-0128) extension forms: the response MAY include
    // one or more `<x xmlns='jabber:x:data' type='result'>` forms.
    // The parser MUST preserve them verbatim so XEP-0115 hash
    // recomputation can run against the exact input that produced
    // the advertised `ver`.
    let form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append("urn:xmpp:dataforms:softwareinfo")
                        .build(),
                )
                .build(),
        )
        .build();
    let q = build_response_query(&[form]);
    let response = parse_disco_info_response(&q).expect("parses");
    assert_eq!(response.extensions.len(), 1);
    assert!(!response.ill_formed);
}

#[test]
fn xep0030_build_info_response_with_extensions_preserves_data_forms() {
    // The builder MUST emit XEP-0128 extension forms inside the
    // `<query>` element so peers receive the full hash input.
    let form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .build();
    let original = Iq::Get {
        from: None,
        to: None,
        id: "i-7".into(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    };
    let response = build_disco_info_response_with_extensions(
        &original,
        &[Identity::server(Some("Waddle"))],
        &[Feature::disco_info()],
        None,
        std::slice::from_ref(&form),
    );
    let Iq::Result {
        payload: Some(query),
        ..
    } = response
    else {
        panic!();
    };
    let extensions: Vec<_> = query
        .children()
        .filter(|c| c.name() == "x" && c.ns() == NS_DATA_FORMS)
        .collect();
    assert_eq!(extensions.len(), 1);
}

// ── Node-scoped query/response ─────────────────────────────────────

#[test]
fn xep0030_build_info_response_with_node_echoes_node_on_response() {
    // §6.3: when a client queries a specific `node=`, the server's
    // response MUST echo the node on `<query node='…'>` so the
    // client can match the response to the right query. Otherwise
    // a multiplexed disco probe (caps + commands + spaces) would
    // de-mux incorrectly.
    let original = Iq::Get {
        from: None,
        to: None,
        id: "i-8".into(),
        payload: Element::builder("query", DISCO_INFO_NS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "https://waddle/caps#abc",
            )
            .build(),
    };
    let response = build_disco_info_response(
        &original,
        &[Identity::server(None)],
        &[],
        Some("https://waddle/caps#abc"),
    );
    let Iq::Result {
        payload: Some(query),
        ..
    } = response
    else {
        panic!();
    };
    assert_eq!(query.attr("node"), Some("https://waddle/caps#abc"));
}
