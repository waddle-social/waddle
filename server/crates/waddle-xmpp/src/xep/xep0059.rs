//! XEP-0059: Result Set Management
//!
//! Provides generic pagination for XMPP result sets. Used by XEP-0313 (MAM),
//! XEP-0030 (Service Discovery), and other XEPs that return lists of items.
//!
//! ## XML Format
//!
//! ### Request (limit + forward pagination)
//!
//! ```xml
//! <set xmlns='http://jabber.org/protocol/rsm'>
//!   <max>10</max>
//!   <after>item-id-123</after>
//! </set>
//! ```
//!
//! ### Request (backward pagination)
//!
//! ```xml
//! <set xmlns='http://jabber.org/protocol/rsm'>
//!   <max>10</max>
//!   <before>item-id-456</before>
//! </set>
//! ```
//!
//! ### Request (last page — empty `<before/>`)
//!
//! ```xml
//! <set xmlns='http://jabber.org/protocol/rsm'>
//!   <max>10</max>
//!   <before/>
//! </set>
//! ```
//!
//! ### Response
//!
//! ```xml
//! <set xmlns='http://jabber.org/protocol/rsm'>
//!   <first index='0'>item-id-001</first>
//!   <last>item-id-010</last>
//!   <count>800</count>
//! </set>
//! ```
//!
//! ## Use Cases
//!
//! - Paginate MAM (message archive) queries
//! - Paginate disco#items results
//! - Paginate MUC room lists
//! - Any XMPP query returning a large set of items

use minidom::Element;
use thiserror::Error;

/// Namespace for XEP-0059 Result Set Management.
pub const NS_RSM: &str = "http://jabber.org/protocol/rsm";

/// Errors that can occur when parsing RSM elements.
#[derive(Debug, Error)]
pub enum RsmError {
    /// The `<max>` element contains a non-numeric value.
    #[error("invalid max value: {0}")]
    InvalidMax(String),

    /// The `<index>` attribute on `<first>` is not a valid number.
    #[error("invalid index value: {0}")]
    InvalidIndex(String),

    /// The `<count>` element contains a non-numeric value.
    #[error("invalid count value: {0}")]
    InvalidCount(String),

    /// The element is not a valid RSM `<set>` element.
    #[error("not an RSM set element")]
    NotRsmElement,
}

/// Pagination request parsed from an RSM `<set>` element.
///
/// Clients include this in queries to control which page of results
/// they receive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RsmRequest {
    /// Maximum number of items to return.
    pub max: Option<u32>,

    /// Return items after this ID (forward pagination).
    pub after: Option<String>,

    /// Return items before this ID (backward pagination).
    ///
    /// An empty string (`Some(String::new())`) means "last page" —
    /// the server should return the final page of results.
    pub before: Option<String>,

    /// Request items starting at this zero-based index.
    ///
    /// Use of `index` is discouraged for large result sets because
    /// the server may not support efficient random access.
    pub index: Option<u32>,
}

impl RsmRequest {
    /// Create a new empty request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of results.
    pub fn with_max(mut self, max: u32) -> Self {
        self.max = Some(max);
        self
    }

    /// Request items after the given ID (forward pagination).
    pub fn with_after(mut self, after: impl Into<String>) -> Self {
        self.after = Some(after.into());
        self
    }

    /// Request items before the given ID (backward pagination).
    pub fn with_before(mut self, before: impl Into<String>) -> Self {
        self.before = Some(before.into());
        self
    }

    /// Request the last page of results (empty `<before/>`).
    pub fn last_page(mut self) -> Self {
        self.before = Some(String::new());
        self
    }

    /// Request items starting at a specific index.
    pub fn with_index(mut self, index: u32) -> Self {
        self.index = Some(index);
        self
    }

    /// Returns `true` if this is a request for the last page.
    pub fn is_last_page_request(&self) -> bool {
        self.before.as_ref().is_some_and(|b| b.is_empty())
    }

    /// Returns `true` if this request contains no pagination parameters.
    pub fn is_empty(&self) -> bool {
        self.max.is_none() && self.after.is_none() && self.before.is_none() && self.index.is_none()
    }
}

/// Pagination response metadata embedded in result stanzas.
///
/// Servers include this to tell the client where they are in the
/// overall result set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RsmResponse {
    /// ID of the first item in this page.
    pub first: Option<String>,

    /// Zero-based index of the first item in the overall result set.
    pub first_index: Option<u32>,

    /// ID of the last item in this page.
    pub last: Option<String>,

    /// Total number of items in the result set (if known).
    pub count: Option<u32>,
}

