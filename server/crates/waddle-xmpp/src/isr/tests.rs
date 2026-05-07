use super::*;
use chrono::Datelike;

#[test]
fn test_token_creation() {
    let token = IsrToken::new(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
        300,
    );

    assert!(!token.is_expired());
    assert!(token.remaining_secs() > 290);
    assert!(token.remaining_secs() <= 300);
    assert!(!token.token.is_empty());
}

#[test]
fn test_token_with_sm_state() {
    let token = IsrToken::with_sm_state(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
        300,
        "stream-123".to_string(),
        10,
        20,
    );

    assert_eq!(token.sm_stream_id, Some("stream-123".to_string()));
    assert_eq!(token.sm_inbound_count, 10);
    assert_eq!(token.sm_outbound_count, 20);
}

#[test]
fn test_token_xml() {
    let token = IsrToken::new(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
        300,
    );

    let xml = token.to_xml();
    assert!(xml.contains("xmlns='urn:xmpp:isr:0'"));
    assert!(xml.contains("expiry='"));
    assert!(xml.contains(&token.token));
}

#[test]
fn test_token_store_create_and_validate() {
    let store = IsrTokenStore::new();

    let token = store.create_token(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
    );

    // Should be able to validate
    let validated = store.validate_token(&token.token);
    assert!(validated.is_some());
    assert_eq!(validated.unwrap().user_id, "user-test123");
}

#[test]
fn test_token_store_consume() {
    let store = IsrTokenStore::new();

    let token = store.create_token(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
    );

    // First consume should succeed
    let consumed = store.consume_token(&token.token);
    assert!(consumed.is_some());

    // Second consume should fail
    let consumed_again = store.consume_token(&token.token);
    assert!(consumed_again.is_none());

    // Validation should also fail
    let validated = store.validate_token(&token.token);
    assert!(validated.is_none());
}

#[test]
fn test_token_store_refresh() {
    let store = IsrTokenStore::new();

    let old_token = store.create_token(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
    );
    let old_token_str = old_token.token.clone();

    // Refresh should return a new token
    let new_token = store.refresh_token(&old_token_str);
    assert!(new_token.is_some());
    let new_token = new_token.unwrap();

    // New token should be different
    assert_ne!(new_token.token, old_token_str);

    // Old token should be invalid
    assert!(store.validate_token(&old_token_str).is_none());

    // New token should be valid
    assert!(store.validate_token(&new_token.token).is_some());
}

#[test]
fn test_token_store_revoke_for_user_id() {
    let store = IsrTokenStore::new();

    // Create tokens for two different users
    let _token1 = store.create_token(
        "user-user1".to_string(),
        "user1@example.com".parse().unwrap(),
    );
    let token2 = store.create_token(
        "user-user2".to_string(),
        "user2@example.com".parse().unwrap(),
    );

    assert_eq!(store.token_count(), 2);

    // Revoke tokens for user1
    store.revoke_tokens_for_user_id("user-user1");

    assert_eq!(store.token_count(), 1);

    // user2's token should still be valid
    assert!(store.validate_token(&token2.token).is_some());
}

#[test]
fn test_parse_isr_token() {
    let xml = "<token xmlns='urn:xmpp:isr:0' expiry='2024-01-01T12:00:00Z'>test-token-123</token>";

    let result = parse_isr_token(xml);
    assert!(result.is_some());

    let (token, expiry) = result.unwrap();
    assert_eq!(token, "test-token-123");
    assert_eq!(expiry.year(), 2024);
}

#[test]
fn test_sasl_success_with_isr() {
    let token = IsrToken::new(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
        300,
    );

    let success = build_sasl_success_with_isr(&token);

    assert!(success.contains("<success"));
    assert!(success.contains("urn:ietf:params:xml:ns:xmpp-sasl"));
    assert!(success.contains("<token"));
    assert!(success.contains("urn:xmpp:isr:0"));
}

#[test]
fn test_update_sm_state() {
    let store = IsrTokenStore::new();

    let token = store.create_token_with_sm(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
        "stream-123".to_string(),
        0,
        0,
    );

    // Update SM state
    assert!(store.update_sm_state(&token.token, 10, 20));

    // Verify update
    let validated = store.validate_token(&token.token).unwrap();
    assert_eq!(validated.sm_inbound_count, 10);
    assert_eq!(validated.sm_outbound_count, 20);
}

