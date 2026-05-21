use super::groupchat_archive::{extract_room_stanza_id, GroupchatInboxProjectionOutcome};
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
    let notification_recovery = groupchat_notification_recovery_item(
        &owner,
        &room,
        &message,
        is_recipient,
        is_durable_recipient,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        &thread,
        dispatch_timestamp,
    );
    let notification_recovery_key = notification_recovery
        .as_ref()
        .map(|recovery| recovery.key.clone());
    let outcome = project_groupchat_inbox(
        inbox_storage,
        deps.connection_registry,
        &owner,
        &room,
        &message,
        is_recipient,
        &thread,
        dispatch_timestamp,
        notification_recovery,
    )
    .await;
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

#[allow(clippy::too_many_arguments)]
fn groupchat_notification_recovery_item(
    owner: &BareJid,
    room: &BareJid,
    message: &Message,
    is_recipient: bool,
    is_durable_recipient: bool,
    is_live_occupant: bool,
    room_members_only: bool,
    sender_can_broadcast_channel_mention: bool,
    thread: &Option<GroupchatThreadProjection>,
    dispatch_timestamp: i64,
) -> Option<waddle_xmpp::inbox::storage::GroupchatNotificationRecovery> {
    if !is_recipient || !is_durable_recipient {
        return None;
    }
    let archive_id = extract_room_stanza_id(message, room)?;
    let sender_jid = message.from.clone()?;
    Some(waddle_xmpp::inbox::storage::GroupchatNotificationRecovery {
        key: waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey {
            recipient: owner.clone(),
            room: room.clone(),
            thread_id: thread.as_ref().map(|thread| thread.thread_id.clone()),
            archive_stanza_id: Xep0359StanzaId::new(archive_id, Jid::from(room.clone())),
        },
        sender_jid,
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
        created_at_ms: dispatch_timestamp.saturating_mul(1_000),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupchatNotificationCandidateQueueOutcome {
    Completed,
    RetryLater,
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
    insert_groupchat_notification_candidate(
        state,
        owner,
        room,
        message,
        sender_jid,
        thread_id,
        Xep0359StanzaId::new(archive_id, Jid::from(room.clone())),
        is_live_occupant,
        room_members_only,
        sender_can_broadcast_channel_mention,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_groupchat_notification_candidate(
    state: &WebSocketState,
    owner: &BareJid,
    room: &BareJid,
    message: &Message,
    sender_jid: Jid,
    thread_id: crate::notification_outbox::NotificationThreadId,
    archive_stanza_id: Xep0359StanzaId,
    is_live_occupant: bool,
    // `room_members_only` is known message-locally on this T0 path
    // (the projection event carries it) and is consumed below to
    // pre-populate the policy cache so the synchronous T0 evaluator
    // never asks the live `RoomRegistryActor`. Each recipient in a
    // groupchat fan-out would otherwise produce an actor round-trip,
    // even though the same bit is already in hand.
    room_members_only: bool,
    // XEP-0513 §"Multi-User Chats Permissions": frozen permission
    // snapshot for `urn:xmpp:mentions:0#channel`. `false` downgrades
    // channel mentions to `NotifyAll` (still delivered, but not pushed
    // as a forced channel mention). The recovery path passes the
    // bool persisted on the recovery row at original T0 dispatch —
    // see [`reconcile_groupchat_notification_candidates`].
    sender_can_broadcast_channel_mention: bool,
) -> GroupchatNotificationCandidateQueueOutcome {
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
    let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
        mentions_slice,
        message,
        owner,
        room,
        owner_occupant_id.as_str(),
        is_live_occupant,
        sender_can_broadcast_channel_mention,
    );
    // XEP-0513 §304: when the per-message mention count exceeds the
    // threshold, the recipient SHOULD ignore *all* mentions on the
    // message — including any `<noping/>` attribute targeting this
    // recipient. Without this guard a sender could spam mentions to
    // exceed the threshold AND attach `<noping/>` for the recipient,
    // and the `Xep0513Noping` T1 suppressor would still fire — turning
    // the count gate into a push-suppression primitive instead of a
    // spam mitigation (Codex-style adversarial follow-up to slice 3b).
    let recipient_noping = !waddle_xmpp::xep::mentions_exceed_threshold(
        mentions_slice,
        message,
        waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT,
    ) && groupchat_mentions_carry_owner_noping(
        mentions_slice,
        owner,
        owner_occupant_id.as_str(),
    );
    let hints = crate::notification_outbox::NotificationMessageHints::none()
        .with_noping(recipient_noping)
        .with_xep0334(
            waddle_xmpp::xep::xep0334::has_hint(message, waddle_xmpp::xep::xep0334::Hint::NoStore),
            waddle_xmpp::xep::xep0334::has_hint(
                message,
                waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
            ),
        );
    let candidate = match crate::notification_outbox::NotificationCandidate::groupchat_with_hints(
        owner.clone(),
        room.clone(),
        sender_jid,
        thread_id,
        archive_stanza_id,
        class,
        hints,
    ) {
        Ok(candidate) => candidate,
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
        crate::notification_outbox::T1PushDispatchOutcome::Deliver => {}
        crate::notification_outbox::T1PushDispatchOutcome::Suppressed { reason } => {
            info!(
                recipient = %owner,
                room = %room,
                class = ?class,
                %reason,
                "ProjectGroupchatInbox: T0 push gate suppressed groupchat candidate; no candidate row persisted"
            );
            waddle_xmpp::prometheus::increment_push_suppressed(reason.as_db_value());
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
            debug!(
                recipient = %owner,
                room = %room,
                class = ?class,
                "ProjectGroupchatInbox: inserted XEP-0357 groupchat notification candidate"
            );
            GroupchatNotificationCandidateQueueOutcome::Completed
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
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

pub(crate) async fn reconcile_groupchat_notification_candidates(
    state: &WebSocketState,
    batch_size: usize,
) -> usize {
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
            return 0;
        }
    };
    let mut completed = 0usize;
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
                if mark_recovery_completed_from_state(state, &recovery.key).await {
                    completed += 1;
                }
                continue;
            }
            Err(error) => {
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
        let outcome = insert_groupchat_notification_candidate(
            state,
            &recovery.key.recipient,
            &recovery.key.room,
            &message,
            recovery.sender_jid.clone(),
            thread_id,
            recovery.key.archive_stanza_id.clone(),
            recovery.is_live_occupant,
            recovery.room_members_only,
            // XEP-0513 §"Multi-User Chats Permissions" frozen at the
            // original T0 dispatch — persisted on the recovery row so
            // replay re-creates the same notification class. Defaulting
            // to `false` here would silently downgrade every channel
            // mention to `NotifyAll` and let the public-group `OnMention`
            // XEP-0492 default suppress it at T1: a silent moderator-
            // push outage after every server restart (adversarial review
            // P1 on PR #738).
            recovery.sender_can_broadcast_channel_mention,
        )
        .await;
        if outcome == GroupchatNotificationCandidateQueueOutcome::Completed
            && mark_recovery_completed_from_state(state, &recovery.key).await
        {
            completed += 1;
        }
    }
    completed
}

async fn mark_recovery_completed_from_state(
    state: &WebSocketState,
    key: &waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey,
) -> bool {
    match state
        .deps
        .protocol
        .inbox_storage
        .mark_groupchat_notification_recovery_completed(key)
        .await
    {
        Ok(marked) => marked > 0,
        Err(error) => {
            warn!(
                recipient = %key.recipient,
                room = %key.room,
                stanza_id = %key.archive_stanza_id,
                error = %error,
                "Groupchat notification recovery completion marker failed"
            );
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupchatNotificationClassDecision {
    Deliver(crate::notification_outbox::NotificationClass),
}

/// Classify a groupchat candidate from a pre-parsed mention slice.
///
/// Callers MUST pass the result of a single
/// `extract_explicit_mentions(message)` parse so the
/// XEP-0513 traversal is not repeated per derivation. The `message`
/// argument is held only for the XEP-0372 references fallback in
/// `groupchat_mentions_owner` — that XEP carries data outside the
/// explicit-mentions tree and is parsed independently.
fn groupchat_notification_class(
    mentions: &[waddle_xmpp::xep::ExplicitMention],
    message: &Message,
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
) -> GroupchatNotificationClassDecision {
    // XEP-0513 §304: "Receiving entities SHOULD ignore all mentions if
    // the message contains more mentions than the threshold specified
    // by `mentions#count`." When the per-message count exceeds the
    // server-internal default, fall through to `NotifyAll` — neither
    // personal-mention nor channel-mention classification applies.
    // The wire payload is preserved (delivery + MAM unchanged per
    // XEP-0513 §526); only the push class is affected. Per-room
    // override of the threshold is deferred to slice 3c.
    if waddle_xmpp::xep::mentions_exceed_threshold(
        mentions,
        message,
        waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT,
    ) {
        return GroupchatNotificationClassDecision::Deliver(
            crate::notification_outbox::NotificationClass::NotifyAll,
        );
    }
    let personal_mention = groupchat_mentions_owner(mentions, message, owner, owner_occupant_id);
    let channel_mention = groupchat_channel_mention_scope(mentions, room)
        .filter(|_| sender_can_broadcast_channel_mention);
    groupchat_notification_class_from_message(personal_mention, channel_mention, is_live_occupant)
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
        // a `<mention/>` that also carries `mentions='#channel'` is
        // channel-scope, not a personal mention naming `owner` — the
        // `<noping/>` on it suppresses CHANNEL pushes (handled by
        // `current_room_channel_mention`), not the owner's personal
        // notification.
        mention.noping
            && !mention.is_channel()
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
    message: &Message,
    owner: &BareJid,
    owner_occupant_id: &str,
) -> bool {
    let xep0513 = mentions.iter().any(|mention| {
        // XEP-0513 §"Multi-User Chats Permissions" §304 hardening
        // (adversarial review on PR #738): a `<mention/>` that ALSO
        // carries `mentions='urn:xmpp:mentions:0#channel'` is a
        // channel-scope payload, not a personal mention — even when
        // it also carries `jid=`/`occupantid=` attributes. Without
        // the `!mention.is_channel()` guard, a non-permitted sender
        // could bypass the channel-broadcast gate by adding a
        // matching `occupantid=`/`jid=` to a channel mention: the
        // classifier would pick `PersonalMention` first (per-recipient
        // match) and never run the channel-scope downgrade. Treat
        // mixed-attribute mentions as channel-scope only.
        !mention.noping
            && !mention.is_channel()
            && (mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == owner)
                || mention
                    .occupant_id
                    .as_deref()
                    .is_some_and(|mentioned| mentioned == owner_occupant_id))
    });
    let xep0372 = extract_references_from_message(message)
        .into_iter()
        .any(|reference| {
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
            &msg_by_jid,
            &owner,
            occupant_id,
        ));
        let msg_by_occ =
            message_with_mention(waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id));
        assert!(groupchat_mentions_owner(
            &parsed_mentions(&msg_by_occ),
            &msg_by_occ,
            &owner,
            occupant_id,
        ));

        let mut noping = waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id);
        noping.noping = true;
        let msg_noping = message_with_mention(noping);
        assert!(!groupchat_mentions_owner(
            &parsed_mentions(&msg_noping),
            &msg_noping,
            &owner,
            occupant_id,
        ));

        let msg_other = message_with_mention(waddle_xmpp::xep::ExplicitMention::occupant_id(
            "somebody-else",
        ));
        assert!(!groupchat_mentions_owner(
            &parsed_mentions(&msg_other),
            &msg_other,
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
        let GroupchatNotificationClassDecision::Deliver(permitted) = groupchat_notification_class(
            &mentions,
            &msg,
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
        let GroupchatNotificationClassDecision::Deliver(downgraded) = groupchat_notification_class(
            &mentions,
            &msg,
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
        let GroupchatNotificationClassDecision::Deliver(permitted) = groupchat_notification_class(
            &mentions,
            &msg,
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
        let GroupchatNotificationClassDecision::Deliver(downgraded) = groupchat_notification_class(
            &mentions,
            &msg,
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
        let _ = groupchat_notification_class(
            &mentions,
            &msg,
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

        let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
            &mentions,
            &msg,
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
        let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
            &mentions,
            &msg,
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
        let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
            &mentions,
            &msg,
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

        let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
            &mentions,
            &msg,
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

        let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
            &mentions,
            &msg,
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

        let GroupchatNotificationClassDecision::Deliver(class) = groupchat_notification_class(
            &mentions,
            &msg,
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

    /// XEP-0513 §304 + Codex-style hardening: when the threshold is
    /// exceeded, EVERY mention is ignored — including any
    /// `<noping/>` attribute targeting the recipient. Otherwise an
    /// attacker spams mentions to exceed the threshold AND attaches
    /// `<noping/>` for the recipient, and the `Xep0513Noping` T1
    /// suppressor still fires — turning the count gate into a push-
    /// suppression primitive instead of a spam mitigation.
    #[test]
    fn xep0513_mention_count_exceeded_also_ignores_noping_suppression() {
        let owner = bare("charlie@example.com");
        let occupant_id = "room-stable-charlie";
        let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

        // Build threshold+1 mentions; one of them names owner with
        // `<noping/>`. The classifier-side gate handles class
        // downgrade; the candidate-emission site separately gates
        // the noping bit via `mentions_exceed_threshold` (see
        // `enqueue_groupchat_notification_candidate`). This test
        // locks the predicate that the noping derivation reads.
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

        // Sanity: the threshold IS exceeded — the predicate the
        // emission site reads MUST return true.
        assert!(
            waddle_xmpp::xep::mentions_exceed_threshold(&mentions, &msg, threshold),
            "fixture must trip the threshold predicate"
        );
        // And the per-recipient noping helper still reports true
        // by itself — the count gate is the OUTER cancel, not an
        // inner one. The emission site combines the two via:
        //   `!mentions_exceed_threshold && groupchat_mentions_carry_owner_noping`
        // so the final bit on the candidate is `false`.
        assert!(
            groupchat_mentions_carry_owner_noping(&mentions, &owner, occupant_id),
            "the per-recipient noping helper must still see the mention; \
             only the outer count gate cancels the suppression so the \
             two helpers remain individually composable"
        );
    }
}
