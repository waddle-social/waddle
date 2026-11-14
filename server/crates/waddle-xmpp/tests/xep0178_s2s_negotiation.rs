//! XEP-0178 server-to-server suite: STARTTLS, SASL EXTERNAL with domain
//! certificates (XEP-0178 §3), the authenticated restart and the RFC 6120
//! stream error contract, driven through the I/O-agnostic [`S2sStream`] for
//! both roles. TLS is represented by the verified peer identity the
//! connection owner would report; no sockets are involved.
//!
//! The initiating role sends `=` without an authorization identity: RFC 6120
//! §6.3.8 forbids an authzid when not acting for another entity, which takes
//! precedence over the XEP-0178 compatibility recommendation to include one.
//! Server Dialback (XEP-0220) is not implemented; its refusal while disabled
//! is covered here.
//!
//! Peer input is written as raw wire fixtures because the codec under test
//! is the boundary that turns bytes into typed values.

use std::str::FromStr;
use std::sync::Arc;

use jid::DomainPart;
use minidom::Element;
use waddle_xmpp::ns::{SASL, STREAM, TLS};
use waddle_xmpp::s2s::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, CodecLimits, DestinationStatus,
    DialbackMode, HeaderAddress, Inbound, InboundAdmission, InitiatingConfig, NegotiatedStream,
    NegotiationPhase, Output, ReceivingConfig, Role, S2sStream, SaslFailureCondition, SaslLimits,
    Signal, StreamDecoder, StreamErrorCondition, StreamHeader, StreamLanguage, StreamVersion,
    TlsPeer,
};

const INITIAL: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' from='home-a.test' to='home-b.test' \
version='1.0' xml:lang='en'>";
const STARTTLS: &[u8] = b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>";
const AUTH_EMPTY: &[u8] =
    b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>=</auth>";
const AUTH_OMITTED: &[u8] =
    b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'/>";
const ABORT: &[u8] = b"<abort xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>";
const MESSAGE: &[u8] =
    b"<message from='alice@home-a.test/phone' to='bob@home-b.test'><body>hi</body></message>";

const RESPONSE: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' from='home-b.test' to='home-a.test' \
id='s2s-1' version='1.0' xml:lang='en'>";
const FEATURES_STARTTLS: &[u8] = b"<stream:features><starttls \
xmlns='urn:ietf:params:xml:ns:xmpp-tls'><required/></starttls></stream:features>";
const FEATURES_EXTERNAL: &[u8] = b"<stream:features><mechanisms \
xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>EXTERNAL</mechanism></mechanisms>\
</stream:features>";
const FEATURES_EMPTY: &[u8] = b"<stream:features/>";
const PROCEED: &[u8] = b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>";
const SUCCESS: &[u8] = b"<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>";

/// Stream header prepended when decoding output that continues an
/// already-open stream.
const CONTINUATION: &[u8] = b"<stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams'>";

fn domain(value: &str) -> DomainPart {
    DomainPart::from_str(value).expect("valid domain")
}

/// Serves `home-b.test` and `muc.home-b.test`, formerly served
/// `old.home-b.test`, and admits only `home-a.test` → `home-b.test`.
struct Admission;

impl InboundAdmission for Admission {
    fn destination(&self, destination: &DomainPart) -> DestinationStatus {
        match destination.as_str() {
            "home-b.test" | "muc.home-b.test" => DestinationStatus::Served,
            "old.home-b.test" => DestinationStatus::Gone,
            _ => DestinationStatus::Unknown,
        }
    }

    fn admits(&self, source: &DomainPart, destination: &DomainPart) -> bool {
        source.as_str() == "home-a.test" && destination.as_str() == "home-b.test"
    }
}

fn receiving_config() -> ReceivingConfig {
    ReceivingConfig {
        admission: Arc::new(Admission),
        default_domain: domain("home-b.test"),
        language: StreamLanguage::english(),
        sasl: SaslLimits::PILOT,
        dialback: DialbackMode::Disabled,
    }
}

fn initiating_config() -> InitiatingConfig {
    InitiatingConfig {
        source: domain("home-a.test"),
        destination: domain("home-b.test"),
        language: StreamLanguage::english(),
        dialback: DialbackMode::Disabled,
    }
}

