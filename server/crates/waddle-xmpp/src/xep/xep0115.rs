//! XEP-0115: Entity Capabilities
//!
//! Implements entity capabilities (caps) for efficient service discovery caching.
//! This allows clients to avoid repeated disco#info queries by caching capabilities
//! based on a verification hash.
//!
//! ## Key Components
//!
//! - `Caps`: The `<c>` element included in presence stanzas
//! - `compute_caps_hash()`: Generates the verification string per Section 5
//! - `CapsCache`: Stores hash-to-features mappings for received caps
//!
//! ## References
//!
//! - <https://xmpp.org/extensions/xep-0115.html>

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use dashmap::DashMap;
use minidom::Element;
use sha1::{Digest, Sha1};
use std::sync::Arc;
use tracing::debug;

use crate::disco::info::{Feature, Identity};

/// XEP-0115 Entity Capabilities namespace.
pub const NS_CAPS: &str = "http://jabber.org/protocol/caps";

/// Default node for Waddle's capabilities.
pub const WADDLE_CAPS_NODE: &str = "https://waddle.social/caps";

const DATA_FORMS_NS: &str = "jabber:x:data";
const FORM_TYPE_FIELD: &str = "FORM_TYPE";

/// Entity Capabilities element (`<c xmlns='http://jabber.org/protocol/caps'>`).
///
/// Included in presence stanzas to advertise capabilities via a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// Hash algorithm used (always "sha-1" per XEP-0115)
    pub hash: String,
    /// Node identifying the software/version (e.g., "https://waddle.social/caps")
    pub node: String,
    /// Verification string (base64-encoded hash of sorted disco#info)
    pub ver: String,
}

impl Caps {
    /// Create a new Caps element with SHA-1 hash.
    pub fn new(node: &str, ver: &str) -> Self {
        Self {
            hash: "sha-1".to_string(),
            node: node.to_string(),
            ver: ver.to_string(),
        }
    }

    /// Create Caps for Waddle server with the given verification string.
    pub fn waddle(ver: &str) -> Self {
        Self::new(WADDLE_CAPS_NODE, ver)
    }

    /// Get the node#ver string used for disco#info queries with caps.
    pub fn node_ver(&self) -> String {
        format!("{}#{}", self.node, self.ver)
    }

    /// Build the `<c>` element for inclusion in presence stanzas.
    pub fn build_element(&self) -> Element {
        Element::builder("c", NS_CAPS)
            .attr("hash", &self.hash)
            .attr("node", &self.node)
            .attr("ver", &self.ver)
            .build()
    }

    /// Parse a Caps element from a minidom Element.
    pub fn from_element(elem: &Element) -> Option<Self> {
        if elem.name() != "c" || elem.ns() != NS_CAPS {
            return None;
        }

        let hash = elem.attr("hash")?.to_string();
        let node = elem.attr("node")?.to_string();
        let ver = elem.attr("ver")?.to_string();

        Some(Self { hash, node, ver })
    }
}

/// Cached disco#info response for a capabilities hash.
#[derive(Debug, Clone)]
pub struct CachedDiscoInfo {
    /// Identities from disco#info
    pub identities: Vec<Identity>,
    /// Features from disco#info
    pub features: Vec<Feature>,
}

impl CachedDiscoInfo {
    /// Create a new cached disco#info entry.
    pub fn new(identities: Vec<Identity>, features: Vec<Feature>) -> Self {
        Self {
            identities,
            features,
        }
    }
}

/// Cache for entity capabilities.
///
/// Maps verification hashes to disco#info responses for efficient lookups.
#[derive(Debug, Clone)]
pub struct CapsCache {
    /// Map from verification hash to disco#info data
    cache: Arc<DashMap<String, CachedDiscoInfo>>,
}

impl Default for CapsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CapsCache {
    /// Create a new empty capabilities cache.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
        }
    }

    /// Store disco#info for a capabilities hash.
    pub fn insert(&self, hash: &str, info: CachedDiscoInfo) {
        debug!(hash = %hash, identities = info.identities.len(), features = info.features.len(), "Caching caps");
        self.cache.insert(hash.to_string(), info);
    }

    /// Retrieve cached disco#info for a capabilities hash.
    pub fn get(&self, hash: &str) -> Option<CachedDiscoInfo> {
        self.cache.get(hash).map(|entry| entry.value().clone())
    }

    /// Check if a capabilities hash is cached.
    pub fn contains(&self, hash: &str) -> bool {
        self.cache.contains_key(hash)
    }

    /// Remove a cached entry.
    pub fn remove(&self, hash: &str) -> Option<CachedDiscoInfo> {
        self.cache.remove(hash).map(|(_, v)| v)
    }

    /// Get the number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        self.cache.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapsDataForm {
    form_type: String,
    fields: Vec<CapsDataField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapsDataField {
    var: String,
    values: Vec<String>,
}

