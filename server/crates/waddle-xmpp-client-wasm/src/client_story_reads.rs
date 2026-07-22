//! Per-user story read state — wasm bridge surface.
//!
//! Stored as a single PEP item on the user's own JID per XEP-0223.
//! Node options are committed via `<publish-options>` precondition on
//! every publish; see `waddle-xmpp-core::waddle_story_reads` and the
//! design spec at
//! `docs/superpowers/specs/2026-05-19-stories-media-and-reads-design.md`.

use chrono::{DateTime, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result, PubsubAccessModel,
    PubsubPublishOptions, PubsubSendLastPublishedItem,
};
use waddle_xmpp_core::waddle_story_reads::{
    StoryId, StoryReads, NS_WADDLE_STORY_READS, PEP_ITEM_WADDLE_STORY_READS,
    PEP_NODE_WADDLE_STORY_READS,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, to_js_value, WaddleClient};

/// JS-facing entry (id, RFC 3339 timestamp). `StoryId` is unwrapped at
/// this boundary — the wasm/JS layer carries it as a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStoryRead {
    pub id: String,
    pub at: String,
}

/// JS-facing read-state payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct JsStoryReads {
    pub entries: Vec<JsStoryRead>,
}

impl From<&StoryReads> for JsStoryReads {
    fn from(reads: &StoryReads) -> Self {
        Self {
            entries: reads
                .iter()
                .map(|(id, at)| JsStoryRead {
                    id: id.as_str().to_owned(),
                    at: at.to_rfc3339(),
                })
                .collect(),
        }
    }
}

/// The `<publish-options>` precondition values. ALL FOUR fields are
/// mandatory — omitting any of them lets a server auto-create the node
/// with different defaults and silently leak read-state notifications.
fn story_reads_publish_options() -> PubsubPublishOptions {
    PubsubPublishOptions {
        persist_items: Some(true),
        access_model: Some(PubsubAccessModel::Whitelist),
        send_last_published_item: Some(PubsubSendLastPublishedItem::Never),
        max_items: Some(1),
    }
}

pub(crate) fn build_story_reads_publish_iq(reads: &StoryReads) -> Element {
    let id = format!("story-reads-publish-{}", Uuid::new_v4());
    build_pubsub_publish_iq(
        &id,
        None,
        PEP_NODE_WADDLE_STORY_READS,
        Some(PEP_ITEM_WADDLE_STORY_READS),
        reads.build_element(),
        Some(&story_reads_publish_options()),
    )
}

pub(crate) fn build_story_reads_fetch_iq() -> Element {
    let id = format!("story-reads-fetch-{}", Uuid::new_v4());
    build_pubsub_items_iq(&id, None, PEP_NODE_WADDLE_STORY_READS, Some(1))
}

