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
    /// Storage backend failure — `pending_delivery.insert` returned
    /// `Err`. The caller MUST treat this as a transient promotion
    /// failure and SKIP `confirm_drained` for the owning session so
    /// the durable SM row survives for restart-time retry. (Copilot
    /// review on PR #346: previously collapsed into `Dropped` so the
    /// caller would call `confirm_drained` and permanently lose the
    /// stanza when offline storage was temporarily failing.)
    StorageFailure,
}

/// Aggregate outcome of promoting every unacked stanza in a session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromotionSummary {
    pub redelivered: u32,
    pub queued: u32,
    pub bounced: u32,
    pub dropped: u32,
    pub unparseable: u32,
    /// Number of stanzas that failed to insert into pending storage.
    /// Non-zero means the session's promotion was lossy: the caller
    /// MUST NOT call `confirm_drained` for this session, so its
    /// durable SM row survives for restart-time retry.
    pub storage_failed: u32,
}

impl PromotionSummary {
    fn record(&mut self, outcome: &PromotedOutcome) {
        match outcome {
            PromotedOutcome::Redelivered { .. } => self.redelivered += 1,
            PromotedOutcome::Queued => self.queued += 1,
            PromotedOutcome::Bounced => self.bounced += 1,
            PromotedOutcome::Dropped => self.dropped += 1,
            PromotedOutcome::Unparseable => self.unparseable += 1,
            PromotedOutcome::StorageFailure => self.storage_failed += 1,
        }
    }

