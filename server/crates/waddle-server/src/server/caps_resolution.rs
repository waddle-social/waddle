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
//!   only implements SHA-1):** XEP-0115 §5.4 step 2 sends the same
//!   disco#info request but forbids validating or globally caching the
//!   result; the discovered identities and features are associated
//!   *only* with the generating full JID, so fan-out can still use
//!   them for that resource until it disconnects or advertises other
//!   caps.
//!
//! Cache keying is per `(hash algorithm, ver)` per XEP-0115 §6.
//!
//! The per-resource mapping and the pending queries are process-local: they
//! describe the sockets this replica holds. A verified `(hash, ver)` entry is
//! also written to the shared `entity_caps` table ([`EntityCapsStore`]), so
//! the replica that filters a XEP-0163 publication for a contact connected
//! elsewhere resolves that contact's caps from the `<c/>` in its replicated
//! presence ([`CapsResolver::info_for_presence`]). The in-memory
//! [`CapsCache`] is the read-through cache in front of the table.
//!
//! On disconnect, the per-resource mapping AND any in-flight pending
//! resolution for that resource are dropped while the bounded LRU
//! `CapsCache` itself stays warm so the next session can reuse
//! cross-session knowledge (XEP-0115 §6).

use std::sync::Arc;

use crate::db::entity_caps::EntityCapsStore;
use dashmap::DashMap;
use jid::{FullJid, Jid};
use tracing::warn;
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
    /// The advertised `hash` is not implemented here (§5.4 step 2): the
    /// reply was not validated and was associated only with the
    /// generating full JID, never with the global cache.
    ResourceOnly,
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
/// (XEP-0115 §6 — caching across sessions is RECOMMENDED); it reads
/// through to the `entity_caps` table the replicas share.
/// `resource_to_ver` is per-session and cleared on disconnect.
/// `pending` tracks outstanding disco#info queries by IQ id so the
/// inbound IQ result handler can match a reply to the original ver.
#[derive(Clone)]
pub struct CapsResolver {
    cache: Arc<CapsCache>,
    resource_to_ver: Arc<DashMap<FullJid, CapsCacheKey>>,
    /// Unvalidated disco#info results for resources that advertised an
    /// unsupported `hash`, keyed by the generating full JID only
    /// (§5.4 step 2). Never consulted for any other JID.
    resource_only: Arc<DashMap<FullJid, (CapsCacheKey, CachedDiscoInfo)>>,
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
            resource_only: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
        }
    }

    pub fn cache(&self) -> &Arc<CapsCache> {
        &self.cache
    }

    /// Record that `full_jid` is advertising `(hash, ver)`. Used both
    /// on a cache hit and after a successful verification.
    pub fn record_resource(&self, full_jid: &FullJid, key: CapsCacheKey) {
        self.resource_only.remove(full_jid);
        self.resource_to_ver.insert(full_jid.clone(), key);
    }

    /// Drop the per-resource mapping AND any in-flight pending
    /// disco#info resolution for that resource. Called on resource
    /// disconnect (live or detached-session expiry). Leaves the
    /// hash-keyed cache warm.
    pub fn drop_resource(&self, full_jid: &FullJid) {
        self.resource_to_ver.remove(full_jid);
        self.resource_only.remove(full_jid);
        self.pending.retain(|_, v| v.full_jid != *full_jid);
    }

    /// Forget the (hash, ver) recorded for `full_jid` without touching
    /// pending resolutions. Called when a resource becomes available
    /// again, so the caps it advertises in the new availability period
    /// are treated as new interest (XEP-0163 last-item replay).
    pub fn forget_resource_key(&self, full_jid: &FullJid) {
        self.resource_to_ver.remove(full_jid);
        self.resource_only.remove(full_jid);
    }

    /// Return the (hash, ver) advertised by `full_jid`, if any.
    pub fn key_for_resource(&self, full_jid: &FullJid) -> Option<CapsCacheKey> {
        self.resource_to_ver
            .get(full_jid)
            .map(|v| v.value().clone())
    }

    /// Identity+features known for `full_jid`: the globally cached,
    /// verified entry for its advertised (hash, ver), or the
    /// resource-only entry discovered for an unsupported hash.
    pub fn info_for_resource(&self, full_jid: &FullJid) -> Option<CachedDiscoInfo> {
        if let Some(key) = self.key_for_resource(full_jid) {
            return self.cached(&key);
        }
        self.resource_only
            .get(full_jid)
            .map(|entry| entry.value().1.clone())
    }

    /// True iff `full_jid` already has resource-only caps for `key`, so
    /// a repeated advertisement needs no second disco#info round-trip.
    pub fn has_resource_only(&self, full_jid: &FullJid, key: &CapsCacheKey) -> bool {
        self.resource_only
            .get(full_jid)
            .is_some_and(|entry| entry.value().0 == *key)
    }

    /// Look up the cached identity+features for a (hash, ver) tuple.
    pub fn cached(&self, key: &CapsCacheKey) -> Option<CachedDiscoInfo> {
        self.cache.get(key)
    }

    /// The verified entry for `key`: this process's cache first, then the
    /// store every replica shares, which fills the cache on a hit. An
    /// unsupported hash is never in the store (XEP-0115 section 5.4 step 2),
    /// and a store failure reads as a miss.
    pub async fn shared(
        &self,
        store: &EntityCapsStore,
        key: &CapsCacheKey,
    ) -> Option<CachedDiscoInfo> {
        if let Some(info) = self.cache.get(key) {
            return Some(info);
        }
        if !is_supported_hash(&key.hash) {
            return None;
        }
        match store.get(key).await {
            Ok(Some(info)) => {
                self.cache.insert(key.clone(), info.clone());
                Some(info)
            }
            Ok(None) => None,
            Err(error) => {
                warn!(error = %error, hash = %key.hash, "Failed to read shared entity caps");
                None
            }
        }
    }

    /// Record the verified entry for `key` in the store every replica
    /// shares. Called only after [`CapsVerification::Match`]. A failed
    /// write leaves this replica's cache intact; another replica resolves
    /// the key again.
    pub async fn publish(&self, store: &EntityCapsStore, key: &CapsCacheKey) {
        let Some(info) = self.cache.get(key) else {
            return;
        };
        if let Err(error) = store
            .put(key, &info, chrono::Utc::now().timestamp_millis())
            .await
        {
            warn!(error = %error, hash = %key.hash, "Failed to record shared entity caps");
        }
    }

    /// The identity and features of `full_jid` for notification filtering
    /// on a replica that need not hold its socket.
    ///
    /// The mapping this process recorded is the fast path. Without one, the
    /// key comes from the `<c/>` of the resource's presence, which the
    /// account coordinator replicates, and resolves through the shared
    /// store. A resource whose caps are not verified yet, or that
    /// advertised an unsupported hash (known only to the replica that
    /// queried it), has none.
    pub async fn info_for_presence(
        &self,
        store: &EntityCapsStore,
        full_jid: &FullJid,
        presence_payloads: &[Element],
    ) -> Option<CachedDiscoInfo> {
        if let Some(info) = self.info_for_resource(full_jid) {
            return Some(info);
        }
        let caps = extract_caps_from_payloads(presence_payloads)?;
        self.shared(store, &CapsCacheKey::from_caps(&caps)).await
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
    /// - On an unsupported hash, the result is recorded for the
    ///   resource only (never in the global cache).
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
            // §5.4 step 2: do not validate or globally cache; associate
            // the result only with the generating entity's JID.
            self.resource_to_ver.remove(&pending.full_jid);
            self.resource_only.insert(
                pending.full_jid,
                (
                    CapsCacheKey::from_caps(&pending.caps),
                    CachedDiscoInfo::new(identities, features),
                ),
            );
            return CapsVerification::ResourceOnly;
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
    extract_caps_from_payloads(&presence.payloads)
}

