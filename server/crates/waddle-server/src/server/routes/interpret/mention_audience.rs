//! Groupchat mention audience: personally-mentioned users outside a room's
//! durable recipient set who still get an inbox row.
//!
//! In an open room most users hold affiliation `none`, so they are not
//! durable recipients. When a message personally mentions one of them
//! (XEP-0372 mention reference or XEP-0513 `jid=` mention) while they are
//! away, the inbox projection would otherwise skip them. This module
//! resolves that extra audience once, while planning the room dispatch;
//! the planned and recorded effects carry the result, so recovery never
//! re-resolves it.
//!
//! A revision of an archived message (XEP-0308 correction, XEP-0424
//! retraction, XEP-0425 moderation) re-derives the audience the archived
//! message notified, by running the same rules over its archived mentions,
//! so the revision replaces the content in those users' rows too.
//!
//! Every check fails closed: a target is included only when it is a local
//! account, has not blocked the author (XEP-0191, matched against the
//! author's real JID, occupant JID, the room and the MUC service), and may
//! enter the room.

use super::{
    extract_explicit_mentions, extract_references_from_message, interpret, warn, ActorRef, Arc,
    BareJid, BlockingStorage, Blocklist, Deps, FullJid, GetAffiliation, GetRoomSnapshot,
    GroupchatInboxAudience, Jid, MamArchivedMessage, Message, OutboundEvent, RoomActor,
    WebSocketState,
};
use crate::server::xmpp_state::XmppChannelRecord;
use futures::stream::{self, StreamExt};
use waddle_xmpp::mam::{ArchivedRichMessage, ArchivedRichPayload};
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::telemetry::attributes::MentionAudienceCheck;
use waddle_xmpp::telemetry::messages::record_mention_audience_check_failed;
use waddle_xmpp::xep::{
    mentions_exceed_threshold_from_parts, personal_mention_targets, ExplicitMention, Reference,
    ReferenceType, DEFAULT_MENTIONS_COUNT,
};

use crate::server::routes::websocket::handlers::presence::{
    get_managed_channel_for_room, resolve_managed_channel_entry_access, RoomArchiveAccess,
};

/// Upper bound on concurrent per-target checks. XEP-0513 §304's default
/// `mentions#count` already caps the targets at this size.
const MAX_CONCURRENT_TARGET_CHECKS: usize = DEFAULT_MENTIONS_COUNT as usize;

/// Frozen room facts shared by every audience resolution in one dispatch.
pub(crate) struct AudienceScope<'a> {
    pub state: &'a WebSocketState,
    pub local_domain: &'a str,
    pub blocking_storage: Option<&'a Arc<dyn BlockingStorage>>,
    pub room: &'a BareJid,
    pub room_actor: &'a ActorRef<RoomActor>,
    /// Room config from the dispatch snapshot.
    pub config: &'a RoomConfig,
    /// Durable recipients from the same snapshot.
    pub durable_recipients: &'a [BareJid],
}

/// The author of the mentioning message, as a mentioned user's XEP-0191
/// blocklist sees them.
pub(crate) struct MentionAuthor {
    /// Real JID (full when known), excluded from the audience by its bare JID.
    pub real: Option<Jid>,
    /// XEP-0045 occupant JID (`room@service/nick`).
    pub occupant: Option<Jid>,
}

/// Resolve the mentioned non-members who get a
/// [`GroupchatInboxAudience::MentionedNonMember`] inbox row for a new
/// message.
///
/// Returns an empty list for members-only rooms (their audience is the
/// durable member set), for corrections (the original message already
/// notified; see [`resolve_revision_audience`]), and when the message
/// exceeds the XEP-0513 §304 mention cap (all mentions are ignored,
/// matching notification classification).
pub(crate) async fn resolve_mention_audience(
    scope: &AudienceScope<'_>,
    author: &MentionAuthor,
    message: &Message,
    is_correction: bool,
) -> Vec<BareJid> {
    if is_correction {
        return Vec::new();
    }
    let mentions = extract_explicit_mentions(message)
        .map(|mentions| mentions.mentions)
        .unwrap_or_default();
    let references = extract_references_from_message(message);
    resolve_from_parts(scope, author, &mentions, &references).await
}

