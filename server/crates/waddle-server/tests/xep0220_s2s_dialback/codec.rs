//! The codec's dialback roles, driven byte by byte without sockets: the
//! Receiving Server (inbound dialback), the Authoritative Server (answers to
//! `db:verify`, at a bounded rate per stream), the Initiating Server
//! (outbound dialback, only to a receiving server that advertised it),
//! verification connections and the XEP-0368 direct-TLS entry.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use minidom::Element;
use waddle_server::s2s::config::STREAM_CODEC_LIMITS;
use waddle_server::s2s::dialback::DialbackSecret;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::ns::{DIALBACK_FEATURES, JABBER_SERVER_DIALBACK, SASL, STANZAS, STREAM};
use waddle_xmpp::s2s::{
    AuthorizationDecision, CloseReason, CodecLimits, DestinationStatus, DialbackError, DialbackKey,
    DialbackKeys, DialbackMode, DialbackOutcome, DialbackRequest, Inbound, InboundAdmission,
    InitiatingConfig, NegotiationClock, Output, ReceivingConfig, S2sStream, SaslLimits,
    StreamAuthentication, StreamDecoder, StreamErrorCondition, StreamHeader, StreamId,
    StreamLanguage, TlsEntry, TlsPeer, VerifyRequest, VerifyVerdict,
};

pub const SECRET: &[u8] = b"the codec suite's dialback secret, 32+ bytes";

const HOME_A: &str = "home-a.test";
const HOME_B: &str = "home-b.test";

/// home-a.test's initial header to home-b.test, declaring the dialback
/// namespace.
const INITIAL: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' xmlns:db='jabber:server:dialback' \
from='home-a.test' to='home-b.test' version='1.0' xml:lang='en'>";
const STARTTLS: &[u8] = b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>";
const AUTH_EMPTY: &[u8] =
    b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>=</auth>";

/// home-b.test's response header to home-a.test.
const RESPONSE: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' xmlns:db='jabber:server:dialback' \
from='home-b.test' to='home-a.test' id='s2s-2' version='1.0' xml:lang='en'>";
/// home-a.test's response header to home-b.test, on a verification
/// connection home-b.test opened.
const RESPONSE_FROM_A: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' xmlns:db='jabber:server:dialback' \
from='home-a.test' to='home-b.test' id='v-1' version='1.0' xml:lang='en'>";
const FEATURES_STARTTLS: &[u8] = b"<stream:features><starttls \
xmlns='urn:ietf:params:xml:ns:xmpp-tls'><required/></starttls></stream:features>";
const FEATURES_EXTERNAL: &[u8] = b"<stream:features><mechanisms \
xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>EXTERNAL</mechanism></mechanisms>\
</stream:features>";
const FEATURES_DIALBACK: &[u8] = b"<stream:features><dialback \
xmlns='urn:xmpp:features:dialback'><errors/></dialback></stream:features>";
const PROCEED: &[u8] = b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>";

/// Prepended when decoding output that continues an open stream.
const CONTINUATION: &[u8] = b"<stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' xmlns:db='jabber:server:dialback'>";

fn domain(text: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(text).expect("a domain")
}

pub fn keys() -> Arc<DialbackSecret> {
    Arc::new(DialbackSecret::new(SECRET))
}

