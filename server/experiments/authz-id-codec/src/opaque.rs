use crate::codec::{decode, encode, validate_id};
use crate::CodecError;

/// A nonempty opaque UTF-8 identifier. No normalization or delimiter parsing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpaqueId(String);

impl OpaqueId {
    pub fn new(value: impl Into<String>) -> Result<Self, CodecError> {
        let value = value.into();
        validate_id(&value)?;
        Ok(Self(value))
    }

    /// Parse a persisted/wire identifier with no legacy fallback.
    pub fn from_wire(wire: &str) -> Result<Self, CodecError> {
        Self::new(decode(wire)?)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_wire(&self) -> EncodedOpaqueId {
        EncodedOpaqueId(encode(&self.0))
    }
}

/// Validated opaque-ID wire representation, distinct from an encoded user JID.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EncodedOpaqueId(String);

impl EncodedOpaqueId {
    /// Serialize only at the eventual storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