/// Resolve the mentioned non-members an archived message notified, so a
/// revision of it (correction, retraction, moderation) can replace the
/// content in their rows with a
/// [`GroupchatInboxAudience::MentionedNonMemberRevision`] row.
///
/// Re-runs exactly the rules [`resolve_mention_audience`] applied to the
/// archived message: nothing for an archived correction or tombstone,
/// nothing over the mention cap, and the same per-target checks.
pub(crate) async fn resolve_revision_audience(
    scope: &AudienceScope<'_>,
    author: &MentionAuthor,
    revised: &MamArchivedMessage,
) -> Vec<BareJid> {
    let Some(rich) = revised.rich.as_ref() else {
        return Vec::new();
    };
    if matches!(
        rich.payload,
        Some(ArchivedRichPayload::Correction { .. } | ArchivedRichPayload::Tombstone(_))
    ) {
        return Vec::new();
    }
    let (mentions, references) = archived_mention_parts(rich);
    resolve_from_parts(scope, author, &mentions, &references).await
}

/// The author identities of an archived groupchat message: the room-stamped
/// real JID and the occupant JID it was archived from.
pub(crate) fn archived_author(revised: &MamArchivedMessage) -> MentionAuthor {
    MentionAuthor {
        real: revised
            .rich
            .as_ref()
            .and_then(|rich| rich.muc_sender.as_ref())
            .map(|sender| sender.jid.clone()),
        occupant: Some(revised.from.clone()),
    }
}

fn archived_mention_parts(rich: &ArchivedRichMessage) -> (Vec<ExplicitMention>, Vec<Reference>) {
    let mentions = rich
        .mentions
        .iter()
        .map(|mention| ExplicitMention {
            begin: mention.begin,
            end: mention.end,
            jid: mention.jid.clone(),
            occupant_id: mention
                .occupant_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            mentions: mention
                .mentions
                .as_ref()
                .map(|group| group.as_str().to_owned()),
            uri: mention.uri.as_ref().map(|uri| uri.as_str().to_owned()),
            active: mention.active,
            noping: mention.noping,
        })
        .collect();
    let references = rich
        .references
        .iter()
        .filter_map(|reference| {
            Some(Reference {
                ref_type: ReferenceType::from_str_attr(reference.ref_type.as_str())?,
                begin: reference
                    .begin
                    .and_then(|value| usize::try_from(value).ok()),
                end: reference.end.and_then(|value| usize::try_from(value).ok()),
                uri: reference.uri.as_ref()?.as_str().to_owned(),
                anchor: reference
                    .anchor
                    .as_ref()
                    .map(|anchor| anchor.as_str().to_owned()),
            })
        })
        .collect();
    (mentions, references)
}

