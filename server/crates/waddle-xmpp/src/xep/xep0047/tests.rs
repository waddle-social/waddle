use super::*;

fn make_iq_set(child: Element) -> Iq {
    Iq {
        from: Some("romeo@montague.net/orchard".parse().expect("valid JID")),
        to: Some("juliet@capulet.com/balcony".parse().expect("valid JID")),
        id: "test-1".to_string(),
        payload: IqType::Set(child),
    }
}

fn make_iq_get(child: Element) -> Iq {
    Iq {
        from: Some("romeo@montague.net/orchard".parse().expect("valid JID")),
        to: Some("juliet@capulet.com/balcony".parse().expect("valid JID")),
        id: "test-1".to_string(),
        payload: IqType::Get(child),
    }
}

// =========================================================================
// Detection tests
// =========================================================================

#[test]
fn test_is_ibb_open() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "test-sid")
        .attr("block-size", "4096")
        .build();
    let iq = make_iq_set(elem);
    assert!(is_ibb_open(&iq));
}

#[test]
fn test_is_ibb_open_false_for_get() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "test-sid")
        .attr("block-size", "4096")
        .build();
    let iq = make_iq_get(elem);
    assert!(!is_ibb_open(&iq));
}

#[test]
fn test_is_ibb_open_false_for_wrong_ns() {
    let elem = Element::builder("open", "some:other:ns")
        .attr("sid", "test-sid")
        .build();
    let iq = make_iq_set(elem);
    assert!(!is_ibb_open(&iq));
}

#[test]
fn test_is_ibb_data() {
    let elem = Element::builder("data", NS_IBB)
        .attr("sid", "test-sid")
        .attr("seq", "0")
        .build();
    let iq = make_iq_set(elem);
    assert!(is_ibb_data(&iq));
}

#[test]
fn test_is_ibb_close() {
    let elem = Element::builder("close", NS_IBB)
        .attr("sid", "test-sid")
        .build();
    let iq = make_iq_set(elem);
    assert!(is_ibb_close(&iq));
}

#[test]
fn test_message_has_ibb_data() {
    let data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "test-sid")
        .attr("seq", "0")
        .build();
    let msg = Element::builder("message", "jabber:client")
        .append(data_elem)
        .build();
    assert!(message_has_ibb_data(&msg));
}

#[test]
fn test_message_has_no_ibb_data() {
    let msg = Element::builder("message", "jabber:client")
        .append(Element::builder("body", "jabber:client").build())
        .build();
    assert!(!message_has_ibb_data(&msg));
}

// =========================================================================
// Parsing tests
// =========================================================================

#[test]
fn test_parse_ibb_open_default_stanza() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "i781hf64")
        .attr("block-size", "4096")
        .build();
    let iq = make_iq_set(elem);

    let open = parse_ibb_open(&iq).expect("should parse");
    assert_eq!(open.sid, "i781hf64");
    assert_eq!(open.block_size, 4096);
    assert_eq!(open.stanza, StanzaType::Iq);
}

#[test]
fn test_parse_ibb_open_message_stanza() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "sess1")
        .attr("block-size", "1024")
        .attr("stanza", "message")
        .build();
    let iq = make_iq_set(elem);

    let open = parse_ibb_open(&iq).expect("should parse");
    assert_eq!(open.stanza, StanzaType::Message);
}

#[test]
fn test_parse_ibb_open_max_block_size() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "max-test")
        .attr("block-size", "65535")
        .build();
    let iq = make_iq_set(elem);

    let open = parse_ibb_open(&iq).expect("should parse");
    assert_eq!(open.block_size, 65535);
}

#[test]
fn test_parse_ibb_open_block_size_too_large() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "big")
        .attr("block-size", "65536")
        .build();
    let iq = make_iq_set(elem);

    let err = parse_ibb_open(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::BlockSizeTooLarge(65536));
}

#[test]
fn test_parse_ibb_open_zero_block_size() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "zero")
        .attr("block-size", "0")
        .build();
    let iq = make_iq_set(elem);

    let err = parse_ibb_open(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::InvalidBlockSize);
}

#[test]
fn test_parse_ibb_open_missing_sid() {
    let elem = Element::builder("open", NS_IBB)
        .attr("block-size", "4096")
        .build();
    let iq = make_iq_set(elem);

    let err = parse_ibb_open(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::MissingSid);
}

