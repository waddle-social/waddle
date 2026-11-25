//! Device-signed inference invocation validation, principal restriction, and
//! strict payer separation.

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use waddle_broker_core::admission::{BudgetLimits, CredentialKey, CredentialScope, RoomScopeKey};
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::AuthError;
use crate::idp_proof::{enforce_not_scram, AuthProvenance, DeviceKeyRegistry};

/// Maximum validity window for a device-signed inference invocation (5 minutes).
pub const MAX_INVOCATION_TTL_MS: i64 = 5 * 60 * 1_000;

/// Principal requesting an inference invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationPrincipal {
    LocalMember {
        account_id: AccountId,
        provenance: AuthProvenance,
    },
    TenantAdmin {
        account_id: AccountId,
        provenance: AuthProvenance,
    },
    /// Room observation callback (background/async). NEVER allowed to select member credentials.
    Observation,
    /// Scheduled job. NEVER allowed to select member credentials.
    Schedule,
    /// Inbound webhook trigger. NEVER allowed to select member credentials.
    Webhook,
    /// Federated remote user. NEVER allowed to select member credentials.
    FederatedRemote { remote_jid: String },
}

/// Explicit payer selection. The broker NEVER silently switches from `Member`
/// to `Tenant` (or vice versa) when any authorization or credential check fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PayerSelection {
    Member {
        account_id: AccountId,
        credential_id: Uuid,
    },
    Tenant {
        credential_id: Uuid,
    },
}

impl PayerSelection {
    pub const fn credential_key(self) -> CredentialKey {
        match self {
            Self::Member {
                account_id,
                credential_id,
            } => CredentialKey {
                credential_id,
                scope: CredentialScope::Member { account_id },
            },
            Self::Tenant { credential_id } => CredentialKey {
                credential_id,
                scope: CredentialScope::Tenant,
            },
        }
    }

    pub const fn trace_label(self) -> &'static str {
        match self {
            Self::Member { .. } => "member",
            Self::Tenant { .. } => "tenant",
        }
    }
}

