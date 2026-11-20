//! Stories verbs (XEP-0501): read and post stories on the community
//! service's `urn:xmpp:pubsub-social-feed:stories:0` node, keep the
//! account's private "seen" state on its own PEP node, and react to a
//! story through XEP-0470 Pubsub Attachments.
//!
//! Every verb parses its untyped FFI inputs into typed values first,
//! then delegates to the shared builders in `waddle-xmpp-client` and
//! the typed payloads in `waddle-xmpp-core`.

use chrono::{DateTime, Duration};
use jid::{BareJid, Jid};
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::error::STANZA_CONDITION_ITEM_NOT_FOUND;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result,
};
use waddle_xmpp_client::story_reactions::{
    build_own_story_attachment_iq, build_story_attachment_publish_iq,
    build_story_attachment_retract_iq, build_story_reaction_summary_iq, parse_own_story_attachment,
    parse_story_reaction_summary,
};
use waddle_xmpp_client::story_reads::{
    build_story_reads_fetch_iq, build_story_reads_publish_iq, parse_story_reads_fetch_result,
};
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::waddle_story_reads::{StoryId, StoryReads};
use waddle_xmpp_core::xep0470::{Attachment, AttachmentPayload, ReactionSet};
use waddle_xmpp_core::xep0501::{
    build_story_element, parse_story, Story, StoryMediaType, DEFAULT_EXPIRY_HOURS, NS_ATOM,
    PUBSUB_NODE_STORIES,
};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

/// One story. The caller decides whether it is still active by
/// comparing `expires_ms` with the clock.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WaddleStory {
    /// Pubsub item id.
    pub id: String,
    pub body: Option<String>,
    pub media_url: String,
    pub media_type: Option<String>,
    /// Bare JID of the author, as stamped by the server.
    pub author: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub posted_ms: Option<i64>,
    /// Milliseconds since the Unix epoch; `None` never expires.
    pub expires_ms: Option<i64>,
}

/// A new story. `expiry_hours: None` is a day; less than an hour is an
/// hour.
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddleStoryPost {
    pub body: Option<String>,
    pub media_url: String,
    pub media_type: Option<String>,
    pub expiry_hours: Option<u32>,
}

/// One story the account has seen.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WaddleStoryRead {
    pub story_id: String,
    /// Milliseconds since the Unix epoch.
    pub read_at_ms: i64,
}

/// How many members reacted to a story with one emoji.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WaddleStoryReaction {
    pub emoji: String,
    pub count: u32,
}

/// The reactions on one story.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WaddleStoryReactions {
    pub reactions: Vec<WaddleStoryReaction>,
    /// The emoji this account reacted with.
    pub mine: Vec<String>,
}

fn story_to_ffi(story: Story) -> Option<WaddleStory> {
    Some(WaddleStory {
        id: story.id,
        body: story.body,
        media_url: story.media_url?,
        media_type: story.media_type.map(|media_type| media_type.to_string()),
        author: story.author,
        posted_ms: story.posted.map(|at| at.timestamp_millis()),
        expires_ms: story.expires.map(|at| at.timestamp_millis()),
    })
}

fn parse_community(community_jid: &str) -> Result<BareJid, WaddleError> {
    community_jid.parse().map_err(|_| WaddleError::InvalidJid)
}

fn require_story_id(story_id: &str) -> Result<(), WaddleError> {
    if story_id.trim().is_empty() {
        return Err(WaddleError::InvalidArgument);
    }
    Ok(())
}

fn is_item_not_found(error: &ClientError) -> bool {
    matches!(error, ClientError::StanzaError(err) if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND)
}

/// A reply where `item-not-found` (nothing was ever published there)
/// is the normal empty state rather than a failure.
fn reply_or_absent(reply: Result<Element, ClientError>) -> Result<Option<Element>, WaddleError> {
    match reply {
        Ok(result) => Ok(Some(result)),
        Err(e) if is_item_not_found(&e) => Ok(None),
        Err(e) => Err(client_error_to_waddle(&e)),
    }
}

fn fresh_iq_id() -> String {
    Uuid::new_v4().to_string()
}

// ── Stories ──────────────────────────────────────────────────────────────────

/// XEP-0060 items GET over the community stories node.
pub(crate) fn stories_fetch_stanza(community: &BareJid, max_items: Option<u32>) -> Element {
    build_pubsub_items_iq(
        &fresh_iq_id(),
        Some(community),
        PUBSUB_NODE_STORIES,
        max_items,
    )
}

