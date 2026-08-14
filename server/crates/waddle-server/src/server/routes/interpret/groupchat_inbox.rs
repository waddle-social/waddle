use super::groupchat_archive::{
    extract_room_stanza_id, GroupchatInboxProjectionInputs, GroupchatInboxProjectionOutcome,
};
use super::*;

pub(super) struct ProjectGroupchatInboxEvent<'a, 'deps> {
    pub deps: &'a Deps<'deps>,
    pub owner: BareJid,
    pub room: BareJid,
    pub message: Box<Message>,
    pub is_recipient: bool,
    pub is_durable_recipient: bool,
    pub is_live_occupant: bool,
    pub room_members_only: bool,
    /// XEP-0513 §"Multi-User Chats Permissions": frozen at dispatch
    /// time. See [`waddle_xmpp::protocol::event::OutboundEvent::ProjectGroupchatInbox`]
    /// for full semantics.
    pub sender_can_broadcast_channel_mention: bool,
    pub thread: Option<GroupchatThreadProjection>,
    pub dispatch_timestamp: i64,
}

pub(super) async fn project_groupchat_inbox_event(input: ProjectGroupchatInboxEvent<'_, '_>) {
    let notification_recovery = groupchat_notification_recovery_item(&input);
    let ProjectGroupchatInboxEvent {
        deps,
        owner,
        room,
        message,
        is_recipient,
        is_durable_recipient,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        thread,
        dispatch_timestamp,
    } = input;
    let Some(inbox_storage) = deps.inbox_storage else {
        debug!(
            owner = %owner,
            room = %room,
            "ProjectGroupchatInbox: no inbox_storage in Deps; skipping (test fixture?)"
        );
        return;
    };
    let notification_recovery_key = notification_recovery
        .as_ref()
        .map(|recovery| recovery.key.clone());
    let outcome = project_groupchat_inbox(GroupchatInboxProjectionInputs {
        inbox_storage,
        connection_registry: deps.connection_registry,
        user_registry: deps.user_registry,
        owner: &owner,
        room: &room,
        message: &message,
        is_recipient,
        thread: &thread,
        dispatch_timestamp,
        notification_recovery,
    })
    .await;
    if let Some(mutation) = groupchat_inbox_mutation(&room, &thread, is_recipient, outcome) {
        deps.capture_intent(IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            mutation,
        });
    }
    maybe_enqueue_groupchat_notification_candidate(GroupchatNotificationProjection {
        deps,
        owner: &owner,
        room: &room,
        message: &message,
        is_recipient,
        is_durable_recipient,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        thread: &thread,
        outcome,
        recovery_key: notification_recovery_key.as_ref(),
    })
    .await;
}

struct GroupchatNotificationProjection<'a, 'deps> {
    deps: &'a Deps<'deps>,
    owner: &'a BareJid,
    room: &'a BareJid,
    message: &'a Message,
    is_recipient: bool,
    is_durable_recipient: bool,
    is_live_occupant: bool,
    room_members_only: bool,
    /// XEP-0513 frozen permission snapshot; see
    /// [`ProjectGroupchatInboxEvent::sender_can_broadcast_channel_mention`].
    sender_can_broadcast_channel_mention: bool,
    thread: &'a Option<GroupchatThreadProjection>,
    outcome: GroupchatInboxProjectionOutcome,
    recovery_key: Option<&'a waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey>,
}

