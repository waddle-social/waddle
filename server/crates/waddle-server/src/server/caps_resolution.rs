//! XEP-0115 entity-capabilities resolution for inbound presence.
//!
//! When a connected resource advertises `<c hash node ver/>` on a
//! presence stanza, the server either:
//!
//! - **Cache hit:** records `(full_jid → ver)` so XEP-0163 §3 fan-out
//!   (PR 2) can filter notifications by per-resource feature lists.
//! - **Cache miss with supported `hash`:** sends a typed `disco#info`
//!   IQ get to the resource with `node="<NODE>#<VER>"` (XEP-0115 §6.2).
//!   The reply is verified per §5.4 — the recipient MUST recompute the
//!   verification string from the disco#info response (identities +
//!   features + XEP-0128 forms) and only cache when the recomputed
//!   hash matches the advertised `ver`.
//! - **Unsupported `hash` value (e.g. `sha-256` advertised but server
//!   only implements SHA-1):** XEP-0115 §5.4 step 2 forbids global
//!   caching but does NOT forbid per-session knowledge. We currently
//!   skip both the disco#info round-trip and the cache write; the
//!   resource's caps are simply unknown to fan-out for that session.
//!
//! Cache keying is per `(hash algorithm, ver)` per XEP-0115 §6.
//!
//! On disconnect, the per-resource mapping AND any in-flight pending
//! resolution for that resource are dropped while the bounded LRU
//! `CapsCache` itself stays warm so the next session can reuse
//! cross-session knowledge (XEP-0115 §6).

use std::sync::Arc;

use dashmap::DashMap;
use jid::{FullJid, Jid};
use waddle_xmpp::disco::info::{Feature, Identity, DISCO_INFO_NS};
use waddle_xmpp::xep::xep0115::{
    compute_caps_hash_with_extensions, CachedDiscoInfo, Caps, CapsCache, CapsCacheKey,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

/// XEP-0115 §8.1: SHA-1 is mandatory-to-implement and is the only
/// hash family this server validates today. Per §5.4 step 2, an
/// advertised hash outside this set means "do NOT cache globally,
/// do NOT invent a verification" — see `is_supported_hash`.
const SUPPORTED_HASHES: &[&str] = &["sha-1"];

pub fn is_supported_hash(hash: &str) -> bool {
    SUPPORTED_HASHES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(hash))
}

/// Outcome of recomputing and verifying a disco#info reply against an
/// advertised `ver` per XEP-0115 §5.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsVerification {
    /// Recomputed hash matched `ver`; the result MUST be cached and
    /// the resource→ver mapping recorded.
    Match,
    /// Recomputed hash did not match `ver`; the result MUST NOT be
    /// cached (poisoning defense, §8.1).
    Mismatch,
    /// The disco#info reply is ill-formed per §5.4 step 2.4
    /// (e.g. duplicate identity, duplicate feature, multiple
    /// FORM_TYPE values in one form). The whole response MUST be
    /// discarded.
    IllFormed,
}

/// Pending state for an outstanding disco#info IQ get the server has
/// sent to a resource that advertised an unknown `ver`.
#[derive(Debug, Clone)]
pub struct PendingCapsResolution {
    pub full_jid: FullJid,
    pub caps: Caps,
}

/// Server-side resolver for XEP-0115 entity capabilities.
///
/// The `cache` is process-wide and survives individual sessions
/// (XEP-0115 §6 — caching across sessions is RECOMMENDED).
/// `resource_to_ver` is per-session and cleared on disconnect.
/// `pending` tracks outstanding disco#info queries by IQ id so the
/// inbound IQ result handler can match a reply to the original ver.
#[derive(Clone)]
pub struct CapsResolver {
    cache: Arc<CapsCache>,
    resource_to_ver: Arc<DashMap<FullJid, CapsCacheKey>>,
    pending: Arc<DashMap<String, PendingCapsResolution>>,
}

impl Default for CapsResolver {
    fn default() -> Self {
        Self::new(Arc::new(CapsCache::new()))
    }
}

impl CapsResolver {
    pub fn new(cache: Arc<CapsCache>) -> Self {
        Self {
            cache,
            resource_to_ver: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
        }
    }

