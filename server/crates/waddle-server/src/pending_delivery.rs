//! XEP-0160 offline-message flush orchestration (issue #209,
//! waddle-server side).
//!
//! [`waddle_xmpp::pending_delivery::flush::build_replay_stanza`] is the
//! pure wire-shape builder. This module ties it to the live system:
//! it reads rows out of the [`PendingDeliveryStorage`], resolves
//! Archived rows against MAM, and pushes the replay stanzas to the
//! recovering resource via the [`ConnectionRegistry`].
//!
//! Locked design points consumed here:
//!
//! - **Q7a/Q7d** — caller (presence handler) gates this on the first
//!   non-negative-priority presence of a fresh session via
//!   [`ConnectionEntry::claim_offline_flush`].
//! - **Q7c** — `claim_for_session` atomically tags rows with the
//!   recipient's resource so a concurrent presence from another
//!   resource sees an empty pool.
//! - **Q5** — wire shape (`<delay/>` with original receipt time, server
//!   `from`, preserved `to`/extensions, no `<stanza-id/>` for Transient).
//! - **Q7b (partial)** — currently rows are deleted on send rather than
//!   on SM-ack. Full SM-ack-keyed lifecycle lands with slice (d) (SM
//!   persistence). Re-flush after pre-ack session death (Q7c) requires
//!   the SM-ack lifecycle and is therefore TODO with the same slice.

use std::sync::Arc;

use jid::{BareJid, FullJid};
use tracing::{debug, instrument, warn};
use waddle_xmpp::pending_delivery::flush::{build_replay_stanza, MaterializedPayload};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, SmSessionId};
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::Stanza;

/// Outcome of a flush attempt for one resource.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlushOutcome {
    /// Number of rows claimed from `pending_delivery`.
    pub claimed: u32,
    /// Number of replayed stanzas successfully pushed to the resource.
    pub pushed: u32,
    /// Number of rows the resolver could not materialize (Archived row
    /// whose MAM lookup is not available — happens when MAM storage is
    /// unwired in the test fixture, never in production).
    pub unresolved: u32,
}

/// Flush every currently-unclaimed `pending_delivery` row for the
/// given recipient to the given resource.
///
/// Called by the presence handler once `claim_offline_flush()` has
/// returned `true` on the recovering [`ConnectionEntry`] — i.e. the
/// first non-negative-priority presence of a fresh session.
///
/// The session-id used to claim rows is derived from the recovering
/// full JID (a concrete SM session id is wired in slice (d)).
#[instrument(skip(storage, registry, archive_resolver), fields(recipient = %recipient, resource = %resource))]
pub async fn flush_for_resource<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    server_domain: &str,
    recipient: &BareJid,
    resource: &FullJid,
    archive_resolver: &R,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    let session_id = SmSessionId::new(resource.to_string());
    let claimed = match storage.claim_for_session(recipient, &session_id).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "claim_for_session failed; skipping flush");
            return FlushOutcome::default();
        }
    };
    let mut outcome = FlushOutcome {
        claimed: claimed.len() as u32,
        ..FlushOutcome::default()
    };
    if claimed.is_empty() {
        return outcome;
    }

    for row in claimed {
        let Some(payload) = materialize(&row, archive_resolver).await else {
            outcome.unresolved += 1;
            continue;
        };
        let replay = build_replay_stanza(payload, server_domain, row.original_receipt_at);
        let stanza = Stanza::Message(replay);
        match registry.send_to(resource, stanza).await {
            SendResult::Sent => outcome.pushed += 1,
            other => {
                debug!(?other, "send to recovering resource failed mid-flush");
            }
        }
    }

    // SLICE (d) TODO: defer deletion until SM-ack of the flush stanza
    // (locked Q7b). Until SM session persistence lands, delete on push
    // so a successful flush doesn't re-deliver on the next presence
    // update; the trade-off is a crash window (push succeeds, server
    // crashes before SM-ack) where the recipient may not have the
    // message and pending_delivery has already dropped it. The MAM
    // catch-up path (Q10a) still recovers Archived rows; Transient
    // rows are by-design ephemeral on crash.
    if outcome.pushed > 0 {
        let _ = storage.delete_claimed(&session_id).await;
    } else {
        // Nothing was delivered — release rows so a subsequent flush
        // can retry them.
        let _ = storage.release_claim(&session_id).await;
    }

    outcome
}