/// Errors raised while validating an inference invocation before admission.
#[derive(Debug, Error)]
pub enum InvocationError {
    #[error("principal {principal} is forbidden from using member-funded credentials")]
    PrincipalCannotUseMemberCredential { principal: &'static str },
    #[error("member credential belongs to a different account than the invoking principal")]
    MemberAccountMismatch,
    #[error("device-signed invocation is required for member-funded inference")]
    SignedInvocationRequired,
    #[error("signed invocation tenant or payer does not match request")]
    SignedInvocationBindingMismatch,
    #[error("invocation expired or TTL exceeds maximum")]
    InvocationExpired,
    #[error("authentication error: {0}")]
    Auth(#[from] AuthError),
}

/// Device-signed inference invocation binding all Step 5 fields in a
/// length-prefixed canonical transcript verified by a registered P-256 device key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedInferenceInvocation {
    pub trust_domain: String,
    pub tenant_id: TenantId,
    pub account_id: AccountId,
    pub device_key_id: Uuid,
    pub extension_id: String,
    pub extension_digest_sha256: [u8; 32],
    pub room_scope: RoomScopeKey,
    pub provider: String,
    pub payer: PayerSelection,
    pub command_digest_sha256: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at_ms: i64,
    pub budget: BudgetLimits,
    pub signature: Vec<u8>,
}

impl SignedInferenceInvocation {
    /// Builds the canonical length-prefixed transcript signed by the client
    /// device's P-256 key.
    pub fn canonical_transcript(&self) -> Vec<u8> {
        let payer_bytes = match self.payer {
            PayerSelection::Member {
                account_id,
                credential_id,
            } => format!("member:{account_id}:{credential_id}"),
            PayerSelection::Tenant { credential_id } => format!("tenant:{credential_id}"),
        };
        let account_uuid = self.account_id.as_uuid();
        let parts: [&[u8]; 16] = [
            b"waddle-inference-invoke-v1",
            self.trust_domain.as_bytes(),
            self.tenant_id.as_str().as_bytes(),
            account_uuid.as_bytes(),
            self.device_key_id.as_bytes(),
            self.extension_id.as_bytes(),
            &self.extension_digest_sha256,
            self.room_scope.scope_id.as_bytes(),
            self.provider.as_bytes(),
            payer_bytes.as_bytes(),
            &self.command_digest_sha256,
            &self.nonce,
            &self.expires_at_ms.to_be_bytes(),
            &self.budget.max_input_tokens.to_be_bytes(),
            &self.budget.max_output_tokens.to_be_bytes(),
            &self.budget.max_spend_microcents.to_be_bytes(),
        ];
        let total: usize = parts.iter().map(|p| 4 + p.len()).sum::<usize>() + 8;
        let mut out = Vec::with_capacity(total);
        for part in parts {
            let len = u32::try_from(part.len()).unwrap_or(u32::MAX);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(part);
        }
        out.extend_from_slice(&4u32.to_be_bytes());
        out.extend_from_slice(&self.budget.max_steps.to_be_bytes());
        out
    }
}

/// Validates that `principal` is permitted to request `payer`.
///
/// Enforces:
/// - `Observation`, `Schedule`, `Webhook`, and `FederatedRemote` principals are
///   NEVER permitted to select `PayerSelection::Member`.
/// - `AuthProvenance::NativeScram` accounts are NEVER permitted to select
///   `PayerSelection::Member`.
/// - A `LocalMember` / `TenantAdmin` can only select their own `Member` credential.
/// - Never silently falls back or rewrites `payer`.
pub fn validate_principal_and_payer(
    principal: &InvocationPrincipal,
    payer: PayerSelection,
) -> Result<(), InvocationError> {
    match payer {
        PayerSelection::Member {
            account_id: cred_owner,
            ..
        } => match principal {
            InvocationPrincipal::LocalMember {
                account_id,
                provenance,
            }
            | InvocationPrincipal::TenantAdmin {
                account_id,
                provenance,
            } => {
                enforce_not_scram(*provenance)?;
                if *account_id != cred_owner {
                    return Err(InvocationError::MemberAccountMismatch);
                }
                Ok(())
            }
            InvocationPrincipal::Observation => {
                Err(InvocationError::PrincipalCannotUseMemberCredential {
                    principal: "observation",
                })
            }
            InvocationPrincipal::Schedule => {
                Err(InvocationError::PrincipalCannotUseMemberCredential {
                    principal: "schedule",
                })
            }
            InvocationPrincipal::Webhook => {
                Err(InvocationError::PrincipalCannotUseMemberCredential {
                    principal: "webhook",
                })
            }
            InvocationPrincipal::FederatedRemote { .. } => {
                Err(InvocationError::PrincipalCannotUseMemberCredential {
                    principal: "federated_remote",
                })
            }
        },
        PayerSelection::Tenant { .. } => Ok(()),
    }
}

/// Verifies a `SignedInferenceInvocation` against the tenant's registered
/// active P-256 device key and expiry bounds.
pub fn validate_signed_invocation(
    registry: &DeviceKeyRegistry,
    invocation: &SignedInferenceInvocation,
    now_ms: i64,
) -> Result<(), InvocationError> {
    if invocation.expires_at_ms <= now_ms
        || invocation.expires_at_ms - now_ms > MAX_INVOCATION_TTL_MS
    {
        return Err(InvocationError::InvocationExpired);
    }

    let key = registry.get_active_key(
        &invocation.tenant_id,
        invocation.device_key_id,
        invocation.account_id,
    )?;
    let verifying_key = VerifyingKey::from_sec1_bytes(&key.public_key_sec1)
        .map_err(|_| AuthError::InvalidDeviceSignature)?;
    let sig = Signature::from_slice(&invocation.signature)
        .or_else(|_| Signature::from_der(&invocation.signature))
        .map_err(|_| AuthError::InvalidDeviceSignature)?;
    let transcript = invocation.canonical_transcript();
    verifying_key
        .verify(&transcript, &sig)
        .map_err(|_| AuthError::InvalidDeviceSignature)?;
    Ok(())
}
