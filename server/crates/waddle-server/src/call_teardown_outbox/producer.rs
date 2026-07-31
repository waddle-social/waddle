//! Single-supervisor retry handoff for producer-side database outages.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::runtime::Handle;

use super::{CallTeardownIntent, CallTeardownOutboxStore};

#[derive(Default)]
struct RetryState {
    running: bool,
    pending: HashSet<Vec<CallTeardownIntent>>,
}

/// Coalesces related teardown batches behind one retry task. Producers use
/// this only after their direct atomic insert fails, so the normal path has no
/// queueing or task handoff.
#[derive(Clone)]
pub(crate) struct CallTeardownPersistenceSupervisor {
    store: Arc<CallTeardownOutboxStore>,
    runtime: Handle,
    state: Arc<Mutex<RetryState>>,
}

impl CallTeardownPersistenceSupervisor {
    pub(crate) fn new(store: Arc<CallTeardownOutboxStore>, runtime: Handle) -> Self {
        Self {
            store,
            runtime,
            state: Arc::new(Mutex::new(RetryState::default())),
        }
    }

    pub(crate) fn retry_batch(&self, intents: Vec<CallTeardownIntent>) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending.insert(intents);
            if state.running {
                return;
            }
            state.running = true;
        }

        let store = Arc::clone(&self.store);
        let state = Arc::clone(&self.state);
        self.runtime.spawn(async move {
            loop {
                let batches = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.pending.drain().collect::<Vec<_>>()
                };
                for batch in batches {
                    persist_with_retry(&store, &batch).await;
                }
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.pending.is_empty() {
                    state.running = false;
                    break;
                }
            }
        });
    }

    #[cfg(test)]
    pub(super) fn state_snapshot(&self) -> (bool, usize) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.running, state.pending.len())
    }
}

async fn persist_with_retry(store: &CallTeardownOutboxStore, intents: &[CallTeardownIntent]) {
    let mut retry_delay = Duration::from_secs(5);
    loop {
        match store.enqueue_batch(intents).await {
            Ok(_) => break,
            Err(error) => {
                tracing::warn!(
                    %error,
                    retry_delay_ms = retry_delay.as_millis(),
                    "call teardown producer persistence retry failed"
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay
                    .saturating_mul(2)
                    .min(Duration::from_secs(10 * 60));
            }
        }
    }
}
