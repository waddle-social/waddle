//! Observe the real local IQ send future without delaying or replacing it.
//!
//! Only `test-support` builds include this module. A subprocess test opts in
//! with an observer address and two request IDs; ordinary traffic is untouched.
//! The first `Pending` poll proves a handler is awaiting capacity. Dropping
//! that still-pending future reports cancellation; a ready poll reports normal
//! completion. Datagram loss fails the test's bounded receive, never proves a
//! handler entered. No clock, channel capacity or handler deadline is changed.

use std::future::{poll_fn, Future};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::task::Poll;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use xmpp_parsers::iq::Iq;

pub const IQ_FORWARD_OBSERVER_ENV: &str = "WADDLE_TEST_IQ_FORWARD_OBSERVER";

static OBSERVATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static OBSERVATION_EPOCH: OnceLock<Instant> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
pub struct IqForwardObserverConfig {
    pub destination: SocketAddr,
    pub request_ids: [String; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IqForwardPhase {
    Pending,
    Completed,
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IqForwardObservation {
    /// Report-call order, independent of datagram delivery order. This alone
    /// does not order polls, which can be preempted before reporting.
    pub sequence: u64,
    /// Shared process-local monotonic clock. For completion this is captured
    /// before the poll that returns Ready, conservatively before completion;
    /// for Pending/cancellation it is the local observation/drop time.
    pub since_epoch: Duration,
    pub request_id: String,
    pub phase: IqForwardPhase,
    pub pending_for: Duration,
}

struct Observer {
    socket: UdpSocket,
    destination: SocketAddr,
    request_id: String,
    epoch: Instant,
    pending_since: Option<Instant>,
    completed: bool,
}

impl Observer {
    fn for_request(iq: &Iq) -> Option<Self> {
        let config = std::env::var(IQ_FORWARD_OBSERVER_ENV).ok()?;
        let config: IqForwardObserverConfig =
            serde_json::from_str(&config).expect("valid IQ forward observer configuration");
        if !config.request_ids.iter().any(|id| id == iq.id()) {
            return None;
        }
        assert!(config.destination.ip().is_loopback());
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind test observer socket");
        socket
            .set_nonblocking(true)
            .expect("nonblocking test observer socket");
        Some(Self {
            socket,
            destination: config.destination,
            request_id: iq.id().to_owned(),
            epoch: *OBSERVATION_EPOCH.get_or_init(Instant::now),
            pending_since: None,
            completed: false,
        })
    }

    fn report(&self, phase: IqForwardPhase, at: Instant) {
        let sequence = OBSERVATION_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let event = IqForwardObservation {
            sequence,
            since_epoch: at.duration_since(self.epoch),
            request_id: self.request_id.clone(),
            phase,
            pending_for: self
                .pending_since
                .map_or(Duration::ZERO, |pending| at.duration_since(pending)),
        };
        let bytes = serde_json::to_vec(&event).expect("serialize test observation");
        self.socket
            .send_to(&bytes, self.destination)
            .expect("send test observation");
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        if self.pending_since.is_some() && !self.completed {
            self.report(IqForwardPhase::Cancelled, Instant::now());
        }
    }
}

pub(crate) async fn observe_local_iq_forward<F, T>(iq: &Iq, send: F) -> T
where
    F: Future<Output = T>,
{
    let Some(mut observer) = Observer::for_request(iq) else {
        return send.await;
    };
    let mut send = std::pin::pin!(send);
    poll_fn(|cx| {
        let poll_started = Instant::now();
        match send.as_mut().poll(cx) {
            Poll::Pending => {
                if observer.pending_since.is_none() {
                    let at = Instant::now();
                    observer.pending_since = Some(at);
                    observer.report(IqForwardPhase::Pending, at);
                }
                Poll::Pending
            }
            Poll::Ready(value) => {
                observer.completed = true;
                observer.report(IqForwardPhase::Completed, poll_started);
                Poll::Ready(value)
            }
        }
    })
    .await
}
