use super::*;

#[test]
fn test_is_blocking_query_get_blocklist() {
    let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "blocklist-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
    };

    assert!(is_blocking_query(&iq));
    assert!(is_blocklist_get(&iq));
    assert!(!is_block_set(&iq));
    assert!(!is_unblock_set(&iq));
}

#[test]
fn test_is_blocking_query_block() {
    let block_elem = Element::builder("block", NS_BLOCKING)
        .append(
            Element::builder("item", NS_BLOCKING)
                .attr("jid", "romeo@montague.net")
                .build(),
        )
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "block-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(block_elem),
    };

    assert!(is_blocking_query(&iq));
    assert!(!is_blocklist_get(&iq));
    assert!(is_block_set(&iq));
    assert!(!is_unblock_set(&iq));
}

#[test]
fn test_is_blocking_query_unblock() {
    let unblock_elem = Element::builder("unblock", NS_BLOCKING)
        .append(
            Element::builder("item", NS_BLOCKING)
                .attr("jid", "romeo@montague.net")
                .build(),
        )
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "unblock-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(unblock_elem),
    };

    assert!(is_blocking_query(&iq));
    assert!(!is_blocklist_get(&iq));
    assert!(!is_block_set(&iq));
    assert!(is_unblock_set(&iq));
}

#[test]
fn test_is_not_blocking_query_wrong_ns() {
    let elem = Element::builder("blocklist", "wrong:namespace").build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(elem),
    };

    assert!(!is_blocking_query(&iq));
}

#[test]
fn test_parse_blocklist_get() {
    let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "blocklist-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
    };

    let request = parse_blocking_request(&iq).unwrap();
    assert!(matches!(request, BlockingRequest::GetBlocklist));
}

#[test]
fn test_parse_block_request() {
    let block_elem = Element::builder("block", NS_BLOCKING)
        .append(
            Element::builder("item", NS_BLOCKING)
                .attr("jid", "romeo@montague.net")
                .build(),
        )
        .append(
            Element::builder("item", NS_BLOCKING)
                .attr("jid", "iago@shakespeare.lit")
                .build(),
        )
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "block-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(block_elem),
    };

    let request = parse_blocking_request(&iq).unwrap();
    match request {
        BlockingRequest::Block(jids) => {
            assert_eq!(jids.len(), 2);
            assert!(jids.contains(&"romeo@montague.net".to_string()));
            assert!(jids.contains(&"iago@shakespeare.lit".to_string()));
        }
        _ => panic!("Expected Block request"),
    }
}

#[test]
fn test_parse_unblock_request() {
    let unblock_elem = Element::builder("unblock", NS_BLOCKING)
        .append(
            Element::builder("item", NS_BLOCKING)
                .attr("jid", "romeo@montague.net")
                .build(),
        )
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "unblock-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(unblock_elem),
    };

    let request = parse_blocking_request(&iq).unwrap();
    match request {
        BlockingRequest::Unblock(jids) => {
            assert_eq!(jids.len(), 1);
            assert_eq!(jids[0], "romeo@montague.net");
        }
        _ => panic!("Expected Unblock request"),
    }
}

#[test]
fn test_parse_unblock_all_request() {
    let unblock_elem = Element::builder("unblock", NS_BLOCKING).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "unblock-all-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(unblock_elem),
    };

    let request = parse_blocking_request(&iq).unwrap();
    match request {
        BlockingRequest::Unblock(jids) => {
            assert!(jids.is_empty());
        }
        _ => panic!("Expected Unblock request"),
    }
}

#[test]
fn test_parse_empty_block_request_error() {
    let block_elem = Element::builder("block", NS_BLOCKING).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "block-empty-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(block_elem),
    };

    let result = parse_blocking_request(&iq);
    assert!(result.is_err());
    match result.unwrap_err() {
        BlockingError::BadRequest(msg) => {
            assert!(msg.contains("at least one item"));
        }
        _ => panic!("Expected BadRequest error"),
    }
}

#[test]
fn test_build_blocklist_response() {
    let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("server.example.com".parse().unwrap()),
        id: "blocklist-get-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
    };

    let blocked_jids = vec![
        "romeo@montague.net".to_string(),
        "iago@shakespeare.lit".to_string(),
    ];

    let response = build_blocklist_response(&original_iq, &blocked_jids);

    assert_eq!(response.id, "blocklist-get-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(Some(_))
    ));

    if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
        assert_eq!(elem.name(), "blocklist");
        assert_eq!(elem.ns(), NS_BLOCKING);

        let items: Vec<_> = elem.children().collect();
        assert_eq!(items.len(), 2);

        let jids: Vec<_> = items.iter().filter_map(|item| item.attr("jid")).collect();
        assert!(jids.contains(&"romeo@montague.net"));
        assert!(jids.contains(&"iago@shakespeare.lit"));
    } else {
        panic!("Expected Result with blocklist element");
    }
}

#[tokio::test]
async fn in_memory_blocking_storage_preserves_stored_jid_forms() {
    let storage = InMemoryBlockingStorage::new();
    let user: BareJid = "alice@example.com".parse().unwrap();
    let entries: Vec<Jid> = vec![
        "bob@example.com/phone".parse().unwrap(),
        "blocked.example.com".parse().unwrap(),
    ];

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
    let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: None,
        id: "blocklist-get-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
    };

    let response = build_blocklist_response(&original_iq, &[]);

    assert_eq!(response.id, "blocklist-get-2");
    if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
        assert_eq!(elem.name(), "blocklist");
        assert!(elem.children().next().is_none());
    } else {
        panic!("Expected Result with empty blocklist element");
    }
}

#[test]
fn test_build_blocking_success() {
    let block_elem = Element::builder("block", NS_BLOCKING).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: None,
        id: "block-set-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(block_elem),
    };

    let response = build_blocking_success(&original_iq);

    assert_eq!(response.id, "block-set-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(None)
    ));
}

#[test]
fn test_build_block_push() {
    let blocked_jids = vec!["romeo@montague.net".to_string()];
    let to: jid::Jid = "user@example.com/resource".parse().expect("valid jid");
    let push = build_block_push(&to, &blocked_jids);

    assert!(push.id.starts_with("push-block-"));
    if let xmpp_parsers::iq::IqType::Set(elem) = &push.payload {
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
fn test_build_unblock_push() {
    let unblocked_jids = vec!["romeo@montague.net".to_string()];
    let to: jid::Jid = "user@example.com/resource".parse().expect("valid jid");
    let push = build_unblock_push(&to, &unblocked_jids);

    assert!(push.id.starts_with("push-unblock-"));
    if let xmpp_parsers::iq::IqType::Set(elem) = &push.payload {
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
fn test_build_blocking_error() {
    let error_response = build_blocking_error(
        "error-1",
        &BlockingError::BadRequest("Invalid JID".to_string()),
    );

    assert!(error_response.contains("type='error'"));
    assert!(error_response.contains("id='error-1'"));
    assert!(error_response.contains("<bad-request"));
    assert!(error_response.contains("Invalid JID"));
}

#[test]
fn test_blocking_error_display() {
    assert_eq!(
        BlockingError::BadRequest("test".to_string()).to_string(),
        "Bad request: test"
    );
    assert_eq!(BlockingError::NotAuthorized.to_string(), "Not authorized");
    assert_eq!(
        BlockingError::InternalError("err".to_string()).to_string(),
        "Internal error: err"
    );
    assert_eq!(
        BlockingError::ItemNotFound("jid".to_string()).to_string(),
        "Item not found: jid"
    );
}
