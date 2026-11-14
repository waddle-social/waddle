//! Startup fence that binds a SpiceDB store to one version of the built-in
//! schema.
//!
//! Every replica used to write its built-in schema at startup, so a replica of
//! an older release could revert a newer schema, for example outcast
//! precedence on channel permissions. The schema is therefore versioned, and a
//! replica writes it only to install it into an unversioned store or to
//! upgrade a store recorded at an older version. A store recorded at a newer
//! version, or holding an unrecognised version record, is refused before
//! anything is written.
//!
//! The version is recorded in a marker relationship,
//! `waddle_authz_schema:installed#version_N@waddle_authz_schema:installed`,
//! whose relation name carries the version. The built-in schema of version N
//! declares only the relation `version_N` in `waddle_authz_schema`. SpiceDB
//! refuses a schema write that would remove a relation or definition that
//! still holds relationships, so once the marker exists:
//!
//! - no binary whose schema lacks `version_N` can overwrite the schema, even
//!   one that predates this fence or ignores the marker: binaries before
//!   versioning lack the whole `waddle_authz_schema` definition, and other
//!   versions lack the relation;
//! - the marker can only be created against a stored schema that declares
//!   its relation, so it cannot claim a version that the stored schema is
//!   not.
//!
//! Relationships are read fully consistently from the datastore, while
//! `ReadSchema` can serve a cached schema after a direct datastore reset; the
//! version is therefore never decided by comparing schema text.
//!
//! The startup order is:
//!
//! 1. Before the schema is written (`EnsureSchema`, after the codec marker
//!    pre-check of `permissions::authz_incarnation`), read the marker:
//!    - this version: proceed without writing the schema;
//!    - none: write the built-in schema;
//!    - an older version M: write the built-in schema that also keeps
//!      `version_M`, so the stored marker stays valid until it is replaced;
//!    - a newer version, or anything else: refuse without writing.
//! 2. The codec guard binds or refuses the store (`EnsureAuthzCodecIncarnation`).
//! 3. Record the version (`RecordAuthzSchemaVersion`): read the marker again;
//!    create it if there is none, with a precondition that the marker
//!    definition is still empty; replace an older marker in one write with a
//!    precondition that it still exists, then write the built-in schema again
//!    to drop the older relation. If the precondition fails, decide again from
//!    what is stored. If that last schema write fails, the older relation
//!    stays declared until the next upgrade; it holds nothing and no
//!    permission refers to it.
//!
//! The marker is recorded only after the codec guard accepts the store, so a
//! store the codec guard refuses gains no relationship and the release that
//! wrote it can still start against it.
//!
//! Marker IDs are fixed ASCII and bypass the authorization ID codec, and the
//! marker definition is not one of the definitions the codec guard requires
//! to be empty. Refusals name versions only, never an identifier.
//!
//! The guard applies to the SpiceDB backend only; the local test backend has
//! no stored schema. The operator procedure, including mixed-version replicas
//! and the retirement of old binaries, is
//! `docs/operations/authz-schema-versions.md` at the repository root.

use thiserror::Error;

use super::schema::SPICEDB_SCHEMA;

/// Schema definition that holds only the schema-version marker.
pub const AUTHZ_SCHEMA_MARKER_DEFINITION: &str = "waddle_authz_schema";

/// Resource and subject ID of the schema-version marker.
pub const AUTHZ_SCHEMA_MARKER_ID: &str = "installed";

/// The version of the built-in schema this build writes and serves.
///
/// Version 1 is the first versioned schema; it adds outcast precedence to
/// every channel permission. Stores without a marker are unversioned and are
/// upgraded.
pub const AUTHZ_SCHEMA_VERSION: u32 = 1;

/// Prefix of the marker relation; the version follows in decimal, without
/// leading zeros. SpiceDB relation names need at least three characters, so
/// the version cannot be a bare `vN`.
pub const AUTHZ_SCHEMA_VERSION_RELATION_PREFIX: &str = "version_";

/// Upper bound on marker-definition rows read at startup. A well-formed store
/// holds at most one; reading a few more is enough to see that a store holds
/// something else.
pub const AUTHZ_SCHEMA_MARKER_READ_LIMIT: u32 = 4;

/// The built-in schema text of [`AUTHZ_SCHEMA_VERSION`].
pub fn builtin_authz_schema() -> &'static str {
    SPICEDB_SCHEMA
}

/// The marker relation that records `version`.
pub fn authz_schema_version_relation(version: u32) -> String {
    format!("{AUTHZ_SCHEMA_VERSION_RELATION_PREFIX}{version}")
}