/// Compute the capabilities verification string per XEP-0115 Section 5.
///
/// The verification string is computed as:
/// 1. Sort identities by category/type/lang/name
/// 2. Sort features alphabetically
/// 3. Sort XEP-0128 data forms by FORM_TYPE and append their fields/values
/// 4. Hash with SHA-1
/// 5. Base64 encode
///
/// ## Arguments
///
/// * `identities` - List of disco#info identities
/// * `features` - List of disco#info features
///
/// ## Returns
///
/// Base64-encoded SHA-1 hash of the verification string.
///
/// ## Example
///
/// ```
/// use waddle_xmpp::disco::info::{Feature, Identity};
/// use waddle_xmpp::xep::xep0115::compute_caps_hash;
///
/// let identities = vec![Identity::server(Some("Test Server"))];
/// let features = vec![Feature::disco_info(), Feature::disco_items()];
/// let hash = compute_caps_hash(&identities, &features);
/// ```
pub fn compute_caps_hash(identities: &[Identity], features: &[Feature]) -> String {
    compute_caps_hash_with_extensions(identities, features, &[])
}

/// Compute the capabilities verification string including XEP-0128 disco forms.
pub fn compute_caps_hash_with_extensions(
    identities: &[Identity],
    features: &[Feature],
    extensions: &[Element],
) -> String {
    let verification_string =
        build_verification_string_with_extensions(identities, features, extensions);
    hash_verification_string(&verification_string)
}

/// Build the verification string from identities, features, and XEP-0128 forms.
fn build_verification_string_with_extensions(
    identities: &[Identity],
    features: &[Feature],
    extensions: &[Element],
) -> String {
    let mut s = String::new();

    let mut sorted_identities: Vec<_> = identities.iter().collect();
    sorted_identities.sort_by(|a, b| {
        (
            a.category.as_str(),
            a.type_.as_str(),
            a.lang.as_deref().unwrap_or(""),
            a.name.as_deref().unwrap_or(""),
        )
            .cmp(&(
                b.category.as_str(),
                b.type_.as_str(),
                b.lang.as_deref().unwrap_or(""),
                b.name.as_deref().unwrap_or(""),
            ))
    });

    for id in sorted_identities {
        s.push_str(&id.category);
        s.push('/');
        s.push_str(&id.type_);
        s.push('/');
        if let Some(ref lang) = id.lang {
            s.push_str(lang);
        }
        s.push('/');
        if let Some(ref name) = id.name {
            s.push_str(name);
        }
        s.push('<');
    }

    let mut sorted_features: Vec<_> = features.iter().map(|f| f.0.as_str()).collect();
    sorted_features.sort();

    for feat in sorted_features {
        s.push_str(feat);
        s.push('<');
    }

    let mut data_forms: Vec<_> = extensions.iter().filter_map(parse_caps_data_form).collect();
    data_forms.sort_by(|a, b| a.form_type.cmp(&b.form_type));

    for form in data_forms {
        s.push_str(&form.form_type);
        s.push('<');

        let mut fields = form.fields;
        fields.sort_by(|a, b| a.var.cmp(&b.var));

        for field in fields {
            s.push_str(&field.var);
            s.push('<');

            let mut values = field.values;
            values.sort();
            if values.is_empty() {
                s.push('<');
                continue;
            }

            for value in values {
                s.push_str(&value);
                s.push('<');
            }
        }
    }

    s
}

fn parse_caps_data_form(extension: &Element) -> Option<CapsDataForm> {
    if extension.name() != "x" || extension.ns() != DATA_FORMS_NS {
        return None;
    }

    if extension.attr("type") != Some("result") {
        return None;
    }

    let mut form_type = None;
    let mut fields = Vec::new();

    for field in extension
        .children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
    {
        let Some(var) = field.attr("var") else {
            return None;
        };

        let values = field
            .children()
            .filter(|child| child.name() == "value" && child.ns() == DATA_FORMS_NS)
            .map(Element::text)
            .collect::<Vec<_>>();

        if var == FORM_TYPE_FIELD {
            if form_type.is_some() {
                return None;
            }

            if field.attr("type") == Some("hidden") {
                form_type = values.first().cloned();
            }
            continue;
        }

        fields.push(CapsDataField {
            var: var.to_string(),
            values,
        });
    }

    form_type.map(|form_type| CapsDataForm { form_type, fields })
}