fn groupchat_notification_recovery_item(
    event: &ProjectGroupchatInboxEvent<'_, '_>,
) -> Option<waddle_xmpp::inbox::storage::GroupchatNotificationRecovery> {
    if !event.is_recipient || !event.is_durable_recipient {
        return None;
    }
    let archive_id = extract_room_stanza_id(&event.message, &event.room)?;
    let sender_jid = event.message.from.clone()?;
    Some(waddle_xmpp::inbox::storage::GroupchatNotificationRecovery {
        key: waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey {
            recipient: event.owner.clone(),
            room: event.room.clone(),
            thread_id: event.thread.as_ref().map(|thread| thread.thread_id.clone()),
            archive_stanza_id: Xep0359StanzaId::new(archive_id, Jid::from(event.room.clone())),
        },
        sender_jid,
        is_live_occupant: event.is_live_occupant,
        room_members_only: event.room_members_only,
        sender_can_broadcast_channel_mention: event.sender_can_broadcast_channel_mention,
        created_at_ms: event.dispatch_timestamp.saturating_mul(1_000),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupchatNotificationCandidateQueueOutcome {
    Completed,
    RetryLater,
}

fn groupchat_inbox_mutation(
    room: &BareJid,
    thread: &Option<GroupchatThreadProjection>,
    is_recipient: bool,
    outcome: GroupchatInboxProjectionOutcome,
) -> Option<waddle_xmpp::ingress::InboxProjectionMutation> {
    match (
        outcome.channel_committed,
        outcome.thread_committed,
        thread.as_ref(),
    ) {
        (true, true, Some(thread)) => Some(
            waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelAndThread {
                room: room.clone(),
                thread_id: waddle_xmpp_core::mam::ThreadId::new(thread.thread_id.clone())?,
                increment_unread: is_recipient,
            },
        ),
        (true, false, _) => Some(
            waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannel {
                room: room.clone(),
                increment_unread: is_recipient,
            },
        ),
        (false, true, Some(thread)) => Some(
            waddle_xmpp::ingress::InboxProjectionMutation::GroupchatThread {
                room: room.clone(),
                thread_id: waddle_xmpp_core::mam::ThreadId::new(thread.thread_id.clone())?,
            },
        ),
        _ => None,
    }
}

async fn maybe_enqueue_groupchat_notification_candidate(
    input: GroupchatNotificationProjection<'_, '_>,
) {
    let GroupchatNotificationProjection {
        deps,
        owner,
        room,
        message,
        is_recipient,
        is_durable_recipient,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        thread,
        outcome,
        recovery_key,
    } = input;
    let queue_outcome = enqueue_groupchat_notification_candidate(GroupchatNotificationProjection {
        deps,
        owner,
        room,
        message,
        is_recipient,
        is_durable_recipient,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        thread,
        outcome,
        recovery_key,
    })
    .await;
    if queue_outcome == GroupchatNotificationCandidateQueueOutcome::Completed {
        if let Some(key) = recovery_key {
            mark_groupchat_notification_recovery_completed(deps, key).await;
        }
    }
}

async fn enqueue_groupchat_notification_candidate(
    input: GroupchatNotificationProjection<'_, '_>,
) -> GroupchatNotificationCandidateQueueOutcome {
    let GroupchatNotificationProjection {
        deps,
        owner,
        room,
        message,
        is_recipient,
        is_durable_recipient,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        thread,
        outcome,
        recovery_key: _,
    } = input;
    if !is_recipient || !is_durable_recipient {
        return GroupchatNotificationCandidateQueueOutcome::Completed;
    }
    // XEP-0203 `<delay/>` filter NOT applied here (Copilot review
    // on PR #738): an earlier shape of this path called
    // `xep0203::has_delay(message)` to suppress push for historical
    // replays. That check is trivially spoofable — a sender can
    // inject `<delay xmlns='urn:xmpp:delay' from='whatever'
    // stamp='2020-01-01T00:00:00Z'/>` into their own outbound
    // stanza, the server forwards it unchanged, and every recipient's
    // push gets suppressed. XEP-0203 §4.1 RECOMMENDS the `from`
    // attribute but does not mandate it, and the server's room
    // dispatcher does not add `<delay/>` for live messages — so any
    // `<delay/>` on this path is by definition user-supplied today
    // (Waddle has no S2S yet). The proper defense is inbound
    // `<delay/>` stripping at the C2S session boundary (deferred to
    // a follow-up slice); until that lands, blindly trusting
    // `<delay/>` here would create an unprivileged push-suppression
    // primitive. MAM-replay through
    // `reconcile_groupchat_notification_candidates` calls
    // `insert_groupchat_notification_candidate` directly, bypassing
    // this function, so MAM replays do not produce duplicate pushes.
    let projection_committed = thread
        .as_ref()
        .map_or(outcome.channel_committed, |_| outcome.thread_committed);
    if !projection_committed {
        return GroupchatNotificationCandidateQueueOutcome::Completed;
    }
    let Some(state) = deps.web_socket_state else {
        return GroupchatNotificationCandidateQueueOutcome::RetryLater;
    };
    let Some(archive_id) = extract_room_stanza_id(message, room) else {
        warn!(
            recipient = %owner,
            room = %room,
            "ProjectGroupchatInbox: skipping XEP-0357 candidate; message has no room stanza-id"
        );
        return GroupchatNotificationCandidateQueueOutcome::Completed;
    };
    let Some(sender_jid) = message.from.clone() else {
        warn!(
            recipient = %owner,
            room = %room,
            "ProjectGroupchatInbox: skipping XEP-0357 candidate; message has no sender"
        );
        return GroupchatNotificationCandidateQueueOutcome::Completed;
    };
    let thread_id = thread
        .as_ref()
        .map(|thread| {
            crate::notification_outbox::NotificationThreadId::new(thread.thread_id.clone())
        })
        .unwrap_or_else(crate::notification_outbox::NotificationThreadId::root);
    insert_groupchat_notification_candidate(GroupchatNotificationCandidateSeed {
        deps: Some(deps),
        state,
        owner,
        room,
        message,
        sender_jid,
        thread_id,
        archive_stanza_id: Xep0359StanzaId::new(archive_id, Jid::from(room.clone())),
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
    })
    .await
}

/// Inputs for [`insert_groupchat_notification_candidate`]: one
/// recipient's XEP-0357 candidate row, assembled either at T0 dispatch
/// (`enqueue_groupchat_notification_candidate`) or during restart
/// recovery (`reconcile_groupchat_notification_candidates`).
struct GroupchatNotificationCandidateSeed<'a> {
    deps: Option<&'a Deps<'a>>,
    state: &'a WebSocketState,
    owner: &'a BareJid,
    room: &'a BareJid,
    message: &'a Message,
    sender_jid: Jid,
    thread_id: crate::notification_outbox::NotificationThreadId,
    archive_stanza_id: Xep0359StanzaId,
    is_live_occupant: bool,
    /// `room_members_only` is known message-locally on this T0 path
    /// (the projection event carries it) and is consumed to
    /// pre-populate the policy cache so the synchronous T0 evaluator
    /// never asks the live `RoomRegistryActor`. Each recipient in a
    /// groupchat fan-out would otherwise produce an actor round-trip,
    /// even though the same bit is already in hand.
    room_members_only: bool,
    /// XEP-0513 §"Multi-User Chats Permissions": frozen permission
    /// snapshot for `urn:xmpp:mentions:0#channel`. `false` downgrades
    /// channel mentions to `NotifyAll` (still delivered, but not pushed
    /// as a forced channel mention). The recovery path passes the
    /// bool persisted on the recovery row at original T0 dispatch —
    /// see [`reconcile_groupchat_notification_candidates`].
    sender_can_broadcast_channel_mention: bool,
}

async fn insert_groupchat_notification_candidate(
    seed: GroupchatNotificationCandidateSeed<'_>,
) -> GroupchatNotificationCandidateQueueOutcome {
    let GroupchatNotificationCandidateSeed {
        deps,
        state,
        owner,
        room,
        message,
        sender_jid,
        thread_id,
        archive_stanza_id,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
    } = seed;
    // Parse explicit mentions ONCE per message and derive every
    // XEP-0513 signal (personal-mention bit, channel-mention scope,
    // owner-`<noping/>`) from the same parsed structure. The previous
    // shape ran `extract_explicit_mentions` three times per recipient
    // (class derivation + channel scope + noping), so a 100-member
    // groupchat fan-out paid 300× the parser cost when 1× suffices.
    let owner_occupant_id =
        waddle_xmpp::xep::generate_occupant_id(owner, room, &state.deps.occupant_id_secret);
    let explicit_mentions = waddle_xmpp::xep::extract_explicit_mentions(message);
    let mentions_slice: &[waddle_xmpp::xep::ExplicitMention] = explicit_mentions
        .as_ref()
        .map_or(&[], |mentions| mentions.mentions.as_slice());
    // Parse XEP-0372 references ONCE per recipient so the §304
    // count gate AND the personal-mention fallback consult the same
    // pre-parsed slice. Previously each helper re-walked
    // `message.payloads` independently (2× per recipient × N
    // recipients = 2N walks per message) — perf review on PR #741.
    let references_vec = waddle_xmpp::xep::extract_references_from_message(message);
    let references_slice: &[waddle_xmpp::xep::Reference] = references_vec.as_slice();
    let GroupchatNotificationClassOutcome {
        decision: GroupchatNotificationClassDecision::Deliver(class),
        // The overflow bit is consumed by the classifier — the
        // class downgrade reflects it. We deliberately do NOT use
        // it to gate `<noping/>` (see below).
        mentions_overflowed: _,
    } = groupchat_notification_class(
        mentions_slice,
        references_slice,
        owner,
        room,
        owner_occupant_id.as_str(),
        is_live_occupant,
        sender_can_broadcast_channel_mention,
    );
    // XEP-0513 §"No Ping": "if the sender includes a `<noping/>`
    // child element in a mention, the receiving entity SHOULD NOT
    // generate a notification (ping) for that mention." That SHOULD
    // is INDEPENDENT of §304's "ignore all mentions" cap — the
    // existing slice-2a T1 suppressor (`SuppressedReason::Xep0513Noping`)
    // honors `<noping/>` unconditionally for every class. A prior
    // shape of this code canceled the noping bit on count-overflow
    // (to prevent a spammer silencing push via `<noping/>` + mention
    // spam), but that contradicts both the §"No Ping" SHOULD and
    // the existing T1 behavior — push candidate creation MUST be
    // suppressed for `<noping/>` recipients even when the message
    // overflows the §304 count cap, while normal delivery + MAM +
    // inbox projection are unaffected (compliance review on
    // PR #741). The class downgrade caused by overflow remains; only
    // the per-recipient `<noping/>` suppression survives the cap.
    let recipient_noping =
        groupchat_mentions_carry_owner_noping(mentions_slice, owner, owner_occupant_id.as_str());
    let hints = crate::notification_outbox::NotificationMessageHints::none()
        .with_noping(recipient_noping)
        .with_xep0334(
            waddle_xmpp::xep::xep0334::has_hint(message, waddle_xmpp::xep::xep0334::Hint::NoStore),
            waddle_xmpp::xep::xep0334::has_hint(
                message,
                waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
            ),
        )
        .with_reaction(waddle_xmpp::xep::xep0444::is_reaction_only_message(message));
    let candidate = match crate::notification_outbox::NotificationCandidate::groupchat_with_hints(
        owner.clone(),
        room.clone(),
        sender_jid,
        thread_id,
        archive_stanza_id.clone(),
        class,
        hints,
    ) {
        // Snapshot the body for the optional XEP-0357 §5.4
        // `last-message-body`; dropped when a XEP-0334 storage hint
        // applies.
        Ok(candidate) => candidate.with_last_message_body(super::prototype_body(message)),
        Err(error) => {
            warn!(
                recipient = %owner,
                room = %room,
                error = %error,
                "ProjectGroupchatInbox: XEP-0357 notification candidate rejected"
            );
            return GroupchatNotificationCandidateQueueOutcome::Completed;
        }
    };
    // T0 push-gate evaluation — compliance: suppressed
    // outcomes leave no row in `notification_candidates`. The same
    // typed evaluator runs again at T1 inside
    // `drain_pending_candidates_into_outbox` as a race-window guard.
    //
    // Pre-populate the policy cache with the known `room_members_only`
    // bit so the typed evaluator hits the cache on its first lookup
    // and never reaches the (unused) `RoomPolicyStore`. This avoids
    // N actor round-trips for an N-member fan-out — every recipient's
    // T0 emission already carries the same bit. The `NoopRoomPolicy`
    // is held only to satisfy the trait-object signature; if the
    // cache ever misses (it won't, given the pre-insert) it would
    // surface as `DeferUnknownRoomPolicy` rather than a silent default.
    let room_policy = crate::notification_outbox::NoopRoomPolicy;
    let mut room_policy_cache = std::collections::BTreeMap::<
        BareJid,
        crate::notification_outbox::RoomPolicyCacheEntry,
    >::new();
    room_policy_cache.insert(
        room.clone(),
        if room_members_only {
            crate::notification_outbox::RoomPolicyCacheEntry::Private
        } else {
            crate::notification_outbox::RoomPolicyCacheEntry::Public
        },
    );
    let dnd_reader = crate::notification_outbox::NoopDndReader;
    let mut dnd_cache =
        std::collections::BTreeMap::<BareJid, crate::notification_outbox::DndState>::new();
    // T0 emission deliberately skips the XEP-0513 `<active/>` filter
    // — current activity is a T1 read per the recipient-state
    // contract. `NoopActivityReader` satisfies the typed signature
    // without persisting a row at T0; T1 then runs the real read.
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
                recipient = %owner,
                room = %room,
                error = ?error,
                "ProjectGroupchatInbox: push gate evaluation failed at T0; deferring groupchat candidate"
            );
            return GroupchatNotificationCandidateQueueOutcome::RetryLater;
        }
    };
    match outcome {
        crate::notification_outbox::T1PushDispatchOutcome::Deliver { .. } => {}
        crate::notification_outbox::T1PushDispatchOutcome::Suppressed { reason } => {
            info!(
                recipient = %candidate.recipient_bare_jid(),
                conversation = %candidate.conversation_jid(),
                notification_class = candidate.class().as_db_value(),
                push_stage = "suppressed",
                suppression_reason = reason.as_db_value(),
                "ProjectGroupchatInbox: T0 push gate suppressed groupchat candidate; no candidate row persisted"
            );
            waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                reason.telemetry_reason(),
            );
            return GroupchatNotificationCandidateQueueOutcome::Completed;
        }
        crate::notification_outbox::T1PushDispatchOutcome::DeferUnknownRoomPolicy => {
            warn!(
                recipient = %owner,
                room = %room,
                class = ?class,
                "ProjectGroupchatInbox: MUC config unavailable at T0; deferring groupchat candidate (unknown room policy is not 'public')"
            );
            return GroupchatNotificationCandidateQueueOutcome::RetryLater;
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
            if let Some(deps) = deps {
                deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
                    owner: owner.clone(),
                    mutation:
                        waddle_xmpp::ingress::NotificationActivityMutation::NotificationCandidate {
                            conversation: room.clone(),
                            archive_stanza_id: archive_stanza_id.clone(),
                            outcome: waddle_xmpp::ingress::NotificationCandidateOutcome::Inserted,
                        },
                });
            }
            debug!(
                recipient = %owner,
                room = %room,
                class = ?class,
                "ProjectGroupchatInbox: inserted XEP-0357 groupchat notification candidate"
            );
            GroupchatNotificationCandidateQueueOutcome::Completed
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
            if let Some(deps) = deps {
                deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
                    owner: owner.clone(),
                    mutation:
                        waddle_xmpp::ingress::NotificationActivityMutation::NotificationCandidate {
                            conversation: room.clone(),
                            archive_stanza_id: archive_stanza_id.clone(),
                            outcome: waddle_xmpp::ingress::NotificationCandidateOutcome::Duplicate,
                        },
                });
            }
            debug!(
                recipient = %owner,
                room = %room,
                class = ?class,
                "ProjectGroupchatInbox: duplicate XEP-0357 groupchat notification candidate ignored"
            );
            GroupchatNotificationCandidateQueueOutcome::Completed
        }
        Err(error) => {
            warn!(
                recipient = %owner,
                room = %room,
                error = %error,
                "ProjectGroupchatInbox: XEP-0357 groupchat notification candidate insert failed"
            );
            GroupchatNotificationCandidateQueueOutcome::RetryLater
        }
    }
}

