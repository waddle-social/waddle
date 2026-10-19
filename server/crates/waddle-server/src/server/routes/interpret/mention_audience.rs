//! Groupchat mention audience: personally-mentioned users outside a room's
//! durable recipient set who still get an inbox row and a notification.
//!
//! In an open room most users hold affiliation `none`, so they are not
//! durable recipients. When a message personally mentions one of them
//! (XEP-0372 mention reference or XEP-0513 `jid=` mention) while they are
//! away, the inbox projection would otherwise skip them. This module
//! resolves that extra audience once, while planning the room dispatch;
//! the planned and recorded effects carry the result, so recovery never
//! re-resolves it.
//!
//! Every check fails closed: a target is included only when it is a local
//! account, has not blocked the sender (XEP-0191), and may enter the room.

use super::*;
use futures::stream::{self, StreamExt};
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::telemetry::attributes::MentionAudienceCheck;
use waddle_xmpp::telemetry::messages::record_mention_audience_check_failed;
use waddle_xmpp::xep::{
    mentions_exceed_threshold_from_parts, personal_mention_targets, DEFAULT_MENTIONS_COUNT,
};

use crate::server::routes::websocket::handlers::presence::{
    get_managed_channel_for_room, resolve_managed_channel_entry_access, RoomArchiveAccess,
};

/// Upper bound on concurrent per-target checks. XEP-0513 §304's default
/// `mentions#count` already caps the targets at this size.
const MAX_CONCURRENT_TARGET_CHECKS: usize = DEFAULT_MENTIONS_COUNT as usize;

/// Frozen room-dispatch inputs for [`resolve_mention_audience`].
pub(super) struct MentionAudienceRequest<'a> {
    pub state: &'a WebSocketState,
    pub deps: &'a Deps<'a>,
    pub room: &'a BareJid,
    pub room_actor: &'a ActorRef<RoomActor>,
    pub sender: &'a FullJid,
    /// The message as it will be projected (after enrichment).
    pub message: &'a Message,
    /// Room config from the dispatch snapshot.
    pub config: &'a RoomConfig,
    /// Durable recipients from the same snapshot.
    pub durable_recipients: &'a [BareJid],
    /// `true` when the message is an XEP-0308 correction.
    pub is_correction: bool,
}

/// Where a managed channel's room-entry decision comes from. Resolved
/// once per dispatch so per-target checks never re-read the channel row.
enum EntryAuthority {
    Managed(crate::server::xmpp_state::XmppChannelRecord),
    Unmanaged,
    Unavailable,
}

/// Resolve the mentioned non-members who get a
/// [`GroupchatInboxAudience::MentionedNonMember`] inbox row.
///
/// Returns an empty list for members-only rooms (their audience is the
/// durable member set), for corrections (the original message already
/// notified), and when the message exceeds the XEP-0513 §304 mention cap
/// (all mentions are ignored, matching notification classification).
pub(super) async fn resolve_mention_audience(request: MentionAudienceRequest<'_>) -> Vec<BareJid> {
    let MentionAudienceRequest {
        state,
        deps,
        room,
        room_actor,
        sender,
        message,
        config,
        durable_recipients,
        is_correction,
    } = request;
    if config.requires_membership() || is_correction {
        return Vec::new();
    }
    let mentions = extract_explicit_mentions(message)
        .map(|mentions| mentions.mentions)
        .unwrap_or_default();
    let references = extract_references_from_message(message);
    if mentions_exceed_threshold_from_parts(&mentions, &references, DEFAULT_MENTIONS_COUNT) {
        return Vec::new();
    }
    let sender_bare = sender.to_bare();
    let candidates: Vec<BareJid> = personal_mention_targets(&mentions, &references)
        .into_iter()
        .filter(|target| target != &sender_bare && !durable_recipients.contains(target))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    let entry = match get_managed_channel_for_room(state, room).await {
        Ok(Some(channel)) => EntryAuthority::Managed(channel),
        Ok(None) => EntryAuthority::Unmanaged,
        Err(error) => {
            warn!(
                room = %room,
                %error,
                "Mention audience: managed channel lookup failed; excluding mentioned non-members"
            );
            EntryAuthority::Unavailable
        }
    };
    let sender_jid = Jid::from(sender.clone());
    let target_checks = TargetChecks {
        state,
        deps,
        room,
        room_actor,
        sender: &sender_jid,
        members_only: config.members_only,
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

struct TargetChecks<'a> {
    state: &'a WebSocketState,
    deps: &'a Deps<'a>,
    room: &'a BareJid,
    room_actor: &'a ActorRef<RoomActor>,
    sender: &'a Jid,
    members_only: bool,
    entry: &'a EntryAuthority,
}

impl TargetChecks<'_> {
    async fn admits(&self, target: &BareJid) -> bool {
        match self.is_local_account(target).await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(()) => return fail_closed(MentionAudienceCheck::Account),
        }
        match self.has_blocked_sender(target).await {
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
            self.state.deps.app_state.db_pool.global_actor(),
            target,
            self.deps.local_domain,
        )
        .await
        .map_err(|error| {
            warn!(%error, "Mention audience: account lookup failed");
        })
    }

    /// XEP-0191: a user who blocked the sender must not receive the
    /// sender's mention as an inbox row or notification.
    async fn has_blocked_sender(&self, target: &BareJid) -> Result<bool, ()> {
        let Some(storage) = self.deps.blocking_storage else {
            warn!("Mention audience: no blocking storage wired");
            return Err(());
        };
        let entries = storage
            .list_blocked_jid_entries(target)
            .await
            .map_err(|error| {
                warn!(%error, "Mention audience: blocklist read failed");
            })?;
        Ok(Blocklist::new(entries).contains_jid(self.sender))
    }

    /// XEP-0045 right to enter: the open-room rule (any non-outcast),
    /// including permission-graph bans on managed channels.
    async fn may_enter(&self, target: &BareJid) -> Result<bool, ()> {
        match self.entry {
            EntryAuthority::Managed(channel) => {
                match resolve_managed_channel_entry_access(
                    self.state,
                    self.room,
                    target,
                    channel,
                    self.members_only,
                )
                .await
                {
                    RoomArchiveAccess::Allowed => Ok(true),
                    RoomArchiveAccess::Denied => Ok(false),
                    RoomArchiveAccess::Error => Err(()),
                }
            }
            EntryAuthority::Unmanaged => self
                .room_actor
                .ask(GetAffiliation {
                    jid: target.clone(),
                })
                .await
                .map(|affiliation| affiliation != waddle_xmpp::Affiliation::Outcast)
                .map_err(|error| {
                    warn!(room = %self.room, error = ?error, "Mention audience: affiliation read failed");
                }),
            EntryAuthority::Unavailable => Err(()),
        }
    }
}

fn fail_closed(check: MentionAudienceCheck) -> bool {
    record_mention_audience_check_failed(check);
    false
}
