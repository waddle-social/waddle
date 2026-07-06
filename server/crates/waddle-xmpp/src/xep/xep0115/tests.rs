use super::*;

use crate::disco::info::{Feature, Identity, DISCO_INFO_NS};

fn data_form_field(var: &str, field_type: Option<&str>, values: &[&str]) -> Element {
    let mut builder = Element::builder("field", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var);

    if let Some(field_type) = field_type {
        builder = builder.attr(minidom::rxml::xml_ncname!("type").to_owned(), field_type);
    }

    for value in values {
        builder = builder.append(
            Element::builder("value", DATA_FORMS_NS)
                .append(*value)
                .build(),
        );
    }

    builder.build()
}

fn software_info_form() -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(data_form_field(
            "ip_version",
            Some("text-multi"),
            &["ipv6", "ipv4"],
        ))
        .append(data_form_field("os", None, &["Mac"]))
        .append(data_form_field("os_version", None, &["10.5.1"]))
        .append(data_form_field("software", None, &["Psi"]))
        .append(data_form_field("software_version", None, &["0.11"]))
        .build()
}

fn software_info_form_with_type(form_type: &str) -> Element {
    let mut form = software_info_form();
    form.set_attr(
        minidom::rxml::Namespace::NONE,
        minidom::rxml::xml_ncname!("type").to_owned(),
        form_type,
    );
    form
}

fn software_info_form_with_form_type_field_type(field_type: &str) -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some(field_type),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(data_form_field("software", None, &["Psi"]))
        .build()
}

fn software_info_form_with_foreign_children() -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(
            Element::builder("field", "urn:waddle:test:foreign")
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "rogue-field")
                .append(
                    Element::builder("value", "urn:waddle:test:foreign")
                        .append("rogue")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "software")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append("Psi")
                        .build(),
                )
                .append(
                    Element::builder("value", "urn:waddle:test:foreign")
                        .append("rogue")
                        .build(),
                )
                .build(),
        )
        .build()
}

fn software_info_form_with_empty_value_field() -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "software")
                .build(),
        )
        .build()
}

fn software_info_form_with_duplicate_form_type() -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo:duplicate"],
        ))
        .append(data_form_field("software", None, &["Psi"]))
        .build()
}

fn software_info_form_with_missing_var() -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append("rogue")
                        .build(),
                )
                .build(),
        )
        .append(data_form_field("software", None, &["Psi"]))
        .build()
}

#[test]
fn test_caps_new() {
    let caps = Caps::new("https://example.com/caps", "abcd1234");
    assert_eq!(caps.hash, "sha-1");
    assert_eq!(caps.node, "https://example.com/caps");
    assert_eq!(caps.ver, "abcd1234");
}

#[test]
fn test_caps_waddle() {
    let caps = Caps::waddle("test-hash");
    assert_eq!(caps.node, WADDLE_CAPS_NODE);
    assert_eq!(caps.ver, "test-hash");
}

#[test]
fn test_caps_node_ver() {
    let caps = Caps::new("https://example.com/caps", "abcd1234");
    assert_eq!(caps.node_ver(), "https://example.com/caps#abcd1234");
}

#[test]
fn test_caps_build_element() {
    let caps = Caps::new("https://example.com/caps", "test-ver");
    let elem = caps.build_element();

    assert_eq!(elem.name(), "c");
    assert_eq!(elem.ns(), NS_CAPS);
    assert_eq!(elem.attr("hash"), Some("sha-1"));
    assert_eq!(elem.attr("node"), Some("https://example.com/caps"));
    assert_eq!(elem.attr("ver"), Some("test-ver"));
}

#[test]
fn test_caps_from_element() {
    let elem = Element::builder("c", NS_CAPS)
        .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "https://test.com",
        )
        .attr(minidom::rxml::xml_ncname!("ver").to_owned(), "xyz789")
        .build();

    let caps = Caps::from_element(&elem).unwrap();
    assert_eq!(caps.hash, "sha-1");
    assert_eq!(caps.node, "https://test.com");
    assert_eq!(caps.ver, "xyz789");
}