fn receiver() -> S2sStream {
    S2sStream::accept(receiving_config(), CodecLimits::PILOT)
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn transmitted(outputs: &[Output]) -> Vec<u8> {
    outputs
        .iter()
        .filter_map(|output| match output {
            Output::Transmit(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .concat()
}

fn close_reason(outputs: &[Output]) -> Option<CloseReason> {
    outputs.iter().find_map(|output| match output {
        Output::Close(reason) => Some(*reason),
        _ => None,
    })
}

fn authorization_request(outputs: &[Output]) -> AuthorizationRequest {
    outputs
        .iter()
        .find_map(|output| match output {
            Output::Authorize(request) => Some(request.clone()),
            _ => None,
        })
        .expect("authorization requested")
}

fn handoff(outputs: Vec<Output>) -> Vec<u8> {
    outputs
        .into_iter()
        .find_map(|output| match output {
            Output::StartTls(handoff) => Some(handoff.into_bytes()),
            _ => None,
        })
        .expect("TLS started")
}

/// Decode one outbound stream segment that starts with a stream header.
fn stream_items(bytes: &[u8]) -> Vec<Inbound> {
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    decoder.push(bytes).expect("peer output fits the codec");
    let mut items = Vec::new();
    while let Some(item) = decoder.next_item().expect("peer output is well-formed") {
        items.push(item);
    }
    items
}

/// Decode output that continues an already-open stream.
fn continuation_elements(bytes: &[u8]) -> Vec<Element> {
    stream_items(&concat(&[CONTINUATION, bytes]))
        .into_iter()
        .filter_map(|item| match item {
            Inbound::Element(element) => Some(element),
            _ => None,
        })
        .collect()
}

/// The bytes following the leading stream header of an outbound segment.
fn after_header(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    decoder.push(bytes).expect("push");
    match decoder.next_item() {
        Ok(Some(Inbound::Header(_))) => {}
        other => panic!("expected a stream header, got {other:?}"),
    }
    // The decoder stops right after the header, so a restart returns
    // exactly the remaining bytes.
    decoder.restart_after_tls().into_bytes()
}

fn expect_header(item: Option<&Inbound>) -> &StreamHeader {
    match item {
        Some(Inbound::Header(header)) => header,
        other => panic!("expected a stream header, got {other:?}"),
    }
}

fn sasl_failure(outputs: &[Output]) -> SaslFailureCondition {
    let element = continuation_elements(&transmitted(outputs))
        .into_iter()
        .find(|element| element.is("failure", SASL))
        .expect("SASL failure sent");
    SaslFailureCondition::from_peer_element(element)
}

/// A receiving stream awaiting `<auth/>` on the TLS-protected stream.
fn receiver_at_sasl() -> S2sStream {
    let mut stream = receiver();
    stream.receive(INITIAL);
    assert!(handoff(stream.receive(STARTTLS)).is_empty());
    assert!(stream
        .tls_established(TlsPeer::Verified(domain("home-a.test")))
        .is_empty());
    let out = stream.receive(INITIAL);
    assert_eq!(close_reason(&out), None);
    assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitAuth);
    stream
}

fn receiver_established() -> S2sStream {
    let mut stream = receiver_at_sasl();
    let request = authorization_request(&stream.receive(AUTH_EMPTY));
    let out = stream.authorization(request.exchange, AuthorizationDecision::Granted);
    assert_eq!(close_reason(&out), None);
    let out = stream.receive(INITIAL);
    assert!(out
        .iter()
        .any(|output| matches!(output, Output::Established(_))));
    assert_eq!(stream.phase(), NegotiationPhase::Established);
    stream
}

fn assert_stream_error(outputs: &[Output], condition: StreamErrorCondition) {
    assert_eq!(
        close_reason(outputs),
        Some(CloseReason::StreamError(condition))
    );
}

// --- end-to-end -----------------------------------------------------------

#[derive(Default)]
struct Side {
    inbox: Vec<u8>,
    tls_pending: bool,
    handoffs: Vec<Vec<u8>>,
    established: Option<NegotiatedStream>,
    closes: Vec<CloseReason>,
    authorizations: Vec<AuthorizationRequest>,
}

fn absorb(outputs: Vec<Output>, me: &mut Side, peer: &mut Side) {
    for output in outputs {
        match output {
            Output::Transmit(bytes) => peer.inbox.extend(bytes),
            Output::StartTls(handoff) => {
                me.handoffs.push(handoff.into_bytes());
                me.tls_pending = true;
            }
            Output::Authorize(request) => me.authorizations.push(request),
            Output::Established(stream) => me.established = Some(stream),
            Output::Close(reason) => me.closes.push(reason),
        }
    }
}

/// Run both roles against each other, delivering bytes in `chunk`-sized
/// reads, until neither side has anything left to do.
fn handshake(chunk: usize) -> (S2sStream, S2sStream, Side, Side) {
    let (mut a, out) = S2sStream::initiate(initiating_config(), CodecLimits::PILOT);
    let mut b = receiver();
    let mut a_side = Side::default();
    let mut b_side = Side::default();
    absorb(out, &mut a_side, &mut b_side);
    let mut granted = 0;
    for _ in 0..64 {
        let mut progress = false;
        if !b_side.inbox.is_empty() {
            let bytes = std::mem::take(&mut b_side.inbox);
            for piece in bytes.chunks(chunk) {
                absorb(b.receive(piece), &mut b_side, &mut a_side);
            }
            progress = true;
        }
        if !a_side.inbox.is_empty() {
            let bytes = std::mem::take(&mut a_side.inbox);
            for piece in bytes.chunks(chunk) {
                absorb(a.receive(piece), &mut a_side, &mut b_side);
            }
            progress = true;
        }
        if a_side.tls_pending && b_side.tls_pending {
            a_side.tls_pending = false;
            b_side.tls_pending = false;
            let out = b.tls_established(TlsPeer::Verified(domain("home-a.test")));
            absorb(out, &mut b_side, &mut a_side);
            let out = a.tls_established(TlsPeer::Verified(domain("home-b.test")));
            absorb(out, &mut a_side, &mut b_side);
            progress = true;
        }
        while granted < b_side.authorizations.len() {
            let exchange = b_side.authorizations[granted].exchange;
            granted += 1;
            let out = b.authorization(exchange, AuthorizationDecision::Granted);
            absorb(out, &mut b_side, &mut a_side);
            progress = true;
        }
        if !progress {
            break;
        }
    }
    (a, b, a_side, b_side)
}

#[test]
fn both_roles_establish_under_every_read_segmentation() {
    for chunk in [usize::MAX, 1, 2, 3, 7, 64] {
        let (a, b, a_side, b_side) = handshake(chunk);
        assert_eq!(a_side.closes, [], "chunk {chunk}");
        assert_eq!(b_side.closes, [], "chunk {chunk}");
        assert_eq!(
            a_side.established,
            Some(NegotiatedStream {
                role: Role::Initiating,
                source: domain("home-a.test"),
                destination: domain("home-b.test"),
            }),
            "chunk {chunk}"
        );
        assert_eq!(
            b_side.established,
            Some(NegotiatedStream {
                role: Role::Receiving,
                source: domain("home-a.test"),
                destination: domain("home-b.test"),
            }),
            "chunk {chunk}"
        );
        assert_eq!(a.phase(), NegotiationPhase::Established);
        assert_eq!(b.phase(), NegotiationPhase::Established);
        assert_eq!(a_side.handoffs, [Vec::<u8>::new()]);
        assert_eq!(b_side.handoffs, [Vec::<u8>::new()]);
        let request = &b_side.authorizations[..];
        assert_eq!(request.len(), 1);
        assert_eq!(request[0].source, domain("home-a.test"));
        assert_eq!(request[0].destination, domain("home-b.test"));
        assert_eq!(b.retained_bytes(), 0);
        assert_eq!(a.retained_bytes(), 0);
    }
}

// --- receiving role: stream headers -------------------------------------

#[test]
fn response_headers_use_fresh_ids_and_echo_the_pair() {
    let mut stream = receiver();
    let first = stream_items(&transmitted(&stream.receive(INITIAL)));
    let header = expect_header(first.first());
    assert_eq!(header.from, HeaderAddress::Domain(domain("home-b.test")));
    assert_eq!(header.to, HeaderAddress::Domain(domain("home-a.test")));
    assert_eq!(header.version, Some(StreamVersion::V1_0));
    assert_eq!(header.lang, Some(StreamLanguage::english()));
    let first_id = header.id.clone().expect("stream id");

    // Pre-TLS features advertise only mandatory STARTTLS.
    let Some(Inbound::Element(features)) = first.get(1) else {
        panic!("features expected");
    };
    assert!(features.is("features", STREAM));
    let starttls = features
        .get_child("starttls", TLS)
        .expect("starttls offered");
    assert!(starttls.get_child("required", TLS).is_some());
    assert_eq!(features.children().count(), 1);

    let _ = handoff(stream.receive(STARTTLS));
    let _ = stream.tls_established(TlsPeer::Verified(domain("home-a.test")));
    let second = stream_items(&transmitted(&stream.receive(INITIAL)));
    let header = expect_header(second.first());
    assert_ne!(header.id.clone().expect("stream id"), first_id);

    // Post-TLS features advertise only SASL EXTERNAL.
    let Some(Inbound::Element(features)) = second.get(1) else {
        panic!("features expected");
    };
    let mechanisms = features.get_child("mechanisms", SASL).expect("mechanisms");
    let offered: Vec<String> = mechanisms.children().map(Element::text).collect();
    assert_eq!(offered, ["EXTERNAL"]);
    assert!(features.get_child("starttls", TLS).is_none());
}

#[test]
fn initial_header_errors_are_sent_inside_a_response_header() {
    let cases: [(&[u8], StreamErrorCondition); 10] = [
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' version='1.0'>",
            StreamErrorCondition::BadFormat,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='unknown.test' version='1.0'>",
            StreamErrorCondition::HostUnknown,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='old.home-b.test' version='1.0'>",
            StreamErrorCondition::HostGone,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
to='home-b.test' version='1.0'>",
            StreamErrorCondition::BadFormat,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='alice@home-a.test' to='home-b.test' version='1.0'>",
            StreamErrorCondition::BadFormat,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='stranger.test' to='home-b.test' version='1.0'>",
            StreamErrorCondition::InvalidFrom,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test'>",
            StreamErrorCondition::UnsupportedVersion,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test' version='0.9'>",
            StreamErrorCondition::UnsupportedVersion,
        ),
        (
            b"<stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='home-b.test' version='1.0'>",
            StreamErrorCondition::InvalidNamespace,
        ),
        (b"<stream:stream <<", StreamErrorCondition::NotWellFormed),
    ];
    for (input, condition) in cases {
        let mut stream = receiver();
        let out = stream.receive(input);
        assert_stream_error(&out, condition);
        assert!(stream.is_closed());
        let items = stream_items(&transmitted(&out));
        let header = expect_header(items.first());
        // The error header names a local domain and reflects nothing from
        // the rejected header.
        assert_eq!(header.from, HeaderAddress::Domain(domain("home-b.test")));
        assert_eq!(header.to, HeaderAddress::Absent);
        assert!(header.id.is_some());
        let Some(Inbound::Element(error)) = items.get(1) else {
            panic!("stream error expected for {condition}");
        };
        assert_eq!(
            StreamErrorCondition::from_peer_element(error.clone()),
            condition
        );
        assert_eq!(error.children().count(), 1, "no text or extra elements");
        assert!(matches!(items.get(2), Some(Inbound::End)));
        assert!(stream.receive(MESSAGE).is_empty());
    }
}

#[test]
fn stanza_before_authentication_is_not_authorized() {
    let mut stream = receiver();
    assert_stream_error(
        &stream.receive(&concat(&[INITIAL, MESSAGE])),
        StreamErrorCondition::NotAuthorized,
    );
    let mut stream = receiver_at_sasl();
    assert_stream_error(
        &stream.receive(MESSAGE),
        StreamErrorCondition::NotAuthorized,
    );
}

#[test]
fn stream_namespace_elements_use_the_stream_prefix() {
    // RFC 6120 §4.8.5 allows a peer to require the `stream` prefix, so
    // features and stream errors are written with the prefix bound on the
    // response header rather than redeclaring the stream namespace.
    let mut stream = receiver();
    let out = stream.receive(INITIAL);
    assert_eq!(after_header(&transmitted(&out)), FEATURES_STARTTLS);

    let mut stream = receiver();
    stream.receive(INITIAL);
    let out = stream.receive(MESSAGE);
    assert_stream_error(&out, StreamErrorCondition::NotAuthorized);
    assert_eq!(
        transmitted(&out),
        b"<stream:error><not-authorized xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>\
</stream:error></stream:stream>"
    );
}

#[test]
fn negotiation_commands_out_of_order_are_policy_violations() {
    let mut stream = receiver();
    assert_stream_error(
        &stream.receive(&concat(&[INITIAL, AUTH_EMPTY])),
        StreamErrorCondition::PolicyViolation,
    );
    let mut stream = receiver_at_sasl();
    assert_stream_error(
        &stream.receive(STARTTLS),
        StreamErrorCondition::PolicyViolation,
    );
    let mut stream = receiver_at_sasl();
    assert_stream_error(
        &stream.receive(b"<response xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>"),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn dialback_is_refused_while_disabled() {
    let mut stream = receiver();
    assert_stream_error(
        &stream.receive(&concat(&[
            INITIAL,
            b"<db:result xmlns:db='jabber:server:dialback' from='home-a.test' \
to='home-b.test'>key</db:result>",
        ])),
        StreamErrorCondition::PolicyViolation,
    );
    // A certificate that cannot support EXTERNAL would need dialback.
    let mut stream = receiver();
    stream.receive(INITIAL);
    let _ = handoff(stream.receive(STARTTLS));
    let _ = stream.tls_established(TlsPeer::NoCertificate);
    assert_stream_error(
        &stream.receive(INITIAL),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn bytes_pipelined_after_starttls_are_refused() {
    // RFC 6120 §5.4.2.3: TLS starts only after `<proceed/>`, so anything the
    // initiating entity sent before reading it is neither XML of this stream
    // nor a handshake it was invited to start.
    let pipelined: [&[u8]; 3] = [
        b"\x16\x03\x01\x02\x00<message from='x@home-a.test' to='y@home-b.test'/>",
        b"<message from='x@home-a.test' to='y@home-b.test'/>",
        b" ",
    ];
    for tail in pipelined {
        let mut stream = receiver();
        let out = stream.receive(&concat(&[INITIAL, STARTTLS, tail]));
        assert_stream_error(&out, StreamErrorCondition::PolicyViolation);
        assert!(!out
            .iter()
            .any(|output| matches!(output, Output::StartTls(_))));
        let elements = continuation_elements(&after_header(&transmitted(&out)));
        assert!(!elements.iter().any(|element| element.is("proceed", TLS)));
        let error = elements
            .iter()
            .find(|element| element.is("error", STREAM))
            .expect("stream error sent in plaintext");
        assert_eq!(
            StreamErrorCondition::from_peer_element(error.clone()),
            StreamErrorCondition::PolicyViolation
        );
        assert!(stream.is_closed());
    }

    // Without pipelined input TLS starts with an empty handoff; whatever
    // arrives afterwards is read from the socket by the TLS engine.
    let mut stream = receiver();
    stream.receive(INITIAL);
    assert!(handoff(stream.receive(STARTTLS)).is_empty());
    assert_eq!(stream.phase(), NegotiationPhase::TlsHandshake);
    assert!(!stream.is_closed());
}

#[test]
fn post_tls_header_must_repeat_the_negotiated_pair() {
    let cases: [(&[u8], StreamErrorCondition); 2] = [
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='muc.home-a.test' to='home-b.test' version='1.0'>",
            StreamErrorCondition::InvalidFrom,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-a.test' to='muc.home-b.test' version='1.0'>",
            StreamErrorCondition::HostUnknown,
        ),
    ];
    for (header, condition) in cases {
        let mut stream = receiver();
        stream.receive(INITIAL);
        let _ = handoff(stream.receive(STARTTLS));
        let _ = stream.tls_established(TlsPeer::Verified(domain("home-a.test")));
        let out = stream.receive(header);
        assert_stream_error(&out, condition);
        let items = stream_items(&transmitted(&out));
        assert!(matches!(items.first(), Some(Inbound::Header(_))));
    }
}

#[test]
fn certificate_identity_must_match_the_stream_source() {
    let mut stream = receiver();
    stream.receive(INITIAL);
    let _ = handoff(stream.receive(STARTTLS));
    let _ = stream.tls_established(TlsPeer::Verified(domain("evil.test")));
    assert_stream_error(&stream.receive(INITIAL), StreamErrorCondition::InvalidFrom);
}

// --- receiving role: SASL EXTERNAL --------------------------------------

#[test]
fn empty_initial_response_requests_authorization_for_the_verified_source() {
    let mut stream = receiver_at_sasl();
    let out = stream.receive(AUTH_EMPTY);
    assert!(transmitted(&out).is_empty());
    let request = authorization_request(&out);
    assert_eq!(request.source, domain("home-a.test"));
    assert_eq!(request.destination, domain("home-b.test"));
    assert_eq!(stream.phase(), NegotiationPhase::SaslAuthorizing);

    let out = stream.authorization(request.exchange, AuthorizationDecision::Granted);
    let elements = continuation_elements(&transmitted(&out));
    assert_eq!(elements.len(), 1);
    assert!(elements[0].is("success", SASL));
    assert_eq!(elements[0].text(), "");
    assert_eq!(stream.phase(), NegotiationPhase::AuthenticatedRestart);

    // Success is emitted exactly once.
    assert!(stream
        .authorization(request.exchange, AuthorizationDecision::Granted)
        .is_empty());

    let out = stream.receive(INITIAL);
    let items = stream_items(&transmitted(&out));
    let Some(Inbound::Element(features)) = items.get(1) else {
        panic!("final features expected");
    };
    assert!(features.is("features", STREAM));
    assert_eq!(features.children().count(), 0);
    assert_eq!(stream.phase(), NegotiationPhase::Established);
}

#[test]
fn omitted_initial_response_gets_one_empty_challenge() {
    for response in [
        &b"<response xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>"[..],
        b"<response xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>=</response>",
        b"<response xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>aG9tZS1hLnRlc3Q=</response>",
    ] {
        let mut stream = receiver_at_sasl();
        let out = stream.receive(AUTH_OMITTED);
        let elements = continuation_elements(&transmitted(&out));
        assert_eq!(elements.len(), 1);
        assert!(elements[0].is("challenge", SASL));
        assert_eq!(elements[0].text(), "");
        assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitResponse);

        let out = stream.receive(response);
        assert_eq!(authorization_request(&out).source, domain("home-a.test"));
    }
}

#[test]
fn matching_authorization_identity_is_tolerated() {
    let mut stream = receiver_at_sasl();
    let out = stream.receive(
        b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>aG9tZS1hLnRlc3Q=</auth>",
    );
    assert_eq!(authorization_request(&out).source, domain("home-a.test"));
}

#[test]
fn sasl_failures_map_to_their_conditions() {
    let cases: [(&[u8], SaslFailureCondition); 7] = [
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>aG9tZS1iLnRlc3Q=</auth>",
            SaslFailureCondition::InvalidAuthzid,
        ),
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>YWxpY2VAaG9tZS1hLnRlc3Q=</auth>",
            SaslFailureCondition::InvalidAuthzid,
        ),
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>!!!</auth>",
            SaslFailureCondition::IncorrectEncoding,
        ),
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>//4=</auth>",
            SaslFailureCondition::IncorrectEncoding,
        ),
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'><x/></auth>",
            SaslFailureCondition::MalformedRequest,
        ),
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>AGEAYg==</auth>",
            SaslFailureCondition::InvalidMechanism,
        ),
        (
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>=</auth>",
            SaslFailureCondition::InvalidMechanism,
        ),
    ];
    for (auth, condition) in cases {
        let mut stream = receiver_at_sasl();
        let out = stream.receive(auth);
        assert_eq!(sasl_failure(&out), condition);
        assert_eq!(
            close_reason(&out),
            Some(CloseReason::SaslFailure(condition))
        );
        assert!(transmitted(&out).ends_with(b"</stream:stream>"));
        assert!(!out
            .iter()
            .any(|output| matches!(output, Output::Authorize(_))));
    }
}