/// The version a marker relation records, if it is exactly the canonical
/// spelling of a version from 1 upwards.
pub fn parse_authz_schema_version_relation(relation: &str) -> Option<u32> {
    let digits = relation.strip_prefix(AUTHZ_SCHEMA_VERSION_RELATION_PREFIX)?;
    let version = digits.parse::<u32>().ok()?;
    (version > 0 && version.to_string() == digits).then_some(version)
}

/// Why the guard refuses to serve a store or to record its version.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AuthzSchemaVersionError {
    #[error(
        "authorization store schema is version {stored}, newer than version {supported} of \
         this server; it was written by a later release and this server does not overwrite \
         it; retire this binary; see docs/operations/authz-schema-versions.md"
    )]
    Newer { stored: u32, supported: u32 },
    #[error(
        "authorization store holds an unrecognised schema-version marker; this server does \
         not overwrite its schema; do not edit the marker; see \
         docs/operations/authz-schema-versions.md"
    )]
    UnrecognisedMarker,
    #[error(
        "authorization store schema version changed while it was being recorded; restart to \
         decide again"
    )]
    Contended,
    #[error("the built-in authorization schema does not declare its own version relation")]
    BuiltinSchemaUnversioned,
}

/// One relationship of the marker definition, as read from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaMarkerRow {
    pub resource_id: String,
    pub relation: String,
    pub subject_type: String,
    pub subject_id: String,
    pub subject_relation: Option<String>,
}

impl SchemaMarkerRow {
    /// The marker that records `version`.
    pub fn for_version(version: u32) -> Self {
        Self {
            resource_id: AUTHZ_SCHEMA_MARKER_ID.to_string(),
            relation: authz_schema_version_relation(version),
            subject_type: AUTHZ_SCHEMA_MARKER_DEFINITION.to_string(),
            subject_id: AUTHZ_SCHEMA_MARKER_ID.to_string(),
            subject_relation: None,
        }
    }
}

/// What the marker definition says about a store, relative to the version a
/// server supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaMarkerState {
    /// No marker: the store is unversioned.
    Absent,
    /// Exactly the marker of the supported version.
    Current,
    /// Exactly the marker of an older version.
    Older(u32),
    /// Exactly the marker of a newer version.
    Newer(u32),
    /// A malformed marker, or more than one row.
    Unrecognised,
}

impl SchemaMarkerState {
    /// The refusal for a state this server must not write over, if any.
    pub fn refusal(self, supported: u32) -> Option<AuthzSchemaVersionError> {
        match self {
            Self::Newer(stored) => Some(AuthzSchemaVersionError::Newer { stored, supported }),
            Self::Unrecognised => Some(AuthzSchemaVersionError::UnrecognisedMarker),
            Self::Absent | Self::Current | Self::Older(_) => None,
        }
    }
}

/// Classify the rows of the marker definition for a server that supports
/// schema version `supported`.
pub fn classify_schema_markers(rows: &[SchemaMarkerRow], supported: u32) -> SchemaMarkerState {
    let row = match rows {
        [] => return SchemaMarkerState::Absent,
        [row] => row,
        _ => return SchemaMarkerState::Unrecognised,
    };
    let Some(version) = parse_authz_schema_version_relation(&row.relation) else {
        return SchemaMarkerState::Unrecognised;
    };
    if *row != SchemaMarkerRow::for_version(version) {
        return SchemaMarkerState::Unrecognised;
    }
    match version.cmp(&supported) {
        std::cmp::Ordering::Equal => SchemaMarkerState::Current,
        std::cmp::Ordering::Less => SchemaMarkerState::Older(version),
        std::cmp::Ordering::Greater => SchemaMarkerState::Newer(version),
    }
}

/// `schema`, which declares the marker relation of `current`, with the marker
/// relation of `retained` declared beside it. Written while a store recorded
/// at `retained` is upgraded, so that its marker stays valid until it is
/// replaced. `None` if `schema` does not declare `current` exactly once, or if
/// the two versions are equal.
pub fn authz_schema_retaining_version(schema: &str, current: u32, retained: u32) -> Option<String> {
    if current == retained {
        return None;
    }
    let declaration = |version: u32| {
        format!(
            "relation {}: {AUTHZ_SCHEMA_MARKER_DEFINITION}",
            authz_schema_version_relation(version)
        )
    };
    let current_declaration = declaration(current);
    let retained_declaration = declaration(retained);
    let mut text = String::with_capacity(schema.len() + retained_declaration.len() + 4);
    let mut found = 0usize;
    for line in schema.split_inclusive('\n') {
        if line.trim() == current_declaration {
            found += 1;
            let indent = &line[..line.len() - line.trim_start().len()];
            text.push_str(indent);
            text.push_str(&retained_declaration);
            text.push('\n');
        }
        text.push_str(line);
    }
    (found == 1).then_some(text)
}
