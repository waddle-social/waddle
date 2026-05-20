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
        thread,
        outcome,
        recovery_key: _,
    } = input;
    if !is_recipient || !is_durable_recipient {
        return GroupchatNotificationCandidateQueueOutcome::Completed;
    }
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
) -> GroupchatNotificationCandidateQueueOutcome {
    let GroupchatNotificationClassDecision::Deliver(class) =
        groupchat_notification_class(state, owner, room, message, is_live_occupant);
    let candidate = match crate::notification_outbox::NotificationCandidate::groupchat(
        owner.clone(),
        room.clone(),
        sender_jid,
        thread_id,
        archive_stanza_id,
        class,
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
    // T0 XEP-0492 push-dispatch gate — compliance: suppressed
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
    let outcome = match crate::notification_outbox::evaluate_xep0492_at_dispatch(
        state
            .deps
            .protocol
            .notification_settings_projection
            .as_ref(),
        &room_policy,
        &candidate,
        &mut room_policy_cache,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            warn!(
                recipient = %owner,
                room = %room,
                error = %error,
                "ProjectGroupchatInbox: XEP-0492 notification setting lookup failed at T0; deferring candidate"
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
                "ProjectGroupchatInbox: XEP-0492 push gate suppressed groupchat candidate at T0; no candidate row persisted"
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

fn groupchat_notification_class(
    state: &WebSocketState,
    owner: &BareJid,
    room: &BareJid,
    message: &Message,
    // TODO(#526 slice 2): move is_live_occupant evaluation to T1 against
    // the notification_activity projection. For slice 1 the bit stays
    // here because there is no T1 projection of MUC presence yet.
    is_live_occupant: bool,
) -> GroupchatNotificationClassDecision {
    let owner_occupant_id =
        waddle_xmpp::xep::generate_occupant_id(owner, room, &state.deps.occupant_id_secret);
    let personal_mention = groupchat_mentions_owner(message, owner, owner_occupant_id.as_str());
    let channel_mention = groupchat_channel_mention_scope(message, room);
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

fn groupchat_mentions_owner(message: &Message, owner: &BareJid, owner_occupant_id: &str) -> bool {
    let xep0513 = extract_explicit_mentions(message).is_some_and(|mentions| {
        mentions.mentions.iter().any(|mention| {
            !mention.noping
                && (mention
                    .jid
                    .as_ref()
                    .is_some_and(|mentioned| mentioned == owner)
                    || mention
                        .occupant_id
                        .as_deref()
                        .is_some_and(|mentioned| mentioned == owner_occupant_id))
        })
    });
    let owner_raw = owner.to_string();
    let xep0372 = extract_references_from_message(message)
        .into_iter()
        .any(|reference| {
            reference.is_mention() && reference.bare_jid() == Some(owner_raw.as_str())
        });
    xep0513 || xep0372
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupchatChannelMentionScope {
    All,
    Active,
}

fn groupchat_channel_mention_scope(
    message: &Message,
    room: &BareJid,
) -> Option<GroupchatChannelMentionScope> {
    let mentions = extract_explicit_mentions(message)?;
    if mentions
        .mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && !mention.active)
    {
        return Some(GroupchatChannelMentionScope::All);
    }
    mentions
        .mentions
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
    let jid_part = uri.strip_prefix("xmpp:")?.split(['?', ';']).next()?.trim();
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

        assert!(groupchat_mentions_owner(
            &message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(owner.clone())),
            &owner,
            occupant_id,
        ));
        assert!(groupchat_mentions_owner(
            &message_with_mention(waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id)),
            &owner,
            occupant_id,
        ));

        let mut noping = waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id);
        noping.noping = true;
        assert!(!groupchat_mentions_owner(
            &message_with_mention(noping),
            &owner,
            occupant_id,
        ));

        assert!(!groupchat_mentions_owner(
            &message_with_mention(waddle_xmpp::xep::ExplicitMention::occupant_id(
                "somebody-else",
            )),
            &owner,
            occupant_id,
        ));
    }

    #[test]
    fn xep0513_groupchat_channel_mentions_are_room_scoped_and_active_aware() {
        let room = bare("team@muc.example.com");

        assert_eq!(
            groupchat_channel_mention_scope(
                &message_with_mention(waddle_xmpp::xep::ExplicitMention::channel()),
                &room,
            ),
            Some(GroupchatChannelMentionScope::All)
        );

        let mut active = waddle_xmpp::xep::ExplicitMention::active_channel();
        active.uri = Some("xmpp:team@muc.example.com".to_string());
        assert_eq!(
            groupchat_channel_mention_scope(&message_with_mention(active), &room),
            Some(GroupchatChannelMentionScope::Active)
        );

        let mut foreign = waddle_xmpp::xep::ExplicitMention::channel();
        foreign.uri = Some("xmpp:other@muc.example.com".to_string());
        assert_eq!(
            groupchat_channel_mention_scope(&message_with_mention(foreign), &room),
            None
        );

        let mut noping = waddle_xmpp::xep::ExplicitMention::channel();
        noping.noping = true;
        assert_eq!(
            groupchat_channel_mention_scope(&message_with_mention(noping), &room),
            None
        );
    }
}
