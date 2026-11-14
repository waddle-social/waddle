//! Adversarial suite for the bounded `jabber:server` stream codec.
//!
//! Inputs are raw wire fixtures: the decoder is the boundary that turns bytes
//! into typed values, so malformed and hostile XML has to be expressed as
//! bytes.

use std::str::FromStr;

use jid::DomainPart;
use minidom::Element;
use waddle_xmpp::ns::{JABBER_SERVER, SASL, STREAM, TLS};
use waddle_xmpp::s2s::{
    CodecLimits, DecodeError, HeaderAddress, Inbound, LimitKind, StreamDecoder, StreamEncoder,
    StreamErrorCondition, StreamHeader, StreamId, StreamLanguage, StreamVersion,
};

const HEADER: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' xmlns:db='jabber:server:dialback' \
from='home-a.test' to='home-b.test' version='1.0' xml:lang='en'>";

const MESSAGE: &[u8] =
    b"<message from='alice@home-a.test/x' to='bob@home-b.test' type='chat'><body>hi</body></message>";

fn domain(value: &str) -> DomainPart {
    DomainPart::from_str(value).expect("valid domain")
}

fn decoder() -> StreamDecoder {
    StreamDecoder::new(CodecLimits::PILOT)
}

/// Push `bytes` and collect every item until more input is needed.
fn decode(decoder: &mut StreamDecoder, bytes: &[u8]) -> Result<Vec<Inbound>, DecodeError> {
    decoder.push(bytes)?;
    let mut items = Vec::new();
    while let Some(item) = decoder.next_item()? {
        items.push(item);
    }
    Ok(items)
}

/// Decode `chunks` pushed one after another, collecting items as they
/// become available.
fn decode_chunks(
    decoder: &mut StreamDecoder,
    chunks: &[&[u8]],
) -> Result<Vec<Inbound>, DecodeError> {
    let mut items = Vec::new();
    for chunk in chunks {
        items.extend(decode(decoder, chunk)?);
    }
    Ok(items)
}

fn bytewise(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.chunks(1).collect()
}

fn header_of(items: &[Inbound]) -> &StreamHeader {
    match items.first() {
        Some(Inbound::Header(header)) => header,
        other => panic!("expected a stream header, got {other:?}"),
    }
}

fn elements_of(items: Vec<Inbound>) -> Vec<Element> {
    items
        .into_iter()
        .filter_map(|item| match item {
            Inbound::Element(element) => Some(element),
            _ => None,
        })
        .collect()
}

fn decode_error(bytes: &[u8]) -> DecodeError {
    decode(&mut decoder(), bytes).expect_err("input must be rejected")
}

fn after_header(stanza: &[u8]) -> DecodeError {
    let mut decoder = decoder();
    decode(&mut decoder, HEADER).expect("header decodes");
    decode(&mut decoder, stanza).expect_err("stanza must be rejected")
}

fn small_limits() -> CodecLimits {
    CodecLimits {
        max_header_bytes: 512,
        max_stanza_bytes: 256,
        max_control_element_bytes: 128,
        max_depth: 4,
        max_attributes_per_element: 8,
        max_attribute_bytes_per_stanza: 128,
        max_elements_per_stanza: 8,
        max_buffered_bytes: 1024,
        max_token_bytes: 512,
        max_namespace_bytes: 64,
        max_tree_bytes_per_stanza: 512,
    }
}

// --- stream headers ---------------------------------------------------

#[test]
fn header_attributes_are_typed() {
    let items = decode(&mut decoder(), HEADER).expect("valid header");
    let header = header_of(&items);
    assert_eq!(header.from, HeaderAddress::Domain(domain("home-a.test")));
    assert_eq!(header.to, HeaderAddress::Domain(domain("home-b.test")));
    assert_eq!(header.version, Some(StreamVersion::V1_0));
    assert_eq!(header.lang, StreamLanguage::parse("en"));
    assert_eq!(header.id, None);
}

#[test]
fn header_accepts_prefix_free_canonicalization() {
    let items = decode(
        &mut decoder(),
        b"<stream xmlns='http://etherx.jabber.org/streams' from='home-a.test' \
to='home-b.test' version='1.0'><message xmlns='jabber:server' from='a@home-a.test' \
to='b@home-b.test'/>",
    )
    .expect("prefix-free stream");
    let elements = elements_of(items);
    assert_eq!(elements.len(), 1);
    assert!(elements[0].is("message", JABBER_SERVER));
}

