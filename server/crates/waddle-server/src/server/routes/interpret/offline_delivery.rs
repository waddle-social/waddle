use super::*;

pub(super) async fn queue_offline_delivery(
    deps: &Deps<'_>,
    recipient: BareJid,
    payload: waddle_xmpp::pending_delivery::PendingPayload,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    original_message: Box<Message>,
) {
    // XEP-0160 §3 step 2/4 — persist for later delivery.
    // The classifier and OfflineDeliveryHandler have already
    // applied XEP-0160 §4 type rules and the XEP-0334 hint
    // matrix; here we just write the row.
    let Some(storage) = deps.pending_delivery_storage else {
        warn!(
            recipient = %recipient,
            "QueueOfflineDelivery emitted but pending_delivery_storage is not wired; \
             dropping (test fixture or unwired deployment)"
        );
        return;
    };
    let notification_archive_stanza_id = match &payload {
        waddle_xmpp::pending_delivery::PendingPayload::Archived(stanza_id) => {
            Some(stanza_id.clone())
        }
        waddle_xmpp::pending_delivery::PendingPayload::Transient(_) => None,
    };
    let row_id = waddle_xmpp::pending_delivery::PendingRowId::fresh();
    let row = waddle_xmpp::pending_delivery::PendingRow {
        id: row_id.clone(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    match storage.insert(row).await {
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::Inserted) => {
            debug!(
                recipient = %recipient,
                "pending_delivery row inserted"
            );
            let outcome = enqueue_xep0357_notification_candidate(
                deps,
                &recipient,
                notification_archive_stanza_id.as_ref(),
            )
            .await;
            if notification_archive_stanza_id.is_some()
                && outcome == NotificationCandidateQueueOutcome::Completed
            {
                mark_pending_notification_outboxed(storage.as_ref(), &row_id, &recipient).await;
            }
        }
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::QuotaExceeded) => {
            waddle_xmpp::telemetry::reliability::increment_pending_delivery_quota_exceeded();
            // XEP-0160 §3 step 3 + RFC 6120 §8.3 — return a
            // typed `<service-unavailable/>` bounce that
            // echoes the original payload (RFC 6120 §8.3.4
            // convention).
            //
            // **Known partial inconsistency**: ArchiveHandler
            // runs earlier in the chain than
            // OfflineDeliveryHandler, so by the time we get
            // here the message is already in MAM. Sender
            // sees `<service-unavailable/>` while the
            // recipient can still pull the message from MAM
            // catch-up on next reconnect — i.e. the bounce
            // is for the *live-delivery* obligation, not
            // for archival visibility.
            //
            // This matches every existing reference XMPP
            // server (Prosody, ejabberd) and is consistent
            // with XEP-0160 §3 step 3's narrow scope
            // ("offline message queue is full"). The
            // alternative — un-archiving on quota — would
            // race with concurrent MAM queries and break
            // XEP-0313's monotonic-archive invariant.
            let error = xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Cancel,
                xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
                "en",
                "Recipient's offline message queue is full",
            );
            let bounce = waddle_xmpp::protocol::handlers::errors::message_error_reply(
                &original_message,
                error,
            );
            let sender_jid = match bounce.to.clone() {
                Some(j) => j,
                None => {
                    warn!(
                        recipient = %recipient,
                        "bounce target JID missing; dropping bounce"
                    );
                    return;
                }
            };
            let bounce_stanza = waddle_xmpp::Stanza::Message(bounce);
            let mut delivered = false;
            match sender_jid.clone().try_into_full() {
                Ok(full) => {
                    if matches!(
                        deps.connection_registry.send_to(&full, bounce_stanza).await,
                        waddle_xmpp::registry::SendResult::Sent
                    ) {
                        delivered = true;
                    }
                }
                Err(bare) => {
                    let resources = match deps.user_registry {
                        Some(user_registry) => {
                            waddle_xmpp::registry::get_resources_for_user(user_registry, &bare)
                                .await
                        }
                        None => Vec::new(),
                    };
                    for full in resources {
                        if matches!(
                            deps.connection_registry
                                .send_to(&full, bounce_stanza.clone())
                                .await,
                            waddle_xmpp::registry::SendResult::Sent
                        ) {
                            delivered = true;
                        }
                    }
                }
            }
            if delivered {
                warn!(
                    recipient = %recipient,
                    sender = %sender_jid,
                    "pending_delivery quota exceeded — bounced \
                     <service-unavailable/> to sender per XEP-0160 §3 step 3"
                );
            } else {
                // Sender is remote (cross-domain) or has no
                // resources currently bound. S2S routing of
                // the bounce is out of scope today; surface
                // the conformance gap loudly so it shows up
                // in deployment logs.
                warn!(
                    recipient = %recipient,
                    sender = %sender_jid,
                    "pending_delivery quota exceeded but \
                     <service-unavailable/> bounce was not \
                     deliverable (remote sender or no bound \
                     resource) — XEP-0160 §3 step 3 \
                     conformance gap until s2s lands"
                );
            }
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "pending_delivery insert failed"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationCandidateQueueOutcome {
    Completed,
    RetryLater,
}

async fn enqueue_xep0357_notification_candidate(
    deps: &Deps<'_>,
    recipient: &BareJid,
    archive_stanza_id: Option<&waddle_xmpp_core::xep0359::StanzaId>,
) -> NotificationCandidateQueueOutcome {
    let Some(archive_stanza_id) = archive_stanza_id else {
        debug!(
            recipient = %recipient,
            "Skipping XEP-0357 candidate for transient offline payload because no committed archive state exists"
        );
        return NotificationCandidateQueueOutcome::Completed;
    };
    let Some(state) = deps.web_socket_state else {
        return NotificationCandidateQueueOutcome::RetryLater;
    };
    enqueue_xep0357_notification_candidate_from_committed_archive(
        state,
        recipient,
        archive_stanza_id,
    )
    .await
}

async fn enqueue_xep0357_notification_candidate_from_committed_archive(
    state: &WebSocketState,
    recipient: &BareJid,
    archive_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
) -> NotificationCandidateQueueOutcome {
    let archive_bare = archive_stanza_id.by.to_bare();
    let archived = match state
        .deps
        .protocol
        .mam_storage
        .get_message_by_archive_or_stanza_id(&archive_bare, archive_stanza_id.as_str())
        .await
    {
        Ok(Some(archived)) => archived,
        Ok(None) => {
            warn!(
                recipient = %recipient,
                stanza_id = %archive_stanza_id,
                "XEP-0357 notification candidate skipped because committed MAM row is missing"
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                stanza_id = %archive_stanza_id,
                error = %error,
                "XEP-0357 notification candidate could not load committed MAM row"
            );
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    };
    let parsed_original_message =
        super::archive_lookup::parse_archived_message_xml(archived.stanza_xml.as_deref());
    let Some(sender_jid) = notification_sender_jid(&archived, parsed_original_message.as_ref())
    else {
        warn!(
            recipient = %recipient,
            stanza_id = %archive_stanza_id,
            archive_sender = %archived.from,
            "XEP-0357 notification candidate skipped because exact sender resource provenance is unavailable"
        );
        return NotificationCandidateQueueOutcome::Completed;
    };
    let sender = sender_jid.to_bare();
    let original_message = parsed_original_message
        .unwrap_or_else(|| super::archive_lookup::fallback_archived_message(&archived));
    enqueue_xep0357_notification_candidate_for_message(
        state,
        recipient,
        &sender,
        &sender_jid,
        archive_stanza_id,
        &original_message,
    )
    .await
}

fn notification_sender_jid(
    archived: &MamArchivedMessage,
    original_message: Option<&Message>,
) -> Option<Jid> {
    if let Some(from) = original_message.and_then(|message| message.from.clone()) {
        if let Some(_resource) = from.resource() {
            if archived.from.resource().is_some() {
                if from == archived.from {
                    return Some(from);
                }
            } else if from.to_bare() == archived.from.to_bare() {
                return Some(from);
            }
        }
        warn!(
            archive_sender = %archived.from,
            stanza_sender = %from,
            "Archived stanza XML sender conflicted with MAM row sender; skipping push candidate"
        );
        return None;
    }

    if archived.from.resource().is_some() {
        Some(archived.from.clone())
    } else {
        None
    }
}

async fn enqueue_xep0357_notification_candidate_for_message(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
    sender_jid: &Jid,
    archive_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    original_message: &Message,
) -> NotificationCandidateQueueOutcome {
    // XEP-0203 `<delay/>` filter NOT applied here (Copilot review on
    // PR #738): see the matching comment in `groupchat_inbox.rs`'s
    // `enqueue_groupchat_notification_candidate`. An earlier shape
    // suppressed pushes for messages carrying any `<delay/>`, but
    // that check is trivially spoofable — a sender can inject
    // `<delay/>` into their own stanza to suppress the recipient's
    // push. Until inbound `<delay/>` stripping lands at the C2S
    // session boundary, the safer behavior is to push as live and
    // accept the (currently theoretical) case where an S2S-buffered
    // delivery generates a real-time push (Waddle has no S2S yet).
    // XEP-0513 mention bit is message-intrinsic and frozen at T0; T1
    // reads it back from the candidate row when running the XEP-0492
    // dispatch gate.
    //
    // Self-directed candidates (sender bare JID == recipient bare JID)
    // are rejected at the `NotificationCandidate::direct_message`
    // constructor as `SelfDirectedNotificationCandidate` — no row is
    // persisted, satisfying the compliance requirement that
    // self-notifications produce no candidate/outbox entry. This is
    // input validation, not recipient-state suppression, so it lives
    // at the typed constructor boundary alongside the existing
    // full-sender-JID and archive-id owner checks.
    // Parse explicit mentions ONCE per message and derive both the
    // mention bit and the `<noping/>` bit from the same parsed
    // structure. The previous shape re-ran `extract_explicit_mentions`
    // twice per recipient (one for `is_mention`, one for noping); for
    // DM the recipient count is 1, but the same pattern is fanned out
    // N× in groupchat so the unified helper keeps both surfaces
    // consistent and avoids redundant XML traversals on the hot path.
    let RecipientMentionBits { is_mention, noping } =
        mention_bits_for_recipient(original_message, recipient);
    let hints = crate::notification_outbox::NotificationMessageHints::none()
        .with_noping(noping)
        .with_xep0334(
            waddle_xmpp::xep::xep0334::has_hint(
                original_message,
                waddle_xmpp::xep::xep0334::Hint::NoStore,
            ),
            waddle_xmpp::xep::xep0334::has_hint(
                original_message,
                waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
            ),
        )
        .with_reaction(waddle_xmpp::xep::xep0444::is_reaction_only_message(
            original_message,
        ));
    let candidate = match crate::notification_outbox::NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid.clone(),
        archive_stanza_id.clone(),
        is_mention,
        hints,
    ) {
        // Snapshot the body for the optional XEP-0357 §5.4
        // `last-message-body`; the candidate drops it when a XEP-0334
        // storage hint applies (off-the-record bodies are never
        // persisted).
        Ok(candidate) => candidate.with_last_message_body(super::prototype_body(
            original_message,
        )),
        Err(
            crate::notification_outbox::NotificationOutboxError::SelfDirectedNotificationCandidate(
                _,
            ),
        ) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                "XEP-0357 notification candidate skipped: self-directed (sender bare JID == recipient bare JID)"
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0357 notification candidate rejected"
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
    };
    // T0 push-gate evaluation — compliance: suppressed
    // outcomes leave no row in `notification_candidates`. The same
    // typed evaluator runs again at T1 inside
    // `drain_pending_candidates_into_outbox` as a race-window guard.
    // DM evaluation never consults `room_policy`, so the no-op
    // adapter is sufficient here; the per-call cache is a fresh empty
    // map (one-shot eval).
    let room_policy = crate::notification_outbox::NoopRoomPolicy;
    let mut room_policy_cache = std::collections::BTreeMap::<
        BareJid,
        crate::notification_outbox::RoomPolicyCacheEntry,
    >::new();
    let dnd_reader = crate::notification_outbox::NoopDndReader;
    let mut dnd_cache =
        std::collections::BTreeMap::<BareJid, crate::notification_outbox::DndState>::new();
    // T0 emission does NOT consult the activity reader — XEP-0513
    // `<active/>` is a T1-only gate (current activity is a T1 read
    // per the recipient-state contract). The `NoopActivityReader` is
    // wired only to satisfy the typed signature; it would never be
    // dispatched into at T0Emit even if the candidate class were
    // `ActiveChannelMention`.
    let activity_reader = crate::notification_activity::NoopActivityReader;
    let mut activity_cache = std::collections::BTreeMap::<
        (BareJid, BareJid),
        Option<crate::notification_activity::NotificationActivity>,
    >::new();
    let eval_deps = crate::notification_outbox::PushEvalDeps {
        settings_projection: state
            .deps
            .protocol
            .notification_settings_projection
            .as_ref(),
        room_policy: &room_policy,
        dnd_reader: &dnd_reader,
        activity_reader: &activity_reader,
        // T0 emission deliberately skips the XEP-0513 `<active/>`
        // filter (current activity is a T1 read), so the TTL is never
        // consulted here. Avoid the per-call env-var read by passing
        // the default-in-ms as a typed placeholder; T1 (which DOES
        // consult the TTL) reads the env-driven value at the drain
        // site (Copilot review on PR #731).
        active_mention_ttl_ms: (crate::notification_outbox::DEFAULT_ACTIVE_MENTION_TTL_SECONDS
            as i64)
            * 1_000,
    };
    let mut eval_caches = crate::notification_outbox::PushEvalCaches {
        room_policy: &mut room_policy_cache,
        dnd: &mut dnd_cache,
        activity: &mut activity_cache,
    };
    let outcome = match crate::notification_outbox::evaluate_push_gate_at_dispatch(
        crate::notification_outbox::PushEvalStage::T0Emit,
        eval_deps,
        &candidate,
        &mut eval_caches,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = ?error,
                "push gate evaluation failed at T0; deferring offline-delivery DM candidate"
            );
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    };
    match outcome {
        crate::notification_outbox::T1PushDispatchOutcome::Deliver { .. } => {}
        crate::notification_outbox::T1PushDispatchOutcome::Suppressed { reason } => {
            info!(
                recipient = %candidate.recipient_bare_jid(),
                conversation = %candidate.conversation_jid(),
                sender = %sender,
                notification_class = candidate.class().as_db_value(),
                push_stage = "suppressed",
                suppression_reason = reason.as_db_value(),
                "T0 push gate suppressed XEP-0357 DM candidate; no candidate row persisted"
            );
            waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                reason.telemetry_reason(),
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
        crate::notification_outbox::T1PushDispatchOutcome::DeferUnknownRoomPolicy => {
            // DM evaluation does not consult room_policy, so this is
            // a structural invariant violation. Fail-loud and retry.
            warn!(
                recipient = %recipient,
                sender = %sender,
                "XEP-0492 evaluator returned DeferUnknownRoomPolicy for a DM candidate; \
                 this is structurally impossible — retrying"
            );
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    }
    match state
        .deps
        .protocol
        .notification_outbox
        .insert_candidate(&candidate)
        .await
    {
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Inserted) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                is_mention,
                "XEP-0357 notification candidate inserted for durable outbox worker"
            );
            NotificationCandidateQueueOutcome::Completed
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                "Duplicate XEP-0357 notification candidate ignored"
            );
            NotificationCandidateQueueOutcome::Completed
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0357 notification candidate insert failed"
            );
            NotificationCandidateQueueOutcome::RetryLater
        }
    }
}

