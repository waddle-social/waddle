//! XEP-0433: Extended Channel Search
//!
//! Provides structured search for MUC rooms by keyword, allowing users
//! to discover channels in a community. Results include room metadata.
//!
//! ## XML Format
//!
//! Search request:
//! ```xml
//! <iq type='get' to='muc.example.com' id='search-1'>
//!   <search xmlns='urn:xmpp:channel-search:0'>
//!     <query>general</query>
//!     <max>20</max>
//!   </search>
//! </iq>
//! ```
//!
//! Search response:
//! ```xml
//! <iq type='result' from='muc.example.com' id='search-1'>
//!   <result xmlns='urn:xmpp:channel-search:0'>
//!     <channel jid='general@muc.example.com' name='General'
//!              description='Main discussion' occupants='42'/>
//!     <channel jid='random@muc.example.com' name='Random'
//!              description='Off-topic chat' occupants='15'/>
//!   </result>
//! </iq>
//! ```
//!
//! ## Use Cases
//!
//! - "Browse channels" dialog in chat apps
//! - Search for rooms by topic/keyword
//! - Discover public rooms in a community

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0433 Extended Channel Search.
pub const NS_CHANNEL_SEARCH: &str = "urn:xmpp:channel-search:0";

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// The search query string.
    pub query: String,
    /// Maximum number of results to return.
    pub max: Option<u32>,
}

impl SearchRequest {
    /// Create a new search request.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max: None,
        }
    }

    /// Set the maximum results.
    pub fn with_max(mut self, max: u32) -> Self {
        self.max = Some(max);
        self
    }
}

/// A channel result from a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelResult {
    /// The room JID.
    pub jid: String,
    /// The room name.
    pub name: Option<String>,
    /// The room description.
    pub description: Option<String>,
    /// Current number of occupants.
    pub occupants: Option<u32>,
}

impl ChannelResult {
    /// Create a new channel result.
    pub fn new(jid: impl Into<String>) -> Self {
        Self {
            jid: jid.into(),
            name: None,
            description: None,
            occupants: None,
        }
    }

    /// Set the name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the occupant count.
    pub fn with_occupants(mut self, count: u32) -> Self {
        self.occupants = Some(count);
        self
    }

    /// Returns the display name (name or JID localpart).
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .unwrap_or_else(|| self.jid.split('@').next().unwrap_or(&self.jid))
    }
}

/// Trait for matching channels against a search query.
pub trait Searchable {
    /// Check if this item matches a search query.
    fn matches_query(&self, query: &str) -> bool;
}

impl Searchable for ChannelResult {
    fn matches_query(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.jid.to_lowercase().contains(&q)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&q))
            || self
                .description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(&q))
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an IQ is a channel search request.
pub fn is_search_request(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem)
        if elem.name() == "search" && elem.ns() == NS_CHANNEL_SEARCH)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a search request from an IQ.
pub fn parse_search_request(iq: &Iq) -> Option<SearchRequest> {
    let elem = match &iq.payload {
        IqType::Get(elem) if elem.name() == "search" && elem.ns() == NS_CHANNEL_SEARCH => elem,
        _ => return None,
    };

    let query = elem
        .children()
        .find(|c| c.name() == "query")
        .map(|c| c.text())
        .unwrap_or_default();

    let max = elem
        .children()
        .find(|c| c.name() == "max")
        .and_then(|c| c.text().parse().ok());

    Some(SearchRequest { query, max })
}

/// Parse search results from an IQ response.
pub fn parse_search_results(iq: &Iq) -> Vec<ChannelResult> {
    let elem = match &iq.payload {
        IqType::Result(Some(elem)) if elem.name() == "result" && elem.ns() == NS_CHANNEL_SEARCH => {
            elem
        }
        _ => return Vec::new(),
    };

    elem.children()
        .filter(|c| c.name() == "channel" && c.ns() == NS_CHANNEL_SEARCH)
        .filter_map(|c| {
            let jid = c.attr("jid").filter(|s| !s.is_empty())?.to_owned();
            let name = c
                .attr("name")
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned());
            let description = c
                .attr("description")
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned());
            let occupants = c.attr("occupants").and_then(|s| s.parse().ok());
            Some(ChannelResult {
                jid,
                name,
                description,
                occupants,
            })
        })
        .collect()
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a search request IQ.
pub fn build_search_request(to: jid::Jid, request: &SearchRequest, id: &str) -> Iq {
    let mut search = Element::builder("search", NS_CHANNEL_SEARCH).build();

    let mut query_elem = Element::builder("query", NS_CHANNEL_SEARCH).build();
    query_elem.append_text_node(&request.query);
    search.append_child(query_elem);

    if let Some(max) = request.max {
        let mut max_elem = Element::builder("max", NS_CHANNEL_SEARCH).build();
        max_elem.append_text_node(max.to_string());
        search.append_child(max_elem);
    }

    Iq {
        from: None,
        to: Some(to),
        id: id.to_owned(),
        payload: IqType::Get(search),
    }
}

