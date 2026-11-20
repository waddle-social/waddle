//! Community feed verbs (XEP-0472): read the feed on the community
//! service's `urn:xmpp:pubsub-social-feed:1` node and post to it.
//!
//! Both verbs parse their untyped FFI inputs into typed values first,
//! then delegate to the shared XEP-0060 builders and the XEP-0472 Atom
//! payload in `waddle-xmpp-core`.

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result,
};
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::xep0472::{
    build_feed_entry_element, parse_feed_entry, FeedEntry, NS_ATOM, PUBSUB_NODE_FEED,
};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

/// Namespace of the `<source kind='…'/>` child the server adds to
/// entries it writes on a member's behalf.
const NS_FEED_SOURCE: &str = "urn:waddle:feed-source:0";

/// What a server-written feed entry reports. Entries a member posted
/// carry no source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WaddleFeedSource {
    Mood,
    Activity,
    Tune,
    Avatar,
    Profile,
    EventReply,
    /// A kind this client does not know yet.
    Other,
}

impl WaddleFeedSource {
    fn from_kind(kind: &str) -> Self {
        match kind {
            "mood" => Self::Mood,
            "activity" => Self::Activity,
            "tune" => Self::Tune,
            "avatar" => Self::Avatar,
            "vcard" => Self::Profile,
            "rsvp" => Self::EventReply,
            _ => Self::Other,
        }
    }
}

/// One entry of the community feed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WaddleFeedEntry {
    /// Pubsub item id.
    pub id: String,
    pub title: Option<String>,
    pub body: String,
    /// Bare JID of the author, as stamped by the server.
    pub author: Option<String>,
    /// Publication time, milliseconds since the Unix epoch.
    pub published_ms: Option<i64>,
    pub link: Option<String>,
    pub source: Option<WaddleFeedSource>,
}

/// A new feed post. The server stamps the author.
#[derive(Clone, Debug, uniffi::Record)]
pub struct WaddleFeedPost {
    pub title: Option<String>,
    pub body: String,
    pub link: Option<String>,
}

fn feed_entry_to_ffi(entry: FeedEntry, source: Option<WaddleFeedSource>) -> WaddleFeedEntry {
    WaddleFeedEntry {
        id: entry.id,
        title: entry.title,
        body: entry.body,
        author: entry.author,
        published_ms: entry.published.map(|at| at.timestamp_millis()),
        link: entry.link,
        source,
    }
}

fn feed_source(entry: &Element) -> Option<WaddleFeedSource> {
    entry
        .get_child("source", NS_FEED_SOURCE)
        .and_then(|source| source.attr("kind"))
        .map(WaddleFeedSource::from_kind)
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

// ── FFI → wire stanza factories ─────────────────────────────────────────────

/// XEP-0060 items GET over the community feed node.
pub(crate) fn feed_fetch_stanza(community: &BareJid, max_items: Option<u32>) -> Element {
    build_pubsub_items_iq(
        &Uuid::new_v4().to_string(),
        Some(community),
        PUBSUB_NODE_FEED,
        max_items,
    )
}

/// Turn a post into the entry to publish: a fresh item id, `author` as
/// the author and `now` as the publication time. A post without text
/// is refused.
pub(crate) fn feed_entry_from_post(
    post: WaddleFeedPost,
    author: &BareJid,
    now: DateTime<Utc>,
) -> Result<FeedEntry, WaddleError> {
    if post.body.trim().is_empty() {
        return Err(WaddleError::InvalidArgument);
    }
    let mut entry = FeedEntry::new(format!("post-{}", Uuid::new_v4()), post.body)
        .with_author(author.as_str())
        .with_published(now);
    entry.title = non_blank(post.title);
    entry.link = non_blank(post.link);
    Ok(entry)
}

/// XEP-0060 publish SET carrying `entry` as an Atom `<entry/>`.
pub(crate) fn feed_publish_stanza(community: &BareJid, entry: &FeedEntry) -> Element {
    build_pubsub_publish_iq(
        &Uuid::new_v4().to_string(),
        Some(community),
        PUBSUB_NODE_FEED,
        Some(&entry.id),
        build_feed_entry_element(entry),
        None,
    )
}

/// Map a feed fetch reply to typed entries, in the order the server
/// sent them. Items that are not valid feed entries are skipped.
pub(crate) fn map_feed_reply(
    reply: Result<Element, ClientError>,
) -> Result<Vec<WaddleFeedEntry>, WaddleError> {
    let result = reply.map_err(|e| client_error_to_waddle(&e))?;
    Ok(parse_pubsub_items_result(&result)
        .into_iter()
        .filter_map(|item| {
            let atom = item.payload("entry", NS_ATOM)?;
            let entry = parse_feed_entry(&item.id, atom)?;
            Some(feed_entry_to_ffi(entry, feed_source(atom)))
        })
        .collect())
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0472: fetch the newest entries of the community feed on
    /// `community_jid` (the `community.<domain>` service), newest
    /// first. `max_items: None` leaves the page size to the server.
    pub async fn fetch_feed(
        &self,
        community_jid: String,
        max_items: Option<u32>,
    ) -> Result<Vec<WaddleFeedEntry>, WaddleError> {
        let community = community_jid
            .parse::<BareJid>()
            .map_err(|_| WaddleError::InvalidJid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = feed_fetch_stanza(&community, max_items);
        map_feed_reply(send_iq_with_timeout(&handle, iq).await)
    }

    /// XEP-0472: post to the community feed and return the entry as
    /// published, so the caller can show it without fetching again. A
    /// member the server does not let post gets the `forbidden` stanza
    /// error.
    pub async fn publish_feed_post(
        &self,
        community_jid: String,
        post: WaddleFeedPost,
    ) -> Result<WaddleFeedEntry, WaddleError> {
        let community = community_jid
            .parse::<BareJid>()
            .map_err(|_| WaddleError::InvalidJid)?;
        let entry = feed_entry_from_post(post, &self.account_bare_jid()?, Utc::now())?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = feed_publish_stanza(&community, &entry);
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(feed_entry_to_ffi(entry, None))
    }
}
