//! The cross-node ask that moves a live stream-management session off its
//! owning node.
//!
//! A resume or a claim move that finds the session live on another node asks
//! that node, through [`RemoteResumeAsker`], to force-detach it. The owning
//! node checks the requester's identity before it closes anything. The
//! durable authority then moves the session under its own claim; this
//! registry keeps only the asker that `waddle-server` wires in.

use jid::BareJid;

/// Ask a remote node (identified by its `node_id`) to release a live SM
/// session for cross-node resume (element 8's "live, owned elsewhere"
/// branch — XEP-0198's "Resumption" section `<conflict/>`-close SHOULD).
///
/// Implemented in `waddle-server` via `RelayHandle` (the swarm's cross-node
/// ask); this trait keeps `waddle-xmpp` free of any clustering/swarm/libp2p
/// dependency — the same injection pattern [`crate::ownership::ClaimStore`]
/// already uses for the Postgres CAS implementation.
#[async_trait::async_trait]
pub trait RemoteResumeAsker: Send + Sync {
    /// Ask node `node_id` to force-detach its live SM session `stream_id`
    /// on behalf of `requester_bare_jid`. The remote node performs its own
    /// defense-in-depth identity check before doing anything destructive
    /// (ADR-0017 Phase 3 plan, Slice 6: "the identity check gates the
    /// destructive close itself, not just the subsequent CAS").
    async fn ask_remote_detach(
        &self,
        node_id: &str,
        stream_id: &str,
        requester_bare_jid: &BareJid,
    ) -> RemoteResumeAskOutcome;
}

/// Typed outcome of [`RemoteResumeAsker::ask_remote_detach`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteResumeAskOutcome {
    /// The remote node force-detached (or already had no live connection
    /// and the session was already persisted) — a persisted snapshot
    /// should now be readable.
    Detached,
    /// The remote node's own identity check rejected `requester_bare_jid`.
    IdentityMismatch,
    /// The remote node reports no live local session under this
    /// `stream_id` (a race with a concurrent detach/expiry/promotion) —
    /// caller should re-check local persistence and retry.
    NotLiveRemotely,
    /// The ask itself failed at the transport layer (unreachable, timed
    /// out, ...) — the owner-unreachable branch.
    Unreachable,
}
