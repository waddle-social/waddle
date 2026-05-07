use super::*;

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
    /// Number of rows dropped because the recipient blocked the sender
    /// AFTER the row was inserted (XEP-0191 §2 step 4 flush-time
    /// re-evaluation, issue #209 PR #360). Blocked rows are deleted
    /// from `pending_delivery` since the block is final until lifted.
    pub dropped_blocked: u32,
}

/// Per-flush context bundling the optional / contextual parameters
/// of [`flush_for_resource`]. Carved out of the function signature so
/// adding a new dependency (e.g. a future XEP-0411 hook) doesn't push
/// the parameter count over the clippy threshold and tempt a
/// suppression. Project hard rule (`server/CLAUDE.md`): never add
/// `#[allow(clippy::too_many_arguments)]` (Greptile/Qodo review on
/// PR #360).
pub struct FlushContext<'a, R>
where
    R: ArchiveResolver + ?Sized,
{
    /// JID-form domain stamped onto the `<delay/>` element added to
    /// each replayed stanza per XEP-0203 §4.1.
    pub server_domain: &'a str,
    /// Recovering connection's XEP-0198 stream id when SM is enabled.
    /// `None` falls back to the delete-on-push path (no ack will fire).
    pub sm_session: Option<&'a SmSessionId>,
    /// Live blocking storage for XEP-0191 §2 step 4 flush-time
    /// re-evaluation. `None` skips the check (test fixtures only;
    /// production always wires the real backend).
    pub blocking_storage: Option<&'a Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>>,
    /// Resolves Archived `PendingRow` references against MAM.
    pub archive_resolver: &'a R,
}

/// Flush every currently-unclaimed `pending_delivery` row for the
/// given recipient to the given resource.
///
/// Called by the presence handler once `claim_offline_flush()` has
/// returned `true` on the recovering [`ConnectionEntry`] — i.e. the
/// first non-negative-priority presence of a fresh session.
///
/// `ctx` carries the optional / contextual parameters
/// (`sm_session`, `blocking_storage`, `archive_resolver`,
/// `server_domain`) — see [`FlushContext`] for details.
#[instrument(skip(storage, registry, ctx), fields(recipient = %recipient, resource = %resource))]
pub async fn flush_for_resource<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    recipient: &BareJid,
    resource: &FullJid,
    ctx: FlushContext<'_, R>,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    let FlushContext {
        server_domain,
        sm_session,
        blocking_storage,
        archive_resolver,
    } = ctx;
    // Snapshot the recipient's current blocklist once for the whole
    // flush batch. XEP-0191 §2 step 4: if the recipient blocked the
    // sender AFTER the row was queued, the row must be dropped.
    // Per-batch (not per-row) to avoid hammering the blocking-storage
    // backend; correctness window is the duration of one flush
    // (typically << 1 s). Same fail-closed policy as
    // `interpret.rs::offline_recipient_pass_blocklist_storage_error_skips_recipient_persistence`:
    // on storage error, abort the flush rather than degrade to an
    // empty blocklist (which would let blocked senders through).
    let blocklist: Option<std::collections::HashSet<jid::BareJid>> = match blocking_storage {
        Some(bs) => match bs.list_blocked_jids(recipient).await {
            Ok(jids) => Some(jids.into_iter().collect()),
            Err(error) => {
                warn!(
                    error = %error,
                    "blocklist load failed; aborting flush to preserve fail-closed XEP-0191 policy"
                );
                return FlushOutcome::default();
            }
        },
        None => None,
    };
    // For non-SM sessions, use a transient per-flush session id so the
    // claim row tag is consistent within the batch. The post-push
    // delete path keys on row id, not session id, so the transient
    // value never escapes this function. Only the SM path keeps the
    // claim alive past the push for the SM-ack lifecycle.
    let transient_session_id;
    let session_id_for_claim: &SmSessionId = match sm_session {
        Some(id) => id,
        None => {
            transient_session_id =
                SmSessionId::new(format!("transient:{}:{}", resource, uuid::Uuid::new_v4()));
            &transient_session_id
        }
    };
    let claimed = match storage
        .claim_for_session(recipient, session_id_for_claim)
        .await
    {
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
            // Archived row whose MAM lookup failed — the original
            // stanza is unrecoverable. Drop the row instead of
            // releasing it so we don't loop forever on a poison pill.
            // The message is permanently lost from the recipient's
            // perspective; we surface it loudly so production logs
            // can flag MAM corruption / unexpected tombstones.
            outcome.unresolved += 1;
            waddle_xmpp::prometheus::increment_pending_delivery_unresolved_poison_pill();
            if let Err(error) = storage.delete_row(&row.id).await {
                warn!(
                    row_id = %row.id,
                    error = %error,
                    "pending_delivery delete_row (unresolved poison pill) failed"
                );
            }
            continue;
        };
        // XEP-0191 §2 step 4 flush-time block re-evaluation
        // (issue #209 PR #360): if the recipient blocked the sender
        // after the row was queued, drop it. Block is final until
        // the recipient lifts it — `delete_row` not `release_row`.
        if let Some(blocked) = blocklist.as_ref() {
            let sender_bare = sender_bare_for_payload(&payload);
            if let Some(sender) = sender_bare {
                if blocked.contains(&sender) {
                    debug!(
                        row_id = %row.id,
                        recipient = %recipient,
                        sender = %sender,
                        "pending_delivery flush dropping row: recipient blocked sender post-intake (XEP-0191 §2 step 4)"
                    );
                    outcome.dropped_blocked += 1;
                    if let Err(error) = storage.delete_row(&row.id).await {
                        // Copilot review on PR #360: without a release
                        // here, the row would stay tagged with the
                        // current (still-live) SM session id.
                        // Consequence: the SM-expiry janitor wouldn't
                        // see it as orphaned (its session is alive),
                        // the SM ack wouldn't delete it
                        // (`outbound_sequence` is NULL — never pushed),
                        // and the next flush wouldn't re-claim it
                        // (`flushed_in_session` not NULL). The row
                        // would wedge permanently and consume quota.
                        // Fall back to `release_row` so the next
                        // recovering resource (or this same session
                        // on a later presence transition) can re-claim
                        // it and re-check the blocklist.
                        warn!(
                            row_id = %row.id,
                            error = %error,
                            "pending_delivery delete_row (blocked at flush) failed; \
                             releasing claim so the next flush can re-check the blocklist"
                        );
                        if let Err(release_error) = storage.release_row(&row.id).await {
                            warn!(
                                row_id = %row.id,
                                error = %release_error,
                                "pending_delivery release_row (blocked-at-flush fallback) \
                                 also failed; row may remain wedged until claim-expiry janitor \
                                 sees the session expire"
                            );
                        }
                    }
                    continue;
                }
            }
        }
        let replay = build_replay_stanza(
            payload,
            server_domain,
            row.original_receipt_at,
            ReplayReason::OfflineStorage,
        );
        let stanza = Stanza::Message(replay);
        // SM-enabled path: tag outbound with row id so the recipient's
        // main loop can stamp `outbound_sequence` post-`record_outbound`.
        // The row stays claimed for the SM-ack lifecycle.
        // Non-SM path: same outbound tag (cheap), but we delete on Sent
        // because there's no SM session to ack against.
        let push_result = if sm_session.is_some() {
            registry
                .send_pending_flush(resource, stanza, row.id.clone(), row.original_receipt_at)
                .await
        } else {
            registry.send_to(resource, stanza).await
        };
        match push_result {
            SendResult::Sent => {
                outcome.pushed += 1;
                if sm_session.is_none() {
                    // Non-SM fallback: delete on push since no `<a h>`
                    // will ever fire (Codex review on PR #358).
                    if let Err(error) = storage.delete_row(&row.id).await {
                        warn!(
                            row_id = %row.id,
                            error = %error,
                            "pending_delivery delete_row (non-SM push) failed; \
                             row may re-deliver on next presence"
                        );
                    }
                }
                // SM-enabled: row stays claimed by `sm_session` with
                // `outbound_sequence = NULL` until the recipient's
                // main loop stamps it via `record_pushed_at`. If the
                // session dies before push, `release_claim` clears
                // the claim for re-flush (Q7c).
            }
            other => {
                debug!(?other, row_id = %row.id, "send to recovering resource failed mid-flush");
                // Per-row release so an undelivered row stays eligible
                // for re-claim on the next flush trigger.
                if let Err(error) = storage.release_row(&row.id).await {
                    warn!(
                        row_id = %row.id,
                        error = %error,
                        "pending_delivery release_row (undelivered) failed"
                    );
                }
            }
        }
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
    /// Read the archived stanza by canonical XEP-0359 [`StanzaId`]
    /// (`{ id, by }`). Returns the typed
    /// [`xmpp_parsers::message::Message`] reconstructed from the MAM
    /// row; returns `None` on miss or any non-fatal lookup failure
    /// (the caller treats this as a poison pill and drops the
    /// `pending_delivery` row).
    async fn resolve(&self, stanza_id: &StanzaId) -> Option<xmpp_parsers::message::Message>;
}

