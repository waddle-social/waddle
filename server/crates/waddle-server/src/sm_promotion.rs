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

use std::sync::Arc;

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use tracing::{debug, instrument, warn};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, DmRouting, LiveDecision, OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::stream_management::DetachedSession;
use waddle_xmpp::Stanza;

/// Outcome of promoting a single unacked stanza per the Q6 chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotedOutcome {
    /// Live-redelivered to an alternate non-negative-priority
    /// resource of the recipient.
    Redelivered { to: FullJid },
    /// Inserted into `pending_delivery` for offline replay.
    Queued,
    /// Bounced `<service-unavailable/>` to the sender per
    /// XEP-0160 §3 step 3 (`pending_delivery` quota exceeded).
    Bounced,
    /// Dropped — classifier produced no actionable sink (e.g.
    /// `<no-store/>`, chat-states-only, error-type to fully-offline
    /// recipient per RFC 6121 §8.5.2.1.4).
    Dropped,
    /// Skipped — stanza could not be parsed back to a typed value
    /// (corrupt unacked queue entry). Logged for operator visibility.
    Unparseable,
}

/// Aggregate outcome of promoting every unacked stanza in a session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromotionSummary {
    pub redelivered: u32,
    pub queued: u32,
    pub bounced: u32,
    pub dropped: u32,
    pub unparseable: u32,
}

impl PromotionSummary {
    fn record(&mut self, outcome: &PromotedOutcome) {
        match outcome {
            PromotedOutcome::Redelivered { .. } => self.redelivered += 1,
            PromotedOutcome::Queued => self.queued += 1,
            PromotedOutcome::Bounced => self.bounced += 1,
            PromotedOutcome::Dropped => self.dropped += 1,
            PromotedOutcome::Unparseable => self.unparseable += 1,
        }
    }
}

/// Walk a session's unacked queue, promoting each stanza per the
/// locked Q6 = B priority chain.
///
/// `original_receipt_fallback` is the wall-clock time stamped onto
/// each promoted `pending_delivery` row's `original_receipt_at`
/// when the underlying [`DetachedSession`] doesn't carry per-
/// stanza receipt times. Today this is `Utc::now()` at expiry —
/// approximate but bounded by the session's resume window. When
/// `DetachedSession.unacked_stanzas` grows a typed shape carrying
/// the real receipt time, the fallback becomes irrelevant.
#[instrument(
    skip(session, registry, pending_storage, blocklist),
    fields(stream_id = %session.stream_id, jid = %session.jid)
)]
pub async fn promote_session_unacked(
    session: &DetachedSession,
    registry: &ConnectionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    blocklist: &Blocklist,
    original_receipt_fallback: DateTime<Utc>,
) -> PromotionSummary {
    let mut summary = PromotionSummary::default();
    let recipient_bare = session.jid.to_bare();

    // Snapshot the recipient's currently-online resources for the
    // classifier. Empty in the common SM-expiry case (otherwise
    // the session wouldn't have been detached in the first place,
    // unless other resources joined after detach).
    let online = build_online_resources(registry, &recipient_bare);

    for (sequence, stanza_xml) in &session.unacked_stanzas {
        let outcome = match parse_message(stanza_xml) {
            Some(message) => {
                promote_one(
                    message,
                    *sequence,
                    &online,
                    blocklist,
                    registry,
                    pending_storage,
                    original_receipt_fallback,
                )
                .await
            }
            None => PromotedOutcome::Unparseable,
        };
        debug!(
            stream_id = %session.stream_id,
            sequence,
            ?outcome,
            "Q6 promotion: per-stanza outcome"
        );
        summary.record(&outcome);
    }

    debug!(
        stream_id = %session.stream_id,
        ?summary,
        "Q6 promotion: session summary"
    );
    summary
}

/// Promote a single typed [`xmpp_parsers::message::Message`] per the
/// locked Q6 chain.
async fn promote_one(
    message: xmpp_parsers::message::Message,
    sequence: u32,
    online: &OnlineResources,
    blocklist: &Blocklist,
    registry: &ConnectionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
) -> PromotedOutcome {
    let routing: DmRouting = classify_dm_intake(&message, online, blocklist);

    // Step 1: alt-resource — if the classifier says live-deliver,
    // route to the active resource set via the connection registry.
    if !matches!(routing.live, LiveDecision::None) {
        if let Some(target) = pick_live_target(&routing, &message, registry) {
            if let SendResult::Sent = registry
                .send_to(&target, Stanza::Message(message.clone()))
                .await
            {
                return PromotedOutcome::Redelivered { to: target };
            }
        }
        // Live decision but no actual send target — fall through to
        // offline storage if the classifier also approved that.
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
                            pending_storage,
                            original_receipt_fallback,
                            registry,
                        )
                        .await;
                    }
                };
            PendingPayload::Archived(waddle_xmpp::protocol::event::StanzaIdRef {
                by: recipient_bare,
                id: waddle_xmpp::protocol::event::StanzaIdValue::new(stanza_id),
            })
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
        pending_storage,
        original_receipt_fallback,
        &message,
        registry,
    )
    .await
}