#[test]
fn test_is_isr_token_request() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

    // Valid token-request IQ
    let token_request_elem = Element::builder("token-request", ISR_NS).build();
    let iq = Iq {
        from: Some("user@example.com/resource".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "token-1".to_string(),
        payload: IqType::Get(token_request_elem),
    };

    assert!(is_isr_token_request(&iq));
}

#[test]
fn test_is_not_isr_token_request_wrong_ns() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

    // Wrong namespace
    let token_request_elem = Element::builder("token-request", "wrong:ns").build();
    let iq = Iq {
        from: None,
        to: None,
        id: "token-1".to_string(),
        payload: IqType::Get(token_request_elem),
    };

    assert!(!is_isr_token_request(&iq));
}

#[test]
fn test_is_not_isr_token_request_wrong_element() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

    // Wrong element name
    let other_elem = Element::builder("other", ISR_NS).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "token-1".to_string(),
        payload: IqType::Get(other_elem),
    };

    assert!(!is_isr_token_request(&iq));
}

#[test]
fn test_is_not_isr_token_request_set_type() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

    // Set type instead of Get
    let token_request_elem = Element::builder("token-request", ISR_NS).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "token-1".to_string(),
        payload: IqType::Set(token_request_elem),
    };

    assert!(!is_isr_token_request(&iq));
}

#[test]
fn test_build_isr_token_result() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};

    let token_request_elem = Element::builder("token-request", ISR_NS).build();
    let original_iq = Iq {
        from: Some("user@example.com/resource".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "token-1".to_string(),
        payload: IqType::Get(token_request_elem),
    };

    let isr_token = IsrToken::new(
        "user-test123".to_string(),
        "user@example.com".parse().unwrap(),
        300,
    );

    let result = build_isr_token_result(&original_iq, &isr_token);

    assert_eq!(result.id, "token-1");
    assert_eq!(
        result.from.as_ref().map(|j| j.to_string()),
        Some("example.com".to_string())
    );
    assert_eq!(
        result.to.as_ref().map(|j| j.to_string()),
        Some("user@example.com/resource".to_string())
    );

    // Check the payload is a Result with a token element
    if let IqType::Result(Some(elem)) = &result.payload {
        assert_eq!(elem.name(), "token");
        assert_eq!(elem.ns(), ISR_NS);
        assert!(elem.attr("expiry").is_some());
        assert_eq!(elem.text(), isr_token.token);
    } else {
        panic!("Expected IqType::Result with Some(Element)");
    }
}

#[test]
fn test_build_isr_token_error() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

    let token_request_elem = Element::builder("token-request", ISR_NS).build();
    let original_iq = Iq {
        from: Some("user@example.com/resource".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "token-1".to_string(),
        payload: IqType::Get(token_request_elem),
    };

    let error = build_isr_token_error(&original_iq, "not-authorized");

    assert_eq!(error.id, "token-1");
    assert_eq!(
        error.from.as_ref().map(|j| j.to_string()),
        Some("example.com".to_string())
    );

    // Check the payload is an Error with the correct condition
    if let IqType::Error(stanza_error) = &error.payload {
        assert_eq!(stanza_error.type_, ErrorType::Auth);
        assert_eq!(
            stanza_error.defined_condition,
            DefinedCondition::NotAuthorized
        );
    } else {
        panic!("Expected IqType::Error");
    }
}

#[test]
fn test_build_isr_token_error_service_unavailable() {
    use minidom::Element;
    use xmpp_parsers::iq::{Iq, IqType};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

    let token_request_elem = Element::builder("token-request", ISR_NS).build();
    let original_iq = Iq {
        from: Some("user@example.com/resource".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "token-2".to_string(),
        payload: IqType::Get(token_request_elem),
    };

    let error = build_isr_token_error(&original_iq, "service-unavailable");

    // Check the payload is an Error with service-unavailable condition
    if let IqType::Error(stanza_error) = &error.payload {
        assert_eq!(stanza_error.type_, ErrorType::Cancel);
        assert_eq!(
            stanza_error.defined_condition,
            DefinedCondition::ServiceUnavailable
        );
    } else {
        panic!("Expected IqType::Error");
    }
}