/// Typed pair of message-frozen XEP-0513 mention signals for a single
/// recipient, derived from one `extract_explicit_mentions` parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RecipientMentionBits {
    /// `true` when any `<mention jid='…'/>` names `recipient`,
    /// regardless of `<noping/>`. Matches the pre-dedup
    /// `ExplicitMentions::mentions_jid` predicate exactly — a `<noping/>`
    /// mention still counts as a mention for class derivation; the T1
    /// evaluator handles the `<noping/>` suppression independently via
    /// the [`Self::noping`] bit.
    is_mention: bool,
    /// `true` when any `<mention jid='…'/>` naming `recipient` also
    /// carries `<noping/>`. Message-frozen at T0 onto the candidate row;
    /// the T1 evaluator reads it back and suppresses with
    /// `SuppressedReason::Xep0513Noping`.
    noping: bool,
}

/// Single-pass derivation of `(is_mention, noping)` for a DM
/// recipient.
///
/// The recipient JID is the bare JID that owns the offline queue; that
/// is the canonical identity referenced by `<mention jid='…'/>` per
/// XEP-0513 §3. Channel-wide
/// `<mention mentions='urn:xmpp:mentions:0#channel'/>` is intentionally
/// NOT treated as an individual mention here — the XEP-0492
/// `<on-mention/>` semantics target explicit user mentions; the
/// channel-mention surface is for MUC reflector announcements, which
/// do not flow through the DM `QueueOfflineDelivery` arm.
fn mention_bits_for_recipient(message: &Message, recipient: &BareJid) -> RecipientMentionBits {
    // Pre-parse XEP-0513 mentions AND XEP-0372 references once so
    // the §304 count gate and the per-recipient is_mention/noping
    // derivation share the same pre-parsed slices.
    let mentions_slice: Vec<_> = waddle_xmpp::xep::extract_explicit_mentions(message)
        .map(|m| m.mentions)
        .unwrap_or_default();
    let references = waddle_xmpp::xep::extract_references_from_message(message);

    // Derive the per-recipient mention bits BEFORE applying the
    // §304 count gate. Both is_mention and noping flow from the
    // per-recipient match; the count gate then downgrades is_mention
    // (the "ignore all mentions" SHOULD) but PRESERVES the noping
    // suppression bit (XEP-0513 §"No Ping" is a separate SHOULD
    // that operates independently of the count cap — compliance
    // review on PR #741). Concretely:
    //   - Without `<noping/>`: count gate downgrades is_mention →
    //     class drops to plain `dm` (XEP-0492 `<on-mention/>`
    //     recipients then get suppressed at T1 via
    //     `Xep0492OnMentionMiss`).
    //   - With `<noping/>`: noping bit persists → the existing
    //     T0/T1 `Xep0513Noping` suppressor fires for that recipient
    //     even on overflowed messages. Spam-targeted `<always/>`
    //     recipients are correctly silenced by the sender's
    //     explicit `<noping/>`, matching the spec-mandated behavior
    //     for non-overflowed messages.
    let mut bits = RecipientMentionBits::default();
    for mention in &mentions_slice {
        let names_recipient = mention
            .jid
            .as_ref()
            .is_some_and(|mentioned| mentioned == recipient);
        if !names_recipient {
            continue;
        }
        bits.is_mention = true;
        if mention.noping {
            bits.noping = true;
        }
    }
    // XEP-0372 mention-reference fallback for the `is_mention` bit:
    // a DM sender naming the recipient via
    // `<reference type='mention' uri='xmpp:recipient@host'/>`
    // (instead of, or alongside, an XEP-0513 `<mention jid='…'/>`)
    // MUST still promote the recipient to `dm_mention` — otherwise
    // the count gate counts XEP-0372 references TOWARD the threshold
    // (promoting spam) but the classifier doesn't count them
    // FOR promotion (demoting legitimate XEP-0372 mentions to plain
    // dm). The groupchat path already does this in
    // `groupchat_mentions_owner`; mirror it here for DM consistency
    // (cross-XEP review on PR #741).
    if !bits.is_mention {
        bits.is_mention = references.iter().any(|reference| {
            reference.is_mention()
                && reference
                    .bare_jid()
                    .is_some_and(|mentioned| &mentioned == recipient)
        });
    }
    // XEP-0513 §304 "ignore all mentions" applied AFTER the noping
    // bit is captured: when the per-message count exceeds the
    // threshold, the recipient's mention-class is downgraded (plain
    // `dm`) but the explicit `<noping/>` suppression — being a
    // separate §"No Ping" SHOULD — is preserved.
    if waddle_xmpp::xep::mentions_exceed_threshold_from_parts(
        &mentions_slice,
        &references,
        waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT,
    ) {
        bits.is_mention = false;
    }
    bits
}

