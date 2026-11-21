//! The projection marker of a scope lifecycle.
//!
//! Each lifecycle holds at most one marker relationship in SpiceDB:
//!
//! ```text
//! waddle_grant_projection:<lifecycle>#marker@waddle_grant_projection:<stamp>
//! ```
//!
//! The stamp is `<incarnation>_<generation>_<operation>_<digest>`: the
//! authority incarnation, the scope generation, the operation that recorded
//! it and the SHA-256 of the intended projection. Identities are 32 lowercase
//! hex digits, the generation is decimal without leading zeros and the digest
//! is 64 lowercase hex digits, so a marker has exactly one spelling.
//!
//! The marker is written in the same `WriteRelationships` request as the
//! relationships it describes, and that request is conditioned on the exact
//! marker it replaces. Marker identifiers are fixed ASCII and bypass the
//! authorization ID codec, like the codec and schema-version markers.

use super::model::{
    AuthorityIncarnation, GrantGeneration, GrantOperationId, ProjectionDigest, ScopeLifecycleId,
};

/// Schema definition that holds only projection markers.
pub const HOST_GRANT_PROJECTION_DEFINITION: &str = "waddle_grant_projection";

/// Relation from a lifecycle's marker resource to its stamp.
pub const HOST_GRANT_PROJECTION_MARKER_RELATION: &str = "marker";

/// What a lifecycle's projection holds: the operation whose intended
/// projection was last written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionMarker {
    pub lifecycle: ScopeLifecycleId,
    pub incarnation: AuthorityIncarnation,
    pub generation: GrantGeneration,
    pub operation: GrantOperationId,
    pub digest: ProjectionDigest,
}

impl ProjectionMarker {
    /// The marker resource ID: the lifecycle.
    pub fn resource_id(&self) -> String {
        self.lifecycle.to_storage()
    }

    /// The marker subject ID.
    pub fn stamp(&self) -> String {
        format!(
            "{}_{}_{}_{}",
            self.incarnation.to_storage(),
            self.generation.value(),
            self.operation.to_storage(),
            self.digest.as_str()
        )
    }

    /// Parse a stamp read for `lifecycle`. Only the canonical spelling is
    /// accepted, so a marker that parses re-encodes to the same text.
    pub fn parse(lifecycle: ScopeLifecycleId, stamp: &str) -> Option<Self> {
        let mut parts = stamp.split('_');
        let incarnation = AuthorityIncarnation::from_storage(parts.next()?)?;
        let generation_text = parts.next()?;
        let generation = GrantGeneration::new(generation_text.parse::<i64>().ok()?)?;
        let operation = GrantOperationId::from_storage(parts.next()?)?;
        let digest = ProjectionDigest::from_storage(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let marker = Self {
            lifecycle,
            incarnation,
            generation,
            operation,
            digest,
        };
        (marker.stamp() == stamp).then_some(marker)
    }
}