#[test]
fn test_parse_ibb_open_missing_block_size() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "no-bs")
        .build();
    let iq = make_iq_set(elem);

    let err = parse_ibb_open(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::InvalidBlockSize);
}

#[test]
fn test_parse_ibb_open_invalid_stanza_type() {
    let elem = Element::builder("open", NS_IBB)
        .attr("sid", "bad-stanza")
        .attr("block-size", "4096")
        .attr("stanza", "presence")
        .build();
    let iq = make_iq_set(elem);

    let err = parse_ibb_open(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::InvalidStanzaType("presence".to_string()));
}

#[test]
fn test_parse_ibb_data() {
    let raw_data = b"Hello, World!";
    let encoded = BASE64.encode(raw_data);

    let mut data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "i781hf64")
        .attr("seq", "0")
        .build();
    data_elem.append_text_node(encoded);

    let iq = make_iq_set(data_elem);
    let data = parse_ibb_data_from_iq(&iq).expect("should parse");

    assert_eq!(data.sid, "i781hf64");
    assert_eq!(data.seq, 0);
    assert_eq!(data.data, raw_data);
}

#[test]
fn test_parse_ibb_data_empty() {
    let data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "empty-test")
        .attr("seq", "5")
        .build();
    let iq = make_iq_set(data_elem);

    let data = parse_ibb_data_from_iq(&iq).expect("should parse");
    assert_eq!(data.sid, "empty-test");
    assert_eq!(data.seq, 5);
    assert!(data.data.is_empty());
}

#[test]
fn test_parse_ibb_data_max_seq() {
    let data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "seq-test")
        .attr("seq", "65535")
        .build();
    let iq = make_iq_set(data_elem);

    let data = parse_ibb_data_from_iq(&iq).expect("should parse");
    assert_eq!(data.seq, 65535);
}

#[test]
fn test_parse_ibb_data_missing_sid() {
    let data_elem = Element::builder("data", NS_IBB).attr("seq", "0").build();
    let iq = make_iq_set(data_elem);

    let err = parse_ibb_data_from_iq(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::MissingSid);
}

#[test]
fn test_parse_ibb_data_missing_seq() {
    let data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "no-seq")
        .build();
    let iq = make_iq_set(data_elem);

    let err = parse_ibb_data_from_iq(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::InvalidSeq);
}

#[test]
fn test_parse_ibb_data_invalid_base64() {
    let mut data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "bad-b64")
        .attr("seq", "0")
        .build();
    data_elem.append_text_node("not-valid-base64!!!");

    let iq = make_iq_set(data_elem);
    let err = parse_ibb_data_from_iq(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::InvalidBase64);
}

#[test]
fn test_parse_ibb_data_from_message() {
    let raw_data = b"message transport";
    let encoded = BASE64.encode(raw_data);

    let mut data_elem = Element::builder("data", NS_IBB)
        .attr("sid", "msg-test")
        .attr("seq", "3")
        .build();
    data_elem.append_text_node(encoded);

    let msg = Element::builder("message", "jabber:client")
        .append(data_elem)
        .build();

    let data = parse_ibb_data_from_message(&msg).expect("should parse");
    assert_eq!(data.sid, "msg-test");
    assert_eq!(data.seq, 3);
    assert_eq!(data.data, raw_data);
}

#[test]
fn test_parse_ibb_close() {
    let close_elem = Element::builder("close", NS_IBB)
        .attr("sid", "i781hf64")
        .build();
    let iq = make_iq_set(close_elem);

    let close = parse_ibb_close(&iq).expect("should parse");
    assert_eq!(close.sid, "i781hf64");
}

#[test]
fn test_parse_ibb_close_missing_sid() {
    let close_elem = Element::builder("close", NS_IBB).build();
    let iq = make_iq_set(close_elem);

    let err = parse_ibb_close(&iq).expect_err("should fail");
    assert_eq!(err, IbbError::MissingSid);
}

// =========================================================================
// Building tests
// =========================================================================

#[test]
fn test_build_ibb_result() {
    let open_elem = Element::builder("open", NS_IBB)
        .attr("sid", "test")
        .attr("block-size", "4096")
        .build();
    let iq = make_iq_set(open_elem);

    let result = build_ibb_result(&iq);
    assert_eq!(result.id, "test-1");
    assert_eq!(result.from, iq.to);
    assert_eq!(result.to, iq.from);
    assert!(matches!(result.payload, IqType::Result(None)));
}

