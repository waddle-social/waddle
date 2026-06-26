//! Cross-device manual-status sync — wasm bridge surface (ADR-010
//! Phase 4).
//!
//! The user's picked presence mode is stored as a single PEP item
//! (`id = current`) on their own JID per XEP-0223, in the Waddle-custom
//! `urn:waddle:status-preference:0` namespace. The server pins the
//! node's privacy (owner-only, `whitelist`) via its well-known-node
//! defaults — mirroring `urn:waddle:dnd:0` — so no `<publish-options>`
//! precondition is sent here (contrast `client_story_reads`, whose node
//! is not well-known and must pin its own config on every publish).
//!
//! Live cross-device adoption rides the generic incoming-pubsub-event
//! path (`on_pubsub_event`): a peer-resource publish arrives as an
//! opaque `<status-preference/>` and is parsed by the chat layer, the
//! same shape the XEP-0108 in-call overlay uses.

use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_core::waddle_status_preference::{
    StatusPreference, NS_WADDLE_STATUS_PREFERENCE, PEP_ITEM_WADDLE_STATUS_PREFERENCE,
    PEP_NODE_WADDLE_STATUS_PREFERENCE,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, to_js_value, WaddleClient};
use crate::NS_CLIENT;

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// JS-facing status preference: `{ mode, status? }`, mapping 1:1 to the
/// chat client's `PresenceMode` (`automatic` | `manual` + status).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStatusPreference {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl From<StatusPreference> for JsStatusPreference {
    fn from(pref: StatusPreference) -> Self {
        Self {
            mode: pref.mode_token().to_string(),
            status: pref.status_token().map(ToOwned::to_owned),
        }
    }
}

fn status_preference_from_js(input: JsStatusPreference) -> Result<StatusPreference, JsValue> {
    StatusPreference::from_tokens(&input.mode, input.status.as_deref())
        .map_err(|err| js_error(format!("invalid status preference: {err}")))
}

pub(crate) fn build_status_preference_publish_iq(pref: &StatusPreference) -> Element {
    let id = format!("status-preference-publish-{}", Uuid::new_v4());
    let item = Element::builder("item", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            PEP_ITEM_WADDLE_STATUS_PREFERENCE,
        )
        .append(pref.build_element())
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            PEP_NODE_WADDLE_STATUS_PREFERENCE,
        )
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(pubsub)
        .build()
}

pub(crate) fn build_status_preference_fetch_iq() -> Element {
    let id = format!("status-preference-fetch-{}", Uuid::new_v4());
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            PEP_NODE_WADDLE_STATUS_PREFERENCE,
        )
        .attr(minidom::rxml::xml_ncname!("max_items").to_owned(), "1")
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(pubsub)
        .build()
}

/// Parse the items result from [`build_status_preference_fetch_iq`].
///
/// Returns `None` when the node has no item, the result shape doesn't
/// match, or the payload is malformed — "no preference" degrades to
/// Automatic at the chat layer, so a hard error here would be worse
/// than absence.
pub(crate) fn parse_status_preference_fetch_result(iq: &Element) -> Option<StatusPreference> {
    let pubsub = iq.get_child("pubsub", NS_PUBSUB)?;
    let items = pubsub.get_child("items", NS_PUBSUB)?;
    items
        .children()
        .filter(|el| el.name() == "item" && el.ns() == NS_PUBSUB)
        .find_map(|item| {
            item.children()
                .find(|c| c.name() == "status-preference" && c.ns() == NS_WADDLE_STATUS_PREFERENCE)
                .and_then(|el| StatusPreference::parse(el).ok())
        })
}

#[wasm_bindgen]
impl WaddleClient {
    /// Publish the user's picked presence mode. Overwrites the single
    /// `current` item on every call (reset publishes `mode='automatic'`
    /// rather than retracting, so the change fans out to other devices).
    pub fn status_preference_publish(&self, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: JsStatusPreference = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid status_preference input: {err}")))?;
            let pref = status_preference_from_js(input)?;
            let iq = build_status_preference_publish_iq(&pref);
            send_iq_command(inner, iq).await?;
            to_js_value(&JsStatusPreference::from(pref))
        })
    }

    /// Fetch the user's stored preference from their own PEP node.
    /// Resolves to `null` when nothing is stored.
    ///
    /// Per XEP-0223 §Security Considerations (CVE-2023-28686), the
    /// result IQ's `from` MUST be absent or the account's bare JID;
    /// anything else is treated as spoofed and yields `null`.
    pub fn status_preference_fetch(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let own_bare = {
                let jid_str = inner.borrow().config.jid.clone();
                let parsed: jid::Jid = jid_str
                    .parse()
                    .map_err(|err| js_error(format!("invalid own JID: {err}")))?;
                parsed.to_bare().to_string()
            };
            let iq = build_status_preference_fetch_iq();
            let result = match send_iq_command(inner, iq).await {
                Ok(r) => r,
                Err(_) => return Ok(JsValue::NULL),
            };
            if let Some(from) = result.attr("from") {
                if from != own_bare {
                    return Ok(JsValue::NULL);
                }
            }
            match parse_status_preference_fetch_result(&result) {
                Some(pref) => to_js_value(&JsStatusPreference::from(pref)),
                None => Ok(JsValue::NULL),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_core::waddle_status_preference::ManualStatus;

    #[test]
    fn publish_iq_targets_node_and_current_item() {
        let iq = build_status_preference_publish_iq(&StatusPreference::Manual(ManualStatus::Away));
        let publish = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("publish", NS_PUBSUB))
            .expect("publish element");
        assert_eq!(
            publish.attr("node"),
            Some(PEP_NODE_WADDLE_STATUS_PREFERENCE)
        );
        let item = publish.get_child("item", NS_PUBSUB).expect("item");
        assert_eq!(item.attr("id"), Some(PEP_ITEM_WADDLE_STATUS_PREFERENCE));
        let payload = item
            .get_child("status-preference", NS_WADDLE_STATUS_PREFERENCE)
            .expect("payload");
        assert_eq!(payload.attr("mode"), Some("manual"));
        assert_eq!(payload.attr("status"), Some("away"));
    }

    #[test]
    fn publish_iq_omits_publish_options() {
        // The node is server-well-known, so the client does NOT pin its
        // own config (contrast story-reads). A stray `<publish-options>`
        // would needlessly risk a precondition mismatch.
        let iq = build_status_preference_publish_iq(&StatusPreference::Automatic);
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
        assert!(pubsub.get_child("publish-options", NS_PUBSUB).is_none());
    }

    #[test]
    fn fetch_iq_targets_node() {
        let iq = build_status_preference_fetch_iq();
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("items", NS_PUBSUB))
            .expect("items element");
        assert_eq!(items.attr("node"), Some(PEP_NODE_WADDLE_STATUS_PREFERENCE));
        assert_eq!(items.attr("max_items"), Some("1"));
    }

    #[test]
    fn parse_fetch_returns_manual_status() {
        let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
              <items node='urn:waddle:status-preference:0'>\
                <item id='current'>\
                  <status-preference xmlns='urn:waddle:status-preference:0' mode='manual' status='dnd'/>\
                </item>\
              </items>\
            </pubsub>\
          </iq>";
        let iq: Element = xml.parse().expect("valid");
        assert_eq!(
            parse_status_preference_fetch_result(&iq),
            Some(StatusPreference::Manual(ManualStatus::Dnd))
        );
    }

    #[test]
    fn parse_fetch_returns_none_when_empty() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='x'/>"
            .parse()
            .expect("valid");
        assert_eq!(parse_status_preference_fetch_result(&iq), None);
    }
}
