//! Typed durable identity for a previously-authenticated XMPP principal.
//!
//! This is deliberately a reference, not a credential: it cannot carry a
//! session token, SASL payload, bearer proof, or a mutable authorization
//! snapshot. The server resolves it against its Postgres authority at resume
//! time and fails closed if the exact version or epoch is no longer current.

use jid::BareJid;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthContextId(Uuid);

impl AuthContextId {
    pub fn new(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AuthContextVersion(u64);

impl AuthContextVersion {
    pub const INITIAL: Self = Self(1);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrincipalAuthEpoch(u64);

impl PrincipalAuthEpoch {
    pub const INITIAL: Self = Self(1);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Nonforgeable authority reference persisted beside an SM snapshot.
///
/// Fields remain private so protocol code cannot manufacture a reference
/// from a String or a token. Creation is restricted to an authenticated
/// server boundary that has already validated the principal.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthenticatedPrincipalRef {
    bare_jid: BareJid,
    auth_context_id: AuthContextId,
    auth_context_version: AuthContextVersion,
    auth_epoch: PrincipalAuthEpoch,
}

impl AuthenticatedPrincipalRef {
    pub fn new(
        bare_jid: BareJid,
        auth_context_id: AuthContextId,
        auth_context_version: AuthContextVersion,
        auth_epoch: PrincipalAuthEpoch,
    ) -> Self {
        Self {
            bare_jid,
            auth_context_id,
            auth_context_version,
            auth_epoch,
        }
    }

    pub fn bare_jid(&self) -> &BareJid {
        &self.bare_jid
    }

    pub fn auth_context_id(&self) -> &AuthContextId {
        &self.auth_context_id
    }

    pub fn auth_context_version(&self) -> AuthContextVersion {
        self.auth_context_version
    }

    pub fn auth_epoch(&self) -> PrincipalAuthEpoch {
        self.auth_epoch
    }
}
