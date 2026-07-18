//! T1 candidate drain: gate evaluation per candidate, coalescing insert
//! and merge of outbox jobs, and typed suppression audit writes.

use super::*;

/// Maximum T1 policy-deferral attempts before a candidate is
/// terminally suppressed. The bound is deliberately generous:
/// 48 attempts with the saturated retry delay is roughly four hours,
/// so transient blocklist, gate, or room-policy infrastructure faults
/// keep retrying while permanently unresolvable candidates stop
/// starving fresher rows.
const MAX_CANDIDATE_POLICY_ATTEMPTS: i64 = 48;

impl NotificationOutboxStore {
    pub async fn drain_pending_candidates_into_outbox(
        &self,
        push_store: &dyn PushSubscriptionStore,
        blocking_storage: &dyn BlockingStorage,
        settings_projection: &crate::notification_settings_projection::NotificationSettingsProjectionStore,
        deps: NotificationDrainDeps<'_>,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<usize, NotificationOutboxError> {
        let NotificationDrainDeps {
            room_policy,
            dnd_reader,
            activity_reader,
        } = deps;
        let candidates = self.pending_candidates(batch_size).await?;
        let mut target_cache =
            std::collections::BTreeMap::<BareJid, Vec<NotificationOutboxTarget>>::new();
        let mut room_policy_cache =
            std::collections::BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();
        let mut dnd_cache = std::collections::BTreeMap::<BareJid, DndState>::new();
        let mut activity_cache =
            std::collections::BTreeMap::<(BareJid, BareJid), Option<NotificationActivity>>::new();
        let active_mention_ttl_ms = active_mention_ttl_ms_from_env();
        let mut processed = 0usize;
        for candidate in candidates {
            // Self-DM filtering happens at the `NotificationCandidate`
            // constructor (`SelfDirectedNotificationCandidate` typed
            // error). A self-directed candidate is structurally
            // invalid and is rejected before it can be persisted, so
            // the T1 drain loop never observes one. See
            // `NotificationCandidate::direct_message` for the typed
            // boundary.
            match xep0191_blocks_notification_candidate(&candidate, blocking_storage).await {
                Ok(true) => {
                    let now_ms = crate::time::now_ms();
                    let mut tx = self.db.begin().await?;
                    record_candidate_suppressed_reason_tx(
                        &mut tx,
                        &candidate,
                        SuppressedReason::Xep0191Blocked,
                    )
                    .await?;
                    let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
                    tx.commit().await?;
                    if claimed > 0 {
                        tracing::info!(
                            recipient = %candidate.recipient_bare_jid(),
                            conversation = %candidate.conversation_jid(),
                            notification_class = candidate.class().as_db_value(),
                            push_stage = "suppressed",
                            suppression_reason = SuppressedReason::Xep0191Blocked.as_db_value(),
                            "push pipeline transition"
                        );
                        waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                            SuppressedReason::Xep0191Blocked.telemetry_reason(),
                        );
                        processed += 1;
                    }
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        recipient = %candidate.recipient_bare_jid(),
                        sender = %candidate.sender_jid(),
                        %error,
                        "XEP-0191 blocklist load failed; deferring notification candidate fail-closed"
                    );
                    if self.defer_candidate_policy_error(&candidate).await? {
                        processed += 1;
                    }
                    continue;
                }
            }
            // T1 push-gate re-evaluation — race-window guard,
            // defense-in-depth (XEP-0492 + XEP-0191 + XEP-0513 + XEP-0334 +
            // Waddle DnD).
            //
            // The same typed evaluator already ran at T0 (DM emission
            // in `offline_delivery.rs`, groupchat emission in
            // `groupchat_inbox.rs`) and a Suppressed outcome there
            // short-circuits the candidate insert entirely. Per the
            // compliance rule the common case is "no row in
            // `notification_candidates` for suppressed outcomes."
            //
            // This T1 invocation catches the race where recipient
            // state changed *between* the T0 emission and the T1
            // dispatch (e.g. the user flipped XEP-0492 to `<never/>`
            // mid-flight, or a groupchat config change toggled
            // members-only). If the projection has changed the drain
            // marks the candidate outboxed without enqueueing a job —
            // the row exists only briefly during the race window,
            // which is acceptable per the locked Q2 design (push
            // output is preserved).
            //
            // The class on the candidate is purely message-derived
            // from T0; combined with the recipient's effective
            // notification level (consulted fresh here against the
            // projection store) the typed reducer decides
            // publish-or-suppress. The room-policy lookup is cached
            // for the duration of this drain pass so a 100-member
            // groupchat does not produce 100 actor round-trips.
            let eval_deps = PushEvalDeps {
                settings_projection,
                room_policy,
                dnd_reader,
                activity_reader,
                active_mention_ttl_ms,
            };
            let mut eval_caches = PushEvalCaches {
                room_policy: &mut room_policy_cache,
                dnd: &mut dnd_cache,
                activity: &mut activity_cache,
            };
            let outcome = match evaluate_push_gate_at_dispatch(
                PushEvalStage::T1Drain,
                eval_deps,
                &candidate,
                &mut eval_caches,
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::warn!(
                        recipient = %candidate.recipient_bare_jid(),
                        conversation = %candidate.conversation_jid(),
                        error = ?error,
                        "push gate evaluation failed at T1; deferring candidate"
                    );
                    if self.defer_candidate_policy_error(&candidate).await? {
                        processed += 1;
                    }
                    continue;
                }
            };
            let rich = match outcome {
                T1PushDispatchOutcome::Suppressed { reason } => {
                    let now_ms = crate::time::now_ms();
                    let mut tx = self.db.begin().await?;
                    record_candidate_suppressed_reason_tx(&mut tx, &candidate, reason).await?;
                    let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
                    tx.commit().await?;
                    if claimed > 0 {
                        tracing::info!(
                            recipient = %candidate.recipient_bare_jid(),
                            conversation = %candidate.conversation_jid(),
                            sender = %candidate.sender_jid(),
                            notification_class = candidate.class().as_db_value(),
                            push_stage = "suppressed",
                            suppression_reason = reason.as_db_value(),
                            "T1 push gate suppressed XEP-0357 notification candidate"
                        );
                        waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                            reason.telemetry_reason(),
                        );
                        processed += 1;
                    }
                    continue;
                }
                T1PushDispatchOutcome::DeferUnknownRoomPolicy => {
                    // Actionable diagnostics for `Err(_)` lookups already
                    // fired exactly once per (drain batch, room) in
                    // `resolve_cached_room_policy`. The per-candidate
                    // deferral is `debug!` here so the cache-miss warn
                    // stays the single source-of-truth signal for
                    // operators triaging room-policy lookup failures.
                    tracing::debug!(
                        recipient = %candidate.recipient_bare_jid(),
                        conversation = %candidate.conversation_jid(),
                        class = ?candidate.class(),
                        "MUC config unavailable at T1; deferring candidate (unknown room policy is not 'public')"
                    );
                    if self.defer_candidate_policy_error(&candidate).await? {
                        processed += 1;
                    }
                    continue;
                }
                T1PushDispatchOutcome::Deliver { rich } => rich,
            };
            let recipient_key = candidate.recipient_bare_jid.clone();
            if !target_cache.contains_key(&recipient_key) {
                let resolved = resolve_first_party_targets(
                    push_store,
                    &candidate.recipient_bare_jid,
                    first_party_service_jid,
                )
                .await?;
                target_cache.insert(recipient_key.clone(), resolved);
            }
            let targets = target_cache
                .get(&recipient_key)
                .expect("target cache populated")
                .clone();
            if targets.is_empty() {
                let reason = SuppressedReason::Xep0357NoRegistration;
                let now_ms = crate::time::now_ms();
                let mut tx = self.db.begin().await?;
                record_candidate_suppressed_reason_tx(&mut tx, &candidate, reason).await?;
                let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
                tx.commit().await?;
                if claimed > 0 {
                    tracing::info!(
                        recipient = %candidate.recipient_bare_jid(),
                        conversation = %candidate.conversation_jid(),
                        notification_class = candidate.class().as_db_value(),
                        push_stage = "suppressed",
                        suppression_reason = reason.as_db_value(),
                        "push pipeline transition"
                    );
                    waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                        reason.telemetry_reason(),
                    );
                    processed += 1;
                }
                continue;
            }
            let context = build_waddle_context(&candidate);
            let now_ms = crate::time::now_ms();
            let mut tx = self.db.begin().await?;
            let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
            if claimed == 0 {
                tx.commit().await?;
                continue;
            }
            for target in &targets {
                enqueue_outbox_job_tx(&mut tx, &candidate, target, &context, &rich, now_ms).await?;
            }
            tx.commit().await?;
            processed += 1;
        }
        Ok(processed)
    }

    async fn defer_candidate_policy_error(
        &self,
        candidate: &NotificationCandidate,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let next_policy_error_count = candidate.policy_error_count + 1;
        if next_policy_error_count >= MAX_CANDIDATE_POLICY_ATTEMPTS {
            let reason = SuppressedReason::PolicyRetriesExhausted;
            let mut tx = self.db.begin().await?;
            tx.execute(
                r#"
                UPDATE notification_candidates
                SET policy_error_count = ?
                WHERE recipient_bare_jid = ?
                  AND conversation_jid = ?
                  AND sender_jid = ?
                  AND thread_id = ?
                  AND stanza_id_by = ?
                  AND stanza_id = ?
                  AND class = ?
                  AND outboxed_at_ms IS NULL
                "#,
                crate::db_params![
                    next_policy_error_count,
                    candidate.recipient_bare_jid.to_string(),
                    candidate.conversation_jid.to_string(),
                    candidate.sender_jid.to_string(),
                    candidate.thread_id.as_str(),
                    candidate.archive_stanza_id.by.to_string(),
                    candidate.archive_stanza_id.id.clone(),
                    candidate.class.as_db_value(),
                ],
            )
            .await?;
            record_candidate_suppressed_reason_tx(&mut tx, candidate, reason).await?;
            let claimed = mark_candidate_outboxed_tx(&mut tx, candidate, now_ms).await?;
            tx.commit().await?;
            if claimed > 0 {
                tracing::info!(
                    recipient = %candidate.recipient_bare_jid(),
                    conversation = %candidate.conversation_jid(),
                    notification_class = candidate.class().as_db_value(),
                    push_stage = "suppressed",
                    suppression_reason = reason.as_db_value(),
                    "push pipeline transition"
                );
                waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                    reason.telemetry_reason(),
                );
            }
            return Ok(claimed > 0);
        }
        self.execute(
            r#"
            UPDATE notification_candidates
            SET policy_error_count = ?,
                next_attempt_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                next_policy_error_count,
                now_ms.saturating_add(policy_retry_delay_ms(next_policy_error_count)),
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?;
        Ok(false)
    }

    pub async fn pending_candidates(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationCandidate>, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let mut rows = self
            .query(
                r#"
                SELECT recipient_bare_jid,
                       conversation_jid,
                       sender_jid,
                       thread_id,
                       stanza_id_by,
                       stanza_id,
                       class,
                       reason,
                       policy_error_count,
                       noping,
                       no_store,
                       no_permanent_store,
                       last_message_body,
                       reaction
                FROM notification_candidates
                WHERE outboxed_at_ms IS NULL
                  AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                ORDER BY created_at_ms ASC,
                         recipient_bare_jid ASC,
                         conversation_jid ASC,
                         sender_jid ASC,
                         thread_id ASC,
                         stanza_id_by ASC,
                         stanza_id ASC,
                         class ASC
                LIMIT ?
                "#,
                crate::db_params![crate::time::now_ms(), batch_size as i64],
            )
            .await?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await? {
            match decode_candidate(&row) {
                Ok(candidate) => candidates.push(candidate),
                Err(error) => {
                    self.mark_malformed_candidate_outboxed(&row, &error).await?;
                }
            }
        }
        Ok(candidates)
    }

    async fn mark_malformed_candidate_outboxed(
        &self,
        row: &Row,
        error: &NotificationOutboxError,
    ) -> Result<(), NotificationOutboxError> {
        let recipient_raw: String = row.get(0)?;
        let conversation_raw: String = row.get(1)?;
        let sender_raw = row
            .get::<Option<String>>(2)?
            .unwrap_or_else(|| "<null>".to_string());
        let thread_id: String = row.get(3)?;
        let stanza_id_by_raw: String = row.get(4)?;
        let stanza_id: String = row.get(5)?;
        let class: String = row.get(6)?;
        tracing::warn!(
            recipient = %recipient_raw,
            conversation = %conversation_raw,
            sender = %sender_raw,
            stanza_id = %stanza_id,
            %error,
            "dropping malformed XEP-0357 notification candidate fail-closed"
        );
        self.execute(
            r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                crate::time::now_ms(),
                recipient_raw,
                conversation_raw,
                thread_id,
                stanza_id_by_raw,
                stanza_id,
                class,
            ],
        )
        .await?;
        Ok(())
    }
}

