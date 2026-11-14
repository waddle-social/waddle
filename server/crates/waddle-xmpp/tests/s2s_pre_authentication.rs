//! The S2S decoder's pre-authentication mode and the connection-owner hooks
//! of [`S2sStream`]: until SASL succeeds, a first-level element outside the
//! stream, TLS, SASL and dialback namespaces is refused as soon as its head
//! closes, before its tree is built, and every negotiation element is held
//! to the control-element ceiling. After SASL success the mode ends, and an
//! established receiving stream can hand address-checked stanzas to its
//! owner.

use std::sync::Arc;

use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{
    AuthorizationDecision, CloseReason, CodecLimits, DecodeError, DestinationStatus, DialbackMode,
    Inbound, InboundAdmission, LimitKind, NegotiationPhase, Output, ReceivingConfig, S2sStream,
    SaslLimits, Signal, StreamDecoder, StreamErrorCondition, StreamLanguage, TlsPeer,
};

const HEADER: &[u8] = b"<stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' from='home-a.test' to='home-b.test' \
version='1.0'>";
const STARTTLS: &[u8] = b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>";
const AUTH_EMPTY: &[u8] =
    b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>=</auth>";
const MESSAGE: &[u8] =
    b"<message from='alice@home-a.test/phone' to='bob@home-b.test'><body>hi</body></message>";

fn domain(value: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(value).expect("valid domain")
}

struct Admission;

impl InboundAdmission for Admission {
    fn destination(&self, destination: &CanonicalDomain) -> DestinationStatus {
        if destination.as_str() == "home-b.test" {
            DestinationStatus::Served
        } else {
            DestinationStatus::Unknown
        }
    }

    fn admits(&self, source: &CanonicalDomain, _destination: &CanonicalDomain) -> bool {
        source.as_str() == "home-a.test"
    }
}

fn receiver() -> S2sStream {
    S2sStream::accept(
        ReceivingConfig {
            admission: Arc::new(Admission),
            default_domain: domain("home-b.test"),
            language: StreamLanguage::english(),
            sasl: SaslLimits::PILOT,
            dialback: DialbackMode::Disabled,
        },
        CodecLimits::PILOT,
    )
}

/// A pre-authentication decoder that has read the stream header.
fn decoder_after_header() -> StreamDecoder {
    let mut decoder = StreamDecoder::pre_authentication(CodecLimits::PILOT);
    decoder.push(HEADER).expect("header fits");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    decoder
}

fn close_reason(outputs: &[Output]) -> Option<CloseReason> {
    outputs.iter().find_map(|output| match output {
        Output::Close(reason) => Some(*reason),
        _ => None,
    })
}

#[test]
fn a_jabber_server_stanza_is_refused_when_its_head_closes() {
    let mut decoder = decoder_after_header();
    // Only the head has arrived: the refusal does not wait for the body.
    decoder
        .push(b"<message from='a@home-a.test' to='b@home-b.test'>")
        .expect("push");
    assert_eq!(decoder.next_item(), Err(DecodeError::Unauthenticated));
    assert_eq!(
        DecodeError::Unauthenticated.condition(),
        StreamErrorCondition::NotAuthorized
    );
}

#[test]
fn a_jabber_client_stanza_is_an_invalid_namespace() {
    let mut decoder = decoder_after_header();
    decoder
        .push(b"<message xmlns='jabber:client' to='b@home-b.test'>")
        .expect("push");
    assert_eq!(decoder.next_item(), Err(DecodeError::InvalidNamespace));
}

#[test]
fn an_element_in_another_namespace_is_a_policy_violation() {
    for head in [
        b"<ping xmlns='urn:xmpp:ping'>".as_slice(),
        b"<unknown>".as_slice(),
        b"<x:bind xmlns:x='urn:ietf:params:xml:ns:xmpp-bind'>".as_slice(),
    ] {
        let mut decoder = decoder_after_header();
        decoder.push(head).expect("push");
        assert_eq!(
            decoder.next_item(),
            Err(DecodeError::UnexpectedElement),
            "{}",
            String::from_utf8_lossy(head)
        );
        assert_eq!(
            DecodeError::UnexpectedElement.condition(),
            StreamErrorCondition::PolicyViolation
        );
    }
}

#[test]
fn negotiation_elements_are_decoded() {
    let mut decoder = decoder_after_header();
    decoder
        .push(b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Element(_)))));
    decoder
        .push(b"<stream:features><starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/></stream:features>")
        .expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Element(_)))));
    decoder
        .push(b"<db:result xmlns:db='jabber:server:dialback' from='a.test' to='b.test'/>")
        .expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Element(_)))));
}

#[test]
fn every_negotiation_element_keeps_the_control_ceiling_from_its_first_byte() {
    let limit = CodecLimits::PILOT.max_control_element_bytes;
    // A stream-namespace element is not a control element outside the
    // mode, but before authentication it is held to the control ceiling.
    let mut decoder = decoder_after_header();
    let mut features = b"<stream:features>".to_vec();
    features.extend(std::iter::repeat_n(b' ', limit));
    decoder.push(&features).expect("push");
    assert_eq!(
        decoder.next_item(),
        Err(DecodeError::LimitExceeded(LimitKind::ControlElementBytes))
    );

    // An element head larger than the ceiling fails before it closes.
    let mut decoder = decoder_after_header();
    let mut head = b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' pad='".to_vec();
    head.extend(std::iter::repeat_n(b'a', limit));
    decoder.push(&head).expect("push");
    assert_eq!(
        decoder.next_item(),
        Err(DecodeError::LimitExceeded(LimitKind::ControlElementBytes))
    );

    // The same features element is accepted by a decoder outside the mode.
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    decoder.push(HEADER).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    features.extend_from_slice(b"</stream:features>");
    decoder.push(&features).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Element(_)))));
}

