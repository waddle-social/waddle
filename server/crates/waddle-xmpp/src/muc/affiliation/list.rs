use std::collections::HashMap;

use jid::BareJid;

use crate::types::Affiliation;

/// An entry in the affiliation list.
#[derive(Debug, Clone)]
pub struct AffiliationEntry {
    /// The user's JID (bare)
    pub jid: BareJid,
    /// The user's affiliation
    pub affiliation: Affiliation,
    /// Optional reason/notes
    pub reason: Option<String>,
}

impl AffiliationEntry {
    /// Create a new affiliation entry.
    pub fn new(jid: BareJid, affiliation: Affiliation) -> Self {
        Self {
            jid,
            affiliation,
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

/// Affiliation list for a MUC room.
///
/// Stores affiliations persistently and supports sync with Zanzibar.
#[derive(Debug, Clone, Default)]
pub struct AffiliationList {
    /// Affiliations by bare JID
    affiliations: HashMap<BareJid, Affiliation>,
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
            .copied()
            .unwrap_or(Affiliation::None)
    }

    /// Set the affiliation for a JID.
    ///
    /// Returns the previous affiliation if it changed.
    pub fn set(&mut self, jid: BareJid, affiliation: Affiliation) -> Option<AffiliationChange> {
        let old = self.get(&jid);
        if old != affiliation {
            if affiliation == Affiliation::None {
                self.affiliations.remove(&jid);
            } else {
                self.affiliations.insert(jid.clone(), affiliation);
            }
            Some(AffiliationChange::new(jid, old, affiliation))
        } else {
            None
        }
    }

    /// Remove a JID from the affiliation list.
    pub fn remove(&mut self, jid: &BareJid) -> Option<AffiliationChange> {
        self.affiliations
            .remove(jid)
            .map(|old| AffiliationChange::new(jid.clone(), old, Affiliation::None))
    }

    /// Get all JIDs with a specific affiliation.
    pub fn by_affiliation(&self, affiliation: Affiliation) -> Vec<BareJid> {
        self.affiliations
            .iter()
            .filter(|(_, &a)| a == affiliation)
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// Get all affiliation entries.
    pub fn all(&self) -> Vec<AffiliationEntry> {
        self.affiliations
            .iter()
            .map(|(jid, &affiliation)| AffiliationEntry::new(jid.clone(), affiliation))
            .collect()
    }

    /// Get the count of affiliations at each level.
    pub fn counts(&self) -> HashMap<Affiliation, usize> {
        let mut counts = HashMap::new();
        for affiliation in self.affiliations.values() {
            *counts.entry(*affiliation).or_insert(0) += 1;
        }
        counts
    }

    /// Check if a JID has at least the specified affiliation.
    pub fn has_at_least(&self, jid: &BareJid, min_affiliation: Affiliation) -> bool {
        self.get(jid) >= min_affiliation
    }

    /// Check if the list contains any owners.
    pub fn has_owner(&self) -> bool {
        self.affiliations.values().any(|&a| a == Affiliation::Owner)
    }
}