#[test]
fn header_with_wrong_stream_namespace_is_invalid_namespace() {
    let error = decode_error(
        b"<stream:stream xmlns='jabber:server' xmlns:stream='http://wrong.example/' \
from='home-a.test' to='home-b.test' version='1.0'>",
    );
    assert_eq!(error, DecodeError::InvalidNamespace);
    assert_eq!(error.condition(), StreamErrorCondition::InvalidNamespace);
}

#[test]
fn header_with_client_content_namespace_is_invalid_namespace() {
    let error = decode_error(
        b"<stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test' version='1.0'>",
    );
    assert_eq!(error, DecodeError::InvalidNamespace);
}

#[test]
fn header_declaring_extension_namespace_is_invalid_namespace() {
    let error = decode_error(
        b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
xmlns:x='urn:example:payload' from='home-a.test' to='home-b.test' version='1.0'>",
    );
    assert_eq!(error, DecodeError::InvalidNamespace);
}

#[test]
fn header_with_non_stream_prefix_is_bad_namespace_prefix() {
    let error = decode_error(
        b"<s:stream xmlns='jabber:server' xmlns:s='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test' version='1.0'>",
    );
    assert_eq!(error, DecodeError::BadNamespacePrefix);
    assert_eq!(error.condition(), StreamErrorCondition::BadNamespacePrefix);
}

#[test]
fn header_root_with_wrong_name_is_bad_format() {
    let error = decode_error(
        b"<stream:open xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test' version='1.0'>",
    );
    assert_eq!(error, DecodeError::BadFormat);
}

#[test]
fn header_addresses_distinguish_absent_and_invalid() {
    let cases: [(&[u8], HeaderAddress, HeaderAddress); 4] = [
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' version='1.0'>",
            HeaderAddress::Absent,
            HeaderAddress::Absent,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='' to='home-b.test' version='1.0'>",
            HeaderAddress::Invalid,
            HeaderAddress::Domain(domain("home-b.test")),
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='alice@home-a.test' to='home-b.test/resource' version='1.0'>",
            HeaderAddress::Invalid,
            HeaderAddress::Invalid,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='bad domain' to='home-b.test' version='1.0'>",
            HeaderAddress::Invalid,
            HeaderAddress::Domain(domain("home-b.test")),
        ),
    ];
    for (bytes, from, to) in cases {
        let items = decode(&mut decoder(), bytes).expect("header decodes");
        let header = header_of(&items);
        assert_eq!(header.from, from);
        assert_eq!(header.to, to);
    }
}

#[test]
fn header_version_parsing() {
    let items = decode(
        &mut decoder(),
        b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test'>",
    )
    .expect("header without version");
    assert_eq!(header_of(&items).version, None);

    let error = decode_error(
        b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test' version='one'>",
    );
    assert_eq!(error, DecodeError::BadFormat);

    let version = StreamVersion::parse("1.010").expect("leading zeros are ignored");
    assert_eq!((version.major(), version.minor()), (1, 10));
    assert!(StreamVersion::parse("2.13") > StreamVersion::parse("2.4"));
    assert!(!StreamVersion::parse("0.9")
        .expect("valid version")
        .supports_xmpp_1_0());
}

#[test]
fn oversized_header_is_rejected_before_completion() {
    let mut decoder = decoder();
    decode(
        &mut decoder,
        b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' id='",
    )
    .expect("partial header");
    let error = decode(&mut decoder, &[b'a'; 9 * 1024]).expect_err("header too large");
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::HeaderBytes));
    assert_eq!(error.condition(), StreamErrorCondition::PolicyViolation);
}

// --- namespaces --------------------------------------------------------

#[test]
fn stanzas_inherit_content_namespace_from_stream_header() {
    let mut decoder = decoder();
    let items = decode_chunks(
        &mut decoder,
        &[
            HEADER,
            MESSAGE,
            b"<iq type='get' id='1' from='home-a.test' to='home-b.test'>\
<query xmlns='urn:example:q'><item/></query><extra/></iq>",
        ],
    )
    .expect("stream decodes");
    let elements = elements_of(items);
    assert_eq!(elements.len(), 2);

    let message = &elements[0];
    assert!(message.is("message", JABBER_SERVER));
    assert!(message.get_child("body", JABBER_SERVER).is_some());
    assert_eq!(message.attr("to"), Some("bob@home-b.test"));

    let iq = &elements[1];
    let query = iq.get_child("query", "urn:example:q").expect("query child");
    assert!(query.get_child("item", "urn:example:q").is_some());
    assert!(iq.get_child("extra", JABBER_SERVER).is_some());
}

