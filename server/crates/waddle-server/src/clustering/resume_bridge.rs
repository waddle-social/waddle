//! ADR-0017 Phase 3 Slice 6: the `RelayActor`-side bridge from the
//! cross-node resume relay handler down to this node's own live
//! `ConnectionRegistry`.
//!
//! `RelayActor` is spawned by `relay::spawn_supervised` at swarm bring-up,
//! well before the WebSocket `ConnectionRegistry` exists (that is built by
//! `server/http.rs` once the rest of the server's dependency graph is
//! ready). `ResumeStealBridge` is constructed empty at swarm-spawn time
//! and completed once the registry exists — the exact same
//! construction-order chicken-and-egg fix
//! `SmSessionLocalClaims` already applies for the SM
//! session registry (ADR-0017 Phase 3 Slice 5, carried debt (b)).
//!
//! **Trust-model precision (council-adjudicated, ADR-0017 Phase 3 plan
//! deviation 54)**: the force-detach identity check
//! (`requester_bare_jid` against the live connection's own bound JID) is a
//! real defense against a malicious/buggy client — or an honest node's own
//! bug — asking to force-detach a session that is not its own. It is **not**
//! a defense against a compromised allowlisted node: a `RelayResumeSteal`
//! ask's `requester_bare_jid` is whatever the sending node places on the
//! wire, and this bridge has no way to verify that field against anything
//! the sender's own client actually SASL-authenticated as. This is
//! consistent with the ADR's enrolled-node-fully-trusted cluster membership
//! model (every other cross-node ask in this phase makes the identical
//! trust assumption), not a gap specific to this bridge.

#[cfg(any(test, feature = "clustering"))]
use std::sync::{Arc, OnceLock};
#[cfg(any(test, feature = "clustering"))]
use std::time::Duration;

use jid::{BareJid, FullJid};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::registry::ConnectionRegistry;
#[cfg(any(test, feature = "clustering"))]
use waddle_xmpp::registry::{ForceDetachOutcome, ForceDetachRequest};

#[derive(Debug)]
pub(crate) struct LocalResumeRejected {
    pub jid: FullJid,
    pub reason: waddle_xmpp::stream_management::SmResumeRejection,
}

/// Capture one local owner and let that owner's task validate `h` before
/// interrupting I/O. After enqueue, wait through its cleanup acknowledgment:
/// a handshake timer cannot turn an accepted detach into a premature miss.
pub(crate) async fn request_validated_local_resume(
    registry: &ConnectionRegistry,
    stream_id: &SmSessionId,
    requester_bare_jid: &BareJid,
    h: u32,
) -> Result<LocalForcedDetachOutcome, LocalResumeRejected> {
    use waddle_xmpp::registry::LocalResumeReply;
    let Some(jid) = registry.sm_stream_owner(stream_id) else {
        return Ok(LocalForcedDetachOutcome::NotLiveLocally);
    };
    let Some(entry) = registry.get_entry(&jid) else {
        return Ok(LocalForcedDetachOutcome::NotLiveLocally);
    };
    if !entry.is_locally_hosted() || entry.sm_stream_id().as_ref() != Some(stream_id) {
        return Ok(LocalForcedDetachOutcome::NotLiveLocally);
    }
    if jid.to_bare() != *requester_bare_jid {
        return Ok(LocalForcedDetachOutcome::IdentityMismatch);
    }
    let Some(control) = entry.resume_control() else {
        return Ok(LocalForcedDetachOutcome::NotLiveLocally);
    };
    match control
        .request(stream_id.clone(), requester_bare_jid.clone(), h)
        .await
    {
        LocalResumeReply::Detached => Ok(LocalForcedDetachOutcome::Detached),
        LocalResumeReply::IdentityMismatch => Ok(LocalForcedDetachOutcome::IdentityMismatch),
        LocalResumeReply::NotPersisted => Ok(LocalForcedDetachOutcome::NotLiveLocally),
        LocalResumeReply::Rejected(reason) => Err(LocalResumeRejected { jid, reason }),
    }
}

