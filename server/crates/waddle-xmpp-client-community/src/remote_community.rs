//! Host-authoritative XEP-0030, XEP-0503 and XEP-0060 operations for remote communities.
use crate::remote_address::Address;
use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use waddle_xmpp_client::discovery as disco;
use waddle_xmpp_client::pep::NS_PUBSUB;
use waddle_xmpp_client::pubsub;

const SPACES: &str = "urn:xmpp:spaces:0";
const META: &str = "http://jabber.org/protocol/pubsub#meta-data";
const ROOMINFO: &str = "http://jabber.org/protocol/muc#roominfo";
const BOOKMARKS: &str = "urn:xmpp:bookmarks:1";
pub(crate) const LIMIT: u32 = 256;

#[derive(Debug)]
pub enum Error<E> {
    Invalid,
    Unexpected,
    Untrusted,
    Transport(E),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleRemoteEntry {
    pub address: String,
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleRemoteCommunity {
    pub kind: String,
    pub address: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub flags: Vec<String>,
    pub entries: Vec<WaddleRemoteEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleRemoteSubscription {
    pub state: String,
    pub subid: Option<String>,
}

pub(crate) async fn request<E, F: Future<Output = Result<Element, E>>>(
    send: &impl Fn(Element) -> F,
    iq: Element,
    expected: &BareJid,
    own_pep: bool,
) -> Result<Element, Error<E>> {
    let result = send(iq).await.map_err(Error::Transport)?;
    match result.attr("from") {
        Some(from) if from.parse::<BareJid>().as_ref().ok() == Some(expected) => {}
        None if own_pep => {}
        _ => return Err(Error::Untrusted),
    }
    if result.attr("type") != Some("result") {
        return Err(Error::Unexpected);
    }
    Ok(result)
}

pub(crate) fn items(result: &Element, node: &str) -> Option<Vec<pubsub::PubsubResultItem>> {
    let items = result
        .get_child("pubsub", NS_PUBSUB)?
        .get_child("items", NS_PUBSUB)?;
    (items.attr("node") == Some(node)).then(|| pubsub::parse_pubsub_items_result(result))
}

fn directory(info: &disco::DiscoInfoResult, result: &Element) -> Option<Vec<WaddleRemoteEntry>> {
    if result
        .get_child("query", disco::DISCO_ITEMS_NS)?
        .attr("node")
        .is_some()
    {
        return None;
    }
    let muc = info.identities.iter().any(|i| i.category == "conference")
        || info.has_feature("http://jabber.org/protocol/muc");
    let pubsub = info.identities.iter().any(|i| i.category == "pubsub");
    let mut seen = HashSet::new();
    Some(
        disco::parse_disco_items_result(result)?
            .into_iter()
            .filter_map(|item| {
                let a = Address::from_parts(&item.jid, item.node.as_deref())?;
                if a.node.is_some() && !pubsub || a.jid.node().is_some() && !muc {
                    return None;
                }
                let address = a.text();
                seen.insert(address.clone()).then_some(WaddleRemoteEntry {
                    address,
                    name: item.name,
                })
            })
            .take(LIMIT as usize)
            .collect(),
    )
}

pub async fn describe<E, F: Future<Output = Result<Element, E>>>(
    send: impl Fn(Element) -> F,
    raw: &str,
) -> Result<WaddleRemoteCommunity, Error<E>> {
    let a = Address::parse(raw).ok_or(Error::Invalid)?;
    let iq = disco::build_disco_info_iq(a.jid.as_str(), a.node.as_deref());
    let result = request(&send, iq, &a.jid, false).await?;
    let info = disco::parse_disco_info_result(&result, a.jid.as_str()).ok_or(Error::Unexpected)?;
    if info.node != a.node {
        return Err(Error::Unexpected);
    }
    let mut d = WaddleRemoteCommunity {
        kind: "other".into(),
        address: a.text(),
        name: info.identities.iter().find_map(|i| i.name.clone()),
        description: None,
        entries: vec![],
        flags: vec![],
    };
    if let Some(node) = &a.node {
        if info.form_value(META, "pubsub#type") != Some(SPACES) {
            return Ok(d);
        }
        d.kind = "space".into();
        if let Some(title) = info.form_value(META, "pubsub#title") {
            d.name = Some(title.to_owned());
        }
        d.description = info
            .form_value(META, "pubsub#description")
            .map(str::to_owned);
        let iq = pubsub::build_pubsub_items_iq(&id(), Some(&a.jid), node, Some(LIMIT));
        let result = match request(&send, iq, &a.jid, false).await {
            Ok(result) => result,
            Err(Error::Transport(_)) => {
                d.flags.push("Room list unavailable".into());
                return Ok(d);
            }
            Err(other) => return Err(other),
        };
        let mut seen = HashSet::new();
        d.entries = items(&result, node)
            .ok_or(Error::Unexpected)?
            .into_iter()
            .filter_map(|item| {
                let room = Address::from_parts(&item.id, None)?;
                let conference = item.payload("conference", BOOKMARKS)?;
                let address = room.text();
                (room.jid.node().is_some() && seen.insert(address.clone())).then(|| {
                    WaddleRemoteEntry {
                        address,
                        name: conference.attr("name").map(str::to_owned),
                    }
                })
            })
            .take(LIMIT as usize)
            .collect();
    } else if a.jid.node().is_some() {
        if info.identities.iter().any(|i| i.category == "conference") {
            d.kind = "room".into();
            d.description = info
                .form_value(ROOMINFO, "muc#roominfo_description")
                .map(str::to_owned);
            for (feature, label) in [
                ("muc_membersonly", "Members only"),
                ("muc_passwordprotected", "Password protected"),
                ("muc_hidden", "Hidden"),
                ("muc_moderated", "Moderated"),
                ("muc_nonanonymous", "Non-anonymous"),
            ] {
                if info.has_feature(feature) {
                    d.flags.push(label.into());
                }
            }
            if let Some(parent) = info
                .form_value(SPACES, "parent")
                .or_else(|| info.form_value(ROOMINFO, "muc#roomconfig_pubsub"))
                .and_then(Address::parse)
                .filter(|a| a.node.is_some())
            {
                d.entries.push(WaddleRemoteEntry {
                    address: parent.text(),
                    name: Some("Parent Space".into()),
                });
            }
        }
    } else {
        d.kind = "directory".into();
        let iq = disco::build_disco_items_iq(a.jid.as_str(), None);
        let result = request(&send, iq, &a.jid, false).await?;
        d.entries = directory(&info, &result).ok_or(Error::Unexpected)?;
    }
    Ok(d)
}

pub async fn subscribe<E, F: Future<Output = Result<Element, E>>>(
    send: impl Fn(Element) -> F,
    own: &BareJid,
    raw: &str,
    follow: bool,
    subid: Option<&str>,
) -> Result<WaddleRemoteSubscription, Error<E>> {
    let a = Address::parse(raw).ok_or(Error::Invalid)?;
    let node = a.node.as_deref().ok_or(Error::Invalid)?;
    let iq = if follow {
        pubsub::build_pubsub_subscribe_iq(&id(), &a.jid, node, own)
    } else {
        pubsub::build_pubsub_unsubscribe_iq(&id(), &a.jid, node, own, subid)
    };
    let result = request(&send, iq, &a.jid, false).await?;
    if !follow {
        return Ok(WaddleRemoteSubscription {
            state: "none".into(),
            subid: None,
        });
    }
    let sub = pubsub::parse_pubsub_subscribe_result(&result).ok_or(Error::Unexpected)?;
    if sub.node != node
        || sub
            .jid
            .as_ref()
            .is_none_or(|s| s.to_string() != own.as_str())
    {
        return Err(Error::Untrusted);
    }
    let state = match sub.state {
        pubsub::PubsubSubscriptionState::Subscribed => "subscribed",
        pubsub::PubsubSubscriptionState::Pending => "pending",
        pubsub::PubsubSubscriptionState::Unconfigured => "unconfigured",
        pubsub::PubsubSubscriptionState::None => "none",
    };
    Ok(WaddleRemoteSubscription {
        state: state.into(),
        subid: sub.subid,
    })
}
pub(crate) fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