#[test]
fn prefixed_content_namespace_is_bad_namespace_prefix() {
    let error = after_header(
        b"<foo:message xmlns:foo='jabber:server' from='a@home-a.test' to='b@home-b.test'/>",
    );
    assert_eq!(error, DecodeError::BadNamespacePrefix);
}

#[test]
fn undeclared_prefix_is_not_well_formed() {
    assert_eq!(after_header(b"<x:message/>"), DecodeError::NotWellFormed);
}

#[test]
fn duplicate_attributes_are_not_well_formed() {
    assert_eq!(
        after_header(b"<message to='b@home-b.test' to='c@home-b.test'/>"),
        DecodeError::NotWellFormed
    );
    assert_eq!(
        after_header(b"<message xmlns:a='urn:x' xmlns:b='urn:x' a:k='1' b:k='2'/>"),
        DecodeError::NotWellFormed
    );
}

#[test]
fn mismatched_end_tag_is_not_well_formed() {
    let error = after_header(b"<message><body>No closing tag!</message>");
    assert_eq!(error, DecodeError::NotWellFormed);
    assert_eq!(error.condition(), StreamErrorCondition::NotWellFormed);
}

#[test]
fn stream_level_text_is_bad_format_but_whitespace_is_counted() {
    let mut decoder = decoder();
    decode_chunks(
        &mut decoder,
        &[HEADER, b" \n\t ", MESSAGE, b"\r\n", MESSAGE],
    )
    .expect("whitespace between stanzas");
    assert_eq!(decoder.whitespace_bytes(), 6);
    let error = decode(&mut decoder, b"keepalive<message/>").expect_err("text at stream level");
    assert_eq!(error, DecodeError::BadFormat);
}

#[test]
fn closing_stream_tag_ends_the_stream() {
    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &[HEADER, MESSAGE, b"</stream:stream>"])
        .expect("stream decodes");
    assert!(matches!(items.last(), Some(Inbound::End)));
    assert!(decoder.is_ended());
    assert_eq!(decoder.next_item(), Ok(None));
}

// --- UTF-8 -------------------------------------------------------------

#[test]
fn utf8_split_across_reads_is_reassembled() {
    let stanza =
        "<message from='a@home-a.test' to='b@home-b.test'><body>héllo \u{1F427} ✓</body></message>";
    let mut whole = HEADER.to_vec();
    whole.extend_from_slice(stanza.as_bytes());

    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &bytewise(&whole)).expect("bytewise decode");
    let elements = elements_of(items);
    let body = elements[0]
        .get_child("body", JABBER_SERVER)
        .expect("body")
        .text();
    assert_eq!(body, "héllo \u{1F427} ✓");

    // Split inside the four-byte penguin.
    let split = whole
        .windows(4)
        .position(|window| window == "\u{1F427}".as_bytes())
        .expect("emoji present")
        + 2;
    let mut decoder = self::decoder();
    let items =
        decode_chunks(&mut decoder, &[&whole[..split], &whole[split..]]).expect("split decode");
    assert_eq!(elements_of(items).len(), 1);
}

#[test]
fn invalid_utf8_is_unsupported_encoding() {
    let error = after_header(b"<message><body>\xff\xfe</body></message>");
    assert_eq!(error, DecodeError::UnsupportedEncoding);
    assert_eq!(error.condition(), StreamErrorCondition::UnsupportedEncoding);
}

#[test]
fn truncated_utf8_sequence_is_unsupported_encoding() {
    let error = after_header(b"<message><body>\xe2\x9c</body></message>");
    assert_eq!(error, DecodeError::UnsupportedEncoding);
}

// --- restricted XML (RFC 6120 §11.1) -------------------------------------

#[test]
fn comments_are_restricted_xml() {
    assert_eq!(after_header(b"<!-- hello -->"), DecodeError::RestrictedXml);
    assert_eq!(
        after_header(b"<message><!-- inside --></message>"),
        DecodeError::RestrictedXml
    );
}

#[test]
fn processing_instructions_are_restricted_xml() {
    assert_eq!(
        after_header(b"<?php echo 1; ?>"),
        DecodeError::RestrictedXml
    );
    assert_eq!(
        after_header(b"<message><?target data?></message>"),
        DecodeError::RestrictedXml
    );
}

#[test]
fn document_type_declaration_is_restricted_xml() {
    let error = decode_error(
        b"<?xml version='1.0'?><!DOCTYPE stream [<!ENTITY x 'y'>]><stream:stream \
xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams'>",
    );
    assert_eq!(error, DecodeError::RestrictedXml);
    assert_eq!(error.condition(), StreamErrorCondition::RestrictedXml);
}