#[test]
fn test_caps_from_element_wrong_name() {
    let elem = Element::builder("x", NS_CAPS)
        .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "https://test.com",
        )
        .attr(minidom::rxml::xml_ncname!("ver").to_owned(), "xyz789")
        .build();

    assert!(Caps::from_element(&elem).is_none());
}

#[test]
fn test_caps_from_element_wrong_ns() {
    let elem = Element::builder("c", "wrong:ns")
        .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "https://test.com",
        )
        .attr(minidom::rxml::xml_ncname!("ver").to_owned(), "xyz789")
        .build();

    assert!(Caps::from_element(&elem).is_none());
}

#[test]
fn test_caps_from_element_missing_attrs() {
    let elem = Element::builder("c", NS_CAPS)
        .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
        // missing node and ver
        .build();

    assert!(Caps::from_element(&elem).is_none());
}

fn key(hash: &str, ver: &str) -> super::CapsCacheKey {
    super::CapsCacheKey::new(hash, ver)
}

#[test]
fn test_caps_cache_insert_and_get() {
    let cache = CapsCache::new();
    let info = CachedDiscoInfo::new(
        vec![Identity::server(Some("Test"))],
        vec![Feature::disco_info()],
    );

    cache.insert(key("sha-1", "test-hash"), info.clone());

    let retrieved = cache.get(&key("sha-1", "test-hash")).unwrap();
    assert_eq!(retrieved.identities.len(), 1);
    assert_eq!(retrieved.features.len(), 1);
}

#[test]
fn test_caps_cache_keys_are_per_hash_algo() {
    // XEP-0115 §6: caching MUST be per `(hash algorithm, ver)`. Two
    // entries with identical `ver` but different `hash` MUST coexist.
    let cache = CapsCache::new();
    let sha1_info = CachedDiscoInfo::new(vec![Identity::server(Some("sha-1"))], vec![]);
    let sha256_info = CachedDiscoInfo::new(vec![Identity::server(Some("sha-256"))], vec![]);

    cache.insert(key("sha-1", "samever"), sha1_info);
    cache.insert(key("sha-256", "samever"), sha256_info);

    assert_eq!(
        cache.get(&key("sha-1", "samever")).unwrap().identities[0]
            .name
            .as_deref(),
        Some("sha-1")
    );
    assert_eq!(
        cache.get(&key("sha-256", "samever")).unwrap().identities[0]
            .name
            .as_deref(),
        Some("sha-256")
    );
}

#[test]
fn test_caps_cache_contains() {
    let cache = CapsCache::new();
    let info = CachedDiscoInfo::new(vec![], vec![]);

    assert!(!cache.contains(&key("sha-1", "hash1")));
    cache.insert(key("sha-1", "hash1"), info);
    assert!(cache.contains(&key("sha-1", "hash1")));
}

#[test]
fn test_caps_cache_remove() {
    let cache = CapsCache::new();
    let info = CachedDiscoInfo::new(vec![], vec![]);

    cache.insert(key("sha-1", "hash1"), info);
    assert!(cache.contains(&key("sha-1", "hash1")));

    let removed = cache.remove(&key("sha-1", "hash1"));
    assert!(removed.is_some());
    assert!(!cache.contains(&key("sha-1", "hash1")));
}

#[test]
fn test_caps_cache_len_and_clear() {
    let cache = CapsCache::new();

    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());

    cache.insert(key("sha-1", "h1"), CachedDiscoInfo::new(vec![], vec![]));
    cache.insert(key("sha-1", "h2"), CachedDiscoInfo::new(vec![], vec![]));

    assert_eq!(cache.len(), 2);
    assert!(!cache.is_empty());

    cache.clear();
    assert!(cache.is_empty());
}