/// This node's local outcome of a force-detach request, before being mapped
/// onto the relay's wire reply type (`RelayResumeStealReply`). Also used
/// directly by same-node resume without requiring clustering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalForcedDetachOutcome {
    /// Identity matched: the connection attempted `<conflict/>`, closed, and ran
    /// its normal XEP-0198 detach-for-resume cleanup — a persisted snapshot
    /// should now be readable.
    Detached,
    /// The requester's bare JID did not match the live connection's own
    /// bound JID.
    IdentityMismatch,
    /// No live local connection is currently publishing this stream id (the
    /// registry isn't wired yet, the reverse-index lookup missed, the
    /// looked-up entry no longer carries this stream id, the connection's
    /// own force-detach channel is already closed, or the connection did
    /// not answer within `budget`). The asking node should re-check
    /// persistence and retry.
    NotLiveLocally,
}

/// The bridge itself. See the module doc for the construction-order
/// rationale.
#[cfg(any(test, feature = "clustering"))]
pub struct ResumeStealBridge {
    connection_registry: OnceLock<Arc<ConnectionRegistry>>,
}

#[cfg(any(test, feature = "clustering"))]
impl ResumeStealBridge {
    /// Construct an empty bridge, before the `ConnectionRegistry` exists.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            connection_registry: OnceLock::new(),
        })
    }

    /// Complete the bridge once the `ConnectionRegistry` exists. Idempotent
    /// no-op (logged) if called more than once — mirrors
    /// `SmSessionLocalClaims::wire`'s exact contract.
    pub fn wire(&self, registry: Arc<ConnectionRegistry>) {
        if self.connection_registry.set(registry).is_err() {
            tracing::error!(
                "ResumeStealBridge::wire called more than once; the connection registry \
                 handle was already wired (ignoring this call)"
            );
        }
    }

    /// Ask this node's own live connection registry to force-detach
    /// `stream_id` on behalf of `requester_bare_jid`, bounded by `budget`.
    /// See [`LocalForcedDetachOutcome`] for the outcome shape.
    pub async fn request_forced_detach(
        &self,
        stream_id: &SmSessionId,
        requester_bare_jid: &BareJid,
        budget: Duration,
    ) -> LocalForcedDetachOutcome {
        let Some(registry) = self.connection_registry.get() else {
            return LocalForcedDetachOutcome::NotLiveLocally;
        };
        request_local_forced_detach(registry, stream_id, requester_bare_jid, budget).await
    }
}

