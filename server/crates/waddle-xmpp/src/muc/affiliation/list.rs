use std::collections::HashMap;

use chrono::{DateTime, Utc};
use jid::BareJid;

use crate::types::Affiliation;

/// An entry in the affiliation list.
#[derive(Debug, Clone)]
pub struct AffiliationEntry {
    /// The user's JID (bare)
    pub jid: BareJid,
    /// The user's affiliation
    pub affiliation: Affiliation,
    /// When this affiliation was granted, if recorded.
    ///
    /// Today's in-memory `AffiliationList` does not record grant
    /// timestamps, so this field is always `None`. Persistence of
    /// `granted_at` is a follow-up tracked under the admin V2
    /// spaces-metadata plumbing (see
    /// `docs/superpowers/specs/2026-05-17-admin-v2-design.md`).
    pub granted_at: Option<DateTime<Utc>>,
    /// Optional reason/notes
    pub reason: Option<String>,
}

impl AffiliationEntry {
    /// Create a new affiliation entry.
    pub fn new(jid: BareJid, affiliation: Affiliation) -> Self {
        Self {
            jid,
            affiliation,
            granted_at: None,
            reason: None,
        }
    }
}

/// Result of an affiliation change operation.
#[derive(Debug, Clone)]
pub struct AffiliationChange {
    /// The user whose affiliation changed
    pub jid: BareJid,
    /// Previous affiliation
    pub old_affiliation: Affiliation,
    /// New affiliation
    pub new_affiliation: Affiliation,
    /// Reason for the change
    pub reason: Option<String>,
}

impl AffiliationChange {
    /// Create a new affiliation change record.
    pub fn new(jid: BareJid, old: Affiliation, new: Affiliation) -> Self {
        Self {
            jid,
            old_affiliation: old,
            new_affiliation: new,
            reason: None,
        }
    }

    /// Check if this is an upgrade (higher privilege).
    pub fn is_upgrade(&self) -> bool {
        self.new_affiliation > self.old_affiliation
    }
}

/// How a stored affiliation entry came to exist (#1110).
///
/// The distinction drives room dormancy: resolver-derived entries are
/// re-derived from the authz resolver on the next join by construction,
/// so dropping them with an evicted room actor loses nothing. Explicit
/// grants (admin IQ writes, bans, the instant-room creator's Owner) are
/// in-memory only and MUST keep blocking dormancy or they would silently
/// evaporate on eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffiliationProvenance {
    /// Written by the join-path authz resolver; reconstructible on the
    /// next join of the same bare JID.
    ResolverDerived,
    /// Written by an explicit grant (XEP-0045 §10.10 admin/owner IQ,
    /// §9.1 ban, instant-room creator Owner per §10.1.1). In-memory
    /// only today, so it pins the room actor in memory.
    ExplicitGrant,
}

/// A stored affiliation value plus its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredAffiliation {
    affiliation: Affiliation,
    provenance: AffiliationProvenance,
}

/// Affiliation list for a MUC room.
///
/// Stores affiliations persistently and supports sync with Zanzibar.
#[derive(Debug, Clone, Default)]
pub struct AffiliationList {
    /// Affiliations by bare JID
    affiliations: HashMap<BareJid, StoredAffiliation>,
}

impl AffiliationList {
    /// Create an empty affiliation list.
    pub fn new() -> Self {
        Self {
            affiliations: HashMap::new(),
        }
    }

    /// Get the affiliation for a JID.
    pub fn get(&self, jid: &BareJid) -> Affiliation {
        self.affiliations
            .get(jid)
            .map(|stored| stored.affiliation)
            .unwrap_or(Affiliation::None)
    }

    /// Set the affiliation for a JID as an explicit grant.
    ///
    /// Returns the previous affiliation if it changed.
    pub fn set(&mut self, jid: BareJid, affiliation: Affiliation) -> Option<AffiliationChange> {
        self.set_with_provenance(jid, affiliation, AffiliationProvenance::ExplicitGrant)
    }