#[test]
fn markup_declarations_inside_the_stream_are_restricted_xml() {
    assert_eq!(
        after_header(b"<!ENTITY lol 'lol'>"),
        DecodeError::RestrictedXml
    );
    assert_eq!(
        after_header(b"<message><!ELEMENT x ANY></message>"),
        DecodeError::RestrictedXml
    );
}

#[test]
fn undeclared_entity_references_are_restricted_xml() {
    assert_eq!(
        after_header(b"<message><body>&lol;</body></message>"),
        DecodeError::RestrictedXml
    );
}

#[test]
fn predefined_entities_and_cdata_are_accepted() {
    let mut decoder = decoder();
    let items = decode_chunks(
        &mut decoder,
        &[
            HEADER,
            b"<message from='a@home-a.test' to='b@home-b.test'><body>&lt;&amp;&#x41;<![CDATA[<!x>]]></body></message>",
        ],
    )
    .expect("predefined entities");
    let body = elements_of(items)[0]
        .get_child("body", JABBER_SERVER)
        .expect("body")
        .text();
    assert_eq!(body, "<&A<!x>");
}

#[test]
fn non_utf8_encoding_declaration_is_rejected() {
    // RFC 6120 §4.9.3.22 suggests `unsupported-encoding`, but the parser
    // reports this with the same error as other forbidden XML declarations,
    // so the decoder answers `restricted-xml`.
    let error = decode_error(b"<?xml version='1.0' encoding='ISO-8859-1'?>");
    assert_eq!(error, DecodeError::RestrictedXml);
}

// --- size and shape limits -----------------------------------------------

#[test]
fn stanza_at_the_byte_limit_is_accepted_and_one_more_byte_is_rejected() {
    let limits = small_limits();
    let open: &[u8] = b"<message><body>";
    let close: &[u8] = b"</body></message>";
    let fill = limits.max_stanza_bytes - open.len() - close.len();

    let mut exact = open.to_vec();
    exact.extend(std::iter::repeat_n(b'x', fill));
    exact.extend_from_slice(close);
    let mut decoder = StreamDecoder::new(limits);
    let items = decode_chunks(&mut decoder, &[HEADER, &exact]).expect("stanza at limit");
    assert_eq!(elements_of(items).len(), 1);

    let mut over = open.to_vec();
    over.extend(std::iter::repeat_n(b'x', fill + 1));
    over.extend_from_slice(close);
    let mut decoder = StreamDecoder::new(limits);
    let error = decode_chunks(&mut decoder, &[HEADER, &over]).expect_err("stanza over limit");
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::StanzaBytes));
}

#[test]
fn oversized_stanza_is_rejected_while_streaming_with_bounded_retention() {
    let mut decoder = decoder();
    decode(&mut decoder, HEADER).expect("header");
    decode(&mut decoder, b"<message><body>").expect("open stanza");
    let chunk = [b'x'; 4096];
    let mut pushed = 0;
    let error = loop {
        match decode(&mut decoder, &chunk) {
            Ok(_) => {
                pushed += chunk.len();
                assert!(decoder.retained_bytes() <= CodecLimits::PILOT.max_stanza_bytes);
                assert!(pushed < 128 * 1024, "limit never enforced");
            }
            Err(error) => break error,
        }
    };
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::StanzaBytes));
    assert!(pushed <= CodecLimits::PILOT.max_stanza_bytes);
    // Errors are terminal.
    assert_eq!(decoder.next_item(), Err(error));
    assert_eq!(decoder.push(b"<message/>"), Err(error));
}

#[test]
fn whitespace_keepalives_are_counted_and_not_retained() {
    let limits = small_limits();
    let mut decoder = StreamDecoder::new(limits);
    decode(&mut decoder, HEADER).expect("header");
    let chunk = [b' '; 64];
    for _ in 0..64 {
        decode(&mut decoder, &chunk).expect("whitespace is allowed between stanzas");
        assert!(decoder.retained_bytes() <= chunk.len());
    }
    // 4 KiB of whitespace exceeds both the stanza and buffered-input
    // ceilings of these limits, yet nothing is retained: whitespace volume
    // is left to the connection's rate limits.
    assert_eq!(decoder.whitespace_bytes(), 64 * 64);
}