/// Turn a post into the story to publish: a fresh item id, `author` as
/// the author, posted now. XEP-0501 requires the media, so a post
/// without it is refused, as is a malformed media type.
pub(crate) fn story_from_post(
    post: WaddleStoryPost,
    author: &BareJid,
) -> Result<Story, WaddleError> {
    let media_url = post.media_url.trim();
    if media_url.is_empty() {
        return Err(WaddleError::InvalidArgument);
    }
    let hours = post
        .expiry_hours
        .map_or(DEFAULT_EXPIRY_HOURS, i64::from)
        .max(1);
    let mut story = Story::new(format!("story-{}", Uuid::new_v4()))
        .with_media(media_url)
        .with_author(author.as_str());
    if let Some(posted) = story.posted {
        story = story.with_expires_at(posted + Duration::hours(hours));
    }
    if let Some(body) = post.body.filter(|body| !body.trim().is_empty()) {
        story = story.with_body(body);
    }
    if let Some(media_type) = post.media_type {
        story = story.with_media_type(
            StoryMediaType::parse(&media_type).ok_or(WaddleError::InvalidArgument)?,
        );
    }
    Ok(story)
}

/// XEP-0060 publish SET carrying `story` as an Atom `<entry/>`.
pub(crate) fn story_publish_stanza(community: &BareJid, story: &Story) -> Element {
    build_pubsub_publish_iq(
        &fresh_iq_id(),
        Some(community),
        PUBSUB_NODE_STORIES,
        Some(&story.id),
        build_story_element(story),
        None,
    )
}

/// Map a stories fetch reply to typed stories, expired ones included.
/// Items that are not valid stories are skipped.
pub(crate) fn map_stories_reply(
    reply: Result<Element, ClientError>,
) -> Result<Vec<WaddleStory>, WaddleError> {
    let result = reply.map_err(|e| client_error_to_waddle(&e))?;
    Ok(parse_pubsub_items_result(&result)
        .into_iter()
        .filter_map(|item| {
            let atom = item.payload("entry", NS_ATOM)?;
            story_to_ffi(parse_story(&item.id, atom)?)
        })
        .collect())
}

// ── Read state ───────────────────────────────────────────────────────────────

/// Parse FFI read entries into the typed read state.
pub(crate) fn story_reads_from_ffi(reads: Vec<WaddleStoryRead>) -> Result<StoryReads, WaddleError> {
    let mut typed = StoryReads::default();
    for read in reads {
        let id = StoryId::try_new(read.story_id).map_err(|_| WaddleError::InvalidArgument)?;
        let at =
            DateTime::from_timestamp_millis(read.read_at_ms).ok_or(WaddleError::InvalidArgument)?;
        typed.mark_read(id, at);
    }
    Ok(typed)
}

/// Map a read-state fetch reply. The reply must come from the account
/// itself (XEP-0223 §Security Considerations): a `from` naming anyone
/// else is refused. `item-not-found` is the state before the first
/// story was read. Any other failure stays an error, so a caller that
/// republishes the whole state never mistakes a failed fetch for
/// "nothing read" and overwrites what another device stored.
pub(crate) fn map_story_reads_reply(
    reply: Result<Element, ClientError>,
    own: &BareJid,
) -> Result<Vec<WaddleStoryRead>, WaddleError> {
    let Some(result) = reply_or_absent(reply)? else {
        return Ok(Vec::new());
    };
    if let Some(from) = result.attr("from") {
        if from.parse::<Jid>().ok().as_ref() != Some(&Jid::from(own.clone())) {
            return Err(WaddleError::UntrustedReply);
        }
    }
    Ok(parse_story_reads_fetch_result(&result)
        .iter()
        .map(|(id, at)| WaddleStoryRead {
            story_id: id.as_str().to_owned(),
            read_at_ms: at.timestamp_millis(),
        })
        .collect())
}

// ── Reactions ────────────────────────────────────────────────────────────────

fn own_attachment(
    reply: Result<Element, ClientError>,
    own: &BareJid,
) -> Result<Option<Attachment>, WaddleError> {
    Ok(reply_or_absent(reply)?.and_then(|result| parse_own_story_attachment(&result, own)))
}

/// Combine the summary reply (counts per emoji) and the account's own
/// attachment reply (which emoji are its own). `item-not-found` on
/// either is a story nobody, or this account, has reacted to.
pub(crate) fn map_story_reactions_replies(
    summary_reply: Result<Element, ClientError>,
    own_reply: Result<Element, ClientError>,
    story_id: &str,
    own: &BareJid,
) -> Result<WaddleStoryReactions, WaddleError> {
    let reactions = reply_or_absent(summary_reply)?
        .and_then(|result| parse_story_reaction_summary(&result, story_id))
        .map(|summary| summary.reactions)
        .unwrap_or_default()
        .into_iter()
        .map(|reaction| WaddleStoryReaction {
            emoji: reaction.emoji,
            count: reaction.count,
        })
        .collect();
    let mine = own_attachment(own_reply, own)?
        .and_then(|attachment| attachment.reactions_set().map(|set| set.emojis.clone()))
        .unwrap_or_default();
    Ok(WaddleStoryReactions { reactions, mine })
}

fn reaction_set(emojis: Vec<String>) -> Result<ReactionSet, WaddleError> {
    let set = ReactionSet::new(emojis);
    set.validate().map_err(|_| WaddleError::InvalidArgument)?;
    Ok(set)
}

