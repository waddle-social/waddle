use super::*;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

fn jid(value: &str) -> Jid {
    value.parse().expect("valid jid")
}

fn blocking_item(value: &str) -> Element {
    Element::builder("item", NS_BLOCKING)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), value)
        .build()
}

fn set_iq(id: &str, payload: Element) -> Iq {
    Iq::Set {
        from: None,
        to: None,
        id: id.to_string(),
        payload,
    }
}

#[test]
fn test_is_blocking_query_get_blocklist() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "blocklist-1".to_string(),
        payload: Element::builder("blocklist", NS_BLOCKING).build(),
    };

    assert!(is_blocking_query(&iq));
    assert!(is_blocklist_get(&iq));
    assert!(!is_block_set(&iq));
    assert!(!is_unblock_set(&iq));
}

#[test]
fn test_is_blocking_query_block() {
    let iq = set_iq(
        "block-1",
        Element::builder("block", NS_BLOCKING)
            .append(blocking_item("romeo@montague.net"))
            .build(),
    );

    assert!(is_blocking_query(&iq));
    assert!(!is_blocklist_get(&iq));
    assert!(is_block_set(&iq));
    assert!(!is_unblock_set(&iq));
}

#[test]
fn test_is_blocking_query_unblock() {
    let iq = set_iq(
        "unblock-1",
        Element::builder("unblock", NS_BLOCKING)
            .append(blocking_item("romeo@montague.net"))
            .build(),
    );

    assert!(is_blocking_query(&iq));
    assert!(!is_blocklist_get(&iq));
    assert!(!is_block_set(&iq));
    assert!(is_unblock_set(&iq));
}

#[test]
fn test_is_not_blocking_query_wrong_ns() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "test-1".to_string(),
        payload: Element::builder("blocklist", "wrong:namespace").build(),
    };

    assert!(!is_blocking_query(&iq));
}

#[test]
fn test_parse_blocklist_get() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "blocklist-1".to_string(),
        payload: Element::builder("blocklist", NS_BLOCKING).build(),
    };

    let request = parse_blocking_request(&iq).unwrap();
    assert!(matches!(request, BlockingRequest::GetBlocklist));
}

#[test]
fn test_parse_block_request_uses_typed_jids() {
    let iq = set_iq(
        "block-1",
        Element::builder("block", NS_BLOCKING)
            .append(blocking_item("romeo@montague.net"))
            .append(blocking_item("iago@shakespeare.lit"))
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap(),
        BlockingRequest::Block(vec![jid("romeo@montague.net"), jid("iago@shakespeare.lit")])
    );
}

#[test]
fn test_parse_block_request_accepts_bare_full_and_domain_jids() {
    let iq = set_iq(
        "block-typed-jids",
        Element::builder("block", NS_BLOCKING)
            .append(blocking_item("romeo@montague.net"))
            .append(blocking_item("romeo@montague.net/balcony"))
            .append(blocking_item("montague.net"))
            .append(blocking_item("montague.net/courtyard"))
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap(),
        BlockingRequest::Block(vec![
            jid("romeo@montague.net"),
            jid("romeo@montague.net/balcony"),
            jid("montague.net"),
            jid("montague.net/courtyard")
        ])
    );
}

#[test]
fn test_parse_block_request_rejects_malformed_jid() {
    let iq = set_iq(
        "block-bad-jid",
        Element::builder("block", NS_BLOCKING)
            .append(blocking_item("@@"))
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::MalformedItemJid)
    );
}

#[test]
fn test_parse_block_request_rejects_missing_jid_attribute() {
    let iq = set_iq(
        "block-missing-jid",
        Element::builder("block", NS_BLOCKING)
            .append(Element::builder("item", NS_BLOCKING).build())
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::MissingItemJid)
    );
}

#[test]
fn test_parse_block_request_rejects_item_text() {
    let iq = set_iq(
        "block-item-text",
        Element::builder("block", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "romeo@montague.net",
                    )
                    .append("not empty")
                    .build(),
            )
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::UnexpectedChild)
    );
}

#[test]
fn test_parse_block_request_rejects_item_whitespace() {
    let iq = set_iq(
        "block-item-whitespace",
        Element::builder("block", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "romeo@montague.net",
                    )
                    .append(" ")
                    .build(),
            )
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::UnexpectedChild)
    );
}

