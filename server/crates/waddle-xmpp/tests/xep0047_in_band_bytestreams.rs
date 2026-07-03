//! XEP-0047: In-Band Bytestreams dedicated suite.
//!
//! Drives the full IQ session lifecycle (§2): open negotiation,
//! sequenced data transfer with block-size enforcement, sequence
//! wrap-around, and close — plus the typed error responses the
//! responder must emit for protocol violations.

use minidom::Element;
use waddle_xmpp::xep::{
    build_ibb_close, build_ibb_data_element, build_ibb_data_iq, build_ibb_item_not_found,
    build_ibb_not_acceptable, build_ibb_open, build_ibb_resource_constraint, build_ibb_result,
    build_ibb_unexpected_request, is_ibb_close, is_ibb_data, is_ibb_open, message_has_ibb_data,
    next_seq, parse_ibb_close, parse_ibb_data_from_iq, parse_ibb_data_from_message, parse_ibb_open,
    validate_data_size, IbbError, IbbStanzaType, NS_IBB,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const INITIATOR: &str = "romeo@montague.net/orchard";
const RESPONDER: &str = "juliet@capulet.com/balcony";

fn jid(s: &str) -> jid::Jid {
    s.parse().expect("valid jid")
}

/// Serialize an IQ to wire XML (jabber:client) and parse it back, so every
/// assertion below runs against the reparsed wire form, not the builder output.
fn wire_round_trip(iq: Iq) -> Iq {
    let elem = Element::from(iq);
    let xml = String::from(&elem);
    Iq::try_from(xml.parse::<Element>().expect("well-formed XML")).expect("valid IQ")
}

// ── Full IQ session lifecycle ────────────────────────────────────────────────

#[test]
fn xep0047_iq_session_open_data_close_lifecycle() {
    let sid = "i781hf64";
    let block_size = 4096;

    // 1. Initiator opens the session.
    let open_iq = wire_round_trip(build_ibb_open(
        Some(jid(INITIATOR)),
        Some(jid(RESPONDER)),
        "jn3h8g65",
        sid,
        block_size,
        IbbStanzaType::Iq,
    ));
    assert!(is_ibb_open(&open_iq));
    let open = parse_ibb_open(&open_iq).expect("valid open");
    assert_eq!(open.sid, sid);
    assert_eq!(open.block_size, block_size);
    assert_eq!(open.stanza, IbbStanzaType::Iq);

    // 2. Responder accepts with an empty IQ-result mirroring id and addressing.
    let ack = build_ibb_result(&open_iq);
    assert!(matches!(
        &ack,
        Iq::Result { payload: None, id, from, to }
            if id == "jn3h8g65"
                && from.as_ref() == Some(&jid(RESPONDER))
                && to.as_ref() == Some(&jid(INITIATOR))
    ));

    // 3. Initiator streams sequenced chunks; responder reassembles.
    let payload: Vec<u8> = (0u16..6000).map(|i| (i % 251) as u8).collect();
    let mut received = Vec::new();
    let mut expected_seq = 0u16;
    for (i, chunk) in payload.chunks(block_size as usize).enumerate() {
        let data_iq = wire_round_trip(build_ibb_data_iq(
            Some(jid(INITIATOR)),
            Some(jid(RESPONDER)),
            &format!("data-{i}"),
            sid,
            expected_seq,
            chunk,
        ));
        assert!(is_ibb_data(&data_iq));
        let data = parse_ibb_data_from_iq(&data_iq).expect("valid data");
        assert_eq!(data.sid, sid);
        assert_eq!(data.seq, expected_seq);
        validate_data_size(&data, block_size).expect("chunk within block-size");
        received.extend_from_slice(&data.data);
        expected_seq = next_seq(expected_seq);
    }
    assert_eq!(received, payload);
    assert_eq!(
        expected_seq, 2,
        "6000 bytes at 4096 block-size is two chunks"
    );

    // 4. Either party closes; the peer acks.
    let close_iq = wire_round_trip(build_ibb_close(
        Some(jid(INITIATOR)),
        Some(jid(RESPONDER)),
        "us71g45j",
        sid,
    ));
    assert!(is_ibb_close(&close_iq));
    let close = parse_ibb_close(&close_iq).expect("valid close");
    assert_eq!(close.sid, sid);
    let close_ack = build_ibb_result(&close_iq);
    assert!(matches!(close_ack, Iq::Result { id, .. } if id == "us71g45j"));
}

#[test]
fn xep0047_sequence_wraps_from_65535_to_zero() {
    // §2.2: seq is a 16-bit counter that wraps to 0 after 65535.
    assert_eq!(next_seq(65534), 65535);
    assert_eq!(next_seq(65535), 0);

    let iq = wire_round_trip(build_ibb_data_iq(
        Some(jid(INITIATOR)),
        Some(jid(RESPONDER)),
        "wrap-1",
        "sid-wrap",
        65535,
        b"tail",
    ));
    let data = parse_ibb_data_from_iq(&iq).expect("valid data");
    assert_eq!(data.seq, 65535);
    assert_eq!(next_seq(data.seq), 0);
}

// ── Open negotiation edge cases ──────────────────────────────────────────────

#[test]
fn xep0047_open_rejects_block_size_above_maximum() {
    let iq = build_ibb_open(None, None, "id", "sid", 65536, IbbStanzaType::Iq);
    assert_eq!(parse_ibb_open(&iq), Err(IbbError::BlockSizeTooLarge(65536)));
}

#[test]
fn xep0047_open_at_exact_maximum_block_size_is_accepted() {
    let iq = wire_round_trip(build_ibb_open(
        None,
        None,
        "id",
        "sid",
        65535,
        IbbStanzaType::Iq,
    ));
    let open = parse_ibb_open(&iq).expect("valid open");
    assert_eq!(open.block_size, 65535);
}

#[test]
fn xep0047_open_missing_stanza_attr_defaults_to_iq() {
    let xml = format!(
        "<iq xmlns='jabber:client' type='set' id='x' from='{INITIATOR}' to='{RESPONDER}'>\
         <open xmlns='{NS_IBB}' sid='s1' block-size='4096'/></iq>"
    );
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    let open = parse_ibb_open(&iq).expect("valid open");
    assert_eq!(open.stanza, IbbStanzaType::Iq);
}

#[test]
fn xep0047_open_with_message_stanza_transport_round_trips() {
    let iq = wire_round_trip(build_ibb_open(
        None,
        None,
        "id",
        "sid",
        4096,
        IbbStanzaType::Message,
    ));
    let open = parse_ibb_open(&iq).expect("valid open");
    assert_eq!(open.stanza, IbbStanzaType::Message);
    assert_eq!(open.stanza.as_str(), "message");
}

#[test]
fn xep0047_open_with_invalid_stanza_type_is_typed_error() {
    let xml = format!(
        "<iq xmlns='jabber:client' type='set' id='x'>\
         <open xmlns='{NS_IBB}' sid='s1' block-size='4096' stanza='presence'/></iq>"
    );
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert_eq!(
        parse_ibb_open(&iq),
        Err(IbbError::InvalidStanzaType("presence".to_owned()))
    );
}

#[test]
fn xep0047_open_requires_sid_and_positive_block_size() {
    let no_sid = format!(
        "<iq xmlns='jabber:client' type='set' id='x'>\
         <open xmlns='{NS_IBB}' block-size='4096'/></iq>"
    );
    let iq = Iq::try_from(no_sid.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert_eq!(parse_ibb_open(&iq), Err(IbbError::MissingSid));

    let zero_bs = format!(
        "<iq xmlns='jabber:client' type='set' id='x'>\
         <open xmlns='{NS_IBB}' sid='s1' block-size='0'/></iq>"
    );
    let iq = Iq::try_from(zero_bs.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert_eq!(parse_ibb_open(&iq), Err(IbbError::InvalidBlockSize));
}

// ── Data transfer violations ─────────────────────────────────────────────────

#[test]
fn xep0047_oversized_chunk_is_rejected_against_negotiated_block_size() {
    let iq = wire_round_trip(build_ibb_data_iq(
        Some(jid(INITIATOR)),
        Some(jid(RESPONDER)),
        "big-1",
        "sid",
        0,
        &[0u8; 17],
    ));
    let data = parse_ibb_data_from_iq(&iq).expect("valid data element");
    assert_eq!(
        validate_data_size(&data, 16),
        Err(IbbError::DataTooLarge {
            actual: 17,
            limit: 16
        })
    );
}

#[test]
fn xep0047_invalid_base64_payload_is_typed_error() {
    let xml = format!(
        "<iq xmlns='jabber:client' type='set' id='x'>\
         <data xmlns='{NS_IBB}' sid='s1' seq='0'>not!!valid@@base64</data></iq>"
    );
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert_eq!(parse_ibb_data_from_iq(&iq), Err(IbbError::InvalidBase64));
}

#[test]
fn xep0047_iq_get_is_not_an_ibb_stanza() {
    let xml = format!(
        "<iq xmlns='jabber:client' type='get' id='x'>\
         <open xmlns='{NS_IBB}' sid='s1' block-size='4096'/></iq>"
    );
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert!(!is_ibb_open(&iq));
    assert_eq!(parse_ibb_open(&iq), Err(IbbError::NotIbb));
}

// ── Message transport ────────────────────────────────────────────────────────

#[test]
fn xep0047_message_transport_data_round_trips() {
    let data_elem = build_ibb_data_element("msg-sid", 7, b"hello via message");
    let message = Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), RESPONDER)
        .append(data_elem)
        .build();
    let reparsed: Element = String::from(&message).parse().expect("well-formed message");

    assert!(message_has_ibb_data(&reparsed));
    let data = parse_ibb_data_from_message(&reparsed).expect("valid data");
    assert_eq!(data.sid, "msg-sid");
    assert_eq!(data.seq, 7);
    assert_eq!(data.data, b"hello via message");
}

#[test]
fn xep0047_message_without_data_is_typed_error() {
    let message: Element = "<message xmlns='jabber:client'><body>plain</body></message>"
        .parse()
        .expect("valid xml");
    assert!(!message_has_ibb_data(&message));
    assert_eq!(parse_ibb_data_from_message(&message), Err(IbbError::NotIbb));
}

// ── Error responses (§2.1, §2.2) ─────────────────────────────────────────────

#[test]
fn xep0047_error_responses_carry_spec_conditions_and_mirror_addressing() {
    let open_iq = build_ibb_open(
        Some(jid(INITIATOR)),
        Some(jid(RESPONDER)),
        "err-1",
        "sid",
        4096,
        IbbStanzaType::Iq,
    );

    let cases = [
        (
            build_ibb_not_acceptable(&open_iq),
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
        ),
        (
            build_ibb_resource_constraint(&open_iq),
            ErrorType::Modify,
            DefinedCondition::ResourceConstraint,
        ),
        (
            build_ibb_item_not_found(&open_iq),
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
        ),
        (
            build_ibb_unexpected_request(&open_iq),
            ErrorType::Cancel,
            DefinedCondition::UnexpectedRequest,
        ),
    ];

    for (error_iq, expected_type, expected_condition) in cases {
        let Iq::Error {
            from,
            to,
            id,
            error,
            ..
        } = error_iq
        else {
            panic!("error builder must produce Iq::Error");
        };
        assert_eq!(id, "err-1");
        assert_eq!(from, Some(jid(RESPONDER)), "error comes from the responder");
        assert_eq!(to, Some(jid(INITIATOR)), "error goes to the initiator");
        assert_eq!(error.type_, expected_type);
        assert_eq!(error.defined_condition, expected_condition);
    }
}

#[test]
fn xep0047_data_iq_wire_shape_is_base64_text_with_sid_and_seq() {
    let iq = build_ibb_data_iq(None, None, "d1", "i781hf64", 0, b"binary payload");
    let Iq::Set { payload, .. } = &iq else {
        panic!("data builder must produce Iq::Set");
    };
    assert_eq!(payload.name(), "data");
    assert_eq!(payload.ns(), NS_IBB);
    assert_eq!(payload.attr("sid"), Some("i781hf64"));
    assert_eq!(payload.attr("seq"), Some("0"));
    assert_eq!(payload.text(), "YmluYXJ5IHBheWxvYWQ=");
    assert_eq!(payload.children().count(), 0);
}
