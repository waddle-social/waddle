//! MAM query parsing and response formatting.
//!
//! Handles XEP-0313 IQ stanzas for querying message archives.
//! Supports Result Set Management (RSM) for pagination per XEP-0059.

use chrono::{DateTime, Utc};
use minidom::Element;
use tracing::debug;
use uuid::Uuid;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};

use super::{ArchivedMessage, MamQuery, MamResult};
use crate::XmppError;

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

/// Parse a MAM query from an IQ stanza.
///
/// Expected format:
/// ```xml
/// <iq type='set' id='query1'>
///   <query xmlns='urn:xmpp:mam:2' queryid='f27'>
///     <x xmlns='jabber:x:data' type='submit'>
///       <field var='FORM_TYPE' type='hidden'>
///         <value>urn:xmpp:mam:2</value>
///       </field>
///       <field var='start'><value>2010-06-07T00:00:00Z</value></field>
///       <field var='end'><value>2010-07-07T13:23:54Z</value></field>
///       <field var='with'><value>juliet@capulet.lit</value></field>
///     </x>
///     <set xmlns='http://jabber.org/protocol/rsm'>
///       <max>10</max>
///       <after>28482-98726-73623</after>
///     </set>
///   </query>
/// </iq>
/// ```
pub fn parse_mam_query(iq: &Iq) -> Result<(String, MamQuery), XmppError> {
    // Find the query element from IqType
    let query_elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) | xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "query" && elem.ns() == MAM_NS {
                elem
            } else {
                return Err(XmppError::bad_request(Some("Missing MAM query element".to_string())));
            }
        }
        _ => return Err(XmppError::bad_request(Some("Invalid IQ type for MAM query".to_string()))),
    };

    // Get the query ID (optional but recommended)
    let query_id = query_elem
        .attr("queryid")
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    let mut mam_query = MamQuery::default();

    // Parse data form fields
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

/// Parse data form fields for MAM filtering.
fn parse_data_form(form: &Element, query: &mut MamQuery) -> Result<(), XmppError> {
    for field in form.children() {
        if field.name() != "field" {
            continue;
        }

        let var = field.attr("var").unwrap_or("");
        // Element::text() returns String directly, not Option<String>
        let value = field
            .children()
            .find(|c| c.name() == "value")
            .map(|v| v.text());

        match var {
            "start" => {
                if let Some(v) = value {
                    if !v.is_empty() {
                        query.start = Some(parse_datetime(&v)?);
                    }
                }
            }
            "end" => {
                if let Some(v) = value {
                    if !v.is_empty() {
                        query.end = Some(parse_datetime(&v)?);
                    }
                }
            }
            "with" => {
                query.with = value.filter(|v| !v.is_empty());
            }
            _ => {} // Ignore unknown fields
        }
    }

    Ok(())
}

/// Parse RSM pagination parameters.
fn parse_rsm(rsm: &Element, query: &mut MamQuery) -> Result<(), XmppError> {
    for child in rsm.children() {
        // Element::text() returns String directly
        match child.name() {
            "max" => {
                let text = child.text();
                if !text.is_empty() {
                    query.max = text.parse().ok();
                }
            }
            "after" => {
                let text = child.text();
                if !text.is_empty() {
                    query.after_id = Some(text);
                }
            }
            "before" => {
                // Empty <before/> means "get last page"
                let text = child.text();
                query.before_id = Some(text);
            }
            _ => {} // Ignore unknown elements
        }
    }

    Ok(())
}

/// Parse an ISO 8601 / RFC 3339 datetime string.
fn parse_datetime(s: &str) -> Result<DateTime<Utc>, XmppError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| XmppError::bad_request(Some(format!("Invalid datetime: {}", e))))
}

/// Check if an IQ is a MAM query.
pub fn is_mam_query(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) | xmpp_parsers::iq::IqType::Get(elem) => {
            elem.name() == "query" && elem.ns() == MAM_NS
        }
        _ => false,
    }
}

/// Build MAM result messages for each archived message.
///
/// Each message is wrapped in:
/// ```xml
/// <message id='aeb213' to='juliet@capulet.lit/chamber'>
///   <result xmlns='urn:xmpp:mam:2' queryid='f27' id='28482-98726-73623'>
///     <forwarded xmlns='urn:xmpp:forward:0'>
///       <delay xmlns='urn:xmpp:delay' stamp='2010-07-10T23:08:25Z'/>
///       <message xmlns='jabber:client' from='romeo@montague.lit/orchard'
///                to='juliet@capulet.lit/balcony' type='chat'>
///         <body>Call me but love, and I'll be new baptized;</body>
///       </message>
///     </forwarded>
///   </result>
/// </message>
/// ```
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

