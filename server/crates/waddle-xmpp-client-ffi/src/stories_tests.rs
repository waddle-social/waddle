//! XEP-0501 / XEP-0470 FFI test suite: story, read-state and
//! story-reaction verb wire shapes as produced by the exact FFI
//! construction path, reply → typed-result mapping, input validation,
//! the rule for what replaces a member's reactions, and the exported
//! methods' typed failure behavior without a live session.

use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::xep0470::{Attachment, AttachmentPayload, ReactionSet};

use crate::stories::{
    map_stories_reply, map_story_reactions_replies, map_story_reads_reply, next_story_attachment,
    reaction_set, stories_fetch_stanza, story_from_post, story_publish_stanza,
    story_reads_from_ffi,
};
use crate::test_support::offline_client;
use crate::{
    WaddleError, WaddleStory, WaddleStoryPost, WaddleStoryReaction, WaddleStoryReactions,
    WaddleStoryRead,
};

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
const STORIES_NODE: &str = "urn:xmpp:pubsub-social-feed:stories:0";
const COMMUNITY: &str = "community.waddle.test";

fn community() -> BareJid {
    COMMUNITY.parse().expect("valid service jid")
}

fn alice() -> BareJid {
    "alice@waddle.test".parse().expect("valid bare jid")
}

fn parse(xml: &str) -> Element {
    xml.parse().expect("fixture parses")
}

fn stanza_error(error_type: StanzaErrorType, condition: &str) -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type,
        condition: condition.to_string(),
        text: None,
        application_condition: None,
    })
}

fn item_not_found() -> ClientError {
    stanza_error(StanzaErrorType::Cancel, "item-not-found")
}

fn forbidden() -> ClientError {
    stanza_error(StanzaErrorType::Auth, "forbidden")
}

fn forbidden_error() -> WaddleError {
    WaddleError::Stanza {
        condition: "forbidden".to_string(),
        text: None,
    }
}

fn photo_post() -> WaddleStoryPost {
    WaddleStoryPost {
        body: Some("at the rink".to_string()),
        media_url: "https://upload.waddle.test/rink.jpg".to_string(),
        media_type: Some("image/jpeg".to_string()),
        expiry_hours: None,
    }
}

fn reactions(values: &[&str]) -> ReactionSet {
    reaction_set(strings(values)).expect("valid reactions")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

// ── Stories: wire shapes ─────────────────────────────────────────────

#[test]
fn fetch_stanza_asks_the_community_stories_node() {
    let iq = stories_fetch_stanza(&community(), Some(50));
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some(COMMUNITY));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("node"), Some(STORIES_NODE));
    assert_eq!(items.attr("max_items"), Some("50"));
}

#[test]
fn publish_stanza_carries_the_story_as_an_atom_entry_with_its_media() {
    let story = story_from_post(photo_post(), &alice()).expect("valid story");
    let iq = story_publish_stanza(&community(), &story);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some(COMMUNITY));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish element");
    assert_eq!(publish.attr("node"), Some(STORIES_NODE));
    let item = publish.get_child("item", NS_PUBSUB).expect("item element");
    assert_eq!(item.attr("id"), Some(story.id.as_str()));
    let enclosure = item
        .get_child("entry", NS_ATOM)
        .and_then(|entry| entry.get_child("link", NS_ATOM))
        .expect("enclosure link");
    assert_eq!(enclosure.attr("rel"), Some("enclosure"));
    assert_eq!(
        enclosure.attr("href"),
        Some("https://upload.waddle.test/rink.jpg")
    );
    assert_eq!(enclosure.attr("type"), Some("image/jpeg"));
}

// ── Stories: post validation ─────────────────────────────────────────

#[test]
fn a_story_gets_a_fresh_id_the_account_as_author_and_a_day_to_live() {
    let first = story_from_post(photo_post(), &alice()).expect("valid story");
    let second = story_from_post(photo_post(), &alice()).expect("valid story");
    assert!(first.id.starts_with("story-"));
    assert_ne!(first.id, second.id);
    assert_eq!(first.author.as_deref(), Some("alice@waddle.test"));
    let lifetime = first.expires.expect("expires") - first.posted.expect("posted");
    assert_eq!(lifetime.num_minutes(), 24 * 60);
}

