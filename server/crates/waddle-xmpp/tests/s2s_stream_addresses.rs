//! Stream header addresses in the identity profile's canonical domain
//! form, in both roles: a U-label and an A-label spelling are one domain,
//! labels made of code points newer than Unicode 3.2 are read without JID
//! preparation, ASCII case is mapped, any other spelling the profile
//! refuses closes the stream, and emitted headers use U-labels (RFC 7622
//! §3.2.1). Also: the initiating role reads a
//! peer's stream error that follows an unusable response header, reads
//! only the STARTTLS and EXTERNAL offers of a features element, and a
//! header can declare the dialback namespace.

use std::sync::Arc;

use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{
    AuthorizationDecision, CloseReason, CodecLimits, DestinationStatus, DialbackMode,
    HeaderAddress, Inbound, InboundAdmission, InitiatingConfig, NegotiationPhase, Output,
    ReceivingConfig, S2sStream, SaslLimits, StreamDecoder, StreamEncoder, StreamErrorCondition,
    StreamHeader, StreamLanguage, TlsPeer,
};

/// `bücher.test` in A-label form.
const A_LABEL: &str = "xn--bcher-kva.test";
const U_LABEL: &str = "bücher.test";

/// A domain whose label is three Tifinagh letters (U+2D30, U+2D31,
/// U+2D33), assigned in Unicode 4.1. IDNA2008 permits them; Nameprep's
/// Unicode 3.2 tables treat them as unassigned.
const TIFINAGH_U_LABEL: &str = "\u{2d30}\u{2d31}\u{2d33}.test";
const TIFINAGH_A_LABEL: &str = "xn--4ljcg.test";

/// A spelling of `home-b.test` with a fullwidth `h` (U+FF48), which IDNA
/// would map to `h` and the profile refuses instead.
const FULLWIDTH_HOME_B: &str = "\u{ff48}ome-b.test";

fn domain(value: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(value).expect("valid domain")
}

/// Serves `home-b.test` and admits only `bücher.test` and the Tifinagh
/// domain.
struct Admission;

impl InboundAdmission for Admission {
    fn destination(&self, destination: &CanonicalDomain) -> DestinationStatus {
        if *destination == domain("home-b.test") {
            DestinationStatus::Served
        } else {
            DestinationStatus::Unknown
        }
    }