#[test]
fn test_caps_cache_lru_eviction_at_capacity() {
    // Bounded LRU prevents unbounded memory growth (PR 438 review #15).
    let cache = CapsCache::with_capacity(2);
    cache.insert(key("sha-1", "a"), CachedDiscoInfo::new(vec![], vec![]));
    cache.insert(key("sha-1", "b"), CachedDiscoInfo::new(vec![], vec![]));
    // Touch "a" so "b" becomes LRU.
    let _ = cache.get(&key("sha-1", "a"));
    cache.insert(key("sha-1", "c"), CachedDiscoInfo::new(vec![], vec![]));
    assert!(cache.contains(&key("sha-1", "a")));
    assert!(
        !cache.contains(&key("sha-1", "b")),
        "LRU eviction MUST drop b"
    );
    assert!(cache.contains(&key("sha-1", "c")));
}

#[test]
fn test_build_verification_string_empty() {
    let s = build_verification_string_with_extensions(&[], &[], &[]);
    assert_eq!(s, "");
}

#[test]
fn test_build_verification_string_identity_only() {
    let identities = vec![Identity::server(Some("Test Server"))];
    let s = build_verification_string_with_extensions(&identities, &[], &[]);
    // Format: category/type/lang/name<
    assert_eq!(s, "server/im//Test Server<");
}

#[test]
fn test_build_verification_string_identity_no_name() {
    let identities = vec![Identity::server(None)];
    let s = build_verification_string_with_extensions(&identities, &[], &[]);
    assert_eq!(s, "server/im//<");
}

#[test]
fn test_build_verification_string_identity_with_lang() {
    let identities = vec![Identity::new("client", "pc", Some("Psi")).with_lang(Some("en"))];
    let s = build_verification_string_with_extensions(&identities, &[], &[]);
    assert_eq!(s, "client/pc/en/Psi<");
}

#[test]
fn test_build_verification_string_features_only() {
    let features = vec![
        Feature::new("http://jabber.org/protocol/disco#info"),
        Feature::new("http://jabber.org/protocol/disco#items"),
    ];
    let s = build_verification_string_with_extensions(&[], &features, &[]);
    // Features should be sorted alphabetically
    assert_eq!(
        s,
        "http://jabber.org/protocol/disco#info<http://jabber.org/protocol/disco#items<"
    );
}

#[test]
fn test_build_verification_string_features_sorted() {
    let features = vec![
        Feature::new("z-feature"),
        Feature::new("a-feature"),
        Feature::new("m-feature"),
    ];
    let s = build_verification_string_with_extensions(&[], &features, &[]);
    assert_eq!(s, "a-feature<m-feature<z-feature<");
}

#[test]
fn test_build_verification_string_full() {
    let identities = vec![
        Identity::server(Some("Test")),
        Identity::new("client", "pc", Some("MyClient")),
    ];
    let features = vec![Feature::new("feature2"), Feature::new("feature1")];
    let s = build_verification_string_with_extensions(&identities, &features, &[]);
    // Identities sorted by category/type/name, then features sorted
    assert_eq!(s, "client/pc//MyClient<server/im//Test<feature1<feature2<");
}

#[test]
fn test_compute_caps_hash_known_value() {
    // Test with a known verification string to ensure SHA-1/base64 is correct
    // Empty verification string hash:
    // SHA1("") = da39a3ee5e6b4b0d3255bfef95601890afd80709
    // Base64 = 2jmj7l5rSw0yVb/vlWAYkK/YBwk=
    let hash = hash_verification_string("");
    assert_eq!(hash, "2jmj7l5rSw0yVb/vlWAYkK/YBwk=");
}

#[test]
fn test_compute_caps_hash_simple_example() {
    let identities = vec![Identity::new("client", "pc", Some("Exodus 0.9.1"))];
    let features = vec![
        Feature::new(NS_CAPS),
        Feature::new(DISCO_INFO_NS),
        Feature::new("http://jabber.org/protocol/disco#items"),
        Feature::new("http://jabber.org/protocol/muc"),
    ];

    let hash = compute_caps_hash(&identities, &features);
    assert_eq!(hash, "QgayPKawpkPSDYmwT/WM94uAlu0=");
}