#[test]
fn a_story_lives_as_long_as_asked_but_at_least_an_hour() {
    let lifetime_hours = |expiry_hours| {
        let story = story_from_post(
            WaddleStoryPost {
                expiry_hours,
                ..photo_post()
            },
            &alice(),
        )
        .expect("valid story");
        (story.expires.expect("expires") - story.posted.expect("posted")).num_minutes() / 60
    };
    assert_eq!(lifetime_hours(Some(6)), 6);
    assert_eq!(lifetime_hours(Some(0)), 1);
}

#[test]
fn a_story_that_would_outlive_the_calendar_is_refused() {
    let forever = WaddleStoryPost {
        expiry_hours: Some(u32::MAX),
        ..photo_post()
    };
    assert_eq!(
        story_from_post(forever, &alice()),
        Err(WaddleError::InvalidArgument)
    );
}

#[test]
fn a_story_without_media_or_with_a_malformed_media_type_is_refused() {
    let no_media = WaddleStoryPost {
        media_url: "  ".to_string(),
        ..photo_post()
    };
    assert_eq!(
        story_from_post(no_media, &alice()),
        Err(WaddleError::InvalidArgument)
    );
    let bad_type = WaddleStoryPost {
        media_type: Some("jpeg".to_string()),
        ..photo_post()
    };
    assert_eq!(
        story_from_post(bad_type, &alice()),
        Err(WaddleError::InvalidArgument)
    );
}

// ── Stories: reply mapping ───────────────────────────────────────────

#[test]
fn a_stories_reply_maps_to_typed_stories() {
    let reply = parse(
        "<iq xmlns='jabber:client' type='result' id='x'>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
             <items node='urn:xmpp:pubsub-social-feed:stories:0'>\
               <item id='story-1'>\
                 <entry xmlns='http://www.w3.org/2005/Atom'>\
                   <title type='text'>at the rink</title>\
                   <id>story-1</id>\
                   <published>2026-11-20T10:00:00Z</published>\
                   <updated>2026-11-20T10:00:00Z</updated>\
                   <content type='text'>at the rink</content>\
                   <link rel='enclosure' href='https://upload.waddle.test/rink.jpg' type='image/jpeg'/>\
                   <author><uri>xmpp:alice@waddle.test</uri></author>\
                   <expires xmlns='urn:waddle:stories:0'>2026-11-21T10:00:00Z</expires>\
                 </entry>\
               </item>\
               <item id='no-media'>\
                 <entry xmlns='http://www.w3.org/2005/Atom'>\
                   <title type='text'>text only</title>\
                   <id>no-media</id>\
                   <updated>2026-11-20T10:00:00Z</updated>\
                 </entry>\
               </item>\
             </items>\
           </pubsub>\
         </iq>",
    );
    assert_eq!(
        map_stories_reply(Ok(reply)),
        Ok(vec![WaddleStory {
            id: "story-1".to_string(),
            body: Some("at the rink".to_string()),
            media_url: "https://upload.waddle.test/rink.jpg".to_string(),
            media_type: Some("image/jpeg".to_string()),
            author: Some("alice@waddle.test".to_string()),
            posted_ms: Some(1_795_168_800_000),
            expires_ms: Some(1_795_255_200_000),
        }])
    );
}

#[test]
fn a_stories_stanza_error_keeps_its_condition() {
    assert_eq!(map_stories_reply(Err(forbidden())), Err(forbidden_error()));
}

// ── Read state ───────────────────────────────────────────────────────

fn reads_reply(from: Option<&str>) -> Element {
    let from = from
        .map(|from| format!(" from='{from}'"))
        .unwrap_or_default();
    parse(&format!(
        "<iq xmlns='jabber:client' type='result' id='x'{from}>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
             <items node='urn:waddle:story:reads:0'>\
               <item id='current'>\
                 <reads xmlns='urn:waddle:story:reads:0'>\
                   <read id='story-1' at='2026-11-20T10:00:00Z'/>\
                 </reads>\
               </item>\
             </items>\
           </pubsub>\
         </iq>"
    ))
}

fn story_1_read() -> Vec<WaddleStoryRead> {
    vec![WaddleStoryRead {
        story_id: "story-1".to_string(),
        read_at_ms: 1_795_168_800_000,
    }]
}

