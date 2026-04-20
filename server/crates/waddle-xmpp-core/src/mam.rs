//! Shared Message Archive Management (MAM) primitives and helpers.
//!
//! These types and builders are safe to share across server and client code.

use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use minidom::Element;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Message, MessageType};

use crate::{CoreError, CoreResult};

/// MAM XML namespace (XEP-0313 v2).
pub const MAM_NS: &str = "urn:xmpp:mam:2";

/// Result Set Management namespace (XEP-0059).
pub const RSM_NS: &str = "http://jabber.org/protocol/rsm";

/// Data Forms namespace.
pub const DATA_FORMS_NS: &str = "jabber:x:data";

/// Stanza ID namespace (XEP-0359).
pub const STANZA_ID_NS: &str = "urn:xmpp:sid:0";

/// Forward namespace (XEP-0297).
pub const FORWARD_NS: &str = "urn:xmpp:forward:0";

/// Delay namespace (XEP-0203).
pub const DELAY_NS: &str = "urn:xmpp:delay";

const CLIENT_NS: &str = "jabber:client";
const REPLY_NS: &str = "urn:xmpp:reply:0";

/// Archived message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedMessage {
    /// Unique message ID.
    pub id: String,
    /// Timestamp when the message was received.
    pub timestamp: DateTime<Utc>,
    /// Sender JID.
    pub from: String,
    /// Recipient JID (room JID for MUC, or contact bare JID for 1:1).
    pub to: String,
    /// Message body.
    pub body: String,
    /// Original stanza ID (if present).
    pub stanza_id: Option<String>,
    /// RFC 6121 thread identifier for this message.
    pub thread_id: Option<String>,
    /// XEP-0461 reply target message ID.
    pub reply_to_id: Option<String>,
    /// XEP-0461 optional original sender JID.
    pub reply_to_jid: Option<String>,
    /// XEP-0359 origin-id supplied by client.
    pub origin_id: Option<String>,
    /// Message type ("chat", "groupchat", "normal", etc.).
    #[serde(default = "default_message_type")]
    pub message_type: String,
    /// Preserved full stanza XML for faithful replay of archived timeline events.
    pub stanza_xml: Option<String>,
}

fn default_message_type() -> String {
    "chat".to_string()
}

impl Default for ArchivedMessage {
    fn default() -> Self {
        Self {
            id: String::new(),
            timestamp: Utc::now(),
            from: String::new(),
            to: String::new(),
            body: String::new(),
            stanza_id: None,
            thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: default_message_type(),
            stanza_xml: None,
        }
    }
}

/// MAM query parameters.
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    /// Start time filter.
    pub start: Option<DateTime<Utc>>,
    /// End time filter.
    pub end: Option<DateTime<Utc>>,
    /// Filter by sender.
    pub with: Option<String>,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// Pagination: before this ID.
    pub before_id: Option<String>,
    /// Pagination: after this ID.
    pub after_id: Option<String>,
}

/// MAM query result.
#[derive(Debug, Clone)]
pub struct MamResult {
    /// Retrieved messages.
    pub messages: Vec<ArchivedMessage>,
    /// Whether there are more messages available.
    pub complete: bool,
    /// First message ID in the result set.
    pub first_id: Option<String>,
    /// Last message ID in the result set.
    pub last_id: Option<String>,
    /// Total count (if available).
    pub count: Option<u32>,
}

/// Parse a MAM query from an IQ stanza.
pub fn parse_mam_query(iq: &Iq) -> CoreResult<(String, MamQuery)> {
    let query_elem = match &iq.payload {
        IqType::Set(elem) | IqType::Get(elem) if elem.name() == "query" && elem.ns() == MAM_NS => {
            elem
        }
        IqType::Set(_) | IqType::Get(_) => {
            return Err(CoreError::bad_request(Some(
                "Missing MAM query element".to_string(),
            )));
        }
        _ => {
            return Err(CoreError::bad_request(Some(
                "Invalid IQ type for MAM query".to_string(),
            )));
        }
    };

    let query_id = query_elem
        .attr("queryid")
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    let mut mam_query = MamQuery::default();

    for child in query_elem.children() {
        if child.name() == "x" && child.ns() == DATA_FORMS_NS {
            parse_data_form(child, &mut mam_query)?;
        } else if child.name() == "set" && child.ns() == RSM_NS {
            parse_rsm(child, &mut mam_query)?;
        }
    }

    debug!(query_id = %query_id, query = ?mam_query, "Parsed MAM query");

    Ok((query_id, mam_query))
}

/// Check if an IQ is a MAM query.
pub fn is_mam_query(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Set(elem) | IqType::Get(elem)
            if elem.name() == "query" && elem.ns() == MAM_NS
    )
}

/// Build MAM result messages for each archived message.
pub fn build_result_messages(
    query_id: &str,
    to_jid: &str,
    messages: &[ArchivedMessage],
) -> Vec<Message> {
    messages
        .iter()
        .map(|archived| build_result_message(query_id, to_jid, archived))
        .collect()
}