async fn mark_groupchat_notification_recovery_completed(
    deps: &Deps<'_>,
    key: &waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey,
) {
    let Some(inbox_storage) = deps.inbox_storage else {
        return;
    };
    if let Err(error) = inbox_storage
        .mark_groupchat_notification_recovery_completed(key)
        .await
    {
        warn!(
            recipient = %key.recipient,
            room = %key.room,
            stanza_id = %key.archive_stanza_id,
            error = %error,
            "ProjectGroupchatInbox: groupchat notification recovery completion marker failed"
        );
    }
}

#[cfg(test)]
pub(crate) async fn reconcile_groupchat_notification_candidates(
    state: &WebSocketState,
    batch_size: usize,
) -> usize {
    reconcile_groupchat_notification_candidates_for_sweep(state, batch_size)
        .await
        .completed
}

pub(crate) async fn reconcile_groupchat_notification_candidates_for_sweep(
    state: &WebSocketState,
    batch_size: usize,
) -> super::NotificationRecoverySweepOutcome {
    let batch_size = batch_size.clamp(1, 1_000);
    let recoveries = match state
        .deps
        .protocol
        .inbox_storage
        .list_pending_groupchat_notification_recoveries(batch_size)
        .await
    {
        Ok(recoveries) => recoveries,
        Err(error) => {
            warn!(
                error = %error,
                "Groupchat notification candidate recovery could not read inbox recovery rows"
            );
            return super::NotificationRecoverySweepOutcome {
                completed: 0,
                had_failure: true,
            };
        }
    };
    let mut completed = 0usize;
    let mut had_failure = false;
    for recovery in recoveries {
        let archive_room = recovery.key.archive_stanza_id.by.to_bare();
        let archived = match state
            .deps
            .protocol
            .mam_storage
            .get_message_by_archive_or_stanza_id(
                &archive_room,
                recovery.key.archive_stanza_id.as_str(),
            )
            .await
        {
            Ok(Some(archived)) => archived,
            Ok(None) => {
                warn!(
                    recipient = %recovery.key.recipient,
                    room = %recovery.key.room,
                    stanza_id = %recovery.key.archive_stanza_id,
                    "Groupchat notification candidate recovery completed because the committed MAM row is missing"
                );
                match mark_recovery_completed_from_state(state, &recovery.key).await {
                    RecoveryCompletionOutcome::Marked => completed += 1,
                    RecoveryCompletionOutcome::NotMarked => {}
                    RecoveryCompletionOutcome::Failed => had_failure = true,
                }
                continue;
            }
            Err(error) => {
                had_failure = true;
                warn!(
                    recipient = %recovery.key.recipient,
                    room = %recovery.key.room,
                    stanza_id = %recovery.key.archive_stanza_id,
                    error = %error,
                    "Groupchat notification candidate recovery could not load committed MAM row"
                );
                continue;
            }
        };
        let message =
            super::archive_lookup::parse_archived_message_xml(archived.stanza_xml.as_deref())
                .unwrap_or_else(|| super::archive_lookup::fallback_archived_message(&archived));
        let thread_id = recovery
            .key
            .thread_id
            .as_ref()
            .map(|thread_id| {
                crate::notification_outbox::NotificationThreadId::new(thread_id.clone())
            })
            .unwrap_or_else(crate::notification_outbox::NotificationThreadId::root);
        let outcome = insert_groupchat_notification_candidate(GroupchatNotificationCandidateSeed {
            deps: None,
            state,
            owner: &recovery.key.recipient,
            room: &recovery.key.room,
            message: &message,
            sender_jid: recovery.sender_jid.clone(),
            thread_id,
            archive_stanza_id: recovery.key.archive_stanza_id.clone(),
            is_live_occupant: recovery.is_live_occupant,
            room_members_only: recovery.room_members_only,
            // XEP-0513 §"Multi-User Chats Permissions" frozen at the
            // original T0 dispatch — persisted on the recovery row so
            // replay re-creates the same notification class. Defaulting
            // to `false` here would silently downgrade every channel
            // mention to `NotifyAll` and let the public-group `OnMention`
            // XEP-0492 default suppress it at T1: a silent moderator-
            // push outage after every server restart (adversarial review
            // P1 on PR #738).
            sender_can_broadcast_channel_mention: recovery.sender_can_broadcast_channel_mention,
        })
        .await;
        match outcome {
            GroupchatNotificationCandidateQueueOutcome::Completed => {
                match mark_recovery_completed_from_state(state, &recovery.key).await {
                    RecoveryCompletionOutcome::Marked => completed += 1,
                    RecoveryCompletionOutcome::NotMarked => {}
                    RecoveryCompletionOutcome::Failed => had_failure = true,
                }
            }
            GroupchatNotificationCandidateQueueOutcome::RetryLater => had_failure = true,
        }
    }
    super::NotificationRecoverySweepOutcome {
        completed,
        had_failure,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryCompletionOutcome {
    Marked,
    NotMarked,
    Failed,
}

async fn mark_recovery_completed_from_state(
    state: &WebSocketState,
    key: &waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey,
) -> RecoveryCompletionOutcome {
    match state
        .deps
        .protocol
        .inbox_storage
        .mark_groupchat_notification_recovery_completed(key)
        .await
    {
        Ok(marked) if marked > 0 => RecoveryCompletionOutcome::Marked,
        Ok(_) => RecoveryCompletionOutcome::NotMarked,
        Err(error) => {
            warn!(
                recipient = %key.recipient,
                room = %key.room,
                stanza_id = %key.archive_stanza_id,
                error = %error,
                "Groupchat notification recovery completion marker failed"
            );
            RecoveryCompletionOutcome::Failed
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupchatNotificationClassDecision {
    Deliver(crate::notification_outbox::NotificationClass),
}

/// Outcome of `groupchat_notification_class`. Carries the typed
/// class decision plus the XEP-0513 §304 "mention count exceeded"
/// provenance bit. The class decision already reflects the overflow
/// (it collapses to `NotifyAll` when the cap is exceeded); the
/// separate `mentions_overflowed` field exposes the provenance so
/// tests can assert it directly and so a future T0 hint that
/// genuinely depends on the "overflowed at classification time"
/// signal can read it without re-running the §304 count helpers.
///
/// The candidate-emission caller deliberately does NOT gate the
/// recipient's `<noping/>` derivation on this bit — XEP-0513
/// §"No Ping" is independent of §304's "ignore all mentions" cap.
/// See the comment near `recipient_noping` in
/// [`enqueue_groupchat_notification_candidate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroupchatNotificationClassOutcome {
    decision: GroupchatNotificationClassDecision,
    /// `true` when the per-message mention count exceeded the
    /// XEP-0513 §304 `mentions#count` threshold and the classifier
    /// collapsed every mention TARGET to `NotifyAll`. Production
    /// emission discards this bit (see struct doc above); the
    /// field is consumed in the count-gate tests
    /// (`xep0513_mention_count_*`) and is available to any future
    /// T0 hint that needs the overflow provenance.
    mentions_overflowed: bool,
}

/// Classify a groupchat candidate from a pre-parsed mention slice.
///
/// Callers MUST pass the result of a single
/// `extract_explicit_mentions(message)` parse AND a single
/// `extract_references_from_message(message)` parse so that neither
/// XEP-0513 mentions nor XEP-0372 references are re-walked per
/// derivation. The previous shape took `&Message` and re-walked the
/// XEP-0372 payloads twice per recipient (once in the §304 count
/// gate and once in `groupchat_mentions_owner`) — for an
/// N-occupant room that was 2N payload sweeps per message when 1
/// suffices (perf review on PR #741).
fn groupchat_notification_class(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    references: &[waddle_xmpp::xep::Reference],
    owner: &BareJid,
    room: &BareJid,
    owner_occupant_id: &str,
    // `is_live_occupant` here is **message-time-frozen presence** — the
    // XMPP room handler computes it at room-dispatch time
    // (`live_recipient_bares.contains(bare)` in
    // `waddle_xmpp::protocol::room::inbox`) and propagates it on the
    // `ProjectGroupchatInbox` event. That makes it a T0 message-frozen
    // input on the same axis as XEP-0513 `<active/>` (sender intent)
    // and XEP-0421 occupant-id (sender provenance), NOT a T1 recipient-
    // state read. Per #506 Q2 the candidate row snapshots message-
    // intrinsic facts and the T1 evaluator reads fresh recipient
    // state; encoding the message-time live-occupant bit into the
    // [`NotificationClass`] taxonomy (`ActiveChannelMention` vs
    // `ChannelMention`) is the snapshot mechanism here. Slice 2 will
    // add a richer `notification_activity` projection so the T1
    // evaluator can additionally consult *current* recipient activity
    // (XEP-0513 §"active mention" §"the receiving server may filter")
    // — that augments this T0 snapshot, it does not relocate it.
    is_live_occupant: bool,
    // XEP-0513 §"Multi-User Chats Permissions" §304: receiving entities
    // SHOULD ignore a channel mention if the sender does not have at
    // least the minimum role required by the room. This is the typed
    // frozen permission snapshot taken at dispatch time in
    // `waddle_xmpp::protocol::room::inbox::sender_may_broadcast_channel_mention`
    // — server default policy is `mentions#channel = moderators`
    // (XEP-0513 example value).
    sender_can_broadcast_channel_mention: bool,
) -> GroupchatNotificationClassOutcome {
    // XEP-0513 §304: "Receiving entities SHOULD ignore all mentions if
    // the message contains more mentions than the threshold specified
    // by `mentions#count`." When the per-message count exceeds the
    // server-internal default, fall through to `NotifyAll` — neither
    // personal-mention nor channel-mention classification applies.
    // The wire payload is preserved (delivery + MAM unchanged per
    // XEP-0513 §526); only the push class is affected. Per-room
    // override of the threshold is deferred to slice 3c.
    //
    // The overflow bit is also propagated to the candidate-emission
    // caller via `GroupchatNotificationClassOutcome` so the noping
    // derivation can reuse it without re-walking the XEP-0372
    // references a second time per recipient (adversarial review on
    // PR #741).
    let mentions_overflowed = waddle_xmpp::xep::mentions_exceed_threshold_from_parts(
        mentions,
        references,
        waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT,
    );
    if mentions_overflowed {
        return GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::NotifyAll,
            ),
            mentions_overflowed,
        };
    }
    let personal_mention = groupchat_mentions_owner(mentions, references, owner, owner_occupant_id);
    let channel_mention = groupchat_channel_mention_scope(mentions, room)
        .filter(|_| sender_can_broadcast_channel_mention);
    GroupchatNotificationClassOutcome {
        decision: groupchat_notification_class_from_message(
            personal_mention,
            channel_mention,
            is_live_occupant,
        ),
        mentions_overflowed,
    }
}

/// Message-derived classification of a groupchat notification candidate.
///
/// After the T0 → T1 push-decision move (#526 slice 1) the class is a
/// pure function of the message payloads + scope: there is no
/// XEP-0492 recipient-state read here. The T1 evaluator at outbox
/// dispatch time consults the projection store and decides
/// publish-or-suppress based on the recorded class + recipient's
/// effective notification level.
///
/// `channel_mention` carries `None` either when no channel mention is
/// present OR when the sender lacks the XEP-0513 §"Multi-User Chats
/// Permissions" minimum role — see [`groupchat_notification_class`]
/// where the role-filter is applied before this function is called.
fn groupchat_notification_class_from_message(
    personal_mention: bool,
    channel_mention: Option<GroupchatChannelMentionScope>,
    is_live_occupant: bool,
) -> GroupchatNotificationClassDecision {
    if personal_mention {
        return GroupchatNotificationClassDecision::Deliver(
            crate::notification_outbox::NotificationClass::PersonalMention,
        );
    }
    match channel_mention {
        Some(GroupchatChannelMentionScope::Active) if is_live_occupant => {
            return GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::ActiveChannelMention,
            );
        }
        Some(GroupchatChannelMentionScope::All) => {
            return GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::ChannelMention,
            );
        }
        _ => {}
    }
    GroupchatNotificationClassDecision::Deliver(
        crate::notification_outbox::NotificationClass::NotifyAll,
    )
}

