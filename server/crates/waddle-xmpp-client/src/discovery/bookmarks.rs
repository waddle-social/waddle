//! XEP-0402 bookmark merge for topology discovery.
//!
//! Port of the web client's bookmark handling in
//! `chat/src/lib/xmpp/discovery.ts` (`discoverTopology`): the user's
//! own-PEP `urn:xmpp:bookmarks:1` items stamp `autojoin` and the
//! bookmark name onto catalog rooms, and rooms that exist *only* as a
//! bookmark are appended — but kept only when their disco#info
//! confirms they are an actual MUC room. A failed bookmarks fetch
//! degrades to the catalog-only result (web parity: the fetch sits in
//! a bare `try/catch`).

use jid::BareJid;

use crate::error::{ClientError, STANZA_CONDITION_ITEM_NOT_FOUND};
use crate::push::CommandDriver;
use crate::xep::xep0402::{build_fetch_bookmarks_iq, parse_bookmarks_response, BookmarkItem};

use super::ids::next_id;
use super::iq::build_disco_info_iq;
use super::parsing::parse_disco_info_result;
use super::types::{DiscoveredChannel, DiscoveredChannelType, SpaceNode};
use super::{MUC_NS, STANDALONE_SPACE_ID, WADDLE_ROOM_METADATA_FORM_TYPE};

/// Fetch the user's own-PEP XEP-0402 bookmarks. An account that never
/// published one has no node, which is an empty list. Any other
/// failure yields `None`: `discover_topology` then degrades to the
/// catalog-only result, like the web client's bare `try/catch` around
/// the pubsub fetch, and reports that the bookmarks were not read.
pub(super) async fn fetch_user_bookmarks<D: CommandDriver>(
    driver: &D,
) -> Option<Vec<BookmarkItem>> {
    let fetch_iq = build_fetch_bookmarks_iq(&format!("bookmarks-{}", next_id()));
    match driver.send_iq(fetch_iq).await {
        Ok(elem) => Some(parse_bookmarks_response(&elem)),
        Err(ClientError::StanzaError(error))
            if error.condition == STANZA_CONDITION_ITEM_NOT_FOUND =>
        {
            Some(Vec::new())
        }
        Err(_) => None,
    }
}

/// Merge the user's XEP-0402 bookmarks into the catalog `channels`.
///
/// - A bookmark for a room already in the catalog stamps
///   `autojoin`/`bookmark_name` onto that channel (bookmark wins over
///   the catalog default).
/// - A bookmark-only room is disco#info'd via `driver`; it is kept
///   only when the info advertises the XEP-0045 MUC feature (web
///   parity: a bookmark-only entry whose hydration fails or does not
///   confirm MUC is dropped). Kept rooms land in the synthetic
///   standalone space with `is_group_dm` stamped from the
///   `urn:waddle:group-dm:0` feature.
///
/// Returns how many bookmark-only channels were appended so the
/// caller can decide whether the standalone space must exist.
pub(super) async fn merge_bookmarks_into_channels<D: CommandDriver>(
    driver: &D,
    channels: &mut Vec<DiscoveredChannel>,
    bookmarks: Vec<BookmarkItem>,
    standalone_space_id: &SpaceNode,
) -> usize {
    let mut bookmark_only: Vec<BookmarkItem> = Vec::new();
    for bookmark in bookmarks {
        if let Some(channel) = channels
            .iter_mut()
            .find(|channel| channel.room_jid == bookmark.jid)
        {
            stamp_bookmark(channel, &bookmark);
            continue;
        }
        if let Some(existing) = bookmark_only
            .iter_mut()
            .find(|existing| existing.jid == bookmark.jid)
        {
            *existing = bookmark;
        } else {
            bookmark_only.push(bookmark);
        }
    }

    if bookmark_only.is_empty() {
        return 0;
    }

    let hydrated = hydrate_bookmark_only_rooms(driver, bookmark_only).await;
    let mut appended = 0usize;
    for (bookmark, info) in hydrated {
        let Some(info) = info else {
            continue;
        };
        if let Some(channel) =
            bookmark_only_channel(&bookmark, &info, standalone_space_id, channels.len() as i32)
        {
            channels.push(channel);
            appended += 1;
        }
    }
    appended
}

