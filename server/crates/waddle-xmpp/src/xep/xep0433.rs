//! XEP-0433: Extended Channel Search
//!
//! Provides structured search for MUC rooms by keyword. Requests carry a
//! XEP-0004 submit form and may carry XEP-0059 Result Set Management, and
//! results are `<item address='...'>` entries with metadata children.

use std::convert::TryFrom;

use minidom::Element;
use xmpp_parsers::iq::Iq;

use super::xep0004::{DataForm, FormType, FromElement, ToElement};
use super::xep0004::{Field, FieldType, NS_DATA_FORMS};
use super::xep0059::{
    build_rsm_request_element, build_rsm_response_element, extract_rsm_request,
    extract_rsm_response, RsmRequest, RsmResponse,
};

/// Namespace for XEP-0433 Extended Channel Search requests and results.
pub const NS_CHANNEL_SEARCH: &str = "urn:xmpp:channel-search:0:search";

/// FORM_TYPE value for search parameter forms.
pub const NS_CHANNEL_SEARCH_PARAMS: &str = "urn:xmpp:channel-search:0:search-params";

/// XEP-0433 query field name.
pub const FIELD_QUERY: &str = "q";

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// The search query string.
    pub query: String,
    /// Maximum number of results to return.
    pub max: Option<u32>,
    /// Result Set Management request metadata.
    pub rsm: Option<RsmRequest>,
}

impl SearchRequest {
    /// Create a new search request.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max: None,
            rsm: None,
        }
    }

    /// Set the maximum results.
    pub fn with_max(mut self, max: u32) -> Self {
        self.max = Some(max);
        self.rsm = Some(self.rsm.unwrap_or_default().with_max(max));
        self
    }

    /// Set explicit RSM request metadata.
    pub fn with_rsm(mut self, rsm: RsmRequest) -> Self {
        self.max = rsm.max;
        self.rsm = Some(rsm);
        self
    }
}

/// A channel result from a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelResult {
    /// The room address.
    pub address: String,
    /// The room name.
    pub name: Option<String>,
    /// The room description.
    pub description: Option<String>,
    /// The room language.
    pub language: Option<String>,
    /// Current number of occupants/users.
    pub nusers: Option<u32>,
}

impl ChannelResult {
    /// Create a new channel result.
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            name: None,
            description: None,
            language: None,
            nusers: None,
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

    /// Set the language.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Set the user count.
    pub fn with_nusers(mut self, count: u32) -> Self {
        self.nusers = Some(count);
        self
    }

    /// Backwards-compatible typed setter for callers that still use the
    /// old local name.
    pub fn with_occupants(self, count: u32) -> Self {
        self.with_nusers(count)
    }

    /// Returns the display name (name or address localpart).
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .unwrap_or_else(|| self.address.split('@').next().unwrap_or(&self.address))
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
        self.address.to_lowercase().contains(&q)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&q))
            || self
                .description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(&q))
            || self
                .language
                .as_deref()
                .is_some_and(|l| l.to_lowercase().contains(&q))
    }
}

/// Check if an IQ is a channel search request.
pub fn is_search_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Get { payload: elem, .. } if elem.is("search", NS_CHANNEL_SEARCH))
}

/// Check if an IQ requests the search form template.
pub fn is_search_form_request(iq: &Iq) -> bool {
    matches!(
        iq,
        Iq::Get { payload: elem, .. }
            if elem.is("search", NS_CHANNEL_SEARCH) && elem.children().next().is_none()
    )
}

/// Parse a search request from an IQ.
pub fn parse_search_request(iq: &Iq) -> Option<SearchRequest> {
    let elem = match iq {
        Iq::Get { payload: elem, .. } if elem.is("search", NS_CHANNEL_SEARCH) => elem,
        _ => return None,
    };

    let form = elem
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .and_then(|child| DataForm::from_element(child).ok())?;
    if form.form_type != FormType::Submit {
        return None;
    }
    if form.get_form_type_value() != Some(NS_CHANNEL_SEARCH_PARAMS) {
        return None;
    }

    let rsm = extract_rsm_request(elem).and_then(Result::ok);
    let query = form.get_value(FIELD_QUERY).unwrap_or_default().to_owned();
    let max = rsm.as_ref().and_then(|rsm| rsm.max);

    Some(SearchRequest { query, max, rsm })
}

/// Parse search results from an IQ response.
pub fn parse_search_results(iq: &Iq) -> Vec<ChannelResult> {
    let elem = match iq {
        Iq::Result {
            payload: Some(elem),
            ..
        } if elem.is("result", NS_CHANNEL_SEARCH) => elem,
        _ => return Vec::new(),
    };

    elem.children()
        .filter(|child| child.is("item", NS_CHANNEL_SEARCH))
        .filter_map(parse_result_item)
        .collect()
}

/// Parse XEP-0059 metadata from a search result.
pub fn parse_search_rsm_response(iq: &Iq) -> Option<RsmResponse> {
    let elem = match iq {
        Iq::Result {
            payload: Some(elem),
            ..
        } if elem.is("result", NS_CHANNEL_SEARCH) => elem,
        _ => return None,
    };
    extract_rsm_response(elem).and_then(Result::ok)
}

