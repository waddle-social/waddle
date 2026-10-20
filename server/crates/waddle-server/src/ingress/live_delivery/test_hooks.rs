//! Exact, message-scoped sink gates for cancellation and ownership race tests.
use jid::FullJid;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::Notify;
use waddle_xmpp::ingress::MessageKey;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Phase {
    Claimed,
    Started,
}
type HookKey = (MessageKey, FullJid, Phase);
static GATES: LazyLock<Mutex<HashMap<HookKey, Arc<Gate>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
pub(crate) struct Gate {
    reached: Notify,
    released: Notify,
}
impl Gate {
    pub(crate) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }
    pub(crate) fn release(&self) {
        self.released.notify_one();
    }
}
fn pause(key: MessageKey, target: FullJid, phase: Phase) -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    GATES
        .lock()
        .expect("send gate")
        .insert((key, target, phase), gate.clone());
    gate
}
pub(crate) fn pause_after_claim(key: MessageKey, target: FullJid) -> Arc<Gate> {
    pause(key, target, Phase::Claimed)
}
pub(crate) fn pause_after_start(key: MessageKey, target: FullJid) -> Arc<Gate> {
    pause(key, target, Phase::Started)
}
async fn wait(key: MessageKey, target: &FullJid, phase: Phase) {
    let gate = GATES
        .lock()
        .expect("send gate")
        .remove(&(key, target.clone(), phase));
    if let Some(gate) = gate {
        gate.reached.notify_one();
        gate.released.notified().await;
    }
}
pub(super) async fn after_claim(key: MessageKey, target: &FullJid) {
    wait(key, target, Phase::Claimed).await;
}
pub(super) async fn after_start(key: MessageKey, target: &FullJid) {
    wait(key, target, Phase::Started).await;
}
