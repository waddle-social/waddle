//! XEP-0202: Entity Time
//!
//! Provides helpers for detecting entity time queries and building responses.
//! Allows clients to query the server's current time and timezone.
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

use chrono::{DateTime, Utc};
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0202 Entity Time.
pub const NS_TIME: &str = "urn:xmpp:time";

/// Check if an IQ stanza is an entity time query.
pub fn is_time_query(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem) if elem.name() == "time" && elem.ns() == NS_TIME)
}

/// Build an entity time response IQ.
///
/// Returns the current UTC time and timezone offset.
pub fn build_time_response(original_iq: &Iq, now: DateTime<Utc>, tzo: &str) -> Iq {
    let utc_str = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let time_elem = Element::builder("time", NS_TIME)
        .append(
            Element::builder("tzo", NS_TIME)
                .append(tzo)
                .build(),
        )
        .append(
            Element::builder("utc", NS_TIME)
                .append(utc_str)
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

/// Build an entity time response with the current UTC time and +00:00 offset.
///
/// Convenience wrapper for servers running in UTC.
pub fn build_time_response_utc(original_iq: &Iq) -> Iq {
    build_time_response(original_iq, Utc::now(), "+00:00")
}

/// Parse a time response to extract UTC timestamp and timezone offset.
pub fn parse_time_response(iq: &Iq) -> Option<(String, String)> {
    let elem = match &iq.payload {
        IqType::Result(Some(elem)) if elem.name() == "time" && elem.ns() == NS_TIME => elem,
        _ => return None,
    };

    let tzo = elem
        .children()
        .find(|c| c.is("tzo", NS_TIME))
        .map(|c| c.text())?;

    let utc = elem
        .children()
        .find(|c| c.is("utc", NS_TIME))
        .map(|c| c.text())?;

    Some((utc, tzo))
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
        let now = Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).single().expect("valid test date");
        let result = build_time_response(&query, now, "+00:00");

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
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 18, 30, 0).single().expect("valid test date");
        let result = build_time_response(&query, now, "-06:00");

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
    fn test_build_time_response_utc() {
        let query = make_time_query();
        let result = build_time_response_utc(&query);

        assert_eq!(result.id, "time-1");
        if let IqType::Result(Some(elem)) = &result.payload {
            let tzo = elem
                .children()
                .find(|c| c.is("tzo", NS_TIME))
                .expect("tzo present");
            assert_eq!(tzo.text(), "+00:00");

            let utc = elem
                .children()
                .find(|c| c.is("utc", NS_TIME))
                .expect("utc present");
            // Just verify it looks like an ISO timestamp
            assert!(utc.text().contains('T'));
            assert!(utc.text().ends_with('Z'));
        } else {
            panic!("Expected Result with payload");
        }
    }

    #[test]
    fn test_parse_time_response() {
        let query = make_time_query();
        let now = Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).single().expect("valid test date");
        let result = build_time_response(&query, now, "-05:00");

        let (utc, tzo) = parse_time_response(&result).expect("parseable");
        assert_eq!(utc, "2024-06-01T12:00:00Z");
        assert_eq!(tzo, "-05:00");
    }

    #[test]
    fn test_parse_time_response_not_result() {
        let query = make_time_query();
        assert!(parse_time_response(&query).is_none());
    }

    #[test]
    fn test_build_time_response_swaps_to_from() {
        let query = make_time_query();
        let now = Utc::now();
        let result = build_time_response(&query, now, "+00:00");

        assert_eq!(result.from, query.to);
        assert_eq!(result.to, query.from);
    }
}