    fn admits(&self, source: &CanonicalDomain, _destination: &CanonicalDomain) -> bool {
        *source == domain(A_LABEL) || *source == domain(TIFINAGH_A_LABEL)
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

fn header(from: &str, to: &str) -> Vec<u8> {
    [
        "<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' from='",
        from,
        "' to='",
        to,
        "' version='1.0'>",
    ]
    .concat()
    .into_bytes()
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

fn first_header(bytes: &[u8]) -> StreamHeader {
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    decoder.push(bytes).expect("push");
    match decoder.next_item() {
        Ok(Some(Inbound::Header(header))) => header,
        other => panic!("expected a header, got {other:?}"),
    }
}

#[test]
fn header_addresses_are_parsed_from_the_raw_text_into_canonical_form() {
    assert_eq!(
        HeaderAddress::parse(Some(U_LABEL)),
        HeaderAddress::Domain(domain(A_LABEL))
    );
    assert_eq!(
        HeaderAddress::parse(Some(A_LABEL)),
        HeaderAddress::Domain(domain(A_LABEL))
    );
    assert_eq!(
        HeaderAddress::parse(Some("home-b.test.")),
        HeaderAddress::Domain(domain("home-b.test"))
    );
    // The raw text is read, so `ß` is not folded to `ss` as JID
    // preparation would fold it.
    assert_eq!(
        HeaderAddress::parse(Some("faß.test")),
        HeaderAddress::Domain(domain("faß.test"))
    );
    assert_ne!(domain("faß.test"), domain("fass.test"));
    // ASCII case is mapped: DNS does not distinguish it.
    for spelling in ["Home-B.test", "HOME-B.TEST"] {
        assert_eq!(
            HeaderAddress::parse(Some(spelling)),
            HeaderAddress::Domain(domain("home-b.test")),
            "{spelling}"
        );
    }
    assert_eq!(
        HeaderAddress::parse(Some("XN--BCHER-KVA.test")),
        HeaderAddress::Domain(domain(A_LABEL))
    );
    // Every other spelling the profile would have to map is refused, not
    // folded: width, compatibility and non-ASCII case.
    for refused in [FULLWIDTH_HOME_B, "BÜCHER.test", "\u{fb00}.test"] {
        assert_eq!(
            HeaderAddress::parse(Some(refused)),
            HeaderAddress::Refused,
            "{refused}"
        );
    }
    // Labels outside Unicode 3.2 are read without JID preparation.
    assert_eq!(
        HeaderAddress::parse(Some(TIFINAGH_U_LABEL)),
        HeaderAddress::Domain(domain(TIFINAGH_A_LABEL))
    );
    assert_eq!(
        HeaderAddress::parse(Some(TIFINAGH_A_LABEL)),
        HeaderAddress::Domain(domain(TIFINAGH_A_LABEL))
    );
    for invalid in [
        "alice@home-b.test",
        "home-b.test/resource",
        "\u{2d30}@home-b.test",
        "home b.test",
    ] {
        assert_eq!(
            HeaderAddress::parse(Some(invalid)),
            HeaderAddress::Invalid,
            "{invalid}"
        );
    }
    assert_eq!(HeaderAddress::parse(Some("")), HeaderAddress::Invalid);
    assert_eq!(HeaderAddress::parse(None), HeaderAddress::Absent);
}

#[test]
fn a_receiving_stream_admits_both_spellings_of_one_domain_and_answers_in_u_labels() {
    for from in [U_LABEL, A_LABEL] {
        let mut stream = receiver();
        let out = stream.receive(&header(from, "home-b.test"));
        assert_eq!(close_reason(&out), None, "{from}");
        let response = first_header(&transmitted(&out));
        assert_eq!(response.to, HeaderAddress::Domain(domain(A_LABEL)));
        let bytes = String::from_utf8(transmitted(&out)).expect("UTF-8");
        assert!(
            bytes.contains(U_LABEL),
            "the response names the peer in U-labels"
        );
        assert!(!bytes.contains(A_LABEL));
    }
}

#[test]
fn a_receiving_stream_compares_the_verified_peer_in_canonical_form() {
    let mut stream = receiver();
    stream.receive(&header(U_LABEL, "home-b.test"));
    stream.receive(b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>");
    stream.tls_established(TlsPeer::Verified(domain(A_LABEL)));
    let out = stream.receive(&header(A_LABEL, "home-b.test"));
    assert_eq!(close_reason(&out), None);
    assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitAuth);
    let out = stream
        .receive(b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>=</auth>");
    let request = out
        .iter()
        .find_map(|output| match output {
            Output::Authorize(request) => Some(request.clone()),
            _ => None,
        })
        .expect("authorization requested");
    assert_eq!(request.source, domain(A_LABEL));
    stream.authorization(request.exchange, AuthorizationDecision::Granted);
}

#[test]
fn a_refused_spelling_closes_the_receiving_stream() {
    let cases = [
        (
            header("BÜCHER.test", "home-b.test"),
            StreamErrorCondition::InvalidFrom,
        ),
        (
            header(A_LABEL, FULLWIDTH_HOME_B),
            StreamErrorCondition::HostUnknown,
        ),
    ];
    for (bytes, condition) in cases {
        let mut stream = receiver();
        assert_eq!(
            close_reason(&stream.receive(&bytes)),
            Some(CloseReason::StreamError(condition))
        );
    }
}

#[test]
fn a_pair_admission_refuses_receives_policy_violation() {
    let mut stream = receiver();
    assert_eq!(
        close_reason(&stream.receive(&header("stranger.test", "home-b.test"))),
        Some(CloseReason::StreamError(
            StreamErrorCondition::PolicyViolation
        ))
    );
}

#[test]
fn ascii_case_in_a_received_header_is_mapped() {
    let mut stream = receiver();
    let out = stream.receive(&header("XN--BCHER-KVA.TEST", "Home-B.test"));
    assert_eq!(close_reason(&out), None);
    let response = first_header(&transmitted(&out));
    assert_eq!(response.from, HeaderAddress::Domain(domain("home-b.test")));
    assert_eq!(response.to, HeaderAddress::Domain(domain(A_LABEL)));
}

#[test]
fn a_receiving_stream_accepts_a_label_outside_unicode_3_2_through_sasl() {
    for from in [TIFINAGH_U_LABEL, TIFINAGH_A_LABEL] {
        let mut stream = receiver();
        let out = stream.receive(&header(from, "home-b.test"));
        assert_eq!(close_reason(&out), None, "{from}");
        let sent = String::from_utf8(transmitted(&out)).expect("UTF-8");
        assert!(sent.contains(TIFINAGH_U_LABEL), "answered in U-labels");
        stream.receive(b"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>");
        stream.tls_established(TlsPeer::Verified(domain(TIFINAGH_A_LABEL)));
        let out = stream.receive(&header(TIFINAGH_U_LABEL, "home-b.test"));
        assert_eq!(close_reason(&out), None, "{from}");
        assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitAuth);
        // An authorization identity naming the verified source in U-label
        // form, base64 of the UTF-8 text, is tolerated.
        let out = stream.receive(
            b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='EXTERNAL'>\
4rSw4rSx4rSzLnRlc3Q=</auth>",
        );
        assert_eq!(close_reason(&out), None, "{from}");
        let request = out
            .iter()
            .find_map(|output| match output {
                Output::Authorize(request) => Some(request.clone()),
                _ => None,
            })
            .expect("authorization requested");
        assert_eq!(request.source, domain(TIFINAGH_A_LABEL));
    }
}

#[test]
fn an_initiating_stream_reaches_a_label_outside_unicode_3_2() {
    let config = InitiatingConfig {
        source: domain("home-b.test"),
        destination: domain(TIFINAGH_U_LABEL),
        language: StreamLanguage::english(),
        dialback: DialbackMode::Disabled,
    };
    let (_, out) = S2sStream::initiate(config.clone(), CodecLimits::PILOT);
    let sent = String::from_utf8(transmitted(&out)).expect("UTF-8");
    assert!(sent.contains(TIFINAGH_U_LABEL), "addressed in U-labels");

    for from in [TIFINAGH_U_LABEL, TIFINAGH_A_LABEL] {
        let (mut stream, _) = S2sStream::initiate(config.clone(), CodecLimits::PILOT);
        let mut input = response(from, "home-b.test");
        input.extend_from_slice(
            b"<stream:features><starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/></stream:features>",
        );
        let out = stream.receive(&input);
        assert_eq!(close_reason(&out), None, "{from}");
        assert_eq!(stream.phase(), NegotiationPhase::PreTls);
    }
}

fn initiator() -> S2sStream {
    S2sStream::initiate(
        InitiatingConfig {
            source: domain("home-b.test"),
            destination: domain(A_LABEL),
            language: StreamLanguage::english(),
            dialback: DialbackMode::Disabled,
        },
        CodecLimits::PILOT,
    )
    .0
}

fn response(from: &str, to: &str) -> Vec<u8> {
    [
        "<stream:stream xmlns='jabber:server' xmlns:stream='http://etherx.jabber.org/streams' from='",
        from,
        "' to='",
        to,
        "' id='r1' version='1.0'>",
    ]
    .concat()
    .into_bytes()
}

#[test]
fn an_initiating_stream_addresses_its_peer_in_u_labels_and_accepts_both_spellings() {
    let (_, out) = S2sStream::initiate(
        InitiatingConfig {
            source: domain("home-b.test"),
            destination: domain(A_LABEL),
            language: StreamLanguage::english(),
            dialback: DialbackMode::Disabled,
        },
        CodecLimits::PILOT,
    );
    let bytes = String::from_utf8(transmitted(&out)).expect("UTF-8");
    assert!(bytes.contains(U_LABEL));

    for from in [U_LABEL, A_LABEL] {
        let mut stream = initiator();
        let mut input = response(from, "home-b.test");
        input.extend_from_slice(
            b"<stream:features><starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/></stream:features>",
        );
        let out = stream.receive(&input);
        assert_eq!(close_reason(&out), None, "{from}");
        assert_eq!(stream.phase(), NegotiationPhase::PreTls);
    }
}

#[test]
fn an_initiating_stream_refuses_a_response_spelling_the_profile_refuses() {
    let mut stream = initiator();
    assert_eq!(
        close_reason(&stream.receive(&response("BÜCHER.test", "home-b.test"))),
        None
    );
    let out = stream.receive(b"<stream:features/>");
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(StreamErrorCondition::InvalidFrom))
    );
}

#[test]
fn an_initiating_stream_refuses_a_response_to_spelling_the_profile_refuses() {
    let mut stream = initiator();
    assert_eq!(
        close_reason(&stream.receive(&response(A_LABEL, FULLWIDTH_HOME_B))),
        None
    );
    let out = stream.receive(b"<stream:features/>");
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(StreamErrorCondition::HostUnknown))
    );
}