#[test]
fn test_build_verification_string_with_extensions_complex_example() {
    let identities = vec![
        Identity::new("client", "pc", Some("Psi 0.11")).with_lang(Some("en")),
        Identity::new("client", "pc", Some("\u{03A8} 0.11")).with_lang(Some("el")),
    ];
    let features = vec![
        Feature::new("http://jabber.org/protocol/muc"),
        Feature::new(DISCO_INFO_NS),
        Feature::new(NS_CAPS),
        Feature::new("http://jabber.org/protocol/disco#items"),
    ];
    let extensions = vec![software_info_form()];

    let verification_string =
        build_verification_string_with_extensions(&identities, &features, &extensions);

    assert_eq!(
            verification_string,
            "client/pc/el/\u{03A8} 0.11<client/pc/en/Psi 0.11<http://jabber.org/protocol/caps<http://jabber.org/protocol/disco#info<http://jabber.org/protocol/disco#items<http://jabber.org/protocol/muc<urn:xmpp:dataforms:softwareinfo<ip_version<ipv4<ipv6<os<Mac<os_version<10.5.1<software<Psi<software_version<0.11<"
        );
}

#[test]
fn test_compute_caps_hash_with_extensions_complex_example() {
    let identities = vec![
        Identity::new("client", "pc", Some("Psi 0.11")).with_lang(Some("en")),
        Identity::new("client", "pc", Some("\u{03A8} 0.11")).with_lang(Some("el")),
    ];
    let features = vec![
        Feature::new("http://jabber.org/protocol/muc"),
        Feature::new(DISCO_INFO_NS),
        Feature::new(NS_CAPS),
        Feature::new("http://jabber.org/protocol/disco#items"),
    ];
    let extensions = vec![software_info_form()];

    let hash = compute_caps_hash_with_extensions(&identities, &features, &extensions);
    assert_eq!(hash, "q07IKJEyjvHSyhy//CH0CxmKi8w=");
}

#[test]
fn test_compute_caps_hash_ignores_non_result_forms() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info(), Feature::disco_items()];
    let baseline = compute_caps_hash(&identities, &features);
    let extensions = vec![software_info_form_with_type("submit")];

    let hash = compute_caps_hash_with_extensions(&identities, &features, &extensions);
    assert_eq!(hash, baseline);
}

#[test]
fn test_compute_caps_hash_ignores_forms_without_hidden_form_type() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info(), Feature::disco_items()];
    let baseline = compute_caps_hash(&identities, &features);
    let extensions = vec![software_info_form_with_form_type_field_type("text-single")];

    let hash = compute_caps_hash_with_extensions(&identities, &features, &extensions);
    assert_eq!(hash, baseline);
}

#[test]
fn test_compute_caps_hash_ignores_foreign_namespaced_form_children() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info(), Feature::disco_items()];
    let clean_extensions = vec![Element::builder("x", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(data_form_field(
            FORM_TYPE_FIELD,
            Some("hidden"),
            &["urn:xmpp:dataforms:softwareinfo"],
        ))
        .append(data_form_field("software", None, &["Psi"]))
        .build()];
    let baseline = compute_caps_hash_with_extensions(&identities, &features, &clean_extensions);
    let extensions = vec![software_info_form_with_foreign_children()];

    let hash = compute_caps_hash_with_extensions(&identities, &features, &extensions);
    assert_eq!(hash, baseline);
}

#[test]
fn test_build_verification_string_treats_missing_field_values_as_empty() {
    let verification_string = build_verification_string_with_extensions(
        &[Identity::server(Some("Waddle"))],
        &[Feature::disco_info()],
        &[software_info_form_with_empty_value_field()],
    );

    assert_eq!(
            verification_string,
            "server/im//Waddle<http://jabber.org/protocol/disco#info<urn:xmpp:dataforms:softwareinfo<software<<"
        );
}

