//! XEP-0472 community feed verb exports: fetch the latest entries
//! from the `urn:xmpp:pubsub-social-feed:1` node on the community
//! service (`community.<domain>`, resolved from the session domain)
//! and publish new posts to it.
//!
//! Mirrors the wasm chat client's `feed_items` / `feed_publish`
//! surface: the shared feed verbs live in
//! `waddle_xmpp_client::social_feed`; this file adapts the typed
//! uniffi Records (timestamps as unix millis, inbox precedent) and
//! mints the `post-<uuid>` item ids.

use jid::BareJid;
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::pubsub::{community_service_jid, parse_pubsub_publish_result_item_id};
use waddle_xmpp_client::social_feed::{
    build_feed_items_iq, build_feed_publish_iq, parse_feed_items_result, FeedItem, FeedSourceKind,
};
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::xep0472::FeedEntry;

use crate::boundary_convert::jid_domain;
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError, WaddleFeedEntry, WaddleFeedSourceKind};

// ── core → FFI mappings ─────────────────────────────────────────────────────

fn source_kind_to_ffi(kind: FeedSourceKind) -> WaddleFeedSourceKind {
    match kind {
        FeedSourceKind::Mood => WaddleFeedSourceKind::Mood,
        FeedSourceKind::Activity => WaddleFeedSourceKind::Activity,
        FeedSourceKind::Tune => WaddleFeedSourceKind::Tune,
        FeedSourceKind::Avatar => WaddleFeedSourceKind::Avatar,
        FeedSourceKind::Vcard => WaddleFeedSourceKind::Vcard,
        FeedSourceKind::Rsvp => WaddleFeedSourceKind::Rsvp,
    }
}

pub(crate) fn feed_item_to_ffi(item: FeedItem) -> WaddleFeedEntry {
    let FeedItem { entry, source } = item;
    WaddleFeedEntry {
        id: entry.id,
        title: entry.title,
        body: entry.body,
        author: entry.author,
        published_epoch_ms: entry.published.map(|ts| ts.timestamp_millis()),
        link: entry.link,
        source: source.map(source_kind_to_ffi),
    }
}

// ── FFI → wire stanza factories ─────────────────────────────────────────────
//
// Pure functions between the FFI argument shapes and the shared
// builders; the dedicated test suite pins their output against the
// XEP wire shapes without a live connection.

/// XEP-0060 items GET over the community feed node; the IQ id carries
/// the conventional `feed-items-` prefix (minted in the shared verb).
pub(crate) fn feed_items_stanza(service: &BareJid, max_items: Option<u32>) -> Element {
    build_feed_items_iq(service, max_items)
}

/// XEP-0060 publish SET for one Atom `<entry/>` under the minted
/// `post-<uuid>` item id; the IQ id carries the conventional
/// `feed-publish-` prefix (minted in the shared verb).
pub(crate) fn feed_publish_stanza(service: &BareJid, item_id: &str, entry: &FeedEntry) -> Element {
    build_feed_publish_iq(service, item_id, entry)
}

/// Map an items reply into typed FFI entries. Unparsable items are
/// skipped by the tolerant shared parse; stanza errors surface typed.
pub(crate) fn map_fetch_feed_reply(
    reply: Result<Element, ClientError>,
) -> Result<Vec<WaddleFeedEntry>, WaddleError> {
    let element = reply.map_err(|e| client_error_to_waddle(&e))?;
    Ok(parse_feed_items_result(&element)
        .into_iter()
        .map(feed_item_to_ffi)
        .collect())
}

/// Map a publish reply to the published item id: the server's §7.1.2
/// item-id echo when present, else the minted id the request carried
/// (a service may legally omit the echo for publisher-supplied ids).
pub(crate) fn map_publish_feed_reply(
    reply: Result<Element, ClientError>,
    minted_item_id: String,
) -> Result<String, WaddleError> {
    let element = reply.map_err(|e| client_error_to_waddle(&e))?;
    Ok(parse_pubsub_publish_result_item_id(&element).unwrap_or(minted_item_id))
}

fn community_service_from_session(jid: &str) -> Result<BareJid, WaddleError> {
    community_service_jid(jid_domain(jid)).map_err(|_| WaddleError::InvalidJid)
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0472: fetch the latest entries from the community Social
    /// Feed node on `community.<domain>` (resolved from the session
    /// JID), ordered as the server delivered them (newest first).
    pub async fn fetch_feed(
        &self,
        max_items: Option<u32>,
    ) -> Result<Vec<WaddleFeedEntry>, WaddleError> {
        let service = community_service_from_session(&self.config.jid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = feed_items_stanza(&service, max_items);
        map_fetch_feed_reply(send_iq_with_timeout(&handle, iq).await)
    }

    /// XEP-0472: publish a new post to the community Social Feed under
    /// a minted `post-<uuid>` item id, returning the published item
    /// id. The author travels as the session bare JID (the server
    /// stamps the authenticated publisher regardless); publish
    /// authorisation is enforced server-side via XEP-0060 affiliations
    /// — a Forbidden stanza error surfaces typed.
    pub async fn publish_feed_post(
        &self,
        body: String,
        title: Option<String>,
    ) -> Result<String, WaddleError> {
        if body.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let service = community_service_from_session(&self.config.jid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let item_id = format!("post-{}", Uuid::new_v4());
        let mut entry = FeedEntry::new(item_id.clone(), body)
            .with_author(self.config.jid.clone())
            .with_published_now();
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            entry = entry.with_title(title);
        }
        let iq = feed_publish_stanza(&service, &item_id, &entry);
        map_publish_feed_reply(send_iq_with_timeout(&handle, iq).await, item_id)
    }
}