#[test]
fn denied_authorization_is_not_authorized() {
    let mut stream = receiver_at_sasl();
    let request = authorization_request(&stream.receive(AUTH_EMPTY));
    let out = stream.authorization(request.exchange, AuthorizationDecision::Denied);
    assert_eq!(sasl_failure(&out), SaslFailureCondition::NotAuthorized);
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::SaslFailure(
            SaslFailureCondition::NotAuthorized
        ))
    );
}

#[test]
fn replacement_auth_makes_pending_decisions_stale() {
    let mut stream = receiver_at_sasl();
    let first = authorization_request(&stream.receive(AUTH_EMPTY));
    let out = stream.receive(AUTH_EMPTY);
    assert!(
        transmitted(&out).is_empty(),
        "no failure for the replaced exchange"
    );
    let second = authorization_request(&out);
    assert_ne!(first.exchange, second.exchange);

    assert!(stream
        .authorization(first.exchange, AuthorizationDecision::Granted)
        .is_empty());
    assert_eq!(stream.phase(), NegotiationPhase::SaslAuthorizing);
    let out = stream.authorization(second.exchange, AuthorizationDecision::Granted);
    assert!(continuation_elements(&transmitted(&out))[0].is("success", SASL));
}

#[test]
fn replacement_before_response_starts_a_new_exchange() {
    let mut stream = receiver_at_sasl();
    stream.receive(AUTH_OMITTED);
    let out = stream.receive(AUTH_EMPTY);
    assert!(transmitted(&out).is_empty());
    authorization_request(&out);

    let mut stream = receiver_at_sasl();
    stream.receive(AUTH_OMITTED);
    let out = stream.receive(b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'/>");
    assert_eq!(sasl_failure(&out), SaslFailureCondition::InvalidMechanism);
}

#[test]
fn abort_is_acknowledged_until_the_abort_budget_is_spent() {
    let mut stream = receiver_at_sasl();
    for _ in 1..SaslLimits::PILOT.max_aborts {
        let out = stream.receive(ABORT);
        assert_eq!(sasl_failure(&out), SaslFailureCondition::Aborted);
        assert_eq!(close_reason(&out), None);
        assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitAuth);
    }
    let out = stream.receive(ABORT);
    assert_eq!(sasl_failure(&out), SaslFailureCondition::Aborted);
    assert_eq!(close_reason(&out), Some(CloseReason::SaslExhausted));
}

