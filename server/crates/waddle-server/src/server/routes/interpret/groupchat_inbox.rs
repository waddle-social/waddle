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
    pub sender_role: waddle_xmpp::Role,
    pub mention_permissions: waddle_xmpp::xep::MentionPermissions,
    pub occupant_id_bare_jids: Vec<(waddle_xmpp::xep::OccupantId, BareJid)>,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids.clone(),
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids: &occupant_id_bare_jids,
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
    sender_role: waddle_xmpp::Role,
    mention_permissions: waddle_xmpp::xep::MentionPermissions,
    occupant_id_bare_jids: &'a [(waddle_xmpp::xep::OccupantId, BareJid)],
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
    sender_role: waddle_xmpp::Role,
    mention_permissions: waddle_xmpp::xep::MentionPermissions,
    occupant_id_bare_jids: Vec<(waddle_xmpp::xep::OccupantId, BareJid)>,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
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
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
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
    room_members_only: bool,
    sender_role: waddle_xmpp::Role,
    mention_permissions: waddle_xmpp::xep::MentionPermissions,
    occupant_id_bare_jids: &[(waddle_xmpp::xep::OccupantId, BareJid)],
) -> GroupchatNotificationCandidateQueueOutcome {
    let class = match groupchat_notification_class(GroupchatNotificationClassInput {
        state,
        owner,
        room,
        message,
        is_live_occupant,
        room_members_only,
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
    })
    .await
    {
        GroupchatNotificationClassDecision::Deliver(class) => class,
        GroupchatNotificationClassDecision::Suppress => {
            return GroupchatNotificationCandidateQueueOutcome::Completed;
        }
        GroupchatNotificationClassDecision::RetryLater => {
            return GroupchatNotificationCandidateQueueOutcome::RetryLater;
        }
    };
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
            recovery.sender_role,
            recovery.mention_permissions,
            &recovery.occupant_id_bare_jids,
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
    Suppress,
    RetryLater,
}

struct GroupchatNotificationClassInput<'a> {
    state: &'a WebSocketState,
    owner: &'a BareJid,
    room: &'a BareJid,
    message: &'a Message,
    is_live_occupant: bool,
    room_members_only: bool,
    sender_role: waddle_xmpp::Role,
    mention_permissions: waddle_xmpp::xep::MentionPermissions,
    occupant_id_bare_jids: &'a [(waddle_xmpp::xep::OccupantId, BareJid)],
}

async fn groupchat_notification_class(
    input: GroupchatNotificationClassInput<'_>,
) -> GroupchatNotificationClassDecision {
    let GroupchatNotificationClassInput {
        state,
        owner,
        room,
        message,
        is_live_occupant,
        room_members_only,
        sender_role,
        mention_permissions,
        occupant_id_bare_jids,
    } = input;
    let conversation_kind = if room_members_only {
        crate::notification_settings_projection::ConversationKind::PrivateGroup
    } else {
        crate::notification_settings_projection::ConversationKind::PublicGroup
    };
    let level = match state
        .deps
        .protocol
        .notification_settings_projection
        .effective_setting(owner, room, conversation_kind)
        .await
    {
        Ok(level) => level,
        Err(error) => {
            warn!(
                recipient = %owner,
                room = %room,
                error = %error,
                "ProjectGroupchatInbox: XEP-0492 setting lookup failed; retrying groupchat push candidate later"
            );
            return GroupchatNotificationClassDecision::RetryLater;
        }
    };
    let owner_occupant_id =
        waddle_xmpp::xep::generate_occupant_id(owner, room, &state.deps.occupant_id_secret);
    let mention_decision = crate::notification_mentions::groupchat_mention_decision(
        message,
        crate::notification_mentions::GroupchatMentionContext {
            recipient: owner,
            recipient_is_live_occupant: is_live_occupant,
            recipient_occupant_id: owner_occupant_id.as_str(),
            occupant_id_bare_jids,
            room,
            sender_role,
            permissions: mention_permissions,
        },
    );
    groupchat_notification_class_for_level(level, mention_decision, is_live_occupant)
}

