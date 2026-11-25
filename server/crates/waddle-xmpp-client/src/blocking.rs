//! XEP-0191 Blocking Command requests, results, and pushes.

use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::blocking::{Block, BlocklistRequest, Unblock};
use xmpp_parsers::iq::Iq;

pub use crate::messaging::namespaces::NS_BLOCKING;

/// A trusted XEP-0191 push describing a change to the account's block list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockingPush {
    /// The listed JIDs were blocked.
    Blocked(Vec<Jid>),
    /// The listed JIDs were unblocked.
    Unblocked(Vec<Jid>),
    /// Every JID was unblocked.
    UnblockedAll,
}

/// A block or targeted unblock command was given no JIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a block or targeted unblock command requires at least one JID")]
pub struct EmptyBlockingJids;

/// Build an IQ-get for the account's block list.
///
/// Request the list after authentication and before changing it. The server
/// sends block-list pushes only to resources that requested it in this session.
#[must_use]
pub fn build_block_list_iq(iq_id: &str) -> Iq {
    Iq::from_get(iq_id, BlocklistRequest)
}

/// Build an IQ-set that blocks the supplied JIDs.
///
/// # Errors
///
/// Returns [`EmptyBlockingJids`] when no JIDs are supplied.
pub fn build_block_iq(iq_id: &str, jids: Vec<Jid>) -> Result<Iq, EmptyBlockingJids> {
    if jids.is_empty() {
        return Err(EmptyBlockingJids);
    }
    Ok(Iq::from_set(iq_id, Block { items: jids }))
}

/// Build an IQ-set that unblocks the supplied JIDs.
///
/// # Errors
///
/// Returns [`EmptyBlockingJids`] when no JIDs are supplied.
pub fn build_unblock_iq(iq_id: &str, jids: Vec<Jid>) -> Result<Iq, EmptyBlockingJids> {
    if jids.is_empty() {
        return Err(EmptyBlockingJids);
    }
    Ok(Iq::from_set(iq_id, Unblock { items: jids }))
}

/// Build an IQ-set explicitly unblocking every JID.
#[must_use]
pub fn build_unblock_all_iq(iq_id: &str) -> Iq {
    Iq::from_set(iq_id, Unblock { items: Vec::new() })
}

/// Parse a block list result, skipping malformed items.
///
/// Returns `None` when the IQ does not carry a block list result.
#[must_use]
pub fn parse_block_list_result(iq: &Iq) -> Option<Vec<Jid>> {
    match iq {
        Iq::Result {
            payload: Some(payload),
            ..
        } if payload.is("blocklist", NS_BLOCKING) => Some(parse_items(payload)),
        _ => None,
    }
}

/// Parse a block-list push sent by the account's bare JID or with no sender.
/// Malformed items are skipped. Only an empty `<unblock/>` means unblock all.
#[must_use]
pub fn parse_blocking_push(iq: &Iq, account: &BareJid) -> Option<BlockingPush> {
    let Iq::Set { from, payload, .. } = iq else {
        return None;
    };
    if from
        .as_ref()
        .is_some_and(|jid| jid.resource().is_some() || &jid.to_bare() != account)
    {
        return None;
    }

    if payload.is("block", NS_BLOCKING) && payload.get_child("item", NS_BLOCKING).is_some() {
        named(parse_items(payload)).map(BlockingPush::Blocked)
    } else if payload.is("unblock", NS_BLOCKING) && payload.attrs().is_empty() {
        if payload.children().next().is_none() && payload.text().trim().is_empty() {
            Some(BlockingPush::UnblockedAll)
        } else {
            named(parse_items(payload)).map(BlockingPush::Unblocked)
        }
    } else {
        None
    }
}

/// A push that names nobody is no change: one whose items were all
/// malformed must not reach a host as an empty list, which is one slip from
/// being read as "everyone".
fn named(jids: Vec<Jid>) -> Option<Vec<Jid>> {
    (!jids.is_empty()).then_some(jids)
}

fn parse_items(payload: &Element) -> Vec<Jid> {
    payload
        .children()
        // By its `jid` alone: an item may carry a child, as a XEP-0377
        // `<report/>` does, and is no less blocked for it.
        .filter(|item| item.is("item", NS_BLOCKING))
        .filter_map(|item| item.attr("jid")?.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests;
