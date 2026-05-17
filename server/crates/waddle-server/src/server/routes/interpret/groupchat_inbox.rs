use super::groupchat_archive::{extract_room_stanza_id, GroupchatInboxProjectionOutcome};
use super::*;

pub(super) struct ProjectGroupchatInboxEvent<'a, 'deps> {
    pub deps: &'a Deps<'deps>,
    pub owner: BareJid,
    pub room: BareJid,
    pub message: Box<Message>,
    pub is_recipient: bool,
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
    let outcome = project_groupchat_inbox(
        inbox_storage,
        deps.connection_registry,
        &owner,
        &room,
        &message,
        is_recipient,
        &thread,
        dispatch_timestamp,
    )
    .await;
    maybe_enqueue_groupchat_notification_candidate(GroupchatNotificationProjection {
        deps,
        owner: &owner,
        room: &room,
        message: &message,
        is_recipient,
        is_live_occupant,
        room_members_only,
        thread: &thread,
        outcome,
    })
    .await;
}

struct GroupchatNotificationProjection<'a, 'deps> {
    deps: &'a Deps<'deps>,
    owner: &'a BareJid,
    room: &'a BareJid,
    message: &'a Message,
    is_recipient: bool,
    is_live_occupant: bool,
    room_members_only: bool,
    thread: &'a Option<GroupchatThreadProjection>,
    outcome: GroupchatInboxProjectionOutcome,
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
        is_live_occupant,
        room_members_only,
        thread,
        outcome,
    } = input;
    if !is_recipient {
        return;
    }
    let projection_committed = thread
        .as_ref()
        .map_or(outcome.channel_committed, |_| outcome.thread_committed);
    if !projection_committed {
        return;
    }
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let Some(archive_id) = extract_room_stanza_id(message, room) else {
        warn!(
            recipient = %owner,
            room = %room,
            "ProjectGroupchatInbox: skipping XEP-0357 candidate; message has no room stanza-id"
        );
        return;
    };
    let Some(sender_jid) = message.from.clone() else {
        warn!(
            recipient = %owner,
            room = %room,
            "ProjectGroupchatInbox: skipping XEP-0357 candidate; message has no sender"
        );
        return;
    };
    let Some(class) = groupchat_notification_class(
        state,
        owner,
        room,
        message,
        is_live_occupant,
        room_members_only,
    )
    .await
    else {
        return;
    };
    let thread_id = thread
        .as_ref()
        .map(|thread| {
            crate::notification_outbox::NotificationThreadId::new(thread.thread_id.clone())
        })
        .unwrap_or_else(crate::notification_outbox::NotificationThreadId::root);
    let candidate = match crate::notification_outbox::NotificationCandidate::groupchat(
        owner.clone(),
        room.clone(),
        sender_jid,
        thread_id,
        Xep0359StanzaId::new(archive_id, Jid::from(room.clone())),
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
            return;
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
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
            debug!(
                recipient = %owner,
                room = %room,
                class = ?class,
                "ProjectGroupchatInbox: duplicate XEP-0357 groupchat notification candidate ignored"
            );
        }
        Err(error) => {
            warn!(
                recipient = %owner,
                room = %room,
                error = %error,
                "ProjectGroupchatInbox: XEP-0357 groupchat notification candidate insert failed"
            );
        }
    }
}

async fn groupchat_notification_class(
    state: &WebSocketState,
    owner: &BareJid,
    room: &BareJid,
    message: &Message,
    is_live_occupant: bool,
    room_members_only: bool,
) -> Option<crate::notification_outbox::NotificationClass> {
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
                "ProjectGroupchatInbox: XEP-0492 setting lookup failed; suppressing groupchat push fail-closed"
            );
            return None;
        }
    };
    let personal_mention = groupchat_mentions_owner(message, owner);
    let channel_mention = groupchat_channel_mention_scope(message);
    let is_mention = personal_mention || channel_mention.is_some();
    if !crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention)
        .should_deliver()
    {
        return None;
    }
    if personal_mention {
        return Some(crate::notification_outbox::NotificationClass::PersonalMention);
    }
    match channel_mention {
        Some(GroupchatChannelMentionScope::Active) if is_live_occupant => {
            return Some(crate::notification_outbox::NotificationClass::ActiveChannelMention);
        }
        Some(GroupchatChannelMentionScope::All) => {
            return Some(crate::notification_outbox::NotificationClass::ChannelMention);
        }
        _ => {}
    }
    if level == waddle_xmpp::xep::NotificationLevel::Always {
        return Some(crate::notification_outbox::NotificationClass::NotifyAll);
    }
    None
}

fn groupchat_mentions_owner(message: &Message, owner: &BareJid) -> bool {
    let xep0513 = extract_explicit_mentions(message).is_some_and(|mentions| {
        mentions.mentions.iter().any(|mention| {
            !mention.noping
                && mention
                    .jid
                    .as_ref()
                    .is_some_and(|mentioned| mentioned == owner)
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

fn groupchat_channel_mention_scope(message: &Message) -> Option<GroupchatChannelMentionScope> {
    let mentions = extract_explicit_mentions(message)?;
    if mentions
        .mentions
        .iter()
        .any(|mention| mention.is_channel() && mention.active && !mention.noping)
    {
        return Some(GroupchatChannelMentionScope::Active);
    }
    mentions
        .mentions
        .iter()
        .any(|mention| mention.is_channel() && !mention.noping)
        .then_some(GroupchatChannelMentionScope::All)
}
