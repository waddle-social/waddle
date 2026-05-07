use std::collections::HashMap;

use crate::types::Affiliation;

/// Maps Waddle permission relations to MUC affiliations.
///
/// The mapping follows RFC-0002 specification for permission hierarchy.
#[derive(Debug, Clone)]
pub struct PermissionMapper {
    /// Custom mapping overrides (relation -> affiliation)
    custom_mappings: HashMap<String, Affiliation>,
}

impl Default for PermissionMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionMapper {
    /// Create a new permission mapper with default mappings.
    pub fn new() -> Self {
        Self {
            custom_mappings: HashMap::new(),
        }
    }

    /// Add a custom mapping override.
    #[cfg(test)]
    pub fn with_mapping(mut self, relation: &str, affiliation: Affiliation) -> Self {
        self.custom_mappings
            .insert(relation.to_string(), affiliation);
        self
    }

    /// Map a Waddle permission relation to MUC affiliation.
    ///
    /// Returns the highest affiliation if multiple relations are present.
    pub fn map_relation(&self, relation: &str) -> Affiliation {
        if let Some(affiliation) = self.custom_mappings.get(relation) {
            return *affiliation;
        }

        match relation {
            "owner" => Affiliation::Owner,
            "admin" => Affiliation::Admin,
            "moderator" => Affiliation::Admin,
            "manager" => Affiliation::Admin,
            "member" => Affiliation::Member,
            "writer" => Affiliation::Member,
            "viewer" => Affiliation::Member,
            _ => Affiliation::None,
        }
    }

    /// Map multiple relations to the highest affiliation.
    ///
    /// When a user has multiple relations (e.g., both member and admin),
    /// the highest privilege wins.
    pub fn map_relations(&self, relations: &[String]) -> Affiliation {
        relations
            .iter()
            .map(|r| self.map_relation(r))
            .max()
            .unwrap_or(Affiliation::None)
    }
}