#[test]
fn abort_during_authorization_invalidates_the_exchange() {
    let mut stream = receiver_at_sasl();
    let request = authorization_request(&stream.receive(AUTH_EMPTY));
    let out = stream.receive(ABORT);
    assert_eq!(sasl_failure(&out), SaslFailureCondition::Aborted);
    assert!(stream
        .authorization(request.exchange, AuthorizationDecision::Granted)
        .is_empty());
    assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitAuth);
}

#[test]
fn fourth_auth_exhausts_the_exchange_budget() {
    let mut stream = receiver_at_sasl();
    for _ in 0..SaslLimits::PILOT.max_auth_starts {
        let out = stream.receive(AUTH_OMITTED);
        assert_eq!(close_reason(&out), None);
    }
    assert_stream_error(
        &stream.receive(AUTH_OMITTED),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn abort_after_the_last_auth_start_closes() {
    let mut stream = receiver_at_sasl();
    for _ in 0..SaslLimits::PILOT.max_auth_starts {
        stream.receive(AUTH_OMITTED);
    }
    let out = stream.receive(ABORT);
    assert_eq!(sasl_failure(&out), SaslFailureCondition::Aborted);
    assert_eq!(close_reason(&out), Some(CloseReason::SaslExhausted));
}

#[test]
fn stanza_pipelined_after_auth_is_never_authenticated() {
    let mut stream = receiver_at_sasl();
    let out = stream.receive(&concat(&[AUTH_EMPTY, MESSAGE]));
    let request = authorization_request(&out);
    assert_stream_error(&out, StreamErrorCondition::NotAuthorized);
    assert!(stream
        .authorization(request.exchange, AuthorizationDecision::Granted)
        .is_empty());
}

#[test]
fn input_before_the_post_sasl_header_is_premature() {
    let mut stream = receiver_at_sasl();
    let request = authorization_request(&stream.receive(&concat(&[AUTH_EMPTY, b"<mess"])));
    let out = stream.authorization(request.exchange, AuthorizationDecision::Granted);
    assert_stream_error(&out, StreamErrorCondition::PolicyViolation);

    // Success is sent on the old stream; the error is carried by a fresh
    // response header on the new one.
    let bytes = transmitted(&out);
    let restart = bytes
        .windows(5)
        .position(|window| window == b"<?xml")
        .expect("new stream header");
    assert!(continuation_elements(&bytes[..restart])[0].is("success", SASL));
    let items = stream_items(&bytes[restart..]);
    let Some(Inbound::Element(error)) = items.get(1) else {
        panic!("stream error expected");
    };
    assert_eq!(
        StreamErrorCondition::from_peer_element(error.clone()),
        StreamErrorCondition::PolicyViolation
    );
}

#[test]
fn authenticated_restart_must_repeat_the_pair() {
    let mut stream = receiver_at_sasl();
    let request = authorization_request(&stream.receive(AUTH_EMPTY));
    let _ = stream.authorization(request.exchange, AuthorizationDecision::Granted);
    assert_stream_error(
        &stream.receive(
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='muc.home-a.test' to='home-b.test' version='1.0'>",
        ),
        StreamErrorCondition::InvalidFrom,
    );
}

// --- receiving role: established stream ---------------------------------

#[test]
fn established_stream_checks_addresses_then_refuses_dispatch() {
    let cases: [(&[u8], StreamErrorCondition); 13] = [
        (
            b"<message to='bob@home-b.test'/>",
            StreamErrorCondition::ImproperAddressing,
        ),
        (
            b"<message from='alice@home-a.test'/>",
            StreamErrorCondition::ImproperAddressing,
        ),
        (
            b"<message from='' to='bob@home-b.test'/>",
            StreamErrorCondition::ImproperAddressing,
        ),
        (
            b"<message from='alice@home-a.test' to='bob@'/>",
            StreamErrorCondition::ImproperAddressing,
        ),
        (
            b"<message from='alice@home-a.test' to='bob@elsewhere.test'/>",
            StreamErrorCondition::HostUnknown,
        ),
        (
            b"<presence from='alice@home-a.test' to='room@muc.home-b.test/nick'/>",
            StreamErrorCondition::HostUnknown,
        ),
        (
            b"<message from='mallory@evil.test' to='bob@home-b.test'/>",
            StreamErrorCondition::InvalidFrom,
        ),
        (MESSAGE, StreamErrorCondition::PolicyViolation),
        (
            b"<iq type='result' id='1' from='home-a.test' to='home-b.test'/>",
            StreamErrorCondition::PolicyViolation,
        ),
        (
            b"<message xmlns='jabber:client' from='alice@home-a.test' to='bob@home-b.test'/>",
            StreamErrorCondition::InvalidNamespace,
        ),
        (
            b"<foo xmlns='urn:example:foo' from='alice@home-a.test' to='bob@home-b.test'/>",
            StreamErrorCondition::UnsupportedStanzaType,
        ),
        (AUTH_EMPTY, StreamErrorCondition::PolicyViolation),
        (
            b"<db:verify xmlns:db='jabber:server:dialback' from='home-a.test' to='home-b.test' \
id='1'>key</db:verify>",
            StreamErrorCondition::PolicyViolation,
        ),
    ];
    for (stanza, condition) in cases {
        let mut stream = receiver_established();
        let out = stream.receive(stanza);
        assert_stream_error(&out, condition);
        let elements = continuation_elements(&transmitted(&out));
        assert_eq!(
            StreamErrorCondition::from_peer_element(elements[0].clone()),
            condition
        );
    }
}

#[test]
fn established_stream_enforces_codec_limits() {
    let mut stream = receiver_established();
    let mut stanza = b"<message from='alice@home-a.test' to='bob@home-b.test'><body>".to_vec();
    stanza.extend(std::iter::repeat_n(b'x', 70 * 1024));
    assert_stream_error(
        &stream.receive(&stanza),
        StreamErrorCondition::PolicyViolation,
    );

    let mut stream = receiver_established();
    assert_stream_error(
        &stream.receive(b"<!-- comment -->"),
        StreamErrorCondition::RestrictedXml,
    );
}

#[test]
fn peer_stream_error_closes_without_an_error_reply() {
    let mut stream = receiver_established();
    let out = stream.receive(
        b"<stream:error><host-unknown xmlns='urn:ietf:params:xml:ns:xmpp-streams'/></stream:error>",
    );
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::PeerStreamError(
            StreamErrorCondition::HostUnknown
        ))
    );
    assert_eq!(transmitted(&out), b"</stream:stream>");
}

