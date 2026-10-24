//! Deterministic observer controls for cross-crate ingress tests.
//!
//! Selection and grant checks still run through the real manager and actor.
//! Successful invocations continue into the real fixture Wasm component.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::types::{DisplayText, ExtensionEffect, MessageHook, PluginId};

/// Deterministic success, warning, or blocked execution for observation fixtures.
#[derive(Debug)]
pub enum ObserverTestBehavior {
    /// Continue into the real fixture component after recording the hook.
    Success,
    /// Return a deterministic host warning after recording the hook.
    Warning,
    /// Wait for the supplied notification before continuing into the real fixture component.
    Blocked(Arc<Notify>),
}

/// Shared observation fixture that records hooks and supports grant revocation.
#[derive(Debug)]
pub struct ObserverTestPlugin {
    pub(crate) id: PluginId,
    behavior: ObserverTestBehavior,
    revoked: AtomicBool,
    invocations: Mutex<Vec<MessageHook>>,
}

impl ObserverTestPlugin {
    /// Create a shared deterministic observer fixture with no recorded invocations and no
    /// revocation.
    pub fn new(id: PluginId, behavior: ObserverTestBehavior) -> Arc<Self> {
        Arc::new(Self {
            id,
            behavior,
            revoked: AtomicBool::new(false),
            invocations: Mutex::new(Vec::new()),
        })
    }

    /// Clone the message hooks recorded so far by this deterministic test plugin.
    ///
    /// # Panics
    ///
    /// Panics if an earlier panic poisoned the recorded-invocation mutex.
    pub fn invocations(&self) -> Vec<MessageHook> {
        self.invocations
            .lock()
            .expect("observer invocations")
            .clone()
    }

    /// Make subsequent observation grant checks fail for this test plugin.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    pub(crate) async fn invoke(&self, hook: MessageHook) -> Option<Vec<ExtensionEffect>> {
        self.invocations
            .lock()
            .expect("observer invocations")
            .push(hook);
        match &self.behavior {
            ObserverTestBehavior::Success => None,
            ObserverTestBehavior::Warning => Some(vec![ExtensionEffect::HostWarning(
                DisplayText::new("observer fixture failed").expect("warning"),
            )]),
            ObserverTestBehavior::Blocked(release) => {
                release.notified().await;
                None
            }
        }
    }
}