#[test]
fn the_mode_survives_the_tls_restart_and_ends_after_sasl() {
    let mut decoder = decoder_after_header();
    assert!(decoder.is_pre_authentication());
    assert!(decoder.restart_after_tls().is_empty());
    assert!(decoder.is_pre_authentication());
    decoder.push(HEADER).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    decoder.push(MESSAGE).expect("push");
    assert_eq!(decoder.next_item(), Err(DecodeError::Unauthenticated));

    let mut decoder = decoder_after_header();
    assert!(decoder.restart_after_sasl().is_empty());
    assert!(!decoder.is_pre_authentication());
    decoder.push(HEADER).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    decoder.push(MESSAGE).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Element(_)))));
}

#[test]
fn bytes_that_are_not_xml_close_without_any_xml() {
    // A TLS ClientHello record sent to the STARTTLS port.
    let mut decoder = StreamDecoder::pre_authentication(CodecLimits::PILOT);
    decoder.push(b"\x16\x03\x01\x02\x00\x01").expect("push");
    assert_eq!(decoder.next_item(), Err(DecodeError::NotXml));

    let mut stream = receiver();
    let out = stream.receive(b"\x16\x03\x01\x02\x00\x01");
    assert_eq!(close_reason(&out), Some(CloseReason::NotXml));
    assert!(!out
        .iter()
        .any(|output| matches!(output, Output::Transmit(_))));

    // Leading whitespace and a byte order mark are still XML.
    let mut decoder = StreamDecoder::pre_authentication(CodecLimits::PILOT);
    decoder.push(b"\n ").expect("push");
    decoder.push(HEADER).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
}

#[test]
fn a_plain_decoder_builds_stanzas_without_the_mode() {
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    assert!(!decoder.is_pre_authentication());
    decoder.push(HEADER).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Header(_)))));
    decoder.push(MESSAGE).expect("push");
    assert!(matches!(decoder.next_item(), Ok(Some(Inbound::Element(_)))));
}

#[test]
fn a_receiving_stream_refuses_a_stanza_head_before_its_body_arrives() {
    let mut stream = receiver();
    stream.receive(HEADER);
    let out = stream.receive(b"<message from='a@home-a.test' to='b@home-b.test'><body>");
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(
            StreamErrorCondition::NotAuthorized
        ))
    );
}

/// A receiving stream past SASL success and the authenticated restart.
fn established(deliver: bool) -> S2sStream {
    let mut stream = receiver();
    if deliver {
        stream.deliver_stanzas();
    }
    stream.receive(HEADER);
    stream.receive(STARTTLS);
    stream.tls_established(TlsPeer::Verified(domain("home-a.test")));
    stream.receive(HEADER);
    let request = stream
        .receive(AUTH_EMPTY)
        .into_iter()
        .find_map(|output| match output {
            Output::Authorize(request) => Some(request),
            _ => None,
        })
        .expect("authorization requested");
    stream.authorization(request.exchange, AuthorizationDecision::Granted);
    stream.receive(HEADER);
    assert_eq!(stream.phase(), NegotiationPhase::Established);
    stream
}

#[test]
fn an_established_stream_hands_address_checked_stanzas_to_its_owner() {
    let mut stream = established(true);
    let out = stream.receive(MESSAGE);
    let stanza = out
        .iter()
        .find_map(|output| match output {
            Output::Stanza(stanza) => Some(stanza.clone()),
            _ => None,
        })
        .expect("the stanza is handed over");
    assert_eq!(stanza.name(), "message");
    assert_eq!(close_reason(&out), None);

    // The owner refuses it.
    let out = stream.signal(Signal::PolicyViolation);
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(
            StreamErrorCondition::PolicyViolation
        ))
    );
}

#[test]
fn delivery_never_bypasses_the_address_checks() {
    let mut stream = established(true);
    let out = stream.receive(b"<message from='a@other.test' to='b@home-b.test'/>");
    assert!(!out.iter().any(|output| matches!(output, Output::Stanza(_))));
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(StreamErrorCondition::InvalidFrom))
    );
}

#[test]
fn without_delivery_an_established_stream_refuses_stanzas() {
    let mut stream = established(false);
    let out = stream.receive(MESSAGE);
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(
            StreamErrorCondition::PolicyViolation
        ))
    );
}

#[test]
fn owner_signals_close_with_their_conditions() {
    for (signal, condition) in [
        (Signal::Conflict, StreamErrorCondition::Conflict),
        (
            Signal::PolicyViolation,
            StreamErrorCondition::PolicyViolation,
        ),
    ] {
        assert_eq!(signal.condition(), condition);
        let mut stream = established(false);
        assert_eq!(
            close_reason(&stream.signal(signal)),
            Some(CloseReason::StreamError(condition))
        );
    }
}
