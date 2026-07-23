//! ADR-0017 Phase 3 Slice 6: the `RelayActor`-side bridge from the
//! cross-node resume relay handler down to this node's own live
//! `ConnectionRegistry`.
//!
//! `RelayActor` is spawned by `relay::spawn_supervised` at swarm bring-up,
//! well before the WebSocket `ConnectionRegistry` exists (that is built by
//! `server/http.rs` once the rest of the server's dependency graph is
//! ready). [`ResumeStealBridge`] is constructed empty at swarm-spawn time
//! and completed once the registry exists — the exact same
//! construction-order chicken-and-egg fix
//! [`super::local_claims::SmSessionLocalClaims`] already applies for the SM
//! session registry (ADR-0017 Phase 3 Slice 5, carried debt (b)).
//!
//! **Trust-model precision (council-adjudicated, ADR-0017 Phase 3 plan
//! deviation 54)**: [`Self::request_forced_detach`]'s identity check
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

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::registry::{ConnectionRegistry, ForceDetachOutcome, ForceDetachRequest};

/// This node's local outcome of a force-detach request, before being mapped
/// onto the relay's wire reply type ([`super::relay::RelayResumeStealReply`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalForcedDetachOutcome {
    /// Identity matched: the connection sent `<conflict/>`, closed, and ran
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
pub struct ResumeStealBridge {
    connection_registry: OnceLock<Arc<ConnectionRegistry>>,
}

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
        let Some(jid) = registry.sm_stream_owner(stream_id) else {
            return LocalForcedDetachOutcome::NotLiveLocally;
        };
        let Some(entry) = registry.get_entry(&jid) else {
            return LocalForcedDetachOutcome::NotLiveLocally;
        };
        // The reverse index is best-effort (see `ConnectionRegistry::sm_stream_owner`'s
        // doc comment) — re-verify against the entry's own authoritative
        // field before acting on it.
        if entry.sm_stream_id().as_ref() != Some(stream_id) {
            return LocalForcedDetachOutcome::NotLiveLocally;
        }
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let request = ForceDetachRequest {
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
        match tokio::time::timeout(budget, ack_rx).await {
            Ok(Ok(ForceDetachOutcome::Detached)) => LocalForcedDetachOutcome::Detached,
            Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => {
                LocalForcedDetachOutcome::IdentityMismatch
            }
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
            Ok(Ok(ForceDetachOutcome::Unavailable)) => LocalForcedDetachOutcome::NotLiveLocally,
            // The connection's task died mid-flight without answering, or
            // this bridge's own bounded wait elapsed first — either way,
            // conservative: report not-live-locally so the asker re-checks
            // persistence rather than assuming a match/mismatch it never
            // observed.
            Ok(Err(_)) | Err(_) => LocalForcedDetachOutcome::NotLiveLocally,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