#[test]
fn peer_close_is_reciprocated() {
    let mut stream = receiver_established();
    let out = stream.receive(b"</stream:stream>");
    assert_eq!(close_reason(&out), Some(CloseReason::PeerClosed));
    assert_eq!(transmitted(&out), b"</stream:stream>");
}

#[test]
fn signals_close_with_their_stream_errors() {
    for (signal, condition) in [
        (Signal::Timeout, StreamErrorCondition::ConnectionTimeout),
        (Signal::Expired, StreamErrorCondition::Reset),
        (Signal::Shutdown, StreamErrorCondition::SystemShutdown),
        (
            Signal::ResourceExhausted,
            StreamErrorCondition::ResourceConstraint,
        ),
    ] {
        let mut stream = receiver_established();
        let out = stream.signal(signal);
        assert_stream_error(&out, condition);
        let elements = continuation_elements(&transmitted(&out));
        assert_eq!(
            StreamErrorCondition::from_peer_element(elements[0].clone()),
            condition
        );
    }

    // Before any header, the error still needs a response header.
    let mut stream = receiver();
    let out = stream.signal(Signal::Timeout);
    let items = stream_items(&transmitted(&out));
    assert!(matches!(items.first(), Some(Inbound::Header(_))));

    // During the TLS handshake no XML may be sent.
    let mut stream = receiver();
    stream.receive(INITIAL);
    let _ = handoff(stream.receive(STARTTLS));
    let out = stream.signal(Signal::Timeout);
    assert_stream_error(&out, StreamErrorCondition::ConnectionTimeout);
    assert!(transmitted(&out).is_empty());
}