/// Returns `true` when any XEP-0513 explicit mention naming `owner`
/// (by JID or occupant-id) also carries `<noping/>`. Snapshotted onto
/// the candidate row at T0 so the T1 evaluator can suppress with
/// `SuppressedReason::Xep0513Noping`. Operates on a pre-parsed slice
/// so the XEP-0513 traversal happens once per message.
fn groupchat_mentions_carry_owner_noping(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    owner: &BareJid,
    owner_occupant_id: &str,
) -> bool {
    mentions.iter().any(|mention| {
        // Mirror the mixed-attribute guard in `groupchat_mentions_owner`:
        // ANY `<mention/>` carrying `mentions='…'` is group-scope (the
        // presence of `mentions=` declares group intent), not a
        // personal mention naming `owner`. The `<noping/>` on a
        // group-scope `#channel` mention causes
        // `current_room_channel_mention` to return `false` (its first
        // guard short-circuits on `mention.noping`), which collapses
        // the channel scope to `None` and the message classifies as
        // `NotifyAll` — i.e. the channel push is suppressed via the
        // scope path, NOT projected onto the owner's personal
        // notification. For unsupported groups the same fall-through
        // applies. Generalised from the slice 3a precedent
        // (`!is_channel()` was too narrow — unsupported groups like
        // `#space` slipped through; review on PR #756).
        mention.noping
            && mention.mentions.is_none()
            && (mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == owner)
                || mention
                    .occupant_id
                    .as_deref()
                    .is_some_and(|mentioned| mentioned == owner_occupant_id))
    })
}

fn groupchat_mentions_owner(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    references: &[waddle_xmpp::xep::Reference],
    owner: &BareJid,
    owner_occupant_id: &str,
) -> bool {
    let xep0513 = mentions.iter().any(|mention| {
        // XEP-0513 §"Multi-User Chats Permissions" hardening
        // (adversarial review on PR #738 + extension on PR #756):
        // ANY `<mention/>` carrying `mentions='…'` is group-scope —
        // the presence of `mentions=` declares group intent — and
        // MUST NOT be classified as a personal mention even when it
        // also carries `jid=`/`occupantid=` attributes. The original
        // slice 3a guard `!is_channel()` plugged the `#channel`
        // permission-bypass attack but missed unsupported groups
        // (`#space`, `#server`, etc.): a `<mention occupantid='X'
        // mentions='#space'/>` would slip through as PersonalMention
        // and piggyback on the personal pipeline. Tightening to
        // `mention.mentions.is_none()` covers EVERY group URI by
        // the wire-shape attribute, not by a hardcoded URI value.
        !mention.noping
            && mention.mentions.is_none()
            && (mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == owner)
                || mention
                    .occupant_id
                    .as_deref()
                    .is_some_and(|mentioned| mentioned == owner_occupant_id))
    });
    let xep0372 = references.iter().any(|reference| {
        reference.is_mention()
            && reference
                .bare_jid()
                .is_some_and(|mentioned| &mentioned == owner)
    });
    xep0513 || xep0372
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupchatChannelMentionScope {
    All,
    Active,
}

fn groupchat_channel_mention_scope(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    room: &BareJid,
) -> Option<GroupchatChannelMentionScope> {
    if mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && !mention.active)
    {
        return Some(GroupchatChannelMentionScope::All);
    }
    mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && mention.active)
        .then_some(GroupchatChannelMentionScope::Active)
}

fn current_room_channel_mention(
    mention: &waddle_xmpp::xep::ExplicitMention,
    room: &BareJid,
) -> bool {
    if !mention.is_channel() || mention.noping {
        return false;
    }
    mention
        .uri
        .as_deref()
        .is_none_or(|uri| xmpp_uri_bare_jid(uri).is_some_and(|target| target == room.clone()))
}

