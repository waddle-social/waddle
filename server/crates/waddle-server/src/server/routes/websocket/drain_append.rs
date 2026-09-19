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
    pub(super) async fn authorize(
        &mut self,
        state: &WebSocketState,
        stanza: &Stanza,
        obligation: Option<SmRelayedAppendObligation>,
    ) -> Option<SmRelayedAppendObligation> {
        use crate::ingress::append_authority::{
            check_canonical_authority, record_degraded_to_unkeyed, AppendAuthorityRejection,
        };
        use waddle_xmpp::telemetry::attributes::IngressAppendAuthorizationFailure;

        let obligation = obligation?;
        let outcome =
            if self.canonical_reads_indeterminate || tokio::time::Instant::now() >= self.deadline {
                Err(AppendAuthorityRejection::ServicesUnavailable)
            } else {
                check_canonical_authority(
                    state.deps.app_state.db_pool.global(),
                    stanza,
                    obligation.key.message_key,
                    &obligation.sender_bare,
                    obligation.key.kind.to_storage(),
                )
                .await
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

/// Parse a recipient-pass wire frame back into the typed message it carries.
/// Only messages hold append obligations; anything else drains unkeyed.
pub(super) fn parse_message_frame(xml: &str) -> Option<Stanza> {
    let element: minidom::Element = xml.parse().ok()?;
    xmpp_parsers::message::Message::try_from(element)
        .ok()
        .map(Stanza::Message)
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
