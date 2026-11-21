//! XEP-0470 story-reaction client suite: the attachment and summary
//! node names the community service expects, the fetch/publish/retract
//! IQ wire shapes, and reply parsing (summary counts, the caller's own
//! attachment with children this client does not understand).

use jid::BareJid;
use minidom::Element;
use waddle_xmpp_core::xep0470::{Attachment, AttachmentPayload, ReactionSet};

use super::{
    build_own_story_attachment_iq, build_story_attachment_publish_iq,
    build_story_attachment_retract_iq, build_story_reaction_summary_iq, parse_own_story_attachment,
    parse_story_reaction_summary, story_attachment_node, story_reactions_summary_node,
};

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_ATTACHMENTS: &str = "urn:xmpp:pubsub-attachments:1";
const SUMMARY_NODE: &str =
    "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0";
const STORY_NODE: &str = "urn:xmpp:pubsub-attachments:1/xmpp:community.waddle.test?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0;item=story-1";

fn community() -> BareJid {
    "community.waddle.test".parse().expect("valid service jid")
}

fn alice() -> BareJid {
    "alice@waddle.test".parse().expect("valid bare jid")
}

fn pubsub_child<'a>(iq: &'a Element, name: &str) -> &'a Element {
    iq.get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child(name, NS_PUBSUB))
        .expect("pubsub child")
}

fn parse(xml: &str) -> Element {
    xml.parse().expect("fixture parses")
}

#[test]
fn summary_node_is_the_summary_namespace_over_the_stories_node() {
    assert_eq!(story_reactions_summary_node(), SUMMARY_NODE);
}

#[test]
fn attachment_node_is_the_namespace_over_the_story_item_uri() {
    assert_eq!(story_attachment_node(&community(), "story-1"), STORY_NODE);
}

#[test]
fn attachment_node_percent_encodes_the_story_id() {
    assert_eq!(
        story_attachment_node(&community(), "a b;c=d/é"),
        "urn:xmpp:pubsub-attachments:1/xmpp:community.waddle.test?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0;item=a%20b%3Bc%3Dd%2F%C3%A9"
    );
}

#[test]
fn summary_iq_asks_the_summary_node_for_the_story_item() {
    let iq = build_story_reaction_summary_iq("iq-1", &community(), "story-1");
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    let items = pubsub_child(&iq, "items");
    assert_eq!(items.attr("node"), Some(SUMMARY_NODE));
    let item = items.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some("story-1"));
}

#[test]
fn own_attachment_iq_asks_the_attachment_node_for_the_callers_item() {
    let iq = build_own_story_attachment_iq("iq-2", &community(), "story-1", &alice());
    assert_eq!(iq.attr("type"), Some("get"));
    let items = pubsub_child(&iq, "items");
    assert_eq!(items.attr("node"), Some(STORY_NODE));
    let item = items.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some("alice@waddle.test"));
}

#[test]
fn publish_iq_stores_the_attachment_under_the_callers_bare_jid() {
    let attachment = Attachment::reactions(ReactionSet::new(["👍", "❤️"]));
    let iq =
        build_story_attachment_publish_iq("iq-3", &community(), "story-1", &alice(), &attachment);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    let publish = pubsub_child(&iq, "publish");
    assert_eq!(publish.attr("node"), Some(STORY_NODE));
    let item = publish.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some("alice@waddle.test"));
    let reactions: Vec<String> = item
        .get_child("attachments", NS_ATTACHMENTS)
        .and_then(|attachments| attachments.get_child("reactions", NS_ATTACHMENTS))
        .expect("reactions")
        .children()
        .map(Element::text)
        .collect();
    assert_eq!(reactions, ["👍", "❤️"]);
}

#[test]
fn retract_iq_removes_the_callers_item() {
    let iq = build_story_attachment_retract_iq("iq-4", &community(), "story-1", &alice());
    assert_eq!(iq.attr("type"), Some("set"));
    let retract = pubsub_child(&iq, "retract");
    assert_eq!(retract.attr("node"), Some(STORY_NODE));
    let item = retract.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some("alice@waddle.test"));
}

#[test]
fn summary_result_yields_per_emoji_counts_for_the_story() {
    let iq = parse(
        "<iq xmlns='jabber:client' type='result' id='x'>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
             <items node='urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0'>\
               <item id='story-other'>\
                 <summary xmlns='urn:xmpp:pubsub-attachments:summary:1'>\
                   <reactions><reaction count='9'>🎉</reaction></reactions>\
                 </summary>\
               </item>\
               <item id='story-1'>\
                 <summary xmlns='urn:xmpp:pubsub-attachments:summary:1'>\
                   <reactions>\
                     <reaction count='2'>👍</reaction>\
                     <reaction count='1'>❤️</reaction>\
                   </reactions>\
                   <noticed count='3'/>\
                 </summary>\
               </item>\
             </items>\
           </pubsub>\
         </iq>",
    );
    let summary = parse_story_reaction_summary(&iq, "story-1").expect("summary");
    let counts: Vec<(&str, u32)> = summary
        .reactions
        .iter()
        .map(|reaction| (reaction.emoji.as_str(), reaction.count))
        .collect();
    assert_eq!(counts, [("👍", 2), ("❤️", 1)]);
    assert_eq!(summary.noticed_count, Some(3));
}

#[test]
fn summary_result_without_the_story_item_is_none() {
    let iq = parse("<iq xmlns='jabber:client' type='result' id='x'/>");
    assert!(parse_story_reaction_summary(&iq, "story-1").is_none());
}

#[test]
fn own_attachment_result_keeps_children_this_client_does_not_understand() {
    let iq = parse(
        "<iq xmlns='jabber:client' type='result' id='x'>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
             <items node='n'>\
               <item id='bob@waddle.test'>\
                 <attachments xmlns='urn:xmpp:pubsub-attachments:1'>\
                   <reactions><reaction>🎉</reaction></reactions>\
                 </attachments>\
               </item>\
               <item id='alice@waddle.test'>\
                 <attachments xmlns='urn:xmpp:pubsub-attachments:1'>\
                   <reactions><reaction>👍</reaction></reactions>\
                   <noticed/>\
                   <vote xmlns='urn:example:vote'>yes</vote>\
                 </attachments>\
               </item>\
             </items>\
           </pubsub>\
         </iq>",
    );
    let attachment = parse_own_story_attachment(&iq, &alice()).expect("own attachment");
    assert_eq!(
        attachment.reactions_set().map(|set| set.emojis.clone()),
        Some(vec!["👍".to_string()])
    );
    assert!(attachment.payloads.contains(&AttachmentPayload::Noticed));
    assert_eq!(attachment.unknown_children.len(), 1);
    assert_eq!(attachment.unknown_children[0].name(), "vote");
}

#[test]
fn own_attachment_result_without_the_callers_item_is_none() {
    let iq = parse(
        "<iq xmlns='jabber:client' type='result' id='x'>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='n'/></pubsub>\
         </iq>",
    );
    assert!(parse_own_story_attachment(&iq, &alice()).is_none());
}