#[test]
fn deep_nesting_is_rejected_at_the_depth_limit() {
    let open = b"<a>".repeat(CodecLimits::PILOT.max_depth - 1);
    let close = b"</a>".repeat(CodecLimits::PILOT.max_depth - 1);
    let mut within = b"<message>".to_vec();
    within.extend_from_slice(&open);
    within.extend_from_slice(&close);
    within.extend_from_slice(b"</message>");
    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &[HEADER, &within]).expect("depth 32");
    assert_eq!(elements_of(items).len(), 1);

    let mut beyond = b"<message>".to_vec();
    beyond.extend_from_slice(&b"<a>".repeat(CodecLimits::PILOT.max_depth));
    assert_eq!(
        after_header(&beyond),
        DecodeError::LimitExceeded(LimitKind::Depth)
    );

    // A nesting bomb is rejected as soon as the depth is exceeded rather than
    // after buffering the whole input.
    let mut decoder = self::decoder();
    decode(&mut decoder, HEADER).expect("header");
    let bomb = b"<a>".repeat(10_000);
    let error = decode(&mut decoder, &bomb).expect_err("nesting bomb");
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::Depth));
}

#[test]
fn attribute_flood_on_one_element_is_rejected() {
    let mut stanza = b"<message".to_vec();
    for name in 0..=CodecLimits::PILOT.max_attributes_per_element {
        stanza.extend_from_slice(b" a");
        stanza.extend_from_slice(name.to_string().as_bytes());
        stanza.extend_from_slice(b"='v'");
    }
    stanza.extend_from_slice(b"/>");
    assert_eq!(
        after_header(&stanza),
        DecodeError::LimitExceeded(LimitKind::AttributesPerElement)
    );
}

#[test]
fn namespace_declarations_count_as_attributes() {
    let limits = small_limits();
    let mut decoder = StreamDecoder::new(limits);
    decode(&mut decoder, HEADER).expect("header");
    let error = decode(
        &mut decoder,
        b"<message xmlns:a='urn:a' xmlns:b='urn:b' xmlns:c='urn:c' xmlns:d='urn:d' \
xmlns:e='urn:e' xmlns:f='urn:f' xmlns:g='urn:g' xmlns:h='urn:h' xmlns:i='urn:i'/>",
    )
    .expect_err("declaration flood");
    assert_eq!(
        error,
        DecodeError::LimitExceeded(LimitKind::AttributesPerElement)
    );
}

#[test]
fn attribute_bytes_are_bounded_across_a_stanza() {
    let value = vec![b'v'; 3000];
    let mut stanza = b"<message".to_vec();
    for name in [b"a", b"b", b"c"] {
        stanza.extend_from_slice(b" ");
        stanza.extend_from_slice(name);
        stanza.extend_from_slice(b"='");
        stanza.extend_from_slice(&value);
        stanza.extend_from_slice(b"'");
    }
    stanza.extend_from_slice(b"/>");
    assert_eq!(
        after_header(&stanza),
        DecodeError::LimitExceeded(LimitKind::AttributeBytes)
    );
}

#[test]
fn element_count_is_bounded_per_stanza() {
    let limits = small_limits();
    let mut stanza = b"<message>".to_vec();
    stanza.extend_from_slice(&b"<a/>".repeat(limits.max_elements_per_stanza));
    stanza.extend_from_slice(b"</message>");
    let mut decoder = StreamDecoder::new(limits);
    let error = decode_chunks(&mut decoder, &[HEADER, &stanza]).expect_err("too many elements");
    assert_eq!(
        error,
        DecodeError::LimitExceeded(LimitKind::ElementsPerStanza)
    );
}

#[test]
fn oversized_names_and_values_hit_the_token_limit() {
    let long = vec![b'a'; CodecLimits::PILOT.max_token_bytes + 16];

    let mut name = b"<message><".to_vec();
    name.extend_from_slice(&long);
    name.extend_from_slice(b"/></message>");
    let error = after_header(&name);
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::TokenBytes));
    assert_eq!(error.condition(), StreamErrorCondition::PolicyViolation);

    let mut value = b"<message to='".to_vec();
    value.extend_from_slice(&long);
    value.extend_from_slice(b"'/>");
    assert_eq!(
        after_header(&value),
        DecodeError::LimitExceeded(LimitKind::TokenBytes)
    );

    // Character data is not a token: it is emitted in pieces.
    let mut text = b"<message><body>".to_vec();
    text.extend_from_slice(&long);
    text.extend_from_slice(b"</body></message>");
    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &[HEADER, &text]).expect("long text");
    assert_eq!(elements_of(items).len(), 1);
}