async fn resolve_from_parts(
    scope: &AudienceScope<'_>,
    author: &MentionAuthor,
    mentions: &[ExplicitMention],
    references: &[Reference],
) -> Vec<BareJid> {
    if scope.config.requires_membership()
        || mentions_exceed_threshold_from_parts(mentions, references, DEFAULT_MENTIONS_COUNT)
    {
        return Vec::new();
    }
    let author_bare = author.real.as_ref().map(Jid::to_bare);
    let candidates: Vec<BareJid> = personal_mention_targets(mentions, references)
        .into_iter()
        .filter(|target| {
            author_bare.as_ref() != Some(target) && !scope.durable_recipients.contains(target)
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    let entry = match get_managed_channel_for_room(scope.state, scope.room).await {
        Ok(Some(channel)) => EntryAuthority::Managed(channel),
        Ok(None) => EntryAuthority::Unmanaged,
        Err(error) => {
            warn!(
                room = %scope.room,
                %error,
                "Mention audience: managed channel lookup failed; excluding mentioned non-members"
            );
            EntryAuthority::Unavailable
        }
    };
    let target_checks = TargetChecks {
        scope,
        author_identities: author_identities(author, scope.room),
        entry: &entry,
    };
    stream::iter(candidates)
        .map(|target| async {
            let admitted = target_checks.admits(&target).await;
            admitted.then_some(target)
        })
        .buffered(MAX_CONCURRENT_TARGET_CHECKS)
        .filter_map(std::future::ready)
        .collect()
        .await
}

/// Every JID a mentioned user may have blocked to stop hearing from this
/// author in this room. The room's bare JID also matches a block of the
/// whole MUC service domain under XEP-0191 matching; the occupant JID
/// matches blocks of `room@service/nick` and `service/nick`.
fn author_identities(author: &MentionAuthor, room: &BareJid) -> Vec<Jid> {
    author
        .real
        .iter()
        .chain(author.occupant.iter())
        .cloned()
        .chain(std::iter::once(Jid::from(room.clone())))
        .collect()
}

/// XEP-0425: replace a moderated message's content in the rows of the
/// mentioned non-members it notified. The moderation IQ archives and
/// broadcasts outside the room dispatch, so it projects the moderation
/// result here, as a
/// [`GroupchatInboxAudience::MentionedNonMemberRevision`] row (no unread
/// bump, no notification) per user the moderated message reached.
pub(crate) async fn project_moderation_to_mentioned_non_members(
    state: &WebSocketState,
    room: &BareJid,
    room_actor: &ActorRef<RoomActor>,
    moderator: &FullJid,
    moderated: &MamArchivedMessage,
    moderation: &Message,
) {
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: moderator.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "Mention audience: room snapshot failed; moderation reaches no mentioned non-member rows"
            );
            return;
        }
    };
    let mut deps = Deps::new(
        &state.deps.protocol.connection_registry,
        state.deps.auth_state.xmpp_domain.as_str(),
    );
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.mam_storage = Some(&state.deps.protocol.mam_storage);
    deps.inbox_storage = Some(&state.deps.protocol.inbox_storage);
    deps.blocking_storage = Some(&state.deps.protocol.blocking_storage);
    deps.web_socket_state = Some(state);
    let scope = AudienceScope {
        state,
        local_domain: deps.local_domain,
        blocking_storage: deps.blocking_storage,
        room,
        room_actor,
        config: &snapshot.config,
        durable_recipients: &snapshot.durable_recipient_bare_jids,
    };
    let owners = resolve_revision_audience(&scope, &archived_author(moderated), moderated).await;
    if owners.is_empty() {
        return;
    }
    let dispatch_timestamp = chrono::Utc::now().timestamp();
    let events = owners
        .into_iter()
        .map(|owner| OutboundEvent::ProjectGroupchatInbox {
            owner,
            room: room.clone(),
            message: Box::new(moderation.clone()),
            audience: GroupchatInboxAudience::MentionedNonMemberRevision,
            is_live_occupant: false,
            room_members_only: snapshot.config.members_only,
            sender_can_broadcast_channel_mention: false,
            thread: None,
            dispatch_timestamp,
        })
        .collect();
    interpret(events, &deps).await;
}

/// Where a managed channel's room-entry decision comes from. Resolved
/// once per dispatch so per-target checks never re-read the channel row.
enum EntryAuthority {
    Managed(XmppChannelRecord),
    Unmanaged,
    Unavailable,
}

struct TargetChecks<'a> {
    scope: &'a AudienceScope<'a>,
    author_identities: Vec<Jid>,
    entry: &'a EntryAuthority,
}

