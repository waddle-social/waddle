//! XEP-0368 direct TLS in the initiating role of the S2S negotiation: the
//! TLS handshake completes before any XML, so the stream opens on the
//! protected connection, never requests STARTTLS and continues with SASL
//! EXTERNAL exactly as after STARTTLS. Discovery of `_xmpps-server` targets
//! is not part of the codec; the connection owner reports the verified
//! receiving server as it would after STARTTLS.

use std::str::FromStr;

use jid::DomainPart;
use waddle_xmpp::s2s::{
    CloseReason, CodecLimits, DialbackMode, InitiatingConfig, NegotiationPhase, Output, S2sStream,
    StreamErrorCondition, StreamLanguage, TlsPeer,
};

const RESPONSE: &[u8] = b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' from='home-b.test' to='home-a.test' \
id='s2s-1' version='1.0' xml:lang='en'>";
const FEATURES_EXTERNAL: &[u8] = b"<stream:features><mechanisms \
xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>EXTERNAL</mechanism></mechanisms>\
</stream:features>";
const FEATURES_STARTTLS_AND_EXTERNAL: &[u8] = b"<stream:features><starttls \
xmlns='urn:ietf:params:xml:ns:xmpp-tls'/><mechanisms \
xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>EXTERNAL</mechanism></mechanisms>\
</stream:features>";
const SUCCESS: &[u8] = b"<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>";
const FEATURES_EMPTY: &[u8] = b"<stream:features/>";

fn domain(value: &str) -> DomainPart {
    DomainPart::from_str(value).expect("valid domain")
}

fn config() -> InitiatingConfig {
    InitiatingConfig {
        source: domain("home-a.test"),
        destination: domain("home-b.test"),
        language: StreamLanguage::english(),
        dialback: DialbackMode::Disabled,
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_direct_tls_stream_opens_on_the_protected_connection() {
    let (stream, out) = S2sStream::initiate_after_tls(
        config(),
        TlsPeer::Verified(domain("home-b.test")),
        CodecLimits::PILOT,
    );
    let bytes = transmitted(&out);
    assert!(contains(&bytes, b"stream:stream"));
    assert!(contains(&bytes, b"to='home-b.test'") || contains(&bytes, b"to=\"home-b.test\""));
    assert_eq!(stream.phase(), NegotiationPhase::TlsStream);
    assert_eq!(close_reason(&out), None);
}

#[test]
fn a_direct_tls_stream_authenticates_with_external_and_never_requests_starttls() {
    let (mut stream, out) = S2sStream::initiate_after_tls(
        config(),
        TlsPeer::Verified(domain("home-b.test")),
        CodecLimits::PILOT,
    );
    assert!(!contains(&transmitted(&out), b"starttls"));

    let out = stream.receive(&[RESPONSE, FEATURES_EXTERNAL].concat());
    let sent = transmitted(&out);
    assert!(contains(&sent, b"mechanism"));
    assert!(contains(&sent, b"EXTERNAL"));
    assert!(!contains(&sent, b"starttls"));
    assert_eq!(stream.phase(), NegotiationPhase::SaslAwaitOutcome);

    stream.receive(SUCCESS);
    assert_eq!(stream.phase(), NegotiationPhase::AuthenticatedRestart);
    let out = stream.receive(&[RESPONSE, FEATURES_EMPTY].concat());
    assert!(out
        .iter()
        .any(|output| matches!(output, Output::Established(_))));
}

#[test]
fn features_offering_starttls_on_a_direct_tls_stream_are_refused() {
    let (mut stream, _) = S2sStream::initiate_after_tls(
        config(),
        TlsPeer::Verified(domain("home-b.test")),
        CodecLimits::PILOT,
    );
    let out = stream.receive(&[RESPONSE, FEATURES_STARTTLS_AND_EXTERNAL].concat());
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::StreamError(
            StreamErrorCondition::PolicyViolation
        ))
    );
}

#[test]
fn a_direct_tls_stream_never_opens_without_a_verified_receiving_server() {
    for peer in [
        TlsPeer::NoCertificate,
        TlsPeer::Verified(domain("other.test")),
    ] {
        let (stream, out) = S2sStream::initiate_after_tls(config(), peer, CodecLimits::PILOT);
        assert!(transmitted(&out).is_empty(), "no XML is sent");
        assert!(matches!(
            close_reason(&out),
            Some(CloseReason::StreamError(_))
        ));
        assert!(stream.is_closed());
    }
}

#[test]
fn a_redirect_ends_the_direct_tls_attempt() {
    let (mut stream, _) = S2sStream::initiate_after_tls(
        config(),
        TlsPeer::Verified(domain("home-b.test")),
        CodecLimits::PILOT,
    );
    let error = b"<stream:error><see-other-host \
xmlns='urn:ietf:params:xml:ns:xmpp-streams'>[2001:db8::1]:5269</see-other-host>\
</stream:error>";
    let out = stream.receive(&[RESPONSE, error.as_slice()].concat());
    assert_eq!(
        close_reason(&out),
        Some(CloseReason::PeerStreamError(
            StreamErrorCondition::SeeOtherHost
        ))
    );
    assert!(transmitted(&out).iter().all(|byte| *byte != b'['));
}
