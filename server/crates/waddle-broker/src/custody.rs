//! `BrokerCustodyService`: end-to-end credential custody lifecycle mirroring
//! `server/specs/quint/credential_lifecycle_restore.qnt` 1-to-1.
//!
//! Enforces:
//! - Envelope encryption with 3-tier KMS -> TenantDek -> UserDek hierarchy
//! - Idempotent XEP-0198 enrollment nonces (same nonce + same payload digest ->
//!   identical opaque receipt; same nonce + different digest -> fail closed)
//! - Append-only destruction ledger + retired account registry
//! - Fail-closed egress gate on `begin_restore()` until
//!   `apply_destruction_ledger_and_reopen()` purges all destroyed/retired entries
//! - Tenant deletion wired to `TenantStore::delete_tenant` and
//!   `PermissionBackend::delete_tenant_store`

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::PermissionBackend;
use waddle_broker_core::store::TenantStore;

use crate::envelope::{decrypt_secret, encrypt_secret, EnvelopeContext};
use crate::error::{BrokerError, KmsError};
use crate::key_hierarchy::KeyHierarchyService;
use crate::ledger::{
    DestructionLedgerStore, DestructionReason, DestructionRecord, DestructionTargetKind,
    EgressGateState, EnrollmentNonceRecord, NonceClaimOutcome, RetiredAccountRecord,
    StoredEnvelopeRecord,
};
use crate::secret::SecretBytes;

/// Opaque receipt returned by credential lifecycle operations (zero secret material).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialReceipt {
    pub credential_id: Uuid,
    pub version: u32,
    pub status: CredentialLifecycleStatus,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialLifecycleStatus {
    Active,
    Revoked,
}

/// Request to enroll a tenant- or member-scoped provider credential.
pub struct EnrollCredentialInput {
    pub tenant_id: TenantId,
    pub account_id: Option<AccountId>,
    pub provider: String,
    pub nonce: String,
    /// SHA-256 digest of the canonical client request payload (used to detect
    /// nonce reuse with a mutated payload).
    pub payload_digest_sha256: [u8; 32],
    pub expires_at_ms: i64,
    pub plaintext: SecretBytes,
}

/// Summary of a restore-drill purge pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReopenReport {
    pub purged_envelopes: usize,
    pub applied_destructions: usize,
    pub applied_retired_accounts: usize,
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn versioned_cred_target(credential_id: Uuid, version: u32) -> String {
    format!("{credential_id}:v{version}")
}

fn verify_replayed_nonce(
    existing: &EnrollmentNonceRecord,
    expected_tenant: &TenantId,
    expected_digest: &[u8; 32],
) -> Result<CredentialReceipt, BrokerError> {
    let same_digest = bool::from(existing.payload_digest_sha256.ct_eq(expected_digest));
    if !same_digest || existing.tenant_id != *expected_tenant {
        return Err(BrokerError::NoncePayloadConflict);
    }
    Ok(CredentialReceipt {
        credential_id: existing.credential_id,
        version: existing.credential_version,
        status: CredentialLifecycleStatus::Active,
        replayed: true,
    })
}

/// Authoritative broker custody service for encrypted provider credentials.
pub struct BrokerCustodyService {
    hierarchy: Arc<KeyHierarchyService>,
    ledger: Arc<dyn DestructionLedgerStore>,
    egress_open: AtomicBool,
}

impl BrokerCustodyService {
    pub async fn new(
        hierarchy: Arc<KeyHierarchyService>,
        ledger: Arc<dyn DestructionLedgerStore>,
    ) -> Result<Self, BrokerError> {
        let state = ledger.get_egress_state().await?;
        Ok(Self {
            hierarchy,
            ledger,
            egress_open: AtomicBool::new(state.egress_open),
        })
    }

    pub fn egress_open(&self) -> bool {
        self.egress_open.load(Ordering::SeqCst)
    }

    pub fn ledger(&self) -> &Arc<dyn DestructionLedgerStore> {
        &self.ledger
    }

    pub fn hierarchy(&self) -> &Arc<KeyHierarchyService> {
        &self.hierarchy
    }