/// Hash the verification string with SHA-1 and base64 encode.
fn hash_verification_string(verification_string: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(verification_string.as_bytes());
    let result = hasher.finalize();
    BASE64.encode(result)
}

/// Build a `<c>` caps element for presence stanzas.
///
/// ## Arguments
///
/// * `node` - The node URL (e.g., "https://waddle.social/caps")
/// * `identities` - Server/client identities for hash computation
/// * `features` - Supported features for hash computation
///
/// ## Returns
///
/// A minidom Element containing the `<c>` element with computed hash.
pub fn build_caps_element(node: &str, identities: &[Identity], features: &[Feature]) -> Element {
    build_caps_element_with_extensions(node, identities, features, &[])
}

/// Build a `<c>` caps element whose `ver` accounts for XEP-0128 extensions.
pub fn build_caps_element_with_extensions(
    node: &str,
    identities: &[Identity],
    features: &[Feature],
    extensions: &[Element],
) -> Element {
    let ver = compute_caps_hash_with_extensions(identities, features, extensions);
    Caps::new(node, &ver).build_element()
}

/// Build Waddle's standard entity capabilities element for presence stanzas.
pub fn build_waddle_caps_element() -> Element {
    let identities = vec![crate::disco::Identity::server(Some("Waddle XMPP Server"))];
    let features = crate::disco::server_features();
    build_caps_element(WADDLE_CAPS_NODE, &identities, &features)
}

/// Ensure a payload list contains an entity capabilities advertisement.
///
/// If the payload list already contains any XEP-0115 `<c/>` element, it is
/// preserved as-is to avoid duplicating caps payloads in the same stanza.
pub fn ensure_caps_payload(payloads: &mut Vec<Element>) {
    if payloads
        .iter()
        .any(|payload| payload.name() == "c" && payload.ns() == NS_CAPS)
    {
        return;
    }

    payloads.push(build_waddle_caps_element());
}

/// Extract Caps from a presence stanza.
pub fn extract_caps_from_presence(presence: &Element) -> Option<Caps> {
    presence
        .children()
        .find(|child| child.name() == "c" && child.ns() == NS_CAPS)
        .and_then(Caps::from_element)
}

/// Check if a disco#info query is for a specific caps node.
///
/// Caps nodes are in the format "node#ver".
pub fn is_caps_node_query(node: Option<&str>) -> bool {
    node.map(|n| n.contains('#')).unwrap_or(false)
}

