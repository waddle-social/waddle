//! XEP-0202: Entity Time
//!
//! Provides helpers for detecting entity time queries and building responses.
//! Allows clients to query the server's current UTC time and local timezone offset.
//!
//! ## XML Format
//!
//! Query:
//! ```xml
//! <iq type='get' to='example.com' id='time-1'>
//!   <time xmlns='urn:xmpp:time'/>
//! </iq>
//! ```
//!
//! Response:
//! ```xml
//! <iq type='result' from='example.com' id='time-1'>
//!   <time xmlns='urn:xmpp:time'>
//!     <tzo>+00:00</tzo>
//!     <utc>2024-06-01T12:00:00Z</utc>
//!   </time>
//! </iq>
//! ```
//!
//! ## Service Discovery
//!
//! The server advertises `urn:xmpp:time` as a feature in disco#info.

use chrono::{DateTime, FixedOffset, Local, Offset, SecondsFormat, Utc};
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0202 Entity Time.
pub const NS_TIME: &str = "urn:xmpp:time";

/// Parsed entity time payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityTime {
    pub utc: DateTime<Utc>,
    pub tzo: FixedOffset,
}

impl EntityTime {
    /// Capture the current UTC timestamp and the host's local timezone offset.
    pub fn now() -> Self {
        let local = Local::now();
        Self {
            utc: local.with_timezone(&Utc),
            tzo: local.offset().fix(),
        }
    }
}

/// Check if an IQ stanza is an entity time query.
pub fn is_time_query(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem) if elem.name() == "time" && elem.ns() == NS_TIME)
}

fn format_time_zone_offset(offset: FixedOffset) -> String {
    let total_seconds = offset.local_minus_utc();
    let sign = if total_seconds < 0 { '-' } else { '+' };
    let absolute_seconds = total_seconds.abs();
    let hours = absolute_seconds / 3600;
    let minutes = (absolute_seconds % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

fn parse_time_zone_offset(value: &str) -> Option<FixedOffset> {
    let bytes = value.as_bytes();
    if bytes.len() != 6 || bytes[3] != b':' {
        return None;
    }

    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };

    let hours = value[1..3].parse::<i32>().ok()?;
    let minutes = value[4..6].parse::<i32>().ok()?;
    if hours > 15 || minutes > 59 {
        return None;
    }

    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

/// Build an entity time response IQ.
pub fn build_time_response(original_iq: &Iq, entity_time: &EntityTime) -> Iq {
    let time_elem = Element::builder("time", NS_TIME)
        .append(
            Element::builder("tzo", NS_TIME)
                .append(format_time_zone_offset(entity_time.tzo))
                .build(),
        )
        .append(
            Element::builder("utc", NS_TIME)
                .append(entity_time.utc.to_rfc3339_opts(SecondsFormat::Secs, true))
                .build(),
        )
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(time_elem)),
    }
}

/// Build an entity time response with the current UTC time and host timezone offset.
pub fn build_current_time_response(original_iq: &Iq) -> Iq {
    build_time_response(original_iq, &EntityTime::now())
}