fn xmpp_uri_bare_jid(uri: &str) -> Option<BareJid> {
    // RFC 5122 / RFC 3986: query is introduced by `?`, fragment by
    // `#`. `;` separates key/value pairs WITHIN the query — it does
    // not delimit the start of the query. Stripping on `?` and `#`
    // is sufficient for extracting the JID prefix (Copilot review
    // on PR #738).
    let jid_part = uri.strip_prefix("xmpp:")?.split(['?', '#']).next()?.trim();
    if jid_part.is_empty() {
        return None;
    }
    jid_part.parse::<Jid>().ok().map(|jid| jid.to_bare())
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

    /// Test helper: parses the message's explicit mentions once and
    /// returns the slice the production code passes to the helpers.
    fn parsed_mentions(message: &Message) -> Vec<waddle_xmpp::xep::ExplicitMention> {
        waddle_xmpp::xep::extract_explicit_mentions(message)
            .map(|m| m.mentions)
            .unwrap_or_default()
    }

    /// Test helper: mirror of the production pre-parse for XEP-0372
    /// references. Production hot path parses these once per
    /// recipient in `insert_groupchat_notification_candidate`.
    fn extract_references(message: &Message) -> Vec<waddle_xmpp::xep::Reference> {
        waddle_xmpp::xep::extract_references_from_message(message)
    }

    /// Dedup regression: `groupchat_mentions_carry_owner_noping`
    /// MUST derive the same `<noping/>` bit from a pre-parsed slice
    /// that the previous `message`-taking shape derived per-call.
    #[test]
    fn groupchat_owner_noping_single_parse_matches_pre_dedup_behavior() {
        let owner = bare("charlie@example.com");
        let occupant_id = "room-stable-charlie";

        // No mentions → false.
        let no_mention = Message::new(None::<Jid>);
        assert!(!groupchat_mentions_carry_owner_noping(
            &parsed_mentions(&no_mention),
            &owner,
            occupant_id,
        ));

        // Plain mention naming the owner → false (no `<noping/>`).
        let plain = message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(owner.clone()));
        assert!(!groupchat_mentions_carry_owner_noping(
            &parsed_mentions(&plain),
            &owner,
            occupant_id,
        ));

        // `<noping/>` mention by JID → true.
        let mut noping_by_jid = waddle_xmpp::xep::ExplicitMention::jid(owner.clone());
        noping_by_jid.noping = true;
        let msg_jid = message_with_mention(noping_by_jid);
        assert!(groupchat_mentions_carry_owner_noping(
            &parsed_mentions(&msg_jid),
            &owner,
            occupant_id,
        ));

        // `<noping/>` mention by occupant-id → true.
        let mut noping_by_occ = waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id);
        noping_by_occ.noping = true;
        let msg_occ = message_with_mention(noping_by_occ);
        assert!(groupchat_mentions_carry_owner_noping(
            &parsed_mentions(&msg_occ),
            &owner,
            occupant_id,
        ));

        // `<noping/>` mention naming someone else → false.
        let other = bare("dave@example.com");
        let mut noping_other = waddle_xmpp::xep::ExplicitMention::jid(other);
        noping_other.noping = true;
        let msg_other = message_with_mention(noping_other);
        assert!(!groupchat_mentions_carry_owner_noping(
            &parsed_mentions(&msg_other),
            &owner,
            occupant_id,
        ));
    }

    #[test]
    fn groupchat_message_classification_matrix() {
        use crate::notification_outbox::NotificationClass;

        // Post-#526 slice 1: the T0 classifier is purely message-derived
        // (personal mention, channel mention scope, live-occupant gate).
        // XEP-0492 enforcement lives at T1, exercised separately in the
        // notification_outbox dispatcher tests.
        let cases = [
            (
                false,
                None,
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::NotifyAll),
            ),
            (
                true,
                None,
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::PersonalMention),
            ),
            (
                false,
                Some(GroupchatChannelMentionScope::All),
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::ChannelMention),
            ),
            (
                false,
                Some(GroupchatChannelMentionScope::Active),
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::NotifyAll),
            ),
            (
                false,
                Some(GroupchatChannelMentionScope::Active),
                true,
                GroupchatNotificationClassDecision::Deliver(
                    NotificationClass::ActiveChannelMention,
                ),
            ),
            (
                true,
                Some(GroupchatChannelMentionScope::All),
                false,
                // Personal mention wins over channel-wide mention.
                GroupchatNotificationClassDecision::Deliver(NotificationClass::PersonalMention),
            ),
        ];

        for (personal_mention, channel_mention, is_live_occupant, expected) in cases {
            assert_eq!(
                groupchat_notification_class_from_message(
                    personal_mention,
                    channel_mention,
                    is_live_occupant,
                ),
                expected,
                "unexpected groupchat T0 class for personal_mention={personal_mention}, channel_mention={channel_mention:?}, is_live_occupant={is_live_occupant}"
            );
        }
    }

    #[test]
    fn xep0513_groupchat_personal_mentions_match_jid_or_occupant_id_and_respect_noping() {
        let owner = bare("charlie@example.com");
        let occupant_id = "room-stable-charlie";

        let msg_by_jid =
            message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(owner.clone()));
        assert!(groupchat_mentions_owner(
            &parsed_mentions(&msg_by_jid),
            &extract_references(&msg_by_jid),
            &owner,
            occupant_id,
        ));
        let msg_by_occ =
            message_with_mention(waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id));
        assert!(groupchat_mentions_owner(
            &parsed_mentions(&msg_by_occ),
            &extract_references(&msg_by_occ),
            &owner,
            occupant_id,
        ));

        let mut noping = waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id);
        noping.noping = true;
        let msg_noping = message_with_mention(noping);
        assert!(!groupchat_mentions_owner(
            &parsed_mentions(&msg_noping),
            &extract_references(&msg_noping),
            &owner,
            occupant_id,
        ));

        let msg_other = message_with_mention(waddle_xmpp::xep::ExplicitMention::occupant_id(
            "somebody-else",
        ));
        assert!(!groupchat_mentions_owner(
            &parsed_mentions(&msg_other),
            &extract_references(&msg_other),
            &owner,
            occupant_id,
        ));
    }

    #[test]
    fn xep0513_groupchat_channel_mentions_are_room_scoped_and_active_aware() {
        let room = bare("team@muc.example.com");

        let msg_channel = message_with_mention(waddle_xmpp::xep::ExplicitMention::channel());
        assert_eq!(
            groupchat_channel_mention_scope(&parsed_mentions(&msg_channel), &room),
            Some(GroupchatChannelMentionScope::All)
        );

        let mut active = waddle_xmpp::xep::ExplicitMention::active_channel();
        active.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg_active = message_with_mention(active);
        assert_eq!(
            groupchat_channel_mention_scope(&parsed_mentions(&msg_active), &room),
            Some(GroupchatChannelMentionScope::Active)
        );

        let mut foreign = waddle_xmpp::xep::ExplicitMention::channel();
        foreign.uri = Some("xmpp:other@muc.example.com".to_string());
        let msg_foreign = message_with_mention(foreign);
        assert_eq!(
            groupchat_channel_mention_scope(&parsed_mentions(&msg_foreign), &room),
            None
        );

        let mut noping = waddle_xmpp::xep::ExplicitMention::channel();
        noping.noping = true;
        let msg_noping = message_with_mention(noping);
        assert_eq!(
            groupchat_channel_mention_scope(&parsed_mentions(&msg_noping), &room),
            None
        );
    }

    /// XEP-0513 §"Multi-User Chats Permissions" §304: receiving entities
    /// SHOULD ignore a channel mention if the sender does not have at
    /// least the minimum role required by the room. The classifier
    /// MUST downgrade a channel mention to `NotifyAll` when the frozen
    /// `sender_can_broadcast_channel_mention` snapshot is `false`. The
    /// mention itself is delivered + archived unchanged; only the push
    /// class changes — that is the entire scope of the permission gate.
    #[test]
    fn xep0513_channel_mention_downgrades_to_notify_all_for_unpermitted_sender() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        let mut channel_uri = waddle_xmpp::xep::ExplicitMention::channel();
        channel_uri.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg = message_with_mention(channel_uri);
        let mentions = parsed_mentions(&msg);

        // Permitted sender (moderator) → ChannelMention.
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(permitted),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            permitted,
            NotificationClass::ChannelMention,
            "moderator's channel mention must boost the push class to ChannelMention"
        );

        // Non-permitted sender (participant) → NotifyAll (downgrade).
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(downgraded),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ false,
        );
        assert_eq!(
            downgraded,
            NotificationClass::NotifyAll,
            "non-permitted sender's channel mention must downgrade to NotifyAll, \
             not stay as ChannelMention — XEP-0513 §304"
        );
    }

    /// Same XEP-0513 §"Multi-User Chats Permissions" gate, applied to
    /// the `<active/>` variant: an active channel mention from a non-
    /// permitted sender MUST also downgrade. The `<active/>` qualifier
    /// is a recipient-state filter (XEP-0513 §"active mention"); it
    /// does NOT confer permission to broadcast.
    #[test]
    fn xep0513_active_channel_mention_downgrades_for_unpermitted_sender() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        let mut active_channel = waddle_xmpp::xep::ExplicitMention::active_channel();
        active_channel.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg = message_with_mention(active_channel);
        let mentions = parsed_mentions(&msg);

        // Permitted sender + live occupant → ActiveChannelMention.
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(permitted),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ true,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            permitted,
            NotificationClass::ActiveChannelMention,
            "moderator's <active/> channel mention to a live occupant \
             must classify as ActiveChannelMention"
        );

        // Non-permitted sender + live occupant → NotifyAll.
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(downgraded),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ true,
            /* sender_can_broadcast_channel_mention */ false,
        );
        assert_eq!(
            downgraded,
            NotificationClass::NotifyAll,
            "non-permitted sender's <active/> channel mention must downgrade \
             to NotifyAll — the active qualifier is a recipient filter, not \
             a permission grant"
        );
    }

    /// XEP-0513 §526 "Security Considerations" allows the server to
    /// filter mentions per its own rules; this PR downgrades the push
    /// CLASS, but the on-wire `<mention/>` payload MUST be delivered +
    /// archived UNCHANGED. The class downgrade is a server-internal
    /// push-decision detail; clients consuming MAM / live delivery /
    /// inbox MUST still see the original `urn:xmpp:mentions:0#channel`
    /// element so their own UI rendering can show "Alice tried to
    /// channel-mention you" even though no push fired.
    #[test]
    fn xep0513_channel_mention_payload_is_preserved_when_class_is_downgraded() {
        use waddle_xmpp::xep::{extract_explicit_mentions, has_explicit_mentions, CHANNEL_MENTION};

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        let mut channel_uri = waddle_xmpp::xep::ExplicitMention::channel();
        channel_uri.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg = message_with_mention(channel_uri);

        // Sanity: the message starts with the channel mention payload.
        assert!(has_explicit_mentions(&msg));
        let original_mentions = extract_explicit_mentions(&msg)
            .expect("starts with mentions")
            .mentions;
        assert!(original_mentions
            .iter()
            .any(|mention| mention.mentions.as_deref() == Some(CHANNEL_MENTION)));

        // Run the classifier with a non-permitted sender (the gate
        // downgrades the class). The classifier returns the class
        // only — it MUST NOT mutate the message payloads.
        let mentions = parsed_mentions(&msg);
        let _classifier_outcome = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ false,
        );

        // Wire shape MUST be unchanged after classification.
        assert!(
            has_explicit_mentions(&msg),
            "classifier MUST NOT strip the channel-mention payload on downgrade"
        );
        let after_mentions = extract_explicit_mentions(&msg)
            .expect("mentions still present")
            .mentions;
        assert_eq!(
            after_mentions, original_mentions,
            "the `<mention mentions='urn:xmpp:mentions:0#channel'/>` payload \
             MUST be delivered + archived unchanged when the push class is \
             downgraded — XEP-0513 §526 allows filtering the push decision, \
             not rewriting the stanza"
        );
    }

    /// Personal mentions are unaffected by the channel-broadcast
    /// permission. XEP-0513 §"Multi-User Chats Permissions" carries a
    /// separate `mentions#individual` field for individual-mention
    /// permission — outside the scope of this slice. A personal mention
    /// from a non-permitted-broadcast sender MUST still classify as
    /// PersonalMention; only `urn:xmpp:mentions:0#channel` is gated.
    #[test]
    fn xep0513_personal_mention_unaffected_by_channel_broadcast_permission() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        let msg = message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(owner.clone()));
        let mentions = parsed_mentions(&msg);

        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ false,
        );
        assert_eq!(
            class,
            NotificationClass::PersonalMention,
            "personal mention class must NOT be affected by the channel \
             broadcast gate — the gate covers `urn:xmpp:mentions:0#channel` only"
        );
    }

    /// Adversarial review on PR #738: a non-permitted sender crafted
    /// `<mention occupantid='target-occ' mentions='urn:xmpp:mentions:0#channel'/>`
    /// would historically bypass the channel-broadcast gate by being
    /// classified as `PersonalMention` (because the personal matcher
    /// found the occupant-id match and didn't check the channel-scope
    /// attribute). The fix in `groupchat_mentions_owner` filters out
    /// mixed-attribute mentions so they ONLY flow through the channel
    /// pipeline (and are subject to the permission gate). Lock the
    /// behavior here.
    #[test]
    fn xep0513_mixed_attribute_mention_does_not_bypass_channel_gate() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        // Craft a `<mention/>` that simultaneously carries
        // `occupantid='charlie-occ'` AND `mentions='#channel'`. The
        // attacker's intent: have charlie classify it as personal
        // (bypassing the gate) while the room sees it as channel.
        let mut mixed = waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id);
        mixed.mentions = Some(waddle_xmpp::xep::CHANNEL_MENTION.to_string());
        let msg = message_with_mention(mixed);
        let mentions = parsed_mentions(&msg);

        // Non-permitted sender (participant). With the fix, the
        // channel-scope wins and the gate downgrades to NotifyAll.
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ false,
        );
        assert_eq!(
            class,
            NotificationClass::NotifyAll,
            "a `<mention occupantid='X' mentions='#channel'/>` from a non-\
             permitted sender MUST NOT classify as PersonalMention — the \
             channel-scope attribute overrides the personal targeting and \
             the gate applies"
        );

        // Permitted sender (moderator). Channel-scope still wins, and
        // the gate now classifies as ChannelMention — NOT
        // PersonalMention.
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class,
            NotificationClass::ChannelMention,
            "a `<mention occupantid='X' mentions='#channel'/>` from a \
             permitted sender MUST classify as ChannelMention — the \
             channel-scope attribute is authoritative regardless of any \
             per-recipient targeting attributes"
        );
    }

    /// XEP-0513 §304 SHOULD: "Receiving entities SHOULD ignore all
    /// mentions if the message contains more mentions than the
    /// threshold specified by `mentions#count`." This locks the
    /// server-internal default (`DEFAULT_MENTIONS_COUNT = 5`).
    /// Exceeding the threshold MUST collapse classification to
    /// `NotifyAll` — not just discard the excess, but ignore EVERY
    /// mention on the message including the ones below the threshold
    /// boundary (slice 3b of #525).
    #[test]
    fn xep0513_mention_count_exceeded_ignores_all_mentions() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        // Build a message with EXACTLY threshold+1 personal mentions,
        // one of which targets `owner`. Without the count gate, owner
        // would classify as `PersonalMention`.
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;
        let mut msg = Message::new(None::<Jid>);
        for i in 0..=threshold {
            let target = if i == 0 {
                owner.clone()
            } else {
                format!("user{i}@example.com").parse().expect("target jid")
            };
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(target),
            ));
        }
        let mentions = parsed_mentions(&msg);
        assert!(
            mentions.len() as u32 > threshold,
            "fixture must produce more than threshold mentions"
        );

        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class,
            NotificationClass::NotifyAll,
            "a message with > DEFAULT_MENTIONS_COUNT mention targets MUST \
             classify as NotifyAll — every mention is ignored per \
             XEP-0513 §304, including the one naming the owner"
        );
        assert!(
            mentions_overflowed,
            "the overflow bit MUST be propagated to the caller so the \
             noping derivation can short-circuit without re-walking the \
             XEP-0372 references"
        );
    }

    /// At-threshold (exactly `DEFAULT_MENTIONS_COUNT` targets) MUST
    /// still be honored — §304 says "more than the threshold", so
    /// the boundary value is inclusive.
    #[test]
    fn xep0513_mention_count_at_threshold_is_honored() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;
        let mut msg = Message::new(None::<Jid>);
        for i in 0..threshold {
            let target = if i == 0 {
                owner.clone()
            } else {
                format!("user{i}@example.com").parse().expect("target jid")
            };
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(target),
            ));
        }
        let mentions = parsed_mentions(&msg);
        assert_eq!(mentions.len() as u32, threshold);

        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class,
            NotificationClass::PersonalMention,
            "exactly DEFAULT_MENTIONS_COUNT mention targets MUST be \
             honored — §304 threshold is exclusive (\"more than\")"
        );
        assert!(
            !mentions_overflowed,
            "at-threshold count MUST NOT set the overflow bit — without \
             this lock-in, a regression flipping `>` to `>=` in \
             `mentions_exceed_threshold` would silently un-suppress the \
             noping bit at the boundary"
        );
    }

    /// XEP-0372 references are also counted toward the §304
    /// threshold. Without this, an attacker bypasses the gate by
    /// using `<reference type='mention' uri='xmpp:X'/>` instead of
    /// XEP-0513 `<mention/>` — same mention semantics, different
    /// wire shape.
    #[test]
    fn xep0513_mention_count_includes_xep0372_references() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        // Half XEP-0513, half XEP-0372 — together exceed threshold.
        let half = threshold / 2 + 1;
        let mut msg = Message::new(None::<Jid>);
        msg.payloads.push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(owner.clone()),
        ));
        for i in 1..half {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("target"),
                ),
            ));
        }
        for i in 0..(threshold - half + 2) {
            msg.payloads.push(waddle_xmpp::xep::build_reference_element(
                &waddle_xmpp::xep::Reference::mention(format!("xmpp:ref{i}@example.com")),
            ));
        }
        let mentions = parsed_mentions(&msg);

        let total = waddle_xmpp::xep::mention_target_count(&mentions, &msg);
        assert!(
            total > threshold,
            "fixture must produce more than {threshold} combined mention targets (got {total})"
        );

        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed: _,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class,
            NotificationClass::NotifyAll,
            "XEP-0372 reference count + XEP-0513 mention count combined \
             MUST trigger the §304 threshold — using references to \
             bypass the cap is exactly the abuse vector this gate \
             closes"
        );
    }

    /// XEP-0513 §"No Ping" is INDEPENDENT of §304's "ignore all
    /// mentions" cap (compliance review on PR #741). When the
    /// threshold is exceeded:
    ///
    ///   - the CLASS is downgraded to `NotifyAll` per §304;
    ///   - per-recipient `<noping/>` is PRESERVED — the existing
    ///     T0/T1 `Xep0513Noping` suppressor fires for the recipient
    ///     even on overflowed messages, suppressing push candidate
    ///     creation without affecting delivery / MAM / inbox.
    ///
    /// This lock-in test asserts the predicate that
    /// `insert_groupchat_notification_candidate` reads:
    /// `groupchat_mentions_carry_owner_noping` MUST return true
    /// regardless of overflow, because the production code no
    /// longer combines it with `!mentions_overflowed`.
    #[test]
    fn xep0513_noping_survives_mention_count_overflow() {
        let owner = bare("charlie@example.com");
        let occupant_id = "room-stable-charlie";
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        let mut msg = Message::new(None::<Jid>);
        let mut owner_noping = waddle_xmpp::xep::ExplicitMention::jid(owner.clone());
        owner_noping.noping = true;
        msg.payloads
            .push(waddle_xmpp::xep::build_mention_element(&owner_noping));
        for i in 0..threshold {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("target"),
                ),
            ));
        }
        let mentions = parsed_mentions(&msg);

        // Sanity: the threshold IS exceeded.
        assert!(
            waddle_xmpp::xep::mentions_exceed_threshold(&mentions, &msg, threshold),
            "fixture must trip the threshold predicate"
        );
        // The per-recipient noping helper reports true regardless of
        // overflow. Production code at
        // `enqueue_groupchat_notification_candidate` uses this
        // predicate unconditionally — the overflow bit no longer
        // gates the noping suppression. Compliance with XEP-0513
        // §"No Ping" PR Compliance ID 7.
        assert!(
            groupchat_mentions_carry_owner_noping(&mentions, &owner, occupant_id),
            "owner's `<noping/>` mention MUST be honored even when the \
             message overflows the §304 count cap — the §\"No Ping\" \
             SHOULD operates independently of §304's class downgrade"
        );
    }

    /// Groupchat mirror of the DM `at_threshold_preserves_noping`
    /// test: at exactly `DEFAULT_MENTIONS_COUNT` total mentions, one
    /// of them with `<noping/>` naming the owner, the gate MUST NOT
    /// fire — `mentions_overflowed` stays `false` and the noping
    /// suppressor at the emission site fires normally.
    #[test]
    fn xep0513_groupchat_mention_count_at_threshold_preserves_owner_noping() {
        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        let mut msg = Message::new(None::<Jid>);
        let mut owner_noping = waddle_xmpp::xep::ExplicitMention::jid(owner.clone());
        owner_noping.noping = true;
        msg.payloads
            .push(waddle_xmpp::xep::build_mention_element(&owner_noping));
        for i in 0..(threshold - 1) {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("target"),
                ),
            ));
        }
        let mentions = parsed_mentions(&msg);
        assert_eq!(mentions.len() as u32, threshold);

        let GroupchatNotificationClassOutcome {
            mentions_overflowed,
            ..
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert!(
            !mentions_overflowed,
            "at-threshold MUST NOT overflow — boundary regression target"
        );
        assert!(
            groupchat_mentions_carry_owner_noping(&mentions, &owner, occupant_id),
            "owner's `<noping/>` mention is present and counted"
        );
        // Production emission reads `groupchat_mentions_carry_owner_noping(...)`
        // unconditionally (the overflow bit no longer gates the
        // noping derivation per the XEP-0513 §"No Ping" SHOULD —
        // see commit d17004c3 / `xep0513_noping_survives_mention_count_overflow`).
        // At-threshold + `<noping/>` therefore produces
        // `recipient_noping = true` and the T0/T1 `Xep0513Noping`
        // suppressor fires for this recipient.
    }

    /// Composition test: a non-permitted sender combines a channel
    /// mention with enough per-user mentions to trip the §304 count
    /// gate. Both gates converge on `NotifyAll`, but the count gate
    /// runs FIRST so `mentions_overflowed` MUST be `true`. A future
    /// refactor that reorders the gates (e.g. moves the channel
    /// permission check above the count check) would change the
    /// observable `mentions_overflowed` field — locked in here.
    #[test]
    fn xep0513_channel_mention_with_count_exceeded_composes_to_notify_all_with_overflow_bit() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        let mut msg = Message::new(None::<Jid>);
        // One channel mention + threshold per-user mentions = threshold+1.
        let mut channel = waddle_xmpp::xep::ExplicitMention::channel();
        channel.uri = Some("xmpp:team@muc.example.com".to_string());
        msg.payloads
            .push(waddle_xmpp::xep::build_mention_element(&channel));
        for i in 0..threshold {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(
                    format!("user{i}@example.com").parse().expect("target"),
                ),
            ));
        }
        let mentions = parsed_mentions(&msg);

        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ false,
        );
        assert_eq!(
            class,
            NotificationClass::NotifyAll,
            "non-permitted sender's channel mention + over-threshold \
             personal mentions MUST collapse to NotifyAll"
        );
        assert!(
            mentions_overflowed,
            "the count gate runs first; the overflow bit MUST be set \
             regardless of the channel-broadcast gate's outcome"
        );
    }

    /// XEP-0203 §"Use in delayed delivery" composition: a delayed
    /// (`<delay xmlns='urn:xmpp:delay'/>`) message with count
    /// exceeded still flows through the §304 gate. The delay element
    /// is not honored on the candidate-emission path (spoofable per
    /// the slice 3a reverted filter), so the count gate is what
    /// downgrades a spammy historical replay to NotifyAll. Locks the
    /// §304 × XEP-0203 composition.
    #[test]
    fn xep0513_delayed_message_with_count_exceeded_still_downgrades_to_notify_all() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        let mut msg = Message::new(None::<Jid>);
        for i in 0..=threshold {
            msg.payloads.push(waddle_xmpp::xep::build_mention_element(
                &waddle_xmpp::xep::ExplicitMention::jid(if i == 0 {
                    owner.clone()
                } else {
                    format!("user{i}@example.com").parse().expect("target")
                }),
            ));
        }
        // Attach a `<delay/>` from a server-shaped JID. The candidate
        // path doesn't honor `<delay/>` (per slice 3a's revert), so
        // the count gate is what matters here.
        msg.payloads
            .push(waddle_xmpp::xep::xep0203::build_delay_element_simple(
                chrono::TimeZone::timestamp_opt(&chrono::Utc, 1_700_000_000, 0)
                    .single()
                    .expect("delay stamp"),
                "muc.example.com",
            ));
        let mentions = parsed_mentions(&msg);

        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class),
            mentions_overflowed,
        } = groupchat_notification_class(
            &mentions,
            &extract_references(&msg),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class,
            NotificationClass::NotifyAll,
            "delayed message with count exceeded MUST still downgrade \
             — §304 fires regardless of XEP-0203 presence"
        );
        assert!(
            mentions_overflowed,
            "the count gate fires for delayed messages identically to \
             live ones"
        );
    }

    /// XEP-0513 §"Active" + #525 explicit XEP test: `<active/>` on
    /// a channel mention is a recipient-state filter that narrows
    /// the candidate population to currently-active occupants. The
    /// classifier MUST distinguish the two channel shapes at T0 so
    /// the T1 active-filter has a class to match on:
    ///
    /// - `<mention mentions='urn:xmpp:mentions:0#channel'/>` (no
    ///   qualifier) → `NotificationClass::ChannelMention`
    ///   (push to all members)
    /// - `<mention mentions='urn:xmpp:mentions:0#channel'><active/></mention>`
    ///   → `NotificationClass::ActiveChannelMention` (T1 filters by
    ///   `last_active_at_ms <= active_mention_ttl_ms`)
    ///
    /// Existing tests cover the permission-downgrade direction; this
    /// one pins the active-vs-non-active boundary itself with the
    /// permission gate held constant (slice 3d of #525).
    #[test]
    fn xep0513_active_qualifier_distinguishes_channel_mention_classification() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        // Bare channel mention (no `<active/>`).
        let mut plain_channel = waddle_xmpp::xep::ExplicitMention::channel();
        plain_channel.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg_plain = message_with_mention(plain_channel);
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class_plain),
            mentions_overflowed: overflowed_plain,
        } = groupchat_notification_class(
            &parsed_mentions(&msg_plain),
            &extract_references(&msg_plain),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ true,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class_plain,
            NotificationClass::ChannelMention,
            "a channel mention WITHOUT `<active/>` must classify as \
             ChannelMention so every member receives a push candidate"
        );
        // Single-mention message MUST NOT trip the §304 overflow bit;
        // catches a count-miscalculation regression that wrongly
        // counts `<active/>`-tagged or unsupported-group mentions
        // (adversarial review on PR #756).
        assert!(
            !overflowed_plain,
            "single channel mention MUST NOT trip the §304 overflow bit"
        );

        // Same shape, but with `<active/>` added — only the
        // recipient-state filter changes; the permission gate is
        // unchanged.
        let mut active_channel = waddle_xmpp::xep::ExplicitMention::active_channel();
        active_channel.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg_active = message_with_mention(active_channel);
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class_active),
            mentions_overflowed: overflowed_active,
        } = groupchat_notification_class(
            &parsed_mentions(&msg_active),
            &extract_references(&msg_active),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ true,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class_active,
            NotificationClass::ActiveChannelMention,
            "the same channel mention WITH `<active/>` must classify \
             as ActiveChannelMention so the T1 active-filter narrows \
             the candidate population to currently-active occupants"
        );
        assert!(
            !overflowed_active,
            "single `<active/>` channel mention MUST NOT trip the §304 \
             overflow bit"
        );

        // T0 collapses `Active` → `NotifyAll` when the recipient is
        // NOT a live occupant. The `Active` arm of
        // `groupchat_notification_class_from_message` requires
        // `is_live_occupant=true`, otherwise it falls through to
        // `NotifyAll`. Pin the collapse so a future refactor that
        // lifts the live-occupant check out of T0 doesn't silently
        // start emitting `ActiveChannelMention` rows for offline
        // recipients (which would then leak the
        // "ActiveChannelMention exists for X" signal to the T1
        // observability surface even for offline recipients).
        let mut active_for_offline = waddle_xmpp::xep::ExplicitMention::active_channel();
        active_for_offline.uri = Some("xmpp:team@muc.example.com".to_string());
        let msg_active_offline = message_with_mention(active_for_offline);
        let GroupchatNotificationClassOutcome {
            decision: GroupchatNotificationClassDecision::Deliver(class_offline),
            mentions_overflowed: overflowed_offline,
        } = groupchat_notification_class(
            &parsed_mentions(&msg_active_offline),
            &extract_references(&msg_active_offline),
            &owner,
            &room,
            occupant_id,
            /* is_live_occupant */ false,
            /* sender_can_broadcast_channel_mention */ true,
        );
        assert_eq!(
            class_offline,
            NotificationClass::NotifyAll,
            "offline recipient + `<active/>` channel mention MUST \
             collapse to NotifyAll at T0 — the `Active` scope only \
             produces `ActiveChannelMention` for live occupants \
             (see the `Active` arm of \
             `groupchat_notification_class_from_message`)"
        );
        assert!(
            !overflowed_offline,
            "overflow bit MUST be independent of `is_live_occupant`"
        );
    }

    /// XEP-0513 + #525 explicit XEP test: a `<mention/>` carrying an
    /// **unadvertised** group URI (`urn:xmpp:mentions:0#space`,
    /// `urn:xmpp:mentions:0#server`, `urn:xmpp:mentions:0#associations`,
    /// `urn:xmpp:mentions:0#hats`) MUST NOT elevate the notification
    /// class. Waddle deliberately advertises only the individual +
    /// `urn:xmpp:mentions:0#channel` subset (#525 scope: "Do not
    /// advertise `#space`, `#server`, `#associations`, or `#hats`
    /// until recipient resolution and permissions are implemented
    /// for them"), and the classifier MUST stay forward-compatible:
    /// a future code change that loosens `is_channel()` to accept
    /// any `urn:xmpp:mentions:0#*` value would silently elevate
    /// unsupported groups to push, which is exactly what this
    /// regression guards against.
    ///
    /// The mention itself is still delivered + archived unchanged
    /// (covered by [`xep0513_channel_mention_payload_is_preserved_when_class_is_downgraded`]);
    /// this test pins ONLY the push-class boundary.
    #[test]
    fn xep0513_unsupported_group_uris_do_not_elevate_notification_class() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        // Every unadvertised group URI from XEP-0513 §"Multi-User
        // Chats Permissions" — the four §303 form fields Waddle
        // chooses not to expose yet. Also includes two
        // forward-compatibility probe URIs (`#moderators` and
        // `#owner`) that are NOT spec-defined group identifiers;
        // they are crafted strings that share the
        // `urn:xmpp:mentions:0#…` prefix to trip a future code
        // change that relaxes `is_channel()` from strict
        // equality to a prefix match. (Note: `moderators` /
        // `owner` appear in XEP-0513 only as `mentions#channel`
        // *form values*, not as group URIs; their use here is
        // purely as wire-shape probes, not as references to any
        // spec-defined `#moderators`/`#owner` group.)
        let unsupported_uris = [
            "urn:xmpp:mentions:0#space",
            "urn:xmpp:mentions:0#server",
            "urn:xmpp:mentions:0#associations",
            "urn:xmpp:mentions:0#hats",
            "urn:xmpp:mentions:0#moderators",
            "urn:xmpp:mentions:0#owner",
        ];

        for group_uri in unsupported_uris {
            // Minimal `<mention/>` fixture: only the `mentions=`
            // attribute. The XEP requires `uri=` for several of these
            // sub-types (`#server` MUST carry the server JID per
            // xep-0513.xml:132; `#hats` MUST carry the hat URI per
            // :188), but Waddle's classifier rejects unsupported
            // groups via the strict-equality check in `is_channel()`
            // (the `mentions=` value must equal exactly
            // `urn:xmpp:mentions:0#channel`) BEFORE inspecting any
            // other attribute. The test is intentionally shape-
            // minimal to prove the rejection survives malformed
            // inputs (defence-in-depth review on PR #756). Wire-shape
            // correctness for properly-formed mentions is pinned by
            // the dedicated XEP custom tests in
            // `crates/waddle-xmpp/tests/xep0513_mentions.rs`.
            let msg = message_with_mention(waddle_xmpp::xep::ExplicitMention {
                mentions: Some(group_uri.to_string()),
                ..waddle_xmpp::xep::ExplicitMention::default()
            });
            let GroupchatNotificationClassOutcome {
                decision: GroupchatNotificationClassDecision::Deliver(class),
                mentions_overflowed,
            } = groupchat_notification_class(
                &parsed_mentions(&msg),
                &extract_references(&msg),
                &owner,
                &room,
                occupant_id,
                /* is_live_occupant */ true,
                // Permitted sender — even an authorised broadcaster
                // must NOT elevate an unadvertised group; the gate
                // covers `#channel` only.
                /* sender_can_broadcast_channel_mention */
                true,
            );
            assert_eq!(
                class,
                NotificationClass::NotifyAll,
                "`<mention mentions='{group_uri}'/>` from a permitted \
                 sender MUST NOT elevate the push class — Waddle does \
                 not advertise `{group_uri}` and unsupported groups \
                 fall through to NotifyAll"
            );
            // §304 overflow: a single unsupported-group mention IS
            // one mention TARGET (per `is_mention_target` at
            // xep0513.rs:54-64), but a single target is well below
            // the default threshold of 5. A future
            // count-miscalculation regression that wrongly elevates
            // unsupported groups' weight would trip here.
            assert!(
                !mentions_overflowed,
                "single `<mention mentions='{group_uri}'/>` MUST NOT \
                 trip the §304 overflow bit"
            );
        }
    }

    /// Mirror of slice 3a's
    /// `xep0513_mixed_attribute_mention_does_not_bypass_channel_gate`
    /// (line 1384) on the **unsupported-group axis**. Slice 3a closed
    /// the attack where `<mention occupantid='X' mentions='#channel'/>`
    /// would bypass the channel-permission gate by being classified as
    /// `PersonalMention`. The same shape with an unsupported group
    /// URI (e.g. `#space`) MUST also be group-scope, not personal:
    /// a `<mention mentions='X'/>` with ANY group URI declares
    /// group-scope intent, and the `occupantid=`/`jid=` attributes
    /// are decoration. Treating them as personal lets clients dodge
    /// Waddle's "unsupported groups don't elevate" policy by
    /// piggybacking on the personal-mention pipeline (cross-XEP +
    /// §303-alignment review on PR #756).
    #[test]
    fn xep0513_mixed_attribute_unsupported_group_does_not_elevate_to_personal() {
        use crate::notification_outbox::NotificationClass;

        let owner = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        // Every unadvertised group URI from the previous test —
        // crafted with a matching `occupantid=` to attempt the
        // personal-mention bypass.
        let unsupported_uris = [
            "urn:xmpp:mentions:0#space",
            "urn:xmpp:mentions:0#server",
            "urn:xmpp:mentions:0#associations",
            "urn:xmpp:mentions:0#hats",
            "urn:xmpp:mentions:0#moderators",
            "urn:xmpp:mentions:0#owner",
        ];

        for group_uri in unsupported_uris {
            let mut mixed = waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id);
            mixed.mentions = Some(group_uri.to_string());
            let msg = message_with_mention(mixed);
            let GroupchatNotificationClassOutcome {
                decision: GroupchatNotificationClassDecision::Deliver(class),
                mentions_overflowed: _,
            } = groupchat_notification_class(
                &parsed_mentions(&msg),
                &extract_references(&msg),
                &owner,
                &room,
                occupant_id,
                /* is_live_occupant */ true,
                /* sender_can_broadcast_channel_mention */ true,
            );
            assert_eq!(
                class,
                NotificationClass::NotifyAll,
                "`<mention occupantid='{occupant_id}' \
                 mentions='{group_uri}'/>` MUST NOT classify as \
                 PersonalMention — the presence of `mentions=` \
                 declares group-scope (per slice 3a precedent for \
                 `#channel`); unsupported groups fall through to \
                 NotifyAll instead of piggybacking on the personal \
                 pipeline"
            );
        }
    }
}
