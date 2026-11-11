//! Local observation of the physical transport's two owned I/O halves.

use tokio::sync::watch;

/// Read-only evidence that the local owner dropped both transport halves.
///
/// A binding's cleanup state can become detached while its physical socket
/// continues to serve another binding. This witness is independent of that
/// state. Relinquishment says nothing about a peer, a close handshake,
/// buffered writes, task completion, or cleanup of other resources.
#[derive(Debug, Clone)]
pub struct LocalIoRelinquishment {
    completion: watch::Receiver<bool>,
}

/// The observation can be lost without either asserting or denying that the
/// transport was relinquished, for example if a half's destructor panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalIoRelinquishmentOutcome {
    Relinquished,
    Unobserved,
}

impl LocalIoRelinquishment {
    /// Observe halves that another owner holds, such as transport pump
    /// tasks. The sender must publish `true` only after both half
    /// destructors have returned, and close without publishing otherwise.
    pub fn from_completion(completion: watch::Receiver<bool>) -> Self {
        Self { completion }
    }

    /// A false result includes both a live owner and an unobserved teardown.
    pub fn is_relinquished(&self) -> bool {
        *self.completion.borrow()
    }

    /// Wait for relinquishment or for the observation to become unavailable.
    pub async fn wait(&self) -> LocalIoRelinquishmentOutcome {
        let mut completion = self.completion.clone();
        let outcome = match completion.wait_for(|relinquished| *relinquished).await {
            Ok(_) => LocalIoRelinquishmentOutcome::Relinquished,
            Err(_) => LocalIoRelinquishmentOutcome::Unobserved,
        };
        outcome
    }
}

/// Owns both halves of one local transport and signals after dropping them.
///
/// The constructor must receive the physical transport's actual, exclusively
/// owned halves. The observer is meaningful only when carried alongside that
/// same connection's identity. The owner is intentionally not cloneable and
/// exposes only mutable borrows of its halves.
pub struct LocalSocketIo<S, R> {
    sender: Option<S>,
    receiver: Option<R>,
    completion: watch::Sender<bool>,
}

impl<S, R> LocalSocketIo<S, R> {
    pub fn new(sender: S, receiver: R) -> (Self, LocalIoRelinquishment) {
        let (completion, observer) = watch::channel(false);
        (
            Self {
                sender: Some(sender),
                receiver: Some(receiver),
                completion,
            },
            LocalIoRelinquishment {
                completion: observer,
            },
        )
    }

    pub fn halves_mut(&mut self) -> (&mut S, &mut R) {
        match (&mut self.sender, &mut self.receiver) {
            (Some(sender), Some(receiver)) => (sender, receiver),
            _ => unreachable!("transport halves are taken only during drop"),
        }
    }
}

impl<S, R> Drop for LocalSocketIo<S, R> {
    fn drop(&mut self) {
        // Explicitly signal only after both destructors return. If either
        // panics, the sender closes without publishing a success value.
        drop(self.sender.take());
        drop(self.receiver.take());
        self.completion.send_replace(true);
    }
}

#[cfg(test)]
mod tests;