/// Build a search result IQ.
pub fn build_search_response(original_iq: &Iq, results: &[ChannelResult]) -> Iq {
    let mut result_elem = Element::builder("result", NS_CHANNEL_SEARCH).build();

    for ch in results {
        let mut channel = Element::builder("channel", NS_CHANNEL_SEARCH)
            .attr("jid", ch.jid.as_str())
            .build();
        if let Some(ref name) = ch.name {
            channel.set_attr("name", name);
        }
        if let Some(ref desc) = ch.description {
            channel.set_attr("description", desc);
        }
        if let Some(occ) = ch.occupants {
            channel.set_attr("occupants", occ.to_string());
        }
        result_elem.append_child(channel);
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(result_elem)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_search_iq() -> Iq {
        build_search_request(
            "muc.example.com".parse().expect("valid jid"),
            &SearchRequest::new("general").with_max(20),
            "search-1",
        )
    }

    #[test]
    fn test_is_search_request() {
        let iq = make_search_iq();
        assert!(is_search_request(&iq));
    }

    #[test]
    fn test_is_search_request_false() {
        let elem = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "x".to_owned(),
            payload: IqType::Get(elem),
        };
        assert!(!is_search_request(&iq));
    }

    #[test]
    fn test_parse_search_request() {
        let iq = make_search_iq();
        let req = parse_search_request(&iq).expect("parseable");
        assert_eq!(req.query, "general");
        assert_eq!(req.max, Some(20));
    }

    #[test]
    fn test_build_and_parse_response() {
        let request_iq = make_search_iq();
        let results = vec![
            ChannelResult::new("general@muc.example.com")
                .with_name("General")
                .with_description("Main chat")
                .with_occupants(42),
            ChannelResult::new("random@muc.example.com").with_name("Random"),
        ];

        let response = build_search_response(&request_iq, &results);
        let parsed = parse_search_results(&response);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].jid, "general@muc.example.com");
        assert_eq!(parsed[0].name.as_deref(), Some("General"));
        assert_eq!(parsed[0].description.as_deref(), Some("Main chat"));
        assert_eq!(parsed[0].occupants, Some(42));
        assert_eq!(parsed[1].jid, "random@muc.example.com");
        assert_eq!(parsed[1].occupants, None);
    }

    #[test]
    fn test_parse_empty_results() {
        let iq = Iq {
            from: None,
            to: None,
            id: "x".to_owned(),
            payload: IqType::Result(None),
        };
        assert!(parse_search_results(&iq).is_empty());
    }

    #[test]
    fn test_channel_result_display_name() {
        let with_name = ChannelResult::new("room@muc").with_name("My Room");
        assert_eq!(with_name.display_name(), "My Room");

        let without = ChannelResult::new("room@muc.example.com");
        assert_eq!(without.display_name(), "room");
    }

    #[test]
    fn test_searchable_trait() {
        let ch = ChannelResult::new("general@muc.example.com")
            .with_name("General Discussion")
            .with_description("Main chat for everyone");

        assert!(ch.matches_query("general"));
        assert!(ch.matches_query("General"));
        assert!(ch.matches_query("discuss"));
        assert!(ch.matches_query("everyone"));
        assert!(!ch.matches_query("random"));
    }

    #[test]
    fn test_search_request_builder() {
        let req = SearchRequest::new("test").with_max(10);
        assert_eq!(req.query, "test");
        assert_eq!(req.max, Some(10));

        let req2 = SearchRequest::new("no-max");
        assert_eq!(req2.max, None);
    }

    #[test]
    fn test_channel_result_builder() {
        let ch = ChannelResult::new("room@muc")
            .with_name("Room")
            .with_description("A room")
            .with_occupants(5);
        assert_eq!(ch.jid, "room@muc");
        assert_eq!(ch.name.as_deref(), Some("Room"));
        assert_eq!(ch.description.as_deref(), Some("A room"));
        assert_eq!(ch.occupants, Some(5));
    }
}
