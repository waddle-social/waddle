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

/// Compute the capabilities verification string per XEP-0115 Section 5.
///
/// The verification string is computed as:
/// 1. Sort identities by category/type/lang/name
/// 2. Sort features alphabetically
/// 3. Concatenate in a specific format with '<' delimiters
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
/// use waddle_xmpp::disco::info::{Identity, Feature};
/// use waddle_xmpp::xep::xep0115::compute_caps_hash;
///
/// let identities = vec![Identity::server(Some("Test Server"))];
/// let features = vec![
///     Feature::disco_info(),
///     Feature::disco_items(),
/// ];
/// let hash = compute_caps_hash(&identities, &features);
/// ```
pub fn compute_caps_hash(identities: &[Identity], features: &[Feature]) -> String {
    let verification_string = build_verification_string(identities, features);
    hash_verification_string(&verification_string)
}

/// Build the verification string from identities and features.
///
/// Per XEP-0115 Section 5.1:
/// 1. For each identity: "category/type/lang/name<"
/// 2. For each feature: "feature<"
/// 3. (Extensions omitted for simplicity - add when needed)
fn build_verification_string(identities: &[Identity], features: &[Feature]) -> String {
    let mut s = String::new();

    // Sort and add identities
    // Format: category/type/lang/name<
    let mut sorted_identities: Vec<_> = identities.iter().collect();
    sorted_identities.sort_by(|a, b| {
        (&a.category, &a.type_, &a.name)
            .cmp(&(&b.category, &b.type_, &b.name))
    });

    for id in sorted_identities {
        s.push_str(&id.category);
        s.push('/');
        s.push_str(&id.type_);
        s.push('/'); // lang (empty for now)
        s.push('/');
        if let Some(ref name) = id.name {
            s.push_str(name);
        }
        s.push('<');
    }

    // Sort and add features
    let mut sorted_features: Vec<_> = features.iter().map(|f| &f.0).collect();
    sorted_features.sort();

    for feat in sorted_features {
        s.push_str(feat);
        s.push('<');
    }

    s
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
pub fn build_caps_element(
    node: &str,
    identities: &[Identity],
    features: &[Feature],
) -> Element {
    let ver = compute_caps_hash(identities, features);
    Caps::new(node, &ver).build_element()
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
        let s = build_verification_string(&[], &[]);
        assert_eq!(s, "");
    }

    #[test]
    fn test_build_verification_string_identity_only() {
        let identities = vec![Identity::server(Some("Test Server"))];
        let s = build_verification_string(&identities, &[]);
        // Format: category/type/lang/name<
        assert_eq!(s, "server/im//Test Server<");
    }

    #[test]
    fn test_build_verification_string_identity_no_name() {
        let identities = vec![Identity::server(None)];
        let s = build_verification_string(&identities, &[]);
        assert_eq!(s, "server/im//<");
    }

    #[test]
    fn test_build_verification_string_features_only() {
        let features = vec![
            Feature::new("http://jabber.org/protocol/disco#info"),
            Feature::new("http://jabber.org/protocol/disco#items"),
        ];
        let s = build_verification_string(&[], &features);
        // Features should be sorted alphabetically
        assert_eq!(s, "http://jabber.org/protocol/disco#info<http://jabber.org/protocol/disco#items<");
    }

    #[test]
    fn test_build_verification_string_features_sorted() {
        let features = vec![
            Feature::new("z-feature"),
            Feature::new("a-feature"),
            Feature::new("m-feature"),
        ];
        let s = build_verification_string(&[], &features);
        assert_eq!(s, "a-feature<m-feature<z-feature<");
    }

    #[test]
    fn test_build_verification_string_full() {
        let identities = vec![
            Identity::server(Some("Test")),
            Identity::new("client", "pc", Some("MyClient")),
        ];
        let features = vec![
            Feature::new("feature2"),
            Feature::new("feature1"),
        ];
        let s = build_verification_string(&identities, &features);
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
    fn test_compute_caps_hash_example() {
        // Based on XEP-0115 Section 5.2 example (simplified)
        // Identity: client/pc/en/Exodus 0.9.1
        // Features sorted:
        //   http://jabber.org/protocol/caps
        //   http://jabber.org/protocol/disco#info
        //   http://jabber.org/protocol/disco#items
        //   http://jabber.org/protocol/muc
        let identities = vec![Identity::new("client", "pc", Some("Exodus 0.9.1"))];
        let features = vec![
            Feature::new(NS_CAPS),
            Feature::new(DISCO_INFO_NS),
            Feature::new("http://jabber.org/protocol/disco#items"),
            Feature::new("http://jabber.org/protocol/muc"),
        ];

        let hash = compute_caps_hash(&identities, &features);

        // The hash should be a valid base64 string of 28 characters (20 bytes base64 encoded)
        assert_eq!(hash.len(), 28);
        assert!(BASE64.decode(&hash).is_ok());
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
        // ver should be a valid hash
        let ver = elem.attr("ver").unwrap();
        assert!(BASE64.decode(ver).is_ok());
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
        assert!(is_caps_node_query(Some("https://waddle.social/caps#abc123")));
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