/// MAM-backed resolver for production use.
pub struct MamArchiveResolver {
    pub mam_storage: Arc<dyn waddle_xmpp::mam::storage::MamStorage>,
}

#[async_trait::async_trait]
impl ArchiveResolver for MamArchiveResolver {
    async fn resolve(&self, stanza_id: &StanzaId) -> Option<xmpp_parsers::message::Message> {
        // MAM lookup keys on the archive's *bare* JID — XEP-0313 §5
        // archives are per-user / per-room (BareJid), and the canonical
        // `StanzaId.by` Jid carries that information (the MAM writer
        // always stamps with a bare-form Jid).
        let archive_bare = stanza_id.by.to_bare();
        let archived = match self
            .mam_storage
            .get_message_by_archive_or_stanza_id(&archive_bare, stanza_id.as_str())
            .await
        {
            Ok(Some(archived)) => archived,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    error = %error,
                    archive_jid = %archive_bare,
                    stanza_id = %stanza_id,
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
    async fn resolve(&self, _stanza_id: &StanzaId) -> Option<xmpp_parsers::message::Message> {
        None
    }
}

async fn materialize<R>(row: &PendingRow, resolver: &R) -> Option<MaterializedPayload>
where
    R: ArchiveResolver + ?Sized,
{
    match &row.payload {
        PendingPayload::Transient(_) => MaterializedPayload::from_transient(row),
        PendingPayload::Archived(stanza_id) => {
            let archived = resolver.resolve(stanza_id).await?;
            Some(MaterializedPayload::Archived(Box::new(archived)))
        }
    }
}

/// Extract the sender's bare JID from a materialized payload for the
/// XEP-0191 §2 step 4 flush-time block re-evaluation. Returns `None`
/// when the message has no `from` attribute (server-origin replays
/// have no flesh-and-blood sender to block).
fn sender_bare_for_payload(payload: &MaterializedPayload) -> Option<jid::BareJid> {
    let message: &xmpp_parsers::message::Message = match payload {
        MaterializedPayload::Archived(m) | MaterializedPayload::Transient(m) => m,
    };
    message.from.as_ref().map(|jid| jid.to_bare())
}