    /// Set the affiliation for a JID, recording its provenance (#1110).
    ///
    /// Provenance transitions are asymmetric on a same-value write:
    /// an explicit grant always stamps `ExplicitGrant` (an admin
    /// re-affirming a resolver-derived tier upgrades it to a durable
    /// in-memory grant), while a resolver write never downgrades an
    /// existing `ExplicitGrant` — dormancy must fail toward keeping
    /// the room when in doubt.
    pub fn set_with_provenance(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
        provenance: AffiliationProvenance,
    ) -> Option<AffiliationChange> {
        // A resolver-derived write must never replace an explicit
        // grant, whatever the values: bans (Outcast) and admin-set
        // tiers are memory-only and would otherwise be silently
        // overwritten on the next join (issue #1110 follow-up — a
        // resolver Member write must not lift an explicit ban).
        if provenance == AffiliationProvenance::ResolverDerived
            && self
                .affiliations
                .get(&jid)
                .is_some_and(|stored| stored.provenance == AffiliationProvenance::ExplicitGrant)
        {
            return None;
        }
        let old = self.get(&jid);
        if old != affiliation {
            if affiliation == Affiliation::None {
                self.affiliations.remove(&jid);
            } else {
                self.affiliations.insert(
                    jid.clone(),
                    StoredAffiliation {
                        affiliation,
                        provenance,
                    },
                );
            }
            Some(AffiliationChange::new(jid, old, affiliation))
        } else {
            if provenance == AffiliationProvenance::ExplicitGrant {
                if let Some(stored) = self.affiliations.get_mut(&jid) {
                    stored.provenance = AffiliationProvenance::ExplicitGrant;
                }
            }
            None
        }
    }

    /// Remove a JID from the affiliation list.
    pub fn remove(&mut self, jid: &BareJid) -> Option<AffiliationChange> {
        self.affiliations
            .remove(jid)
            .map(|old| AffiliationChange::new(jid.clone(), old.affiliation, Affiliation::None))
    }

    /// Get all JIDs with a specific affiliation.
    pub fn by_affiliation(&self, affiliation: Affiliation) -> Vec<BareJid> {
        self.affiliations
            .iter()
            .filter(|(_, stored)| stored.affiliation == affiliation)
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// True when no non-default affiliations are recorded.
    pub fn is_empty(&self) -> bool {
        self.affiliations.is_empty()
    }

    /// True when at least one entry is an explicit grant (#1110). Used
    /// by `MucRoom::is_dormant` to decide whether an empty room is safe
    /// to evict from the in-memory registry: explicit grants are
    /// in-memory only, so eviction would silently drop them, while
    /// resolver-derived entries are re-derived on the next join and are
    /// safe to drop with the actor.
    pub fn has_explicit_grants(&self) -> bool {
        self.affiliations
            .values()
            .any(|stored| stored.provenance == AffiliationProvenance::ExplicitGrant)
    }

    /// Get all affiliation entries.
    pub fn all(&self) -> Vec<AffiliationEntry> {
        self.affiliations
            .iter()
            .map(|(jid, stored)| AffiliationEntry::new(jid.clone(), stored.affiliation))
            .collect()
    }

    /// Get the count of affiliations at each level.
    pub fn counts(&self) -> HashMap<Affiliation, usize> {
        let mut counts = HashMap::new();
        for stored in self.affiliations.values() {
            *counts.entry(stored.affiliation).or_insert(0) += 1;
        }
        counts
    }

    /// Check if a JID has at least the specified affiliation.
    pub fn has_at_least(&self, jid: &BareJid, min_affiliation: Affiliation) -> bool {
        self.get(jid) >= min_affiliation
    }

    /// Check if the list contains any owners.
    pub fn has_owner(&self) -> bool {
        self.affiliations
            .values()
            .any(|stored| stored.affiliation == Affiliation::Owner)
    }
}