    /// True when at least one stanza in this session failed to
    /// promote due to a transient storage backend error. Callers
    /// MUST inspect this before invoking `confirm_drained`: a
    /// `true` result means the durable SM row must be kept so a
    /// later janitor pass / restart can retry promotion.
    pub fn has_storage_failure(&self) -> bool {
        self.storage_failed > 0
    }
}

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
                promote_one(
                    message,
                    entry.sequence,
                    &online,
                    blocklist,
                    registry,
                    pending_storage,
                    entry.original_receipt_at,
                )
                .await
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
    // route to the recipient's connected resource(s) via the
    // ConnectionRegistry. Locked Q6 = B step 1 (alt-resource) +
    // RFC 6121 §8.5.2 (bare-JID fanout to ALL non-negative-priority
    // resources, not just the highest-priority one — Copilot
    // review on PR #346: earlier code took only the first via
    // `next()` which silently lost deliveries on multi-resource
    // users).
    if !matches!(routing.live, LiveDecision::None) {
        let targets = collect_live_targets(&routing, &message, registry);
        if !targets.is_empty() {
            // Send to all eligible resources; mark redelivered if at
            // least one send succeeds (matches the live-route fanout
            // semantics in interpret.rs's `RouteToConnection` arm).
            let mut delivered_to: Option<FullJid> = None;
            for target in targets {
                if matches!(
                    registry
                        .send_to(&target, Stanza::Message(message.clone()))
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
        outbound_sequence: None,
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
                "Q6 promotion: pending_delivery insert failed; \
                 caller must NOT confirm_drained so durable SM row survives \
                 for restart-time retry"
            );
            PromotedOutcome::StorageFailure
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
///
/// Filters to resources that are both connected AND have sent
/// available presence. RFC 6121 §8.5.2.1.1 says only "available
/// resources that have specified a non-negative priority" are
/// candidates for bare-JID delivery; classify_dm_intake's online-
/// check should match. Without the `presence_available` filter
/// (Qodo review on PR #346), a connected-but-unavailable resource
/// would be mis-classified as a live recipient and the unacked
/// stanza would silently fail to route.
fn build_online_resources(
    registry: &ConnectionRegistry,
    recipient_bare: &BareJid,
) -> OnlineResources {
    let pairs: Vec<(FullJid, i8)> = registry
        .get_resources_for_user(recipient_bare)
        .into_iter()
        .filter_map(|full| {
            let entry = registry.get_entry(&full)?;
            if !entry.is_presence_available() {
                return None;
            }
            Some((full, entry.presence_priority()))
        })
        .collect();
    OnlineResources::from_pairs(pairs)
}

/// Collect every live-delivery target per the classifier's
/// `LiveDecision`. Returns an empty vec if no online resource
/// matches.
///
/// For `DeliverToFull`: if the addressed full JID is connected,
/// route there only. If it isn't (the original-detached resource is
/// gone), fall back to RFC 6121 §8.5.3's bare-JID fanout — locked
/// Q6 = B intent: the message gets to SOME resource of the
/// recipient, not just the original target.
///
/// For `DeliverToBareWithFanout`: route to ALL non-negative-priority
/// resources, matching `interpret.rs`'s live-route fanout (Copilot
/// review on PR #346: earlier code took only the first via `next()`
/// which lost deliveries on multi-resource users).
fn collect_live_targets(
    routing: &DmRouting,
    message: &xmpp_parsers::message::Message,
    registry: &ConnectionRegistry,
) -> Vec<FullJid> {
    let bare_target = match message.to.as_ref() {
        Some(jid) => jid.to_bare(),
        None => return Vec::new(),
    };
    match routing.live {
        LiveDecision::None => Vec::new(),
        LiveDecision::DeliverToFull => {
            let full_target = message
                .to
                .as_ref()
                .and_then(|jid| jid.clone().try_into_full().ok())
                .filter(|full| registry.get_entry(full).is_some());
            if let Some(full) = full_target {
                vec![full]
            } else {
                // Addressed resource has gone offline since the
                // classifier ran (or before promotion fired).
                // Fall back to bare-JID fanout per RFC 6121 §8.5.3
                // ("treat as if addressed to bare JID").
                registry.select_routable_resources_for_user(&bare_target)
            }
        }
        LiveDecision::DeliverToBareWithFanout => {
            registry.select_routable_resources_for_user(&bare_target)
        }
    }
}

/// Parse a wire-XML stanza back to its typed [`Stanza`] variant.
/// Returns `None` for unparseable XML or unknown root elements.
/// Handles `<message/>`, `<iq/>`, and `<presence/>` — the three
/// stanza kinds the SM unacked queue can hold (Copilot review on
/// PR #346: previous code only parsed `<message/>` and silently
/// dropped IQ/presence as Unparseable).
fn parse_stanza(xml: &str) -> Option<Stanza> {
    let element: xmpp_parsers::minidom::Element = xml.parse().ok()?;
    match element.name() {
        "message" => xmpp_parsers::message::Message::try_from(element)
            .ok()
            .map(Stanza::Message),
        "iq" => xmpp_parsers::iq::Iq::try_from(element).ok().map(Stanza::Iq),
        "presence" => xmpp_parsers::presence::Presence::try_from(element)
            .ok()
            .map(Stanza::Presence),
        _ => None,
    }
}

/// Promote an unacked `<iq/>` per the unavailable-resource semantics.
/// IQs cannot be queued offline (XEP-0160 §3 narrative explicitly
/// scopes offline storage to message stanzas; XEP-0160 §1 line 63
/// excludes IQ/presence). Try alt-resource live-redelivery; drop
/// otherwise.
async fn promote_iq(iq: xmpp_parsers::iq::Iq, registry: &ConnectionRegistry) -> PromotedOutcome {
    let target = iq
        .to
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .filter(|full| registry.get_entry(full).is_some());
    if let Some(target) = target {
        if matches!(
            registry.send_to(&target, Stanza::Iq(iq)).await,
            SendResult::Sent
        ) {
            return PromotedOutcome::Redelivered { to: target };
        }
    }
    PromotedOutcome::Dropped
}

/// Promote an unacked `<presence/>` per the unavailable-resource
/// semantics. Presence is not stored offline (RFC 6121 §8.5.2.1.4).
/// Try alt-resource live-redelivery; drop otherwise.
async fn promote_presence(
    presence: xmpp_parsers::presence::Presence,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let target = presence
        .to
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .filter(|full| registry.get_entry(full).is_some());
    if let Some(target) = target {
        if matches!(
            registry.send_to(&target, Stanza::Presence(presence)).await,
            SendResult::Sent
        ) {
            return PromotedOutcome::Redelivered { to: target };
        }
    }
    PromotedOutcome::Dropped
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
        let now = Utc::now();
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
                .map(
                    |(i, xml)| waddle_xmpp::stream_management::DetachedUnackedStanza {
                        sequence: i as u32 + 1,
                        stanza_xml: xml,
                        original_receipt_at: now,
                    },
                )
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

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

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

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

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

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

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

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

        assert_eq!(summary.dropped, 1);
        assert_eq!(summary.queued, 0);
        assert_eq!(summary.bounced, 0);
    }

    #[tokio::test]
    async fn full_jid_target_falls_back_to_bare_jid_fanout() {
        // Locked Q6 = B + RFC 6121 §8.5.3: when the addressed full
        // JID has gone offline but other resources of the recipient
        // are online, the unacked stanza must reach SOME resource
        // (not just be dropped). This tests the bare-JID fallback
        // path when classifier returns DeliverToFull.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        // Alice's web resource is online; laptop is detached.
        let alt = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(alt.clone(), tx);
        registry.update_presence(&alt, true, 1);

        // Stanza was originally addressed to alice/laptop (full JID).
        let xml = {
            let mut m = xmpp_parsers::message::Message::new(Some(
                "alice@example.com/laptop".parse::<jid::Jid>().unwrap(),
            ));
            m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().unwrap());
            m.type_ = xmpp_parsers::message::MessageType::Chat;
            m.bodies
                .insert(String::new(), xmpp_parsers::message::Body("hi".to_string()));
            let element: xmpp_parsers::minidom::Element = m.into();
            let mut buf = Vec::new();
            element.write_to(&mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        };

        let session =
            detached_session_with_unacked("stream-1", full("alice@example.com/laptop"), vec![xml]);

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

        assert_eq!(summary.redelivered, 1);
        assert_eq!(summary.queued, 0);
        assert!(rx.try_recv().is_ok(), "stanza redelivered to /web");
    }

    #[tokio::test]
    async fn bare_jid_target_fans_out_to_all_routable_resources() {
        // Locked Q6 step 1 + RFC 6121 §8.5.2: bare-JID-addressed
        // stanzas fan out to every non-negative-priority resource,
        // not just the highest-priority one. (Copilot review on
        // PR #346: prior code only delivered to one.)
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let web = full("alice@example.com/web");
        let mobile = full("alice@example.com/mobile");
        let (tx_web, mut rx_web) = tokio::sync::mpsc::channel(8);
        let (tx_mobile, mut rx_mobile) = tokio::sync::mpsc::channel(8);
        registry.register(web.clone(), tx_web);
        registry.register(mobile.clone(), tx_mobile);
        registry.update_presence(&web, true, 1);
        registry.update_presence(&mobile, true, 1);

        let session = detached_session_with_unacked(
            "stream-laptop",
            full("alice@example.com/laptop"),
            vec![dm_xml("bob@elsewhere/x", "alice@example.com", "fanout")],
        );

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

        assert_eq!(summary.redelivered, 1);
        // Both resources receive the stanza.
        assert!(rx_web.try_recv().is_ok(), "web received fanout");
        assert!(rx_mobile.try_recv().is_ok(), "mobile received fanout");
    }

    #[tokio::test]
    async fn iq_unacked_promoted_to_alt_resource_when_addressed_resource_online() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let target = full("alice@example.com/laptop");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(target.clone(), tx);

        // Build an IQ result addressed to alice/laptop.
        let iq_xml = {
            let iq = xmpp_parsers::iq::Iq {
                from: Some("server.example/srv".parse().unwrap()),
                to: Some("alice@example.com/laptop".parse().unwrap()),
                id: "iq-1".to_string(),
                payload: xmpp_parsers::iq::IqType::Result(None),
            };
            let element: xmpp_parsers::minidom::Element = iq.into();
            let mut buf = Vec::new();
            element.write_to(&mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        };

        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec![iq_xml],
        );

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

        assert_eq!(summary.redelivered, 1);
        assert_eq!(
            summary.unparseable, 0,
            "IQ must not be classified Unparseable"
        );
        assert!(
            rx.try_recv().is_ok(),
            "IQ redelivered to addressed resource"
        );
    }

    #[tokio::test]
    async fn iq_unacked_dropped_when_no_resource_online() {
        // IQs cannot be queued offline (XEP-0160 §1 line 63).
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let iq_xml = {
            let iq = xmpp_parsers::iq::Iq {
                from: Some("server.example/srv".parse().unwrap()),
                to: Some("alice@example.com/laptop".parse().unwrap()),
                id: "iq-1".to_string(),
                payload: xmpp_parsers::iq::IqType::Result(None),
            };
            let element: xmpp_parsers::minidom::Element = iq.into();
            let mut buf = Vec::new();
            element.write_to(&mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        };

        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec![iq_xml],
        );

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

        assert_eq!(summary.dropped, 1);
        assert_eq!(summary.queued, 0, "IQs never go to offline storage");
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
        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;
        assert_eq!(summary.unparseable, 1);
        assert_eq!(summary.queued, 0);
    }

    #[tokio::test]
    async fn promoted_pending_row_carries_per_stanza_original_receipt_at() {
        // Issue #209 PR #361: the Q6 SM-expiry promotion must stamp
        // each `pending_delivery` row's `original_receipt_at` with
        // the per-stanza value from the source DetachedUnackedStanza,
        // NOT a wall-clock fallback at expiry time. This is what
        // makes the eventual XEP-0203 `<delay/>` on the offline
        // replay carry the real failed-delivery time per
        // XEP-0203 §4.1 + XEP-0198 §5 line 364.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let receipt_time = chrono::DateTime::<Utc>::from_timestamp_millis(1_700_000_000_000)
            .expect("valid millis");
        let session = waddle_xmpp::stream_management::DetachedSession {
            stream_id: "stream-receipt-test".to_string(),
            user_id: "alice".to_string(),
            jid: full("alice@example.com/laptop"),
            inbound_count: 0,
            outbound_count: 1,
            last_acked: 0,
            unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: dm_xml("bob@elsewhere/x", "alice@example.com", "missed me"),
                original_receipt_at: receipt_time,
            }],
            max_resume_time: Some(60),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;
        assert_eq!(summary.queued, 1);

        let rows = storage.list(&bare("alice@example.com")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].original_receipt_at, receipt_time,
            "promoted row's original_receipt_at MUST be the per-stanza value, \
             not Utc::now() at expiry"
        );
    }

    #[tokio::test]
    async fn storage_failure_records_storage_failed_not_dropped() {
        // Copilot review on PR #346: pending_delivery insert backend
        // failures must NOT be silently collapsed into Dropped, since
        // the caller would then call confirm_drained and permanently
        // lose the unacked stanza. The PromotionSummary must surface
        // a separate `storage_failed` counter so the caller can keep
        // the durable SM row for restart-time retry.
        use async_trait::async_trait;
        use waddle_xmpp::pending_delivery::storage::{PendingDeliveryStorage, PendingStorageError};
        use waddle_xmpp::pending_delivery::{InsertOutcome, PendingRow, PendingRowId, SmSessionId};

        struct AlwaysFailingPending;
        #[async_trait]
        impl PendingDeliveryStorage for AlwaysFailingPending {
            async fn insert(&self, _row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
                Err(PendingStorageError::Other(
                    "simulated backend failure".into(),
                ))
            }
            async fn list(
                &self,
                _recipient: &BareJid,
            ) -> Result<Vec<PendingRow>, PendingStorageError> {
                Ok(vec![])
            }
            async fn claim_for_session(
                &self,
                _recipient: &BareJid,
                _session: &SmSessionId,
            ) -> Result<Vec<PendingRow>, PendingStorageError> {
                Ok(vec![])
            }
            async fn delete_claimed(
                &self,
                _session: &SmSessionId,
            ) -> Result<u64, PendingStorageError> {
                Ok(0)
            }
            async fn delete_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
                Ok(0)
            }
            async fn release_claim(
                &self,
                _session: &SmSessionId,
            ) -> Result<u64, PendingStorageError> {
                Ok(0)
            }
            async fn release_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
                Ok(0)
            }
            async fn record_pushed_at(
                &self,
                _id: &PendingRowId,
                _sequence: u32,
            ) -> Result<u64, PendingStorageError> {
                Ok(0)
            }
            async fn delete_acked_through(
                &self,
                _session: &SmSessionId,
                _sequence_max: u32,
            ) -> Result<u64, PendingStorageError> {
                Ok(0)
            }
            async fn list_orphaned_claims(
                &self,
                _live_sessions: &[SmSessionId],
            ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
                Ok(vec![])
            }
            async fn count(&self, _recipient: &BareJid) -> Result<u32, PendingStorageError> {
                Ok(0)
            }
        }

        let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(AlwaysFailingPending);
        let registry = ConnectionRegistry::new();
        let session = detached_session_with_unacked(
            "stream-1",
            full("alice@example.com/laptop"),
            vec![dm_xml(
                "bob@elsewhere/x",
                "alice@example.com",
                "transient backend down",
            )],
        );

        let summary =
            promote_session_unacked(&session, &registry, &storage, &Blocklist::empty()).await;

        assert_eq!(summary.storage_failed, 1, "storage failure surfaced");
        assert_eq!(summary.dropped, 0, "must not collapse into Dropped");
        assert_eq!(summary.queued, 0);
        assert!(
            summary.has_storage_failure(),
            "has_storage_failure() must be true so caller skips confirm_drained"
        );
    }
}
