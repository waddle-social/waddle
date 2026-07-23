use jid::{BareJid, Jid};
use minidom::Element;

const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_PUBSUB_ATTACHMENTS_SUMMARY: &str = "urn:xmpp:pubsub-attachments:summary:1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubEventItem {
    pub id: Option<String>,
    pub retracted: bool,
    pub payload: PubsubEventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubEvent {
    pub from: Option<Jid>,
    pub node: String,
    pub items: Vec<PubsubEventItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PubsubEventPayload {
    AttachmentSummary(AttachmentSummaryEvent),
    Opaque { element: Element },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSummaryEvent {
    pub reactions: Vec<ReactionSummaryEvent>,
    pub noticed_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionSummaryEvent {
    pub emoji: String,
    pub count: u32,
    pub reactors: Vec<BareJid>,
}

/// XEP-0060 §7.2.2.1 item-retract notification, flattened for
/// [`crate::event::ClientEvent::PubsubItemsRetracted`]: which items
/// disappeared from which node on which service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubRetractedItems {
    /// Notifying service (`from` of the event message); `None` when
    /// the stanza carried no `from` (the account's own PEP service).
    pub service: Option<Jid>,
    pub node: String,
    pub item_ids: Vec<String>,
}

/// One XEP-0470 attachment-summary update, flattened for
/// [`crate::event::ClientEvent::PubsubAttachmentSummary`]: the summary
/// counts for one attached-to item on a summary node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubAttachmentSummaryUpdate {
    /// Notifying service (`from` of the event message); `None` when
    /// the stanza carried no `from`.
    pub service: Option<Jid>,
    /// The summary node the notification arrived on
    /// (`urn:xmpp:pubsub-attachments:summary:1/<target-node>`).
    pub node: String,
    /// ItemID of the attached-to item the summary describes.
    pub item_id: Option<String>,
    pub summary: AttachmentSummaryEvent,
}

impl PubsubEvent {
    /// The §7.2.2.1 retractions in this event, or `None` when nothing
    /// was retracted.
    pub fn retracted_items(&self) -> Option<PubsubRetractedItems> {
        let item_ids: Vec<String> = self
            .items
            .iter()
            .filter(|item| item.retracted)
            .filter_map(|item| item.id.clone())
            .collect();
        if item_ids.is_empty() {
            return None;
        }
        Some(PubsubRetractedItems {
            service: self.from.clone(),
            node: self.node.clone(),
            item_ids,
        })
    }

    /// The XEP-0470 attachment-summary payloads in this event, one
    /// update per summarised item.
    pub fn attachment_summaries(&self) -> Vec<PubsubAttachmentSummaryUpdate> {
        self.items
            .iter()
            .filter_map(|item| {
                let PubsubEventPayload::AttachmentSummary(summary) = &item.payload else {
                    return None;
                };
                Some(PubsubAttachmentSummaryUpdate {
                    service: self.from.clone(),
                    node: self.node.clone(),
                    item_id: item.id.clone(),
                    summary: summary.clone(),
                })
            })
            .collect()
    }
}

pub fn parse_pubsub_events(message: &Element) -> Vec<PubsubEvent> {
    if message.name() != "message" {
        return Vec::new();
    }
    let Some(event) = message.get_child("event", NS_PUBSUB_EVENT) else {
        return Vec::new();
    };
    let from = message.attr("from").and_then(|value| value.parse().ok());
    event
        .children()
        .filter(|child| child.is("items", NS_PUBSUB_EVENT))
        .filter_map(|items| parse_items(items, from.clone()))
        .collect()
}

fn parse_items(items: &Element, from: Option<Jid>) -> Option<PubsubEvent> {
    let node = items.attr("node")?.to_owned();
    let event_items = items
        .children()
        .filter_map(|child| {
            if child.is("item", NS_PUBSUB_EVENT) {
                return Some(parse_item(child));
            }
            if child.is("retract", NS_PUBSUB_EVENT) {
                return Some(parse_retract(child));
            }
            None
        })
        .collect::<Vec<_>>();
    Some(PubsubEvent {
        from,
        node,
        items: event_items,
    })
}

fn parse_item(item: &Element) -> PubsubEventItem {
    let payload = item
        .children()
        .next()
        .map(parse_payload)
        .unwrap_or(PubsubEventPayload::Empty);
    PubsubEventItem {
        id: item.attr("id").map(ToOwned::to_owned),
        retracted: false,
        payload,
    }
}

fn parse_retract(retract: &Element) -> PubsubEventItem {
    PubsubEventItem {
        id: retract.attr("id").map(ToOwned::to_owned),
        retracted: true,
        payload: PubsubEventPayload::Empty,
    }
}

fn parse_payload(payload: &Element) -> PubsubEventPayload {
    if payload.is("summary", NS_PUBSUB_ATTACHMENTS_SUMMARY) {
        return PubsubEventPayload::AttachmentSummary(parse_attachment_summary(payload));
    }
    PubsubEventPayload::Opaque {
        element: payload.clone(),
    }
}

fn parse_attachment_summary(summary: &Element) -> AttachmentSummaryEvent {
    let reactions_parent = summary
        .get_child("reactions", NS_PUBSUB_ATTACHMENTS_SUMMARY)
        .unwrap_or(summary);
    let reactions = reactions_parent
        .children()
        .filter(|child| child.is("reaction", NS_PUBSUB_ATTACHMENTS_SUMMARY))
        .map(|reaction| ReactionSummaryEvent {
            emoji: reaction.text(),
            count: reaction
                .attr("count")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0),
            reactors: reaction
                .children()
                .filter(|child| child.is("reactor", NS_PUBSUB_ATTACHMENTS_SUMMARY))
                .filter_map(|reactor| reactor.attr("jid")?.parse::<BareJid>().ok())
                .collect(),
        })
        .collect();
    let noticed_count = summary
        .children()
        .find(|child| child.is("noticed", NS_PUBSUB_ATTACHMENTS_SUMMARY))
        .and_then(|noticed| noticed.attr("count"))
        .and_then(|count| count.parse::<u32>().ok());
    AttachmentSummaryEvent {
        reactions,
        noticed_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS_CLIENT: &str = "jabber:client";

    fn event_message(node: &str, item_id: &str, payload: Element) -> Element {
        Element::builder("message", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "headline")
            .append(
                Element::builder("event", NS_PUBSUB_EVENT)
                    .append(
                        Element::builder("items", NS_PUBSUB_EVENT)
                            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                            .append(
                                Element::builder("item", NS_PUBSUB_EVENT)
                                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                                    .append(payload)
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    #[test]
    fn parses_attachment_summary_event() {
        let message = event_message(
            "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
            "story-1",
            Element::builder("summary", NS_PUBSUB_ATTACHMENTS_SUMMARY)
                .append(
                    Element::builder("reactions", NS_PUBSUB_ATTACHMENTS_SUMMARY)
                        .append(
                            Element::builder("reaction", NS_PUBSUB_ATTACHMENTS_SUMMARY)
                                .attr(minidom::rxml::xml_ncname!("count").to_owned(), "2")
                                .append(
                                    Element::builder("reactor", NS_PUBSUB_ATTACHMENTS_SUMMARY)
                                        .attr(
                                            minidom::rxml::xml_ncname!("jid").to_owned(),
                                            "alice@example.com",
                                        )
                                        .build(),
                                )
                                .append("👍")
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("noticed", NS_PUBSUB_ATTACHMENTS_SUMMARY)
                        .attr(minidom::rxml::xml_ncname!("count").to_owned(), "1")
                        .build(),
                )
                .build(),
        );
        let events = parse_pubsub_events(&message);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].from, None);
        assert_eq!(events[0].items[0].id.as_deref(), Some("story-1"));
        let PubsubEventPayload::AttachmentSummary(summary) = &events[0].items[0].payload else {
            panic!("expected attachment summary payload");
        };
        assert_eq!(summary.reactions[0].emoji, "👍");
        assert_eq!(summary.reactions[0].count, 2);
        assert_eq!(
            summary.reactions[0].reactors,
            vec!["alice@example.com".parse::<BareJid>().expect("valid jid")]
        );
        assert_eq!(summary.noticed_count, Some(1));
    }

    #[test]
    fn preserves_unknown_payload_opaquely() {
        let message = event_message(
            "urn:example:unknown",
            "current",
            Element::builder("future", "urn:example:future")
                .append("payload")
                .build(),
        );
        let events = parse_pubsub_events(&message);
        let PubsubEventPayload::Opaque { element } = &events[0].items[0].payload else {
            panic!("expected opaque payload");
        };
        let xml = String::from(element);
        assert!(xml.contains("urn:example:future"), "{xml}");
        assert!(xml.contains("payload"), "{xml}");
    }

    #[test]
    fn parses_event_from_as_typed_jid() {
        let message = Element::builder("message", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "community.example.com",
            )
            .append(
                Element::builder("event", NS_PUBSUB_EVENT)
                    .append(
                        Element::builder("items", NS_PUBSUB_EVENT)
                            .attr(minidom::rxml::xml_ncname!("node").to_owned(), "urn:test")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let events = parse_pubsub_events(&message);
        assert_eq!(
            events[0].from,
            Some("community.example.com".parse::<Jid>().expect("valid jid"))
        );
    }

    #[test]
    fn parses_pubsub_retract_event_as_typed_item() {
        let message = Element::builder("message", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "community.example.com",
            )
            .append(
                Element::builder("event", NS_PUBSUB_EVENT)
                    .append(
                        Element::builder("items", NS_PUBSUB_EVENT)
                            .attr(
                                minidom::rxml::xml_ncname!("node").to_owned(),
                                "urn:xmpp:pubsub-social-feed:stories:0",
                            )
                            .append(
                                Element::builder("retract", NS_PUBSUB_EVENT)
                                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "story-1")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();

        let events = parse_pubsub_events(&message);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].node, "urn:xmpp:pubsub-social-feed:stories:0");
        assert_eq!(events[0].items[0].id.as_deref(), Some("story-1"));
        assert!(events[0].items[0].retracted);
        assert_eq!(events[0].items[0].payload, PubsubEventPayload::Empty);
    }

    #[test]
    fn flattens_retractions_into_typed_client_event_payload() {
        let event = PubsubEvent {
            from: Some("community.example.com".parse().expect("valid jid")),
            node: "urn:xmpp:pubsub-social-feed:stories:0".to_owned(),
            items: vec![
                PubsubEventItem {
                    id: Some("story-1".to_owned()),
                    retracted: true,
                    payload: PubsubEventPayload::Empty,
                },
                PubsubEventItem {
                    id: Some("story-2".to_owned()),
                    retracted: false,
                    payload: PubsubEventPayload::Empty,
                },
                PubsubEventItem {
                    id: Some("story-3".to_owned()),
                    retracted: true,
                    payload: PubsubEventPayload::Empty,
                },
            ],
        };
        let retracted = event.retracted_items().expect("retractions present");
        assert_eq!(
            retracted.service,
            Some("community.example.com".parse::<Jid>().expect("valid jid"))
        );
        assert_eq!(retracted.node, "urn:xmpp:pubsub-social-feed:stories:0");
        assert_eq!(retracted.item_ids, vec!["story-1", "story-3"]);
    }

    #[test]
    fn no_retractions_yields_no_client_event_payload() {
        let event = PubsubEvent {
            from: None,
            node: "urn:test".to_owned(),
            items: vec![PubsubEventItem {
                id: Some("kept".to_owned()),
                retracted: false,
                payload: PubsubEventPayload::Empty,
            }],
        };
        assert_eq!(event.retracted_items(), None);
    }

    #[test]
    fn flattens_attachment_summaries_into_typed_client_event_payloads() {
        let summary = AttachmentSummaryEvent {
            reactions: vec![ReactionSummaryEvent {
                emoji: "👍".to_owned(),
                count: 2,
                reactors: vec!["alice@example.com".parse().expect("valid jid")],
            }],
            noticed_count: Some(1),
        };
        let event = PubsubEvent {
            from: Some("community.example.com".parse().expect("valid jid")),
            node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0"
                .to_owned(),
            items: vec![
                PubsubEventItem {
                    id: Some("story-1".to_owned()),
                    retracted: false,
                    payload: PubsubEventPayload::AttachmentSummary(summary.clone()),
                },
                PubsubEventItem {
                    id: Some("opaque".to_owned()),
                    retracted: false,
                    payload: PubsubEventPayload::Opaque {
                        element: Element::builder("future", "urn:example:future").build(),
                    },
                },
            ],
        };
        let updates = event.attachment_summaries();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].item_id.as_deref(), Some("story-1"));
        assert_eq!(updates[0].summary, summary);
        assert_eq!(
            updates[0].node,
            "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0"
        );
    }
}