/// The attachment that should replace `current` once the account's
/// reactions are `emojis`: the reactions are swapped, everything else
/// the account attached (including children this client does not
/// understand) is kept. `None` means nothing is left to publish.
pub(crate) fn next_story_attachment(
    current: Option<Attachment>,
    emojis: Vec<String>,
) -> Result<Option<Attachment>, WaddleError> {
    let reactions = reaction_set(emojis)?;
    let mut next = current.unwrap_or_else(Attachment::empty);
    next.payloads
        .retain(|payload| !matches!(payload, AttachmentPayload::Reactions(_)));
    if !reactions.is_empty() {
        next.payloads
            .insert(0, AttachmentPayload::Reactions(reactions));
    }
    Ok((!next.payloads.is_empty() || !next.unknown_children.is_empty()).then_some(next))
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0501: fetch the stories on `community_jid` (the
    /// `community.<domain>` service). Expired stories are included.
    pub async fn fetch_stories(
        &self,
        community_jid: String,
        max_items: Option<u32>,
    ) -> Result<Vec<WaddleStory>, WaddleError> {
        let community = parse_community(&community_jid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = stories_fetch_stanza(&community, max_items);
        map_stories_reply(send_iq_with_timeout(&handle, iq).await)
    }

    /// XEP-0501: post a story and return it as published, so the
    /// caller can show it without fetching again.
    pub async fn publish_story(
        &self,
        community_jid: String,
        post: WaddleStoryPost,
    ) -> Result<WaddleStory, WaddleError> {
        let community = parse_community(&community_jid)?;
        let story = story_from_post(post, &self.account_bare_jid()?)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = story_publish_stanza(&community, &story);
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        story_to_ffi(story).ok_or(WaddleError::InvalidArgument)
    }

    /// Fetch which stories this account has seen, from its private
    /// read-state item. Empty before the first story was read.
    pub async fn fetch_story_reads(&self) -> Result<Vec<WaddleStoryRead>, WaddleError> {
        let own = self.account_bare_jid()?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_story_reads_fetch_iq(&fresh_iq_id());
        map_story_reads_reply(send_iq_with_timeout(&handle, iq).await, &own)
    }

    /// Replace this account's read state with `reads`. The item holds
    /// the whole state, so pass every story still worth remembering.
    pub async fn publish_story_reads(
        &self,
        reads: Vec<WaddleStoryRead>,
    ) -> Result<(), WaddleError> {
        let reads = story_reads_from_ffi(reads)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_story_reads_publish_iq(&fresh_iq_id(), &reads);
        send_iq_with_timeout(&handle, iq)
            .await
            .map(|_| ())
            .map_err(|e| client_error_to_waddle(&e))
    }

    /// XEP-0470: fetch the reaction counts on one story and which
    /// emoji this account reacted with.
    pub async fn fetch_story_reactions(
        &self,
        community_jid: String,
        story_id: String,
    ) -> Result<WaddleStoryReactions, WaddleError> {
        let community = parse_community(&community_jid)?;
        require_story_id(&story_id)?;
        let own = self.account_bare_jid()?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let summary_iq = build_story_reaction_summary_iq(&fresh_iq_id(), &community, &story_id);
        let own_iq = build_own_story_attachment_iq(&fresh_iq_id(), &community, &story_id, &own);
        let (summary_reply, own_reply) = tokio::join!(
            send_iq_with_timeout(&handle, summary_iq),
            send_iq_with_timeout(&handle, own_iq),
        );
        map_story_reactions_replies(summary_reply, own_reply, &story_id, &own)
    }

    /// XEP-0470: make `emojis` this account's reactions to one story,
    /// replacing the ones it had; an empty list removes them. At most
    /// 12 reactions, each a single emoji.
    pub async fn set_story_reactions(
        &self,
        community_jid: String,
        story_id: String,
        emojis: Vec<String>,
    ) -> Result<(), WaddleError> {
        let community = parse_community(&community_jid)?;
        require_story_id(&story_id)?;
        reaction_set(emojis.clone())?;
        let own = self.account_bare_jid()?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let fetch = build_own_story_attachment_iq(&fresh_iq_id(), &community, &story_id, &own);
        let current = own_attachment(send_iq_with_timeout(&handle, fetch).await, &own)?;
        let had_attachment = current.is_some();
        let Some(next) = next_story_attachment(current, emojis)? else {
            if !had_attachment {
                return Ok(());
            }
            // Already gone (a retry after a lost reply) is success.
            let retract =
                build_story_attachment_retract_iq(&fresh_iq_id(), &community, &story_id, &own);
            return reply_or_absent(send_iq_with_timeout(&handle, retract).await).map(|_| ());
        };
        // `item-not-found` here means the story itself is gone.
        let publish =
            build_story_attachment_publish_iq(&fresh_iq_id(), &community, &story_id, &own, &next);
        send_iq_with_timeout(&handle, publish)
            .await
            .map(|_| ())
            .map_err(|e| client_error_to_waddle(&e))
    }
}