// --- initiating role ------------------------------------------------------

fn initiator() -> (S2sStream, Vec<u8>) {
    let (stream, out) = S2sStream::initiate(initiating_config(), CodecLimits::PILOT);
    (stream, transmitted(&out))
}

/// An initiating stream awaiting the TLS-protected response header.
fn initiator_after_tls() -> S2sStream {
    let (mut stream, _) = initiator();
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_STARTTLS]));
    assert_eq!(continuation_elements(&transmitted(&out)).len(), 1);
    assert!(handoff(stream.receive(PROCEED)).is_empty());
    let out = stream.tls_established(TlsPeer::Verified(domain("home-b.test")));
    assert!(matches!(
        stream_items(&transmitted(&out)).first(),
        Some(Inbound::Header(_))
    ));
    stream
}

#[test]
fn initiator_opens_with_its_source_and_destination() {
    let (stream, bytes) = initiator();
    let items = stream_items(&bytes);
    let header = expect_header(items.first());
    assert_eq!(header.from, HeaderAddress::Domain(domain("home-a.test")));
    assert_eq!(header.to, HeaderAddress::Domain(domain("home-b.test")));
    assert_eq!(header.version, Some(StreamVersion::V1_0));
    assert_eq!(header.id, None);
    assert_eq!(stream.role(), Role::Initiating);
    assert_eq!(stream.phase(), NegotiationPhase::PreTls);
}

