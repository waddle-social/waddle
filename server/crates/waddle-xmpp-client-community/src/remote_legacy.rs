//! Import the account's XEP-0049 joined-Space preferences into its private XEP-0223 node.
use crate::remote_community::{id, request, LIMIT};
use crate::remote_preferences::{self, preference, WaddleSavedSpace, JOINED};
use jid::BareJid;
use minidom::rxml::xml_ncname;
use minidom::Element;
use std::collections::HashSet;
use std::future::Future;

const PRIVATE: &str = "jabber:iq:private";

fn legacy_iq(kind: &str) -> Element {
    Element::builder("iq", "jabber:client")
        .attr(xml_ncname!("type").to_owned(), kind)
        .attr(xml_ncname!("id").to_owned(), id())
        .append(
            Element::builder("query", PRIVATE)
                .append(Element::builder("spaces", JOINED).build())
                .build(),
        )
        .build()
}
fn document(result: &Element) -> Option<&Element> {
    result
        .get_child("query", PRIVATE)?
        .get_child("spaces", JOINED)
}

/// Retain the old document after a partial import or concurrent edit. A stored PEP
/// entry is authoritative, including an explicit decision to stop following.
pub async fn migrate<
    E,
    F: Future<Output = Result<Element, E>>,
    R: Future<Output = Result<Vec<WaddleSavedSpace>, E>>,
>(
    send: impl Fn(Element) -> F,
    read: impl Fn() -> R,
    own: &BareJid,
    mut current: Vec<WaddleSavedSpace>,
) -> Vec<WaddleSavedSpace> {
    if current.len() >= LIMIT as usize {
        return current;
    }
    let Ok(result) = request(&send, legacy_iq("get"), own, true).await else {
        return current;
    };
    let Some(source) = document(&result) else {
        return current;
    };
    let mut seen = HashSet::new();
    let mut legacy = Vec::new();
    for child in source.children() {
        let Some(entry) = preference(child) else {
            return current;
        };
        if seen.insert(entry.address.clone()) {
            legacy.push(entry);
        }
        if seen.len() > LIMIT as usize {
            return current;
        }
    }
    if legacy.is_empty() {
        return current;
    }
    for entry in &legacy {
        if current
            .iter()
            .any(|published| published.address == entry.address)
        {
            continue;
        }
        let Ok(latest) = read().await else {
            return current;
        };
        current = latest;
        if current
            .iter()
            .any(|published| published.address == entry.address)
        {
            continue;
        }
        if current.len() >= LIMIT as usize
            || remote_preferences::update(&send, own, entry, false)
                .await
                .is_err()
        {
            return current;
        }
    }
    // Only published preferences can renew subscriptions; an unfinished import is retried at the next read.
    let Ok(latest) = read().await else {
        return current;
    };
    let stored = latest.iter().map(|s| &s.address).collect::<HashSet<_>>();
    if legacy.iter().all(|pref| stored.contains(&pref.address)) {
        // XEP-0049 has no compare-and-swap. Re-read before replacing a document older clients can still write.
        if let Ok(now) = request(&send, legacy_iq("get"), own, true).await {
            if document(&now) == Some(source) {
                let _ = request(&send, legacy_iq("set"), own, true).await;
            }
        }
    }
    latest
}
