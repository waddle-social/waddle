use super::*;

/// Maximum number of `pending_delivery` rows claimed and pushed per
/// batch by [`flush_for_resource`] (issue #1220). Deliberately far
/// smaller than the recovering connection's outbound channel capacity
/// (`OUTBOUND_CHANNEL_SIZE = 256`) so a large offline backlog drains in
/// paced batches whose per-batch push volume can never on its own fill
/// the mpsc — the flush claims the next batch only after the previous
/// one has been handed to the outbound channel.
pub const FLUSH_BATCH_SIZE: usize = 64;

/// Outcome of a flush attempt for one resource.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlushOutcome {
    /// Number of rows claimed from `pending_delivery`.
    pub claimed: u32,
    /// Number of bounded claim batches the flush processed (issue
    /// #1220). One per `claim_batch_for_session` that returned rows;
    /// a fully-drained backlog of `N` rows takes
    /// `ceil(N / FLUSH_BATCH_SIZE)` batches.
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
    /// Resolves Archived `PendingRow` references against MAM.
    pub archive_resolver: &'a R,
    /// The recovering connection's registry ownership token (the
    /// carbons handle returned by `register_*`), for the paced flush's
    /// per-batch ownership re-check (issue #1220). The flush now runs
    /// on a detached task, so between batches a same-full-JID
    /// replacement can supersede this connection; `send_pending_flush`
    /// routes by full JID, so without this check the detached flush
    /// would keep claiming batches under *this* session id and pushing
    /// them to the *replacement* connection — whose XEP-0198 `<a h>`
    /// (keyed by its own session id) can't delete them, so they orphan
    /// under the old session until the claim-expiry janitor releases
    /// them and a later flush re-delivers them (duplicate). Before each
    /// batch the flush verifies it still owns its slot via
    /// [`ConnectionRegistry::entry_if_owner`] and stops otherwise,
    /// bounding that exposure to at most the in-flight batch (client
    /// stanza-id dedup + the janitor reconcile the remainder). `None`
    /// skips the check (test fixtures / callers that flush inline).
    pub owner: Option<&'a std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
        owner,
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
    // Issue #1220: claim and push in bounded batches instead of one
    // unbounded claim + unpaced burst. Each `claim_batch_for_session`
    // tags at most `FLUSH_BATCH_SIZE` rows (deliberately « the 256-slot
    // outbound mpsc), pushes them, then — for the SM path — leaves them
    // claimed awaiting `<a h>` ack, so the next batch claims the *next*
    // unclaimed prefix and the loop walks the backlog forward in FIFO
    // order. The loop stops on a drained (short) batch, a transient MAM
    // abort, or a terminal push failure (channel closed / not
    // connected), releasing any still-claimed rows in the current batch
    // for a later re-flush.
    let mut outcome = FlushOutcome::default();
    'batches: loop {
        // Issue #1220: the flush runs on a detached task, so between
        // batches a same-full-JID replacement can supersede this
        // connection. `send_pending_flush` routes by full JID, so if we
        // kept claiming and pushing we would deliver *this* session's
        // claimed rows to the *replacement* — whose `<a h>` (keyed by
        // its own session id) can't delete them, orphaning them under
        // this session until the janitor releases them and a later
        // flush re-delivers (duplicate). Stop as soon as we no longer
        // own our registry slot; already-claimed prior-batch rows are
        // handled by the SM-ack lifecycle / claim-expiry janitor. The
        // check is per-batch (not per-row): the residual is at most the
        // in-flight batch, reconciled by client stanza-id dedup.
        if let Some(owner) = owner {
            if registry.entry_if_owner(resource, owner).is_none() {
                debug!(
                    "recovering connection no longer owns its registry slot; \
                     stopping paced pending_delivery flush"
                );
                break;
            }
        }
        let claimed = match storage
            .claim_batch_for_session(recipient, session_id_for_claim, FLUSH_BATCH_SIZE)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(error = %error, "claim_batch_for_session failed; aborting flush");
                break;
            }
        };
        if claimed.is_empty() {
            break;
        }
        let batch_len = claimed.len();
        outcome.claimed += batch_len as u32;
        outcome.batches += 1;

        let mut rows = claimed.into_iter();
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
                    //
                    // Deliberately DIFFERENT from the blocked-row path
                    // below: on a `delete_row` failure we leave the row
                    // claimed (recovered later by the claim-expiry
                    // janitor when the SM session expires) rather than
                    // `release_row`-ing it for retry. Releasing here would
                    // re-claim the poison pill on the very next batch —
                    // and a whole batch of delete-failing poison rows
                    // would spin the paced loop forever (a full batch is
                    // never "short", so it never terminates). Leaking one
                    // quota slot until session expiry is strictly better
                    // than a hot reclaim loop (issue #1220 review).
                    outcome.unresolved += 1;
                    waddle_xmpp::prometheus::increment_pending_delivery_unresolved_poison_pill();
                    if let Err(error) = storage.delete_row(&row.id).await {
                        warn!(
                            row_id = %row.id,
                            error = %error,
                            "pending_delivery delete_row (unresolved poison pill) failed; \
                             leaving row claimed for claim-expiry janitor recovery"
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
                    waddle_xmpp::prometheus::increment_pending_delivery_archive_lookup_transient_failure();
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
                    // Terminal for the whole flush (issue #1220): a
                    // non-`Sent` result means the recovering connection's
                    // outbound channel is closed or the resource is gone —
                    // every subsequent push would fail the same way, so
                    // stop instead of hammering. Release the undelivered row
                    // AND every remaining claimed row in this batch so a
                    // later recovering resource can re-flush them; rows in
                    // not-yet-claimed later batches are never touched.
                    debug!(
                        ?other,
                        row_id = %row.id,
                        "send to recovering resource failed mid-flush; releasing this and \
                         remaining claimed batch rows and stopping"
                    );
                    release_row_or_warn(storage, &row.id, "undelivered send failure").await;
                    for undelivered in rows.by_ref() {
                        release_row_or_warn(
                            storage,
                            &undelivered.id,
                            "undelivered send failure (batch abort)",
                        )
                        .await;
                    }
                    break 'batches;
                }
            }
        }

        // Batch fully processed with no terminal condition. A short
        // batch (fewer than the cap) means the backlog is drained.
        if batch_len < FLUSH_BATCH_SIZE {
            break;
        }
    }

    // Observability (issue #1220): make the paced drain visible so a
    // large offline backlog reads as many batches rather than one
    // unbounded burst. Accumulated once at the end from the outcome so
    // a mid-flush abort still reports exactly the work performed.
    if outcome.batches > 0 {
        waddle_xmpp::prometheus::add_pending_flush_batches(u64::from(outcome.batches));
    }
    if outcome.pushed > 0 {
        waddle_xmpp::prometheus::add_pending_flush_rows_pushed(u64::from(outcome.pushed));
    }

    outcome
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
