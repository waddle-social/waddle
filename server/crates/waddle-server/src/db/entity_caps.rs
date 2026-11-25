//! The verified XEP-0115 entity capabilities every replica shares
//! (`entity_caps`, migration 2029).
//!
//! A resource's capabilities are resolved by the replica that holds its
//! socket, but XEP-0163 notification filtering runs on the replica that
//! processed the publish. The table is content-addressed by
//! `(hash, ver)` and only written after the recomputed verification string
//! equals `ver` (XEP-0115 section 5.4), so replicas cannot disagree and a
//! write is idempotent. The in-memory `CapsCache` sits in front of it as a
//! read-through cache; an entry never changes, so it needs no invalidation.
//!
//! Each entry is the disco#info `<query/>` element, serialized and bounded
//! ([`MAX_INFO_BYTES`], [`MAX_FEATURES`], [`MAX_IDENTITIES`]), and the table
//! holds at most [`MAX_ROWS`] entries: a write over the bound deletes the
//! oldest by `verified_at_ms`, so a client that rotates its `ver` cannot
//! grow it without limit.

use minidom::Element;
use thiserror::Error;
use waddle_xmpp::disco::info::{parse_disco_info_response, Identity, DISCO_INFO_NS};
use waddle_xmpp::xep::xep0115::{CachedDiscoInfo, CapsCacheKey};

use super::Database;

/// The longest serialized entry, in bytes.
pub const MAX_INFO_BYTES: usize = 64 * 1024;

/// The most features one entry holds.
pub const MAX_FEATURES: usize = 1024;

/// The most identities one entry holds.
pub const MAX_IDENTITIES: usize = 64;

/// The most entries the table holds before a write prunes the oldest.
pub const MAX_ROWS: i64 = 100_000;

/// The longest `hash` and `ver` the table records, in bytes.
const MAX_HASH_BYTES: usize = 32;
const MAX_VER_BYTES: usize = 256;

/// Why the store could not be used.
#[derive(Debug, Error)]
pub enum EntityCapsStoreError {
    /// The database failed; the outcome of a write is unknown.
    #[error("the entity capabilities store failed: {0}")]
    Storage(String),
}

fn storage(error: impl std::fmt::Display) -> EntityCapsStoreError {
    EntityCapsStoreError::Storage(error.to_string())
}

/// What [`EntityCapsStore::put`] did with an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// The entry is recorded, by this write or an earlier one.
    Recorded,
    /// The entry exceeds a bound and was not recorded.
    OverBound,
}

/// The shared verified capabilities.
#[derive(Clone)]
pub struct EntityCapsStore {
    db: Database,
}

impl EntityCapsStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// The verified entry for `key`, if any replica recorded one.
    pub async fn get(
        &self,
        key: &CapsCacheKey,
    ) -> Result<Option<CachedDiscoInfo>, EntityCapsStoreError> {
        if !key_fits(key) {
            return Ok(None);
        }
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT info FROM entity_caps WHERE hash = ? AND ver = ?",
                crate::db_params![key.hash.as_str(), key.ver.as_str()],
            )
            .await
            .map_err(storage)?;
        let Some(row) = rows.next().await.map_err(storage)? else {
            return Ok(None);
        };
        let info: String = row.get(0).map_err(storage)?;
        Ok(decode(&info))
    }

    /// Record the verified `info` for `key`. An existing entry stays: the
    /// key is content-addressed, so it is the same set.
    pub async fn put(
        &self,
        key: &CapsCacheKey,
        info: &CachedDiscoInfo,
        now_ms: i64,
    ) -> Result<PutOutcome, EntityCapsStoreError> {
        if !key_fits(key) {
            return Ok(PutOutcome::OverBound);
        }
        let Some(encoded) = encode(info) else {
            return Ok(PutOutcome::OverBound);
        };
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "INSERT INTO entity_caps (hash, ver, info, verified_at_ms) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (hash, ver) DO NOTHING",
                crate::db_params![key.hash.as_str(), key.ver.as_str(), encoded, now_ms],
            )
            .await
            .map_err(storage)?;
        let mut rows = guard
            .query("SELECT count(*) FROM entity_caps", ())
            .await
            .map_err(storage)?;
        let held: i64 = match rows.next().await.map_err(storage)? {
            Some(row) => row.get(0).map_err(storage)?,
            None => 0,
        };
        drop(rows);
        if held > MAX_ROWS {
            guard
                .execute(
                    "DELETE FROM entity_caps WHERE (hash, ver) IN \
                     (SELECT hash, ver FROM entity_caps ORDER BY verified_at_ms ASC LIMIT ?)",
                    crate::db_params![held - MAX_ROWS],
                )
                .await
                .map_err(storage)?;
        }
        Ok(PutOutcome::Recorded)
    }
}

fn key_fits(key: &CapsCacheKey) -> bool {
    !key.hash.is_empty()
        && key.hash.len() <= MAX_HASH_BYTES
        && !key.ver.as_str().is_empty()
        && key.ver.as_str().len() <= MAX_VER_BYTES
}

/// The disco#info `<query/>` that carries `info`, serialized, or `None` when
/// the entry exceeds a bound.
fn encode(info: &CachedDiscoInfo) -> Option<String> {
    if info.features.len() > MAX_FEATURES || info.identities.len() > MAX_IDENTITIES {
        return None;
    }
    let mut query = Element::builder("query", DISCO_INFO_NS);
    for identity in &info.identities {
        query = query.append(identity_element(identity));
    }
    for feature in &info.features {
        query = query.append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), &feature.0)
                .build(),
        );
    }
    let mut bytes = Vec::new();
    query.build().write_to(&mut bytes).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_INFO_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn identity_element(identity: &Identity) -> Element {
    let mut element = Element::builder("identity", DISCO_INFO_NS)
        .attr(
            minidom::rxml::xml_ncname!("category").to_owned(),
            &identity.category,
        )
        .attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            &identity.type_,
        );
    if let Some(lang) = &identity.lang {
        element = element.attr_ns(
            minidom::rxml::Namespace::XML,
            minidom::rxml::xml_ncname!("lang").to_owned(),
            lang,
        );
    }
    if let Some(name) = &identity.name {
        element = element.attr(minidom::rxml::xml_ncname!("name").to_owned(), name);
    }
    element.build()
}

/// The entry a stored `<query/>` carries. A row that does not parse as a
/// well-formed disco#info answer is not an entry.
fn decode(info: &str) -> Option<CachedDiscoInfo> {
    let query: Element = info.parse().ok()?;
    let parsed = parse_disco_info_response(&query).ok()?;
    if parsed.ill_formed {
        return None;
    }
    Some(CachedDiscoInfo::new(parsed.identities, parsed.features))
}