#[test]
fn test_build_ibb_open() {
    let iq = build_ibb_open(
        Some("romeo@montague.net/orchard".parse().expect("valid JID")),
        Some("juliet@capulet.com/balcony".parse().expect("valid JID")),
        "open-1",
        "session-abc",
        4096,
        StanzaType::Iq,
    );

    assert_eq!(iq.id, "open-1");
    if let IqType::Set(elem) = &iq.payload {
        assert_eq!(elem.name(), "open");
        assert_eq!(elem.ns(), NS_IBB);
        assert_eq!(elem.attr("sid"), Some("session-abc"));
        assert_eq!(elem.attr("block-size"), Some("4096"));
        assert_eq!(elem.attr("stanza"), Some("iq"));
    } else {
        panic!("Expected IQ set");
    }
}

#[test]
fn test_build_ibb_data_iq() {
    let raw = b"Hello!";
    let iq = build_ibb_data_iq(
        Some("romeo@montague.net".parse().expect("valid JID")),
        Some("juliet@capulet.com".parse().expect("valid JID")),
        "data-1",
        "session-abc",
        0,
        raw,
    );

    if let IqType::Set(elem) = &iq.payload {
        assert_eq!(elem.name(), "data");
        assert_eq!(elem.ns(), NS_IBB);
        assert_eq!(elem.attr("sid"), Some("session-abc"));
        assert_eq!(elem.attr("seq"), Some("0"));

        let decoded = BASE64.decode(elem.text().trim()).expect("valid base64");
        assert_eq!(decoded, raw);
    } else {
        panic!("Expected IQ set");
    }
}

#[test]
fn test_build_ibb_data_element() {
    let raw = b"chunk data";
    let elem = build_ibb_data_element("sid-1", 42, raw);

    assert_eq!(elem.name(), "data");
    assert_eq!(elem.ns(), NS_IBB);
    assert_eq!(elem.attr("sid"), Some("sid-1"));
    assert_eq!(elem.attr("seq"), Some("42"));

    let decoded = BASE64.decode(elem.text().trim()).expect("valid base64");
    assert_eq!(decoded, raw);
}

#[test]
fn test_build_ibb_close() {
    let iq = build_ibb_close(
        Some("romeo@montague.net".parse().expect("valid JID")),
        Some("juliet@capulet.com".parse().expect("valid JID")),
        "close-1",
        "session-abc",
    );

    if let IqType::Set(elem) = &iq.payload {
        assert_eq!(elem.name(), "close");
        assert_eq!(elem.ns(), NS_IBB);
        assert_eq!(elem.attr("sid"), Some("session-abc"));
    } else {
        panic!("Expected IQ set");
    }
}

// =========================================================================
// Error response tests
// =========================================================================

#[test]
fn test_build_ibb_not_acceptable() {
    let open_elem = Element::builder("open", NS_IBB)
        .attr("sid", "rejected")
        .attr("block-size", "4096")
        .build();
    let iq = make_iq_set(open_elem);

    let err_iq = build_ibb_not_acceptable(&iq);
    assert_eq!(err_iq.id, "test-1");
    assert!(matches!(err_iq.payload, IqType::Error(_)));
}

#[test]
fn test_build_ibb_resource_constraint() {
    let open_elem = Element::builder("open", NS_IBB)
        .attr("sid", "too-big")
        .attr("block-size", "65535")
        .build();
    let iq = make_iq_set(open_elem);

    let err_iq = build_ibb_resource_constraint(&iq);
    assert!(matches!(err_iq.payload, IqType::Error(_)));
}

#[test]
fn test_build_ibb_item_not_found() {
    let close_elem = Element::builder("close", NS_IBB)
        .attr("sid", "unknown")
        .build();
    let iq = make_iq_set(close_elem);

    let err_iq = build_ibb_item_not_found(&iq);
    assert!(matches!(err_iq.payload, IqType::Error(_)));
}

// =========================================================================
// Validation tests
// =========================================================================

#[test]
fn test_validate_data_size_ok() {
    let data = IbbData {
        sid: "test".to_string(),
        seq: 0,
        data: vec![0u8; 4096],
    };
    assert!(validate_data_size(&data, 4096).is_ok());
}

#[test]
fn test_validate_data_size_too_large() {
    let data = IbbData {
        sid: "test".to_string(),
        seq: 0,
        data: vec![0u8; 4097],
    };
    let err = validate_data_size(&data, 4096).expect_err("should fail");
    assert_eq!(
        err,
        IbbError::DataTooLarge {
            actual: 4097,
            limit: 4096
        }
    );
}

