use std::collections::HashMap;
use std::sync::Arc;

use jid::BareJid;
use tracing::debug;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{PendingRowId, SmSessionId};
use waddle_xmpp::stream_management::{DetachedSession, DetachedUnackedStanza};
use waddle_xmpp::Stanza;

use super::stanza::parse_stanza;
use super::PromotedOutcome;

pub(super) enum PendingLinks {
    Known(HashMap<u32, Vec<PendingRowId>>),
    Unknown,
}

/// Whether terminal policy still needs to classify at least one internal
/// resume barrier. Until that classification runs, callers must retain the
/// session outside every generic transient dead-letter budget.
pub(crate) fn session_has_unclassified_barrier(session: &DetachedSession) -> bool {
    session
        .unacked_stanzas
        .iter()
        .any(DetachedUnackedStanza::is_resume_barrier)
}

/// Snapshot every pending-delivery row linked to this session before any
/// barrier is pruned. An unreadable or structurally ambiguous relation is
/// deliberately represented as [`PendingLinks::Unknown`] so classification
/// retains the barrier for reconciliation.
pub(super) async fn load_pending_links(
    session: &DetachedSession,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    recipient: &BareJid,
) -> PendingLinks {
    if !session_has_unclassified_barrier(session) {
        return PendingLinks::Known(HashMap::new());
    }

    match pending_storage.list(recipient).await {
        Ok(rows) => {
            let source_session_id = SmSessionId::new(session.stream_id.clone());
            let mut links = HashMap::<u32, Vec<PendingRowId>>::new();
            let mut ambiguous = false;
            for row in rows
                .into_iter()
                .filter(|row| row.flushed_in_session.as_ref() == Some(&source_session_id))
            {
                if let Some(sequence) = row.outbound_sequence {
                    links.entry(sequence).or_default().push(row.id);
                } else {
                    ambiguous = true;
                    tracing::error!(
                        stream_id = %session.stream_id,
                        row_id = %row.id,
                        "Q6 promotion: source session owns pending row without outbound sequence; resume barrier link is ambiguous"
                    );
                }
            }
            if ambiguous {
                PendingLinks::Unknown
            } else {
                PendingLinks::Known(links)
            }
        }
        Err(error) => {
            tracing::warn!(
                stream_id = %session.stream_id,
                %error,
                "Q6 promotion: could not classify pending-row links for resume barrier"
            );
            PendingLinks::Unknown
        }
    }
}

/// Classify one typed resume-barrier row without invoking application
/// delivery. Only an exact, conformant, unlinked server-to-resource ping is
/// safe to discard; every malformed or unresolved relation is quarantined.
pub(super) fn classify(
    session: &DetachedSession,
    entry: &DetachedUnackedStanza,
    pending_links: &PendingLinks,
) -> PromotedOutcome {
    let valid_barrier = match parse_stanza(&entry.stanza_xml) {
        Some(Stanza::Iq(iq)) => {
            waddle_xmpp::xep::xep0199::is_ping_from_server_to_full_jid(&iq, &session.jid)
        }
        Some(Stanza::Message(_) | Stanza::Presence(_)) | None => false,
    };
    if !valid_barrier {
        tracing::error!(
            stream_id = %session.stream_id,
            sequence = entry.sequence,
            "Q6 promotion: replay row is tagged as a resume barrier but is not the internal conformant XEP-0199 ping shape; retaining it"
        );
        return PromotedOutcome::Quarantined;
    }

    match pending_links {
        PendingLinks::Unknown => PromotedOutcome::Quarantined,
        PendingLinks::Known(links) => match links.get(&entry.sequence) {
            None => {
                debug!(
                    stream_id = %session.stream_id,
                    sequence = entry.sequence,
                    "Q6 promotion: discarded resume barrier without application delivery"
                );
                PromotedOutcome::NotPromotable
            }
            Some(rows) => {
                tracing::error!(
                    stream_id = %session.stream_id,
                    sequence = entry.sequence,
                    linked_rows = rows.len(),
                    "Q6 promotion: resume barrier unexpectedly owns pending-delivery row(s); retaining both for reconciliation"
                );
                PromotedOutcome::Quarantined
            }
        },
    }
}
