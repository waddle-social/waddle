use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::PersistedIngressAppend;
use waddle_xmpp::stream_management::DetachedSession;
use waddle_xmpp::Stanza;

use super::{
    promote_iq, promote_one, promote_presence, promote_session_unacked, PromotedOutcome,
    PromotionContext, PromotionSummary, TerminalOverflowPromotionDeps, TOMBSTONE_CLOCK_SKEW_SLACK,
};

/// Promote ledger-backed entries through the atomic custody handoff. Replay
/// entries without ledger ownership retain the ordinary XEP-0198 policy.
pub(crate) async fn promote_session_with_custody(
    session: &DetachedSession,
    deps: TerminalOverflowPromotionDeps<'_>,
) -> PromotionSummary {
    use waddle_xmpp::stream_management::persistence::IngressCustodyDisposition;
    let stream = SmSessionId::new(session.stream_id.clone());
    let mut summary = PromotionSummary::default();
    for entry in &session.unacked_stanzas {
        let candidates = match deps
            .sm_registry
            .get_ingress_appends_for_sequence(&stream, entry.sequence)
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, %stream, "SM promotion: custody lookup failed; retaining retry responsibility");
                summary.record(entry.sequence, &PromotedOutcome::StorageFailure);
                break;
            }
        };
        let typed = super::stanza::parse_stanza(&entry.stanza_xml);
        let matching: Vec<_> = candidates
            .into_iter()
            .filter(|append| {
                append.original_receipt_at.timestamp_millis()
                    == entry.original_receipt_at.timestamp_millis()
                    && typed
                        .as_ref()
                        .is_some_and(|stanza| stanza.to_element() == append.payload.to_element())
            })
            .collect();
        if matching.is_empty() {
            let one = super::detached_session_for_terminal_entry(session, entry.clone());
            let single = promote_session_unacked(
                &one,
                deps.registry,
                deps.user_registry,
                deps.pending_storage,
                deps.blocklist,
                deps.server_domain,
                deps.recent_tombstones,
            )
            .await;
            summary.redelivered += single.redelivered;
            summary.queued += single.queued;
            summary.bounced += single.bounced;
            summary.dropped += single.dropped;
            summary.not_promotable += single.not_promotable;
            summary.unparseable += single.unparseable;
            summary.scrubbed += single.scrubbed;
            summary.storage_failed += single.storage_failed;
            summary.promoted_sequences.extend(single.promoted_sequences);
        } else {
            let outcome = if let Some(append) = matching
                .iter()
                .find(|append| append.disposition == IngressCustodyDisposition::Pending)
            {
                promote_ingress_custody(append, deps).await
            } else {
                PromotedOutcome::NotPromotable
            };
            let settled = !matches!(
                outcome,
                PromotedOutcome::StorageFailure
                    | PromotedOutcome::Redelivered { .. }
                    | PromotedOutcome::Bounced
            );
            let mut completion_failed = !settled;
            if settled {
                for append in matching
                    .iter()
                    .filter(|append| append.disposition == IngressCustodyDisposition::Pending)
                {
                    if deps
                        .sm_registry
                        .complete_ingress_append(append, IngressCustodyDisposition::Promoted)
                        .await
                        .is_err()
                    {
                        completion_failed = true;
                    }
                }
            }
            if completion_failed {
                summary.record(entry.sequence, &PromotedOutcome::StorageFailure);
            } else {
                summary.record(entry.sequence, &outcome);
            }
        }
        if summary.has_storage_failure() {
            break;
        }
    }
    summary
}

/// Recover the typed custody payload even when the bounded replay queue and
/// canonical ingress row no longer exist. Uses the same unavailable-resource
/// policy as ordinary XEP-0198 expiration.
pub(crate) async fn promote_ingress_custody(
    append: &PersistedIngressAppend,
    deps: TerminalOverflowPromotionDeps<'_>,
) -> PromotedOutcome {
    if deps.recent_tombstones.iter().any(|record| {
        append.original_receipt_at <= record.recorded_at_utc + TOMBSTONE_CLOCK_SKEW_SLACK
            && record
                .key
                .matches_message_element(&append.payload.to_element())
    }) {
        return PromotedOutcome::Scrubbed;
    }
    match append.payload.clone() {
        Stanza::Message(message) => {
            // The original resource is unavailable. Commit a durable offline
            // handoff before the janitor schedules any live pending flush.
            let online = waddle_xmpp::protocol::dm_routing::OnlineResources::empty();
            promote_one(
                message,
                append.sequence,
                PromotionContext {
                    online: &online,
                    blocklist: deps.blocklist,
                    registry: deps.registry,
                    user_registry: deps.user_registry,
                    pending_storage: deps.pending_storage,
                    original_receipt_fallback: append.original_receipt_at,
                    server_domain: deps.server_domain,
                    origin: super::pending::PromotionOrigin::IngressCustody(append),
                },
            )
            .await
        }
        Stanza::Iq(iq) => promote_iq(*iq, deps.registry).await,
        Stanza::Presence(presence) => promote_presence(presence, deps.registry).await,
    }
}
