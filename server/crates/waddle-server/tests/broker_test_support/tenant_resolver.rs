use async_trait::async_trait;
use jid::BareJid;
use uuid::Uuid;
use waddle_broker::admission::invocation::SignedInferenceInvocation;
use waddle_broker::idp_proof::AuthProvenance;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_extensions::host_tools::InvocationContext;
use waddle_extensions::runtime::{HostInferenceError, HostInferenceErrorCode};
use waddle_server::server::extension_inference_host::BrokerCredentialResolver;

pub struct StaticTenantResolver {
    pub account_id: AccountId,
    pub credential_id: Uuid,
}

#[async_trait]
impl BrokerCredentialResolver for StaticTenantResolver {
    async fn resolve_requester_account(
        &self,
        _tenant_id: &TenantId,
        _requester: &BareJid,
    ) -> Result<(AccountId, AuthProvenance), HostInferenceError> {
        Ok((self.account_id, AuthProvenance::ExternalIdp))
    }

    async fn resolve_tenant_credential(
        &self,
        _tenant_id: &TenantId,
        _provider: &str,
        _credential_name: &str,
    ) -> Result<Uuid, HostInferenceError> {
        Ok(self.credential_id)
    }

    async fn resolve_member_invocation(
        &self,
        _tenant_id: &TenantId,
        _context: &InvocationContext,
        _provider: &str,
    ) -> Result<(AccountId, Uuid, SignedInferenceInvocation), HostInferenceError> {
        Err(HostInferenceError::new(
            HostInferenceErrorCode::Denied,
            "member invocation denied in test",
        ))
    }
}
