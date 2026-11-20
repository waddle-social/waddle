//! Story reactions: XEP-0470 Pubsub Attachments over the XEP-0501
//! community stories node.
//!
//! A member's reactions to one story are a single item, keyed by the
//! member's bare JID, on the story's attachment node
//! (`urn:xmpp:pubsub-attachments:1/<story item URI>`). The community
//! service keeps per-emoji counts for every story on one summary node,
//! keyed by story id.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp_core::xep0470::{
    build_attachments_element, parse_attachments_element, Attachment, NS_PUBSUB_ATTACHMENTS,
    PUBSUB_ATTACHMENTS_NODE_PREFIX,
};
use waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES;

use crate::pubsub::{
    build_pubsub_item_iq, build_pubsub_publish_iq, build_pubsub_retract_iq,
    parse_pubsub_items_result,
};
use crate::pubsub_event::{parse_attachment_summary, AttachmentSummaryEvent};

#[cfg(test)]
mod tests;

/// Summary node holding the reaction counts of every story: the
/// XEP-0470 summary namespace over the stories node.
pub const STORY_REACTIONS_SUMMARY_NODE: &str =
    "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0";

const NS_PUBSUB_ATTACHMENTS_SUMMARY: &str = "urn:xmpp:pubsub-attachments:summary:1";

/// Attachment node of one story on `community`. The service accepts
/// only this exact spelling, so the node and item id are
/// percent-encoded the way it encodes them.
#[must_use]
pub fn story_attachment_node(community: &BareJid, story_id: &str) -> String {
    format!(
        "{PUBSUB_ATTACHMENTS_NODE_PREFIX}/xmpp:{community}?;node={};item={}",
        urlencoding::encode(PUBSUB_NODE_STORIES),
        urlencoding::encode(story_id)
    )
}

/// Items GET for the reaction counts of one story.
#[must_use]
pub fn build_story_reaction_summary_iq(
    iq_id: &str,
    community: &BareJid,
    story_id: &str,
) -> Element {
    build_pubsub_item_iq(
        iq_id,
        Some(community),
        STORY_REACTIONS_SUMMARY_NODE,
        story_id,
    )
}

/// Items GET for the attachment `own` published on one story.
#[must_use]
pub fn build_own_story_attachment_iq(
    iq_id: &str,
    community: &BareJid,
    story_id: &str,
    own: &BareJid,
) -> Element {
    build_pubsub_item_iq(
        iq_id,
        Some(community),
        &story_attachment_node(community, story_id),
        own.as_str(),
    )
}

/// Publish SET replacing the attachment `own` holds on one story.
#[must_use]
pub fn build_story_attachment_publish_iq(
    iq_id: &str,
    community: &BareJid,
    story_id: &str,
    own: &BareJid,
    attachment: &Attachment,
) -> Element {
    build_pubsub_publish_iq(
        iq_id,
        Some(community),
        &story_attachment_node(community, story_id),
        Some(own.as_str()),
        build_attachments_element(attachment),
        None,
    )
}

/// Retract SET removing the attachment `own` holds on one story.
#[must_use]
pub fn build_story_attachment_retract_iq(
    iq_id: &str,
    community: &BareJid,
    story_id: &str,
    own: &BareJid,
) -> Element {
    build_pubsub_retract_iq(
        iq_id,
        Some(community),
        &story_attachment_node(community, story_id),
        own.as_str(),
    )
}

/// The reaction counts of `story_id` in a summary items result, or
/// `None` when the result holds no summary for that story.
#[must_use]
pub fn parse_story_reaction_summary(
    iq: &Element,
    story_id: &str,
) -> Option<AttachmentSummaryEvent> {
    parse_pubsub_items_result(iq)
        .into_iter()
        .filter(|item| item.id == story_id)
        .find_map(|item| {
            item.payload("summary", NS_PUBSUB_ATTACHMENTS_SUMMARY)
                .map(parse_attachment_summary)
        })
}

/// The attachment `own` published, from an attachment-node items
/// result, or `None` when `own` has none there.
#[must_use]
pub fn parse_own_story_attachment(iq: &Element, own: &BareJid) -> Option<Attachment> {
    parse_pubsub_items_result(iq)
        .into_iter()
        .filter(|item| item.id == own.as_str())
        .find_map(|item| {
            item.payload("attachments", NS_PUBSUB_ATTACHMENTS)
                .and_then(parse_attachments_element)
        })
}
