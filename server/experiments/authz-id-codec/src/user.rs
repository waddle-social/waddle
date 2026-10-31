use jid::BareJid;

use crate::codec::{decode, encode, validate_id};
use crate::CodecError;

/// A validated bare user JID with a localpart, within the codec byte limit.
///
/// This validates syntax according to the `jid` crate. It does not establish a
/// canonical principal identity, resolve an account, or authenticate a user.
/// Equivalent domain spellings can remain distinct; see the README and the
/// domain-alias regression test before considering any downstream integration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ValidatedUserJid(BareJid);

impl ValidatedUserJid {
    /// Accept an already parsed bare JID, never a resource JID.
    pub fn new(jid: BareJid) -> Result<Self, CodecError> {
        validate_id(jid.as_str())?;
        if jid.node().is_none() {
            return Err(CodecError::InvalidUserJid);
        }
        Ok(Self(jid))
    }

    /// Require raw text to survive `jid` parsing without a byte change.
    ///
    /// Parser stability is not proof of logical identity canonicalization.
    pub fn parse_exact(value: &str) -> Result<Self, CodecError> {
        validate_id(value)?;
        let jid = BareJid::new(value).map_err(|_| CodecError::InvalidUserJid)?;
        if jid.as_str() != value {
            return Err(CodecError::UserJidChangedByParser);
        }
        Self::new(jid)
    }

    /// Reject legacy bytes, full JIDs, and text changed by the `jid` parser.
    pub fn from_wire(wire: &str) -> Result<Self, CodecError> {
        Self::parse_exact(&decode(wire)?)
    }

    pub fn jid(&self) -> &BareJid {
        &self.0
    }

    pub fn to_wire(&self) -> EncodedUserId {
        EncodedUserId(encode(self.0.as_str()))
    }
}

/// Validated user-ID wire representation, distinct from an encoded opaque ID.
///
/// ```compile_fail
/// use waddle_authz_id_codec_experiment::{EncodedUserId, OpaqueId};
/// let user: EncodedUserId = OpaqueId::new("alice@example.org")?.to_wire();
/// # Ok::<(), waddle_authz_id_codec_experiment::CodecError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EncodedUserId(String);

impl EncodedUserId {
    /// Serialize only at the eventual storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
