//! Client-visible resume token and caller-minted operation identity.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngExt;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the resume id is not a token this server issues")]
pub struct SmResumeTokenError;

/// The opaque value a client receives as the `id` of `<enabled/>` and
/// presents as the `previd` of `<resume/>`.
///
/// It is a bearer secret for the session and is never stored: the durable
/// record holds only [`SmResumeTokenDigest`]. `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct SmResumeToken([u8; TOKEN_BYTES]);

impl SmResumeToken {
    pub fn mint() -> Self {
        let mut bytes = [0_u8; TOKEN_BYTES];
        rand::rng().fill(&mut bytes[..]);
        Self(bytes)
    }

    /// Parse the `previd` attribute of a `<resume/>`.
    pub fn from_wire(previd: &str) -> Result<Self, SmResumeTokenError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(previd)
            .map_err(|_| SmResumeTokenError)?;
        <[u8; TOKEN_BYTES]>::try_from(bytes)
            .map(Self)
            .map_err(|_| SmResumeTokenError)
    }

    /// The attribute value sent to the client.
    pub fn to_wire(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn digest(&self) -> SmResumeTokenDigest {
        SmResumeTokenDigest(Sha256::digest(self.0).into())
    }
}

impl fmt::Debug for SmResumeToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SmResumeToken(..)")
    }
}

/// SHA-256 of a resume token: the durable lookup key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmResumeTokenDigest([u8; 32]);

impl SmResumeTokenDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Identity of one enable, resume or admission attempt. The caller mints it
/// before the commit and keeps it until the outcome is known, so an unknown
/// commit is resolved under the identity it was issued with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmOperationId(Uuid);

impl SmOperationId {
    pub fn mint() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}
