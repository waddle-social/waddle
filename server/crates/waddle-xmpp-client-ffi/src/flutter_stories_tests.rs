//! Per-author story requests against the XEP-0501 and XEP-0470 service shapes.

use std::cell::RefCell;
use std::collections::VecDeque;

use jid::BareJid;
use minidom::Element;
use waddle_xmpp_client::push::CommandDriver;
use waddle_xmpp_client::ClientResult;
use waddle_xmpp_client_community::pep_stories::{self, StoryError};
use waddle_xmpp_client_community::stories::WaddleStoryInput;
use waddle_xmpp_core::xep0470::NS_PUBSUB_ATTACHMENTS;
use waddle_xmpp_core::xep0501::{parse_story, NS_ATOM, PUBSUB_NODE_STORIES};

const NS: &str = "http://jabber.org/protocol/pubsub";

struct Driver {
    requests: RefCell<Vec<Element>>,
    replies: RefCell<VecDeque<Element>>,
}

impl Driver {
    fn new(replies: &[&str]) -> Self {
        Self {
            requests: RefCell::default(),
            replies: RefCell::new(
                replies
                    .iter()
                    .map(|xml| xml.parse().expect("fixture"))
                    .collect(),
            ),
        }
    }
}

impl CommandDriver for Driver {
    async fn send_iq(&self, iq: Element) -> ClientResult<Element> {
        self.requests.borrow_mut().push(iq);
        Ok(self.replies.borrow_mut().pop_front().expect("expected IQ"))
    }
}

#[tokio::test]
async fn xep0501_publish_uses_own_pep_and_stamps_the_account_as_atom_author() {
    let own: BareJid = "alice@example.test".parse().unwrap();
    let driver = Driver::new(&["<iq xmlns='jabber:client' type='result'/>"]);
    let published = pep_stories::publish(
        &driver,
        &own,
        WaddleStoryInput {
            body: Some("A photo".into()),
            media_url: Some("https://example.test/one.jpg".into()),
            media_type: Some("image/jpeg".into()),
            expiry_hours: None,
            author: Some("bob@example.test".into()),
        },
    )
    .await
    .unwrap();
    let sent = driver.requests.borrow();
    assert_eq!(sent[0].attr("to"), None);
    let publish = sent[0]
        .get_child("pubsub", NS)
        .unwrap()
        .get_child("publish", NS)
        .unwrap();
    assert_eq!(publish.attr("node"), Some(PUBSUB_NODE_STORIES));
    let item = publish.get_child("item", NS).unwrap();
    assert_eq!(item.attr("id"), Some(published.id.as_str()));
    let story = parse_story(&published.id, item.get_child("entry", NS_ATOM).unwrap()).unwrap();
    assert_eq!(story.author.as_deref(), Some(own.as_str()));
    assert_eq!(
        story.media_url.as_deref(),
        Some("https://example.test/one.jpg")
    );
    assert!(story.expires.unwrap() > story.posted.unwrap());
}

#[tokio::test]
async fn xep0501_only_follows_presence_subscriptions_and_rejects_foreign_iq_replies() {
    let own: BareJid = "alice@example.test".parse().unwrap();
    let author: BareJid = "bob@example.test".parse().unwrap();
    let driver = Driver::new(&[
        "<iq xmlns='jabber:client' type='result'><query xmlns='jabber:iq:roster'><item jid='bob@example.test' subscription='to'/><item jid='eve@example.test' subscription='from'/></query></iq>",
        "<iq xmlns='jabber:client' type='result' from='eve@example.test'/>",
    ]);
    assert_eq!(
        pep_stories::authors(&driver, &own).await.unwrap(),
        [own.as_str(), author.as_str()]
    );
    assert!(matches!(
        pep_stories::fetch(&driver, &author, &own, Some(20)).await,
        Err(StoryError::Untrusted)
    ));
    let sent = driver.requests.borrow();
    assert_eq!(sent[1].attr("to"), Some(author.as_str()));
    assert_eq!(
        sent[1]
            .get_child("pubsub", NS)
            .unwrap()
            .get_child("items", NS)
            .unwrap()
            .attr("node"),
        Some(PUBSUB_NODE_STORIES)
    );
}

#[tokio::test]
async fn xep0470_updates_the_reactors_item_on_the_authors_node_without_losing_other_payloads() {
    let own: BareJid = "alice@example.test".parse().unwrap();
    let author: BareJid = "bob@example.test".parse().unwrap();
    let driver = Driver::new(&[
        "<iq xmlns='jabber:client' type='result' from='bob@example.test'><pubsub xmlns='http://jabber.org/protocol/pubsub'><items><item id='alice@example.test'><attachments xmlns='urn:xmpp:pubsub-attachments:1'><noticed/><extra xmlns='urn:example:attachment'/></attachments></item></items></pubsub></iq>",
        "<iq xmlns='jabber:client' type='result' from='bob@example.test'/>",
    ]);
    pep_stories::set_reactions(&driver, &author, "a/b", &own, vec!["🐧".into()])
        .await
        .unwrap();
    let sent = driver.requests.borrow();
    assert_eq!(sent[1].attr("to"), Some(author.as_str()));
    let publish = sent[1]
        .get_child("pubsub", NS)
        .unwrap()
        .get_child("publish", NS)
        .unwrap();
    assert_eq!(publish.attr("node"),Some("urn:xmpp:pubsub-attachments:1/xmpp:bob@example.test?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0;item=a%2Fb"));
    let item = publish.get_child("item", NS).unwrap();
    assert_eq!(item.attr("id"), Some(own.as_str()));
    let attachment = item
        .get_child("attachments", NS_PUBSUB_ATTACHMENTS)
        .unwrap();
    assert!(attachment
        .get_child("noticed", NS_PUBSUB_ATTACHMENTS)
        .is_some());
    assert!(attachment
        .get_child("extra", "urn:example:attachment")
        .is_some());
    assert_eq!(
        attachment
            .get_child("reactions", NS_PUBSUB_ATTACHMENTS)
            .unwrap()
            .get_child("reaction", NS_PUBSUB_ATTACHMENTS)
            .unwrap()
            .text(),
        "🐧"
    );
}
