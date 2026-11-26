//! XEP-0433: Extended Channel Search
//!
//! Provides structured search for MUC rooms by keyword. Requests carry a
//! XEP-0004 submit form and may carry XEP-0059 Result Set Management, and
//! results are `<item address='...'>` entries with metadata children.

use std::convert::TryFrom;

use minidom::Element;
use xmpp_parsers::iq::Iq;

use super::xep0004::{DataForm, FormType, FromElement, ToElement};
use super::xep0004::{Field, FieldOption, FieldType, NS_DATA_FORMS};
use super::xep0059::{
    build_rsm_request_element, build_rsm_response_element, extract_rsm_request,
    extract_rsm_response, RsmRequest, RsmResponse,
};

/// Namespace for XEP-0433 Extended Channel Search requests and results.
pub const NS_CHANNEL_SEARCH: &str = "urn:xmpp:channel-search:0:search";

/// FORM_TYPE value for search parameter forms.
pub const NS_CHANNEL_SEARCH_PARAMS: &str = "urn:xmpp:channel-search:0:search-params";

/// Namespace of the XEP-0433 application-defined error conditions.
pub const NS_CHANNEL_SEARCH_ERROR: &str = "urn:xmpp:channel-search:0:error";

/// Namespace of the XEP-0433 sort key values.
pub const NS_CHANNEL_SEARCH_ORDER: &str = "urn:xmpp:channel-search:0:order";

/// XEP-0433 query field name.
pub const FIELD_QUERY: &str = "q";

/// XEP-0433 field: search in the channel address.
pub const FIELD_SEARCH_IN_ADDRESS: &str = "sinaddress";

/// XEP-0433 field: search in the channel name.
pub const FIELD_SEARCH_IN_NAME: &str = "sinname";

/// XEP-0433 field: search in the channel description.
pub const FIELD_SEARCH_IN_DESCRIPTION: &str = "sindescription";

/// XEP-0433 field: service types to search.
pub const FIELD_TYPES: &str = "types";

/// XEP-0433 field: result sort key.
pub const FIELD_KEY: &str = "key";

/// The service type value for XEP-0045 group chats, the only type this
/// service searches.
pub const SERVICE_TYPE_MUC: &str = "xep-0045";

/// Sort key value ordering results by channel address (Clark notation).
pub const SORT_KEY_ADDRESS: &str = "{urn:xmpp:channel-search:0:order}address";

/// Sort key value ordering results by descending user count (Clark notation).
pub const SORT_KEY_NUSERS: &str = "{urn:xmpp:channel-search:0:order}nusers";

/// How results are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Ascending by channel address.
    #[default]
    Address,
    /// Descending by number of users.
    Nusers,
}

impl SortKey {
    /// The Clark-notation wire value.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Address => SORT_KEY_ADDRESS,
            Self::Nusers => SORT_KEY_NUSERS,
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        match value {
            SORT_KEY_ADDRESS => Some(Self::Address),
            SORT_KEY_NUSERS => Some(Self::Nusers),
            _ => None,
        }
    }
}

/// Which channel properties a keyword search looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchScope {
    /// Search in the channel address.
    pub address: bool,
    /// Search in the channel name.
    pub name: bool,
    /// Search in the channel description.
    pub description: bool,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self {
            address: true,
            name: true,
            description: true,
        }
    }
}

/// Why a search request cannot be executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRequestError {
    /// Not a submit form of the search-params type.
    Malformed,
    /// No field defining a result set is present: `bad-request`, type
    /// `cancel`, with `<no-search-conditions/>`.
    NoSearchConditions,
    /// `q` supplied but without any usable term: `bad-request`, type
    /// `modify`, with `<invalid-search-terms/>`.
    InvalidSearchTerms,
    /// The `key` is not supported: `feature-not-implemented`, type
    /// `modify`, with `<invalid-sort-key/>`.
    InvalidSortKey,
}

impl SearchRequestError {
    /// Local name of the application-defined condition, if the error has one.
    pub fn application_condition(self) -> Option<&'static str> {
        match self {
            Self::Malformed => None,
            Self::NoSearchConditions => Some("no-search-conditions"),
            Self::InvalidSearchTerms => Some("invalid-search-terms"),
            Self::InvalidSortKey => Some("invalid-sort-key"),
        }
    }
}

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// The search query string.
    pub query: String,
    /// Maximum number of results to return.
    pub max: Option<u32>,
    /// Result Set Management request metadata.
    pub rsm: Option<RsmRequest>,
    /// Channel properties the keyword is matched against.
    pub scope: SearchScope,
    /// Requested service types; `None` when the field was not submitted.
    pub types: Option<Vec<String>>,
    /// Result ordering.
    pub key: SortKey,
}

impl SearchRequest {
    /// Create a new search request.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max: None,
            rsm: None,
            scope: SearchScope::default(),
            types: None,
            key: SortKey::default(),
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
    /// Check if this item matches a search query within `scope`.
    fn matches_query(&self, query: &str, scope: SearchScope) -> bool;
}