#[test]
fn namespace_names_are_capped() {
    let limit = CodecLimits::PILOT.max_namespace_bytes;
    let namespace = |len: usize| {
        let mut uri = b"urn:".to_vec();
        uri.resize(len, b'x');
        uri
    };

    let mut at_limit = b"<message><x xmlns='".to_vec();
    at_limit.extend_from_slice(&namespace(limit));
    at_limit.extend_from_slice(b"'/></message>");
    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &[HEADER, &at_limit]).expect("namespace at limit");
    assert_eq!(elements_of(items).len(), 1);

    for declaration in [&b" xmlns='"[..], &b" xmlns:p='"[..]] {
        let mut over = b"<message".to_vec();
        over.extend_from_slice(declaration);
        over.extend_from_slice(&namespace(limit + 1));
        over.extend_from_slice(b"'/>");
        let error = after_header(&over);
        assert_eq!(error, DecodeError::LimitExceeded(LimitKind::NamespaceBytes));
        assert_eq!(error.condition(), StreamErrorCondition::PolicyViolation);
    }
}

#[test]
fn inherited_namespace_copies_are_charged_to_the_tree_budget() {
    // A few kilobytes of wire, within every raw limit: one long default
    // namespace inherited by a thousand empty elements would otherwise be
    // copied into each of them.
    let limits = CodecLimits::PILOT;
    let mut uri = b"urn:".to_vec();
    uri.resize(limits.max_namespace_bytes, b'x');
    let mut stanza = b"<x xmlns='".to_vec();
    stanza.extend_from_slice(&uri);
    stanza.extend_from_slice(b"'>");
    stanza.extend_from_slice(&b"<a/>".repeat(limits.max_elements_per_stanza - 1));
    stanza.extend_from_slice(b"</x>");
    assert!(stanza.len() < limits.max_stanza_bytes);

    let error = after_header(&stanza);
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::TreeBytes));
    assert_eq!(error.condition(), StreamErrorCondition::PolicyViolation);

    // The same shape fails while streaming, long before the whole element
    // has arrived.
    let mut decoder = decoder();
    decode(&mut decoder, HEADER).expect("header");
    let chunk_error = stanza.chunks(64).enumerate().find_map(|(index, chunk)| {
        decode(&mut decoder, chunk)
            .err()
            .map(|error| (index, error))
    });
    let (index, error) = chunk_error.expect("tree budget enforced");
    assert_eq!(error, DecodeError::LimitExceeded(LimitKind::TreeBytes));
    assert!((index + 1) * 64 < stanza.len() / 2);
}

#[test]
fn prefixed_namespace_copies_are_charged_to_the_tree_budget() {
    let limits = CodecLimits::PILOT;
    let mut uri = b"urn:".to_vec();
    uri.resize(limits.max_namespace_bytes, b'x');
    let declaration = |stanza: &mut Vec<u8>| {
        stanza.extend_from_slice(b"<message xmlns:p='");
        stanza.extend_from_slice(&uri);
        stanza.extend_from_slice(b"'>");
    };

    // Prefixed attribute names: each attribute key names the namespace.
    let mut attributes = Vec::new();
    declaration(&mut attributes);
    attributes.extend_from_slice(&b"<a p:k=''/>".repeat(limits.max_elements_per_stanza - 1));
    attributes.extend_from_slice(b"</message>");
    assert_eq!(
        after_header(&attributes),
        DecodeError::LimitExceeded(LimitKind::TreeBytes)
    );

    // Prefixed element names: each element copies the namespace.
    let mut elements = Vec::new();
    declaration(&mut elements);
    elements.extend_from_slice(&b"<p:a/>".repeat(limits.max_elements_per_stanza - 1));
    elements.extend_from_slice(b"</message>");
    assert_eq!(
        after_header(&elements),
        DecodeError::LimitExceeded(LimitKind::TreeBytes)
    );
}

#[test]
fn honest_stanza_at_the_element_limit_fits_the_tree_budget() {
    let limits = CodecLimits::PILOT;
    let mut stanza = b"<iq type='result' id='1' from='home-a.test' to='home-b.test'>\
<query xmlns='http://jabber.org/protocol/disco#items'>"
        .to_vec();
    stanza.extend_from_slice(&b"<item/>".repeat(limits.max_elements_per_stanza - 2));
    stanza.extend_from_slice(b"</query></iq>");
    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &[HEADER, &stanza]).expect("element limit");
    let elements = elements_of(items);
    let query = elements[0]
        .get_child("query", "http://jabber.org/protocol/disco#items")
        .expect("query");
    assert_eq!(query.children().count(), limits.max_elements_per_stanza - 2);
}