#[test]
fn a_reads_reply_from_the_own_account_maps_to_typed_reads() {
    assert_eq!(
        map_story_reads_reply(Ok(reads_reply(None)), &alice()),
        Ok(story_1_read())
    );
    assert_eq!(
        map_story_reads_reply(Ok(reads_reply(Some("alice@waddle.test"))), &alice()),
        Ok(story_1_read())
    );
}

#[test]
fn a_reads_reply_from_anyone_else_is_not_trusted() {
    assert_eq!(
        map_story_reads_reply(Ok(reads_reply(Some("mallory@waddle.test"))), &alice()),
        Err(WaddleError::UntrustedReply)
    );
}

#[test]
fn never_having_read_a_story_is_an_empty_read_state() {
    assert_eq!(
        map_story_reads_reply(Err(item_not_found()), &alice()),
        Ok(Vec::new())
    );
}

#[test]
fn a_failed_reads_fetch_is_an_error_not_an_empty_read_state() {
    assert_eq!(
        map_story_reads_reply(Err(forbidden()), &alice()),
        Err(forbidden_error())
    );
}

#[test]
fn reads_round_trip_through_the_wire_payload() {
    let reads = story_reads_from_ffi(story_1_read()).expect("valid reads");
    let element = reads.build_element();
    let read = element.children().next().expect("read element");
    assert_eq!(read.attr("id"), Some("story-1"));
    assert_eq!(
        read.attr("at")
            .map(|at| at.parse::<chrono::DateTime<chrono::Utc>>()),
        Some(Ok("2026-11-20T10:00:00Z".parse().expect("valid timestamp")))
    );
}

#[test]
fn a_read_with_an_empty_story_id_is_refused() {
    let result = story_reads_from_ffi(vec![WaddleStoryRead {
        story_id: String::new(),
        read_at_ms: 0,
    }]);
    assert_eq!(result.map(|_| ()), Err(WaddleError::InvalidArgument));
}

// ── Reactions: reply mapping ─────────────────────────────────────────

fn summary_reply() -> Element {
    parse(
        "<iq xmlns='jabber:client' type='result' id='x'>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
             <items node='urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0'>\
               <item id='story-1'>\
                 <summary xmlns='urn:xmpp:pubsub-attachments:summary:1'>\
                   <reactions>\
                     <reaction count='2'>👍</reaction>\
                     <reaction count='1'>❤️</reaction>\
                   </reactions>\
                 </summary>\
               </item>\
             </items>\
           </pubsub>\
         </iq>",
    )
}

fn own_reply(children: &str) -> Element {
    parse(&format!(
        "<iq xmlns='jabber:client' type='result' id='x'>\
           <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
             <items node='n'>\
               <item id='alice@waddle.test'>\
                 <attachments xmlns='urn:xmpp:pubsub-attachments:1'>{children}</attachments>\
               </item>\
             </items>\
           </pubsub>\
         </iq>"
    ))
}

#[test]
fn reactions_combine_the_counts_with_the_accounts_own_emoji() {
    let reactions = map_story_reactions_replies(
        Ok(summary_reply()),
        Ok(own_reply("<reactions><reaction>👍</reaction></reactions>")),
        "story-1",
        &alice(),
    );
    assert_eq!(
        reactions,
        Ok(WaddleStoryReactions {
            reactions: vec![
                WaddleStoryReaction {
                    emoji: "👍".to_string(),
                    count: 2,
                },
                WaddleStoryReaction {
                    emoji: "❤️".to_string(),
                    count: 1,
                },
            ],
            mine: strings(&["👍"]),
        })
    );
}

#[test]
fn a_story_nobody_reacted_to_has_no_reactions() {
    let reactions = map_story_reactions_replies(
        Err(item_not_found()),
        Err(item_not_found()),
        "story-1",
        &alice(),
    );
    assert_eq!(
        reactions,
        Ok(WaddleStoryReactions {
            reactions: Vec::new(),
            mine: Vec::new(),
        })
    );
}

#[test]
fn a_failed_reactions_fetch_is_an_error() {
    assert_eq!(
        map_story_reactions_replies(Err(forbidden()), Ok(own_reply("")), "story-1", &alice()),
        Err(forbidden_error())
    );
    assert_eq!(
        map_story_reactions_replies(Ok(summary_reply()), Err(forbidden()), "story-1", &alice()),
        Err(forbidden_error())
    );
}