impl Searchable for ChannelResult {
    fn matches_query(&self, query: &str, scope: SearchScope) -> bool {
        let q = query.to_lowercase();
        let contains = |text: &str| text.to_lowercase().contains(&q);
        (scope.address && contains(&self.address))
            || (scope.name && self.name.as_deref().is_some_and(contains))
            || (scope.description && self.description.as_deref().is_some_and(contains))
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
///
/// `q` must be present (XEP-0433 "No Search Conditions") and yield a term
/// after trimming ("Invalid Search Terms"); `key`, when present, must be a
/// supported sort key ("Unsupported Sort Key"). Fields the service does not
/// understand are ignored.
pub fn parse_search_request(iq: &Iq) -> Result<SearchRequest, SearchRequestError> {
    let elem = match iq {
        Iq::Get { payload: elem, .. } if elem.is("search", NS_CHANNEL_SEARCH) => elem,
        _ => return Err(SearchRequestError::Malformed),
    };

    let form = elem
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .and_then(|child| DataForm::from_element(child).ok())
        .ok_or(SearchRequestError::Malformed)?;
    if form.form_type != FormType::Submit
        || form.form_type_value() != Some(NS_CHANNEL_SEARCH_PARAMS)
    {
        return Err(SearchRequestError::Malformed);
    }

    let rsm = extract_rsm_request(elem).and_then(Result::ok);
    let query_field = form
        .field(FIELD_QUERY)
        .ok_or(SearchRequestError::NoSearchConditions)?;
    let query = query_field.value().unwrap_or_default().trim().to_owned();
    if query.is_empty() {
        return Err(SearchRequestError::InvalidSearchTerms);
    }
    let key = match form.value(FIELD_KEY) {
        None => SortKey::default(),
        Some(value) => SortKey::from_wire(value).ok_or(SearchRequestError::InvalidSortKey)?,
    };
    let defaults = SearchScope::default();
    let scope = SearchScope {
        address: form
            .bool(FIELD_SEARCH_IN_ADDRESS)
            .unwrap_or(defaults.address),
        name: form.bool(FIELD_SEARCH_IN_NAME).unwrap_or(defaults.name),
        description: form
            .bool(FIELD_SEARCH_IN_DESCRIPTION)
            .unwrap_or(defaults.description),
    };
    let types = form.values(FIELD_TYPES).map(<[String]>::to_vec);
    let max = rsm.as_ref().and_then(|rsm| rsm.max);

    Ok(SearchRequest {
        query,
        max,
        rsm,
        scope,
        types,
        key,
    })
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

    let mut form = DataForm::new(FormType::Submit)
        .add_field(Field::form_type(NS_CHANNEL_SEARCH_PARAMS))
        .add_field(Field::text_single(FIELD_QUERY, request.query.as_str()))
        .add_field(Field::boolean(
            FIELD_SEARCH_IN_ADDRESS,
            request.scope.address,
        ))
        .add_field(Field::boolean(FIELD_SEARCH_IN_NAME, request.scope.name))
        .add_field(Field::boolean(
            FIELD_SEARCH_IN_DESCRIPTION,
            request.scope.description,
        ))
        .add_field(Field::new(FIELD_KEY, FieldType::ListSingle).with_value(request.key.as_wire()));
    if let Some(types) = &request.types {
        let mut field = Field::new(FIELD_TYPES, FieldType::ListMulti);
        field.values = types.clone();
        form = form.add_field(field);
    }
    let form = form.to_element();
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
        .add_field(Field::boolean(FIELD_SEARCH_IN_ADDRESS, true).with_label("Search in address"))
        .add_field(Field::boolean(FIELD_SEARCH_IN_NAME, true).with_label("Search in name"))
        .add_field(
            Field::boolean(FIELD_SEARCH_IN_DESCRIPTION, true).with_label("Search in description"),
        )
        .add_field(
            Field::new(FIELD_TYPES, FieldType::ListMulti)
                .with_label("Service types")
                .with_value(SERVICE_TYPE_MUC)
                .add_option(FieldOption::with_label(
                    "XEP-0045 Multi User Chats",
                    SERVICE_TYPE_MUC,
                )),
        )
        .add_field(
            Field::new(FIELD_KEY, FieldType::ListSingle)
                .with_label("Sort results by")
                .with_value(SORT_KEY_ADDRESS)
                .add_option(FieldOption::with_label("Address", SORT_KEY_ADDRESS)),
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
        assert_eq!(req.key, SortKey::Address);
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
    fn sort_key_wire_values_use_the_order_namespace() {
        assert_eq!(
            SORT_KEY_ADDRESS,
            format!("{{{NS_CHANNEL_SEARCH_ORDER}}}address")
        );
        assert_eq!(
            SORT_KEY_NUSERS,
            format!("{{{NS_CHANNEL_SEARCH_ORDER}}}nusers")
        );
    }

    #[test]
    fn test_searchable() {
        let ch = ChannelResult::new("general@muc.example.com")
            .with_name("General Discussion")
            .with_description("Main chat");
        let all = SearchScope::default();
        assert!(ch.matches_query("general", all));
        assert!(ch.matches_query("DISCUSSION", all));
        assert!(ch.matches_query("chat", all));
        assert!(!ch.matches_query("random", all));
        let name_only = SearchScope {
            address: false,
            name: true,
            description: false,
        };
        assert!(ch.matches_query("discussion", name_only));
        assert!(!ch.matches_query("chat", name_only));
        assert!(!ch.matches_query("muc.example", name_only));
    }
}