#[cfg(any(feature = "native", feature = "wasm"))]
async fn hydrate_bookmark_only_rooms<D: CommandDriver>(
    driver: &D,
    bookmarks: Vec<BookmarkItem>,
) -> Vec<(BookmarkItem, Option<super::types::DiscoInfoResult>)> {
    use futures::stream::{self, StreamExt};

    const BOOKMARK_DISCO_CONCURRENCY: usize = 8;

    stream::iter(bookmarks)
        .map(|bookmark| async move {
            let info = discover_room_info(driver, &bookmark.jid).await;
            (bookmark, info)
        })
        .buffered(BOOKMARK_DISCO_CONCURRENCY)
        .collect()
        .await
}

#[cfg(not(any(feature = "native", feature = "wasm")))]
async fn hydrate_bookmark_only_rooms<D: CommandDriver>(
    driver: &D,
    bookmarks: Vec<BookmarkItem>,
) -> Vec<(BookmarkItem, Option<super::types::DiscoInfoResult>)> {
    let mut hydrated = Vec::with_capacity(bookmarks.len());
    for bookmark in bookmarks {
        let info = discover_room_info(driver, &bookmark.jid).await;
        hydrated.push((bookmark, info));
    }
    hydrated
}

/// Stamp a bookmark's autojoin/name onto a catalog channel.
pub(super) fn stamp_bookmark(channel: &mut DiscoveredChannel, bookmark: &BookmarkItem) {
    channel.autojoin = bookmark.autojoin;
    channel.user_autojoin = Some(bookmark.autojoin);
    channel.bookmark_name = bookmark.name.clone();
}

/// Build the channel for a room that exists only as a bookmark.
/// Returns `None` unless the room's disco#info advertises the
/// XEP-0045 MUC feature — a stale bookmark for a destroyed or
/// non-MUC entity must not surface a phantom channel (web parity).
pub(super) fn bookmark_only_channel(
    bookmark: &BookmarkItem,
    info: &super::types::DiscoInfoResult,
    standalone_space_id: &SpaceNode,
    position: i32,
) -> Option<DiscoveredChannel> {
    if !info.has_feature(MUC_NS) {
        return None;
    }
    let channel_type = info
        .form_value(WADDLE_ROOM_METADATA_FORM_TYPE, "waddle#channel_type")
        .and_then(DiscoveredChannelType::from_metadata)
        .unwrap_or(DiscoveredChannelType::Text);
    let description = info
        .form_value(
            "http://jabber.org/protocol/muc#roominfo",
            "muc#roominfo_description",
        )
        .filter(|description| !description.trim().is_empty())
        .map(str::to_string);
    let name = bookmark
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            bookmark
                .jid
                .node()
                .map(|node| node.as_str().to_string())
                .unwrap_or_else(|| bookmark.jid.to_string())
        });
    let (is_group_dm, members_only) = info.room_flags();
    Some(DiscoveredChannel {
        id: format!("{}::{}", STANDALONE_SPACE_ID, bookmark.jid),
        room_jid: bookmark.jid.clone(),
        name,
        description,
        channel_type,
        position,
        space_id: standalone_space_id.clone(),
        autojoin: bookmark.autojoin,
        user_autojoin: Some(bookmark.autojoin),
        bookmark_name: bookmark.name.clone(),
        is_group_dm,
        members_only,
    })
}

async fn discover_room_info<D: CommandDriver>(
    driver: &D,
    room_jid: &BareJid,
) -> Option<super::types::DiscoInfoResult> {
    let jid_str = room_jid.to_string();
    let iq = build_disco_info_iq(&jid_str, None);
    let result = driver.send_iq(iq).await.ok()?;
    parse_disco_info_result(&result, &jid_str)
}