/// Build a single MAM result message.
fn build_result_message(query_id: &str, to_jid: &str, archived: &ArchivedMessage) -> Message {
    // Build the inner message element
    let inner_msg = Element::builder("message", "jabber:client")
        .attr("from", &archived.from)
        .attr("to", &archived.to)
        .attr("type", "groupchat")
        .append(Element::builder("body", "jabber:client").append(archived.body.clone()))
        .build();

    // Build the delay element
    let delay = Element::builder("delay", DELAY_NS)
        .attr("stamp", archived.timestamp.to_rfc3339())
        .build();

    // Build the forwarded wrapper
    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(delay)
        .append(inner_msg)
        .build();

    // Build the result wrapper
    let result = Element::builder("result", MAM_NS)
        .attr("queryid", query_id)
        .attr("id", &archived.id)
        .append(forwarded)
        .build();

    // Build the outer message
    let msg_id = Uuid::now_v7().to_string();
    let mut msg = Message::new(Some(to_jid.parse().unwrap_or_else(|_| {
        jid::Jid::from(jid::BareJid::new("unknown").unwrap())
    })));
    msg.id = Some(msg_id);
    msg.type_ = MessageType::Normal;
    msg.payloads.push(result);

    msg
}

/// Build the MAM fin (completion) IQ response.
///
/// ```xml
/// <iq type='result' id='query1'>
///   <fin xmlns='urn:xmpp:mam:2' complete='true'>
///     <set xmlns='http://jabber.org/protocol/rsm'>
///       <first index='0'>28482-98726-73623</first>
///       <last>09af3-cc343-b409f</last>
///       <count>5</count>
///     </set>
///   </fin>
/// </iq>
/// ```
pub fn build_fin_iq(
    original_iq: &Iq,
    result: &MamResult,
) -> Iq {
    let mut set_builder = Element::builder("set", RSM_NS);

    if let Some(ref first) = result.first_id {
        let first_elem = Element::builder("first", RSM_NS)
            .attr("index", "0")
            .append(first.clone())
            .build();
        set_builder = set_builder.append(first_elem);
    }

    if let Some(ref last) = result.last_id {
        let last_elem = Element::builder("last", RSM_NS)
            .append(last.clone())
            .build();
        set_builder = set_builder.append(last_elem);
    }

    if let Some(count) = result.count {
        let count_elem = Element::builder("count", RSM_NS)
            .append(count.to_string())
            .build();
        set_builder = set_builder.append(count_elem);
    }

    let fin = Element::builder("fin", MAM_NS)
        .attr("complete", if result.complete { "true" } else { "false" })
        .append(set_builder.build())
        .build();

    // Build the result IQ
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(fin)),
    }
}

/// Add a stanza-id extension to a message for MAM compliance.
///
/// Per XEP-0359, the server should add a stanza-id to messages it archives:
/// ```xml
/// <stanza-id xmlns='urn:xmpp:sid:0' id='archive-id' by='room@conference.example.com'/>
/// ```
pub fn add_stanza_id(message: &mut Message, archive_id: &str, by: &str) {
    let stanza_id = Element::builder("stanza-id", STANZA_ID_NS)
        .attr("id", archive_id)
        .attr("by", by)
        .build();
    message.payloads.push(stanza_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn test_is_mam_query() {
        let query_elem = Element::builder("query", MAM_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        assert!(is_mam_query(&iq));
    }

    #[test]
    fn test_is_not_mam_query() {
        let other_elem = Element::builder("other", "some:other:ns").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(other_elem),
        };

        assert!(!is_mam_query(&iq));
    }

    #[test]
    fn test_parse_datetime() {
        let dt = parse_datetime("2024-01-15T10:30:00Z").unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn test_build_result_message() {
        let archived = ArchivedMessage {
            id: "msg-123".to_string(),
            timestamp: Utc::now(),
            from: "user@example.com/nick".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Hello, world!".to_string(),
            stanza_id: None,
        };

        let msg = build_result_message("query-1", "user@example.com", &archived);
        assert!(msg.payloads.iter().any(|p| p.name() == "result" && p.ns() == MAM_NS));
    }
}