    pub fn cache(&self) -> &Arc<CapsCache> {
        &self.cache
    }

    /// Record that `full_jid` is advertising `(hash, ver)`. Used both
    /// on a cache hit and after a successful verification.
    pub fn record_resource(&self, full_jid: &FullJid, key: CapsCacheKey) {
        self.resource_to_ver.insert(full_jid.clone(), key);
    }

    /// Drop the per-resource mapping AND any in-flight pending
    /// disco#info resolution for that resource. Called on resource
    /// disconnect (live or detached-session expiry). Leaves the
    /// hash-keyed cache warm.
    pub fn drop_resource(&self, full_jid: &FullJid) {
        self.resource_to_ver.remove(full_jid);
        self.pending.retain(|_, v| v.full_jid != *full_jid);
    }

    /// Return the (hash, ver) advertised by `full_jid`, if any.
    pub fn key_for_resource(&self, full_jid: &FullJid) -> Option<CapsCacheKey> {
        self.resource_to_ver
            .get(full_jid)
            .map(|v| v.value().clone())
    }

    /// Look up the cached identity+features for a (hash, ver) tuple.
    pub fn cached(&self, key: &CapsCacheKey) -> Option<CachedDiscoInfo> {
        self.cache.get(key)
    }

    /// Begin tracking a pending disco#info resolution for `caps`
    /// keyed by `iq_id`. Used for cache misses where the server
    /// just sent the disco#info IQ get.
    pub fn begin_pending(&self, iq_id: String, full_jid: FullJid, caps: Caps) {
        self.pending
            .insert(iq_id, PendingCapsResolution { full_jid, caps });
    }

    /// True iff there is already a pending disco#info resolution for
    /// this `(full_jid, hash, ver)` tuple. Callers use this to avoid
    /// queuing a second outbound query while the first is still in
    /// flight, which a malicious client could exploit by spamming
    /// presence updates with random `ver` values.
    pub fn has_pending_for(&self, full_jid: &FullJid, caps: &Caps) -> bool {
        self.pending.iter().any(|entry| {
            let v = entry.value();
            v.full_jid == *full_jid && v.caps.hash == caps.hash && v.caps.ver == caps.ver
        })
    }

    /// Number of currently-pending resolutions. Test/telemetry hook.
    #[doc(hidden)]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Take a pending entry by IQ id. Returns `None` if no resolution
    /// is in flight under that id (a stray result; ignore per §5.4).
    pub fn take_pending(&self, iq_id: &str) -> Option<PendingCapsResolution> {
        self.pending.remove(iq_id).map(|(_, v)| v)
    }

    /// Verify a disco#info reply for a previously-pending resolution
    /// per XEP-0115 §5.4.
    ///
    /// - On Match, the (hash, ver) is cached and the resource→key
    ///   mapping recorded.
    /// - On Mismatch, neither side effect occurs (poisoning defense).
    /// - On IllFormed, neither side effect occurs (§5.4 step 2.4).
    ///
    /// `extensions` MUST include any XEP-0128 `<x xmlns="jabber:x:data"
    /// type="result"/>` forms returned alongside identities/features —
    /// dropping them silently breaks verification for any client that
    /// emits a software-info form.
    pub fn complete_pending(
        &self,
        pending: PendingCapsResolution,
        identities: Vec<Identity>,
        features: Vec<Feature>,
        extensions: Vec<Element>,
        ill_formed: bool,
    ) -> CapsVerification {
        if ill_formed {
            return CapsVerification::IllFormed;
        }
        if !is_supported_hash(&pending.caps.hash) {
            // §5.4 step 2: unsupported hash → MUST NOT cache.
            return CapsVerification::Mismatch;
        }
        let recomputed = compute_caps_hash_with_extensions(&identities, &features, &extensions);
        if recomputed != pending.caps.ver {
            return CapsVerification::Mismatch;
        }
        let key = CapsCacheKey::from_caps(&pending.caps);
        self.cache
            .insert(key.clone(), CachedDiscoInfo::new(identities, features));
        self.record_resource(&pending.full_jid, key);
        CapsVerification::Match
    }
}

