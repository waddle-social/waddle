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
    timeout: AtomicBool,
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
    if entry.pause.is_none() {
        hooks.remove(&key);
    }
    timeout
}
