use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use thiserror::Error;

const PREFIX: &str = "v2_";

/// Maximum SpiceDB object-ID wire length, in bytes.
pub const MAX_WIRE_BYTES: usize = 1024;

/// Largest input whose unpadded base64url plus `v2_` fits the wire limit.
/// 765 bytes encode to 1023 bytes; 766 bytes would require 1025 bytes.
pub const MAX_ID_BYTES: usize = 765;

/// Validation failures contain no identifier material.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("identifier must not be empty")]
    Empty,
    #[error("identifier exceeds the codec byte limit")]
    TooLong,
    #[error("encoded identifier has an unsupported version")]
    UnsupportedVersion,
    #[error("encoded identifier is not canonical unpadded base64url")]
    InvalidEncoding,
    #[error("decoded identifier is not UTF-8")]
    InvalidUtf8,
    #[error("user identifier must be a bare JID with a localpart")]
    InvalidUserJid,
    #[error("user identifier changes when parsed as a JID")]
    UserJidChangedByParser,
}

pub(crate) fn validate_id(value: &str) -> Result<(), CodecError> {
    if value.is_empty() {
        return Err(CodecError::Empty);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(CodecError::TooLong);
    }
    Ok(())
}

// Callers hold validated, immutable source types.
pub(crate) fn encode(value: &str) -> String {
    let mut wire = String::from(PREFIX);
    URL_SAFE_NO_PAD.encode_string(value, &mut wire);
    wire
}

pub(crate) fn decode(wire: &str) -> Result<String, CodecError> {
    // Bound allocation and decoding work before examining the payload.
    if wire.len() > MAX_WIRE_BYTES {
        return Err(CodecError::TooLong);
    }
    let payload = wire
        .strip_prefix(PREFIX)
        .ok_or(CodecError::UnsupportedVersion)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| CodecError::InvalidEncoding)?;
    let value = String::from_utf8(bytes).map_err(|_| CodecError::InvalidUtf8)?;
    validate_id(&value)?;
    // Explicitly require a unique representation, including zero trailing bits.
    if encode(&value) != wire {
        return Err(CodecError::InvalidEncoding);
    }
    Ok(value)
}