impl RsmResponse {
    /// Create a new empty response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the first item ID and optional index.
    pub fn with_first(mut self, id: impl Into<String>, index: Option<u32>) -> Self {
        self.first = Some(id.into());
        self.first_index = index;
        self
    }

    /// Set the last item ID.
    pub fn with_last(mut self, id: impl Into<String>) -> Self {
        self.last = Some(id.into());
        self
    }

    /// Set the total count.
    pub fn with_count(mut self, count: u32) -> Self {
        self.count = Some(count);
        self
    }

    /// Build an RSM response from first/last IDs and an optional count.
    ///
    /// Convenience constructor for the common case where you know the
    /// first/last IDs and total count but not the index.
    pub fn from_page(
        first_id: Option<String>,
        last_id: Option<String>,
        count: Option<u32>,
    ) -> Self {
        Self {
            first: first_id,
            first_index: None,
            last: last_id,
            count,
        }
    }

    /// Returns `true` if the response contains no pagination metadata.
    pub fn is_empty(&self) -> bool {
        self.first.is_none() && self.last.is_none() && self.count.is_none()
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Check whether an XML element is an RSM `<set>` element.
pub fn is_rsm_element(elem: &Element) -> bool {
    elem.name() == "set" && elem.ns() == NS_RSM
}

/// Parse an RSM request from a `<set>` child element.
///
/// Typically called on the `<set>` child of a query element
/// (e.g. inside `<query xmlns='urn:xmpp:mam:2'>`).
pub fn parse_rsm_request(elem: &Element) -> Result<RsmRequest, RsmError> {
    if !is_rsm_element(elem) {
        return Err(RsmError::NotRsmElement);
    }

    let mut request = RsmRequest::new();

    for child in elem.children() {
        match child.name() {
            "max" => {
                let text = child.text();
                if !text.is_empty() {
                    request.max = Some(
                        text.parse()
                            .map_err(|_| RsmError::InvalidMax(text.clone()))?,
                    );
                }
            }
            "after" => {
                let text = child.text();
                if !text.is_empty() {
                    request.after = Some(text);
                }
            }
            "before" => {
                // An empty <before/> means "last page"
                request.before = Some(child.text());
            }
            "index" => {
                let text = child.text();
                if !text.is_empty() {
                    request.index = Some(
                        text.parse()
                            .map_err(|_| RsmError::InvalidIndex(text.clone()))?,
                    );
                }
            }
            _ => {} // Ignore unknown children per XMPP extensibility rules
        }
    }

    Ok(request)
}

/// Parse an RSM response from a `<set>` child element.
///
/// Typically called on the `<set>` child of a result element
/// (e.g. inside `<fin xmlns='urn:xmpp:mam:2'>`).
pub fn parse_rsm_response(elem: &Element) -> Result<RsmResponse, RsmError> {
    if !is_rsm_element(elem) {
        return Err(RsmError::NotRsmElement);
    }

    let mut response = RsmResponse::new();

    for child in elem.children() {
        match child.name() {
            "first" => {
                let text = child.text();
                if !text.is_empty() {
                    response.first = Some(text);
                }
                if let Some(idx_str) = child.attr("index") {
                    response.first_index = Some(
                        idx_str
                            .parse()
                            .map_err(|_| RsmError::InvalidIndex(idx_str.to_string()))?,
                    );
                }
            }
            "last" => {
                let text = child.text();
                if !text.is_empty() {
                    response.last = Some(text);
                }
            }
            "count" => {
                let text = child.text();
                if !text.is_empty() {
                    response.count = Some(
                        text.parse()
                            .map_err(|_| RsmError::InvalidCount(text.clone()))?,
                    );
                }
            }
            _ => {} // Ignore unknown children
        }
    }

    Ok(response)
}

/// Extract an RSM `<set>` child from a parent element (if present).
///
/// Searches the children of `parent` for a `<set xmlns='...rsm'>` element
/// and parses it as an [`RsmRequest`].
pub fn extract_rsm_request(parent: &Element) -> Option<Result<RsmRequest, RsmError>> {
    parent
        .children()
        .find(|c| is_rsm_element(c))
        .map(parse_rsm_request)
}

/// Extract an RSM `<set>` child from a parent element (if present).
///
/// Searches the children of `parent` for a `<set xmlns='...rsm'>` element
/// and parses it as an [`RsmResponse`].
pub fn extract_rsm_response(parent: &Element) -> Option<Result<RsmResponse, RsmError>> {
    parent
        .children()
        .find(|c| is_rsm_element(c))
        .map(parse_rsm_response)
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

/// Build an RSM `<set>` element for a request.
pub fn build_rsm_request_element(request: &RsmRequest) -> Element {
    let mut builder = Element::builder("set", NS_RSM);

    if let Some(max) = request.max {
        builder = builder.append(
            Element::builder("max", NS_RSM)
                .append(max.to_string())
                .build(),
        );
    }

    if let Some(ref after) = request.after {
        builder = builder.append(
            Element::builder("after", NS_RSM)
                .append(after.clone())
                .build(),
        );
    }

    if let Some(ref before) = request.before {
        let mut before_builder = Element::builder("before", NS_RSM);
        if !before.is_empty() {
            before_builder = before_builder.append(before.clone());
        }
        builder = builder.append(before_builder.build());
    }

    if let Some(index) = request.index {
        builder = builder.append(
            Element::builder("index", NS_RSM)
                .append(index.to_string())
                .build(),
        );
    }

    builder.build()
}

/// Build an RSM `<set>` element for a response.
pub fn build_rsm_response_element(response: &RsmResponse) -> Element {
    let mut builder = Element::builder("set", NS_RSM);

    if let Some(ref first) = response.first {
        let mut first_builder = Element::builder("first", NS_RSM);
        if let Some(index) = response.first_index {
            first_builder = first_builder.attr("index", index.to_string());
        }
        first_builder = first_builder.append(first.clone());
        builder = builder.append(first_builder.build());
    }

    if let Some(ref last) = response.last {
        builder = builder.append(
            Element::builder("last", NS_RSM)
                .append(last.clone())
                .build(),
        );
    }

    if let Some(count) = response.count {
        builder = builder.append(
            Element::builder("count", NS_RSM)
                .append(count.to_string())
                .build(),
        );
    }

    builder.build()
}

// ---------------------------------------------------------------------------
// Trait for RSM-capable queries
// ---------------------------------------------------------------------------

/// Trait for query types that support RSM pagination.
///
/// Implement this on your query struct (e.g., `MamQuery`) so that
/// pagination parameters can be extracted and applied generically.
pub trait RsmPaginated {
    /// Get the RSM request parameters from this query.
    fn rsm_request(&self) -> Option<&RsmRequest>;

    /// Set the RSM request parameters on this query.
    fn set_rsm_request(&mut self, request: RsmRequest);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_rsm_element() {
        let rsm = Element::builder("set", NS_RSM).build();
        assert!(is_rsm_element(&rsm));

        let not_rsm = Element::builder("set", "other:ns").build();
        assert!(!is_rsm_element(&not_rsm));

        let wrong_name = Element::builder("query", NS_RSM).build();
        assert!(!is_rsm_element(&wrong_name));
    }

    #[test]
    fn test_parse_rsm_request_max_only() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("10").build())
            .build();

        let request = parse_rsm_request(&elem).expect("valid RSM request");
        assert_eq!(request.max, Some(10));
        assert_eq!(request.after, None);
        assert_eq!(request.before, None);
        assert_eq!(request.index, None);
    }

    #[test]
    fn test_parse_rsm_request_forward_pagination() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("20").build())
            .append(Element::builder("after", NS_RSM).append("item-123").build())
            .build();