fn groupchat_notification_class_for_level(
    level: waddle_xmpp::xep::NotificationLevel,
    mention_decision: crate::notification_mentions::GroupchatMentionDecision,
    is_live_occupant: bool,
) -> GroupchatNotificationClassDecision {
    let personal_mention = match mention_decision.personal {
        Some(crate::notification_mentions::GroupchatMentionScope::All) => true,
        Some(crate::notification_mentions::GroupchatMentionScope::Active) => is_live_occupant,
        None => false,
    };
    let is_mention = personal_mention || mention_decision.channel.is_some();
    if !crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention)
        .should_deliver()
    {
        return GroupchatNotificationClassDecision::Suppress;
    }
    if personal_mention {
        return GroupchatNotificationClassDecision::Deliver(
            crate::notification_outbox::NotificationClass::PersonalMention,
        );
    }
    match mention_decision.channel {
        Some(crate::notification_mentions::GroupchatMentionScope::Active) if is_live_occupant => {
            return GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::ActiveChannelMention,
            );
        }
        Some(crate::notification_mentions::GroupchatMentionScope::All) => {
            return GroupchatNotificationClassDecision::Deliver(
                crate::notification_outbox::NotificationClass::ChannelMention,
            );
        }
        _ => {}
    }
    if level == waddle_xmpp::xep::NotificationLevel::Always {
        return GroupchatNotificationClassDecision::Deliver(
            crate::notification_outbox::NotificationClass::NotifyAll,
        );
    }
    GroupchatNotificationClassDecision::Suppress
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_mentions::{GroupchatMentionDecision, GroupchatMentionScope};

    #[test]
    fn xep0492_groupchat_push_policy_matrix() {
        use crate::notification_outbox::NotificationClass;
        use waddle_xmpp::xep::NotificationLevel;

        let cases = [
            (
                NotificationLevel::Always,
                GroupchatMentionDecision::default(),
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::NotifyAll),
            ),
            (
                NotificationLevel::Always,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::All),
                    channel: None,
                },
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::PersonalMention),
            ),
            (
                NotificationLevel::Always,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::Active),
                    channel: None,
                },
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::NotifyAll),
            ),
            (
                NotificationLevel::Always,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::Active),
                    channel: None,
                },
                true,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::PersonalMention),
            ),
            (
                NotificationLevel::Always,
                GroupchatMentionDecision {
                    personal: None,
                    channel: Some(GroupchatMentionScope::All),
                },
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::ChannelMention),
            ),
            (
                NotificationLevel::Always,
                GroupchatMentionDecision {
                    personal: None,
                    channel: Some(GroupchatMentionScope::Active),
                },
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::NotifyAll),
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision::default(),
                true,
                GroupchatNotificationClassDecision::Suppress,
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::All),
                    channel: None,
                },
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::PersonalMention),
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::Active),
                    channel: None,
                },
                false,
                GroupchatNotificationClassDecision::Suppress,
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::Active),
                    channel: None,
                },
                true,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::PersonalMention),
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision {
                    personal: None,
                    channel: Some(GroupchatMentionScope::All),
                },
                false,
                GroupchatNotificationClassDecision::Deliver(NotificationClass::ChannelMention),
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision {
                    personal: None,
                    channel: Some(GroupchatMentionScope::Active),
                },
                true,
                GroupchatNotificationClassDecision::Deliver(
                    NotificationClass::ActiveChannelMention,
                ),
            ),
            (
                NotificationLevel::OnMention,
                GroupchatMentionDecision {
                    personal: None,
                    channel: Some(GroupchatMentionScope::Active),
                },
                false,
                GroupchatNotificationClassDecision::Suppress,
            ),
            (
                NotificationLevel::Never,
                GroupchatMentionDecision {
                    personal: Some(GroupchatMentionScope::All),
                    channel: None,
                },
                true,
                GroupchatNotificationClassDecision::Suppress,
            ),
            (
                NotificationLevel::Never,
                GroupchatMentionDecision {
                    personal: None,
                    channel: Some(GroupchatMentionScope::All),
                },
                true,
                GroupchatNotificationClassDecision::Suppress,
            ),
            (
                NotificationLevel::Never,
                GroupchatMentionDecision::default(),
                false,
                GroupchatNotificationClassDecision::Suppress,
            ),
        ];

        for (level, mention_decision, is_live_occupant, expected) in cases {
            assert_eq!(
                groupchat_notification_class_for_level(level, mention_decision, is_live_occupant,),
                expected,
                "unexpected groupchat XEP-0492 decision for {level:?}, mention_decision={mention_decision:?}, is_live_occupant={is_live_occupant}"
            );
        }
    }
}