async fn promote_as_transient(
    message: xmpp_parsers::message::Message,
    recipient_bare: BareJid,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let payload = PendingPayload::Transient(Box::new(message.clone()));
    insert_pending(
        recipient_bare,
        payload,
        pending_storage,
        original_receipt_fallback,
        &message,
        registry,
    )
    .await
}

async fn insert_pending(
    recipient: BareJid,
    payload: PendingPayload,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_at: DateTime<Utc>,
    original_message: &xmpp_parsers::message::Message,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
    };
    match pending_storage.insert(row).await {
        Ok(InsertOutcome::Inserted) => PromotedOutcome::Queued,
        Ok(InsertOutcome::QuotaExceeded) => {
            // XEP-0160 §3 step 3 + RFC 6120 §8.3 — bounce
            // <service-unavailable/> to the sender. We use the same
            // typed StanzaError builder the routing layer uses for
            // intake-time quota overflow so the wire shape is
            // identical.
            send_quota_bounce(original_message, &recipient, registry).await;
            PromotedOutcome::Bounced
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "Q6 promotion: pending_delivery insert failed"
            );
            PromotedOutcome::Dropped
        }
    }
}

async fn send_quota_bounce(
    original_message: &xmpp_parsers::message::Message,
    recipient: &BareJid,
    registry: &ConnectionRegistry,
) {
    let error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Cancel,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
        "en",
        "Recipient's offline message queue is full",
    );
    let bounce =
        waddle_xmpp::protocol::handlers::errors::message_error_reply(original_message, error);
    let Some(sender_jid) = bounce.to.clone() else {
        warn!(
            recipient = %recipient,
            "Q6 promotion: bounce target JID missing; dropping bounce"
        );
        return;
    };
    let stanza = Stanza::Message(bounce);
    let mut delivered = false;
    match sender_jid.clone().try_into_full() {
        Ok(full) => {
            if matches!(registry.send_to(&full, stanza).await, SendResult::Sent) {
                delivered = true;
            }
        }
        Err(bare) => {
            for full in registry.get_resources_for_user(&bare) {
                if matches!(
                    registry.send_to(&full, stanza.clone()).await,
                    SendResult::Sent
                ) {
                    delivered = true;
                }
            }
        }
    }
    if !delivered {
        warn!(
            recipient = %recipient,
            sender = %sender_jid,
            "Q6 promotion: <service-unavailable/> bounce was not deliverable \
             (remote sender or no bound resource) — XEP-0160 §3 step 3 \
             conformance gap until s2s lands"
        );
    }
}

/// Build an [`OnlineResources`] snapshot for `recipient_bare` from
/// the connection registry.
fn build_online_resources(
    registry: &ConnectionRegistry,
    recipient_bare: &BareJid,
) -> OnlineResources {
    let pairs: Vec<(FullJid, i8)> = registry
        .get_resources_for_user(recipient_bare)
        .into_iter()
        .filter_map(|full| {
            registry
                .get_entry(&full)
                .map(|entry| (full, entry.presence_priority()))
        })
        .collect();
    OnlineResources::from_pairs(pairs)
}

/// Pick a live-delivery target full JID per the classifier's
/// `LiveDecision`. Returns `None` if no online resource matches.
fn pick_live_target(
    routing: &DmRouting,
    message: &xmpp_parsers::message::Message,
    registry: &ConnectionRegistry,
) -> Option<FullJid> {
    match routing.live {
        LiveDecision::None => None,
        LiveDecision::DeliverToFull => message
            .to
            .as_ref()
            .and_then(|jid| jid.clone().try_into_full().ok())
            .filter(|full| registry.get_entry(full).is_some()),
        LiveDecision::DeliverToBareWithFanout => message.to.as_ref().and_then(|jid| {
            let bare = jid.to_bare();
            registry
                .select_routable_resources_for_user(&bare)
                .into_iter()
                .next()
        }),
    }
}

