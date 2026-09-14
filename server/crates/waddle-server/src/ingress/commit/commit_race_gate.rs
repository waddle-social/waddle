//! Origin-scoped test gates before and after durable admission.
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::Semaphore;
use waddle_xmpp_core::xep0359::OriginId;

static GATES: LazyLock<Mutex<HashMap<OriginId, Arc<Gate>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    BeforeAdmission,
    AfterAdmission,
}

struct Gate {
    phase: Phase,
    entered: Semaphore,
    release: Semaphore,
}

pub(crate) struct Registration {
    origin: OriginId,
    gate: Arc<Gate>,
}

impl Registration {
    pub(crate) fn new(origin: OriginId) -> Self {
        Self::at_phase(origin, Phase::AfterAdmission)
    }

    pub(crate) fn before_admission(origin: OriginId) -> Self {
        Self::at_phase(origin, Phase::BeforeAdmission)
    }

    fn at_phase(origin: OriginId, phase: Phase) -> Self {
        let gate = Arc::new(Gate {
            phase,
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        assert!(GATES
            .lock()
            .expect("gate registry")
            .insert(origin.clone(), Arc::clone(&gate))
            .is_none());
        Self { origin, gate }
    }

    pub(crate) async fn entered(&self) {
        self.gate
            .entered
            .acquire()
            .await
            .expect("entry gate")
            .forget();
    }

    pub(crate) fn release(&self) {
        self.gate.release.add_permits(1);
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        GATES.lock().expect("gate registry").remove(&self.origin);
        self.release();
    }
}

pub(super) async fn after_admission(origin: Option<&OriginId>) {
    wait_at_phase(origin, Phase::AfterAdmission).await;
}

pub(super) async fn before_admission(origin: Option<&OriginId>) {
    wait_at_phase(origin, Phase::BeforeAdmission).await;
}

async fn wait_at_phase(origin: Option<&OriginId>, phase: Phase) {
    let gate = origin.and_then(|origin| {
        let mut gates = GATES.lock().expect("gate registry");
        if gates.get(origin).is_some_and(|gate| gate.phase == phase) {
            gates.remove(origin)
        } else {
            None
        }
    });
    if let Some(gate) = gate {
        gate.entered.add_permits(1);
        gate.release.acquire().await.expect("release gate").forget();
    }
}
