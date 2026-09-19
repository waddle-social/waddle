//! Keying a detach-drained frame with its origin's ingress obligation (issue #1789).
//!
//! A frame relayed to a registered socket is queued with the origin's obligation,
//! unverified. If the socket detaches before writing it, the drain appends it to the
//! XEP-0198 replay queue; without the obligation, a recovery re-execution of the same
//! obligation finds no `sm_ingress_appends` proof and queues the stanza again.

use super::replay::DrainedAppend;
use super::*;
use waddle_xmpp::stream_management::{
    SmDrainedAppendTicket, SmIngressAppendKey, SmKeyedAppendOutcome, SmRelayedAppendObligation,
};

/// Authorize the relayed claim against canonical ingress state.
///
/// Failure degrades to an unkeyed drain (decision recorded on #1789): the origin was
/// already told `Delivered`, so refusing the entry would turn a canonical-read outage
/// into silent loss. At-least-once survives; only the dedupe key is lost.
pub(super) async fn authorize(
    state: &WebSocketState,
    stanza: &Stanza,
    obligation: Option<SmRelayedAppendObligation>,
) -> Option<SmRelayedAppendObligation> {
    let obligation = obligation?;
    match crate::ingress::append_authority::check_canonical_authority(
        state.deps.app_state.db_pool.global(),
        stanza,
        obligation.key.message_key,
        &obligation.sender_bare,
        obligation.key.kind.to_storage(),
    )
    .await
    {
        Ok(()) => Some(obligation),
        Err(reason) => {
            crate::ingress::append_authority::record_degraded_to_unkeyed(
                &reason,
                &obligation.sender_bare,
            );
            None
        }
    }
}

pub(super) enum Claim {
    /// The obligation already holds a deliverable allocation: drop the frame uncounted.
    AlreadyAllocated,
    /// Second drain: the detached stream took the entry and its proof in one write.
    RecordedInDetachedStream,
    /// First drain: unallocated as of this read; the session store proves it.
    Reserved(SmDrainedAppendTicket),
    /// The ledger could not be used; drain the entry as before.
    Unkeyed,
}

pub(super) async fn claim(
    state: &WebSocketState,
    detached_stream_id: Option<&str>,
    sequence: u32,
    xml: &str,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    key: SmIngressAppendKey,
    drained_appends: &[DrainedAppend],
) -> Claim {
    let registry = &state.deps.protocol.sm_session_registry;
    let Some(stream_id) = detached_stream_id else {
        // A re-execution can queue the same obligation twice before the detach;
        // the ledger cannot see the first until the session store commits.
        if drained_appends
            .iter()
            .any(|drained| drained.0.key() == &key)
        {
            return Claim::AlreadyAllocated;
        }
        return match registry.reserve_drained_ingress_append(key).await {
            Ok(Some(ticket)) => Claim::Reserved(ticket),
            Ok(None) => Claim::AlreadyAllocated,
            Err(error) => {
                warn!(%error, "ingress append ledger unreadable in detach drain; draining unkeyed");
                Claim::Unkeyed
            }
        };
    };
    match registry
        .record_keyed_outbound_for_detached_stream_at(
            stream_id,
            sequence,
            xml.to_owned(),
            original_receipt_at,
            key,
        )
        .await
    {
        Ok(SmKeyedAppendOutcome::Appended { .. }) => Claim::RecordedInDetachedStream,
        Ok(SmKeyedAppendOutcome::AlreadyAppended { .. }) => Claim::AlreadyAllocated,
        Ok(SmKeyedAppendOutcome::NoSession) => Claim::Unkeyed,
        Err(error) => {
            warn!(stream_id, %error, "keyed detach-drain record failed; draining unkeyed");
            Claim::Unkeyed
        }
    }
}