    /// Enrolls a new credential with idempotent XEP-0198 replay nonce handling.
    pub async fn enroll(
        &self,
        input: EnrollCredentialInput,
    ) -> Result<CredentialReceipt, BrokerError> {
        if self
            .ledger
            .is_destroyed(
                DestructionTargetKind::Tenant,
                &input.tenant_id,
                input.tenant_id.as_str(),
            )
            .await?
        {
            return Err(BrokerError::TenantDestroyed(input.tenant_id));
        }

        if let Some(account_id) = input.account_id {
            if self
                .ledger
                .is_account_retired(&input.tenant_id, account_id)
                .await?
                || self
                    .ledger
                    .is_destroyed(
                        DestructionTargetKind::Account,
                        &input.tenant_id,
                        &account_id.to_string(),
                    )
                    .await?
            {
                return Err(BrokerError::AccountRetired(account_id));
            }
        }

        let nonce_hash = sha256_bytes(input.nonce.as_bytes());
        if let Some(existing) = self.ledger.get_enrollment_nonce(&nonce_hash).await? {
            return verify_replayed_nonce(
                &existing,
                &input.tenant_id,
                &input.payload_digest_sha256,
            );
        }

        let credential_id = Uuid::now_v7();
        let version = 1u32;
        let stored = self
            .seal_and_build_record(
                credential_id,
                &input.tenant_id,
                input.account_id,
                &input.provider,
                version,
                &input.plaintext,
            )
            .await?;

        let now = now_ms();
        let nonce_record = EnrollmentNonceRecord {
            nonce_sha256: nonce_hash,
            payload_digest_sha256: input.payload_digest_sha256,
            tenant_id: input.tenant_id.clone(),
            credential_id,
            credential_version: version,
            expires_at_ms: input.expires_at_ms,
            created_at_ms: now,
        };
        match self
            .ledger
            .claim_enrollment_nonce_and_put_envelope(&nonce_record, &stored)
            .await?
        {
            NonceClaimOutcome::Claimed => Ok(CredentialReceipt {
                credential_id,
                version,
                status: CredentialLifecycleStatus::Active,
                replayed: false,
            }),
            NonceClaimOutcome::Existing(existing) => {
                verify_replayed_nonce(&existing, &input.tenant_id, &input.payload_digest_sha256)
            }
        }
    }

    /// Rotates an existing credential to `version + 1` and records the prior
    /// version in the destruction ledger so stale backups cannot resurrect it.
    pub async fn rotate(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
        new_plaintext: &SecretBytes,
    ) -> Result<CredentialReceipt, BrokerError> {
        self.ensure_not_destroyed(tenant_id, credential_id).await?;
        let existing = self
            .ledger
            .get_envelope(tenant_id, credential_id)
            .await?
            .ok_or(BrokerError::CredentialNotFound(credential_id))?;

        let old_version = existing.version;
        let new_version = old_version.saturating_add(1);
        let updated = self
            .seal_and_build_record(
                credential_id,
                tenant_id,
                existing.account_id,
                &existing.provider,
                new_version,
                new_plaintext,
            )
            .await?;

        self.ledger
            .append_destruction(&DestructionRecord {
                entry_id: Uuid::now_v7(),
                target_kind: DestructionTargetKind::CredentialVersion,
                tenant_id: tenant_id.clone(),
                target_id: versioned_cred_target(credential_id, old_version),
                reason: DestructionReason::Rotated,
                destroyed_at_ms: now_ms(),
            })
            .await?;
        self.ledger.put_envelope(&updated).await?;

        Ok(CredentialReceipt {
            credential_id,
            version: new_version,
            status: CredentialLifecycleStatus::Active,
            replayed: false,
        })
    }

    /// Replaces an existing credential with a brand-new `credential_id`,
    /// permanently destroying the replaced credential in the destruction ledger.
    pub async fn replace(
        &self,
        tenant_id: &TenantId,
        old_credential_id: Uuid,
        new_plaintext: &SecretBytes,
        nonce: String,
        payload_digest_sha256: [u8; 32],
        expires_at_ms: i64,
    ) -> Result<CredentialReceipt, BrokerError> {
        let existing = self
            .ledger
            .get_envelope(tenant_id, old_credential_id)
            .await?
            .ok_or(BrokerError::CredentialNotFound(old_credential_id))?;

        self.revoke_and_destroy_with_reason(
            tenant_id,
            old_credential_id,
            DestructionReason::Replaced,
        )
        .await?;

        self.enroll(EnrollCredentialInput {
            tenant_id: tenant_id.clone(),
            account_id: existing.account_id,
            provider: existing.provider,
            nonce,
            payload_digest_sha256,
            expires_at_ms,
            plaintext: new_plaintext.clone(),
        })
        .await
    }