fn enabled() -> DialbackMode {
    DialbackMode::Enabled(keys() as Arc<dyn DialbackKeys>)
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// home-b.test serves itself, admits home-a.test and permits it dialback
/// when `permits` says so.
struct Admission {
    permits: bool,
}

impl InboundAdmission for Admission {
    fn destination(&self, destination: &CanonicalDomain) -> DestinationStatus {
        if destination.as_str() == HOME_B {
            DestinationStatus::Served
        } else {
            DestinationStatus::Unknown
        }
    }

    fn admits(&self, source: &CanonicalDomain, destination: &CanonicalDomain) -> bool {
        source.as_str() == HOME_A && destination.as_str() == HOME_B
    }

    fn permits_dialback(&self, source: &CanonicalDomain, destination: &CanonicalDomain) -> bool {
        self.permits && self.admits(source, destination)
    }
}

fn receiver(dialback: DialbackMode, permits: bool) -> S2sStream {
    receiver_with_clock(dialback, permits, NegotiationClock::system())
}

fn receiver_with_clock(
    dialback: DialbackMode,
    permits: bool,
    clock: NegotiationClock,
) -> S2sStream {
    let mut stream = S2sStream::accept(
        ReceivingConfig {
            admission: Arc::new(Admission { permits }),
            default_domain: domain(HOME_B),
            language: StreamLanguage::english(),
            sasl: SaslLimits::PILOT,
            dialback,
            clock,
        },
        STREAM_CODEC_LIMITS,
    );
    stream.deliver_stanzas();
    stream
}

/// A clock a test moves forward by hand.
#[derive(Clone)]
struct ManualClock(Arc<Mutex<Instant>>);

impl ManualClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn advance(&self, by: Duration) {
        match self.0.lock() {
            Ok(mut now) => *now += by,
            Err(poisoned) => *poisoned.into_inner() += by,
        }
    }

    fn clock(&self) -> NegotiationClock {
        let now = Arc::clone(&self.0);
        NegotiationClock::new(move || match now.lock() {
            Ok(now) => *now,
            Err(poisoned) => *poisoned.into_inner(),
        })
    }
}