/// Typed wrapper around the server's authoritative XMPP domain. Parsed
/// once at startup so downstream call sites that need a `Jid` value
/// (e.g. server-issued IQ `from`) cannot panic on bad input —
/// validation is concentrated at the boundary per the typed-payloads
/// hard rule.
#[derive(Debug, Clone)]
pub struct ServerDomainJid(Jid);

impl ServerDomainJid {
    /// Parse and validate `domain` as a `Jid` once. Returns `Err` if
    /// the configured XMPP domain is not a valid JID — startup MUST
    /// fail loudly rather than reach a runtime panic.
    pub fn parse(domain: &str) -> Result<Self, jid::Error> {
        Ok(Self(domain.parse::<Jid>()?))
    }

    pub fn as_jid(&self) -> &Jid {
        &self.0
    }
}

/// Build a typed disco#info IQ get for caps resolution.
/// Per XEP-0115 §6.2 the query MUST carry `node="<NODE>#<VER>"`.
pub fn build_caps_disco_info_query(
    server_domain: &ServerDomainJid,
    target: &FullJid,
    caps: &Caps,
    iq_id: &str,
) -> Iq {
    let query = Element::builder("query", DISCO_INFO_NS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            caps.node_ver(),
        )
        .build();
    Iq::Get {
        from: Some(server_domain.as_jid().clone()),
        to: Some(Jid::from(target.clone())),
        id: iq_id.to_string(),
        payload: query,
    }
}

