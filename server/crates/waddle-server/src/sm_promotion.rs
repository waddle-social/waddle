//! XEP-0198 SM-expiry promotion (issue #209 slice (d) phase 4,
//! locked Q6 = B).
//!
//! When a detached XEP-0198 SM session's resume window closes (or
//! the server gracefully drains a live session at shutdown), the
//! server MUST treat its unacked stanzas the way XEP-0198 §5
//! line 364 prescribes:
//!
//! > "treat unacknowledged stanzas in the same way that it would
//! > treat a stanza sent to an unavailable resource, by either
//! > returning an error to the sender, delivery to an alternate
//! > resource, or committing the stanza to offline storage."
//!
//! The locked Q6 = B priority chain implements all three options in
//! priority order: **alt-resource → offline-storage → service-
//! unavailable error**. Each unacked stanza is re-run through the
//! [`classify_dm_intake`] classifier (locked Q6b: "promotion filter
//! delegates to classify_dm_intake" — single source of truth for
//! the type/hint matrix) and the resulting [`DmRouting`] gates which
//! branch fires.

mod live;
mod pending;
mod stanza;
#[cfg(test)]
mod tests;
mod types;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{debug, instrument};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, DmRouting, LiveDecision, OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::stream_management::DetachedSession;
use waddle_xmpp::Stanza;

use live::{build_online_resources, collect_live_targets};
use pending::{insert_pending, promote_as_transient};
use stanza::{parse_stanza, promote_iq, promote_presence};
pub use types::{PromotedOutcome, PromotionSummary};

/// Walk a session's unacked queue, promoting each stanza per the
/// locked Q6 = B priority chain. Each promoted `pending_delivery`
/// row's `original_receipt_at` is the per-stanza receipt time
/// preserved on the [`DetachedUnackedStanza`] (issue #209 PR #361:
/// previously a wall-clock fallback at expiry — now correct per
/// XEP-0203 §4.1 + XEP-0198 §5 line 364).
#[instrument(
    skip(session, registry, pending_storage, blocklist),
    fields(stream_id = %session.stream_id, jid = %session.jid)
)]
pub async fn promote_session_unacked(
    session: &DetachedSession,
    registry: &ConnectionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    blocklist: &Blocklist,
    server_domain: &str,
) -> PromotionSummary {
    let mut summary = PromotionSummary::default();
    let recipient_bare = session.jid.to_bare();

    // Snapshot the recipient's currently-online resources for the
    // classifier. Empty in the common SM-expiry case (otherwise
    // the session wouldn't have been detached in the first place,
    // unless other resources joined after detach).
    let online = build_online_resources(registry, &recipient_bare);

    for entry in &session.unacked_stanzas {
        let outcome = match parse_stanza(&entry.stanza_xml) {
            Some(Stanza::Message(message)) => {
                let ctx = PromotionContext {
                    online: &online,
                    blocklist,
                    registry,
                    pending_storage,
                    original_receipt_fallback: entry.original_receipt_at,
                    server_domain,
                };
                promote_one(message, entry.sequence, ctx).await
            }
            Some(Stanza::Iq(iq)) => promote_iq(iq, registry).await,
            Some(Stanza::Presence(presence)) => promote_presence(presence, registry).await,
            None => PromotedOutcome::Unparseable,
        };
        debug!(
            stream_id = %session.stream_id,
            sequence = entry.sequence,
            ?outcome,
            "Q6 promotion: per-stanza outcome"
        );
        summary.record(&outcome);
    }

    debug!(
        stream_id = %session.stream_id,
        redelivered = summary.redelivered,
        queued = summary.queued,
        bounced = summary.bounced,
        dropped = summary.dropped,
        not_promotable = summary.not_promotable,
        unparseable = summary.unparseable,
        storage_failed = summary.storage_failed,
        "Q6 promotion: session summary"
    );
    summary
}

struct PromotionContext<'a> {
    online: &'a OnlineResources,
    blocklist: &'a Blocklist,
    registry: &'a ConnectionRegistry,
    pending_storage: &'a Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    server_domain: &'a str,
}