/// Parse a time response into typed UTC and timezone offset values.
pub fn parse_time_response(iq: &Iq) -> Option<EntityTime> {
    let elem = match &iq.payload {
        IqType::Result(Some(elem)) if elem.name() == "time" && elem.ns() == NS_TIME => elem,
        _ => return None,
    };

    let tzo = elem
        .children()
        .find(|c| c.is("tzo", NS_TIME))
        .map(|c| c.text())
        .and_then(|value| parse_time_zone_offset(&value))?;

    let utc = elem
        .children()
        .find(|c| c.is("utc", NS_TIME))
        .map(|c| c.text())
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())?;
    if utc.offset().local_minus_utc() != 0 {
        return None;
    }

    Some(EntityTime {
        utc: utc.with_timezone(&Utc),
        tzo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_time_query() -> Iq {
        let time_elem = Element::builder("time", NS_TIME).build();
        Iq {
            from: Some("alice@example.com".parse().expect("valid jid")),
            to: Some("example.com".parse().expect("valid jid")),
            id: "time-1".to_string(),
            payload: IqType::Get(time_elem),
        }
    }

    fn sample_time(offset_seconds: i32) -> EntityTime {
        EntityTime {
            utc: Utc
                .with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
                .single()
                .expect("valid test date"),
            tzo: FixedOffset::east_opt(offset_seconds).expect("valid offset"),
        }
    }

    #[test]
    fn test_is_time_query() {
        let iq = make_time_query();
        assert!(is_time_query(&iq));
    }

    #[test]
    fn test_is_time_query_false_for_other_ns() {
        let other = Element::builder("time", "jabber:client").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "t-1".to_string(),
            payload: IqType::Get(other),
        };
        assert!(!is_time_query(&iq));
    }

    #[test]
    fn test_is_time_query_false_for_set() {
        let time_elem = Element::builder("time", NS_TIME).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "t-2".to_string(),
            payload: IqType::Set(time_elem),
        };
        assert!(!is_time_query(&iq));
    }

    #[test]
    fn test_build_time_response() {
        let query = make_time_query();
        let result = build_time_response(&query, &sample_time(0));

        assert_eq!(result.id, "time-1");
        assert_eq!(result.from, query.to);
        assert_eq!(result.to, query.from);

        if let IqType::Result(Some(elem)) = &result.payload {
            assert_eq!(elem.name(), "time");
            assert_eq!(elem.ns(), NS_TIME);

            let tzo = elem
                .children()
                .find(|c| c.is("tzo", NS_TIME))
                .expect("tzo present");
            assert_eq!(tzo.text(), "+00:00");

            let utc = elem
                .children()
                .find(|c| c.is("utc", NS_TIME))
                .expect("utc present");
            assert_eq!(utc.text(), "2024-06-01T12:00:00Z");
        } else {
            panic!("Expected Result with payload");
        }
    }

    #[test]
    fn test_build_time_response_with_offset() {
        let query = make_time_query();
        let result = build_time_response(&query, &sample_time(-6 * 3600));

        if let IqType::Result(Some(elem)) = &result.payload {
            let tzo = elem
                .children()
                .find(|c| c.is("tzo", NS_TIME))
                .expect("tzo present");
            assert_eq!(tzo.text(), "-06:00");
        } else {
            panic!("Expected Result with payload");
        }
    }

    #[test]
    fn test_build_current_time_response() {
        let query = make_time_query();
        let result = build_current_time_response(&query);

        assert_eq!(result.id, "time-1");
        let parsed = parse_time_response(&result).expect("parseable current entity time");
        assert_eq!(parsed.tzo, EntityTime::now().tzo);
    }

    #[test]
    fn test_parse_time_response() {
        let query = make_time_query();
        let result = build_time_response(&query, &sample_time(-5 * 3600));

        let parsed = parse_time_response(&result).expect("parseable");
        assert_eq!(parsed, sample_time(-5 * 3600));
    }

    #[test]
    fn test_parse_time_response_not_result() {
        let query = make_time_query();
        assert!(parse_time_response(&query).is_none());
    }

    #[test]
    fn test_parse_time_response_rejects_invalid_tzo() {
        let iq = Iq {
            from: None,
            to: None,
            id: "tzo-invalid".to_string(),
            payload: IqType::Result(Some(
                Element::builder("time", NS_TIME)
                    .append(Element::builder("tzo", NS_TIME).append("+16:00").build())
                    .append(
                        Element::builder("utc", NS_TIME)
                            .append("2024-06-01T12:00:00Z")
                            .build(),
                    )
                    .build(),
            )),
        };

        assert!(parse_time_response(&iq).is_none());
    }

    #[test]
    fn test_parse_time_response_rejects_non_utc_timestamp() {
        let iq = Iq {
            from: None,
            to: None,
            id: "utc-invalid".to_string(),
            payload: IqType::Result(Some(
                Element::builder("time", NS_TIME)
                    .append(Element::builder("tzo", NS_TIME).append("-05:00").build())
                    .append(
                        Element::builder("utc", NS_TIME)
                            .append("2024-06-01T12:00:00-05:00")
                            .build(),
                    )
                    .build(),
            )),
        };

        assert!(parse_time_response(&iq).is_none());
    }

    #[test]
    fn test_build_time_response_swaps_to_from() {
        let query = make_time_query();
        let result = build_time_response(&query, &EntityTime::now());

        assert_eq!(result.from, query.to);
        assert_eq!(result.to, query.from);
    }
}
