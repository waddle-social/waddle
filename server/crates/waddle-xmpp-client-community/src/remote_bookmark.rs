//! Read/modify/write of XEP-0402 room preferences after host-confirmed presence.
use crate::remote_address::Address;
use crate::remote_community::{id, request, Error};
use jid::BareJid;
use minidom::Element;
use std::future::Future;
use waddle_xmpp_client::pep::{NS_BOOKMARKS, NS_PUBSUB};
use waddle_xmpp_client::pubsub;
use waddle_xmpp_client::xep::xep0402::{self, BookmarkItem};

pub fn read_iq() -> Element {
    xep0402::build_fetch_bookmarks_iq(&id())
}

pub async fn update<E, F: Future<Output = Result<Element, E>>>(
    send: impl Fn(Element) -> F,
    own: &BareJid,
    room: &str,
    name: Option<String>,
    autojoin: bool,
    forget: bool,
    response: Option<&Element>,
) -> Result<(), Error<E>> {
    let a = Address::from_parts(room, None)
        .filter(|r| r.jid.node().is_some())
        .ok_or(Error::Invalid)?;
    let mut stored = None;
    let mut old_keys = Vec::new();
    if let Some(result) = response {
        if result.attr("type") != Some("result")
            || result.attr("from").is_some_and(|s| s != own.as_str())
        {
            return Err(Error::Untrusted);
        }
        let published = result
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("items", NS_PUBSUB))
            .filter(|items| items.attr("node") == Some(NS_BOOKMARKS))
            .ok_or(Error::Unexpected)?;
        for item in published.children().filter(|c| c.is("item", NS_PUBSUB)) {
            let Some(key) = item.attr("id") else {
                continue;
            };
            if Address::from_parts(key, None).as_ref() != Some(&a) {
                continue;
            }
            if forget || key != a.jid.as_str() {
                old_keys.push(key.to_owned());
            }
            if stored.is_none() || key == a.jid.as_str() {
                if let Some(entry) = BookmarkItem::parse_item(item) {
                    stored = Some(entry);
                }
            }
        }
    }
    if !forget && (autojoin || stored.is_some()) {
        let mut entry = stored.unwrap_or_else(|| BookmarkItem {
            jid: a.jid.clone(),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: vec![],
        });
        entry.jid = a.jid;
        entry.autojoin = autojoin;
        if name.is_some() {
            entry.name = name;
        }
        request(
            &send,
            xep0402::build_publish_bookmark_iq(&entry, &id()),
            own,
            true,
        )
        .await?;
    }
    for key in old_keys {
        request(
            &send,
            pubsub::build_pubsub_retract_iq(&id(), None, NS_BOOKMARKS, &key),
            own,
            true,
        )
        .await?;
    }
    Ok(())
}
