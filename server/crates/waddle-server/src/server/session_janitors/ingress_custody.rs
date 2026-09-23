//! Recovery of accepted ingress payloads whose SM replay cache has disappeared.

use tracing::warn;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::IngressCustodyDisposition;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmIngressAppendKey};

use crate::server::routes::websocket::WebSocketState;
use crate::sm_promotion::{
    promote_ingress_custody, recent_tombstones_for_promotion,
    scrub_pending_for_tombstones_recorded_during_promotion, PromotedOutcome, PromotionScrubOutcome,
    TerminalOverflowPromotionDeps,
};

/// Cancellation after claim acquisition must still publish exact-fence release
/// responsibility. The normal claim retry lane handles backend failures.
struct RecoveryClaim<'a> {
    registry: &'a InMemorySmSessionRegistry,
    stream: &'a SmSessionId,
}

impl Drop for RecoveryClaim<'_> {
    fn drop(&mut self) {
        self.registry
            .defer_unpublished_enabled_claim_release(self.stream.as_str());
    }
}

/// The cursor traverses the immutable obligation key, including protected rows,
/// so a page of long-lived active streams cannot starve later orphaned custody.
pub(crate) async fn run_ingress_custody_sweep(
    state: &WebSocketState,
    cursor: &mut Option<SmIngressAppendKey>,
) -> bool {
    const PAGE_SIZE: usize = 256;
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
    tokio::time::timeout(BUDGET, drain_page(state, cursor, PAGE_SIZE))
        .await
        .unwrap_or(false)
}

async fn drain_page(
    state: &WebSocketState,
    cursor: &mut Option<SmIngressAppendKey>,
    limit: usize,
) -> bool {
    let registry = &state.deps.protocol.sm_session_registry;
    let pending = match registry
        .list_pending_ingress_appends_after(cursor.as_ref(), limit)
        .await
    {
        Ok(pending) => pending,
        Err(error) => {
            warn!(%error, "SM custody recovery: durable inventory read failed");
            return false;
        }
    };
    let at_end = pending.len() < limit;
    let mut completed = true;
    for candidate in pending {
        // Advance before I/O: cancellation cannot let one stalled owner pin
        // every subsequent sweep. Failed rows are revisited on the next lap.
        *cursor = Some(candidate.key.clone());
        let stream = &candidate.accepting_stream;
        let _operation = match registry.lock_session_operation(stream.as_str()).await {
            Ok(operation) => operation,
            Err(error) => {
                warn!(%stream, %error, "SM custody recovery: lifecycle lock failed");
                completed = false;
                continue;
            }
        };
        if state
            .deps
            .protocol
            .connection_registry
            .active_sm_stream_ids()
            .contains(stream)
        {
            continue;
        }
        match registry.has_retirement_protection(stream).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                warn!(%stream, %error, "SM custody recovery: owner probe failed");
                completed = false;
                continue;
            }
        }
        let Some(authority) = registry.ensure_session_claim(stream.as_str()).await else {
            completed = false;
            continue;
        };
        let _claim = RecoveryClaim { registry, stream };
        // The fenced storage adapter acquires its own identity read guard.
        // Nesting those guards would deadlock behind a queued identity rotation.
        drop(authority);
        if state
            .deps
            .protocol
            .connection_registry
            .active_sm_stream_ids()
            .contains(stream)
        {
            continue;
        }
        // A client ack, normal promotion or durable tombstone scrub may have
        // discharged custody since this page was read. Re-read under ownership.
        let append = match registry.get_ingress_append(&candidate.key).await {
            Ok(Some(append)) if append.disposition == IngressCustodyDisposition::Pending => append,
            Ok(_) => continue,
            Err(error) => {
                warn!(%stream, %error, "SM custody recovery: current custody read failed");
                completed = false;
                continue;
            }
        };
        let blocklist = match state
            .deps
            .protocol
            .blocking_storage
            .list_blocked_jid_entries(&append.key.resource.to_bare())
            .await
        {
            Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
            Err(error) => {
                warn!(%stream, %error, "SM custody recovery: blocklist unavailable; retaining payload");
                completed = false;
                continue;
            }
        };
        let tombstones = match recent_tombstones_for_promotion(registry, "ingress custody") {
            Ok(tombstones) => tombstones,
            Err(_) => {
                completed = false;
                continue;
            }
        };
        let outcome = promote_ingress_custody(
            &append,
            TerminalOverflowPromotionDeps {
                sm_registry: registry,
                registry: &state.deps.protocol.connection_registry,
                user_registry: &state.deps.protocol.user_registry,
                pending_storage: &state.deps.protocol.pending_delivery_storage,
                blocklist: &blocklist,
                server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                recent_tombstones: &tombstones,
            },
        )
        .await;
        if matches!(
            outcome,
            PromotedOutcome::StorageFailure
                | PromotedOutcome::Redelivered { .. }
                | PromotedOutcome::Bounced
        ) {
            completed = false;
            continue;
        }
        if scrub_pending_for_tombstones_recorded_during_promotion(
            registry,
            &state.deps.protocol.pending_delivery_storage,
            &tombstones,
            "ingress custody",
        )
        .await
            == PromotionScrubOutcome::Failed
        {
            completed = false;
            continue;
        }
        if let Err(error) = registry
            .complete_ingress_append(&append, IngressCustodyDisposition::Promoted)
            .await
        {
            // The sink accepted the payload but its receipt failed. Retrying
            // is intentionally at-least-once, never evidence of a lost payload.
            warn!(%stream, %error, "SM custody recovery: completion failed; retaining payload");
            completed = false;
        }
        drop(_claim);
        drop(_operation);
        // Release the recovery-only fence before the next row: many payloads
        // can share one retired stream, and they must not drain at one per tick.
        registry.retry_pending_claim_releases(8).await;
        if matches!(outcome, PromotedOutcome::Queued) {
            // A newly available resource may have spent its initial offline
            // flush between the routing snapshot and the durable insert.
            crate::server::routes::websocket::redrive_terminal_pending_rows_to_live_resource(
                state,
                &append.key.resource.to_bare(),
            )
            .await;
        }
    }
    if at_end {
        *cursor = None;
    }
    completed
}