pub(super) async fn enqueue_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context: &Element,
    rich: &RichSummary,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    // The durable schema stores XML as TEXT; keep protocol context typed until this DB write edge.
    let context_xml = String::from(context);
    for _ in 0..8 {
        let inserted =
            insert_outbox_job_tx(tx, candidate, target, context_xml.as_str(), rich, now_ms).await?;
        if inserted > 0 {
            return Ok(());
        }
        match merge_outbox_job_tx(tx, candidate, target, context_xml.as_str(), rich, now_ms).await?
        {
            OutboxMergeOutcome::Merged => return Ok(()),
            OutboxMergeOutcome::MalformedExistingJobFailed
            | OutboxMergeOutcome::QueuedJobNotFound
            | OutboxMergeOutcome::QueuedJobChanged => {}
        }
    }
    Err(NotificationOutboxError::OutboxCoalesceContention)
}

pub(super) async fn insert_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    rich: &RichSummary,
    now_ms: i64,
) -> Result<u64, NotificationOutboxError> {
    let job_id = NotificationOutboxJobId::fresh();
    let sender_jids = encode_sender_jids(std::slice::from_ref(&candidate.sender_jid))?;
    Ok(tx
        .execute(
            r#"
            INSERT INTO notification_outbox (
                job_id,
                recipient_bare_jid,
                push_service_jid,
                node,
                conversation_jid,
                sender_jid,
                sender_jids,
                thread_id,
                class,
                message_count,
                context_xml,
                summary_sender_jid,
                summary_body,
                status,
                attempt_count,
                policy_error_count,
                last_error,
                next_attempt_at_ms,
                claimed_at_ms,
                claim_token,
                created_at_ms,
                updated_at_ms,
                published_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, 0, 0, NULL, NULL, NULL, NULL, ?, ?, NULL)
            ON CONFLICT DO NOTHING
            "#,
            crate::db_params![
                job_id.as_str(),
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                sender_jids,
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                context_xml,
                rich.sender.as_ref().map(ToString::to_string),
                rich.body.clone(),
                STATUS_QUEUED,
                now_ms,
                now_ms,
            ],
        )
        .await?)
}