/// Extract a `<c hash node ver/>` from the extension children of a presence.
pub fn extract_caps_from_payloads(payloads: &[Element]) -> Option<Caps> {
    payloads
        .iter()
        .find(|child| child.name() == "c" && child.ns() == waddle_xmpp::xep::xep0115::NS_CAPS)
        .and_then(Caps::from_element)
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
    fn complete_pending_with_unsupported_hash_is_resource_only() {
        // XEP-0115 §5.4 step 2: an unsupported hash is neither validated
        // nor globally cached; the discovered features are associated
        // only with the generating full JID.
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        let other_jid: FullJid = "alice@localhost/r2".parse().expect("jid");
        let caps = Caps {
            hash: "sha-256".to_string(),
            node: "https://example.test/caps".to_string(),
            ver: ver.clone(),
        };
        resolver.begin_pending("iq-unsupp".into(), full_jid.clone(), caps);
        let pending = resolver.take_pending("iq-unsupp").expect("pending");

        let outcome =
            resolver.complete_pending(pending, identities, features.clone(), vec![], false);

        assert_eq!(outcome, CapsVerification::ResourceOnly);
        assert!(resolver
            .cached(&CapsCacheKey::new("sha-256", &ver))
            .is_none());
        assert!(resolver.cached(&CapsCacheKey::new("sha-1", &ver)).is_none());
        assert_eq!(resolver.key_for_resource(&full_jid), None);
        assert_eq!(
            resolver
                .info_for_resource(&full_jid)
                .expect("info")
                .features,
            features
        );
        assert!(resolver.has_resource_only(&full_jid, &CapsCacheKey::new("sha-256", &ver)));
        assert!(resolver.info_for_resource(&other_jid).is_none());
        resolver.drop_resource(&full_jid);
        assert!(resolver.info_for_resource(&full_jid).is_none());
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

    async fn shared_store() -> EntityCapsStore {
        let db = crate::db::Database::in_memory(&format!("caps-shared-{}", uuid::Uuid::new_v4()))
            .await
            .expect("in-memory database");
        crate::db::MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        EntityCapsStore::new(db)
    }

    fn caps_payload(hash: &str, ver: &str) -> Vec<Element> {
        vec![Element::builder("c", waddle_xmpp::xep::xep0115::NS_CAPS)
            .attr(minidom::rxml::xml_ncname!("hash").to_owned(), hash)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "https://example.test/caps",
            )
            .attr(minidom::rxml::xml_ncname!("ver").to_owned(), ver)
            .build()]
    }

    /// Two resolvers over one store are two replicas: the one that holds
    /// the socket verifies and publishes, the other has no mapping for the
    /// resource and learns its caps from the `<c/>` in the replicated
    /// presence.
    #[tokio::test]
    async fn caps_verified_on_one_replica_are_known_to_another() {
        let store = shared_store().await;
        let holder = CapsResolver::default();
        let publisher = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        holder.begin_pending(
            "iq-1".into(),
            full_jid.clone(),
            Caps::new("https://example.test/caps", &ver),
        );
        let pending = holder.take_pending("iq-1").expect("pending entry");
        assert_eq!(
            holder.complete_pending(pending, identities, features, vec![], false),
            CapsVerification::Match
        );
        let key = CapsCacheKey::new("sha-1", &ver);
        let payloads = caps_payload("sha-1", &ver);

        // Not yet shared: the publishing replica knows nothing.
        assert!(publisher
            .info_for_presence(&store, &full_jid, &payloads)
            .await
            .is_none());

        holder.publish(&store, &key).await;

        let info = publisher
            .info_for_presence(&store, &full_jid, &payloads)
            .await
            .expect("the shared entry resolves the resource");
        assert!(info
            .features
            .iter()
            .any(|feature| feature.0 == "http://jabber.org/protocol/disco#info"));
        assert!(
            publisher.cached(&key).is_some(),
            "a store hit fills the replica's own cache"
        );
        assert_eq!(
            publisher.key_for_resource(&full_jid),
            None,
            "the replica records no mapping for a socket it does not hold"
        );
    }

    #[tokio::test]
    async fn presence_without_usable_caps_resolves_nothing() {
        let store = shared_store().await;
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let key = CapsCacheKey::new("sha-1", &ver);
        resolver
            .cache()
            .insert(key.clone(), CachedDiscoInfo::new(identities, features));
        resolver.publish(&store, &key).await;
        let other = CapsResolver::default();
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");

        // No `<c/>`, an unknown ver, and an unsupported hash family.
        assert!(other
            .info_for_presence(&store, &full_jid, &[])
            .await
            .is_none());
        assert!(other
            .info_for_presence(&store, &full_jid, &caps_payload("sha-1", "unknown"))
            .await
            .is_none());
        assert!(other
            .info_for_presence(&store, &full_jid, &caps_payload("sha-256", &ver))
            .await
            .is_none());
    }

    #[test]
    fn extract_caps_from_payloads_finds_the_caps_child() {
        let mut payloads = vec![Element::builder("idle", "urn:xmpp:idle:1").build()];
        assert!(extract_caps_from_payloads(&payloads).is_none());
        payloads.extend(caps_payload("sha-1", "abc"));
        let caps = extract_caps_from_payloads(&payloads).expect("caps");
        assert_eq!((caps.hash.as_str(), caps.ver.as_str()), ("sha-1", "abc"));
    }

    #[test]
    fn server_domain_jid_parses_at_boundary() {
        assert!(ServerDomainJid::parse("localhost").is_ok());
        assert!(ServerDomainJid::parse("waddle.social").is_ok());
        assert!(ServerDomainJid::parse("").is_err());
    }
}