#[test]
fn control_elements_have_a_smaller_byte_limit() {
    let mut auth = b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>".to_vec();
    auth.extend(std::iter::repeat_n(b'A', 9 * 1024));
    auth.extend_from_slice(b"</auth>");
    assert_eq!(
        after_header(&auth),
        DecodeError::LimitExceeded(LimitKind::ControlElementBytes)
    );

    // The same payload size is acceptable in an ordinary stanza.
    let mut message = b"<message><body>".to_vec();
    message.extend(std::iter::repeat_n(b'A', 9 * 1024));
    message.extend_from_slice(b"</body></message>");
    let mut decoder = decoder();
    let items = decode_chunks(&mut decoder, &[HEADER, &message]).expect("large stanza");
    assert_eq!(elements_of(items).len(), 1);
}

#[test]
fn unparsed_input_is_bounded() {
    let mut decoder = decoder();
    decode(&mut decoder, HEADER).expect("header");
    let flood = vec![b' '; CodecLimits::PILOT.max_buffered_bytes + 1];
    assert_eq!(
        decoder.push(&flood),
        Err(DecodeError::LimitExceeded(LimitKind::BufferedBytes))
    );
}

// --- segmentation and restart boundaries --------------------------------

#[test]
fn bytewise_and_coalesced_input_decode_identically() {
    let mut whole = HEADER.to_vec();
    whole.extend_from_slice(MESSAGE);
    whole.extend_from_slice(b" <presence from='a@home-a.test' to='b@home-b.test'/>");
    whole.extend_from_slice(b"</stream:stream>");

    let coalesced = decode(&mut decoder(), &whole).expect("coalesced");
    let segmented = decode_chunks(&mut decoder(), &bytewise(&whole)).expect("bytewise");
    assert_eq!(coalesced, segmented);
    assert_eq!(coalesced.len(), 4);
}

#[test]
fn bytes_after_proceed_are_handed_to_tls_and_never_parsed() {
    let tls_records: &[u8] =
        b"\x16\x03\x01\x00\x05<message from='evil@home-a.test' to='b@home-b.test'/>";
    let mut input = HEADER.to_vec();
    input.extend_from_slice(b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>");
    input.extend_from_slice(tls_records);

    let mut decoder = decoder();
    decoder.push(&input).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    match decoder.next_item() {
        Ok(Some(Inbound::Element(element))) => assert!(element.is("proceed", TLS)),
        other => panic!("expected proceed, got {other:?}"),
    }
    let handoff = decoder.restart_after_tls();
    assert_eq!(handoff.into_bytes(), tls_records);
    // The restarted decoder holds nothing and requires a new header.
    assert_eq!(decoder.retained_bytes(), 0);
    assert_eq!(decoder.next_item(), Ok(None));
    let error = decode(&mut decoder, MESSAGE).expect_err("stanza without a header");
    assert_eq!(error, DecodeError::InvalidNamespace);
}

#[test]
fn proceed_boundary_is_exact_under_every_split() {
    let tail: &[u8] = b" \x16\x03";
    let mut input = HEADER.to_vec();
    input.extend_from_slice(b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'></proceed>");
    input.extend_from_slice(tail);
    for split in 1..input.len() {
        let mut decoder = decoder();
        let mut pending = &input[split..];
        decoder.push(&input[..split]).expect("first segment");
        let mut proceed = false;
        while !proceed {
            match decoder.next_item().expect("segments decode") {
                Some(Inbound::Header(_)) => {}
                Some(Inbound::Element(element)) => {
                    assert!(element.is("proceed", TLS), "split at {split}");
                    proceed = true;
                }
                Some(Inbound::End) => panic!("unexpected end at split {split}"),
                None => {
                    assert!(
                        !pending.is_empty(),
                        "proceed never decoded at split {split}"
                    );
                    decoder.push(pending).expect("second segment");
                    pending = &[];
                }
            }
        }
        // Whatever was not yet pushed would be read from the socket by the
        // TLS engine directly.
        let mut handoff = decoder.restart_after_tls().into_bytes();
        handoff.extend_from_slice(pending);
        assert_eq!(handoff, tail, "split at {split}");
    }
}

#[test]
fn bytes_after_sasl_success_are_discarded() {
    let mut input = HEADER.to_vec();
    input.extend_from_slice(b"<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>");
    input.extend_from_slice(MESSAGE);

    let mut decoder = decoder();
    decoder.push(&input).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    match decoder.next_item() {
        Ok(Some(Inbound::Element(element))) => assert!(element.is("success", SASL)),
        other => panic!("expected success, got {other:?}"),
    }
    let discarded = decoder.restart_after_sasl();
    assert_eq!(discarded.bytes(), MESSAGE.len());
    assert_eq!(decoder.next_item(), Ok(None));
}

#[test]
fn restart_discards_previous_namespace_context() {
    let mut decoder = decoder();
    decode(&mut decoder, HEADER).expect("first stream");
    decode(
        &mut decoder,
        b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>",
    )
    .expect("proceed");
    let handoff = decoder.restart_after_tls();
    assert!(handoff.is_empty());

    // The new stream uses prefix-free canonicalization and declares no
    // content namespace, so an unqualified first-level element is in the
    // stream namespace rather than inheriting `jabber:server`.
    let items = decode(
        &mut decoder,
        b"<stream xmlns='http://etherx.jabber.org/streams' from='home-a.test' to='home-b.test' \
version='1.0'><message/>",
    )
    .expect("second stream");
    let elements = elements_of(items);
    assert!(elements[0].is("message", STREAM));
}

#[test]
fn restart_mid_stanza_hands_over_the_partial_element() {
    let mut decoder = decoder();
    decode(&mut decoder, HEADER).expect("header");
    decode(
        &mut decoder,
        b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/><mess",
    )
    .expect("proceed");
    // `decode` parsed past the boundary into a partial element; the handoff
    // still starts right after the boundary element.
    assert_eq!(decoder.restart_after_tls().into_bytes(), b"<mess");
}

// --- encoder -------------------------------------------------------------

#[test]
fn encoder_output_round_trips_through_the_decoder() {
    let header = StreamHeader::response(
        domain("home-b.test"),
        Some(domain("home-a.test")),
        StreamLanguage::english(),
    );
    let (mut encoder, mut bytes) = StreamEncoder::open(&header).expect("open");
    let message = Element::builder("message", JABBER_SERVER)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), "b@home-b.test")
        .append(
            Element::builder("body", JABBER_SERVER)
                .append("a < b & c")
                .build(),
        )
        .build();
    bytes.extend(encoder.element(&message).expect("element"));
    bytes.extend(
        encoder
            .stream_error(StreamErrorCondition::HostUnknown)
            .expect("stream error"),
    );
    assert!(encoder.is_closed());

    let items = decode(&mut decoder(), &bytes).expect("round trip");
    let decoded = header_of(&items);
    assert_eq!(decoded.from, header.from);
    assert_eq!(decoded.to, header.to);
    assert_eq!(decoded.id, header.id);
    assert_eq!(decoded.version, Some(StreamVersion::V1_0));
    assert_eq!(decoded.lang, Some(StreamLanguage::english()));
    assert!(matches!(items.last(), Some(Inbound::End)));

    let elements = elements_of(items);
    assert_eq!(elements[0], message);
    assert_eq!(
        StreamErrorCondition::from_peer_element(elements[1].clone()),
        StreamErrorCondition::HostUnknown
    );
}

