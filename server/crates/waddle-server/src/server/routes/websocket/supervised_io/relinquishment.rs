//! Evidence from dropping the factory-supplied halves owned by pump futures.

use super::{custodian_state_is_terminal, CustodianState};
use futures::{Sink, Stream};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::watch;
use waddle_xmpp::ownership::ConnectionRef;
use waddle_xmpp::registry::LocalIoRelinquishmentOutcome;

const READER_DROPPED: u8 = 1;
const WRITER_DROPPED: u8 = 2;
const BOTH_DROPPED: u8 = READER_DROPPED | WRITER_DROPPED;

/// Observation of both pump-owned halves supplied for one connection reference.
///
/// Relinquishment does not assert task completion, a close handshake, peer
/// behavior, or delivery of buffered writes. The generic factory does not
/// itself prove exclusive ownership of a physical WebSocket; that is a later
/// connection-owner contract. An unavailable or still-starting reservation
/// has no observer until its factory has returned both halves.
#[derive(Clone, Debug)]
pub struct SplitIoRelinquishment {
    connection_ref: ConnectionRef,
    completion: watch::Receiver<bool>,
    state: watch::Receiver<CustodianState>,
}

impl SplitIoRelinquishment {
    pub(super) fn new(
        connection_ref: ConnectionRef,
        completion: watch::Receiver<bool>,
        state: watch::Receiver<CustodianState>,
    ) -> Self {
        Self {
            connection_ref,
            completion,
            state,
        }
    }

    /// The connection reference associated with these factory-supplied halves.
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }

    /// Whether this witness is associated with the supplied exact reference.
    pub fn is_for(&self, connection_ref: &ConnectionRef) -> bool {
        &self.connection_ref == connection_ref
    }

    /// The current result, or `None` while the observation is pending.
    ///
    /// A successful result follows both half destructors. An unobserved
    /// result additionally requires terminal startup and pump-join proof;
    /// a lost reaper or a noncooperative pump cannot establish it.
    pub fn outcome(&self) -> Option<LocalIoRelinquishmentOutcome> {
        if *self.completion.borrow() {
            return Some(LocalIoRelinquishmentOutcome::Relinquished);
        }
        if custodian_state_is_terminal(*self.state.borrow())
            && self.completion.has_changed().is_err()
        {
            // Read again after channel closure so a concurrent final marker
            // cannot be mistaken for an unobserved teardown.
            return Some(if *self.completion.borrow() {
                LocalIoRelinquishmentOutcome::Relinquished
            } else {
                LocalIoRelinquishmentOutcome::Unobserved
            });
        }
        None
    }

    /// Wait without controlling either pump or the independent custodian.
    pub async fn wait(&self) -> LocalIoRelinquishmentOutcome {
        let mut observer = self.clone();
        let mut completion_open = true;
        let mut state_open = true;
        loop {
            if let Some(outcome) = observer.outcome() {
                return outcome;
            }
            tokio::select! {
                changed = observer.completion.changed(), if completion_open => {
                    completion_open = changed.is_ok();
                }
                changed = observer.state.changed(), if state_open => {
                    state_open = changed.is_ok();
                }
                else => std::future::pending::<()>().await,
            }
        }
    }
}

/// A non-cloneable owner of one factory-supplied half. It may only be polled or dropped.
pub(super) struct TrackedHalf<T> {
    half: Option<T>,
    marker: u8,
    dropped: Arc<AtomicU8>,
    completion: watch::Sender<bool>,
}

pub(super) fn track_halves<R, W>(
    reader: R,
    writer: W,
) -> (TrackedHalf<R>, TrackedHalf<W>, watch::Receiver<bool>) {
    let (completion, observer) = watch::channel(false);
    let dropped = Arc::new(AtomicU8::new(0));
    (
        TrackedHalf {
            half: Some(reader),
            marker: READER_DROPPED,
            dropped: dropped.clone(),
            completion: completion.clone(),
        },
        TrackedHalf {
            half: Some(writer),
            marker: WRITER_DROPPED,
            dropped,
            completion,
        },
        observer,
    )
}

impl<T> Drop for TrackedHalf<T> {
    fn drop(&mut self) {
        // A panicking underlying destructor must never publish this half's
        // marker. Its sender still drops during unwinding.
        drop(self.half.take());
        let previous = self.dropped.fetch_or(self.marker, Ordering::AcqRel);
        if previous | self.marker == BOTH_DROPPED {
            self.completion.send_replace(true);
        }
    }
}

impl<R: Stream + Unpin> Stream for TrackedHalf<R> {
    type Item = R::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let reader = self
            .get_mut()
            .half
            .as_mut()
            .expect("half exists until drop");
        Pin::new(reader).poll_next(cx)
    }
}

impl<W: Sink<M> + Unpin, M> Sink<M> for TrackedHalf<W> {
    type Error = W::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let writer = self
            .get_mut()
            .half
            .as_mut()
            .expect("half exists until drop");
        Pin::new(writer).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: M) -> Result<(), Self::Error> {
        let writer = self
            .get_mut()
            .half
            .as_mut()
            .expect("half exists until drop");
        Pin::new(writer).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let writer = self
            .get_mut()
            .half
            .as_mut()
            .expect("half exists until drop");
        Pin::new(writer).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let writer = self
            .get_mut()
            .half
            .as_mut()
            .expect("half exists until drop");
        Pin::new(writer).poll_close(cx)
    }
}