pub(super) enum OutboxMergeOutcome {
    Merged,
    MalformedExistingJobFailed,
    QueuedJobNotFound,
    QueuedJobChanged,
}

pub(super) async fn merge_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    rich: &RichSummary,
    now_ms: i64,
) -> Result<OutboxMergeOutcome, NotificationOutboxError> {
    let mut rows = tx
        .query(
            r#"
            SELECT job_id, sender_jid, sender_jids
            FROM notification_outbox
            WHERE recipient_bare_jid = ?
              AND push_service_jid = ?
              AND node = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND class = ?
              AND status = ?
            LIMIT 1
            "#,
            crate::db_params![
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                STATUS_QUEUED,
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(OutboxMergeOutcome::QueuedJobNotFound);
    };
    let job_id_raw: String = row.get(0)?;
    let sender_raw = row
        .get::<Option<String>>(1)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()));
    let sender_jids_raw = row
        .get::<Option<String>>(2)?
        .ok_or(NotificationOutboxError::MissingSenderJidSet);
    let existing_sender_jid = match sender_raw.and_then(|raw| {
        let sender_jid = raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidSenderJid(raw))?;
        require_full_sender_jid(&sender_jid)?;
        require_sender_matches_conversation(&sender_jid, &candidate.conversation_jid)?;
        Ok(sender_jid)
    }) {
        Ok(sender_jid) => sender_jid,
        Err(error) => {
            mark_malformed_outbox_job_failed_tx(
                tx,
                job_id_raw.as_str(),
                &error.to_string(),
                now_ms,
            )
            .await?;
            return Ok(OutboxMergeOutcome::MalformedExistingJobFailed);
        }
    };
    let mut sender_jids = match sender_jids_raw.and_then(|raw| {
        let sender_jids = decode_sender_jids(&raw)?;
        require_full_sender_jid_set(&sender_jids)?;
        require_sender_set_matches_conversation(&sender_jids, &candidate.conversation_jid)?;
        require_sender_set_contains_scalar(&sender_jids, &existing_sender_jid)?;
        Ok(sender_jids)
    }) {
        Ok(sender_jids) => sender_jids,
        Err(error) => {
            mark_malformed_outbox_job_failed_tx(
                tx,
                job_id_raw.as_str(),
                &error.to_string(),
                now_ms,
            )
            .await?;
            return Ok(OutboxMergeOutcome::MalformedExistingJobFailed);
        }
    };
    if !sender_jids
        .iter()
        .any(|sender_jid| sender_jid == &candidate.sender_jid)
    {
        sender_jids.push(candidate.sender_jid.clone());
    }
    let sender_jids = encode_sender_jids(&sender_jids)?;
    let affected = tx
        .execute(
            r#"
        UPDATE notification_outbox
        SET message_count = message_count + 1,
            context_xml = ?,
            sender_jid = ?,
            sender_jids = ?,
            summary_sender_jid = ?,
            summary_body = ?,
            policy_error_count = 0,
            last_error = NULL,
            next_attempt_at_ms = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
          AND status = ?
        "#,
            crate::db_params![
                context_xml,
                candidate.sender_jid.to_string(),
                sender_jids,
                rich.sender.as_ref().map(ToString::to_string),
                rich.body.clone(),
                now_ms,
                job_id_raw,
                STATUS_QUEUED,
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(OutboxMergeOutcome::QueuedJobChanged);
    }
    Ok(OutboxMergeOutcome::Merged)
}