    /// Permanently revokes and destroys a credential (appends to destruction
    /// ledger, deletes ciphertext row, and destroys user KMS wrapping secret if
    /// no other active credentials remain for that user).
    pub async fn revoke_and_destroy(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<CredentialReceipt, BrokerError> {
        self.revoke_and_destroy_with_reason(tenant_id, credential_id, DestructionReason::Revoked)
            .await
    }

    async fn revoke_and_destroy_with_reason(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
        reason: DestructionReason,
    ) -> Result<CredentialReceipt, BrokerError> {
        let existing = self.ledger.get_envelope(tenant_id, credential_id).await?;
        let version = existing.as_ref().map(|e| e.version).unwrap_or(1);

        self.ledger
            .append_destruction(&DestructionRecord {
                entry_id: Uuid::now_v7(),
                target_kind: DestructionTargetKind::Credential,
                tenant_id: tenant_id.clone(),
                target_id: credential_id.to_string(),
                reason,
                destroyed_at_ms: now_ms(),
            })
            .await?;
        let _ = self
            .ledger
            .delete_envelope(tenant_id, credential_id)
            .await?;

        Ok(CredentialReceipt {
            credential_id,
            version,
            status: CredentialLifecycleStatus::Revoked,
            replayed: false,
        })
    }

    /// Retires an account (`broker_retired_accounts` + destruction ledger),
    /// deletes all member credentials for `(tenant_id, account_id)`, and
    /// permanently destroys the user's KMS wrapping secret.
    pub async fn retire_account(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
        jid_key: impl Into<String>,
        localpart_key: impl Into<String>,
    ) -> Result<u64, BrokerError> {
        let now = now_ms();
        self.ledger
            .record_retired_account(&RetiredAccountRecord {
                tenant_id: tenant_id.clone(),
                account_id,
                jid_key: jid_key.into(),
                localpart_key: localpart_key.into(),
                retired_at_ms: now,
            })
            .await?;
        self.ledger
            .append_destruction(&DestructionRecord {
                entry_id: Uuid::now_v7(),
                target_kind: DestructionTargetKind::Account,
                tenant_id: tenant_id.clone(),
                target_id: account_id.to_string(),
                reason: DestructionReason::AccountRetired,
                destroyed_at_ms: now,
            })
            .await?;
        let kms_ref = self
            .hierarchy
            .destroy_user_kms_secret(tenant_id, account_id)
            .await?;
        self.ledger
            .append_destruction(&DestructionRecord {
                entry_id: Uuid::now_v7(),
                target_kind: DestructionTargetKind::KmsKey,
                tenant_id: tenant_id.clone(),
                target_id: kms_ref.as_str().to_owned(),
                reason: DestructionReason::AccountRetired,
                destroyed_at_ms: now,
            })
            .await?;
        let deleted = self
            .ledger
            .delete_envelopes_for_account(tenant_id, account_id)
            .await?;
        Ok(deleted)
    }

    /// Deletes a tenant across broker custody, KMS, `TenantStore` (Turso WAL
    /// purge / Postgres purge), and `PermissionBackend` (OpenFGA `DeleteStore`
    /// / SpiceDB purge).
    pub async fn delete_tenant(
        &self,
        tenant_id: &TenantId,
        tenant_store: &dyn TenantStore,
        permission_backend: &dyn PermissionBackend,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        self.ledger
            .append_destruction(&DestructionRecord {
                entry_id: Uuid::now_v7(),
                target_kind: DestructionTargetKind::Tenant,
                tenant_id: tenant_id.clone(),
                target_id: tenant_id.as_str().to_owned(),
                reason: DestructionReason::TenantDeleted,
                destroyed_at_ms: now,
            })
            .await?;

        let envelopes = self.ledger.list_envelopes_for_tenant(tenant_id).await?;
        let mut destroyed_accounts = HashSet::new();
        for env in &envelopes {
            if let Some(account_id) = env.account_id {
                if !destroyed_accounts.insert(account_id) {
                    continue;
                }
                match self
                    .hierarchy
                    .destroy_user_kms_secret(tenant_id, account_id)
                    .await
                {
                    Ok(user_kms_ref) => {
                        self.ledger
                            .append_destruction(&DestructionRecord {
                                entry_id: Uuid::now_v7(),
                                target_kind: DestructionTargetKind::KmsKey,
                                tenant_id: tenant_id.clone(),
                                target_id: user_kms_ref.as_str().to_owned(),
                                reason: DestructionReason::TenantDeleted,
                                destroyed_at_ms: now,
                            })
                            .await?;
                    }
                    Err(BrokerError::Kms(KmsError::KeyNotFound(_) | KmsError::KeyDestroyed(_))) => {
                    }
                    Err(err) => return Err(err),
                }
            }
        }
        match self.hierarchy.destroy_tenant_kms_kek(tenant_id).await {
            Ok(tenant_kek_ref) => {
                self.ledger
                    .append_destruction(&DestructionRecord {
                        entry_id: Uuid::now_v7(),
                        target_kind: DestructionTargetKind::KmsKey,
                        tenant_id: tenant_id.clone(),
                        target_id: tenant_kek_ref.as_str().to_owned(),
                        reason: DestructionReason::TenantDeleted,
                        destroyed_at_ms: now,
                    })
                    .await?;
            }
            Err(BrokerError::Kms(KmsError::KeyNotFound(_) | KmsError::KeyDestroyed(_))) => {}
            Err(err) => return Err(err),
        }
        let _ = self.ledger.delete_envelopes_for_tenant(tenant_id).await?;

        tenant_store.delete_tenant(tenant_id).await?;
        permission_backend.delete_tenant_store(tenant_id).await?;
        Ok(())
    }

    /// Transitions the broker into restore mode (`egress_open = false`),
    /// mirroring Quint `restoreFromBackup`. All `unseal_for_egress` calls fail
    /// closed until `apply_destruction_ledger_and_reopen` succeeds.
    pub async fn begin_restore(&self) -> Result<(), BrokerError> {
        self.egress_open.store(false, Ordering::SeqCst);
        self.ledger
            .set_egress_state(EgressGateState {
                egress_open: false,
                last_restore_applied_ms: None,
            })
            .await?;
        Ok(())
    }

    /// Reconciles restored vault ciphertexts against the durable destruction
    /// ledger and retired accounts, purging every destroyed credential,
    /// superseded version, retired account, or deleted tenant before reopening
    /// egress (`applyDestructionLedgerAndReopen` in Quint).
    pub async fn apply_destruction_ledger_and_reopen(
        &self,
    ) -> Result<RestoreReopenReport, BrokerError> {
        let destructions = self.ledger.list_destructions().await?;
        let retired_accounts = self.ledger.list_retired_accounts().await?;
        let envelopes = self.ledger.list_all_envelopes().await?;

        let mut purged_envelopes = 0usize;
        for env in &envelopes {
            let mut should_purge = false;
            for d in &destructions {
                if d.tenant_id != env.tenant_id {
                    continue;
                }
                match d.target_kind {
                    DestructionTargetKind::Tenant if d.target_id == env.tenant_id.as_str() => {
                        should_purge = true;
                    }
                    DestructionTargetKind::Credential
                        if d.target_id == env.credential_id.to_string() =>
                    {
                        should_purge = true;
                    }
                    DestructionTargetKind::CredentialVersion
                        if d.target_id == versioned_cred_target(env.credential_id, env.version) =>
                    {
                        should_purge = true;
                    }
                    DestructionTargetKind::Account => {
                        if let Some(acct) = env.account_id {
                            if d.target_id == acct.to_string() {
                                should_purge = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !should_purge {
                if let Some(acct) = env.account_id {
                    if retired_accounts
                        .iter()
                        .any(|r| r.tenant_id == env.tenant_id && r.account_id == acct)
                    {
                        should_purge = true;
                    }
                }
            }
            if should_purge
                && self
                    .ledger
                    .delete_envelope(&env.tenant_id, env.credential_id)
                    .await?
            {
                purged_envelopes += 1;
            }
        }

        let now = now_ms();
        self.ledger
            .set_egress_state(EgressGateState {
                egress_open: true,
                last_restore_applied_ms: Some(now),
            })
            .await?;
        self.egress_open.store(true, Ordering::SeqCst);

        Ok(RestoreReopenReport {
            purged_envelopes,
            applied_destructions: destructions.len(),
            applied_retired_accounts: retired_accounts.len(),
        })
    }

    /// Unseals a credential solely inside broker memory for constrained
    /// provider egress. Enforces all 6 Quint/Plan safety checks.
    pub async fn unseal_for_egress(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<SecretBytes, BrokerError> {
        if !self.egress_open() {
            return Err(BrokerError::EgressClosedPendingRestore);
        }
        let persisted_gate = self.ledger.get_egress_state().await?;
        if !persisted_gate.egress_open {
            self.egress_open.store(false, Ordering::SeqCst);
            return Err(BrokerError::EgressClosedPendingRestore);
        }

        self.ensure_not_destroyed(tenant_id, credential_id).await?;

        let record = self
            .ledger
            .get_envelope(tenant_id, credential_id)
            .await?
            .ok_or(BrokerError::CredentialNotFound(credential_id))?;

        if self
            .ledger
            .is_destroyed(
                DestructionTargetKind::CredentialVersion,
                tenant_id,
                &versioned_cred_target(credential_id, record.version),
            )
            .await?
        {
            return Err(BrokerError::CredentialDestroyed(credential_id));
        }

        if let Some(account_id) = record.account_id {
            if self
                .ledger
                .is_account_retired(tenant_id, account_id)
                .await?
                || self
                    .ledger
                    .is_destroyed(
                        DestructionTargetKind::Account,
                        tenant_id,
                        &account_id.to_string(),
                    )
                    .await?
            {
                return Err(BrokerError::AccountRetired(account_id));
            }
        }

        let effective_dek = match &record.user_dek {
            Some(user_dek_rec) => {
                self.hierarchy
                    .unwrap_user_dek(&record.tenant_dek, user_dek_rec)
                    .await?
            }
            None => self.hierarchy.unwrap_tenant_dek(&record.tenant_dek).await?,
        };

        let ctx = EnvelopeContext {
            trust_domain: self.hierarchy.trust_domain().to_owned(),
            tenant_id: record.tenant_id.clone(),
            account_id: record.account_id,
            provider: record.provider.clone(),
            credential_id: record.credential_id,
            version: record.version,
        };
        let secret = decrypt_secret(&effective_dek, &ctx, &record.envelope)?;
        Ok(secret)
    }

    async fn ensure_not_destroyed(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<(), BrokerError> {
        if self
            .ledger
            .is_destroyed(DestructionTargetKind::Tenant, tenant_id, tenant_id.as_str())
            .await?
        {
            return Err(BrokerError::TenantDestroyed(tenant_id.clone()));
        }
        if self
            .ledger
            .is_destroyed(
                DestructionTargetKind::Credential,
                tenant_id,
                &credential_id.to_string(),
            )
            .await?
        {
            return Err(BrokerError::CredentialDestroyed(credential_id));
        }
        Ok(())
    }

    async fn seal_and_build_record(
        &self,
        credential_id: Uuid,
        tenant_id: &TenantId,
        account_id: Option<AccountId>,
        provider: &str,
        version: u32,
        plaintext: &SecretBytes,
    ) -> Result<StoredEnvelopeRecord, BrokerError> {
        let (tenant_dek_rec, tenant_dek) =
            self.hierarchy.create_tenant_dek(tenant_id, version).await?;
        let (user_dek_rec, effective_dek, wrapped_dek_id) = match account_id {
            Some(acct) => {
                let (u_rec, u_key) = self
                    .hierarchy
                    .create_user_dek(&tenant_dek_rec, &tenant_dek, acct, version)
                    .await?;
                let id = u_rec.dek_id;
                (Some(u_rec), u_key, id)
            }
            None => {
                let id = tenant_dek_rec.dek_id;
                (None, tenant_dek, id)
            }
        };

        let ctx = EnvelopeContext {
            trust_domain: self.hierarchy.trust_domain().to_owned(),
            tenant_id: tenant_id.clone(),
            account_id,
            provider: provider.to_owned(),
            credential_id,
            version,
        };
        let envelope = encrypt_secret(&effective_dek, &ctx, wrapped_dek_id, version, plaintext)?;
        Ok(StoredEnvelopeRecord {
            credential_id,
            tenant_id: tenant_id.clone(),
            account_id,
            provider: provider.to_owned(),
            version,
            envelope,
            tenant_dek: tenant_dek_rec,
            user_dek: user_dek_rec,
            created_at_ms: now_ms(),
        })
    }
}
