//! Message-scoped fault injection; concurrent fixtures cannot consume a hook.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
};

use tokio::sync::Notify;
use waddle_xmpp::ingress::MessageKey;

#[derive(Default)]
struct Hooks {
    pause: Option<Arc<TerminalizationGate>>,
    recovery_freeze: Option<Arc<TerminalizationGate>>,
    timeout: AtomicBool,
    receipt_failure: Option<super::EffectReceiptKey>,
}

static HOOKS: LazyLock<Mutex<HashMap<MessageKey, Hooks>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
pub(crate) struct TerminalizationGate {
    reached: Notify,
    release: Notify,
}

impl TerminalizationGate {
    pub(crate) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

pub(crate) fn pause_before_terminalization(key: MessageKey) -> Arc<TerminalizationGate> {
    let gate = Arc::new(TerminalizationGate::default());
    HOOKS
        .lock()
        .expect("terminalization hooks")
        .entry(key)
        .or_default()
        .pause = Some(Arc::clone(&gate));
    gate
}

pub(crate) fn force_terminalization_timeout_once(key: MessageKey) {
    HOOKS
        .lock()
        .expect("terminalization hooks")
        .entry(key)
        .or_default()
        .timeout
        .store(true, Ordering::SeqCst);
}

pub(super) async fn before_terminalization(key: MessageKey) {
    let pause = HOOKS
        .lock()
        .expect("terminalization hooks")
        .get_mut(&key)
        .and_then(|hooks| hooks.pause.take());
    if let Some(gate) = pause {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
}

pub(super) fn take_terminalization_timeout(key: MessageKey) -> bool {
    let mut hooks = HOOKS.lock().expect("terminalization hooks");
    let Some(entry) = hooks.get(&key) else {
        return false;
    };
    let timeout = entry.timeout.swap(false, Ordering::SeqCst);
    if entry.pause.is_none() && entry.recovery_freeze.is_none() && entry.receipt_failure.is_none() {
        hooks.remove(&key);
    }
    timeout
}

pub(crate) fn pause_after_recovery_freeze(key: MessageKey) -> Arc<TerminalizationGate> {
    let gate = Arc::new(TerminalizationGate::default());
    HOOKS
        .lock()
        .expect("recovery hooks")
        .entry(key)
        .or_default()
        .recovery_freeze = Some(Arc::clone(&gate));
    gate
}

pub(crate) async fn after_recovery_freeze(key: MessageKey) {
    let pause = HOOKS
        .lock()
        .expect("recovery hooks")
        .get_mut(&key)
        .and_then(|hooks| hooks.recovery_freeze.take());
    if let Some(gate) = pause {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
}

/// Fail the generic receipt write after its external side effect completed.
pub(crate) fn fail_receipt_once(key: MessageKey, receipt: super::EffectReceiptKey) {
    HOOKS
        .lock()
        .expect("receipt hooks")
        .entry(key)
        .or_default()
        .receipt_failure = Some(receipt);
}

pub(super) fn take_receipt_failure(key: MessageKey, receipt: &super::EffectReceiptKey) -> bool {
    let mut hooks = HOOKS.lock().expect("receipt hooks");
    let Some(entry) = hooks.get_mut(&key) else {
        return false;
    };
    if entry.receipt_failure.as_ref() != Some(receipt) {
        return false;
    }
    entry.receipt_failure = None;
    true
}