/// Target one captured connection generation, never a successor publishing the
/// same SM ID. A timeout is inconclusive: callers must still claim the detached
/// snapshot and pass the socket-cleanup barrier before serving the replacement.
#[cfg(any(test, feature = "clustering"))]
pub(crate) async fn request_local_forced_detach(
    registry: &ConnectionRegistry,
    stream_id: &SmSessionId,
    requester_bare_jid: &BareJid,
    budget: Duration,
) -> LocalForcedDetachOutcome {
    let Some(jid) = registry.sm_stream_owner(stream_id) else {
        return LocalForcedDetachOutcome::NotLiveLocally;
    };
    let Some(entry) = registry.get_entry(&jid) else {
        return LocalForcedDetachOutcome::NotLiveLocally;
    };
    // The reverse index is best-effort (see `ConnectionRegistry::sm_stream_owner`'s
    // doc comment) — re-verify against the entry's own authoritative
    // field before acting on it.
    if !entry.is_locally_hosted() || entry.sm_stream_id().as_ref() != Some(stream_id) {
        return LocalForcedDetachOutcome::NotLiveLocally;
    }
    if jid.to_bare() != *requester_bare_jid {
        return LocalForcedDetachOutcome::IdentityMismatch;
    }
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let request = ForceDetachRequest {
        origin: waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
        requester_bare_jid: requester_bare_jid.clone(),
        ack: ack_tx,
    };
    // Council-adjudicated FIX 5: `try_send`, not a blocking
    // `send().await` — a full (capacity `FORCE_DETACH_CHANNEL_CAPACITY`,
    // see that field's own doc comment) or already-closed channel
    // answers `NotLiveLocally` immediately rather than waiting on
    // capacity that may never free up, keeping this call's own budget
    // meaningful (a blocking send has no bound of its own).
    if let Err(error) = entry.force_detach_sender().try_send(request) {
        match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                tracing::debug!(
                    "cross-node resume force-detach: connection's force-detach channel is \
                         full; reporting not-live-locally so the asker retries"
                );
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                // The connection's own task already exited (e.g. it
                // detached on its own, or died) — the force-detach
                // channel is gone.
            }
        }
        return LocalForcedDetachOutcome::NotLiveLocally;
    }
    // Interrupt only this captured generation, after both authoritative
    // identity checks and successful enqueue. A replacement has its own token.
    entry.write_interruption.cancel();
    match tokio::time::timeout(budget, ack_rx).await {
        Ok(Ok(ForceDetachOutcome::Detached)) => LocalForcedDetachOutcome::Detached,
        Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => LocalForcedDetachOutcome::IdentityMismatch,
        // Council-adjudicated FIX 4: identity matched and the
        // connection closed, but its own detach-for-resume cleanup did
        // NOT end in a persisted snapshot (storage-error fallback,
        // ownership-race promotion, or any other non-detach path in
        // `cleanup_connection_shutdown`) — the asker must not proceed
        // with `steal_for_resume` against a snapshot that was never
        // written. Ack only what actually happened: treat this
        // identically to not-live-locally so the asker re-checks
        // persistence and retries.
        Ok(Ok(ForceDetachOutcome::NotPersisted)) => LocalForcedDetachOutcome::NotLiveLocally,
        // The connection's task died mid-flight without answering, or
        // this bridge's own bounded wait elapsed first — either way,
        // conservative: report not-live-locally so the asker re-checks
        // persistence rather than assuming a match/mismatch it never
        // observed.
        Ok(Err(_)) | Err(_) => LocalForcedDetachOutcome::NotLiveLocally,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::FullJid;
    use tokio::sync::mpsc;
    use waddle_xmpp::registry::ConnectionEntry;

    fn register_resumable_connection(
        registry: &ConnectionRegistry,
        jid: &FullJid,
        stream_id: &SmSessionId,
    ) -> mpsc::Receiver<ForceDetachRequest> {
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new(sender);
        let requests = entry
            .take_force_detach_rx()
            .expect("new connection receiver");
        registry.register_entry(jid.clone(), entry);
        registry.set_sm_stream_id(jid, Some(stream_id.clone()));
        requests
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_local_detaches_keep_targeting_the_captured_connection() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/device".parse().expect("valid jid");
        let requester = jid.to_bare();
        let old_stream_id = SmSessionId::new("old-stream");
        let replacement_stream_id = old_stream_id.clone();
        let mut old_requests = register_resumable_connection(&registry, &jid, &old_stream_id);

        // Both contenders must have selected the old connection before its owner
        // finishes detaching. Installing the replacement before acknowledging
        // either request makes any subsequent full-JID lookup unsafe.
        let owner = async {
            let first = tokio::time::timeout(Duration::from_secs(1), old_requests.recv())
                .await
                .expect("first request arrives before timeout")
                .expect("first detach request");
            let second = tokio::time::timeout(Duration::from_secs(1), old_requests.recv())
                .await
                .expect("second request arrives before timeout")
                .expect("second detach request");
            assert_eq!(first.requester_bare_jid, requester);
            assert_eq!(second.requester_bare_jid, requester);
            let replacement_requests =
                register_resumable_connection(&registry, &jid, &replacement_stream_id);
            first
                .ack
                .send(ForceDetachOutcome::Detached)
                .expect("first requester waiting");
            second
                .ack
                .send(ForceDetachOutcome::Detached)
                .expect("second requester waiting");
            replacement_requests
        };
        let (first, second, mut replacement_requests) = tokio::join!(
            request_local_forced_detach(
                &registry,
                &old_stream_id,
                &requester,
                Duration::from_secs(1)
            ),
            request_local_forced_detach(
                &registry,
                &old_stream_id,
                &requester,
                Duration::from_secs(1)
            ),
            owner,
        );

        assert_eq!(first, LocalForcedDetachOutcome::Detached);
        assert_eq!(second, LocalForcedDetachOutcome::Detached);
        assert!(matches!(
            replacement_requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            registry
                .get_entry(&jid)
                .expect("replacement registered")
                .sm_stream_id(),
            Some(replacement_stream_id)
        );
    }

    #[tokio::test]
    async fn local_detach_rejects_another_identity_without_signaling_the_owner() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/device".parse().expect("valid jid");
        let requester: BareJid = "mallory@example.com".parse().expect("valid jid");
        let stream_id = SmSessionId::new("alice-stream");
        let mut requests = register_resumable_connection(&registry, &jid, &stream_id);

        assert_eq!(
            request_local_forced_detach(&registry, &stream_id, &requester, Duration::from_secs(1))
                .await,
            LocalForcedDetachOutcome::IdentityMismatch
        );
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn local_detach_wait_is_bounded_when_the_owner_does_not_acknowledge() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/device".parse().expect("valid jid");
        let stream_id = SmSessionId::new("unresponsive-stream");
        let mut requests = register_resumable_connection(&registry, &jid, &stream_id);
        let budget = Duration::from_millis(50);
        let started = tokio::time::Instant::now();

        assert_eq!(
            request_local_forced_detach(&registry, &stream_id, &jid.to_bare(), budget).await,
            LocalForcedDetachOutcome::NotLiveLocally
        );
        assert_eq!(started.elapsed(), budget);
        assert!(requests
            .try_recv()
            .expect("request sent before timeout")
            .ack
            .is_closed());
    }

    #[tokio::test]
    async fn local_detach_ignores_a_stale_stream_reverse_index() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/device".parse().expect("valid jid");
        let stale_stream_id = SmSessionId::new("stale-stream");
        let mut requests = register_resumable_connection(&registry, &jid, &stale_stream_id);
        // Mutate the entry directly to simulate the documented best-effort
        // reverse index lagging behind the authoritative connection state.
        registry
            .get_entry(&jid)
            .expect("connection registered")
            .set_sm_stream_id(Some(SmSessionId::new("current-stream")));
        assert_eq!(
            registry.sm_stream_owner(&stale_stream_id),
            Some(jid.clone())
        );

        assert_eq!(
            request_local_forced_detach(
                &registry,
                &stale_stream_id,
                &jid.to_bare(),
                Duration::from_secs(1),
            )
            .await,
            LocalForcedDetachOutcome::NotLiveLocally
        );
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn unwired_bridge_reports_not_live_locally() {
        let bridge = ResumeStealBridge::new();
        let stream_id = SmSessionId::new("stream-1");
        let jid: BareJid = "alice@example.com".parse().expect("valid jid");
        let outcome = bridge
            .request_forced_detach(&stream_id, &jid, Duration::from_millis(50))
            .await;
        assert_eq!(outcome, LocalForcedDetachOutcome::NotLiveLocally);
    }

    #[tokio::test]
    async fn wired_bridge_with_no_matching_connection_reports_not_live_locally() {
        let bridge = ResumeStealBridge::new();
        bridge.wire(Arc::new(ConnectionRegistry::new()));
        let stream_id = SmSessionId::new("stream-1");
        let jid: BareJid = "alice@example.com".parse().expect("valid jid");
        let outcome = bridge
            .request_forced_detach(&stream_id, &jid, Duration::from_millis(50))
            .await;
        assert_eq!(outcome, LocalForcedDetachOutcome::NotLiveLocally);
    }
}
