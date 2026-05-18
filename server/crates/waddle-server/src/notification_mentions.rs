//! Server-side mention notification policy.
//!
//! This module intentionally derives the push "mention bit" only from
//! structured XMPP mention payloads. Message bodies are never scanned.

use jid::{BareJid, Jid};
use waddle_xmpp::xep::{
    extract_explicit_mentions, extract_references_from_message, ExplicitMention, MentionPermissions,
};
use xmpp_parsers::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupchatMentionScope {
    All,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GroupchatMentionDecision {
    pub personal: Option<GroupchatMentionScope>,
    pub channel: Option<GroupchatMentionScope>,
}

pub(crate) struct GroupchatMentionContext<'a> {
    pub recipient: &'a BareJid,
    pub recipient_is_live_occupant: bool,
    pub recipient_occupant_id: &'a str,
    pub occupant_id_bare_jids: &'a [(waddle_xmpp::xep::OccupantId, BareJid)],
    pub room: &'a BareJid,
    pub sender_role: waddle_xmpp::Role,
    pub permissions: MentionPermissions,
}

pub(crate) fn direct_message_mentions_recipient(message: &Message, recipient: &BareJid) -> bool {
    let explicit_mentions_recipient = extract_explicit_mentions(message).is_some_and(|mentions| {
        mentions
            .mentions
            .iter()
            .any(|mention| mention_matches_direct_recipient(mention, recipient))
    });
    let reference_mentions_recipient =
        extract_references_from_message(message)
            .iter()
            .any(|reference| {
                reference.is_mention()
                    && xmpp_uri_bare_jid(&reference.uri)
                        .is_some_and(|mentioned| mentioned == *recipient)
            });
    explicit_mentions_recipient || reference_mentions_recipient
}