/// Parse a caps node query to extract the base node and verification string.
///
/// ## Arguments
///
/// * `node` - The node string from disco#info query (e.g., "https://waddle.social/caps#hash")
///
/// ## Returns
///
/// A tuple of (node, ver) if the node contains a '#', otherwise None.
pub fn parse_caps_node(node: &str) -> Option<(&str, &str)> {
    node.split_once('#')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disco::info::{Feature, Identity, DISCO_INFO_NS};

    fn data_form_field(var: &str, field_type: Option<&str>, values: &[&str]) -> Element {
        let mut builder = Element::builder("field", DATA_FORMS_NS).attr("var", var);

        if let Some(field_type) = field_type {
            builder = builder.attr("type", field_type);
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
            .attr("type", "result")
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
        form.set_attr("type", form_type);
        form
    }

    fn software_info_form_with_form_type_field_type(field_type: &str) -> Element {
        Element::builder("x", DATA_FORMS_NS)
            .attr("type", "result")
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
            .attr("type", "result")
            .append(data_form_field(
                FORM_TYPE_FIELD,
                Some("hidden"),
                &["urn:xmpp:dataforms:softwareinfo"],
            ))
            .append(
                Element::builder("field", "urn:waddle:test:foreign")
                    .attr("var", "rogue-field")
                    .append(
                        Element::builder("value", "urn:waddle:test:foreign")
                            .append("rogue")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", DATA_FORMS_NS)
                    .attr("var", "software")
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
            .attr("type", "result")
            .append(data_form_field(
                FORM_TYPE_FIELD,
                Some("hidden"),
                &["urn:xmpp:dataforms:softwareinfo"],
            ))
            .append(
                Element::builder("field", DATA_FORMS_NS)
                    .attr("var", "software")
                    .build(),
            )
            .build()
    }

    fn software_info_form_with_duplicate_form_type() -> Element {
        Element::builder("x", DATA_FORMS_NS)
            .attr("type", "result")
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
            .attr("type", "result")
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
            .attr("hash", "sha-1")
            .attr("node", "https://test.com")
            .attr("ver", "xyz789")
            .build();

        let caps = Caps::from_element(&elem).unwrap();
        assert_eq!(caps.hash, "sha-1");
        assert_eq!(caps.node, "https://test.com");
        assert_eq!(caps.ver, "xyz789");
    }

    #[test]
    fn test_caps_from_element_wrong_name() {
        let elem = Element::builder("x", NS_CAPS)
            .attr("hash", "sha-1")
            .attr("node", "https://test.com")
            .attr("ver", "xyz789")
            .build();

        assert!(Caps::from_element(&elem).is_none());
    }

    #[test]
    fn test_caps_from_element_wrong_ns() {
        let elem = Element::builder("c", "wrong:ns")
            .attr("hash", "sha-1")
            .attr("node", "https://test.com")
            .attr("ver", "xyz789")
            .build();

        assert!(Caps::from_element(&elem).is_none());
    }

    #[test]
    fn test_caps_from_element_missing_attrs() {
        let elem = Element::builder("c", NS_CAPS)
            .attr("hash", "sha-1")
            // missing node and ver
            .build();

        assert!(Caps::from_element(&elem).is_none());
    }

    #[test]
    fn test_caps_cache_insert_and_get() {
        let cache = CapsCache::new();
        let info = CachedDiscoInfo::new(
            vec![Identity::server(Some("Test"))],
            vec![Feature::disco_info()],
        );

        cache.insert("test-hash", info.clone());

        let retrieved = cache.get("test-hash").unwrap();
        assert_eq!(retrieved.identities.len(), 1);
        assert_eq!(retrieved.features.len(), 1);
    }

    #[test]
    fn test_caps_cache_contains() {
        let cache = CapsCache::new();
        let info = CachedDiscoInfo::new(vec![], vec![]);

        assert!(!cache.contains("hash1"));
        cache.insert("hash1", info);
        assert!(cache.contains("hash1"));
    }

    #[test]
    fn test_caps_cache_remove() {
        let cache = CapsCache::new();
        let info = CachedDiscoInfo::new(vec![], vec![]);

        cache.insert("hash1", info);
        assert!(cache.contains("hash1"));

        let removed = cache.remove("hash1");
        assert!(removed.is_some());
        assert!(!cache.contains("hash1"));
    }

    #[test]
    fn test_caps_cache_len_and_clear() {
        let cache = CapsCache::new();

        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());

        cache.insert("h1", CachedDiscoInfo::new(vec![], vec![]));
        cache.insert("h2", CachedDiscoInfo::new(vec![], vec![]));

        assert_eq!(cache.len(), 2);
        assert!(!cache.is_empty());

        cache.clear();
        assert!(cache.is_empty());
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
            .attr("type", "result")
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

        let elem = build_caps_element_with_extensions(
            WADDLE_CAPS_NODE,
            &identities,
            &features,
            &extensions,
        );

        assert_eq!(elem.attr("ver"), Some("q07IKJEyjvHSyhy//CH0CxmKi8w="));
    }

    #[test]
    fn test_build_waddle_caps_element() {
        let elem = build_waddle_caps_element();

        assert_eq!(elem.name(), "c");
        assert_eq!(elem.ns(), NS_CAPS);
        assert_eq!(elem.attr("node"), Some(WADDLE_CAPS_NODE));
        assert_eq!(elem.attr("hash"), Some("sha-1"));
    }

    #[test]
    fn test_ensure_caps_payload_adds_caps_once() {
        let mut payloads = Vec::new();

        ensure_caps_payload(&mut payloads);
        ensure_caps_payload(&mut payloads);

        let caps_payloads: Vec<_> = payloads
            .iter()
            .filter(|payload| payload.name() == "c" && payload.ns() == NS_CAPS)
            .collect();
        assert_eq!(caps_payloads.len(), 1);
        assert_eq!(caps_payloads[0].attr("node"), Some(WADDLE_CAPS_NODE));
    }

    #[test]
    fn test_ensure_caps_payload_preserves_existing_caps() {
        let existing = Caps::new("https://example.com/caps", "existing").build_element();
        let mut payloads = vec![existing.clone()];

        ensure_caps_payload(&mut payloads);

        assert_eq!(payloads, vec![existing]);
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
}