#[test]
fn test_parse_block_request_rejects_item_child() {
    let iq = set_iq(
        "block-item-child",
        Element::builder("block", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "romeo@montague.net",
                    )
                    .append(Element::builder("extra", NS_BLOCKING).build())
                    .build(),
            )
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::UnexpectedChild)
    );
}

#[test]
fn test_parse_unblock_rejects_unknown_child_instead_of_unblock_all() {
    let iq = set_iq(
        "unblock-bad-child",
        Element::builder("unblock", NS_BLOCKING)
            .append(Element::builder("foo", NS_BLOCKING).build())
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::UnexpectedChild)
    );
}

#[test]
fn test_parse_unblock_rejects_wrong_namespace_item_instead_of_unblock_all() {
    let iq = set_iq(
        "unblock-wrong-ns-item",
        Element::builder("unblock", NS_BLOCKING)
            .append(
                Element::builder("item", "urn:example:wrong")
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "romeo@montague.net",
                    )
                    .build(),
            )
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::UnexpectedChild)
    );
}

#[test]
fn test_parse_unblock_request() {
    let iq = set_iq(
        "unblock-1",
        Element::builder("unblock", NS_BLOCKING)
            .append(blocking_item("romeo@montague.net"))
            .build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap(),
        BlockingRequest::Unblock(vec![jid("romeo@montague.net")])
    );
}

#[test]
fn test_parse_unblock_all_request() {
    let iq = set_iq(
        "unblock-all-1",
        Element::builder("unblock", NS_BLOCKING).build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap(),
        BlockingRequest::Unblock(Vec::new())
    );
}

#[test]
fn test_parse_empty_block_request_error() {
    let iq = set_iq(
        "block-empty-1",
        Element::builder("block", NS_BLOCKING).build(),
    );

    assert_eq!(
        parse_blocking_request(&iq).unwrap_err(),
        BlockingError::BadRequest(BlockingBadRequest::MissingBlockItem)
    );
}

#[test]
fn test_build_blocklist_response() {
    let original_iq = Iq::Get {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("server.example.com".parse().unwrap()),
        id: "blocklist-get-1".to_string(),
        payload: Element::builder("blocklist", NS_BLOCKING).build(),
    };
    let blocked_jids = vec![jid("romeo@montague.net"), jid("iago@shakespeare.lit")];

    let response = build_blocklist_response(&original_iq, &blocked_jids);

    assert_eq!(response.id(), "blocklist-get-1");
    if let Iq::Result {
        payload: Some(elem),
        ..
    } = &response
    {
        assert_eq!(elem.name(), "blocklist");
        assert_eq!(elem.ns(), NS_BLOCKING);

        let items: Vec<_> = elem.children().collect();
        assert_eq!(items.len(), 2);

        let jids: Vec<_> = items.iter().filter_map(|item| item.attr("jid")).collect();
        assert_eq!(jids, vec!["romeo@montague.net", "iago@shakespeare.lit"]);
    } else {
        panic!("Expected Result with blocklist element");
    }
}

#[tokio::test]
async fn in_memory_blocking_storage_preserves_stored_jid_forms() {
    let storage = InMemoryBlockingStorage::new();
    let user: BareJid = "alice@example.com".parse().unwrap();
    let entries = vec![jid("bob@example.com/phone"), jid("blocked.example.com")];

    storage.set_blocklist_jids(user.clone(), entries.clone());

    assert_eq!(
        storage
            .list_blocked_jid_entries(&user)
            .await
            .expect("stored JID entries"),
        entries
    );
    assert!(storage
        .list_blocked_jids(&user)
        .await
        .expect("bare JID snapshot")
        .contains(&"blocked.example.com".parse().unwrap()));
}

#[test]
fn test_build_empty_blocklist_response() {
    let original_iq = Iq::Get {
        from: Some("user@example.com".parse().unwrap()),
        to: None,
        id: "blocklist-get-2".to_string(),
        payload: Element::builder("blocklist", NS_BLOCKING).build(),
    };

    let response = build_blocklist_response(&original_iq, &[]);

    assert_eq!(response.id(), "blocklist-get-2");
    if let Iq::Result {
        payload: Some(elem),
        ..
    } = &response
    {
        assert_eq!(elem.name(), "blocklist");
        assert!(elem.children().next().is_none());
    } else {
        panic!("Expected Result with empty blocklist element");
    }
}