/// Build the MAM fin (completion) IQ response.
pub fn build_fin_iq(original_iq: &Iq, result: &MamResult) -> Iq {
    let fin = Element::builder("fin", MAM_NS)
        .attr("complete", if result.complete { "true" } else { "false" })
        .append(build_rsm_response_element(result))
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(fin)),
    }
}

/// Add a stanza-id extension to a message for MAM compliance.
pub fn add_stanza_id(message: &mut Message, archive_id: &str, by: &str) {
    let stanza_id = Element::builder("stanza-id", STANZA_ID_NS)
        .attr("id", archive_id)
        .attr("by", by)
        .build();
    message.payloads.push(stanza_id);
}

fn parse_data_form(form: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for field in form.children() {
        if field.name() != "field" {
            continue;
        }

        let var = field.attr("var").unwrap_or("");
        let value = field
            .children()
            .find(|c| c.name() == "value")
            .map(|value| value.text());

        match var {
            "start" => {
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    query.start = Some(parse_datetime(&value)?);
                }
            }
            "end" => {
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    query.end = Some(parse_datetime(&value)?);
                }
            }
            "with" => {
                query.with = value.filter(|value| !value.is_empty());
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_rsm(rsm: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for child in rsm.children() {
        match child.name() {
            "max" => {
                let value = child.text();
                if !value.is_empty() {
                    query.max = Some(value.parse().map_err(|_| {
                        CoreError::bad_request(Some(format!("Invalid RSM max value: {}", value)))
                    })?);
                }
            }
            "after" => {
                let value = child.text();
                if !value.is_empty() {
                    query.after_id = Some(value);
                }
            }
            "before" => {
                query.before_id = Some(child.text());
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_datetime(value: &str) -> CoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|error| CoreError::bad_request(Some(format!("Invalid datetime: {}", error))))
}

fn build_result_message(query_id: &str, to_jid: &str, archived: &ArchivedMessage) -> Message {
    let inner_msg = archived_inner_message(archived);
    let delay = Element::builder("delay", DELAY_NS)
        .attr("stamp", archived.timestamp.to_rfc3339())
        .build();
    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(delay)
        .append(inner_msg)
        .build();
    let result = Element::builder("result", MAM_NS)
        .attr("queryid", query_id)
        .attr("id", &archived.id)
        .append(forwarded)
        .build();

    let mut msg = Message::new(Some(parse_message_jid(to_jid)));
    msg.id = Some(Uuid::now_v7().to_string());
    msg.type_ = MessageType::Normal;
    msg.payloads.push(result);
    msg
}

fn parse_message_jid(to_jid: &str) -> Jid {
    to_jid
        .parse()
        .unwrap_or_else(|_| Jid::from(BareJid::new("unknown").expect("valid fallback JID")))
}

fn archived_inner_message(archived: &ArchivedMessage) -> Element {
    if let Some(stanza_xml) = archived.stanza_xml.as_deref() {
        match stanza_xml.parse::<Element>() {
            Ok(element) => return element,
            Err(error) => {
                warn!(
                    archive_id = %archived.id,
                    error = %error,
                    "Failed to parse archived stanza XML"
                );
            }
        }
    }

    build_legacy_inner_message(archived)
}

fn build_legacy_inner_message(archived: &ArchivedMessage) -> Element {
    let msg_type = if archived.message_type.is_empty() {
        "chat"
    } else {
        archived.message_type.as_str()
    };

    let mut builder = Element::builder("message", CLIENT_NS)
        .attr("from", &archived.from)
        .attr("to", &archived.to)
        .attr("type", msg_type);

    if let Some(stanza_id) = archived.stanza_id.as_deref() {
        builder = builder.attr("id", stanza_id);
    }
    if !archived.body.is_empty() {
        builder = builder.append(
            Element::builder("body", CLIENT_NS)
                .append(archived.body.clone())
                .build(),
        );
    }
    if let Some(thread_id) = archived.thread_id.as_deref() {
        builder = builder.append(
            Element::builder("thread", CLIENT_NS)
                .append(thread_id)
                .build(),
        );
    }
    if let Some(reply_to_id) = archived.reply_to_id.as_deref() {
        let mut reply = Element::builder("reply", REPLY_NS).attr("id", reply_to_id);
        if let Some(reply_to_jid) = archived.reply_to_jid.as_deref() {
            reply = reply.attr("to", reply_to_jid);
        }
        builder = builder.append(reply.build());
    }
    if let Some(origin_id) = archived.origin_id.as_deref() {
        builder = builder.append(
            Element::builder("origin-id", STANZA_ID_NS)
                .attr("id", origin_id)
                .build(),
        );
    }

    builder.build()
}

fn build_rsm_response_element(result: &MamResult) -> Element {
    let mut builder = Element::builder("set", RSM_NS);

    if let Some(first) = result.first_id.as_deref() {
        builder = builder.append(Element::builder("first", RSM_NS).append(first).build());
    }
    if let Some(last) = result.last_id.as_deref() {
        builder = builder.append(Element::builder("last", RSM_NS).append(last).build());
    }
    if let Some(count) = result.count {
        builder = builder.append(
            Element::builder("count", RSM_NS)
                .append(count.to_string())
                .build(),
        );
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn parses_mam_query_with_form_and_rsm() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-1".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .attr("queryid", "query-1")
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "submit")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "start")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("2024-01-15T10:30:00Z")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "with")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("juliet@example.com")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("set", RSM_NS)
                            .append(Element::builder("max", RSM_NS).append("10").build())
                            .append(Element::builder("after", RSM_NS).append("msg-9").build())
                            .build(),
                    )
                    .build(),
            ),
        };

        let (query_id, query) = parse_mam_query(&iq).expect("valid MAM query");

        assert_eq!(query_id, "query-1");
        assert_eq!(query.max, Some(10));
        assert_eq!(query.after_id.as_deref(), Some("msg-9"));
        assert_eq!(query.with.as_deref(), Some("juliet@example.com"));
        let start = query.start.expect("start filter");
        assert_eq!(start.year(), 2024);
        assert_eq!(start.month(), 1);
        assert_eq!(start.day(), 15);
    }

    #[test]
    fn parses_last_page_rsm_before() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-2".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("set", RSM_NS)
                            .append(Element::builder("before", RSM_NS).build())
                            .build(),
                    )
                    .build(),
            ),
        };

        let (_, query) = parse_mam_query(&iq).expect("valid MAM query");

        assert_eq!(query.before_id, Some(String::new()));
    }

    #[test]
    fn rejects_invalid_datetime() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-3".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "start")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("not-a-date")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
        };

        let err = parse_mam_query(&iq).expect_err("invalid MAM query");
        assert!(matches!(err, CoreError::BadRequest(_)));
    }

    #[test]
    fn builds_result_message_from_legacy_fields() {
        let archived = ArchivedMessage {
            id: "msg-123".to_string(),
            timestamp: Utc::now(),
            from: "user@example.com/nick".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Hello, world!".to_string(),
            thread_id: Some("thread-1".to_string()),
            reply_to_id: Some("parent-1".to_string()),
            reply_to_jid: Some("alice@example.com".to_string()),
            origin_id: Some("origin-1".to_string()),
            ..Default::default()
        };

        let msg = build_result_messages("query-1", "user@example.com", &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");

        assert!(inner_msg.children().any(|c| c.name() == "thread"));
        assert!(inner_msg
            .children()
            .any(|c| c.name() == "reply" && c.ns() == REPLY_NS));
        assert!(inner_msg
            .children()
            .any(|c| c.name() == "origin-id" && c.ns() == STANZA_ID_NS));
    }

    #[test]
    fn preserves_archived_stanza_payload() {
        let archived = ArchivedMessage {
            id: "msg-124".to_string(),
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: String::new(),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='reaction-1'><reactions xmlns='urn:xmpp:reactions:0' id='msg-1'><reaction>👍</reaction></reactions></message>".to_string(),
            ),
            ..Default::default()
        };

        let msg = build_result_messages("query-2", "user@example.com", &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");
        let reactions = inner_msg
            .children()
            .find(|c| c.name() == "reactions" && c.ns() == "urn:xmpp:reactions:0")
            .expect("reactions payload");

        assert_eq!(inner_msg.attr("id"), Some("reaction-1"));
        assert_eq!(reactions.attr("id"), Some("msg-1"));
    }

    #[test]
    fn builds_fin_iq_with_rsm_metadata() {
        let original = Iq {
            from: Some(parse_message_jid("romeo@example.com/orchard")),
            to: Some(parse_message_jid("juliet@example.com/balcony")),
            id: "iq-1".to_string(),
            payload: IqType::Get(Element::builder("query", MAM_NS).build()),
        };
        let result = MamResult {
            messages: Vec::new(),
            complete: true,
            first_id: Some("msg-1".to_string()),
            last_id: Some("msg-2".to_string()),
            count: Some(2),
        };

        let fin = build_fin_iq(&original, &result);
        let payload = match fin.payload {
            IqType::Result(Some(payload)) => payload,
            other => panic!("unexpected fin payload: {:?}", other),
        };
        let set = payload
            .children()
            .find(|child| child.name() == "set" && child.ns() == RSM_NS)
            .expect("rsm set");

        assert_eq!(payload.attr("complete"), Some("true"));
        assert_eq!(
            set.get_child("first", RSM_NS).map(|child| child.text()),
            Some("msg-1".to_string())
        );
        assert_eq!(
            set.get_child("last", RSM_NS).map(|child| child.text()),
            Some("msg-2".to_string())
        );
        assert_eq!(
            set.get_child("count", RSM_NS).map(|child| child.text()),
            Some("2".to_string())
        );
    }

    #[test]
    fn adds_stanza_id_payload() {
        let mut message = Message::new(Some(parse_message_jid("juliet@example.com")));

        add_stanza_id(&mut message, "archive-1", "room@example.com");

        let stanza_id = message
            .payloads
            .iter()
            .find(|payload| payload.name() == "stanza-id" && payload.ns() == STANZA_ID_NS)
            .expect("stanza-id payload");
        assert_eq!(stanza_id.attr("id"), Some("archive-1"));
        assert_eq!(stanza_id.attr("by"), Some("room@example.com"));
    }
}
