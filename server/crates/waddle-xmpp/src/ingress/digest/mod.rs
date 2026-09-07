use std::fmt;

use crate::ingress::IngressTypeError;

mod input;
mod limits;
pub mod v1;

pub use input::{DigestContext, DigestInput, DigestInputError};
pub use limits::MAX_TEXT_LEN;

/// Version of the canonical semantic-digest algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigestVersion {
    V1,
}

impl DigestVersion {
    pub fn to_storage(&self) -> u8 {
        match self {
            Self::V1 => 1,
        }
    }

    pub fn from_storage(value: u8) -> Result<Self, IngressTypeError> {
        match value {
            1 => Ok(Self::V1),
            value => Err(IngressTypeError::UnsupportedDigestVersion { value }),
        }
    }
}

/// Versioned digest of a stanza's semantics.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SemanticDigest {
    version: DigestVersion,
    bytes: [u8; 32],
}

impl SemanticDigest {
    pub(crate) fn from_parts(version: DigestVersion, bytes: [u8; 32]) -> Self {
        Self { version, bytes }
    }

    pub fn from_storage(version: u8, bytes: [u8; 32]) -> Result<Self, IngressTypeError> {
        Ok(Self::from_parts(
            DigestVersion::from_storage(version)?,
            bytes,
        ))
    }

    pub fn to_storage(&self) -> (u8, [u8; 32]) {
        (self.version.to_storage(), self.bytes)
    }
}

impl fmt::Debug for SemanticDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut prefix = String::with_capacity(8);
        for byte in self.bytes.iter().take(4) {
            use std::fmt::Write as _;

            write!(&mut prefix, "{byte:02x}")?;
        }

        f.debug_struct("SemanticDigest")
            .field("version", &self.version)
            .field("prefix", &prefix)
            .finish()
    }
}
