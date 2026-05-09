//! XEP-0115 entity-capabilities resolution for inbound presence.
//!
//! When a connected resource advertises `<c hash node ver/>` on a
//! presence stanza, the server either:
//!
//! - **Cache hit:** records `(full_jid → ver)` so XEP-0163 §3 fan-out
//!   (PR 2) can filter notifications by per-resource feature lists.
//! - **Cache miss:** sends a typed `disco#info` IQ get to the resource
//!   with `node="<NODE>#<VER>"` (XEP-0115 §6.2). The reply is verified
//!   per §5.4 — the recipient MUST recompute the verification string
//!   from the disco#info response and only cache when the recomputed
//!   hash matches the advertised `ver`.
//!
//! On disconnect, the per-resource mapping is dropped while the
//! hash-keyed `CapsCache` itself stays warm so the next session can
//! reuse cross-session knowledge (XEP-0115 §6).

use std::str::FromStr;
use std::sync::Arc;

use dashmap::DashMap;
use jid::{FullJid, Jid};
use waddle_xmpp::disco::info::{Feature, Identity};
use waddle_xmpp::xep::xep0115::{compute_caps_hash, CachedDiscoInfo, Caps, CapsCache};
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::minidom::Element;

const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

/// Outcome of recomputing and verifying a disco#info reply against an
/// advertised `ver` per XEP-0115 §5.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsVerification {
    /// Recomputed hash matched `ver`; the result MUST be cached and
    /// the resource→ver mapping recorded.
    Match,
    /// Recomputed hash did not match `ver`; the result MUST NOT be
    /// cached (poisoning defense).
    Mismatch,
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
    resource_to_ver: Arc<DashMap<String, String>>,
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

    /// Record that `full_jid` is advertising `ver`. Used both on a
    /// cache hit and after a successful verification.
    pub fn record_resource(&self, full_jid: &FullJid, ver: &str) {
        self.resource_to_ver
            .insert(full_jid.to_string(), ver.to_string());
    }

    /// Drop the per-resource mapping. Called on resource disconnect.
    /// Leaves the hash-keyed cache warm.
    pub fn drop_resource(&self, full_jid: &FullJid) {
        self.resource_to_ver.remove(&full_jid.to_string());
    }

    /// Return the ver advertised by `full_jid`, if any.
    pub fn ver_for_resource(&self, full_jid: &FullJid) -> Option<String> {
        self.resource_to_ver
            .get(&full_jid.to_string())
            .map(|v| v.value().clone())
    }

    /// Look up the cached identity+features for a ver.
    pub fn cached(&self, ver: &str) -> Option<CachedDiscoInfo> {
        self.cache.get(ver)
    }

    /// Begin tracking a pending disco#info resolution for `caps`
    /// keyed by `iq_id`. Used for cache misses where the server
    /// just sent the disco#info IQ get.
    pub fn begin_pending(&self, iq_id: String, full_jid: FullJid, caps: Caps) {
        self.pending
            .insert(iq_id, PendingCapsResolution { full_jid, caps });
    }

    /// Take a pending entry by IQ id. Returns `None` if no resolution
    /// is in flight under that id (a stray result; ignore per §5.4).
    pub fn take_pending(&self, iq_id: &str) -> Option<PendingCapsResolution> {
        self.pending.remove(iq_id).map(|(_, v)| v)
    }

    /// Verify a disco#info reply for a previously-pending resolution
    /// per XEP-0115 §5.4. On match, the (hash, ver) is cached and the
    /// resource→ver mapping recorded. On mismatch, neither side
    /// effect occurs.
    pub fn complete_pending(
        &self,
        pending: PendingCapsResolution,
        identities: Vec<Identity>,
        features: Vec<Feature>,
    ) -> CapsVerification {
        let recomputed = compute_caps_hash(&identities, &features);
        if recomputed != pending.caps.ver {
            return CapsVerification::Mismatch;
        }
        self.cache.insert(
            &pending.caps.ver,
            CachedDiscoInfo::new(identities, features),
        );
        self.record_resource(&pending.full_jid, &pending.caps.ver);
        CapsVerification::Match
    }
}

/// Build a typed disco#info IQ get for caps resolution.
/// Per XEP-0115 §6.2 the query MUST carry `node="<NODE>#<VER>"`.
pub fn build_caps_disco_info_query(
    server_domain: &str,
    target: &FullJid,
    caps: &Caps,
    iq_id: &str,
) -> Result<Iq, String> {
    let from = Jid::from_str(server_domain)
        .map_err(|e| format!("server domain is not a valid JID: {e}"))?;
    let query = Element::builder("query", NS_DISCO_INFO)
        .attr("node", caps.node_ver())
        .build();
    Ok(Iq {
        from: Some(from),
        to: Some(Jid::from(target.clone())),
        id: iq_id.to_string(),
        payload: IqType::Get(query),
    })
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

    fn sample_identities() -> Vec<Identity> {
        vec![Identity::server(Some("Waddle Test"))]
    }

    fn sample_features() -> Vec<Feature> {
        vec![Feature::disco_info(), Feature::disco_items()]
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

        let outcome = resolver.complete_pending(pending, identities.clone(), features.clone());

        assert_eq!(outcome, CapsVerification::Match);
        assert!(resolver.cached(&ver).is_some());
        assert_eq!(resolver.ver_for_resource(&full_jid), Some(ver));
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

        let outcome = resolver.complete_pending(pending, identities, features);

        assert_eq!(outcome, CapsVerification::Mismatch);
        assert!(resolver.cached(advertised).is_none());
        assert_eq!(resolver.ver_for_resource(&full_jid), None);
    }

    #[test]
    fn drop_resource_clears_mapping_but_not_hash_cache() {
        let resolver = CapsResolver::default();
        let identities = sample_identities();
        let features = sample_features();
        let ver = compute_caps_hash(&identities, &features);
        let full_jid: FullJid = "alice@localhost/r1".parse().expect("jid");
        resolver
            .cache()
            .insert(&ver, CachedDiscoInfo::new(identities, features));
        resolver.record_resource(&full_jid, &ver);

        resolver.drop_resource(&full_jid);

        assert_eq!(resolver.ver_for_resource(&full_jid), None);
        assert!(resolver.cached(&ver).is_some());
    }
}