        let request = parse_rsm_request(&elem).expect("valid RSM request");
        assert_eq!(request.max, Some(20));
        assert_eq!(request.after, Some("item-123".to_string()));
        assert_eq!(request.before, None);
    }

    #[test]
    fn test_parse_rsm_request_backward_pagination() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("15").build())
            .append(
                Element::builder("before", NS_RSM)
                    .append("item-456")
                    .build(),
            )
            .build();

        let request = parse_rsm_request(&elem).expect("valid RSM request");
        assert_eq!(request.max, Some(15));
        assert_eq!(request.before, Some("item-456".to_string()));
        assert!(!request.is_last_page_request());
    }

    #[test]
    fn test_parse_rsm_request_last_page() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("10").build())
            .append(Element::builder("before", NS_RSM).build())
            .build();

        let request = parse_rsm_request(&elem).expect("valid RSM request");
        assert_eq!(request.max, Some(10));
        assert_eq!(request.before, Some(String::new()));
        assert!(request.is_last_page_request());
    }

    #[test]
    fn test_parse_rsm_request_with_index() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("10").build())
            .append(Element::builder("index", NS_RSM).append("50").build())
            .build();

        let request = parse_rsm_request(&elem).expect("valid RSM request");
        assert_eq!(request.max, Some(10));
        assert_eq!(request.index, Some(50));
    }

    #[test]
    fn test_parse_rsm_request_invalid_max() {
        let elem = Element::builder("set", NS_RSM)
            .append(
                Element::builder("max", NS_RSM)
                    .append("not-a-number")
                    .build(),
            )
            .build();

        let err = parse_rsm_request(&elem).unwrap_err();
        assert!(matches!(err, RsmError::InvalidMax(_)));
    }

    #[test]
    fn test_parse_rsm_request_invalid_index() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("index", NS_RSM).append("bad").build())
            .build();

        let err = parse_rsm_request(&elem).unwrap_err();
        assert!(matches!(err, RsmError::InvalidIndex(_)));
    }

    #[test]
    fn test_parse_rsm_request_not_rsm_element() {
        let elem = Element::builder("query", "other:ns").build();
        let err = parse_rsm_request(&elem).unwrap_err();
        assert!(matches!(err, RsmError::NotRsmElement));
    }

    #[test]
    fn test_parse_rsm_response_full() {
        let elem = Element::builder("set", NS_RSM)
            .append(
                Element::builder("first", NS_RSM)
                    .attr("index", "0")
                    .append("item-001")
                    .build(),
            )
            .append(Element::builder("last", NS_RSM).append("item-010").build())
            .append(Element::builder("count", NS_RSM).append("800").build())
            .build();

        let response = parse_rsm_response(&elem).expect("valid RSM response");
        assert_eq!(response.first, Some("item-001".to_string()));
        assert_eq!(response.first_index, Some(0));
        assert_eq!(response.last, Some("item-010".to_string()));
        assert_eq!(response.count, Some(800));
    }

    #[test]
    fn test_parse_rsm_response_count_only() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("count", NS_RSM).append("0").build())
            .build();

        let response = parse_rsm_response(&elem).expect("valid RSM response");
        assert_eq!(response.first, None);
        assert_eq!(response.last, None);
        assert_eq!(response.count, Some(0));
    }

    #[test]
    fn test_parse_rsm_response_invalid_count() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("count", NS_RSM).append("abc").build())
            .build();

        let err = parse_rsm_response(&elem).unwrap_err();
        assert!(matches!(err, RsmError::InvalidCount(_)));
    }

    #[test]
    fn test_parse_rsm_response_first_without_index() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("first", NS_RSM).append("item-abc").build())
            .append(Element::builder("last", NS_RSM).append("item-xyz").build())
            .build();

        let response = parse_rsm_response(&elem).expect("valid RSM response");
        assert_eq!(response.first, Some("item-abc".to_string()));
        assert_eq!(response.first_index, None);
        assert_eq!(response.last, Some("item-xyz".to_string()));
        assert_eq!(response.count, None);
    }

    #[test]
    fn test_build_rsm_request_element_max_only() {
        let request = RsmRequest::new().with_max(10);
        let elem = build_rsm_request_element(&request);

        assert_eq!(elem.name(), "set");
        assert_eq!(elem.ns(), NS_RSM);

        let max = elem
            .children()
            .find(|c| c.name() == "max")
            .expect("max element");
        assert_eq!(max.text(), "10");
    }

    #[test]
    fn test_build_rsm_request_element_forward() {
        let request = RsmRequest::new().with_max(20).with_after("id-42");
        let elem = build_rsm_request_element(&request);

        let after = elem
            .children()
            .find(|c| c.name() == "after")
            .expect("after element");
        assert_eq!(after.text(), "id-42");
    }

    #[test]
    fn test_build_rsm_request_element_last_page() {
        let request = RsmRequest::new().with_max(10).last_page();
        let elem = build_rsm_request_element(&request);

        let before = elem
            .children()
            .find(|c| c.name() == "before")
            .expect("before element");
        assert_eq!(before.text(), "");
    }

    #[test]
    fn test_build_rsm_request_element_with_index() {
        let request = RsmRequest::new().with_max(10).with_index(50);
        let elem = build_rsm_request_element(&request);

        let index = elem
            .children()
            .find(|c| c.name() == "index")
            .expect("index element");
        assert_eq!(index.text(), "50");
    }

    #[test]
    fn test_build_rsm_response_element_full() {
        let response = RsmResponse::new()
            .with_first("item-001", Some(0))
            .with_last("item-010")
            .with_count(800);
        let elem = build_rsm_response_element(&response);

        assert_eq!(elem.name(), "set");
        assert_eq!(elem.ns(), NS_RSM);

        let first = elem
            .children()
            .find(|c| c.name() == "first")
            .expect("first element");
        assert_eq!(first.text(), "item-001");
        assert_eq!(first.attr("index"), Some("0"));

        let last = elem
            .children()
            .find(|c| c.name() == "last")
            .expect("last element");
        assert_eq!(last.text(), "item-010");

        let count = elem
            .children()
            .find(|c| c.name() == "count")
            .expect("count element");
        assert_eq!(count.text(), "800");
    }

    #[test]
    fn test_build_rsm_response_element_no_index() {
        let response = RsmResponse::new()
            .with_first("item-abc", None)
            .with_last("item-xyz");
        let elem = build_rsm_response_element(&response);

        let first = elem
            .children()
            .find(|c| c.name() == "first")
            .expect("first element");
        assert_eq!(first.attr("index"), None);
    }

    #[test]
    fn test_build_rsm_response_element_count_only() {
        let response = RsmResponse::new().with_count(0);
        let elem = build_rsm_response_element(&response);

        assert!(elem.children().find(|c| c.name() == "first").is_none());
        assert!(elem.children().find(|c| c.name() == "last").is_none());

        let count = elem
            .children()
            .find(|c| c.name() == "count")
            .expect("count element");
        assert_eq!(count.text(), "0");
    }

    #[test]
    fn test_roundtrip_request() {
        let original = RsmRequest::new().with_max(25).with_after("msg-999");
        let elem = build_rsm_request_element(&original);
        let parsed = parse_rsm_request(&elem).expect("valid roundtrip");

        assert_eq!(parsed, original);
    }

    #[test]
    fn test_roundtrip_request_last_page() {
        let original = RsmRequest::new().with_max(10).last_page();
        let elem = build_rsm_request_element(&original);
        let parsed = parse_rsm_request(&elem).expect("valid roundtrip");

        assert_eq!(parsed, original);
        assert!(parsed.is_last_page_request());
    }

    #[test]
    fn test_roundtrip_response() {
        let original = RsmResponse::new()
            .with_first("first-id", Some(5))
            .with_last("last-id")
            .with_count(100);
        let elem = build_rsm_response_element(&original);
        let parsed = parse_rsm_response(&elem).expect("valid roundtrip");

        assert_eq!(parsed, original);
    }

    #[test]
    fn test_extract_rsm_request_from_parent() {
        let parent = Element::builder("query", "urn:xmpp:mam:2")
            .append(
                Element::builder("set", NS_RSM)
                    .append(Element::builder("max", NS_RSM).append("10").build())
                    .build(),
            )
            .build();

        let request = extract_rsm_request(&parent)
            .expect("RSM element present")
            .expect("valid RSM request");
        assert_eq!(request.max, Some(10));
    }

    #[test]
    fn test_extract_rsm_request_absent() {
        let parent = Element::builder("query", "urn:xmpp:mam:2").build();
        assert!(extract_rsm_request(&parent).is_none());
    }

    #[test]
    fn test_extract_rsm_response_from_parent() {
        let parent = Element::builder("fin", "urn:xmpp:mam:2")
            .append(
                Element::builder("set", NS_RSM)
                    .append(Element::builder("count", NS_RSM).append("42").build())
                    .build(),
            )
            .build();

        let response = extract_rsm_response(&parent)
            .expect("RSM element present")
            .expect("valid RSM response");
        assert_eq!(response.count, Some(42));
    }

    #[test]
    fn test_rsm_request_is_empty() {
        assert!(RsmRequest::new().is_empty());
        assert!(!RsmRequest::new().with_max(10).is_empty());
    }

    #[test]
    fn test_rsm_response_is_empty() {
        assert!(RsmResponse::new().is_empty());
        assert!(!RsmResponse::new().with_count(0).is_empty());
    }

    #[test]
    fn test_rsm_response_from_page() {
        let response = RsmResponse::from_page(
            Some("first-id".to_string()),
            Some("last-id".to_string()),
            Some(50),
        );
        assert_eq!(response.first, Some("first-id".to_string()));
        assert_eq!(response.first_index, None);
        assert_eq!(response.last, Some("last-id".to_string()));
        assert_eq!(response.count, Some(50));
    }

    #[test]
    fn test_parse_rsm_request_empty_set() {
        let elem = Element::builder("set", NS_RSM).build();
        let request = parse_rsm_request(&elem).expect("valid empty RSM request");
        assert!(request.is_empty());
    }

    #[test]
    fn test_parse_rsm_request_ignores_unknown_children() {
        let elem = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("10").build())
            .append(Element::builder("unknown-ext", "urn:example:ext").build())
            .build();

        let request = parse_rsm_request(&elem).expect("valid RSM request");
        assert_eq!(request.max, Some(10));
    }
}