#[test]
fn test_compute_caps_hash_ignores_forms_with_duplicate_form_type_fields() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info(), Feature::disco_items()];
    let baseline = compute_caps_hash(&identities, &features);
    let extensions = vec![software_info_form_with_duplicate_form_type()];

    let hash = compute_caps_hash_with_extensions(&identities, &features, &extensions);
    assert_eq!(hash, baseline);
}

#[test]
fn test_compute_caps_hash_ignores_forms_with_missing_var_fields() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info(), Feature::disco_items()];
    let baseline = compute_caps_hash(&identities, &features);
    let extensions = vec![software_info_form_with_missing_var()];

    let hash = compute_caps_hash_with_extensions(&identities, &features, &extensions);
    assert_eq!(hash, baseline);
}

#[test]
fn test_compute_caps_hash_deterministic() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info(), Feature::disco_items()];

    let hash1 = compute_caps_hash(&identities, &features);
    let hash2 = compute_caps_hash(&identities, &features);

    assert_eq!(hash1, hash2);
}

#[test]
fn test_compute_caps_hash_different_for_different_inputs() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features1 = vec![Feature::disco_info()];
    let features2 = vec![Feature::disco_info(), Feature::disco_items()];

    let hash1 = compute_caps_hash(&identities, &features1);
    let hash2 = compute_caps_hash(&identities, &features2);

    assert_ne!(hash1, hash2);
}

#[test]
fn test_build_caps_element() {
    let identities = vec![Identity::server(Some("Waddle"))];
    let features = vec![Feature::disco_info()];

    let elem = build_caps_element(WADDLE_CAPS_NODE, &identities, &features);

    assert_eq!(elem.name(), "c");
    assert_eq!(elem.ns(), NS_CAPS);
    assert_eq!(elem.attr("hash"), Some("sha-1"));
    assert_eq!(elem.attr("node"), Some(WADDLE_CAPS_NODE));
    let ver = elem.attr("ver").unwrap();
    assert!(BASE64.decode(ver).is_ok());
}

#[test]
fn test_build_caps_element_with_extensions_uses_extension_hash() {
    let identities = vec![
        Identity::new("client", "pc", Some("Psi 0.11")).with_lang(Some("en")),
        Identity::new("client", "pc", Some("\u{03A8} 0.11")).with_lang(Some("el")),
    ];
    let features = vec![
        Feature::new("http://jabber.org/protocol/muc"),
        Feature::new(DISCO_INFO_NS),
        Feature::new(NS_CAPS),
        Feature::new("http://jabber.org/protocol/disco#items"),
    ];
    let extensions = vec![software_info_form()];

    let elem =
        build_caps_element_with_extensions(WADDLE_CAPS_NODE, &identities, &features, &extensions);

    assert_eq!(elem.attr("ver"), Some("q07IKJEyjvHSyhy//CH0CxmKi8w="));
}

#[test]
fn test_extract_caps_from_presence() {
    let caps_elem = Caps::new("https://test.com", "abc123").build_element();
    let presence = Element::builder("presence", "jabber:client")
        .append(caps_elem)
        .build();

    let caps = extract_caps_from_presence(&presence).unwrap();
    assert_eq!(caps.node, "https://test.com");
    assert_eq!(caps.ver, "abc123");
}

#[test]
fn test_extract_caps_from_presence_no_caps() {
    let presence = Element::builder("presence", "jabber:client").build();
    assert!(extract_caps_from_presence(&presence).is_none());
}

#[test]
fn test_is_caps_node_query() {
    assert!(is_caps_node_query(Some(
        "https://waddle.social/caps#abc123"
    )));
    assert!(is_caps_node_query(Some("node#ver")));
    assert!(!is_caps_node_query(Some("plain-node")));
    assert!(!is_caps_node_query(None));
}

#[test]
fn test_parse_caps_node() {
    let result = parse_caps_node("https://waddle.social/caps#abc123");
    assert_eq!(result, Some(("https://waddle.social/caps", "abc123")));

    let result = parse_caps_node("no-hash-here");
    assert_eq!(result, None);
}