fn initiating(dialback: DialbackMode) -> InitiatingConfig {
    InitiatingConfig {
        source: domain(HOME_A),
        destination: domain(HOME_B),
        language: StreamLanguage::english(),
        dialback,
    }
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

fn assert_stream_error(outputs: &[Output], condition: StreamErrorCondition) {
    assert_eq!(
        close_reason(outputs),
        Some(CloseReason::StreamError(condition)),
        "{outputs:?}"
    );
}

/// The headers and first-level elements in `bytes`, which either open a
/// stream or continue an open one.
fn decode(bytes: &[u8]) -> (Vec<StreamHeader>, Vec<Element>) {
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    let opens = bytes.starts_with(b"<?xml") || bytes.starts_with(b"<stream:stream");
    if !opens {
        decoder.push(CONTINUATION).expect("a continuation header");
    }
    decoder.push(bytes).expect("output within the limits");
    let mut headers = Vec::new();
    let mut elements = Vec::new();
    while let Ok(Some(item)) = decoder.next_item() {
        match item {
            Inbound::Header(header) => headers.push(header),
            Inbound::Element(element) => elements.push(element),
            Inbound::End => break,
        }
    }
    if !opens {
        headers.clear();
    }
    (headers, elements)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn offers_dialback(features: &Element) -> bool {
    features.children().any(|child| {
        child.is("dialback", DIALBACK_FEATURES)
            && child
                .children()
                .any(|errors| errors.is("errors", DIALBACK_FEATURES))
    })
}

fn offers_sasl(features: &Element) -> bool {
    features
        .children()
        .any(|child| child.is("mechanisms", SASL))
}

/// What a receiving stream sent after its post-TLS header.
struct AfterTls {
    stream: S2sStream,
    bytes: Vec<u8>,
    stream_id: StreamId,
    features: Element,
}

/// Negotiate STARTTLS as home-a.test, report `peer` as what TLS proved and
/// send the post-TLS header.
fn after_tls(mut stream: S2sStream, peer: TlsPeer) -> AfterTls {
    stream.receive(INITIAL);
    let out = stream.receive(STARTTLS);
    assert!(out
        .iter()
        .any(|output| matches!(output, Output::StartTls(_))));
    assert!(stream.tls_established(peer).is_empty());
    let out = stream.receive(INITIAL);
    let bytes = transmitted(&out);
    let (headers, elements) = decode(&bytes);
    let stream_id = headers
        .first()
        .and_then(|header| header.id.clone())
        .expect("a response header with an ID");
    let features = elements
        .into_iter()
        .find(|element| element.is("features", STREAM))
        .expect("post-TLS features");
    AfterTls {
        stream,
        bytes,
        stream_id,
        features,
    }
}

fn result_request(from: &str, to: &str, key: &str) -> Vec<u8> {
    format!("<db:result from='{from}' to='{to}'>{key}</db:result>").into_bytes()
}

fn verify_request(from: &str, to: &str, id: &str, key: &str) -> Vec<u8> {
    format!("<db:verify from='{from}' to='{to}' id='{id}'>{key}</db:verify>").into_bytes()
}

fn verify_request_for(stream_id: &str) -> Vec<u8> {
    let key = keys()
        .key(
            &domain(HOME_A),
            &domain(HOME_B),
            &StreamId::parse(stream_id).expect("an ID"),
        )
        .expect("a key");
    verify_request(HOME_A, HOME_B, stream_id, key.as_str())
}

fn dialback_request(outputs: &[Output]) -> DialbackRequest {
    outputs
        .iter()
        .find_map(|output| match output {
            Output::VerifyDialback(request) => Some(request.clone()),
            _ => None,
        })
        .expect("a dialback key to verify")
}

/// The only dialback element in `outputs`' transmitted bytes.
fn dialback_element(outputs: &[Output], name: &str) -> Element {
    let (_, elements) = decode(&transmitted(outputs));
    elements
        .into_iter()
        .find(|element| element.is(name, JABBER_SERVER_DIALBACK))
        .unwrap_or_else(|| panic!("a <db:{name}/> in {outputs:?}"))
}

/// A receiving stream without a certificate whose key is awaiting
/// verification.
fn verifying() -> AfterTls {
    let mut after = after_tls(receiver(enabled(), true), TlsPeer::NoCertificate);
    let out = after
        .stream
        .receive(&result_request(HOME_A, HOME_B, "0123abcd"));
    dialback_request(&out);
    after
}

#[test]
fn without_a_certificate_only_dialback_is_offered_where_permitted() {
    let after = after_tls(receiver(enabled(), true), TlsPeer::NoCertificate);
    assert!(offers_dialback(&after.features));
    assert!(!offers_sasl(&after.features));
    // The response header declares the dialback namespace.
    assert!(contains(&after.bytes, b"jabber:server:dialback"));

    // With a verified certificate, SASL EXTERNAL and dialback.
    let after = after_tls(receiver(enabled(), true), TlsPeer::Verified(domain(HOME_A)));
    assert!(offers_dialback(&after.features));
    assert!(offers_sasl(&after.features));
}

#[test]
fn the_dialback_namespace_is_declared_only_where_a_role_runs() {
    let mut stream = receiver(DialbackMode::Disabled, false);
    let out = stream.receive(INITIAL);
    assert!(!contains(&transmitted(&out), b"jabber:server:dialback"));
    let mut stream = receiver(DialbackMode::Disabled, true);
    let out = stream.receive(INITIAL);
    assert!(contains(&transmitted(&out), b"jabber:server:dialback"));
    let mut stream = receiver(enabled(), false);
    let out = stream.receive(INITIAL);
    assert!(contains(&transmitted(&out), b"jabber:server:dialback"));
}

#[test]
fn a_valid_key_establishes_a_dialback_stream_without_a_restart() {
    let mut after = after_tls(receiver(enabled(), true), TlsPeer::NoCertificate);
    let out = after
        .stream
        .receive(&result_request(HOME_A, HOME_B, "0123abcd"));
    let request = dialback_request(&out);
    assert_eq!(request.source, domain(HOME_A));
    assert_eq!(request.destination, domain(HOME_B));
    assert_eq!(request.stream_id, after.stream_id);
    assert_eq!(request.key, DialbackKey::parse("0123abcd").expect("a key"));
    assert!(
        transmitted(&out).is_empty(),
        "nothing is answered before the verification"
    );

    let out = after.stream.dialback(DialbackOutcome::Valid);
    let result = dialback_element(&out, "result");
    assert_eq!(result.attr("type"), Some("valid"));
    assert_eq!(result.attr("from"), Some(HOME_B));
    assert_eq!(result.attr("to"), Some(HOME_A));
    let established = out.iter().find_map(|output| match output {
        Output::Established(stream) => Some(stream.clone()),
        _ => None,
    });
    let established = established.expect("an established stream");
    assert_eq!(established.authentication, StreamAuthentication::Dialback);
    assert_eq!(established.source, domain(HOME_A));
    assert_eq!(established.destination, domain(HOME_B));

    // The pre-authentication ceiling of 8 KiB no longer applies.
    let body = "x".repeat(20 * 1024);
    let stanza = format!(
        "<message from='alice@home-a.test' to='bob@home-b.test'><body>{body}</body></message>"
    );
    let out = after.stream.receive(stanza.as_bytes());
    assert!(
        out.iter().any(|output| matches!(output, Output::Stanza(_))),
        "{out:?}"
    );
}

#[test]
fn an_invalid_key_is_answered_and_the_stream_closes() {
    let mut after = verifying();
    let out = after.stream.dialback(DialbackOutcome::Invalid);
    assert_eq!(
        dialback_element(&out, "result").attr("type"),
        Some("invalid")
    );
    assert_eq!(close_reason(&out), Some(CloseReason::DialbackRefused));
    assert!(!out
        .iter()
        .any(|output| matches!(output, Output::Established(_))));
}

#[test]
fn a_key_that_could_not_be_verified_is_a_dialback_error() {
    let mut after = verifying();
    let out = after.stream.dialback(DialbackOutcome::Error(
        DialbackError::RemoteConnectionFailed,
    ));
    let result = dialback_element(&out, "result");
    assert_eq!(result.attr("type"), Some("error"));
    let error = result
        .children()
        .find(|child| child.name() == "error")
        .expect("an error child");
    assert_eq!(error.attr("type"), Some("cancel"));
    assert!(error
        .children()
        .any(|condition| condition.is("remote-connection-failed", STANZAS)));
    assert_eq!(close_reason(&out), Some(CloseReason::DialbackFailed));
}

#[test]
fn dialback_where_it_is_not_permitted_is_refused() {
    // The Authoritative role alone: empty features, and a key is refused.
    let mut after = after_tls(receiver(enabled(), false), TlsPeer::NoCertificate);
    assert!(!offers_dialback(&after.features));
    assert!(!offers_sasl(&after.features));
    assert_stream_error(
        &after
            .stream
            .receive(&result_request(HOME_A, HOME_B, "0123abcd")),
        StreamErrorCondition::PolicyViolation,
    );
    // A verified certificate without permission: SASL EXTERNAL only.
    let mut after = after_tls(
        receiver(enabled(), false),
        TlsPeer::Verified(domain(HOME_A)),
    );
    assert!(!offers_dialback(&after.features));
    assert_stream_error(
        &after
            .stream
            .receive(&result_request(HOME_A, HOME_B, "0123abcd")),
        StreamErrorCondition::PolicyViolation,
    );
    // No role at all: no certificate ends the stream at the header.
    let mut stream = receiver(DialbackMode::Disabled, false);
    stream.receive(INITIAL);
    stream.receive(STARTTLS);
    stream.tls_established(TlsPeer::NoCertificate);
    assert_stream_error(
        &stream.receive(INITIAL),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn dialback_is_never_accepted_before_tls() {
    for element in [
        result_request(HOME_A, HOME_B, "0123abcd"),
        verify_request_for("s-1"),
    ] {
        let mut stream = receiver(enabled(), true);
        stream.receive(INITIAL);
        assert_stream_error(
            &stream.receive(&element),
            StreamErrorCondition::PolicyViolation,
        );
    }
}

#[test]
fn one_key_per_stream() {
    let mut after = verifying();
    assert_stream_error(
        &after
            .stream
            .receive(&result_request(HOME_A, HOME_B, "4567abcd")),
        StreamErrorCondition::PolicyViolation,
    );
    // Nor once the stream is established.
    let mut after = verifying();
    after.stream.dialback(DialbackOutcome::Valid);
    assert_stream_error(
        &after
            .stream
            .receive(&result_request(HOME_A, HOME_B, "4567abcd")),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn a_stream_carries_one_domain_pair() {
    let cases = [
        (
            result_request("other.test", HOME_B, "0123abcd"),
            StreamErrorCondition::InvalidFrom,
        ),
        (
            result_request("muc.home-a.test", HOME_B, "0123abcd"),
            StreamErrorCondition::InvalidFrom,
        ),
        (
            result_request(HOME_A, "muc.home-b.test", "0123abcd"),
            StreamErrorCondition::HostUnknown,
        ),
        (
            b"<db:result to='home-b.test'>0123abcd</db:result>".to_vec(),
            StreamErrorCondition::ImproperAddressing,
        ),
        (
            b"<db:result from='home-a.test' to='home-b.test'/>".to_vec(),
            StreamErrorCondition::BadFormat,
        ),
    ];
    for (element, condition) in cases {
        let mut after = after_tls(receiver(enabled(), true), TlsPeer::NoCertificate);
        assert_stream_error(&after.stream.receive(&element), condition);
    }
}

#[test]
fn unsolicited_answers_on_an_inbound_stream_are_refused() {
    let answers: [&[u8]; 4] = [
        b"<db:result from='home-a.test' to='home-b.test' type='valid'/>",
        b"<db:result from='home-a.test' to='home-b.test' type='invalid'/>",
        b"<db:verify from='home-a.test' to='home-b.test' id='s-1' type='valid'/>",
        b"<db:verify from='home-a.test' to='home-b.test' id='s-1' type='invalid'/>",
    ];
    for answer in answers {
        let mut after = after_tls(receiver(enabled(), true), TlsPeer::NoCertificate);
        assert_stream_error(
            &after.stream.receive(answer),
            StreamErrorCondition::PolicyViolation,
        );
    }
    // Another name in the dialback namespace.
    let mut after = after_tls(receiver(enabled(), true), TlsPeer::NoCertificate);
    assert_stream_error(
        &after
            .stream
            .receive(b"<db:other from='home-a.test' to='home-b.test'/>"),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn the_authoritative_server_answers_for_keys_it_issued() {
    let mut after = after_tls(receiver(enabled(), false), TlsPeer::NoCertificate);
    let out = after.stream.receive(&verify_request_for("s-1"));
    let answer = dialback_element(&out, "verify");
    assert_eq!(answer.attr("type"), Some("valid"));
    assert_eq!(answer.attr("from"), Some(HOME_B));
    assert_eq!(answer.attr("to"), Some(HOME_A));
    assert_eq!(answer.attr("id"), Some("s-1"));
    assert!(out
        .iter()
        .any(|output| matches!(output, Output::DialbackAnswered { valid: true })));
    assert_eq!(close_reason(&out), None);

    // A forged key, or the key for another stream ID.
    let forged = verify_request(HOME_A, HOME_B, "s-2", &"0".repeat(64));
    let out = after.stream.receive(&forged);
    assert_eq!(
        dialback_element(&out, "verify").attr("type"),
        Some("invalid")
    );
    assert!(out
        .iter()
        .any(|output| matches!(output, Output::DialbackAnswered { valid: false })));
    let key = keys()
        .key(
            &domain(HOME_A),
            &domain(HOME_B),
            &StreamId::parse("s-3").expect("an ID"),
        )
        .expect("a key");
    let out = after
        .stream
        .receive(&verify_request(HOME_A, HOME_B, "s-4", key.as_str()));
    assert_eq!(
        dialback_element(&out, "verify").attr("type"),
        Some("invalid")
    );

    // Without the deployment's secret no request is answered.
    let mut after = after_tls(
        receiver(DialbackMode::Disabled, true),
        TlsPeer::NoCertificate,
    );
    assert_stream_error(
        &after.stream.receive(&verify_request_for("s-1")),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn verification_requests_are_for_the_streams_pair_and_bounded() {
    let cases = [
        (
            verify_request("other.test", HOME_B, "s-1", "0123abcd"),
            StreamErrorCondition::InvalidFrom,
        ),
        (
            verify_request(HOME_A, "muc.home-b.test", "s-1", "0123abcd"),
            StreamErrorCondition::HostUnknown,
        ),
        (
            b"<db:verify from='home-a.test' to='home-b.test'>0123abcd</db:verify>".to_vec(),
            StreamErrorCondition::BadFormat,
        ),
    ];
    for (element, condition) in cases {
        let mut after = after_tls(receiver(enabled(), false), TlsPeer::NoCertificate);
        assert_stream_error(&after.stream.receive(&element), condition);
    }

    // Eight requests within a minute are answered; a ninth within the same
    // minute ends the stream.
    let clock = ManualClock::new();
    let mut after = after_tls(
        receiver_with_clock(enabled(), false, clock.clock()),
        TlsPeer::NoCertificate,
    );
    for index in 0..8 {
        let out = after
            .stream
            .receive(&verify_request_for(&format!("s-{index}")));
        assert_eq!(close_reason(&out), None, "request {index}");
        clock.advance(Duration::from_secs(5));
    }
    assert_stream_error(
        &after.stream.receive(&verify_request_for("s-8")),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn verification_requests_are_a_rate_not_a_lifetime_cap() {
    let clock = ManualClock::new();
    let mut after = after_tls(
        receiver_with_clock(enabled(), false, clock.clock()),
        TlsPeer::NoCertificate,
    );
    // Three full windows' worth of requests at the rate: all answered.
    for index in 0..24 {
        let out = after
            .stream
            .receive(&verify_request_for(&format!("s-{index}")));
        assert_eq!(
            dialback_element(&out, "verify").attr("type"),
            Some("valid"),
            "request {index}"
        );
        assert_eq!(close_reason(&out), None, "request {index}");
        clock.advance(Duration::from_millis(7_500));
    }
    // Eight at once, a minute after the last: answered, and the window is
    // full again.
    clock.advance(Duration::from_secs(60));
    for index in 24..32 {
        let out = after
            .stream
            .receive(&verify_request_for(&format!("s-{index}")));
        assert_eq!(close_reason(&out), None, "request {index}");
    }
    assert_stream_error(
        &after.stream.receive(&verify_request_for("s-32")),
        StreamErrorCondition::PolicyViolation,
    );
}

#[test]
fn an_established_sasl_stream_still_answers_verification_requests() {
    let mut after = after_tls(
        receiver(enabled(), false),
        TlsPeer::Verified(domain(HOME_A)),
    );
    let out = after.stream.receive(AUTH_EMPTY);
    let request = out
        .iter()
        .find_map(|output| match output {
            Output::Authorize(request) => Some(request.clone()),
            _ => None,
        })
        .expect("an authorization request");
    after
        .stream
        .authorization(request.exchange, AuthorizationDecision::Granted);
    let out = after.stream.receive(INITIAL);
    assert!(out.iter().any(|output| matches!(
        output,
        Output::Established(stream) if stream.authentication == StreamAuthentication::SaslExternal
    )));
    let out = after.stream.receive(&verify_request_for("s-9"));
    assert_eq!(dialback_element(&out, "verify").attr("type"), Some("valid"));
    assert_eq!(close_reason(&out), None);
}

/// An initiating stream after STARTTLS, with home-b.test's certificate
/// verified.
fn initiator_after_tls(dialback: DialbackMode) -> (S2sStream, Vec<u8>) {
    let (mut stream, initial) = S2sStream::initiate(initiating(dialback), STREAM_CODEC_LIMITS);
    let opening = transmitted(&initial);
    stream.receive(&concat(&[RESPONSE, FEATURES_STARTTLS]));
    let out = stream.receive(PROCEED);
    assert!(out
        .iter()
        .any(|output| matches!(output, Output::StartTls(_))));
    let out = stream.tls_established(TlsPeer::Verified(domain(HOME_B)));
    assert!(!transmitted(&out).is_empty(), "the post-TLS header");
    (stream, opening)
}

/// An initiating stream that has sent its dialback key.
fn initiator_awaiting_result() -> S2sStream {
    let (mut stream, _) = initiator_after_tls(enabled());
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_DIALBACK]));
    dialback_element(&out, "result");
    stream
}

#[test]
fn the_initiating_server_sends_its_key_when_external_is_not_offered() {
    let (mut stream, opening) = initiator_after_tls(enabled());
    assert!(contains(&opening, b"jabber:server:dialback"));
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_DIALBACK]));
    let result = dialback_element(&out, "result");
    assert_eq!(result.attr("from"), Some(HOME_A));
    assert_eq!(result.attr("to"), Some(HOME_B));
    assert_eq!(result.attr("type"), None);
    let expected = keys()
        .key(
            &domain(HOME_B),
            &domain(HOME_A),
            &StreamId::parse("s2s-2").expect("an ID"),
        )
        .expect("a key");
    assert_eq!(result.text(), expected.as_str());

    let out = stream.receive(b"<db:result from='home-b.test' to='home-a.test' type='valid'/>");
    let established = out.iter().find_map(|output| match output {
        Output::Established(stream) => Some(stream.clone()),
        _ => None,
    });
    let established = established.expect("an established stream");
    assert_eq!(established.authentication, StreamAuthentication::Dialback);
    assert_eq!(established.source, domain(HOME_A));
    assert_eq!(established.destination, domain(HOME_B));
}

#[test]
fn the_initiating_server_stops_when_its_key_is_refused() {
    let refusals: [(&[u8], CloseReason); 2] = [
        (
            b"<db:result from='home-b.test' to='home-a.test' type='invalid'/>",
            CloseReason::DialbackRefused,
        ),
        (
            b"<db:result from='home-b.test' to='home-a.test' type='error'><error type='cancel'>\
<remote-connection-failed xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></db:result>",
            CloseReason::DialbackFailed,
        ),
    ];
    for (refusal, reason) in refusals {
        let mut stream = initiator_awaiting_result();
        let out = stream.receive(refusal);
        assert_eq!(close_reason(&out), Some(reason));
    }
}

/// home-b.test's response header to home-a.test without the dialback
/// namespace.
const RESPONSE_WITHOUT_DIALBACK: &[u8] = b"<?xml version='1.0'?><stream:stream \
xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' \
from='home-b.test' to='home-a.test' id='s2s-2' version='1.0' xml:lang='en'>";

const FEATURES_NONE: &[u8] = b"<stream:features/>";

#[test]
fn the_initiating_server_sends_its_key_only_where_dialback_is_advertised() {
    // Neither the header's namespace nor the feature: no key is sent.
    let (mut stream, _) = initiator_after_tls(enabled());
    let out = stream.receive(&concat(&[RESPONSE_WITHOUT_DIALBACK, FEATURES_NONE]));
    let (_, elements) = decode(&transmitted(&out));
    assert!(!elements
        .iter()
        .any(|element| element.ns() == JABBER_SERVER_DIALBACK));
    assert_stream_error(&out, StreamErrorCondition::PolicyViolation);

    // The namespace on the post-TLS header advertises traditional dialback
    // (XEP-0220 §2.4.1).
    let (mut stream, _) = initiator_after_tls(enabled());
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_NONE]));
    assert_eq!(dialback_element(&out, "result").attr("to"), Some(HOME_B));

    // The feature advertises dialback with error handling (§2.4.2).
    let (mut stream, _) = initiator_after_tls(enabled());
    let out = stream.receive(&concat(&[RESPONSE_WITHOUT_DIALBACK, FEATURES_DIALBACK]));
    assert_eq!(dialback_element(&out, "result").attr("to"), Some(HOME_B));
}

#[test]
fn the_initiating_server_refuses_unsolicited_and_uncorrelated_answers() {
    // A result before any key was sent.
    let (mut stream, _) = initiator_after_tls(enabled());
    stream.receive(RESPONSE);
    assert_stream_error(
        &stream.receive(b"<db:result from='home-b.test' to='home-a.test' type='valid'/>"),
        StreamErrorCondition::PolicyViolation,
    );
    // A result for another pair, a request, a verification answer.
    let uncorrelated: [&[u8]; 4] = [
        b"<db:result from='muc.home-b.test' to='home-a.test' type='valid'/>",
        b"<db:result from='home-b.test' to='other.test' type='valid'/>",
        b"<db:result from='home-b.test' to='home-a.test'>0123abcd</db:result>",
        b"<db:verify from='home-b.test' to='home-a.test' id='s2s-2' type='valid'/>",
    ];
    for answer in uncorrelated {
        let mut stream = initiator_awaiting_result();
        assert_stream_error(
            &stream.receive(answer),
            StreamErrorCondition::PolicyViolation,
        );
    }
}

#[test]
fn the_initiating_server_prefers_external_when_it_is_offered() {
    let (mut stream, _) = initiator_after_tls(enabled());
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    let (_, elements) = decode(&transmitted(&out));
    assert!(elements.iter().any(|element| element.is("auth", SASL)));
    assert!(!elements
        .iter()
        .any(|element| element.ns() == JABBER_SERVER_DIALBACK));
}

fn verification(entry: TlsEntry) -> (S2sStream, Vec<Output>) {
    S2sStream::verify(
        InitiatingConfig {
            source: domain(HOME_B),
            destination: domain(HOME_A),
            language: StreamLanguage::english(),
            dialback: DialbackMode::Disabled,
        },
        VerifyRequest {
            stream_id: StreamId::parse("in-7").expect("an ID"),
            key: DialbackKey::parse("0123abcd").expect("a key"),
        },
        entry,
        STREAM_CODEC_LIMITS,
    )
}

/// A verification connection that has sent its request.
fn verification_awaiting_answer() -> S2sStream {
    let (mut stream, initial) = verification(TlsEntry::StartTls);
    assert!(contains(&transmitted(&initial), b"jabber:server:dialback"));
    stream.receive(&concat(&[RESPONSE_FROM_A, FEATURES_STARTTLS]));
    stream.receive(PROCEED);
    stream.tls_established(TlsPeer::Verified(domain(HOME_A)));
    // The request goes out whatever is offered; SASL is never attempted.
    let out = stream.receive(&concat(&[RESPONSE_FROM_A, FEATURES_EXTERNAL]));
    let request = dialback_element(&out, "verify");
    assert_eq!(request.attr("from"), Some(HOME_B));
    assert_eq!(request.attr("to"), Some(HOME_A));
    assert_eq!(request.attr("id"), Some("in-7"));
    assert_eq!(request.text(), "0123abcd");
    let (_, elements) = decode(&transmitted(&out));
    assert!(!elements.iter().any(|element| element.is("auth", SASL)));
    stream
}

#[test]
fn a_verification_connection_reports_the_correlated_answer_and_closes() {
    for (answer, verdict) in [
        (
            &b"<db:verify from='home-a.test' to='home-b.test' id='in-7' type='valid'/>"[..],
            VerifyVerdict::Valid,
        ),
        (
            &b"<db:verify from='home-a.test' to='home-b.test' id='in-7' type='invalid'/>"[..],
            VerifyVerdict::Invalid,
        ),
    ] {
        let mut stream = verification_awaiting_answer();
        let out = stream.receive(answer);
        assert!(out
            .iter()
            .any(|output| matches!(output, Output::VerifyVerdict(found) if *found == verdict)));
        assert_eq!(close_reason(&out), Some(CloseReason::VerificationComplete));
    }
}

#[test]
fn a_verification_connection_refuses_uncorrelated_answers() {
    let uncorrelated: [&[u8]; 4] = [
        b"<db:verify from='home-a.test' to='home-b.test' id='in-8' type='valid'/>",
        b"<db:verify from='other.test' to='home-b.test' id='in-7' type='valid'/>",
        b"<db:verify from='home-a.test' to='home-b.test' type='valid'/>",
        b"<db:result from='home-a.test' to='home-b.test' type='valid'/>",
    ];
    for answer in uncorrelated {
        let mut stream = verification_awaiting_answer();
        let out = stream.receive(answer);
        assert_stream_error(&out, StreamErrorCondition::PolicyViolation);
        assert!(!out
            .iter()
            .any(|output| matches!(output, Output::VerifyVerdict(_))));
    }
}

#[test]
fn direct_tls_starts_with_the_handshake_and_never_negotiates_starttls() {
    let (mut stream, initial) = S2sStream::initiate_with(
        initiating(DialbackMode::Disabled),
        TlsEntry::DirectTls,
        STREAM_CODEC_LIMITS,
    );
    assert_eq!(initial.len(), 1);
    match &initial[0] {
        Output::StartTls(handoff) => assert!(handoff.is_empty()),
        other => panic!("expected TLS first, got {other:?}"),
    }
    let out = stream.tls_established(TlsPeer::Verified(domain(HOME_B)));
    let (headers, _) = decode(&transmitted(&out));
    assert_eq!(headers.len(), 1, "the first header follows the handshake");
    // STARTTLS offered inside TLS is refused.
    assert_stream_error(
        &stream.receive(&concat(&[RESPONSE, FEATURES_STARTTLS])),
        StreamErrorCondition::PolicyViolation,
    );

    // A direct-TLS verification connection starts the same way.
    let (_, initial) = verification(TlsEntry::DirectTls);
    assert!(matches!(initial.as_slice(), [Output::StartTls(handoff)] if handoff.is_empty()));

    // And the successful direct-TLS path reaches SASL EXTERNAL.
    let (mut stream, _) = S2sStream::initiate_with(
        initiating(DialbackMode::Disabled),
        TlsEntry::DirectTls,
        STREAM_CODEC_LIMITS,
    );
    stream.tls_established(TlsPeer::Verified(domain(HOME_B)));
    let out = stream.receive(&concat(&[RESPONSE, FEATURES_EXTERNAL]));
    let (_, elements) = decode(&transmitted(&out));
    assert!(elements.iter().any(|element| element.is("auth", SASL)));
}
