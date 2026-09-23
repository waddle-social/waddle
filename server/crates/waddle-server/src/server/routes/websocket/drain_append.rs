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

/// Canonical-read health across one drain.
///
/// Authorization is a serial read per keyed frame, bounded at 250 ms, and it runs
/// before the detached session is stored — so until the drain ends the client's
/// `<resume/>` finds nothing. One indeterminate read therefore ends authorization for
/// the rest of the drain: a full queue against a browned-out database would otherwise
/// hold the detach for a minute to protect a dedupe key it cannot obtain anyway.
pub(super) struct DrainAuthority {
    canonical_reads_indeterminate: bool,
    /// Healthy reads are serial too. Past this point the rest of the drain is unkeyed.
    deadline: tokio::time::Instant,
}

/// Total time one drain may spend authorizing. It bounds the detach — and with it
/// the client's ability to resume and a graceful restart's drain — independently of
/// queue depth and of how slow a merely degraded database is.
const DRAIN_AUTHORIZATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

impl Default for DrainAuthority {
    fn default() -> Self {
        Self {
            canonical_reads_indeterminate: false,
            deadline: tokio::time::Instant::now() + DRAIN_AUTHORIZATION_BUDGET,
        }
    }
}

impl DrainAuthority {
    /// Authorize the relayed claim against canonical ingress state.
    ///
    /// Failure degrades to an unkeyed drain (decision recorded on #1789): the origin
    /// was already told `Delivered`, so refusing the entry would turn a canonical-read
    /// outage into silent loss. At-least-once survives; only the dedupe key is lost.
    ///
    /// `stanza` is `None` for an entry the live handler already recorded: it bound the
    /// obligation to the typed stanza there, so only the canonical read remains.
    pub(super) async fn authorize(
        &mut self,
        state: &WebSocketState,
        stanza: Option<&Stanza>,
        obligation: Option<SmRelayedAppendObligation>,
    ) -> Option<SmRelayedAppendObligation> {
        use crate::ingress::append_authority::{
            check_canonical_sender, check_stanza_binding, record_degraded_to_unkeyed,
            AppendAuthorityRejection,
        };
        use waddle_xmpp::telemetry::attributes::IngressAppendAuthorizationFailure;

        let obligation = obligation?;
        let exhausted =
            self.canonical_reads_indeterminate || tokio::time::Instant::now() >= self.deadline;
        let bound = stanza.map_or(Ok(()), |stanza| {
            check_stanza_binding(
                stanza,
                &obligation.sender_bare,
                obligation.key.kind.to_storage(),
            )
        });
        let outcome = match bound {
            Err(reason) => Err(reason),
            Ok(()) if exhausted => Err(AppendAuthorityRejection::ServicesUnavailable),
            Ok(()) => {
                check_canonical_sender(
                    state.deps.app_state.db_pool.global(),
                    obligation.key.message_key,
                    &obligation.sender_bare,
                )
                .await
            }
        };
        match outcome {
            Ok(()) => Some(obligation),
            Err(reason) => {
                self.canonical_reads_indeterminate |= matches!(
                    reason.failure_class(),
                    IngressAppendAuthorizationFailure::Indeterminate
                );
                record_degraded_to_unkeyed(&reason, &obligation.sender_bare);
                None
            }
        }
    }
}

/// Reserve proof for entries the live handler already recorded into the SM queue.
///
/// The handler dequeues a frame and records it *before* the transport write, so a
/// failed or unacknowledged write leaves a recovery-owned entry that no drain ever
/// sees. Its obligation is proven with the session snapshot like a drained one. An
/// obligation that already holds an allocation elsewhere keeps its entry, unproven:
/// the sequence is counted and the client may already have the stanza.
pub(super) async fn reserve_live_recorded(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
    drained_appends: &mut Vec<DrainedAppend>,
) {
    let mut authority = DrainAuthority::default();
    for (sequence, obligation) in sm_state.unacked_ingress_appends() {
        let Some(obligation) = authority.authorize(state, None, Some(obligation)).await else {
            continue;
        };
        if drained_appends
            .iter()
            .any(|drained| drained.0.key() == &obligation.key)
        {
            continue;
        }
        match state
            .deps
            .protocol
            .sm_session_registry
            .reserve_drained_ingress_append(obligation.key)
            .await
        {
            Ok(Some(ticket)) => {
                if let Some((payload, received_at)) = sm_state.ingress_replay_payload(sequence) {
                    drained_appends.push(DrainedAppend(ticket.at(sequence, payload, received_at)));
                } else {
                    warn!(
                        sequence,
                        "ingress replay payload unavailable at detach; entry stays unkeyed"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                warn!(%error, "ingress append ledger unreadable at detach; entry stays unkeyed");
            }
        }
    }
}

/// Bind a relayed obligation to the typed stanza on the live path, where the stanza
/// is still in hand. A mismatch degrades to unkeyed, as everywhere else.
pub(super) fn bind_live(
    stanza: &Stanza,
    obligation: Option<SmRelayedAppendObligation>,
) -> Option<SmRelayedAppendObligation> {
    let obligation = obligation?;
    match crate::ingress::append_authority::check_stanza_binding(
        stanza,
        &obligation.sender_bare,
        obligation.key.kind.to_storage(),
    ) {
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

/// Bind an authorized obligation to the frame the recipient pass emitted.
///
/// That frame is wire XML, and the keyed registry boundary is typed, so it is parsed
/// back exactly once. Only messages hold append obligations; a frame that is not one
/// drains unkeyed, counted like every other degrade.
pub(super) fn key_recipient_pass_frame(
    xml: &str,
    obligation: SmRelayedAppendObligation,
) -> Option<(SmIngressAppendKey, Stanza)> {
    let message = xml
        .parse::<minidom::Element>()
        .ok()
        .and_then(|element| xmpp_parsers::message::Message::try_from(element).ok());
    match message {
        Some(message) => Some((obligation.key, Stanza::Message(message))),
        None => {
            crate::ingress::append_authority::record_degraded_to_unkeyed(
                &crate::ingress::append_authority::AppendAuthorityRejection::NotMessage,
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
    stanza: &Stanza,
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
            stanza,
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