// ── Reactions: what replaces a member's reactions ────────────────────

#[test]
fn first_reactions_become_a_new_attachment() {
    let next = next_story_attachment(None, reactions(&["👍", " ❤️ ", "👍"]));
    assert_eq!(
        next,
        Some(Attachment::reactions(ReactionSet::new(["👍", "❤️"])))
    );
}

#[test]
fn new_reactions_replace_the_old_and_keep_everything_else() {
    let vote = parse("<vote xmlns='urn:example:vote'>yes</vote>");
    let current = Attachment::reactions(ReactionSet::new(["👍"]))
        .with_payload(AttachmentPayload::Noticed)
        .with_unknown_child(vote.clone());
    let next = next_story_attachment(Some(current), reactions(&["🎉"])).expect("attachment");
    assert_eq!(
        next.reactions_set().map(|set| set.emojis.clone()),
        Some(strings(&["🎉"]))
    );
    assert!(next.payloads.contains(&AttachmentPayload::Noticed));
    assert_eq!(next.unknown_children, vec![vote]);
    let reaction_sets = next
        .payloads
        .iter()
        .filter(|payload| matches!(payload, AttachmentPayload::Reactions(_)))
        .count();
    assert_eq!(reaction_sets, 1);
}

#[test]
fn clearing_reactions_leaves_nothing_when_nothing_else_was_attached() {
    let current = Attachment::reactions(ReactionSet::new(["👍"]));
    assert_eq!(next_story_attachment(Some(current), reactions(&[])), None);
    assert_eq!(next_story_attachment(None, reactions(&[" "])), None);
}

#[test]
fn clearing_reactions_keeps_what_else_was_attached() {
    let current =
        Attachment::reactions(ReactionSet::new(["👍"])).with_payload(AttachmentPayload::Noticed);
    let next = next_story_attachment(Some(current), reactions(&[])).expect("attachment");
    assert_eq!(next.payloads, vec![AttachmentPayload::Noticed]);
}

#[test]
fn too_many_reactions_or_more_than_one_emoji_each_are_refused() {
    let thirteen: Vec<String> = "😀😁😂🤣😃😄😅😆😉😊😋😎😍"
        .chars()
        .map(String::from)
        .collect();
    assert_eq!(thirteen.len(), 13);
    assert_eq!(reaction_set(thirteen), Err(WaddleError::InvalidArgument));
    assert_eq!(
        reaction_set(strings(&["👍👍"])),
        Err(WaddleError::InvalidArgument)
    );
}

// ── Exported verbs without a session ─────────────────────────────────

#[tokio::test]
async fn story_verbs_report_not_connected() {
    let client = offline_client();
    let community = || COMMUNITY.to_string();
    let story = || "story-1".to_string();
    let offline = Err(WaddleError::NotConnected);
    assert_eq!(client.fetch_stories(community(), None).await, offline);
    assert_eq!(
        client.publish_story(community(), photo_post()).await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client.fetch_story_reads().await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client.publish_story_reads(story_1_read()).await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client.fetch_story_reactions(community(), story()).await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client
            .set_story_reactions(community(), story(), strings(&["👍"]))
            .await,
        Err(WaddleError::NotConnected)
    );
}

#[tokio::test]
async fn story_verbs_refuse_invalid_input_before_they_need_a_session() {
    let client = offline_client();
    let bad_jid = || "not a jid".to_string();
    assert_eq!(
        client.fetch_stories(bad_jid(), None).await,
        Err(WaddleError::InvalidJid)
    );
    assert_eq!(
        client.publish_story(bad_jid(), photo_post()).await,
        Err(WaddleError::InvalidJid)
    );
    assert_eq!(
        client
            .fetch_story_reactions(bad_jid(), "story-1".to_string())
            .await,
        Err(WaddleError::InvalidJid)
    );
    assert_eq!(
        client
            .fetch_story_reactions(COMMUNITY.to_string(), " ".to_string())
            .await,
        Err(WaddleError::InvalidArgument)
    );
    assert_eq!(
        client
            .set_story_reactions(
                COMMUNITY.to_string(),
                "story-1".to_string(),
                strings(&["👍👍"])
            )
            .await,
        Err(WaddleError::InvalidArgument)
    );
}
