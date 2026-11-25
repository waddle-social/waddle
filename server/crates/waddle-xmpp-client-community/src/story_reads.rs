use crate::{invalid_input, InvalidInput};
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
/// JS-facing entry (id, RFC 3339 timestamp). `StoryId` is unwrapped at
/// this boundary — the wasm/JS layer carries it as a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleStoryRead {
    pub id: String,
    pub at: String,
}

/// JS-facing read-state payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaddleStoryReads {
    pub entries: Vec<WaddleStoryRead>,
}

impl From<&StoryReads> for WaddleStoryReads {
    fn from(reads: &StoryReads) -> Self {
        Self {
            entries: reads
                .iter()
                .map(|(id, at)| WaddleStoryRead {
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
    {
        let mut outbound_options = PubsubPublishOptions::default();
        outbound_options.persist_items = Some(true);
        outbound_options.access_model = Some(PubsubAccessModel::Whitelist);
        outbound_options.send_last_published_item = Some(PubsubSendLastPublishedItem::Never);
        outbound_options.max_items = Some(1);
        outbound_options
    }
}

pub fn build_story_reads_publish_iq(reads: &StoryReads) -> Element {
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

pub fn build_story_reads_fetch_iq() -> Element {
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
pub fn parse_story_reads_fetch_result(iq: &Element) -> StoryReads {
    parse_pubsub_items_result(iq)
        .into_iter()
        .find_map(|item| {
            item.payload("reads", NS_WADDLE_STORY_READS)
                .and_then(|reads_el| StoryReads::parse(reads_el).ok())
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleStoryReadsInput {
    pub entries: Vec<WaddleStoryReadInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleStoryReadInput {
    pub id: String,
    pub at: String,
}

pub fn story_reads_from_input(input: WaddleStoryReadsInput) -> Result<StoryReads, InvalidInput> {
    let mut reads = StoryReads::default();
    for entry in input.entries {
        let id = StoryId::try_new(entry.id)
            .map_err(|err| invalid_input(format!("invalid story id: {err}")))?;
        let at = entry
            .at
            .parse::<DateTime<Utc>>()
            .map_err(|_| invalid_input(format!("invalid timestamp: {}", entry.at)))?;
        reads.mark_read(id, at);
    }
    Ok(reads)
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