pub(crate) fn groupchat_mention_decision(
    message: &Message,
    ctx: GroupchatMentionContext<'_>,
) -> GroupchatMentionDecision {
    let explicit_mentions = extract_explicit_mentions(message);
    let references = extract_references_from_message(message);
    let supported_targets = supported_logical_mention_targets(
        explicit_mentions
            .as_ref()
            .map_or(&[][..], |mentions| mentions.mentions.as_slice()),
        &references,
        &ctx,
    );

    let permitted_target_count = supported_targets
        .iter()
        .filter(|target| target_allowed_by_permissions(target, &ctx))
        .count();

    if permitted_target_count > ctx.permissions.count as usize {
        return GroupchatMentionDecision::default();
    }

    let personal = if ctx.permissions.individual.allows_role(ctx.sender_role) {
        groupchat_personal_mention_scope(
            explicit_mentions
                .as_ref()
                .map_or(&[][..], |mentions| mentions.mentions.as_slice()),
            &references,
            &ctx,
        )
    } else {
        None
    };

    let channel = if ctx.permissions.channel.allows_role(ctx.sender_role) {
        explicit_mentions
            .as_ref()
            .and_then(|mentions| groupchat_channel_mention_scope(&mentions.mentions, ctx.room))
    } else {
        None
    };

    GroupchatMentionDecision { personal, channel }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SupportedMentionTarget {
    Jid(BareJid),
    OccupantId(String),
    Channel(BareJid),
}

fn supported_logical_mention_targets(
    mentions: &[ExplicitMention],
    references: &[waddle_xmpp::xep::Reference],
    ctx: &GroupchatMentionContext<'_>,
) -> Vec<SupportedMentionTarget> {
    let mut targets = Vec::new();
    for mention in mentions {
        if let Some(target) = supported_explicit_mention_target(mention, ctx) {
            push_unique_target(&mut targets, target);
        }
    }
    for reference in references {
        if !reference.is_mention() {
            continue;
        }
        if let Some(target) = xmpp_uri_bare_jid(&reference.uri).map(SupportedMentionTarget::Jid) {
            push_unique_target(&mut targets, target);
        }
    }
    targets
}

fn supported_explicit_mention_target(
    mention: &ExplicitMention,
    ctx: &GroupchatMentionContext<'_>,
) -> Option<SupportedMentionTarget> {
    if mention.noping {
        return None;
    }
    if mention.mentions.is_none() {
        if let Some(occupant_id) = mention.occupant_id.as_deref() {
            if let Some((_, bare_jid)) = ctx
                .occupant_id_bare_jids
                .iter()
                .find(|(known_occupant_id, _)| known_occupant_id.as_str() == occupant_id)
            {
                return Some(SupportedMentionTarget::Jid(bare_jid.clone()));
            }
            return Some(SupportedMentionTarget::OccupantId(occupant_id.to_string()));
        }
    }
    if current_room_channel_mention(mention, ctx.room) {
        return Some(SupportedMentionTarget::Channel(ctx.room.clone()));
    }
    None
}

fn push_unique_target(targets: &mut Vec<SupportedMentionTarget>, target: SupportedMentionTarget) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

fn target_allowed_by_permissions(
    target: &SupportedMentionTarget,
    ctx: &GroupchatMentionContext<'_>,
) -> bool {
    match target {
        SupportedMentionTarget::Jid(_) | SupportedMentionTarget::OccupantId(_) => {
            ctx.permissions.individual.allows_role(ctx.sender_role)
        }
        SupportedMentionTarget::Channel(_) => ctx.permissions.channel.allows_role(ctx.sender_role),
    }
}

fn mention_matches_direct_recipient(mention: &ExplicitMention, recipient: &BareJid) -> bool {
    !mention.noping
        && mention.mentions.is_none()
        && mention
            .jid
            .as_ref()
            .is_some_and(|mentioned| mentioned == recipient)
}

fn groupchat_personal_mention_scope(
    mentions: &[ExplicitMention],
    references: &[waddle_xmpp::xep::Reference],
    ctx: &GroupchatMentionContext<'_>,
) -> Option<GroupchatMentionScope> {
    let mut scope = None;
    for mention in mentions {
        if !mention_matches_groupchat_recipient(mention, ctx) {
            continue;
        }
        scope = mention_scope_dominant(
            scope,
            if mention.active {
                GroupchatMentionScope::Active
            } else {
                GroupchatMentionScope::All
            },
        );
    }
    for reference in references {
        if !ctx.recipient_is_live_occupant
            && reference.is_mention()
            && xmpp_uri_bare_jid(&reference.uri)
                .is_some_and(|mentioned| mentioned == *ctx.recipient)
        {
            scope = mention_scope_dominant(scope, GroupchatMentionScope::All);
        }
    }
    scope
}

fn mention_matches_groupchat_recipient(
    mention: &ExplicitMention,
    ctx: &GroupchatMentionContext<'_>,
) -> bool {
    if mention.noping || mention.mentions.is_some() {
        return false;
    }
    let Some(mentioned_occupant_id) = mention.occupant_id.as_deref() else {
        return false;
    };
    if ctx
        .occupant_id_bare_jids
        .iter()
        .any(|(known_occupant_id, bare_jid)| {
            known_occupant_id.as_str() == mentioned_occupant_id && bare_jid == ctx.recipient
        })
    {
        return true;
    }
    mentioned_occupant_id == ctx.recipient_occupant_id
}

fn mention_scope_dominant(
    current: Option<GroupchatMentionScope>,
    next: GroupchatMentionScope,
) -> Option<GroupchatMentionScope> {
    match (current, next) {
        (Some(GroupchatMentionScope::All), _) | (_, GroupchatMentionScope::All) => {
            Some(GroupchatMentionScope::All)
        }
        _ => Some(GroupchatMentionScope::Active),
    }
}

fn groupchat_channel_mention_scope(
    mentions: &[ExplicitMention],
    room: &BareJid,
) -> Option<GroupchatMentionScope> {
    if mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && !mention.active)
    {
        return Some(GroupchatMentionScope::All);
    }
    mentions
        .iter()
        .any(|mention| current_room_channel_mention(mention, room) && mention.active)
        .then_some(GroupchatMentionScope::Active)
}

