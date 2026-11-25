//! The account's private XEP-0223 preference for each qualified Space (ADR-0020).
use crate::remote_address::Address;
use crate::remote_community::{id, items, request, Error, LIMIT};
use jid::BareJid;
use minidom::rxml::xml_ncname;
use minidom::Element;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use waddle_xmpp_client::pubsub;

pub(crate) const JOINED: &str = "urn:waddle:joined-spaces:0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleSavedSpace {
    pub address: String,
    pub name: Option<String>,
    pub follow: bool,
    pub subid: Option<String>,
    pub revision: Option<String>,
}

pub(crate) fn preference(entry: &Element) -> Option<WaddleSavedSpace> {
    if !entry.is("space", JOINED) {
        return None;
    }
    let a = Address::from_parts(entry.attr("service")?, Some(entry.attr("node")?))?;
    let follow = matches!(entry.attr("follow"), Some("true" | "1"));
    Some(WaddleSavedSpace {
        address: a.text(),
        name: entry.attr("name").map(str::to_owned),
        follow,
        subid: entry.attr("subid").filter(|_| follow).map(str::to_owned),
        revision: entry.attr("rev").map(str::to_owned),
    })
}
pub fn list_iq() -> Element {
    pubsub::build_pubsub_items_iq(&id(), None, JOINED, Some(LIMIT))
}
pub fn parse(result: &Element, own: &BareJid) -> Option<Vec<WaddleSavedSpace>> {
    if result.attr("type") != Some("result")
        || result.attr("from").is_some_and(|from| from != own.as_str())
    {
        return None;
    }
    let mut seen = HashSet::new();
    Some(
        items(result, JOINED)?
            .into_iter()
            .filter_map(|item| {
                let entry = preference(item.payload("space", JOINED)?)?;
                (entry.address == item.id && seen.insert(item.id)).then_some(entry)
            })
            .take(LIMIT as usize)
            .collect(),
    )
}

fn save_iq(a: &Address, name: Option<&str>, follow: bool, subid: Option<&str>) -> Option<Element> {
    let node = a.node.as_deref()?;
    let mut space = Element::builder("space", JOINED)
        .attr(xml_ncname!("service").to_owned(), a.jid.as_str())
        .attr(xml_ncname!("node").to_owned(), node)
        .attr(xml_ncname!("rev").to_owned(), id());
    if let Some(name) = name.filter(|s| !s.is_empty()) {
        space = space.attr(xml_ncname!("name").to_owned(), name);
    }
    if follow {
        space = space.attr(xml_ncname!("follow").to_owned(), "true");
        if let Some(subid) = subid {
            space = space.attr(xml_ncname!("subid").to_owned(), subid);
        }
    }
    let mut options = pubsub::PubsubPublishOptions::default();
    options.persist_items = Some(true);
    options.access_model = Some(pubsub::PubsubAccessModel::Whitelist);
    Some(pubsub::build_pubsub_publish_iq(
        &id(),
        None,
        JOINED,
        Some(&a.text()),
        space.build(),
        Some(&options),
    ))
}

pub async fn update<E, F: Future<Output = Result<Element, E>>>(
    send: impl Fn(Element) -> F,
    own: &BareJid,
    entry: &WaddleSavedSpace,
    forget: bool,
) -> Result<(), Error<E>> {
    let a = Address::parse(&entry.address)
        .filter(|a| a.node.is_some())
        .ok_or(Error::Invalid)?;
    let iq = if forget {
        pubsub::build_pubsub_retract_iq(&id(), None, JOINED, &a.text())
    } else {
        save_iq(
            &a,
            entry.name.as_deref(),
            entry.follow,
            entry.subid.as_deref(),
        )
        .ok_or(Error::Invalid)?
    };
    request(&send, iq, own, true).await?;
    Ok(())
}
