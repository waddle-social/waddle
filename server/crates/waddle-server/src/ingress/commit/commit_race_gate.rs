//! Origin-scoped test gate while Phase B owns the durable admission locks.
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::Semaphore;
use waddle_xmpp_core::xep0359::OriginId;

static GATES: LazyLock<Mutex<HashMap<OriginId, Arc<Gate>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct Gate {
    entered: Semaphore,
    release: Semaphore,
}

pub(crate) struct Registration {
    origin: OriginId,
    gate: Arc<Gate>,
}

impl Registration {
    pub(crate) fn new(origin: OriginId) -> Self {
        let gate = Arc::new(Gate {
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
    let gate = origin.and_then(|origin| GATES.lock().expect("gate registry").remove(origin));
    if let Some(gate) = gate {
        gate.entered.add_permits(1);
        gate.release.acquire().await.expect("release gate").forget();
    }
}
