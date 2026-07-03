//! XEP-0059: Result Set Management dedicated suite.
//!
//! Wire-level round trips of RSM `<set/>` request/response elements,
//! typed error variants for malformed numeric fields, and the
//! empty-`<before/>` "last page" semantics from §2.5.

use minidom::Element;
use waddle_xmpp::xep::{
    build_rsm_request_element, build_rsm_response_element, extract_rsm_request,
    extract_rsm_response, is_rsm_element, parse_rsm_request, parse_rsm_response, RsmError,
    RsmRequest, RsmResponse, NS_RSM,
};

fn reparse(elem: &Element) -> Element {
    String::from(elem)
        .parse::<Element>()
        .expect("serialized set is well-formed XML")
}

#[test]
fn xep0059_namespace_is_exact() {
    assert_eq!(NS_RSM, "http://jabber.org/protocol/rsm");
}

#[test]
fn xep0059_forward_pagination_request_round_trips() {
    let request = RsmRequest::new()
        .with_max(10)
        .with_after("peterpan@neverland.lit");
    let parsed =
        parse_rsm_request(&reparse(&build_rsm_request_element(&request))).expect("round trip");
    assert_eq!(parsed, request);
    assert!(!parsed.is_last_page_request());
}

#[test]
fn xep0059_backward_pagination_request_round_trips() {
    let request = RsmRequest::new()
        .with_max(10)
        .with_before("peter@pixyland.org");
    let parsed =
        parse_rsm_request(&reparse(&build_rsm_request_element(&request))).expect("round trip");
    assert_eq!(parsed.before.as_deref(), Some("peter@pixyland.org"));
}

#[test]
fn xep0059_empty_before_means_last_page() {
    // §2.5: an empty <before/> requests the final page.
    let elem = build_rsm_request_element(&RsmRequest::new().with_max(10).last_page());
    let before = elem.get_child("before", NS_RSM).expect("before child");
    assert!(before.text().is_empty());

    let parsed = parse_rsm_request(&reparse(&elem)).expect("round trip");
    assert!(parsed.is_last_page_request());
    assert_eq!(parsed.before.as_deref(), Some(""));
}

#[test]
fn xep0059_index_request_round_trips() {
    let request = RsmRequest::new().with_max(10).with_index(371);
    let parsed =
        parse_rsm_request(&reparse(&build_rsm_request_element(&request))).expect("round trip");
    assert_eq!(parsed.index, Some(371));
}

#[test]
fn xep0059_response_with_first_index_round_trips() {
    let response = RsmResponse::new()
        .with_first("stpeter@jabber.org", Some(0))
        .with_last("peterpan@neverland.lit")
        .with_count(800);
    let elem = build_rsm_response_element(&response);
    let first = elem.get_child("first", NS_RSM).expect("first child");
    assert_eq!(first.attr("index"), Some("0"));

    let parsed = parse_rsm_response(&reparse(&elem)).expect("round trip");
    assert_eq!(parsed, response);
}

#[test]
fn xep0059_count_only_response_round_trips() {
    // §2.7: getting the item count without retrieving items.
    let response = RsmResponse::from_page(None, None, Some(800));
    let elem = build_rsm_response_element(&response);
    assert_eq!(elem.children().count(), 1);

    let parsed = parse_rsm_response(&reparse(&elem)).expect("round trip");
    assert_eq!(parsed.count, Some(800));
    assert!(parsed.first.is_none());
    assert!(parsed.last.is_none());
}

#[test]
fn xep0059_invalid_numeric_fields_are_typed_errors() {
    let bad_max: Element = "<set xmlns='http://jabber.org/protocol/rsm'><max>lots</max></set>"
        .parse()
        .expect("valid xml");
    assert!(matches!(
        parse_rsm_request(&bad_max),
        Err(RsmError::InvalidMax(v)) if v == "lots"
    ));

    let bad_index: Element =
        "<set xmlns='http://jabber.org/protocol/rsm'><index>first</index></set>"
            .parse()
            .expect("valid xml");
    assert!(matches!(
        parse_rsm_request(&bad_index),
        Err(RsmError::InvalidIndex(v)) if v == "first"
    ));

    let bad_count: Element =
        "<set xmlns='http://jabber.org/protocol/rsm'><count>many</count></set>"
            .parse()
            .expect("valid xml");
    assert!(matches!(
        parse_rsm_response(&bad_count),
        Err(RsmError::InvalidCount(v)) if v == "many"
    ));
}

#[test]
fn xep0059_non_rsm_element_is_rejected() {
    let elem = Element::builder("set", "urn:example:other").build();
    assert!(!is_rsm_element(&elem));
    assert!(matches!(
        parse_rsm_request(&elem),
        Err(RsmError::NotRsmElement)
    ));
    assert!(matches!(
        parse_rsm_response(&elem),
        Err(RsmError::NotRsmElement)
    ));
}

#[test]
fn xep0059_extract_from_mam_style_query_wrapper() {
    let query: Element = "<query xmlns='urn:xmpp:mam:2'>\
                          <set xmlns='http://jabber.org/protocol/rsm'>\
                          <max>20</max><after>09af3-cc343-b409f</after>\
                          </set></query>"
        .parse()
        .expect("valid xml");

    let request = extract_rsm_request(&query)
        .expect("set child present")
        .expect("parses");
    assert_eq!(request.max, Some(20));
    assert_eq!(request.after.as_deref(), Some("09af3-cc343-b409f"));

    let no_set: Element = "<query xmlns='urn:xmpp:mam:2'/>"
        .parse()
        .expect("valid xml");
    assert!(extract_rsm_request(&no_set).is_none());
}

#[test]
fn xep0059_extract_response_from_fin_wrapper() {
    let fin: Element = "<fin xmlns='urn:xmpp:mam:2'>\
                        <set xmlns='http://jabber.org/protocol/rsm'>\
                        <first index='0'>23452-4534-1</first>\
                        <last>390-2342-22</last><count>16</count>\
                        </set></fin>"
        .parse()
        .expect("valid xml");

    let response = extract_rsm_response(&fin)
        .expect("set child present")
        .expect("parses");
    assert_eq!(response.first.as_deref(), Some("23452-4534-1"));
    assert_eq!(response.first_index, Some(0));
    assert_eq!(response.last.as_deref(), Some("390-2342-22"));
    assert_eq!(response.count, Some(16));
}

#[test]
fn xep0059_unknown_children_are_ignored_per_extensibility_rules() {
    let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'>\
                         <max>5</max>\
                         <ext xmlns='http://jabber.org/protocol/rsm'>x</ext>\
                         </set>"
        .parse()
        .expect("valid xml");
    let request = parse_rsm_request(&elem).expect("parses despite unknown child");
    assert_eq!(request.max, Some(5));
}

#[test]
fn xep0059_empty_set_is_empty_request() {
    let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'/>"
        .parse()
        .expect("valid xml");
    let request = parse_rsm_request(&elem).expect("parses");
    assert!(request.is_empty());
    assert!(RsmResponse::new().is_empty());
}
