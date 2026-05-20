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
                .attr(minidom::rxml::xml_ncname!("index").to_owned(), "0")
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

    assert!(!elem.children().any(|c| c.name() == "first"));
    assert!(!elem.children().any(|c| c.name() == "last"));

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