#[test]
fn initiator_authenticates_with_external_and_no_authorization_identity() {
    let mut stream = initiator_after_tls();
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    let elements = continuation_elements(&transmitted(&out));
    assert_eq!(elements.len(), 1);
    let auth = &elements[0];
    assert!(auth.is("auth", SASL));
    assert_eq!(auth.attr("mechanism"), Some("EXTERNAL"));
    assert_eq!(auth.text(), "=");
    assert_eq!(auth.children().count(), 0);
    assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitOutcome);

    let out = stream.receive(SUCCESS);
    let items = stream_items(&transmitted(&out));
    let header = expect_header(items.first());
    assert_eq!(header.from, HeaderAddress::Domain(domain("home-a.test")));
    assert_eq!(stream.phase(), NegotiationPhase::AuthenticatedRestart);

    let out = stream.receive(&concat(&[RESPONSE, FEATURES_EMPTY]));
    assert_eq!(
        out,
        [Output::Established(NegotiatedStream {
            role: Role::Initiating,
            source: domain("home-a.test"),
            destination: domain("home-b.test"),
        })]
    );
}

#[test]
fn initiator_requests_starttls_even_when_it_is_stripped() {
    let (mut stream, _) = initiator();
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_EMPTY]));
    let elements = continuation_elements(&transmitted(&out));
    assert!(elements[0].is("starttls", TLS));
    assert_eq!(stream.phase(), NegotiationPhase::PreTls);
}