impl TargetChecks<'_> {
    async fn admits(&self, target: &BareJid) -> bool {
        match self.is_local_account(target).await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(()) => return fail_closed(MentionAudienceCheck::Account),
        }
        match self.has_blocked_author(target).await {
            Ok(false) => {}
            Ok(true) => return false,
            Err(()) => return fail_closed(MentionAudienceCheck::Blocking),
        }
        match self.may_enter(target).await {
            Ok(allowed) => allowed,
            Err(()) => fail_closed(MentionAudienceCheck::RoomAccess),
        }
    }

    async fn is_local_account(&self, target: &BareJid) -> Result<bool, ()> {
        crate::auth::local_account_jid_exists(
            self.scope.state.deps.app_state.db_pool.global_actor(),
            target,
            self.scope.local_domain,
        )
        .await
        .map_err(|error| {
            warn!(%error, "Mention audience: account lookup failed");
        })
    }

    /// XEP-0191: a user who blocked the author, the occupant, the room or
    /// the MUC service must not receive the mention as an inbox row or
    /// notification.
    async fn has_blocked_author(&self, target: &BareJid) -> Result<bool, ()> {
        let Some(storage) = self.scope.blocking_storage else {
            warn!("Mention audience: no blocking storage wired");
            return Err(());
        };
        let entries = storage
            .list_blocked_jid_entries(target)
            .await
            .map_err(|error| {
                warn!(%error, "Mention audience: blocklist read failed");
            })?;
        let blocklist = Blocklist::new(entries);
        Ok(self
            .author_identities
            .iter()
            .any(|identity| blocklist.contains_jid(identity)))
    }

    /// XEP-0045 right to enter: the open-room rule (any non-outcast),
    /// including permission-graph bans on managed channels.
    async fn may_enter(&self, target: &BareJid) -> Result<bool, ()> {
        let scope = self.scope;
        match self.entry {
            EntryAuthority::Managed(channel) => {
                match resolve_managed_channel_entry_access(
                    scope.state,
                    scope.room,
                    target,
                    channel,
                    scope.config.members_only,
                )
                .await
                {
                    RoomArchiveAccess::Allowed => Ok(true),
                    RoomArchiveAccess::Denied => Ok(false),
                    RoomArchiveAccess::Error => Err(()),
                }
            }
            EntryAuthority::Unmanaged => scope
                .room_actor
                .ask(GetAffiliation {
                    jid: target.clone(),
                })
                .await
                .map(|affiliation| affiliation != waddle_xmpp::Affiliation::Outcast)
                .map_err(|error| {
                    warn!(room = %scope.room, error = ?error, "Mention audience: affiliation read failed");
                }),
            EntryAuthority::Unavailable => Err(()),
        }
    }
}

fn fail_closed(check: MentionAudienceCheck) -> bool {
    record_mention_audience_check_failed(check);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("bare JID")
    }

    #[test]
    fn xep0191_author_identities_cover_real_occupant_room_and_service_blocks() {
        let room = bare("lobby@muc.example.com");
        let author = MentionAuthor {
            real: Some("alice@example.com/phone".parse().expect("real JID")),
            occupant: Some("lobby@muc.example.com/alice".parse().expect("occupant JID")),
        };
        let identities = author_identities(&author, &room);
        for blocked in [
            "alice@example.com",
            "alice@example.com/phone",
            "lobby@muc.example.com",
            "lobby@muc.example.com/alice",
            "muc.example.com",
            "muc.example.com/alice",
        ] {
            let blocklist = Blocklist::new([blocked.parse::<Jid>().expect("blocked JID")]);
            assert!(
                identities
                    .iter()
                    .any(|identity| blocklist.contains_jid(identity)),
                "a block of {blocked} must stop the mention"
            );
        }
        let unrelated = Blocklist::new(["bob@example.com".parse::<Jid>().expect("other JID")]);
        assert!(!identities
            .iter()
            .any(|identity| unrelated.contains_jid(identity)));
    }

    #[test]
    fn xep0372_archived_mentions_round_trip_into_personal_targets() {
        let rich = ArchivedRichMessage {
            references: vec![waddle_xmpp::mam::ArchivedReference {
                ref_type: waddle_xmpp::mam::RichText::try_new("mention").expect("type"),
                begin: Some(0),
                end: Some(5),
                uri: waddle_xmpp::mam::RichText::try_new("xmpp:carol@example.com"),
                anchor: None,
            }],
            mentions: vec![waddle_xmpp::mam::ArchivedMention {
                begin: None,
                end: None,
                jid: Some(bare("dave@example.com")),
                occupant_id: None,
                mentions: None,
                uri: None,
                active: false,
                noping: true,
            }],
            ..Default::default()
        };
        let (mentions, references) = archived_mention_parts(&rich);
        let targets: Vec<BareJid> = personal_mention_targets(&mentions, &references)
            .into_iter()
            .collect();
        assert_eq!(
            targets,
            vec![bare("carol@example.com")],
            "archived <noping/> mentions stay excluded, references stay targets"
        );
    }
}
