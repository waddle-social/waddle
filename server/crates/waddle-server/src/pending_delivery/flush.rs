use super::*;
use waddle_xmpp::pending_delivery::storage::{PendingClaim, PendingClaimToken};
use waddle_xmpp::registry::{OutboundStanza, PendingFlushReservation};

/// Bound on rows claimed and pushed per `claim_batch_for_session` iteration
/// inside [`flush_for_resource`] (issue #1220). Deliberately « the 256-slot
/// recipient outbound mpsc (`OUTBOUND_CHANNEL_SIZE`) so a single batch never
/// fills the channel on its own; the batch loop backpressures on the mpsc
/// between batches instead of materializing the whole offline backlog at once.
pub(crate) const FLUSH_BATCH_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingDispatchReadiness {
    Ready,
    Deferred,
}

/// Durable predecessor check for an archived offline replay. This check must
/// not wait on the receiving connection's inbound loop or treat a completed
/// offline-enqueue receipt as proof that the pending row reached the client.
#[async_trait::async_trait]
pub trait PendingDispatchGate: Send + Sync {
    async fn check_turn(
        &self,
        row: &PendingRow,
        resource: &FullJid,
        stream: Option<&SmSessionId>,
    ) -> Result<PendingDispatchReadiness, PendingStorageError>;
}

/// Outcome of a flush attempt for one resource.
#[derive(Debug, Default)]
pub struct FlushOutcome {
    /// Number of rows claimed from `pending_delivery`.
    pub claimed: u32,
    /// Number of `claim_batch_for_session` batches drained (issue #1220).
    pub batches: u32,
    /// Number of replayed stanzas successfully pushed to the resource.
    pub pushed: u32,
    /// Number of rows the resolver definitively could not materialize
    /// (Archived row whose MAM lookup returned no row, or whose
    /// preserved wire XML is unparseable). These are poison pills:
    /// the row is deleted so the flush loop never wedges on it.
    pub unresolved: u32,
    /// Number of rows whose MAM lookup failed *transiently* (issue
    /// #1122), plus every later row in the same claimed batch: the
    /// first transient failure is batch-fatal (see the Err arm in
    /// [`flush_for_resource`]) so FIFO order is preserved and a MAM
    /// outage isn't hammered once per remaining row. These rows are
    /// NOT deleted — their claims are released, and the presence
    /// handler resets the connection's offline-flush CAS when this
    /// counter is non-zero so the client's next presence update
    /// re-attempts the flush (another recovering resource can also
    /// pick the rows up).
    pub deferred_transient: u32,
    /// Rows deferred by the archive predecessor gate. This is a subset of
    /// `deferred_transient`, allowing the off-connection delivery pump to retry
    /// ordering contention without retrying unrelated MAM lookup failures.
    pub deferred_ordering: u32,
    /// Number of rows dropped because the recipient blocked the sender
    /// AFTER the row was inserted (XEP-0191 §2 step 4 flush-time
    /// re-evaluation, issue #209 PR #360). Blocked rows are deleted
    /// from `pending_delivery` since the block is final until lifted.
    pub dropped_blocked: u32,
    // Owned retry work crosses the bounded terminal-flush handoff without
    // losing the exact claim token or allowing a copied outcome to fork it.
    failed_releases: Vec<DeferredClaimRelease>,
    pending_offers: Vec<PendingOffer>,
    unqueued_offers: Vec<PendingClaim>,
}

impl FlushOutcome {
    pub(crate) fn has_retry_work(&self) -> bool {
        !self.failed_releases.is_empty()
            || !self.pending_offers.is_empty()
            || !self.unqueued_offers.is_empty()
    }

    fn absorb(&mut self, mut next: Self) {
        self.claimed += next.claimed;
        self.batches += next.batches;
        self.pushed += next.pushed;
        self.unresolved += next.unresolved;
        self.dropped_blocked += next.dropped_blocked;
        self.deferred_transient = next.deferred_transient;
        self.deferred_ordering = next.deferred_ordering;
        self.failed_releases.append(&mut next.failed_releases);
        self.pending_offers.append(&mut next.pending_offers);
        self.unqueued_offers.append(&mut next.unqueued_offers);
    }
}