fn parse_message(xml: &str) -> Option<xmpp_parsers::message::Message> {
    let element: xmpp_parsers::minidom::Element = xml.parse().ok()?;
    if element.name() != "message" {
        return None;
    }
    xmpp_parsers::message::Message::try_from(element).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::QuotaPolicy;

    fn full(s: &str) -> FullJid {
        s.parse().unwrap()
    }

    fn bare(s: &str) -> BareJid {
        s.parse().unwrap()
    }

    fn detached_session_with_unacked(
        stream_id: &str,
        jid: FullJid,
        unacked_xml: Vec<String>,
    ) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: "alice".to_string(),
            jid,
            inbound_count: 0,
            outbound_count: unacked_xml.len() as u32,
            last_acked: 0,
            unacked_stanzas: unacked_xml
                .into_iter()
                .enumerate()
                .map(|(i, xml)| (i as u32 + 1, xml))
                .collect(),
            max_resume_time: Some(60),
            detached_at: Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        }
    }

    fn dm_xml(from: &str, to: &str, body: &str) -> String {
        let mut m = xmpp_parsers::message::Message::new(Some(to.parse::<jid::Jid>().unwrap()));
        m.from = Some(from.parse::<jid::Jid>().unwrap());
        m.type_ = xmpp_parsers::message::MessageType::Chat;
        m.bodies
            .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
        let element: xmpp_parsers::minidom::Element = m.into();
        let mut buf = Vec::new();
        element.write_to(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[tokio::test]
    async fn promotes_to_pending_delivery_when_no_alt_resource() {
        // Locked Q6 = B step 2: alt-resource fails (no online
        // resources for alice), so pending_delivery insert fires.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec![dm_xml("bob@elsewhere/x", "alice@example.com", "missed me?")],
        );

        let summary = promote_session_unacked(
            &session,
            &registry,
            &storage,
            &Blocklist::empty(),
            Utc::now(),
        )
        .await;

        assert_eq!(summary.queued, 1);
        assert_eq!(summary.redelivered, 0);
        assert_eq!(summary.bounced, 0);
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn promotes_to_alt_resource_when_one_is_online() {
        // Locked Q6 = B step 1: another resource of the same user
        // is online with non-negative priority — re-route there
        // instead of queueing.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let alt = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(alt.clone(), tx);
        registry.update_presence(&alt, true, 1);

        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec![dm_xml(
                "bob@elsewhere/x",
                "alice@example.com",
                "alt resource",
            )],
        );

        let summary = promote_session_unacked(
            &session,
            &registry,
            &storage,
            &Blocklist::empty(),
            Utc::now(),
        )
        .await;

        assert_eq!(summary.redelivered, 1);
        assert_eq!(summary.queued, 0);
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
        assert!(rx.try_recv().is_ok(), "stanza pushed to alt resource");
    }

    #[tokio::test]
    async fn bounces_service_unavailable_when_quota_exceeded() {
        // Locked Q6 = B step 3: pending_delivery quota refused →
        // <service-unavailable/> bounced to sender per XEP-0160 §3
        // step 3 + RFC 6120 §8.3.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap {
                max_rows: 0,
            }));
        let registry = ConnectionRegistry::new();
        // Register the sender so the bounce can be delivered.
        let sender = full("bob@example.com/x");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(sender.clone(), tx);
        registry.update_presence(&sender, true, 1);

        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec![dm_xml(
                "bob@example.com/x",
                "alice@example.com",
                "queue full",
            )],
        );

        let summary = promote_session_unacked(
            &session,
            &registry,
            &storage,
            &Blocklist::empty(),
            Utc::now(),
        )
        .await;

        assert_eq!(summary.bounced, 1);
        assert_eq!(summary.queued, 0);
        let bounce = rx.try_recv().expect("bounce stanza pushed back to sender");
        match &bounce.stanza {
            Stanza::Message(m) => {
                assert_eq!(m.type_, xmpp_parsers::message::MessageType::Error);
            }
            _ => panic!("expected Message bounce"),
        }
    }

    #[tokio::test]
    async fn drops_no_store_hint_stanzas_silently() {
        // Classifier returns pending=None for <no-store/>, so the
        // promotion drops without bouncing.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let mut m = xmpp_parsers::message::Message::new(Some(
            "alice@example.com".parse::<jid::Jid>().unwrap(),
        ));
        m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().unwrap());
        m.type_ = xmpp_parsers::message::MessageType::Chat;
        m.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("ephemeral".to_string()),
        );
        waddle_xmpp::xep::xep0334::add_hint(&mut m, waddle_xmpp::xep::xep0334::Hint::NoStore);
        let element: xmpp_parsers::minidom::Element = m.into();
        let mut buf = Vec::new();
        element.write_to(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        let session =
            detached_session_with_unacked("stream-1", full("alice@example.com/laptop"), vec![xml]);

        let summary = promote_session_unacked(
            &session,
            &registry,
            &storage,
            &Blocklist::empty(),
            Utc::now(),
        )
        .await;

        assert_eq!(summary.dropped, 1);
        assert_eq!(summary.queued, 0);
        assert_eq!(summary.bounced, 0);
    }

    #[tokio::test]
    async fn skips_unparseable_stanzas() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec!["not actually XML".to_string()],
        );
        let summary = promote_session_unacked(
            &session,
            &registry,
            &storage,
            &Blocklist::empty(),
            Utc::now(),
        )
        .await;
        assert_eq!(summary.unparseable, 1);
        assert_eq!(summary.queued, 0);
    }
}
