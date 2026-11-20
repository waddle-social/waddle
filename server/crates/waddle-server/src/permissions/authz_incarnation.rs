//! Startup guard that binds a SpiceDB store to one authorization-ID codec.
//!
//! Stored relationships do not record which encoding produced their IDs, and
//! an ID written by an earlier encoding can equal a current wire ID: the legacy
//! channel ID `v2_c2t5` is the `v2_` encoding of `sky`. Serving a store written
//! by another encoding is unsafe in both directions. Legacy grants that no
//! longer match are silently ignored, including negative ones such as a
//! channel `outcast`, and colliding IDs alias different principals.
//!
//! The guard therefore records the codec in a marker relationship,
//! `waddle_authz_codec:incarnation#codec@waddle_authz_codec:v2`, and writes
//! it only into a store that holds no Waddle relationships.
//!
//! Before the built-in schema is written, the marker is read: a store marked
//! for another codec is refused without rewriting its schema. A store whose
//! schema lacks the marker definition, or has no schema yet, has no marker.
//! After the schema is written and before serving:
//!
//! - the marker names this codec: proceed;
//! - the marker definition holds anything else: refuse;
//! - there is no marker: read one relationship of each Waddle definition. If
//!   any exists, read the marker again: another replica may have marked the
//!   empty store and started writing grants since the first read. Proceed if
//!   it now names this codec, otherwise refuse. If the store is empty, create
//!   the marker with preconditions that the marker definition and every
//!   Waddle definition are still empty, so that of several replicas starting
//!   together exactly one writes it and the others re-read it.
//!
//! Marker IDs are fixed ASCII and deliberately bypass the codec, so a later
//! codec can still read which encoding a store holds. Refusals carry the
//! definition name at most, never an identifier.
//!
//! The guard applies to the SpiceDB backend only. The local test backend keeps
//! identifiers unencoded in its own SQL tables.
//!
//! The marker is the store's only record of its encoding: once it exists, a
//! binary built before this guard must not run against the store, and the
//! marker must not be removed by hand. The operator procedure, including the
//! loss of every grant when a store is emptied, is
//! `docs/operations/authz-codec-cutover.md` at the repository root.

use thiserror::Error;

use super::authz_id::AUTHZ_ID_WIRE_PREFIX;
use super::schema::spicedb_definition_names;

/// Schema definition that holds only the codec marker.
pub const AUTHZ_CODEC_MARKER_DEFINITION: &str = "waddle_authz_codec";

/// Resource ID of the codec marker.
pub const AUTHZ_CODEC_MARKER_ID: &str = "incarnation";

/// Relation from the marker resource to the codec version.
pub const AUTHZ_CODEC_MARKER_RELATION: &str = "codec";

/// The codec version this build writes and reads: [`AUTHZ_ID_WIRE_PREFIX`]
/// without its `_` separator.
pub const AUTHZ_CODEC_VERSION: &str = "v2";

// Keep the marker version and the wire prefix from drifting apart.
const _: () = {
    let prefix = AUTHZ_ID_WIRE_PREFIX.as_bytes();
    let version = AUTHZ_CODEC_VERSION.as_bytes();
    assert!(prefix.len() == version.len() + 1);
    assert!(prefix[version.len()] == b'_');
    let mut index = 0;
    while index < version.len() {
        assert!(prefix[index] == version[index]);
        index += 1;
    }
};

/// Upper bound on marker-definition rows read at startup. A well-formed store
/// holds exactly one; reading a few more is enough to see that a store holds
/// something else.
pub const AUTHZ_CODEC_MARKER_READ_LIMIT: u32 = 4;

/// Why the guard refuses to serve a store.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AuthzIncarnationError {
    #[error(
        "authorization store holds `{resource_type}` relationships but no codec marker; \
         it was written by another ID encoding and must be emptied, with every older \
         server stopped, before this server can use it; see \
         docs/operations/authz-codec-cutover.md"
    )]
    UnmarkedStoreNotEmpty { resource_type: &'static str },
    #[error(
        "authorization store is marked for a different or unrecognised ID codec; run the \
         server release that wrote it and do not edit the marker; see \
         docs/operations/authz-codec-cutover.md"
    )]
    IncompatibleMarker,
    #[error(
        "authorization store changed while its codec marker was being written; restart to \
         decide again"
    )]
    Contended,
}

/// One relationship of the marker definition, as read from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecMarkerRow {
    pub resource_id: String,
    pub relation: String,
    pub subject_type: String,
    pub subject_id: String,
    pub subject_relation: Option<String>,
}

impl CodecMarkerRow {
    /// The marker this build writes and accepts.
    pub fn current() -> Self {
        Self {
            resource_id: AUTHZ_CODEC_MARKER_ID.to_string(),
            relation: AUTHZ_CODEC_MARKER_RELATION.to_string(),
            subject_type: AUTHZ_CODEC_MARKER_DEFINITION.to_string(),
            subject_id: AUTHZ_CODEC_VERSION.to_string(),
            subject_relation: None,
        }
    }
}

/// What the marker definition says about a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecMarkerState {
    /// No marker: the store has never been bound to a codec.
    Absent,
    /// Exactly the marker for this codec.
    Current,
    /// Another version, a malformed marker, or extra rows.
    Incompatible,
}

/// Classify the rows of the marker definition.
pub fn classify_codec_markers(rows: &[CodecMarkerRow]) -> CodecMarkerState {
    match rows {
        [] => CodecMarkerState::Absent,
        [row] if *row == CodecMarkerRow::current() => CodecMarkerState::Current,
        _ => CodecMarkerState::Incompatible,
    }
}

/// Every schema definition whose relationships the guard requires to be absent
/// before it marks a store: all of them except the marker definition. Derived
/// from the built-in schema so a new definition is covered automatically.
pub fn guarded_resource_definitions() -> impl Iterator<Item = &'static str> {
    spicedb_definition_names().filter(|name| *name != AUTHZ_CODEC_MARKER_DEFINITION)
}