/// Extract a `<c hash node ver/>` payload from a typed `Presence`.
pub fn extract_caps_payload(presence: &xmpp_parsers::presence::Presence) -> Option<Caps> {
    for child in &presence.payloads {
        if child.name() == "c" && child.ns() == waddle_xmpp::xep::xep0115::NS_CAPS {
            return Caps::from_element(child);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::xep::xep0115::compute_caps_hash;

    fn sample_identities() -> Vec<Identity> {
        vec![Identity::server(Some("Waddle Test"))]
    }

    fn sample_features() -> Vec<Feature> {
        vec![Feature::disco_info(), Feature::disco_items()]
    }

    /// Build a XEP-0128 `<x xmlns="jabber:x:data" type="result">` form
    /// with a single FORM_TYPE field and one extra var. Used to
    /// exercise the extension path through `compute_caps_hash_with_extensions`.
    fn sample_extension_form(form_type: &str, software: &str) -> Element {
        Element::builder("x", "jabber:x:data")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("field", "jabber:x:data")
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                    .append(
                        Element::builder("value", "jabber:x:data")
                            .append(form_type)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", "jabber:x:data")
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "software")
                    .append(
                        Element::builder("value", "jabber:x:data")
                            .append(software)
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    #[test]
    fn complete_pending_with_matching_hash_caches_and_records() {
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let caps = Caps::new("https://example.test/caps", &ver);
        resolver.begin_pending("iq-1".into(), full_jid.clone(), caps.clone());
        let pending = resolver.take_pending("iq-1").expect("pending entry");

        let outcome =
            resolver.complete_pending(pending, identities.clone(), features.clone(), vec![], false);

        assert_eq!(outcome, CapsVerification::Match);
        let key = CapsCacheKey::new("sha-1", &ver);
        assert!(resolver.cached(&key).is_some());
        assert_eq!(resolver.key_for_resource(&full_jid), Some(key));
    }

    #[test]
    fn complete_pending_with_mismatched_hash_rejects_and_skips_cache() {
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let advertised = "ZZZdefinitely-not-the-real-hashZZZ";
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let caps = Caps::new("https://example.test/caps", advertised);
        resolver.begin_pending("iq-2".into(), full_jid.clone(), caps);
        let pending = resolver.take_pending("iq-2").expect("pending entry");

        let outcome = resolver.complete_pending(pending, identities, features, vec![], false);

        assert_eq!(outcome, CapsVerification::Mismatch);
        assert!(resolver
            .cached(&CapsCacheKey::new("sha-1", advertised))
            .is_none());
        assert_eq!(resolver.key_for_resource(&full_jid), None);
    }

    #[test]
    fn complete_pending_with_unsupported_hash_does_not_cache() {
        // PR #438 adversarial review issue #1: an unsupported `hash`
        // attribute MUST NOT be cached even if the recomputed value
        // happens to match by accident. We always reject when the
        // advertised hash isn't in our SUPPORTED_HASHES set.
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let caps = Caps {
            hash: "sha-256".to_string(),
            node: "https://example.test/caps".to_string(),
            ver: ver.clone(),
        };
        resolver.begin_pending("iq-unsupp".into(), full_jid.clone(), caps);
        let pending = resolver.take_pending("iq-unsupp").expect("pending");

        let outcome = resolver.complete_pending(pending, identities, features, vec![], false);

        assert_eq!(outcome, CapsVerification::Mismatch);
        assert!(resolver
            .cached(&CapsCacheKey::new("sha-256", &ver))
            .is_none());
        assert!(resolver.cached(&CapsCacheKey::new("sha-1", &ver)).is_none());
        assert_eq!(resolver.key_for_resource(&full_jid), None);
    }

    #[test]
    fn complete_pending_ill_formed_response_is_rejected() {
        // §5.4 step 2.4: ill-formed disco#info response MUST be
        // discarded (no caching, no mapping).
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let caps = Caps::new("https://example.test/caps", &ver);
        resolver.begin_pending("iq-bad".into(), full_jid.clone(), caps);
        let pending = resolver.take_pending("iq-bad").expect("pending");

        let outcome = resolver.complete_pending(pending, identities, features, vec![], true);

        assert_eq!(outcome, CapsVerification::IllFormed);
        assert!(resolver.cached(&CapsCacheKey::new("sha-1", &ver)).is_none());
        assert_eq!(resolver.key_for_resource(&full_jid), None);
    }

    #[test]
    fn complete_pending_includes_xep0128_form_in_recomputed_hash() {
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let extension = sample_extension_form("urn:xmpp:dataforms:softwareinfo", "Waddle");
        // ver was computed *with* the form. Without feeding the form
        // back into recomputation the verification would mismatch —
        // see Issue 1 in the PR 1 adversarial review.
        let ver = compute_caps_hash_with_extensions(
            &identities,
            &features,
            std::slice::from_ref(&extension),
        );
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let caps = Caps::new("https://example.test/caps", &ver);
        resolver.begin_pending("iq-3".into(), full_jid.clone(), caps);
        let pending = resolver.take_pending("iq-3").expect("pending entry");

        let outcome = resolver.complete_pending(
            pending,
            identities.clone(),
            features.clone(),
            vec![extension],
            false,
        );

        assert_eq!(outcome, CapsVerification::Match);
        assert_eq!(
            resolver.key_for_resource(&full_jid),
            Some(CapsCacheKey::new("sha-1", &ver))
        );
    }

    #[test]
    fn drop_resource_clears_mapping_and_pending_but_not_hash_cache() {
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let other_jid: FullJid = "bob@localhost/r1".parse().expect("jid");
        let key = CapsCacheKey::new("sha-1", &ver);
        resolver
            .cache()
            .insert(key.clone(), CachedDiscoInfo::new(identities, features));
        resolver.record_resource(&full_jid, key.clone());
        // Pending entry for the resource that's about to disconnect AND
        // an unrelated entry that MUST survive.
        resolver.begin_pending(
            "iq-leak".into(),
            full_jid.clone(),
            Caps::new("https://example.test/caps", &ver),
        );
        resolver.begin_pending(
            "iq-keep".into(),
            other_jid.clone(),
            Caps::new("https://example.test/caps", "other-ver"),
        );

        resolver.drop_resource(&full_jid);

        assert_eq!(resolver.key_for_resource(&full_jid), None);
        assert!(
            resolver.take_pending("iq-leak").is_none(),
            "drop_resource MUST also evict the pending entry to bound memory growth"
        );
        assert!(
            resolver.take_pending("iq-keep").is_some(),
            "drop_resource MUST not touch unrelated pending entries"
        );
        assert!(resolver.cached(&key).is_some());
    }

    #[test]
    fn server_domain_jid_parses_at_boundary() {
        assert!(ServerDomainJid::parse("localhost").is_ok());
        assert!(ServerDomainJid::parse("waddle.social").is_ok());
        assert!(ServerDomainJid::parse("").is_err());
    }
}