#[test]
fn test_validate_data_size_empty() {
    let data = IbbData {
        sid: "test".to_string(),
        seq: 0,
        data: Vec::new(),
    };
    assert!(validate_data_size(&data, 4096).is_ok());
}

// =========================================================================
// Sequence number tests
// =========================================================================

#[test]
fn test_next_seq_normal() {
    assert_eq!(next_seq(0), 1);
    assert_eq!(next_seq(100), 101);
}

#[test]
fn test_next_seq_wraps() {
    assert_eq!(next_seq(65535), 0);
}

// =========================================================================
// StanzaType tests
// =========================================================================

#[test]
fn test_stanza_type_from_attr() {
    assert_eq!(StanzaType::from_attr(None).expect("ok"), StanzaType::Iq);
    assert_eq!(
        StanzaType::from_attr(Some("iq")).expect("ok"),
        StanzaType::Iq
    );
    assert_eq!(
        StanzaType::from_attr(Some("message")).expect("ok"),
        StanzaType::Message
    );
    assert!(StanzaType::from_attr(Some("presence")).is_err());
}

#[test]
fn test_stanza_type_as_str() {
    assert_eq!(StanzaType::Iq.as_str(), "iq");
    assert_eq!(StanzaType::Message.as_str(), "message");
}

// =========================================================================
// IbbError Display / From<IbbError> tests
// =========================================================================

#[test]
fn test_ibb_error_display() {
    assert!(IbbError::MissingSid
        .to_string()
        .contains("missing session ID"));
    assert!(IbbError::InvalidBase64.to_string().contains("base64"));
    assert!(IbbError::BlockSizeTooLarge(70000)
        .to_string()
        .contains("70000"));
}

#[test]
fn test_ibb_error_to_xmpp_error() {
    let xmpp_err: XmppError = IbbError::MissingSid.into();
    assert!(matches!(
        xmpp_err,
        XmppError::Stanza {
            condition: crate::StanzaErrorCondition::BadRequest,
            ..
        }
    ));

    let xmpp_err: XmppError = IbbError::BlockSizeTooLarge(99999).into();
    assert!(matches!(
        xmpp_err,
        XmppError::Stanza {
            condition: crate::StanzaErrorCondition::NotAcceptable,
            ..
        }
    ));
}

// =========================================================================
// Roundtrip tests
// =========================================================================

#[test]
fn test_roundtrip_open() {
    let iq = build_ibb_open(
        Some("alice@example.com".parse().expect("valid JID")),
        Some("bob@example.com".parse().expect("valid JID")),
        "rt-1",
        "roundtrip-sid",
        8192,
        StanzaType::Message,
    );

    let parsed = parse_ibb_open(&iq).expect("should parse");
    assert_eq!(parsed.sid, "roundtrip-sid");
    assert_eq!(parsed.block_size, 8192);
    assert_eq!(parsed.stanza, StanzaType::Message);
}

#[test]
fn test_roundtrip_data() {
    let payload = b"The quick brown fox jumps over the lazy dog";
    let iq = build_ibb_data_iq(
        Some("alice@example.com".parse().expect("valid JID")),
        Some("bob@example.com".parse().expect("valid JID")),
        "rt-2",
        "roundtrip-sid",
        42,
        payload,
    );

    let parsed = parse_ibb_data_from_iq(&iq).expect("should parse");
    assert_eq!(parsed.sid, "roundtrip-sid");
    assert_eq!(parsed.seq, 42);
    assert_eq!(parsed.data, payload);
}

#[test]
fn test_roundtrip_close() {
    let iq = build_ibb_close(
        Some("alice@example.com".parse().expect("valid JID")),
        Some("bob@example.com".parse().expect("valid JID")),
        "rt-3",
        "roundtrip-sid",
    );

    let parsed = parse_ibb_close(&iq).expect("should parse");
    assert_eq!(parsed.sid, "roundtrip-sid");
}

#[test]
fn test_roundtrip_data_element_in_message() {
    let payload = b"message-based data";
    let data_elem = build_ibb_data_element("msg-sid", 7, payload);

    let msg = Element::builder("message", "jabber:client")
        .append(data_elem)
        .build();

    assert!(message_has_ibb_data(&msg));
    let parsed = parse_ibb_data_from_message(&msg).expect("should parse");
    assert_eq!(parsed.sid, "msg-sid");
    assert_eq!(parsed.seq, 7);
    assert_eq!(parsed.data, payload);
}