fn current_room_channel_mention(mention: &ExplicitMention, room: &BareJid) -> bool {
    if !mention.is_channel() || mention.noping {
        return false;
    }
    mention
        .uri
        .as_deref()
        .is_none_or(|uri| xmpp_uri_bare_jid(uri).is_some_and(|target| target == *room))
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

    fn message_with_mention(mention: ExplicitMention) -> Message {
        let mut message = Message::new(None::<Jid>);
        message
            .payloads
            .push(waddle_xmpp::xep::build_mention_element(&mention));
        message
    }

    fn message_with_reference(uri: impl Into<String>) -> Message {
        let mut message = Message::new(None::<Jid>);
        message
            .payloads
            .push(waddle_xmpp::xep::build_reference_element(
                &waddle_xmpp::xep::Reference::mention(uri),
            ));
        message
    }

    fn group_context<'a>(
        recipient: &'a BareJid,
        occupant_id: &'a str,
        room: &'a BareJid,
    ) -> GroupchatMentionContext<'a> {
        GroupchatMentionContext {
            recipient,
            recipient_is_live_occupant: false,
            recipient_occupant_id: occupant_id,
            occupant_id_bare_jids: &[],
            room,
            sender_role: waddle_xmpp::Role::Participant,
            permissions: MentionPermissions::default(),
        }
    }

    #[test]
    fn xep0513_direct_message_mentions_respect_noping_and_group_types() {
        let recipient = bare("alice@example.com");

        assert!(direct_message_mentions_recipient(
            &message_with_mention(ExplicitMention::jid(recipient.clone())),
            &recipient,
        ));

        let mut noping = ExplicitMention::jid(recipient.clone());
        noping.noping = true;
        assert!(!direct_message_mentions_recipient(
            &message_with_mention(noping),
            &recipient,
        ));

        let mut unsupported_group = ExplicitMention::jid(recipient.clone());
        unsupported_group.mentions = Some("urn:xmpp:mentions:0#space".to_string());
        assert!(!direct_message_mentions_recipient(
            &message_with_mention(unsupported_group),
            &recipient,
        ));
    }

    #[test]
    fn xep0372_direct_message_reference_mentions_match_bare_jid() {
        let recipient = bare("bob@example.com");

        assert!(direct_message_mentions_recipient(
            &message_with_reference("xmpp:bob@example.com?message"),
            &recipient,
        ));
        assert!(!direct_message_mentions_recipient(
            &message_with_reference("xmpp:carol@example.com?message"),
            &recipient,
        ));
    }

    #[test]
    fn xep0513_groupchat_personal_mentions_match_occupant_id_only() {
        let recipient = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = "room-stable-charlie";

        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(ExplicitMention::jid(recipient.clone())),
                group_context(&recipient, occupant_id, &room),
            )
            .personal,
            None,
            "MUC XEP-0513 jid= mentions are not compliant when XEP-0421 occupant IDs are supported"
        );
        assert!(groupchat_mention_decision(
            &message_with_mention(ExplicitMention::occupant_id(occupant_id)),
            group_context(&recipient, occupant_id, &room),
        )
        .personal
        .is_some());
    }

    #[test]
    fn xep0513_groupchat_personal_mentions_use_frozen_occupant_mapping_first() {
        let recipient = bare("charlie@example.com");
        let room = bare("team@muc.example.com");
        let frozen_occupant_id = waddle_xmpp::xep::OccupantId::new("frozen-charlie");
        let occupant_id_bare_jids = vec![(frozen_occupant_id.clone(), recipient.clone())];

        let decision = groupchat_mention_decision(
            &message_with_mention(ExplicitMention::occupant_id(frozen_occupant_id.as_str())),
            GroupchatMentionContext {
                recipient_occupant_id: "current-secret-charlie",
                occupant_id_bare_jids: &occupant_id_bare_jids,
                ..group_context(&recipient, "current-secret-charlie", &room)
            },
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::All));
    }

    #[test]
    fn xep0372_groupchat_reference_mentions_match_non_live_bare_jid() {
        let recipient = bare("dana@example.com");
        let room = bare("team@muc.example.com");

        let decision = groupchat_mention_decision(
            &message_with_reference("xmpp:dana@example.com?message"),
            group_context(&recipient, "room-stable-dana", &room),
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::All));
    }

    #[test]
    fn xep0372_groupchat_reference_does_not_ping_live_occupant() {
        let recipient = bare("dana@example.com");
        let room = bare("team@muc.example.com");

        let decision = groupchat_mention_decision(
            &message_with_reference("xmpp:dana@example.com?message"),
            GroupchatMentionContext {
                recipient_is_live_occupant: true,
                ..group_context(&recipient, "room-stable-dana", &room)
            },
        );

        assert_eq!(decision.personal, None);
    }

    #[test]
    fn xep0513_groupchat_channel_mentions_are_room_scoped_and_active_aware() {
        let recipient = bare("eric@example.com");
        let room = bare("team@muc.example.com");

        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(ExplicitMention::channel()),
                group_context(&recipient, "room-stable-eric", &room),
            )
            .channel,
            Some(GroupchatMentionScope::All)
        );

        let mut active = ExplicitMention::active_channel();
        active.uri = Some("xmpp:team@muc.example.com".to_string());
        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(active),
                group_context(&recipient, "room-stable-eric", &room),
            )
            .channel,
            Some(GroupchatMentionScope::Active)
        );

        let mut foreign = ExplicitMention::channel();
        foreign.uri = Some("xmpp:other@muc.example.com".to_string());
        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(foreign),
                group_context(&recipient, "room-stable-eric", &room),
            )
            .channel,
            None
        );
    }

    #[test]
    fn xep0513_groupchat_channel_permission_denies_participants() {
        let recipient = bare("fran@example.com");
        let room = bare("team@muc.example.com");
        let permissions = MentionPermissions {
            channel: waddle_xmpp::xep::MentionPermission::Moderators,
            ..MentionPermissions::default()
        };

        let decision = groupchat_mention_decision(
            &message_with_mention(ExplicitMention::channel()),
            GroupchatMentionContext {
                permissions,
                ..group_context(&recipient, "room-stable-fran", &room)
            },
        );

        assert_eq!(decision.channel, None);
    }

    #[test]
    fn xep0513_groupchat_count_threshold_suppresses_all_mentions() {
        let recipient = bare("george@example.com");
        let room = bare("team@muc.example.com");
        let permissions = MentionPermissions {
            count: 1,
            ..MentionPermissions::default()
        };
        let mut message = message_with_mention(ExplicitMention::occupant_id("room-stable-george"));
        message
            .payloads
            .push(waddle_xmpp::xep::build_mention_element(
                &ExplicitMention::channel(),
            ));

        let decision = groupchat_mention_decision(
            &message,
            GroupchatMentionContext {
                permissions,
                ..group_context(&recipient, "room-stable-george", &room)
            },
        );

        assert_eq!(decision, GroupchatMentionDecision::default());
    }

    #[test]
    fn xep0513_count_threshold_ignores_disallowed_channel_mentions() {
        let recipient = bare("hannah@example.com");
        let room = bare("team@muc.example.com");
        let permissions = MentionPermissions {
            count: 1,
            channel: waddle_xmpp::xep::MentionPermission::Moderators,
            ..MentionPermissions::default()
        };
        let mut message = message_with_mention(ExplicitMention::occupant_id("room-stable-hannah"));
        message
            .payloads
            .push(waddle_xmpp::xep::build_mention_element(
                &ExplicitMention::channel(),
            ));

        let decision = groupchat_mention_decision(
            &message,
            GroupchatMentionContext {
                permissions,
                sender_role: waddle_xmpp::Role::Participant,
                ..group_context(&recipient, "room-stable-hannah", &room)
            },
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::All));
        assert_eq!(decision.channel, None);
    }

    #[test]
    fn xep0513_count_threshold_dedupes_equivalent_supported_mentions() {
        let recipient = bare("iris@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = waddle_xmpp::xep::OccupantId::new("room-stable-iris");
        let occupant_id_bare_jids = vec![(occupant_id.clone(), recipient.clone())];
        let permissions = MentionPermissions {
            count: 1,
            ..MentionPermissions::default()
        };
        let mut message = message_with_mention(ExplicitMention::occupant_id(occupant_id.as_str()));
        message
            .payloads
            .push(waddle_xmpp::xep::build_reference_element(
                &waddle_xmpp::xep::Reference::mention("xmpp:iris@example.com"),
            ));

        let decision = groupchat_mention_decision(
            &message,
            GroupchatMentionContext {
                permissions,
                occupant_id_bare_jids: &occupant_id_bare_jids,
                ..group_context(&recipient, "room-stable-iris", &room)
            },
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::All));
    }

    #[test]
    fn xep0513_count_threshold_is_not_recipient_dependent() {
        let recipient = bare("other@example.com");
        let mentioned = bare("iris@example.com");
        let room = bare("team@muc.example.com");
        let occupant_id = waddle_xmpp::xep::OccupantId::new("room-stable-iris");
        let occupant_id_bare_jids = vec![(occupant_id.clone(), mentioned)];
        let permissions = MentionPermissions {
            count: 2,
            ..MentionPermissions::default()
        };
        let mut message = message_with_mention(ExplicitMention::occupant_id(occupant_id.as_str()));
        message
            .payloads
            .push(waddle_xmpp::xep::build_reference_element(
                &waddle_xmpp::xep::Reference::mention("xmpp:iris@example.com"),
            ));
        message
            .payloads
            .push(waddle_xmpp::xep::build_mention_element(
                &ExplicitMention::occupant_id("room-stable-other"),
            ));

        let decision = groupchat_mention_decision(
            &message,
            GroupchatMentionContext {
                permissions,
                occupant_id_bare_jids: &occupant_id_bare_jids,
                ..group_context(&recipient, "room-stable-other", &room)
            },
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::All));
    }

    #[test]
    fn xep0513_groupchat_noping_suppresses_personal_and_channel_mentions() {
        let recipient = bare("noah@example.com");
        let room = bare("team@muc.example.com");
        let mut personal = ExplicitMention::occupant_id("room-stable-noah");
        personal.noping = true;
        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(personal),
                group_context(&recipient, "room-stable-noah", &room),
            ),
            GroupchatMentionDecision::default()
        );

        let mut channel = ExplicitMention::channel();
        channel.noping = true;
        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(channel),
                group_context(&recipient, "room-stable-noah", &room),
            ),
            GroupchatMentionDecision::default()
        );

        let mut active_channel = ExplicitMention::active_channel();
        active_channel.noping = true;
        assert_eq!(
            groupchat_mention_decision(
                &message_with_mention(active_channel),
                group_context(&recipient, "room-stable-noah", &room),
            ),
            GroupchatMentionDecision::default()
        );
    }

    #[test]
    fn xep0513_count_threshold_ignores_unsupported_group_mentions() {
        let recipient = bare("jane@example.com");
        let room = bare("team@muc.example.com");
        let permissions = MentionPermissions {
            count: 1,
            ..MentionPermissions::default()
        };
        let unsupported_group = ExplicitMention {
            mentions: Some("urn:xmpp:mentions:0#space".to_string()),
            ..ExplicitMention::default()
        };
        let mut message = message_with_mention(ExplicitMention::occupant_id("room-stable-jane"));
        message
            .payloads
            .push(waddle_xmpp::xep::build_mention_element(&unsupported_group));

        let decision = groupchat_mention_decision(
            &message,
            GroupchatMentionContext {
                permissions,
                ..group_context(&recipient, "room-stable-jane", &room)
            },
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::All));
    }

    #[test]
    fn xep0513_active_personal_mentions_are_scoped() {
        let recipient = bare("keira@example.com");
        let room = bare("team@muc.example.com");
        let mut mention = ExplicitMention::occupant_id("room-stable-keira");
        mention.active = true;

        let decision = groupchat_mention_decision(
            &message_with_mention(mention),
            group_context(&recipient, "room-stable-keira", &room),
        );

        assert_eq!(decision.personal, Some(GroupchatMentionScope::Active));
    }

    #[test]
    fn xep0513_unsupported_group_mentions_are_ignored_for_push() {
        let recipient = bare("hannah@example.com");
        let room = bare("team@muc.example.com");
        let mention = ExplicitMention {
            mentions: Some("urn:xmpp:mentions:0#space".to_string()),
            ..ExplicitMention::default()
        };

        let decision = groupchat_mention_decision(
            &message_with_mention(mention),
            group_context(&recipient, "room-stable-hannah", &room),
        );

        assert_eq!(decision, GroupchatMentionDecision::default());
    }
}