/// Promote a single typed [`xmpp_parsers::message::Message`] per the
/// locked Q6 chain.
async fn promote_one(
    message: xmpp_parsers::message::Message,
    sequence: u32,
    ctx: PromotionContext<'_>,
) -> PromotedOutcome {
    if waddle_xmpp_core::mam::is_mam_query_response_message(&message) {
        waddle_xmpp::prometheus::increment_sm_promotion_not_promotable();
        return PromotedOutcome::NotPromotable;
    }

    let routing: DmRouting = classify_dm_intake(&message, ctx.online, ctx.blocklist);

    // Step 1: alt-resource — if the classifier says live-deliver,
    // route to the recipient's connected resource(s) via the
    // ConnectionRegistry. Locked Q6 = B step 1 (alt-resource) +
    // RFC 6121 §8.5.2 (bare-JID fanout to ALL non-negative-priority
    // resources, not just the highest-priority one — Copilot
    // review on PR #346: earlier code took only the first via
    // `next()` which silently lost deliveries on multi-resource
    // users).
    if !matches!(routing.live, LiveDecision::None) {
        let targets = collect_live_targets(&routing, &message, ctx.registry);
        if !targets.is_empty() {
            let delayed = build_replay_stanza(
                MaterializedPayload::Transient(Box::new(message.clone())),
                ctx.server_domain,
                ctx.original_receipt_fallback,
                ReplayReason::SmRedelivery,
            );
            // Send to all eligible resources; mark redelivered if at
            // least one send succeeds (matches the live-route fanout
            // semantics in interpret.rs's `RouteToConnection` arm).
            let mut delivered_to = None;
            for target in targets {
                if matches!(
                    ctx.registry
                        .send_to(&target, Stanza::Message(delayed.clone()))
                        .await,
                    SendResult::Sent
                ) && delivered_to.is_none()
                {
                    delivered_to = Some(target);
                }
            }
            if let Some(target) = delivered_to {
                return PromotedOutcome::Redelivered { to: target };
            }
        }
        // Classifier said deliver but no live target took the stanza
        // (full-JID target had gone offline by send time, or the
        // socket buffer rejected). Fall through to offline storage.
    }

    // Step 2: offline storage — if the classifier marked the stanza
    // for `pending_delivery`, insert.
    match routing.pending {
        PendingDecision::None => {
            // Neither live nor offline survived — nothing to do.
            // Common reasons: <no-store/>, chat-states-only, or
            // type='error' to a fully-offline recipient (silently
            // dropped per RFC 6121 §8.5.2.1.4).
            return PromotedOutcome::Dropped;
        }
        PendingDecision::Archived | PendingDecision::Transient => {}
    }

    let payload = match routing.pending {
        PendingDecision::Archived => {
            // The classifier said the stanza is MAM-archived. The
            // archive write happened on the original intake (before
            // it was even queued in unacked). For Q6 promotion we
            // need the recipient-by stanza-id to point at; extract
            // from the message itself (it was stamped on intake by
            // the Canonicalize handler).
            let recipient_bare = match message.to.as_ref() {
                Some(jid) => jid.to_bare(),
                None => return PromotedOutcome::Dropped,
            };
            let recipient_jid = jid::Jid::from(recipient_bare.clone());
            let stanza_id =
                match waddle_xmpp_core::xep0359::extract_stanza_id_by(&message, &recipient_jid) {
                    Some(id) => id,
                    None => {
                        debug!(
                            sequence,
                            "Q6 promotion: classifier said Archived but no recipient \
                         <stanza-id> stamp present; falling back to Transient"
                        );
                        // Fallback: store inline as Transient so the
                        // message isn't lost, with a warn marker for the
                        // chain-misconfiguration suspicion.
                        return promote_as_transient(
                            message,
                            recipient_bare,
                            ctx.pending_storage,
                            ctx.original_receipt_fallback,
                            ctx.registry,
                        )
                        .await;
                    }
                };
            PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                stanza_id,
                recipient_jid,
            ))
        }
        PendingDecision::Transient => PendingPayload::Transient(Box::new(message.clone())),
        PendingDecision::None => unreachable!("guarded above"),
    };

    let recipient_bare = match message.to.as_ref() {
        Some(jid) => jid.to_bare(),
        None => return PromotedOutcome::Dropped,
    };

    insert_pending(
        recipient_bare,
        payload,
        ctx.pending_storage,
        ctx.original_receipt_fallback,
        &message,
        ctx.registry,
    )
    .await
}