pub(super) async fn mark_malformed_outbox_job_failed_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
    error: &str,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    tx.execute(
        r#"
        UPDATE notification_outbox
        SET status = ?,
            policy_error_count = 0,
            last_error = ?,
            next_attempt_at_ms = NULL,
            claimed_at_ms = NULL,
            claim_token = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
          AND status = ?
        "#,
        crate::db_params![
            STATUS_FAILED,
            format!("malformed notification outbox job: {error}"),
            now_ms,
            job_id,
            STATUS_QUEUED,
        ],
    )
    .await?;
    Ok(())
}

pub(super) async fn mark_candidate_outboxed_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    now_ms: i64,
) -> Result<u64, NotificationOutboxError> {
    Ok(tx
        .execute(
            r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                now_ms,
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?)
}

/// Records a typed [`SuppressedReason`] onto a not-yet-outboxed
/// candidate row inside an active transaction. Always called BEFORE
/// [`mark_candidate_outboxed_tx`] in the T1 suppression path so the
/// `suppressed_reason` column persists for the row's lifetime in the
/// outboxed-prune retention window.
pub(super) async fn record_candidate_suppressed_reason_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    reason: SuppressedReason,
) -> Result<u64, NotificationOutboxError> {
    Ok(tx
        .execute(
            r#"
            UPDATE notification_candidates
            SET suppressed_reason = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                reason.as_db_value(),
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?)
}

pub(super) async fn resolve_first_party_targets(
    push_store: &dyn PushSubscriptionStore,
    recipient: &BareJid,
    first_party_service_jid: &BareJid,
) -> Result<Vec<NotificationOutboxTarget>, NotificationOutboxError> {
    let registrations = push_store
        .get_for_user(&recipient.to_string())
        .await
        .map_err(|error| NotificationOutboxError::Push(error.to_string()))?;
    let first_party_service = first_party_service_jid.to_string();
    let mut targets = Vec::new();
    for registration in registrations {
        if registration.service_jid != first_party_service {
            continue;
        }
        match target_from_subscription(&registration) {
            Ok(Some(target)) if target.push_service_jid() == first_party_service_jid => {
                targets.push(target);
            }
            Ok(Some(target)) => {
                tracing::warn!(
                    recipient = %recipient,
                    registration_service = %registration.service_jid,
                    target_service = %target.push_service_jid(),
                    "first-party XEP-0357 registration target did not parse back to the configured service"
                );
            }
            Ok(None) => {
                tracing::warn!(
                    recipient = %recipient,
                    service = %registration.service_jid,
                    "first-party XEP-0357 registration missing node; skipping notification outbox target"
                );
            }
            Err(error) => {
                tracing::warn!(
                    recipient = %recipient,
                    error = %error,
                    "first-party XEP-0357 registration could not be converted into a notification outbox target"
                );
            }
        }
    }
    Ok(targets)
}
