//! Deterministic observer controls for cross-crate ingress tests.
//!
//! Selection and grant checks still run through the real manager and actor.
//! Successful invocations continue into the real fixture Wasm component.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::types::{DisplayText, ExtensionEffect, MessageHook, PluginId};

#[derive(Debug)]
pub enum ObserverTestBehavior {
    Success,
    Warning,
    Blocked(Arc<Notify>),
}

#[derive(Debug)]
pub struct ObserverTestPlugin {
    pub(crate) id: PluginId,
    behavior: ObserverTestBehavior,
    revoked: AtomicBool,
    invocations: Mutex<Vec<MessageHook>>,
}

impl ObserverTestPlugin {
    pub fn new(id: PluginId, behavior: ObserverTestBehavior) -> Arc<Self> {
        Arc::new(Self {
            id,
            behavior,
            revoked: AtomicBool::new(false),
            invocations: Mutex::new(Vec::new()),
        })
    }

    pub fn invocations(&self) -> Vec<MessageHook> {
        self.invocations
            .lock()
            .expect("observer invocations")
            .clone()
    }

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