#[test]
fn initiator_checks_the_response_header() {
    let cases: [(&[u8], StreamErrorCondition); 5] = [
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='evil.test' to='home-a.test' id='1' version='1.0'>",
            StreamErrorCondition::InvalidFrom,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
to='home-a.test' id='1' version='1.0'>",
            StreamErrorCondition::BadFormat,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-b.test' to='other.test' id='1' version='1.0'>",
            StreamErrorCondition::HostUnknown,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-b.test' to='home-a.test' version='1.0'>",
            StreamErrorCondition::BadFormat,
        ),
        (
            b"<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-b.test' to='home-a.test' id='1'>",
            StreamErrorCondition::UnsupportedVersion,
        ),
    ];
    for (header, condition) in cases {
        let (mut stream, _) = initiator();
        let out = stream.receive(header);
        assert_stream_error(&out, condition);
        let elements = continuation_elements(&transmitted(&out));
        assert_eq!(
            StreamErrorCondition::from_peer_element(elements[0].clone()),
            condition
        );
    }
}

#[test]
fn initiator_hands_bytes_after_proceed_to_tls() {
    let (mut stream, _) = initiator();
    stream.receive(&concat(&[RESPONSE, FEATURES_STARTTLS]));
    let tls_bytes: &[u8] = b"\x16\x03\x03\x00\x7a<stream:error/>";
    let out = stream.receive(&concat(&[PROCEED, tls_bytes]));
    assert_eq!(close_reason(&out), None);
    assert_eq!(handoff(out), tls_bytes);
    assert_eq!(stream.phase(), NegotiationPhase::TlsHandshake);
}

#[test]
fn initiator_closes_when_starttls_is_refused() {
    let (mut stream, _) = initiator();
    stream.receive(&concat(&[RESPONSE, FEATURES_STARTTLS]));
    let out = stream.receive(b"<failure xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>");
    assert_eq!(close_reason(&out), Some(CloseReason::TlsRefused));
    assert_eq!(transmitted(&out), b"</stream:stream>");
}

#[test]
fn initiator_rejects_an_unexpected_tls_peer_without_xml() {
    let (mut stream, _) = initiator();
    stream.receive(&concat(&[RESPONSE, FEATURES_STARTTLS]));
    let _ = handoff(stream.receive(PROCEED));
    let out = stream.tls_established(TlsPeer::Verified(domain("evil.test")));
    assert_stream_error(&out, StreamErrorCondition::PolicyViolation);
    assert!(transmitted(&out).is_empty());
}

#[test]
fn initiator_refuses_streams_without_external() {
    let cases: [&[u8]; 3] = [
        b"<stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>\
<mechanism>PLAIN</mechanism></mechanisms></stream:features>",
        b"<stream:features><starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/><mechanisms \
xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>EXTERNAL</mechanism></mechanisms>\
</stream:features>",
        b"<stream:features><dialback xmlns='urn:xmpp:features:dialback'/></stream:features>",
    ];
    for features in cases {
        let mut stream = initiator_after_tls();
        assert_stream_error(
            &stream.receive(&concat(&[RESPONSE, features])),
            StreamErrorCondition::PolicyViolation,
        );
    }
}

#[test]
fn initiator_sasl_outcomes() {
    let mut stream = initiator_after_tls();
    stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    let out = stream
        .receive(b"<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::SaslFailure(
            SaslFailureCondition::NotAuthorized
        ))
    );

    for outcome in [
        &b"<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>ZGF0YQ==</success>"[..],
        b"<challenge xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>",
    ] {
        let mut stream = initiator_after_tls();
        stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
        assert_stream_error(
            &stream.receive(outcome),
            StreamErrorCondition::PolicyViolation,
        );
    }
}

#[test]
fn initiator_treats_bytes_after_success_as_premature() {
    let mut stream = initiator_after_tls();
    stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    let out = stream.receive(&concat(&[SUCCESS, RESPONSE]));
    assert_stream_error(&out, StreamErrorCondition::PolicyViolation);
    // The error is sent on the new stream the initiator opened.
    let items = stream_items(&transmitted(&out));
    assert!(matches!(items.first(), Some(Inbound::Header(_))));
}

#[test]
fn initiator_refuses_further_negotiation_after_sasl() {
    let mut stream = initiator_after_tls();
    stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    stream.receive(SUCCESS);
    assert_stream_error(
        &stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL])),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn initiator_refuses_inbound_stanzas() {
    let mut stream = initiator_after_tls();
    stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    stream.receive(SUCCESS);
    stream.receive(&concat(&[RESPONSE, FEATURES_EMPTY]));
    assert_eq!(stream.phase(), NegotiationPhase::Established);
    assert_stream_error(
        &stream.receive(b"<message from='bob@home-b.test' to='alice@home-a.test'/>"),
        StreamErrorCondition::PolicyViolation,
    );
}