/// Resolves Archived `PendingRow` references against MAM.
///
/// Production wiring uses [`MamArchiveResolver`] over a real
/// [`waddle_xmpp::mam::storage::MamStorage`] handle. Tests use
/// [`NullArchiveResolver`] when only Transient rows are exercised.
#[async_trait::async_trait]
pub trait ArchiveResolver: Send + Sync {
    /// Read the archived stanza by recipient bare JID + stanza-id.
    async fn resolve(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Option<xmpp_parsers::message::Message>;
}

/// MAM-backed resolver for production use.
pub struct MamArchiveResolver {
    pub mam_storage: Arc<dyn waddle_xmpp::mam::storage::MamStorage>,
}

#[async_trait::async_trait]
impl ArchiveResolver for MamArchiveResolver {
    async fn resolve(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Option<xmpp_parsers::message::Message> {
        let archived = match self
            .mam_storage
            .get_message_by_archive_or_stanza_id(archive_jid, stanza_id)
            .await
        {
            Ok(Some(archived)) => archived,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    error = %error,
                    archive_jid = %archive_jid,
                    stanza_id,
                    "MAM lookup failed during flush"
                );
                return None;
            }
        };
        // Parse the preserved wire XML back into a typed Message. The
        // archived row includes server-stamped <stanza-id> by recipient
        // bare, so the parsed Message already carries the XEP-0359
        // identifier required by locked Q5c.
        let stanza_xml = archived.stanza_xml.as_deref()?;
        let element: xmpp_parsers::minidom::Element = stanza_xml.parse().ok()?;
        xmpp_parsers::message::Message::try_from(element).ok()
    }
}

/// No-op resolver for tests that only exercise Transient rows.
#[derive(Debug, Default)]
pub struct NullArchiveResolver;

#[async_trait::async_trait]
impl ArchiveResolver for NullArchiveResolver {
    async fn resolve(
        &self,
        _archive_jid: &BareJid,
        _stanza_id: &str,
    ) -> Option<xmpp_parsers::message::Message> {
        None
    }
}

async fn materialize<R>(row: &PendingRow, resolver: &R) -> Option<MaterializedPayload>
where
    R: ArchiveResolver + ?Sized,
{
    match &row.payload {
        PendingPayload::Transient(_) => MaterializedPayload::from_transient(row),
        PendingPayload::Archived(stanza_id_ref) => {
            let archived = resolver
                .resolve(&stanza_id_ref.by, stanza_id_ref.id.as_str())
                .await?;
            Some(MaterializedPayload::Archived(Box::new(archived)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow};
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn bare(s: &str) -> BareJid {
        s.parse().expect("bare jid")
    }

    fn full(s: &str) -> FullJid {
        s.parse().expect("full jid")
    }

    fn transient_row(recipient: &str, body: &str) -> PendingRow {
        let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
        m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        PendingRow {
            recipient: bare(recipient),
            original_receipt_at: Utc::now(),
            payload: PendingPayload::Transient(Box::new(m)),
            flushed_in_session: None,
        }
    }

    #[tokio::test]
    async fn flush_with_no_rows_is_noop() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &full("alice@example.com/web"),
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome, FlushOutcome::default());
    }

    #[tokio::test]
    async fn flush_pushes_transient_rows_and_deletes_on_success() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        // Insert two transient rows.
        for body in ["one", "two"] {
            storage
                .insert(transient_row("alice@example.com", body))
                .await
                .unwrap();
        }

        // Wire a registered connection so send_to actually has a sink.
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);

        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.claimed, 2);
        assert_eq!(outcome.pushed, 2);
        assert_eq!(outcome.unresolved, 0);

        // Both messages were sent on the wire.
        let mut received = Vec::new();
        while let Ok(stanza) = rx.try_recv() {
            received.push(stanza);
        }
        assert_eq!(received.len(), 2);

        // Rows have been deleted on successful push (slice (d) will
        // shift this to SM-ack-keyed deletion).
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn flush_releases_rows_when_no_push_succeeds() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "hi"))
            .await
            .unwrap();

        // No connection registered → send_to returns NotConnected.
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");

        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.claimed, 1);
        assert_eq!(outcome.pushed, 0);
        // Row stays in storage but with flushed_in_session cleared so
        // a later flush can retry.
        let rows = storage.list(&bare("alice@example.com")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].flushed_in_session.is_none());
    }
}