#[test]
fn first_level_elements_are_written_in_the_header_scope() {
    let header = StreamHeader::response(
        domain("home-b.test"),
        Some(domain("home-a.test")),
        StreamLanguage::english(),
    );
    let (mut encoder, _) = StreamEncoder::open(&header).expect("open");

    // Stream-namespace elements use the `stream` prefix bound on the
    // header; other namespaces are declared as the default namespace.
    let features = Element::builder("features", STREAM)
        .append(
            Element::builder("starttls", TLS)
                .append(Element::builder("required", TLS).build())
                .build(),
        )
        .build();
    assert_eq!(
        encoder.element(&features).expect("features"),
        b"<stream:features><starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'>\
<required/></starttls></stream:features>"
    );

    // Stanzas inherit `jabber:server` from the header.
    let message = Element::builder("message", JABBER_SERVER)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), "b@home-b.test")
        .append(Element::builder("body", JABBER_SERVER).append("hi").build())
        .build();
    assert_eq!(
        encoder.element(&message).expect("message"),
        b"<message to='b@home-b.test'><body>hi</body></message>"
    );

    assert_eq!(
        encoder
            .stream_error(StreamErrorCondition::PolicyViolation)
            .expect("stream error"),
        b"<stream:error><policy-violation xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>\
</stream:error></stream:stream>"
    );
}

#[test]
fn stream_ids_are_fresh() {
    let first = StreamId::random();
    let second = StreamId::random();
    assert_ne!(first, second);
    assert!(first.as_str().len() >= 16);
}

#[test]
fn stream_error_elements_carry_only_the_condition() {
    let element = StreamErrorCondition::ImproperAddressing.to_element();
    assert!(element.is("error", STREAM));
    assert_eq!(element.children().count(), 1);
    assert_eq!(
        StreamErrorCondition::from_peer_element(element),
        StreamErrorCondition::ImproperAddressing
    );
}
