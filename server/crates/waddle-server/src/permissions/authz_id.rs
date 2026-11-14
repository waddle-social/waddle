//! Versioned, injective encoding of authorization identifiers for SpiceDB.
//!
//! SpiceDB object IDs accept only `[a-zA-Z0-9/_|\-=+]`, at most 1024 bytes
//! (`ObjectReference.object_id` in the Authzed v1 API). Permission objects and
//! subjects carry arbitrary UTF-8 source identifiers such as bare JIDs, room
//! JIDs and channel or Space names, so every ID crosses the SpiceDB boundary
//! through this codec and nothing else. `SpiceDbPermissionBackend` is the only
//! production SpiceDB client; it encodes every resource ID, subject ID and
//! filter it sends and decodes every relationship and lookup result it reads.
//!
//! - Wire form: the [`AUTHZ_ID_WIRE_PREFIX`] version prefix `v2_`, followed by
//!   canonical unpadded base64url of the exact source UTF-8 bytes. The mapping
//!   is injective on source bytes and its output always fits the SpiceDB
//!   alphabet.
//! - Unicode: source bytes are preserved exactly. The codec applies no case
//!   folding, Unicode normalization, IDNA mapping or delimiter splitting;
//!   `e\u{301}` and `é` are different identifiers. User subjects must
//!   additionally be bare account JIDs that the `jid` parser leaves
//!   byte-for-byte unchanged (see [`AuthzUserJid`]), so a spelling that the
//!   parser would rewrite is rejected rather than silently aliased. That
//!   acceptance set is defined by the `jid` parser, so the workspace pins it
//!   to an exact version: changing the parser is a codec version change.
//! - Length: at most [`MAX_AUTHZ_ID_BYTES`] source bytes, which encode to at
//!   most 1023 wire bytes. Longer identifiers, including some valid JIDs, are
//!   unsupported and rejected.
//! - Decoding accepts only the canonical `v2_` form. Unprefixed (legacy) IDs,
//!   other versions, padding, non-zero trailing bits, invalid UTF-8 and
//!   oversized input fail closed. There is no legacy fallback: an identity is
//!   never inferred from a legacy ID.
//! - Every wire ID begins with `v2_`, so a source ID of `*` can never become
//!   SpiceDB's subject wildcard.
//! - Injectivity holds on the wire and carries through to stored
//!   relationships only because the datastore compares object IDs byte for
//!   byte. SpiceDB's PostgreSQL datastore keeps them in character columns
//!   compared under PostgreSQL's deterministic collation, where equality is
//!   byte equality; a datastore that compared IDs case-insensitively would
//!   alias base64url payloads that differ only in letter case.
//!
//! The codec separates encoded identifiers from each other but cannot separate
//! them from relationships written by an earlier encoding: any legacy ID that
//! happens to start with `v2_` is indistinguishable from a new one (the legacy
//! channel ID `v2_c2t5` is the wire ID of `sky`). A store is therefore bound
//! to this encoding by the startup guard in `authz_incarnation`, which marks
//! only a store proven empty of Waddle relationships and refuses any other.
//! Existing relationships are never decoded to reconstruct grants.
//!
//! Canonical encoding establishes uniqueness over accepted source spellings,
//! not over logical principals: `a@example.org` and `a@example.org.` are both
//! accepted and remain different subjects. Principal canonicalization belongs
//! to the account directory and identity boundary, not to this codec.
//!
//! Errors carry no identifier material. Identifiers are not credentials, but
//! they are personal data and must not be used as metric attributes.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jid::{BareJid, DomainPart, NodePart};
use thiserror::Error;

use super::tuple::{Object, ObjectType, Subject, SubjectType};

/// Version prefix of every encoded authorization ID.
pub const AUTHZ_ID_WIRE_PREFIX: &str = "v2_";

/// SpiceDB's object-ID limit, in bytes.
pub const MAX_AUTHZ_WIRE_BYTES: usize = 1024;

/// Largest source identifier, in UTF-8 bytes, whose encoding fits the SpiceDB
/// limit. 765 bytes encode to 1023 wire bytes; 766 bytes would need 1025.
pub const MAX_AUTHZ_ID_BYTES: usize = 765;

/// Why an identifier cannot cross the SpiceDB boundary.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AuthzIdError {
    #[error("authorization identifier must not be empty")]
    Empty,
    #[error("authorization identifier exceeds the codec byte limit")]
    TooLong,
    #[error("encoded authorization identifier has a missing or unsupported version")]
    UnsupportedVersion,
    #[error("encoded authorization identifier is not canonical unpadded base64url")]
    InvalidEncoding,
    #[error("decoded authorization identifier is not UTF-8")]
    InvalidUtf8,
    #[error("user subject must be a bare JID with a localpart")]
    InvalidUserJid,
    #[error("user subject changes when parsed as a JID")]
    UserJidChangedByParser,
}

/// A bare account JID used as a `user` subject.
///
/// The text must parse as a bare JID with a localpart, without a resource,
/// and survive parsing unchanged. This establishes syntax only; it neither
/// resolves an account nor authenticates anyone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthzUserJid(BareJid);

impl AuthzUserJid {
    /// Accept an already parsed bare JID.
    pub fn new(jid: BareJid) -> Result<Self, AuthzIdError> {
        validate_source(jid.as_str())?;
        if jid.node().is_none() {
            return Err(AuthzIdError::InvalidUserJid);
        }
        Ok(Self(jid))
    }

    /// Parse text that the `jid` parser must leave byte-for-byte unchanged.
    pub fn parse_exact(value: &str) -> Result<Self, AuthzIdError> {
        validate_source(value)?;
        let jid = BareJid::new(value).map_err(|_| AuthzIdError::InvalidUserJid)?;
        if jid.as_str() != value {
            return Err(AuthzIdError::UserJidChangedByParser);
        }
        Self::new(jid)
    }

