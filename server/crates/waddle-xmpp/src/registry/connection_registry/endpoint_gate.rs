use crate::ownership::BindingRef;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointGateState {
    #[cfg(test)]
    Pending,
    Active,
    #[cfg(test)]
    Sealed,
}

/// Synchronous admission fence for a single binding's local endpoint.
///
/// The gate lock is held only while checking identity/state and performing a
/// non-blocking final enqueue. Callers must not await from the closure.
#[derive(Debug)]
pub(super) struct EndpointGate {
    binding: BindingRef,
    state: Mutex<EndpointGateState>,
    sealed: tokio_util::sync::CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndpointGateError {
    WrongBinding,
    #[cfg(test)]
    NotActive,
    Sealed,
    Poisoned,
    ChannelClosed,
}

impl EndpointGate {
    pub(super) fn active(binding: BindingRef) -> Self {
        Self {
            binding,
            state: Mutex::new(EndpointGateState::Active),
            sealed: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[cfg(test)]
    fn pending(binding: BindingRef) -> Self {
        Self {
            binding,
            state: Mutex::new(EndpointGateState::Pending),
            sealed: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[cfg(test)]
    fn activate(&self, binding: &BindingRef) -> Result<(), EndpointGateError> {
        if binding != &self.binding {
            return Err(EndpointGateError::WrongBinding);
        }
        let mut state = self.state.lock().map_err(|_| EndpointGateError::Poisoned)?;
        match *state {
            EndpointGateState::Pending => {
                *state = EndpointGateState::Active;
                Ok(())
            }
            EndpointGateState::Active => Err(EndpointGateError::NotActive),
            EndpointGateState::Sealed => Err(EndpointGateError::Sealed),
        }
    }

    /// Seal this exact binding. Sealing is terminal; a stale request cannot
    /// affect a successor and an old binding cannot be reactivated.
    #[cfg(test)]
    fn seal(&self, binding: &BindingRef) -> Result<bool, EndpointGateError> {
        if binding != &self.binding {
            return Err(EndpointGateError::WrongBinding);
        }
        let mut state = self.state.lock().map_err(|_| EndpointGateError::Poisoned)?;
        if *state == EndpointGateState::Sealed {
            return Ok(false);
        }
        *state = EndpointGateState::Sealed;
        self.sealed.cancel();
        Ok(true)
    }

    pub(super) fn with_active<R>(
        &self,
        binding: &BindingRef,
        enqueue: impl FnOnce() -> R,
    ) -> Result<R, EndpointGateError> {
        if binding != &self.binding {
            return Err(EndpointGateError::WrongBinding);
        }
        let state = self.state.lock().map_err(|_| EndpointGateError::Poisoned)?;
        match *state {
            EndpointGateState::Active => Ok(enqueue()),
            #[cfg(test)]
            EndpointGateState::Pending => Err(EndpointGateError::NotActive),
            #[cfg(test)]
            EndpointGateState::Sealed => Err(EndpointGateError::Sealed),
        }
    }

    pub(super) fn sealed(&self) -> &tokio_util::sync::CancellationToken {
        &self.sealed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{ConnectionRef, NodeIdentity};
    use std::sync::Arc;
    use std::thread;

    fn binding() -> BindingRef {
        ConnectionRef::new(NodeIdentity::new("node", "epoch"))
            .next_binding_ref()
            .expect("fresh binding generation")
    }

    #[test]
    fn activation_seal_and_final_enqueue_share_one_ordering_domain() {
        let binding = binding();
        let gate = Arc::new(EndpointGate::pending(binding.clone()));
        let (enqueue_entered_tx, enqueue_entered_rx) = std::sync::mpsc::channel();
        let (release_enqueue_tx, release_enqueue_rx) = std::sync::mpsc::channel();
        let (seal_started_tx, seal_started_rx) = std::sync::mpsc::channel();
        let (seal_done_tx, seal_done_rx) = std::sync::mpsc::channel();

        gate.activate(&binding).expect("pending binding activates");
        let enqueue = {
            let gate = Arc::clone(&gate);
            let binding = binding.clone();
            thread::spawn(move || {
                gate.with_active(&binding, || {
                    enqueue_entered_tx.send(()).expect("report enqueue entry");
                    release_enqueue_rx.recv().expect("release final enqueue");
                })
            })
        };
        enqueue_entered_rx
            .recv()
            .expect("enqueue holds gate at final send");

        let sealing = {
            let gate = Arc::clone(&gate);
            let binding = binding.clone();
            thread::spawn(move || {
                seal_started_tx.send(()).expect("report seal attempt");
                let result = gate.seal(&binding);
                seal_done_tx.send(result).expect("report seal result");
            })
        };
        seal_started_rx.recv().expect("seal thread started");
        assert!(seal_done_rx.try_recv().is_err());

        release_enqueue_tx
            .send(())
            .expect("release final enqueue closure");
        assert!(enqueue.join().expect("enqueue thread").is_ok());
        assert_eq!(seal_done_rx.recv().expect("seal result"), Ok(true));
        sealing.join().expect("seal thread");
        assert_eq!(gate.activate(&binding), Err(EndpointGateError::Sealed));
        assert_eq!(
            gate.with_active(&binding, || ()),
            Err(EndpointGateError::Sealed)
        );
    }

    #[test]
    fn stale_force_detach_binding_cannot_seal_or_enqueue_on_successor_gate() {
        let connection = ConnectionRef::new(NodeIdentity::new("node", "epoch"));
        let stale = connection.next_binding_ref().expect("first binding");
        let current = connection.next_binding_ref().expect("successor binding");
        let gate = EndpointGate::pending(current.clone());

        assert_eq!(gate.seal(&stale), Err(EndpointGateError::WrongBinding));
        gate.activate(&current).expect("activate current binding");
        let mut admitted = false;
        gate.with_active(&current, || admitted = true)
            .expect("current binding admits");
        assert!(admitted);
        assert_eq!(
            gate.with_active(&stale, || ()),
            Err(EndpointGateError::WrongBinding)
        );
    }
}
