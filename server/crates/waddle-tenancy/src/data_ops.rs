//! Shared tenant-scoped data operations contract implemented by both
//! `TursoTenantStore` (per-tenant WAL `.db`) and `PostgresTenantStore`
//! (multi-tenant `tenant_id`-scoped tables).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use waddle_broker_core::identity::AccountId;
use waddle_broker_core::store::{TenantQueryScope, TenantStore, TenantStoreError};

/// Tenant-scoped user record with immutable `account_id` UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantUserRecord {
    pub account_id: AccountId,
    pub jid: String,
    pub username: String,
    pub xmpp_localpart: String,
    pub display_name: Option<String>,
}

/// Tenant-scoped external identity binding (`auth_identities`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantAuthIdentityRecord {
    pub id: String,
    pub user_jid: String,
    pub provider_id: String,
    pub issuer: String,
    pub subject: String,
}

/// Tenant-scoped space metadata (`spaces_metadata`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantSpaceRecord {
    pub space_node: String,
    pub space_jid: String,
    pub name: String,
}

/// Tenant-scoped federated remote participant membership/cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteParticipantRecord {
    pub room_id: String,
    pub remote_bare_jid: String,
    pub affiliation: String,
}

/// Tenant-scoped background job envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantJobRecord {
    pub job_id: String,
    pub queue: String,
    pub payload_json: String,
    pub status: String,
}

/// Backend-agnostic tenant-scoped CRUD/queue contract extending [`TenantStore`].
/// Every method requires a fenced [`TenantQueryScope`] so no cross-tenant read
/// or write can occur on either Turso or PostgreSQL.
#[async_trait]
pub trait TenantDataOps: TenantStore {
    async fn upsert_user(
        &self,
        scope: &TenantQueryScope,
        user: &TenantUserRecord,
    ) -> Result<(), TenantStoreError>;

    async fn get_user_by_localpart(
        &self,
        scope: &TenantQueryScope,
        xmpp_localpart: &str,
    ) -> Result<Option<TenantUserRecord>, TenantStoreError>;

    async fn update_user_account_id_for_test(
        &self,
        scope: &TenantQueryScope,
        jid: &str,
        new_account_id: AccountId,
    ) -> Result<(), TenantStoreError>;

    async fn upsert_auth_identity(
        &self,
        scope: &TenantQueryScope,
        identity: &TenantAuthIdentityRecord,
    ) -> Result<(), TenantStoreError>;

    async fn get_auth_identity(
        &self,
        scope: &TenantQueryScope,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<TenantAuthIdentityRecord>, TenantStoreError>;

    async fn upsert_space(
        &self,
        scope: &TenantQueryScope,
        space: &TenantSpaceRecord,
    ) -> Result<(), TenantStoreError>;

    async fn get_space(
        &self,
        scope: &TenantQueryScope,
        space_node: &str,
    ) -> Result<Option<TenantSpaceRecord>, TenantStoreError>;

    async fn record_remote_participant(
        &self,
        scope: &TenantQueryScope,
        participant: &RemoteParticipantRecord,
    ) -> Result<(), TenantStoreError>;

    async fn list_remote_participants(
        &self,
        scope: &TenantQueryScope,
        room_id: &str,
    ) -> Result<Vec<RemoteParticipantRecord>, TenantStoreError>;

    async fn enqueue_job(
        &self,
        scope: &TenantQueryScope,
        job: &TenantJobRecord,
    ) -> Result<(), TenantStoreError>;

    async fn claim_next_job(
        &self,
        scope: &TenantQueryScope,
        queue: &str,
    ) -> Result<Option<TenantJobRecord>, TenantStoreError>;
}