    /// Build the subject of a local account from its stored username and
    /// domain. Each part must already be in the form the `jid` parser
    /// produces, so a stored username that the parser would rewrite (for
    /// example one with upper-case letters) is rejected instead of naming a
    /// different account.
    pub fn from_account_parts(username: &str, domain: &str) -> Result<Self, AuthzIdError> {
        let node = NodePart::new(username).map_err(|_| AuthzIdError::InvalidUserJid)?;
        let host = DomainPart::new(domain).map_err(|_| AuthzIdError::InvalidUserJid)?;
        if node.as_str() != username || host.as_str() != domain {
            return Err(AuthzIdError::UserJidChangedByParser);
        }
        // Re-parse so the result is exactly what `from_wire` accepts back.
        Self::parse_exact(BareJid::from_parts(Some(&node), &host).as_str())
    }

    /// Decode a SpiceDB subject ID, with no legacy fallback.
    pub fn from_wire(wire: &str) -> Result<Self, AuthzIdError> {
        Self::parse_exact(&decode_wire(wire)?)
    }

    pub fn jid(&self) -> &BareJid {
        &self.0
    }

    pub fn to_subject_wire(&self) -> EncodedSubjectId {
        EncodedSubjectId(encode_wire(self.0.as_str()))
    }
}

/// A nonempty opaque identifier: object IDs and non-user subject IDs.
///
/// The exact UTF-8 bytes are preserved, including case, composition,
/// whitespace and characters such as `@`, `.`, `|`, `=`, `#` and `:`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthzOpaqueId(String);

impl AuthzOpaqueId {
    pub fn new(value: impl Into<String>) -> Result<Self, AuthzIdError> {
        let value = value.into();
        validate_source(&value)?;
        Ok(Self(value))
    }

    /// Decode a SpiceDB object or subject ID, with no legacy fallback.
    pub fn from_wire(wire: &str) -> Result<Self, AuthzIdError> {
        Self::new(decode_wire(wire)?)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn to_object_wire(&self) -> EncodedObjectId {
        EncodedObjectId(encode_wire(&self.0))
    }

    pub fn to_subject_wire(&self) -> EncodedSubjectId {
        EncodedSubjectId(encode_wire(&self.0))
    }
}

/// The SpiceDB wire form of a resource (object) ID. Only the codec builds it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EncodedObjectId(String);

impl EncodedObjectId {
    /// Encode a permission object's ID.
    pub fn encode(object: &Object) -> Result<Self, AuthzIdError> {
        Ok(AuthzOpaqueId::new(object.id.as_str())?.to_object_wire())
    }

    /// The wire text, used only when building a SpiceDB request.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The SpiceDB wire form of a subject ID. Only the codec builds it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EncodedSubjectId(String);

impl EncodedSubjectId {
    /// Encode a permission subject's ID. `user` subjects must be exact bare
    /// account JIDs; other subject types are opaque. The userset relation is
    /// a separate SpiceDB field and is not part of the ID.
    pub fn encode(subject: &Subject) -> Result<Self, AuthzIdError> {
        match subject.subject_type {
            SubjectType::User => Ok(AuthzUserJid::parse_exact(&subject.id)?.to_subject_wire()),
            SubjectType::Space | SubjectType::Channel | SubjectType::Role => {
                Ok(AuthzOpaqueId::new(subject.id.as_str())?.to_subject_wire())
            }
        }
    }

    /// The wire text, used only when building a SpiceDB request.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Decode a resource ID that SpiceDB returned for `object_type`.
pub fn decode_object(object_type: ObjectType, wire: &str) -> Result<Object, AuthzIdError> {
    Ok(Object::new(
        object_type,
        AuthzOpaqueId::from_wire(wire)?.into_string(),
    ))
}

/// Decode a subject ID that SpiceDB returned for `subject_type`.
pub fn decode_subject(
    subject_type: SubjectType,
    wire: &str,
    relation: Option<String>,
) -> Result<Subject, AuthzIdError> {
    let id = match subject_type {
        SubjectType::User => AuthzUserJid::from_wire(wire)?.jid().to_string(),
        SubjectType::Space | SubjectType::Channel | SubjectType::Role => {
            AuthzOpaqueId::from_wire(wire)?.into_string()
        }
    };
    Ok(Subject {
        subject_type,
        id,
        relation,
    })
}

fn validate_source(value: &str) -> Result<(), AuthzIdError> {
    if value.is_empty() {
        return Err(AuthzIdError::Empty);
    }
    if value.len() > MAX_AUTHZ_ID_BYTES {
        return Err(AuthzIdError::TooLong);
    }
    Ok(())
}

// Callers pass validated source text only.
fn encode_wire(value: &str) -> String {
    let mut wire = String::from(AUTHZ_ID_WIRE_PREFIX);
    URL_SAFE_NO_PAD.encode_string(value, &mut wire);
    wire
}

fn decode_wire(wire: &str) -> Result<String, AuthzIdError> {
    // Bound allocation and decoding work before examining the payload.
    if wire.len() > MAX_AUTHZ_WIRE_BYTES {
        return Err(AuthzIdError::TooLong);
    }
    let payload = wire
        .strip_prefix(AUTHZ_ID_WIRE_PREFIX)
        .ok_or(AuthzIdError::UnsupportedVersion)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AuthzIdError::InvalidEncoding)?;
    let value = String::from_utf8(bytes).map_err(|_| AuthzIdError::InvalidUtf8)?;
    validate_source(&value)?;
    // Require the unique representation, including zero trailing bits.
    if encode_wire(&value) != wire {
        return Err(AuthzIdError::InvalidEncoding);
    }
    Ok(value)
}