fn parse_result_item(item: &Element) -> Option<ChannelResult> {
    let address = item.attr("address").filter(|s| !s.is_empty())?.to_owned();
    let child_text = |name: &str| {
        item.get_child(name, NS_CHANNEL_SEARCH)
            .map(|child| child.text())
            .filter(|text| !text.is_empty())
    };
    Some(ChannelResult {
        address,
        name: child_text("name"),
        description: child_text("description"),
        language: child_text("language"),
        nusers: child_text("nusers").and_then(|text| text.parse().ok()),
    })
}

/// Build a search request IQ.
pub fn build_search_request(to: jid::Jid, request: &SearchRequest, id: &str) -> Iq {
    let mut search = Element::builder("search", NS_CHANNEL_SEARCH);

    if let Some(rsm) = request
        .rsm
        .clone()
        .or_else(|| request.max.map(|max| RsmRequest::new().with_max(max)))
        .filter(|rsm| !rsm.is_empty())
    {
        search = search.append(build_rsm_request_element(&rsm));
    }

    let form = DataForm::new(FormType::Submit)
        .add_field(Field::form_type(NS_CHANNEL_SEARCH_PARAMS))
        .add_field(Field::text_single(FIELD_QUERY, request.query.as_str()))
        .to_element();
    search = search.append(form);

    Iq::Get {
        from: None,
        to: Some(to),
        id: id.to_owned(),
        payload: search.build(),
    }
}

/// Build a response containing the channel search form template.
pub fn build_search_form_response(original_iq: &Iq) -> Iq {
    let form = DataForm::new(FormType::Form)
        .add_field(Field::form_type(NS_CHANNEL_SEARCH_PARAMS))
        .add_field(
            Field::new(FIELD_QUERY, FieldType::TextSingle)
                .with_label("Search for")
                .with_required(),
        )
        .to_element();
    let payload = Element::builder("search", NS_CHANNEL_SEARCH)
        .append(form)
        .build();

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(payload),
    }
}

/// Build a search result IQ.
pub fn build_search_response(original_iq: &Iq, results: &[ChannelResult]) -> Iq {
    let count = u32::try_from(results.len()).unwrap_or(u32::MAX);
    let rsm = RsmResponse::from_page(
        results.first().map(|result| result.address.clone()),
        results.last().map(|result| result.address.clone()),
        Some(count),
    );
    build_search_response_with_rsm(original_iq, results, &rsm)
}

/// Build a search result IQ with explicit RSM metadata.
pub fn build_search_response_with_rsm(
    original_iq: &Iq,
    results: &[ChannelResult],
    rsm: &RsmResponse,
) -> Iq {
    let mut result_elem = Element::builder("result", NS_CHANNEL_SEARCH);

    for ch in results {
        let mut item = Element::builder("item", NS_CHANNEL_SEARCH).attr(
            minidom::rxml::xml_ncname!("address").to_owned(),
            ch.address.as_str(),
        );
        if let Some(ref name) = ch.name {
            item = item.append(
                Element::builder("name", NS_CHANNEL_SEARCH)
                    .append(name.as_str())
                    .build(),
            );
        }
        if let Some(ref description) = ch.description {
            item = item.append(
                Element::builder("description", NS_CHANNEL_SEARCH)
                    .append(description.as_str())
                    .build(),
            );
        }
        if let Some(ref language) = ch.language {
            item = item.append(
                Element::builder("language", NS_CHANNEL_SEARCH)
                    .append(language.as_str())
                    .build(),
            );
        }
        if let Some(nusers) = ch.nusers {
            item = item.append(
                Element::builder("nusers", NS_CHANNEL_SEARCH)
                    .append(nusers.to_string())
                    .build(),
            );
        }
        result_elem = result_elem.append(item.build());
    }

    if !rsm.is_empty() {
        result_elem = result_elem.append(build_rsm_response_element(rsm));
    }

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(result_elem.build()),
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
    fn test_parse_search_request() {
        let iq = make_search_iq();
        let req = parse_search_request(&iq).expect("parseable");
        assert_eq!(req.query, "general");
        assert_eq!(req.max, Some(20));
        assert_eq!(req.rsm.and_then(|rsm| rsm.max), Some(20));
    }

    #[test]
    fn test_build_and_parse_response() {
        let request_iq = make_search_iq();
        let results = vec![
            ChannelResult::new("general@muc.example.com")
                .with_name("General")
                .with_description("Main chat")
                .with_language("en")
                .with_nusers(42),
            ChannelResult::new("random@muc.example.com").with_name("Random"),
        ];

        let response = build_search_response(&request_iq, &results);
        let parsed = parse_search_results(&response);

        assert_eq!(parsed, results);
        let rsm = parse_search_rsm_response(&response).expect("rsm response");
        assert_eq!(rsm.count, Some(2));
        assert_eq!(rsm.first.as_deref(), Some("general@muc.example.com"));
        assert_eq!(rsm.last.as_deref(), Some("random@muc.example.com"));
    }

    #[test]
    fn test_searchable() {
        let ch = ChannelResult::new("general@muc.example.com")
            .with_name("General Discussion")
            .with_description("Main chat");
        assert!(ch.matches_query("general"));
        assert!(ch.matches_query("DISCUSSION"));
        assert!(ch.matches_query("chat"));
        assert!(!ch.matches_query("random"));
    }
}