#[derive(Debug)]
struct PendingOffer {
    claim: PendingClaim,
    reservation: PendingFlushReservation,
    outbound: OutboundStanza,
    sm_enabled: bool,
}

/// Per-flush context bundling the optional / contextual parameters
/// of [`flush_for_resource`]. Carved out of the function signature so
/// adding a new dependency (e.g. a future XEP-0411 hook) doesn't push
/// the parameter count over the clippy threshold and tempt a
/// suppression. Project hard rule (`server/CLAUDE.md`): never add
/// a clippy suppression attribute (Greptile/Qodo review on
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
    /// The recovering connection's registry ownership token (issue #1220
    /// review). When `Some`, SM flush pushes are owner-gated
    /// (`send_pending_flush_if_owner`) so a same-full-JID replacement that
    /// races in mid-flush does not receive rows claimed under the original
    /// session's stream id. `None` (test fixtures) uses the ungated send.
    pub owner: Option<&'a std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Resolves Archived `PendingRow` references against MAM.
    pub archive_resolver: &'a R,
    /// Canonical archive-order dispatch gate, required by production callers.
    pub dispatch_gate: Option<&'a dyn PendingDispatchGate>,
}

/// Re-drive ordering contention off the connection loop. Ownership and
/// availability are checked before every pass so a sleeping pump cannot claim
/// rows for a replacement or an unavailable resource.
pub async fn flush_for_resource_with_retry<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    recipient: &BareJid,
    resource: &FullJid,
    ctx: FlushContext<'_, R>,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    resume_flush_for_resource_with_retry(
        storage,
        registry,
        recipient,
        resource,
        ctx,
        FlushOutcome::default(),
    )
    .await
}

/// Continue the retry pump from a bounded pass, including release work that
/// must settle even after its original resource becomes unavailable/replaced.
pub(crate) async fn resume_flush_for_resource_with_retry<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    recipient: &BareJid,
    resource: &FullJid,
    ctx: FlushContext<'_, R>,
    mut total: FlushOutcome,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    let mut delay = std::time::Duration::from_millis(100);
    loop {
        settle_retry_work(storage, registry, &mut total).await;
        if total.has_retry_work() {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(std::time::Duration::from_secs(5));
            continue;
        }
        if let Some(owner) = ctx.owner {
            let Some(entry) = registry.entry_if_owner(resource, owner) else {
                return total;
            };
            if !entry.is_locally_hosted() {
                return total;
            }
            if !entry.is_presence_available() || entry.presence_priority() < 0 {
                entry.reset_offline_flush();
                return total;
            }
        }
        let outcome = flush_for_resource(
            storage,
            registry,
            recipient,
            resource,
            FlushContext {
                server_domain: ctx.server_domain,
                sm_session: ctx.sm_session,
                blocking_storage: ctx.blocking_storage,
                owner: ctx.owner,
                archive_resolver: ctx.archive_resolver,
                dispatch_gate: ctx.dispatch_gate,
            },
        )
        .await;
        let progressed = outcome.pushed > 0;
        total.absorb(outcome);
        if total.deferred_ordering == 0 {
            return total;
        }
        if progressed {
            delay = std::time::Duration::from_millis(100);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_secs(5));
    }
}