async fn mark_pending_notification_outboxed(
    storage: &dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage,
    row_id: &waddle_xmpp::pending_delivery::PendingRowId,
    recipient: &BareJid,
) -> bool {
    match storage.mark_notification_outboxed(row_id).await {
        Ok(_) => true,
        Err(error) => {
            warn!(
                recipient = %recipient,
                row_id = %row_id,
                error = %error,
                "pending_delivery notification outbox marker write failed; janitor will retry"
            );
            false
        }
    }
}

#[cfg(test)]
pub(crate) async fn reconcile_xep0357_notification_candidates(
    state: &WebSocketState,
    batch_size: usize,
) -> usize {
    reconcile_xep0357_notification_candidates_for_sweep(state, batch_size)
        .await
        .completed
}

pub(crate) async fn reconcile_xep0357_notification_candidates_for_sweep(
    state: &WebSocketState,
    batch_size: usize,
) -> super::NotificationRecoverySweepOutcome {
    let batch_size = batch_size.clamp(1, 1_000);
    let pending_storage = state.deps.protocol.pending_delivery_storage.as_ref();
    let rows = match pending_storage.list_unoutboxed_archived(batch_size).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                error = %error,
                "XEP-0357 notification candidate recovery could not read pending_delivery rows"
            );
            return super::NotificationRecoverySweepOutcome {
                completed: 0,
                had_failure: true,
            };
        }
    };
    let mut completed = 0usize;
    let mut had_failure = false;
    for row in rows {
        let waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id) =
            &row.payload
        else {
            continue;
        };
        let outcome = enqueue_xep0357_notification_candidate_from_committed_archive(
            state,
            &row.recipient,
            archive_stanza_id,
        )
        .await;
        match outcome {
            NotificationCandidateQueueOutcome::Completed => {
                if !mark_pending_notification_outboxed(pending_storage, &row.id, &row.recipient)
                    .await
                {
                    had_failure = true;
                }
                completed += 1;
            }
            NotificationCandidateQueueOutcome::RetryLater => had_failure = true,
        }
    }
    super::NotificationRecoverySweepOutcome {
        completed,
        had_failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare JID")
    }

    fn message_with_mention(mention: waddle_xmpp::xep::ExplicitMention) -> Message {
        let mut message = Message::new(None::<Jid>);
        message
            .payloads
            .push(waddle_xmpp::xep::build_mention_element(&mention));
        message
    }

    /// Dedup regression: `mention_bits_for_recipient` MUST derive
    /// both `is_mention` and `noping` from a single
    /// `extract_explicit_mentions` parse, and behavior MUST match the
    /// pre-dedup two-helper shape (mention bit set whenever a JID
    /// matches, noping bit set independently when `<noping/>` is
    /// present on the matching mention).
    #[test]
    fn dm_mention_bits_single_parse_matches_pre_dedup_behavior() {
        let recipient = bare("alice@example.com");

        // No mentions → both bits unset.
        let no_mention = Message::new(None::<Jid>);
        assert_eq!(
            mention_bits_for_recipient(&no_mention, &recipient),
            RecipientMentionBits::default(),
        );

        // Plain mention naming the recipient → mention set, noping unset.
        let plain = message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(recipient.clone()));
        assert_eq!(
            mention_bits_for_recipient(&plain, &recipient),
            RecipientMentionBits {
                is_mention: true,
                noping: false,
            },
        );

        // `<noping/>` mention naming the recipient → BOTH bits set;
        // matches the original `ExplicitMentions::mentions_jid`
        // semantics (which ignored `<noping/>` when deriving `is_mention`).
        let mut noping = waddle_xmpp::xep::ExplicitMention::jid(recipient.clone());
        noping.noping = true;
        let msg_noping = message_with_mention(noping);
        assert_eq!(
            mention_bits_for_recipient(&msg_noping, &recipient),
            RecipientMentionBits {
                is_mention: true,
                noping: true,
            },
        );

        // Mention naming someone else → both bits unset.
        let other = bare("bob@example.com");
        let foreign = message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(other));
        assert_eq!(
            mention_bits_for_recipient(&foreign, &recipient),
            RecipientMentionBits::default(),
        );
    }

    /// XEP-0513 §304 SHOULD: when the per-message count exceeds the
    /// threshold, the recipient's mention class is downgraded
    /// (is_mention → false, class drops from `dm_mention` to plain
    /// `dm`). The `<noping/>` suppression is NOT cleared — XEP-0513
    /// §"No Ping" is a separate SHOULD that operates independently
    /// of the count cap. Compliance review on PR #741 mandated this
    /// composition.
    #[test]
    fn xep0513_dm_mention_count_exceeded_downgrades_is_mention_but_preserves_noping() {
        let recipient = bare("alice@example.com");
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        // Case A: count exceeded, NO `<noping/>` for recipient → both
        // bits collapse (is_mention forced false; no noping to set).
        let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        msg.payloads.push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(recipient.clone()),
        ));
        for i in 0..threshold {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("user bare"),
                ),
            ));
        }
        let bits = mention_bits_for_recipient(&msg, &recipient);
        assert!(
            !bits.is_mention,
            "count exceeded MUST downgrade is_mention to false (§304 \
             ignore-all-mentions for the class decision)"
        );
        assert!(
            !bits.noping,
            "no `<noping/>` for the recipient → noping bit stays false"
        );

        // Case B: count exceeded AND `<noping/>` for recipient → the
        // class downgrades (is_mention=false) BUT the noping bit is
        // preserved so the existing T0/T1 `Xep0513Noping` suppressor
        // still fires for this recipient.
        let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        let mut recipient_noping_mention =
            waddle_xmpp::xep::ExplicitMention::jid(recipient.clone());
        recipient_noping_mention.noping = true;
        msg.payloads.push(waddle_xmpp::xep::build_mention_element(
            &recipient_noping_mention,
        ));
        for i in 0..threshold {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("user bare"),
                ),
            ));
        }
        let bits = mention_bits_for_recipient(&msg, &recipient);
        assert!(
            !bits.is_mention,
            "count exceeded MUST downgrade is_mention to false"
        );
        assert!(
            bits.noping,
            "XEP-0513 §\"No Ping\" SHOULD MUST survive §304 count \
             overflow — `<noping/>` suppresses push candidate creation \
             for this recipient regardless of the count cap"
        );
    }

    /// At-threshold (exactly `DEFAULT_MENTIONS_COUNT` mentions) MUST
    /// preserve the recipient's `<noping/>` suppression. The §304
    /// threshold is exclusive ("more than"), so equal-count is still
    /// honored.
    #[test]
    fn xep0513_dm_mention_count_at_threshold_preserves_noping() {
        let recipient = bare("alice@example.com");
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        let mut recipient_noping = waddle_xmpp::xep::ExplicitMention::jid(recipient.clone());
        recipient_noping.noping = true;
        msg.payloads
            .push(waddle_xmpp::xep::build_mention_element(&recipient_noping));
        // Fill to exactly threshold mentions total.
        for i in 0..(threshold - 1) {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("user bare"),
                ),
            ));
        }
        let bits = mention_bits_for_recipient(&msg, &recipient);
        assert!(
            bits.is_mention,
            "at-threshold count is still honored — is_mention MUST be set"
        );
        assert!(
            bits.noping,
            "at-threshold count is still honored — noping MUST be set"
        );
    }

    /// Cross-XEP review on PR #741: a DM sender naming the recipient
    /// solely via an XEP-0372 `<reference type='mention'/>` (no
    /// XEP-0513 `<mention/>`) MUST still set the recipient's
    /// `is_mention` bit. Without this the §304 count gate counts
    /// XEP-0372 toward the threshold (penalising spam) while the
    /// classifier ignores XEP-0372 for promotion (demoting
    /// legitimate XEP-0372-only DMs to plain `dm`).
    #[test]
    fn xep0372_only_dm_mention_promotes_recipient_to_dm_mention() {
        let recipient = bare("alice@example.com");
        let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        waddle_xmpp::xep::add_reference(
            &mut msg,
            &waddle_xmpp::xep::Reference::mention("xmpp:alice@example.com"),
        );
        let bits = mention_bits_for_recipient(&msg, &recipient);
        assert!(
            bits.is_mention,
            "a DM with only an XEP-0372 `<reference type='mention'/>` \
             naming the recipient MUST still set is_mention — the \
             groupchat path already does this via groupchat_mentions_owner"
        );
        assert!(
            !bits.noping,
            "XEP-0372 has no `<noping/>` concept; noping stays false"
        );
    }

    /// Composition with the §304 count gate: an XEP-0372-only mention
    /// still respects the threshold cap. Six references targeting
    /// six different users (including the recipient) trip the gate
    /// → recipient's `is_mention` is downgraded. XEP-0372 has no
    /// `<noping/>` concept so the noping bit is unaffected and the
    /// overall outcome matches `RecipientMentionBits::default()`.
    #[test]
    fn xep0372_dm_mention_count_exceeded_downgrades_is_mention() {
        let recipient = bare("alice@example.com");
        let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;
        // One reference naming the recipient + threshold others = exceed.
        waddle_xmpp::xep::add_reference(
            &mut msg,
            &waddle_xmpp::xep::Reference::mention("xmpp:alice@example.com"),
        );
        for i in 0..threshold {
            waddle_xmpp::xep::add_reference(
                &mut msg,
                &waddle_xmpp::xep::Reference::mention(format!("xmpp:user{i}@example.com")),
            );
        }
        let bits = mention_bits_for_recipient(&msg, &recipient);
        assert!(!bits.is_mention, "is_mention MUST be false on count-exceed");
        assert!(
            !bits.noping,
            "XEP-0372 has no `<noping/>` shape; noping stays false \
             (the §\"No Ping\" preservation rule applies only when the \
             recipient's XEP-0513 mention carries `<noping/>`)"
        );
    }
}