/// Parse the items result returned by `build_story_reads_fetch_iq()`.
///
/// Returns an empty `StoryReads` if the node has no items or the result
/// shape doesn't match expectations (including items without an ItemID
/// — the read-state item is always published under the fixed `current`
/// id) — read state is non-critical, so silently degrading to "nothing
/// read" is better than surfacing an error the user can't act on.
pub(crate) fn parse_story_reads_fetch_result(iq: &Element) -> StoryReads {
    parse_pubsub_items_result(iq)
        .into_iter()
        .find_map(|item| {
            item.payload("reads", NS_WADDLE_STORY_READS)
                .and_then(|reads_el| StoryReads::parse(reads_el).ok())
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsStoryReadsInput {
    pub entries: Vec<JsStoryReadInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsStoryReadInput {
    pub id: String,
    pub at: String,
}

fn story_reads_from_js(input: JsStoryReadsInput) -> Result<StoryReads, JsValue> {
    let mut reads = StoryReads::default();
    for entry in input.entries {
        let id =
            StoryId::new(entry.id).map_err(|err| js_error(format!("invalid story id: {err}")))?;
        let at = entry
            .at
            .parse::<DateTime<Utc>>()
            .map_err(|_| js_error(format!("invalid timestamp: {}", entry.at)))?;
        reads.mark_read(id, at);
    }
    Ok(reads)
}

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest read-state from the user's own PEP node.
    ///
    /// Per XEP-0223 §Security Considerations (CVE-2023-28686), the
    /// result IQ's `from` attribute MUST be either absent or equal to
    /// the account's bare JID. Anything else is treated as spoofed
    /// and produces an empty result.
    pub fn story_reads_fetch(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let own_bare = {
                let jid_str = inner.borrow().config.jid.clone();
                let parsed: jid::Jid = jid_str
                    .parse()
                    .map_err(|err| js_error(format!("invalid own JID: {err}")))?;
                parsed.to_bare().to_string()
            };
            let iq = build_story_reads_fetch_iq();
            let result = match send_iq_command(inner, iq).await {
                Ok(r) => r,
                Err(_) => return to_js_value(&JsStoryReads::default()),
            };
            if let Some(from) = result.attr("from") {
                if from != own_bare {
                    return to_js_value(&JsStoryReads::default());
                }
            }
            let reads = parse_story_reads_fetch_result(&result);
            to_js_value(&JsStoryReads::from(&reads))
        })
    }

    /// Publish the user's read-state. Overwrites the single `current`
    /// item on every call.
    pub fn story_reads_publish(&self, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: JsStoryReadsInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid story_reads input: {err}")))?;
            let reads = story_reads_from_js(input)?;
            let iq = build_story_reads_publish_iq(&reads);
            send_iq_command(inner, iq).await?;
            to_js_value(&JsStoryReads::from(&reads))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
    const NS_PUBSUB_PUBLISH_OPTIONS: &str = "http://jabber.org/protocol/pubsub#publish-options";
    const NS_XDATA: &str = "jabber:x:data";

    #[test]
    fn publish_iq_carries_required_publish_options() {
        let reads = StoryReads::default();
        let iq = build_story_reads_publish_iq(&reads);
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub element");
        let options = pubsub
            .get_child("publish-options", NS_PUBSUB)
            .expect("publish-options element");
        let form = options.get_child("x", NS_XDATA).expect("data form");

        let field_value = |var: &str| -> Option<String> {
            form.children()
                .filter(|c| c.name() == "field" && c.ns() == NS_XDATA)
                .find(|c| c.attr("var") == Some(var))
                .and_then(|f| f.get_child("value", NS_XDATA))
                .map(|v| v.text())
        };

        assert_eq!(
            field_value("FORM_TYPE").as_deref(),
            Some(NS_PUBSUB_PUBLISH_OPTIONS)
        );
        assert_eq!(field_value("pubsub#persist_items").as_deref(), Some("true"));
        assert_eq!(
            field_value("pubsub#access_model").as_deref(),
            Some("whitelist")
        );
        assert_eq!(
            field_value("pubsub#send_last_published_item").as_deref(),
            Some("never")
        );
        assert_eq!(field_value("pubsub#max_items").as_deref(), Some("1"));
    }

    #[test]
    fn publish_iq_targets_correct_node_and_item() {
        let reads = StoryReads::default();
        let iq = build_story_reads_publish_iq(&reads);
        let publish = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("publish", NS_PUBSUB))
            .expect("publish element");
        assert_eq!(publish.attr("node"), Some(PEP_NODE_WADDLE_STORY_READS));
        let item = publish.get_child("item", NS_PUBSUB).expect("item");
        assert_eq!(item.attr("id"), Some(PEP_ITEM_WADDLE_STORY_READS));
    }

    #[test]
    fn fetch_iq_targets_correct_node() {
        let iq = build_story_reads_fetch_iq();
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("items", NS_PUBSUB))
            .expect("items element");
        assert_eq!(items.attr("node"), Some(PEP_NODE_WADDLE_STORY_READS));
        assert_eq!(items.attr("max_items"), Some("1"));
    }

    #[test]
    fn parse_fetch_handles_missing_pubsub() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='x'/>"
            .parse()
            .expect("valid");
        let reads = parse_story_reads_fetch_result(&iq);
        assert!(reads.is_empty());
    }

    #[test]
    fn parse_fetch_ignores_items_without_ids() {
        // The read-state item is always published under the fixed
        // `current` id; an id-less item is not ours and degrades to
        // "nothing read" rather than being trusted.
        let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
              <items node='urn:waddle:story:reads:0'>\
                <item>\
                  <reads xmlns='urn:waddle:story:reads:0'>\
                    <read id='story-a' at='2026-05-19T10:11:12Z'/>\
                  </reads>\
                </item>\
              </items>\
            </pubsub>\
          </iq>";
        let iq: Element = xml.parse().expect("valid");
        let reads = parse_story_reads_fetch_result(&iq);
        assert!(reads.is_empty());
    }

    #[test]
    fn parse_fetch_returns_entries() {
        let xml = "<iq xmlns='jabber:client' type='result' id='x'>\
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
              <items node='urn:waddle:story:reads:0'>\
                <item id='current'>\
                  <reads xmlns='urn:waddle:story:reads:0'>\
                    <read id='story-a' at='2026-05-19T10:11:12Z'/>\
                  </reads>\
                </item>\
              </items>\
            </pubsub>\
          </iq>";
        let iq: Element = xml.parse().expect("valid");
        let reads = parse_story_reads_fetch_result(&iq);
        assert_eq!(reads.len(), 1);
    }
}
