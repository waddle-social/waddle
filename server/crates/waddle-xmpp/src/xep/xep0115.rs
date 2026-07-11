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
            .attr(minidom::rxml::xml_ncname!("hash").to_owned(), &self.hash)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), &self.node)
            .attr(minidom::rxml::xml_ncname!("ver").to_owned(), &self.ver)
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

/// Typed wrapper for an XEP-0115 verification string. The `ver` is
/// opaque base64-encoded ciphertext at the protocol layer; the
/// newtype prevents accidental confusion with other strings (JIDs,
/// node names, hash-algo identifiers) and satisfies the
/// typed-payloads hard rule for protocol data carried past the I/O
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapsVer(String);

impl CapsVer {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CapsVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CapsVer {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for CapsVer {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&String> for CapsVer {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

/// Composite cache key that XEP-0115 §6 mandates: caching is per
/// `(hash algorithm, verification string)`. Two clients advertising
/// the same opaque `ver` under different hash families MUST not
/// collide in the cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapsCacheKey {
    pub hash: String,
    pub ver: CapsVer,
}

impl CapsCacheKey {
    pub fn new(hash: impl Into<String>, ver: impl Into<CapsVer>) -> Self {
        Self {
            hash: hash.into(),
            ver: ver.into(),
        }
    }

    pub fn from_caps(caps: &Caps) -> Self {
        Self::new(caps.hash.clone(), caps.ver.clone())
    }
}

/// Default soft cap on cached `(hash, ver)` entries before the
/// least-recently-used entry is evicted. XEP-0115 §6 RECOMMENDS
/// long-lived caching, but unbounded growth from rotating client
/// versions is operationally untenable. 10k entries is generous
/// for typical deployments and bounded.
pub const DEFAULT_CAPS_CACHE_CAPACITY: usize = 10_000;

/// Cache for entity capabilities.
///
/// Keyed on `(hash, ver)` per XEP-0115 §6. Bounded by an LRU policy
/// so a long-lived server with rotating client populations doesn't
/// grow without bound.
#[derive(Clone)]
pub struct CapsCache {
    inner: Arc<std::sync::Mutex<lru::LruCache<CapsCacheKey, CachedDiscoInfo>>>,
}

impl std::fmt::Debug for CapsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.inner.lock().map(|g| g.len()).unwrap_or_default();
        f.debug_struct("CapsCache").field("len", &len).finish()
    }
}

impl Default for CapsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CapsCache {
    /// Create a new empty capabilities cache with the default LRU
    /// capacity (`DEFAULT_CAPS_CACHE_CAPACITY`).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPS_CACHE_CAPACITY)
    }

    /// Create a new empty capabilities cache with a custom LRU
    /// capacity. Use this in tests to exercise the eviction policy.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap =
            std::num::NonZeroUsize::new(capacity.max(1)).expect("max(1) above guarantees nonzero");
        Self {
            inner: Arc::new(std::sync::Mutex::new(lru::LruCache::new(cap))),
        }
    }

    /// Store disco#info for a (hash, ver) tuple. Evicts the LRU entry
    /// if at capacity.
    pub fn insert(&self, key: CapsCacheKey, info: CachedDiscoInfo) {
        debug!(
            hash = %key.hash,
            ver = %key.ver,
            identities = info.identities.len(),
            features = info.features.len(),
            "Caching caps"
        );
        if let Ok(mut guard) = self.inner.lock() {
            guard.put(key, info);
        }
    }

    /// Retrieve cached disco#info, refreshing LRU recency.
    pub fn get(&self, key: &CapsCacheKey) -> Option<CachedDiscoInfo> {
        self.inner
            .lock()
            .ok()
            .and_then(|mut guard| guard.get(key).cloned())
    }

    /// Non-recency-affecting presence check.
    pub fn contains(&self, key: &CapsCacheKey) -> bool {
        self.inner
            .lock()
            .ok()
            .map(|guard| guard.contains(key))
            .unwrap_or(false)
    }

    /// Remove a cached entry.
    pub fn remove(&self, key: &CapsCacheKey) -> Option<CachedDiscoInfo> {
        self.inner.lock().ok().and_then(|mut guard| guard.pop(key))
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|guard| guard.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear();
        }
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
        let var = field.attr("var")?;

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
                form_type = values
                    .first()
                    .filter(|value| !value.trim().is_empty())
                    .cloned();
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
mod tests;