#[test]
fn a_peer_stream_error_after_an_empty_response_from_is_read_not_answered() {
    let mut stream = initiator();
    let mut input = b"<stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' from='' id='e1' version='1.0'>"
        .to_vec();
    input.extend_from_slice(
        b"<stream:error><host-unknown xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>\
</stream:error>",
    );
    let out = stream.receive(&input);
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::PeerStreamError(
            StreamErrorCondition::HostUnknown
        ))
    );
    let sent = String::from_utf8(transmitted(&out)).expect("UTF-8");
    assert!(!sent.contains("bad-format"), "no error is sent back");
}

#[test]
fn unrelated_feature_children_are_ignored() {
    let mut stream = initiator();
    let mut input = response(A_LABEL, "home-b.test");
    input.extend_from_slice(
        b"<stream:features><starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>\
<dialback xmlns='urn:xmpp:features:dialback'><errors/></dialback>\
<bidi xmlns='urn:xmpp:features:bidi'/><unknown/></stream:features>",
    );
    let out = stream.receive(&input);
    assert_eq!(close_reason(&out), None);
    stream.receive(b"<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>");
    stream.tls_established(TlsPeer::Verified(domain(A_LABEL)));
    let mut input = response(U_LABEL, "home-b.test");
    input.extend_from_slice(
        b"<stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>\
<mechanism>EXTERNAL</mechanism><mechanism>SCRAM-SHA-1</mechanism></mechanisms>\
<dialback xmlns='urn:xmpp:features:dialback'/><x:odd xmlns:x='urn:example:odd' a='1'>text</x:odd>\
</stream:features>",
    );
    let out = stream.receive(&input);
    assert_eq!(close_reason(&out), None);
    assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitOutcome);
}

#[test]
fn an_emitted_header_can_declare_the_dialback_namespace() {
    let mut header = StreamHeader::initial(
        domain("home-b.test"),
        domain(A_LABEL),
        StreamLanguage::english(),
    );
    let (_, plain) = StreamEncoder::open(&header).expect("encode");
    let plain = String::from_utf8(plain).expect("UTF-8");
    assert!(!plain.contains("jabber:server:dialback"));

    header.declare_dialback = true;
    let (_, declared) = StreamEncoder::open(&header).expect("encode");
    let declared = String::from_utf8(declared).expect("UTF-8");
    assert!(declared.contains("xmlns:db"));
    assert!(declared.contains("jabber:server:dialback"));
    // The decoder accepts the declaration on a received header.
    first_header(declared.as_bytes());
}