#[test]
fn test_build_blocking_success() {
    let original_iq = set_iq(
        "block-set-1",
        Element::builder("block", NS_BLOCKING).build(),
    );

    let response = build_blocking_success(&original_iq);

    assert_eq!(response.id(), "block-set-1");
    assert!(matches!(response, Iq::Result { payload: None, .. }));
}

#[test]
fn test_build_block_push() {
    let blocked_jids = vec![jid("romeo@montague.net")];
    let to: Jid = "user@example.com/resource".parse().expect("valid jid");
    let push = build_block_push(&to, &blocked_jids).expect("non-empty block push");

    assert!(uuid::Uuid::parse_str(push.id()).is_ok());
    if let Iq::Set { payload: elem, .. } = &push {
        assert_eq!(elem.name(), "block");
        assert_eq!(elem.ns(), NS_BLOCKING);
        let items: Vec<_> = elem.children().collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].attr("jid"), Some("romeo@montague.net"));
    } else {
        panic!("Expected Set with block element");
    }
}

#[test]
fn test_build_block_push_rejects_empty_items() {
    let to: Jid = "user@example.com/resource".parse().expect("valid jid");
    assert_eq!(
        build_block_push(&to, &[]),
        Err(BlockingError::BadRequest(
            BlockingBadRequest::MissingBlockItem
        ))
    );
}

#[test]
fn test_build_unblock_push() {
    let unblocked_jids = vec![jid("romeo@montague.net")];
    let to: Jid = "user@example.com/resource".parse().expect("valid jid");
    let push = build_unblock_push(&to, &unblocked_jids);

    assert!(uuid::Uuid::parse_str(push.id()).is_ok());
    if let Iq::Set { payload: elem, .. } = &push {
        assert_eq!(elem.name(), "unblock");
        assert_eq!(elem.ns(), NS_BLOCKING);
        let items: Vec<_> = elem.children().collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].attr("jid"), Some("romeo@montague.net"));
    } else {
        panic!("Expected Set with unblock element");
    }
}

#[test]
fn test_build_unblock_all_push_is_empty_unblock() {
    let to: Jid = "user@example.com/resource".parse().expect("valid jid");
    let push = build_unblock_push(&to, &[]);

    if let Iq::Set { payload: elem, .. } = &push {
        assert_eq!(elem.name(), "unblock");
        assert_eq!(elem.ns(), NS_BLOCKING);
        assert!(elem.children().next().is_none());
    } else {
        panic!("Expected Set with unblock element");
    }
}

#[test]
fn test_build_blocking_error_is_typed_iq_error() {
    let original_iq = set_iq("error-1", Element::builder("block", NS_BLOCKING).build());

    let error_response = build_blocking_error(
        &original_iq,
        &BlockingError::BadRequest(BlockingBadRequest::MissingBlockItem),
    );

    if let Iq::Error { error, payload, .. } = error_response {
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
        assert!(payload.is_some());
    } else {
        panic!("Expected typed IQ error");
    }
}

#[test]
fn test_blocking_error_kind_mappings() {
    assert_eq!(
        BlockingError::NotAuthorized.stanza_error_kind(),
        (ErrorType::Auth, DefinedCondition::NotAuthorized)
    );
    assert_eq!(
        BlockingError::InternalError.stanza_error_kind(),
        (ErrorType::Wait, DefinedCondition::InternalServerError)
    );
    assert_eq!(
        BlockingError::ItemNotFound.stanza_error_kind(),
        (ErrorType::Cancel, DefinedCondition::ItemNotFound)
    );
}

#[test]
fn test_blocking_error_display() {
    assert_eq!(
        BlockingError::BadRequest(BlockingBadRequest::MissingBlockItem).to_string(),
        "Bad request: block request must contain at least one item"
    );
    assert_eq!(BlockingError::NotAuthorized.to_string(), "Not authorized");
    assert_eq!(BlockingError::InternalError.to_string(), "Internal error");
    assert_eq!(BlockingError::ItemNotFound.to_string(), "Item not found");
}

#[test]
fn xep0191_module_has_no_manual_error_xml_or_escape_helper() {
    let source = include_str!("../xep0191.rs");

    assert!(!source.contains("escape_xml"));
    assert!(!source.contains("pub fn build_blocking_error(request_id: &str"));
    assert!(!source.contains("<iq type='error'"));
}
