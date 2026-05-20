//! XEP-0012: Last Activity
//!
//! Provides helpers for detecting last activity queries and building responses.
//!
//! ## Use Cases
//!
//! 1. **Server/component query** (bare domain JID): Returns server uptime in seconds.
//! 2. **Offline user query** (bare user JID): Returns seconds since last logout
//!    and optional last status message.
//! 3. **Online user query** (bare user JID, user is online): Returns seconds=0.
//!
//! ## Service Discovery
//!
//! Advertises `jabber:iq:last` as a feature in disco#info responses.

use minidom::Element;
use xmpp_parsers::iq::Iq;

/// Namespace for XEP-0012 Last Activity.
pub const NS_LAST_ACTIVITY: &str = "jabber:iq:last";

/// Check if an IQ stanza is a last activity query (XEP-0012).
pub fn is_last_activity_query(iq: &Iq) -> bool {
    matches!(iq, Iq::Get { payload: elem, .. } if elem.name() == "query" && elem.ns() == NS_LAST_ACTIVITY)
}

/// Build a last activity result IQ.
///
/// # Arguments
/// * `original_iq` - The incoming IQ request (used for from/to/id).
/// * `seconds` - Number of seconds since last activity.
/// * `status` - Optional status text (e.g., last unavailable presence status).
pub fn build_last_activity_response(original_iq: &Iq, seconds: u64, status: Option<&str>) -> Iq {
    let mut query = Element::builder("query", NS_LAST_ACTIVITY)
        .attr(
            minidom::rxml::xml_ncname!("seconds").to_owned(),
            seconds.to_string(),
        )
        .build();

    if let Some(text) = status {
        query.append_text_node(text);
    }

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(query),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_last_activity_query() {
        let query_elem = Element::builder("query", NS_LAST_ACTIVITY).build();
        let iq = Iq::Get {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("bob@example.com".parse().unwrap()),
            id: "last-1".to_string(),
            payload: query_elem,
        };

        assert!(is_last_activity_query(&iq));
    }

    #[test]
    fn test_is_last_activity_query_false_for_other_ns() {
        let other_elem = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq::Get {
            from: None,
            to: None,
            id: "last-2".to_string(),
            payload: other_elem,
        };

        assert!(!is_last_activity_query(&iq));
    }

    #[test]
    fn test_is_last_activity_query_false_for_set() {
        let query_elem = Element::builder("query", NS_LAST_ACTIVITY).build();
        let iq = Iq::Set {
            from: None,
            to: None,
            id: "last-3".to_string(),
            payload: query_elem,
        };

        assert!(!is_last_activity_query(&iq));
    }

    #[test]
    fn test_build_last_activity_response_server_uptime() {
        let query_elem = Element::builder("query", NS_LAST_ACTIVITY).build();
        let iq = Iq::Get {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "last-4".to_string(),
            payload: query_elem,
        };

        let result = build_last_activity_response(&iq, 3600, None);

        assert_eq!(result.id(), "last-4");
        assert_eq!(result.from(), iq.to());
        assert_eq!(result.to(), iq.from());

        if let Iq::Result {
            payload: Some(elem),
            ..
        } = &result
        {
            assert_eq!(elem.name(), "query");
            assert_eq!(elem.ns(), NS_LAST_ACTIVITY);
            assert_eq!(elem.attr("seconds"), Some("3600"));
            assert_eq!(elem.text(), "");
        } else {
            panic!("Expected Result with payload");
        }
    }

    #[test]
    fn test_build_last_activity_response_with_status() {
        let query_elem = Element::builder("query", NS_LAST_ACTIVITY).build();
        let iq = Iq::Get {
            from: Some("romeo@montague.net".parse().unwrap()),
            to: Some("juliet@capulet.com".parse().unwrap()),
            id: "last-5".to_string(),
            payload: query_elem,
        };

        let result = build_last_activity_response(&iq, 903, Some("Heading Home"));

        if let Iq::Result {
            payload: Some(elem),
            ..
        } = &result
        {
            assert_eq!(elem.attr("seconds"), Some("903"));
            assert_eq!(elem.text(), "Heading Home");
        } else {
            panic!("Expected Result with payload");
        }
    }

    #[test]
    fn test_build_last_activity_response_online_user() {
        let query_elem = Element::builder("query", NS_LAST_ACTIVITY).build();
        let iq = Iq::Get {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("bob@example.com".parse().unwrap()),
            id: "last-6".to_string(),
            payload: query_elem,
        };

        let result = build_last_activity_response(&iq, 0, None);

        if let Iq::Result {
            payload: Some(elem),
            ..
        } = &result
        {
            assert_eq!(elem.attr("seconds"), Some("0"));
        } else {
            panic!("Expected Result with payload");
        }
    }

    #[test]
    fn test_build_last_activity_response_swaps_to_from() {
        let query_elem = Element::builder("query", NS_LAST_ACTIVITY).build();
        let iq = Iq::Get {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "last-7".to_string(),
            payload: query_elem,
        };

        let result = build_last_activity_response(&iq, 42, None);

        assert_eq!(result.from(), iq.to());
        assert_eq!(result.to(), iq.from());
    }
}