/// Flush every currently-unclaimed `pending_delivery` row for the
/// given recipient to the given resource. Release failures are retained in the
/// returned outcome for the retry pump; this single pass never waits for a
/// failing release backend to recover.
///
/// Called by the presence handler once `claim_offline_flush()` has
/// returned `true` on the recovering [`ConnectionEntry`] — i.e. the
/// first non-negative-priority presence of a fresh session.
///
/// `ctx` carries the optional / contextual parameters
/// (`sm_session`, `blocking_storage`, `archive_resolver`,
/// `server_domain`) — see [`FlushContext`] for details.
#[instrument(skip(storage, registry, ctx, recipient, resource), fields(recipient = %recipient, resource = %resource))]
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
    let mut failed_releases = Vec::new();
    let mut outcome = flush_for_resource_inner(
        storage,
        registry,
        recipient,
        resource,
        ctx,
        &mut failed_releases,
        false,
    )
    .await;
    outcome.failed_releases = failed_releases;
    outcome
}

/// Exact claims still awaiting an ordering-deferred release.
#[derive(Debug, PartialEq, Eq)]
struct DeferredClaimRelease {
    row_id: PendingRowId,
    session: SmSessionId,
    token: PendingClaimToken,
}

async fn flush_for_resource_inner<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    recipient: &BareJid,
    resource: &FullJid,
    ctx: FlushContext<'_, R>,
    failed_releases: &mut Vec<DeferredClaimRelease>,
    single_batch: bool,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    let FlushContext {
        server_domain,
        sm_session,
        blocking_storage,
        owner,
        archive_resolver,
        dispatch_gate,
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
    let blocklist: Option<waddle_xmpp::protocol::Blocklist> = match blocking_storage {
        Some(bs) => match bs.list_blocked_jid_entries(recipient).await {
            Ok(jids) => Some(waddle_xmpp::protocol::Blocklist::new(jids)),
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
    let mut outcome = FlushOutcome::default();
    // Drain the backlog in bounded FIFO batches (issue #1220) instead of one
    // unbounded `claim_for_session`. `FLUSH_BATCH_SIZE` is deliberately « the
    // 256-slot recipient outbound mpsc so a single batch cannot fill the
    // channel; combined with the off-task spawn in `regular.rs`, the flush
    // producer backpressures on the mpsc while the recipient's connection task
    // keeps draining it — the >256-row self-send deadlock is gone. `cursor` is
    // the FIFO row-id boundary that isolates each batch from the last (see
    // `PendingDeliveryStorage::claim_batch_for_session`).
    let mut cursor: Option<PendingRowId> = None;
    'batches: loop {
        let claim_token = PendingClaimToken::fresh();
        let claimed = if dispatch_gate.is_some() {
            storage
                .claim_archive_ordered_batch_for_session(
                    recipient,
                    session_id_for_claim,
                    &claim_token,
                    FLUSH_BATCH_SIZE,
                )
                .await
        } else {
            storage
                .claim_batch_for_session(
                    recipient,
                    session_id_for_claim,
                    cursor.as_ref(),
                    FLUSH_BATCH_SIZE,
                )
                .await
        };
        let batch = match claimed {
            Ok(rows) => rows,
            Err(PendingStorageError::ClaimContended) => {
                outcome.deferred_transient += 1;
                outcome.deferred_ordering += 1;
                break 'batches;
            }
            Err(error) => {
                warn!(error = %error, "claim_batch_for_session failed; ending flush");
                outcome.deferred_transient += 1;
                break 'batches;
            }
        };
        if batch.is_empty() {
            break 'batches;
        }
        let batch_len = batch.len();
        outcome.claimed += batch_len as u32;
        outcome.batches += 1;
        // Advance the cursor to the last claimed row so the next batch starts
        // strictly after it — even rows released below (blocked / undelivered)
        // are not retried within this pass; they re-arm on the next flush
        // trigger, exactly as the pre-batch single claim behaved.
        cursor = batch.last().map(|row| row.id.clone());

        let mut rows = batch.into_iter();
        while let Some(row) = rows.next() {
            let payload = match materialize(&row, archive_resolver).await {
                Ok(Some(payload)) => payload,
                Ok(None) => {
                    // Archived row whose MAM lookup definitively found
                    // nothing usable (row missing, tombstoned, or its
                    // preserved XML is unparseable) — the original stanza
                    // is unrecoverable. Drop the row instead of releasing
                    // it so we don't loop forever on a poison pill. The
                    // message is permanently lost from the recipient's
                    // perspective; we surface it loudly so production
                    // logs can flag MAM corruption / unexpected
                    // tombstones.
                    outcome.unresolved += 1;
                    waddle_xmpp::telemetry::reliability::increment_pending_delivery_unresolved_poison_pill();
                    if let Err(error) = storage.delete_row(&row.id).await {
                        warn!(
                            row_id = %row.id,
                            error = %error,
                            "pending_delivery delete_row (unresolved poison pill) failed"
                        );
                    }
                    continue;
                }
                Err(error) => {
                    // Issue #1122: the MAM lookup ERRORED — a transient
                    // storage failure, not a missing row. Deleting here
                    // would let a momentary MAM outage destroy queued
                    // offline mail. The failure is BATCH-FATAL:
                    // pending_delivery is FIFO (XEP-0160 §3 order of
                    // receipt), so delivering later rows while releasing
                    // this one would reorder the recipient's offline
                    // mail, and a hard MAM outage would otherwise cost
                    // one failing lookup per remaining archived row,
                    // awaited inline in the presence handler. Release
                    // this row and every remaining claimed row, then
                    // abort the flush. Retry fires on the client's next
                    // presence update — `maybe_flush_pending_delivery`
                    // resets the connection's offline-flush CAS when
                    // `deferred_transient > 0` — or via another
                    // recovering resource.
                    outcome.deferred_transient += 1;
                    waddle_xmpp::telemetry::reliability::increment_pending_delivery_archive_lookup_transient_failure();
                    warn!(
                        row_id = %row.id,
                        error = %error,
                        "pending_delivery archive lookup failed transiently; \
                         aborting flush batch and releasing this row and all \
                         remaining claimed rows for retry"
                    );
                    release_row_or_warn(storage, &row.id, "transient archive failure").await;
                    for deferred in rows.by_ref() {
                        outcome.deferred_transient += 1;
                        release_row_or_warn(
                            storage,
                            &deferred.id,
                            "transient archive failure (batch abort)",
                        )
                        .await;
                    }
                    // Batch-fatal AND flush-fatal: claim no further batches.
                    break 'batches;
                }
            };
            // XEP-0191 §2 step 4 flush-time block re-evaluation
            // (issue #209 PR #360): if the recipient blocked the sender
            // after the row was queued, drop it. Block is final until
            // the recipient lifts it — `delete_row` not `release_row`.
            if let Some(blocked) = blocklist.as_ref() {
                let sender_jid = sender_jid_for_payload(&payload);
                if let Some(sender) = sender_jid {
                    if blocked.contains_jid(sender) {
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
            if row.payload.is_archived() {
                if let Some(gate) = dispatch_gate {
                    let ready = match gate.check_turn(&row, resource, sm_session).await {
                        Ok(PendingDispatchReadiness::Ready) => true,
                        Ok(PendingDispatchReadiness::Deferred) => false,
                        Err(error) => {
                            warn!(row_id = %row.id, error = %error, "pending archive dispatch check failed");
                            false
                        }
                    };
                    if !ready {
                        outcome.deferred_transient += 1;
                        outcome.deferred_ordering += 1;
                        release_ordering_claim(
                            storage,
                            &row.id,
                            session_id_for_claim,
                            &claim_token,
                            failed_releases,
                        )
                        .await;
                        for deferred in rows.by_ref() {
                            outcome.deferred_transient += 1;
                            outcome.deferred_ordering += 1;
                            release_ordering_claim(
                                storage,
                                &deferred.id,
                                session_id_for_claim,
                                &claim_token,
                                failed_releases,
                            )
                            .await;
                        }
                        break 'batches;
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
            if dispatch_gate.is_some() {
                // Reserve capacity before committing the durable offer marker.
                // Recovery never waits for a full receiver and never cancels a
                // future between a committed offer and its synchronous enqueue.
                let reservation = if single_batch {
                    registry.try_reserve_pending_flush(resource, owner)
                } else {
                    registry.reserve_pending_flush(resource, owner).await
                };
                let sent = match reservation {
                    Ok(reservation) => {
                        let outbound = if sm_session.is_some() {
                            OutboundStanza::for_pending_flush(
                                stanza,
                                row.id.clone(),
                                row.original_receipt_at,
                            )
                        } else {
                            OutboundStanza::new(stanza)
                        };
                        attempt_offer(
                            storage,
                            registry,
                            PendingOffer {
                                claim: PendingClaim {
                                    row_id: row.id.clone(),
                                    session: session_id_for_claim.clone(),
                                    token: claim_token,
                                },
                                reservation,
                                outbound,
                                sm_enabled: sm_session.is_some(),
                            },
                            &mut outcome,
                        )
                        .await
                    }
                    Err(_) => {
                        release_ordering_claim(
                            storage,
                            &row.id,
                            session_id_for_claim,
                            &claim_token,
                            failed_releases,
                        )
                        .await;
                        false
                    }
                };
                if sent {
                    continue;
                }
                // A stolen claim or unresolved offer aborts the entire suffix,
                // including transient rows; the stale producer never skips A
                // and queues B ahead of its replacement.
                outcome.deferred_transient += 1;
                outcome.deferred_ordering += 1;
                for deferred in rows.by_ref() {
                    outcome.deferred_transient += 1;
                    outcome.deferred_ordering += 1;
                    release_ordering_claim(
                        storage,
                        &deferred.id,
                        session_id_for_claim,
                        &claim_token,
                        failed_releases,
                    )
                    .await;
                }
                break 'batches;
            }
            // SM-enabled path: tag outbound with row id so the recipient's
            // main loop can stamp `outbound_sequence` post-`record_outbound`.
            // The row stays claimed for the SM-ack lifecycle.
            // Non-SM path: same outbound tag (cheap), but we delete on Sent
            // because there's no SM session to ack against.
            let push_result = if sm_session.is_some() {
                // Owner-gate the SM push when the caller provided an ownership
                // token (issue #1220 review): binds the flush to the session
                // it was planned for so a same-full-JID replacement racing in
                // mid-flush cannot receive rows claimed under the original
                // stream id. On mismatch the send returns NotConnected and the
                // `other =>` arm below releases this row and the rest for the
                // replacement's own flush.
                match owner {
                    Some(owner) => {
                        registry
                            .send_pending_flush_if_owner(
                                resource,
                                owner,
                                stanza,
                                row.id.clone(),
                                row.original_receipt_at,
                            )
                            .await
                    }
                    None => {
                        registry
                            .send_pending_flush(
                                resource,
                                stanza,
                                row.id.clone(),
                                row.original_receipt_at,
                            )
                            .await
                    }
                }
            } else {
                // Non-SM path: gate at the actual send too. An ungated
                // send_to would let a silent same-FullJID replacement that
                // registered mid-flush receive the row — and the delete-on-
                // Sent below would then erase the durable copy for a session
                // the flush was never planned for.
                match owner {
                    Some(owner) => registry.send_to_if_owner(resource, owner, stanza).await,
                    None => registry.send_to(resource, stanza).await,
                }
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
                    // The recipient's channel is gone (NotConnected / ChannelClosed
                    // are the only non-Sent outcomes of the blocking send). Every
                    // remaining row would fail identically, so release this row and
                    // the rest of the batch and stop the flush — the rows re-arm on
                    // the next flush trigger. Do NOT claim further batches.
                    release_row_or_warn(storage, &row.id, "undelivered (channel gone)").await;
                    for undelivered in rows.by_ref() {
                        release_row_or_warn(
                            storage,
                            &undelivered.id,
                            "undelivered (channel gone, batch abort)",
                        )
                        .await;
                    }
                    break 'batches;
                }
            }
        }
        // A short batch means the unclaimed backlog is drained.
        if single_batch || batch_len < FLUSH_BATCH_SIZE {
            break 'batches;
        }
    }

    outcome
}

/// One bounded recovery step. Retry custody is returned to the fair janitor
/// scheduler rather than creating another sleeping delivery pump.
pub(crate) async fn flush_recovery_pass<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    recipient: &BareJid,
    resource: &FullJid,
    ctx: FlushContext<'_, R>,
    mut outcome: FlushOutcome,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    settle_retry_work(storage, registry, &mut outcome).await;
    if outcome.has_retry_work() {
        return outcome;
    }
    if let Some(owner) = ctx.owner {
        let Some(entry) = registry.entry_if_owner(resource, owner) else {
            return outcome;
        };
        if !entry.is_locally_hosted()
            || !entry.is_presence_available()
            || entry.presence_priority() < 0
        {
            return outcome;
        }
    }
    let mut failed_releases = Vec::new();
    let mut next = flush_for_resource_inner(
        storage,
        registry,
        recipient,
        resource,
        ctx,
        &mut failed_releases,
        true,
    )
    .await;
    next.failed_releases = failed_releases;
    outcome.absorb(next);
    outcome
}

async fn attempt_offer(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    offer: PendingOffer,
    outcome: &mut FlushOutcome,
) -> bool {
    match storage.mark_claim_offered(&offer.claim).await {
        Err(error) => {
            warn!(row_id = %offer.claim.row_id, %error, "offer reservation uncertain; retaining exact reconciliation custody");
            outcome.pending_offers.push(offer);
            return false;
        }
        Ok(false) => return false,
        Ok(true) => {}
    }
    // Deliberately no await between observed ownership and guarded enqueue.
    let PendingOffer {
        claim,
        reservation,
        outbound,
        sm_enabled,
    } = offer;
    if registry
        .send_reserved_pending_flush(reservation, outbound)
        .is_sent()
    {
        outcome.pushed += 1;
        if !sm_enabled {
            if let Err(error) = storage.delete_row(&claim.row_id).await {
                warn!(row_id = %claim.row_id, %error, "non-SM offered pending row delete failed");
            }
        }
        true
    } else {
        // The registry proved no enqueue occurred. Only this exact owner may
        // abandon a possibly committed reservation; scanners cannot do so.
        if let Err(error) = storage.release_unqueued_offer(&claim).await {
            warn!(row_id = %claim.row_id, %error, "unqueued offer cleanup failed; retaining custody");
            outcome.unqueued_offers.push(claim);
        }
        false
    }
}

async fn settle_retry_work(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    outcome: &mut FlushOutcome,
) {
    for offer in std::mem::take(&mut outcome.pending_offers) {
        attempt_offer(storage, registry, offer, outcome).await;
    }
    for claim in std::mem::take(&mut outcome.unqueued_offers) {
        if let Err(error) = storage.release_unqueued_offer(&claim).await {
            warn!(row_id = %claim.row_id, %error, "unqueued offer cleanup still pending");
            outcome.unqueued_offers.push(claim);
        }
    }
    retry_ordering_releases(storage, &mut outcome.failed_releases).await;
}

async fn release_ordering_claim(
    storage: &Arc<dyn PendingDeliveryStorage>,
    row_id: &PendingRowId,
    session: &SmSessionId,
    token: &PendingClaimToken,
    failed_releases: &mut Vec<DeferredClaimRelease>,
) {
    if let Err(error) = storage
        .release_unpushed_row_if_session(row_id, session, token)
        .await
    {
        warn!(%row_id, %error, "ordering-deferred claim release failed; retaining for retry");
        failed_releases.push(DeferredClaimRelease {
            row_id: row_id.clone(),
            session: session.clone(),
            token: *token,
        });
    }
}

async fn retry_ordering_releases(
    storage: &Arc<dyn PendingDeliveryStorage>,
    failed_releases: &mut Vec<DeferredClaimRelease>,
) {
    for claim in std::mem::take(failed_releases) {
        release_ordering_claim(
            storage,
            &claim.row_id,
            &claim.session,
            &claim.token,
            failed_releases,
        )
        .await;
    }
}

/// Release a row's flush claim, downgrading a release failure to a
/// warning: the claim-expiry janitor will release the row once the
/// claiming session expires.
async fn release_row_or_warn(
    storage: &Arc<dyn PendingDeliveryStorage>,
    row_id: &PendingRowId,
    context: &'static str,
) {
    if let Err(error) = storage.release_row(row_id).await {
        warn!(
            row_id = %row_id,
            error = %error,
            context,
            "pending_delivery release_row failed; claim-expiry janitor \
             will release it once the session expires"
        );
    }
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
    /// row.
    ///
    /// The three outcomes are semantically distinct (issue #1122):
    ///
    /// - `Ok(Some(message))` — resolved; the caller replays it.
    /// - `Ok(None)` — definitive miss (no archive row, tombstone, or
    ///   unparseable preserved XML). The caller treats this as a
    ///   poison pill and deletes the `pending_delivery` row.
    /// - `Err(_)` — *transient* lookup failure (MAM storage outage).
    ///   Batch-fatal for the caller: it releases this row's claim AND
    ///   every remaining claimed row, then aborts the flush so FIFO
    ///   order is preserved; the queued messages are never destroyed
    ///   by a momentary outage. Permanent decode failures
    ///   (`Serialization` / `InvalidQuery` row corruption) must be
    ///   reported as `Ok(None)`, never as `Err(_)`.
    async fn resolve(
        &self,
        stanza_id: &StanzaId,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError>;
}

/// Transient failure resolving an Archived `pending_delivery` row
/// against MAM (issue #1122). Distinct from a genuine miss
/// (`Ok(None)`): the flush loop releases the row for retry instead of
/// poison-pill-deleting it.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveResolveError {
    /// The MAM storage lookup itself failed (DB outage, timeout, …).
    /// Carries only *retryable* storage errors: permanent decode
    /// failures (`MamStorageError::Serialization` / `InvalidQuery`)
    /// are mapped to `Ok(None)` poison pills by
    /// [`MamArchiveResolver::resolve`] and never reach this variant.
    #[error("MAM storage lookup failed: {0}")]
    Storage(waddle_xmpp::mam::storage::MamStorageError),
}

/// MAM-backed resolver for production use.
pub struct MamArchiveResolver {
    pub mam_storage: Arc<dyn waddle_xmpp::mam::storage::MamStorage>,
}

#[async_trait::async_trait]
impl ArchiveResolver for MamArchiveResolver {
    async fn resolve(
        &self,
        stanza_id: &StanzaId,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError> {
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
            // Definitive miss: no archive row for this id. Poison
            // pill — the caller deletes the pending row.
            Ok(None) => return Ok(None),
            // Permanent, retry-can-never-succeed conditions — genuine
            // poison pills. `Ok(None)` deletes the row and fires the
            // loud corruption counter instead of retrying forever under
            // the "no mail lost" transient counter:
            //   - `Serialization`: bad timestamp/JID column, surfaced by
            //     decode_sqlite_message_row / decode_postgres_message_row.
            //   - `InvalidQuery`: an unusable stored id.
            //   - `NotFound`: a definitive miss. The lookup returns
            //     `Ok(None)` for a genuine miss today, so this is
            //     unreachable here, but classifying it as a poison pill
            //     (not transient) keeps a future refactor that surfaces
            //     `NotFound` from wedging the row in an immortal retry.
            Err(
                error @ (waddle_xmpp::mam::storage::MamStorageError::Serialization(_)
                | waddle_xmpp::mam::storage::MamStorageError::InvalidOrdinal(_)
                | waddle_xmpp::mam::storage::MamStorageError::MissingOrdinal(_)
                | waddle_xmpp::mam::storage::MamStorageError::InvalidQuery(_)
                | waddle_xmpp::mam::storage::MamStorageError::NotFound(_)),
            ) => {
                warn!(
                    error = %error,
                    archive_jid = %archive_bare,
                    stanza_id = %stanza_id,
                    "MAM lookup resolved to a permanent miss during flush; treating as poison pill"
                );
                return Ok(None);
            }
            // Transient storage failure (issue #1122): propagate the
            // typed error so the caller RELEASES the row (and the
            // rest of the claimed batch) for retry instead of
            // destroying queued mail during a MAM outage.
            //
            // `NotOwner`/`ClusterColocationMismatch` (ADR-0017 Phase 3
            // Slice 7 FIX 1) are defensive-only here: both are only ever
            // returned by `store_message_fenced` (a write path), never by
            // this read lookup. Matched exhaustively so a future
            // `MamStorageError` variant forces this classification to be
            // revisited; treated as transient (not a poison pill) since
            // treating a fencing/co-location condition as "permanently
            // unresolvable" would risk deleting a pending row that could
            // still resolve once ownership/co-location is restored.
            Err(
                error @ (waddle_xmpp::mam::storage::MamStorageError::Database(_)
                | waddle_xmpp::mam::storage::MamStorageError::OrdinalExhausted
                | waddle_xmpp::mam::storage::MamStorageError::NotOwner { .. }
                | waddle_xmpp::mam::storage::MamStorageError::ClusterColocationMismatch {
                    ..
                }),
            ) => {
                warn!(
                    error = %error,
                    archive_jid = %archive_bare,
                    stanza_id = %stanza_id,
                    "MAM lookup failed transiently during flush"
                );
                return Err(ArchiveResolveError::Storage(error));
            }
        };
        // Parse the preserved wire XML back into a typed Message. The
        // archived row includes server-stamped <stanza-id> by recipient
        // bare, so the parsed Message already carries the XEP-0359
        // identifier required by locked Q5c.
        //
        // An absent or unparseable `stanza_xml` is row corruption, not
        // a transient condition — retrying can never succeed, so these
        // stay `Ok(None)` poison pills.
        let Some(stanza_xml) = archived.stanza_xml.as_deref() else {
            return Ok(None);
        };
        let Ok(element) = stanza_xml.parse::<xmpp_parsers::minidom::Element>() else {
            return Ok(None);
        };
        Ok(xmpp_parsers::message::Message::try_from(element).ok())
    }
}

/// No-op resolver for tests that only exercise Transient rows.
#[derive(Debug, Default)]
pub struct NullArchiveResolver;

#[async_trait::async_trait]
impl ArchiveResolver for NullArchiveResolver {
    async fn resolve(
        &self,
        _stanza_id: &StanzaId,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError> {
        Ok(None)
    }
}

async fn materialize<R>(
    row: &PendingRow,
    resolver: &R,
) -> Result<Option<MaterializedPayload>, ArchiveResolveError>
where
    R: ArchiveResolver + ?Sized,
{
    match &row.payload {
        PendingPayload::Transient(_) => Ok(MaterializedPayload::from_transient(row)),
        PendingPayload::Archived(stanza_id) => Ok(resolver
            .resolve(stanza_id)
            .await?
            .map(|archived| MaterializedPayload::Archived(Box::new(archived)))),
    }
}

/// Extract the sender's JID from a materialized payload for the
/// XEP-0191 §2 step 4 flush-time block re-evaluation. Returns `None`
/// when the message has no `from` attribute (server-origin replays
/// have no flesh-and-blood sender to block).
fn sender_jid_for_payload(payload: &MaterializedPayload) -> Option<&jid::Jid> {
    let message: &xmpp_parsers::message::Message = match payload {
        MaterializedPayload::Archived(m) | MaterializedPayload::Transient(m) => m,
    };
    message.from.as_ref()
}
