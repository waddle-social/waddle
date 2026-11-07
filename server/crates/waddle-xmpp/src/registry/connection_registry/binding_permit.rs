//! Revocable local evidence for one published connection binding.

use std::sync::{Arc, Mutex};

use jid::FullJid;
use tokio_util::sync::CancellationToken;

use crate::ownership::{BindingRef, ConnectionRef};

use super::LocalConnectionBindingId;

#[derive(Debug, Default)]
pub(super) struct BindingPermitState {
    revoked: Mutex<bool>,
    signal: CancellationToken,
}

impl BindingPermitState {
    pub(super) fn is_active(&self) -> bool {
        !*self
            .revoked
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    /// Set the stop point while a route shard is held. Notify after releasing it.
    pub(super) fn revoke_deferred(&self) {
        *self
            .revoked
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = true;
    }

    pub(super) fn notify_revoked(&self) {
        self.signal.cancel();
    }

    /// Reserve bounded control-queue capacity and revoke in one decision.
    /// The callback must not notify or invoke arbitrary user code.
    pub(super) fn revoke_on_ok<T, E>(
        &self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let mut revoked = self
            .revoked
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let result = operation();
        if result.is_ok() {
            *revoked = true;
        }
        result
    }

    /// Serialize a fallible replacement with old-binding start attempts. The
    /// replacement callback may take the new socket's gate, but must not
    /// re-enter this cell. A refused publication leaves the old cell intact.
    pub(super) fn revoke_on_success<T>(&self, replace: impl FnOnce() -> Option<T>) -> Option<T> {
        let mut revoked = self
            .revoked
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let result = replace();
        if result.is_some() {
            *revoked = true;
        }
        result
    }
}

/// Exact local binding captured from the current registry entry. This is not
/// a database, node-lease, remote-owner, or durable session authority.
#[derive(Debug, Clone)]
pub struct LocalBindingPermit {
    jid: FullJid,
    owner: LocalConnectionBindingId,
    binding: BindingRef,
    state: Arc<BindingPermitState>,
}

impl LocalBindingPermit {
    pub(super) fn new(
        jid: FullJid,
        owner: LocalConnectionBindingId,
        binding: BindingRef,
        state: Arc<BindingPermitState>,
    ) -> Self {
        Self {
            jid,
            owner,
            binding,
            state,
        }
    }

    pub(super) fn shares_state(&self, state: &Arc<BindingPermitState>) -> bool {
        Arc::ptr_eq(&self.state, state)
    }

    pub fn jid(&self) -> &FullJid {
        &self.jid
    }

    pub fn owner(&self) -> LocalConnectionBindingId {
        self.owner
    }

    pub fn binding(&self) -> &BindingRef {
        &self.binding
    }

    pub fn connection(&self) -> &ConnectionRef {
        self.binding.connection_ref()
    }

    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    pub async fn revoked(&self) {
        self.state.signal.cancelled().await;
    }

    /// Execute one synchronous socket start only if revocation has not won.
    /// The operation must not await or re-enter this permit or the registry.
    pub fn start_if_current<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let revoked = self
            .state
            .revoked
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if *revoked {
            None
        } else {
            Some(operation())
        }
    }
}
