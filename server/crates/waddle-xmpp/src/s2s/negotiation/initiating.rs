//! Initiating-role negotiation: STARTTLS, SASL EXTERNAL, authenticated
//! restart.

use minidom::Element;

use crate::ns::{SASL, TLS};
use crate::s2s::condition::StreamErrorCondition;
use crate::s2s::decoder::DecodeError;
use crate::s2s::header::{HeaderAddress, StreamHeader, StreamVersion};

use super::classify::{classify, SaslPayload, TopLevel};
use super::elements::{self, EXTERNAL};
use super::types::{
    CloseReason, DialbackMode, InitiatingConfig, NegotiatedStream, NegotiationInput,
    NegotiationPhase, Role, Step, TlsPeer,
};

/// SASL success data accepted from the receiving server. EXTERNAL defines
/// none, so anything beyond `=` is refused.
const MAX_SUCCESS_DATA_BYTES: usize = 0;

/// Which response header and features the initiating role awaits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    PreTls,
    Tls,
    Authenticated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitHeader(Stage),
    /// The response header failed its checks with this condition. A peer
    /// that cannot serve the stream often answers with a header carrying
    /// an empty or missing address followed by its own stream error (RFC
    /// 6120 §4.9.1.3), so the next element decides: a stream error from the
    /// peer closes with that error, anything else with this condition.
    AwaitPeerError(Stage, StreamErrorCondition),
    AwaitFeatures(Stage),
    AwaitProceed,
    TlsHandshake,
    AwaitSaslOutcome,
    Established,
    Closed,
}

#[derive(Debug)]
pub(crate) struct Initiating {
    config: InitiatingConfig,
    state: State,
}

impl Initiating {
    /// Start a stream; the returned steps open the initial stream header.
    pub(crate) fn start(config: InitiatingConfig) -> (Self, Vec<Step>) {
        let open = Step::Open(initial_header(&config));
        (
            Self {
                config,
                state: State::AwaitHeader(Stage::PreTls),
            },
            vec![open],
        )
    }

    pub(crate) fn phase(&self) -> NegotiationPhase {
        match self.state {
            State::AwaitHeader(Stage::PreTls)
            | State::AwaitPeerError(Stage::PreTls, _)
            | State::AwaitFeatures(Stage::PreTls)
            | State::AwaitProceed => NegotiationPhase::PreTls,
            State::TlsHandshake => NegotiationPhase::TlsHandshake,
            State::AwaitHeader(Stage::Tls)
            | State::AwaitPeerError(Stage::Tls, _)
            | State::AwaitFeatures(Stage::Tls) => NegotiationPhase::TlsStream,
            State::AwaitSaslOutcome => NegotiationPhase::SaslAwaitOutcome,
            State::AwaitHeader(Stage::Authenticated)
            | State::AwaitPeerError(Stage::Authenticated, _)
            | State::AwaitFeatures(Stage::Authenticated) => NegotiationPhase::AuthenticatedRestart,
            State::Established => NegotiationPhase::Established,
            State::Closed => NegotiationPhase::Closed,
        }
    }

    pub(crate) fn handle(&mut self, input: NegotiationInput) -> Vec<Step> {
        let mut steps = Vec::new();
        if self.state == State::Closed {
            return steps;
        }
        match input {
            NegotiationInput::Header(header) => self.on_header(&header, &mut steps),
            NegotiationInput::Element(element) | NegotiationInput::PipelinedElement(element) => {
                self.on_element(element, &mut steps)
            }
            NegotiationInput::PeerEnd => self.close(&mut steps, CloseReason::PeerClosed),
            NegotiationInput::DecodeFailed(DecodeError::NotXml) => {
                self.close(&mut steps, CloseReason::NotXml)
            }
            NegotiationInput::DecodeFailed(error) => self.fail(&mut steps, error.condition()),
            NegotiationInput::PrematureInput => {
                self.fail(&mut steps, StreamErrorCondition::PolicyViolation)
            }
            NegotiationInput::TlsEstablished(peer) => self.on_tls(peer, &mut steps),
            // The initiating role never requests an authorization decision.
            NegotiationInput::Authorization { .. } => {}
            NegotiationInput::Signal(signal) => self.fail(&mut steps, signal.condition()),
        }
        steps
    }

    fn on_header(&mut self, header: &StreamHeader, steps: &mut Vec<Step>) {
        let State::AwaitHeader(stage) = self.state else {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        };
        match self.check_response_header(header) {
            Ok(()) => self.state = State::AwaitFeatures(stage),
            Err(condition) => self.state = State::AwaitPeerError(stage, condition),
        }
    }

    /// RFC 6120 §4.7: the response header names the destination as `from`,
    /// this stream's source as `to`, carries a stream ID and speaks XMPP 1.0.
    fn check_response_header(&self, header: &StreamHeader) -> Result<(), StreamErrorCondition> {
        if !header.version.is_some_and(StreamVersion::supports_xmpp_1_0) {
            return Err(StreamErrorCondition::UnsupportedVersion);
        }
        match &header.from {
            HeaderAddress::Domain(from) if *from == self.config.destination => {}
            HeaderAddress::Domain(_) | HeaderAddress::Refused => {
                return Err(StreamErrorCondition::InvalidFrom)
            }
            HeaderAddress::Absent | HeaderAddress::Invalid => {
                return Err(StreamErrorCondition::BadFormat)
            }
        }
        match &header.to {
            HeaderAddress::Domain(to) if *to == self.config.source => {}
            HeaderAddress::Domain(_) | HeaderAddress::Refused => {
                return Err(StreamErrorCondition::HostUnknown)
            }
            HeaderAddress::Absent | HeaderAddress::Invalid => {
                return Err(StreamErrorCondition::BadFormat)
            }
        }
        if header.id.is_none() {
            return Err(StreamErrorCondition::BadFormat);
        }
        Ok(())
    }

    fn on_element(&mut self, element: Element, steps: &mut Vec<Step>) {
        match (self.state, classify(element, MAX_SUCCESS_DATA_BYTES)) {
            (_, TopLevel::PeerStreamError(condition)) => {
                self.close(steps, CloseReason::PeerStreamError(condition))
            }
            (State::AwaitPeerError(_, condition), _) => self.fail(steps, condition),
            (_, TopLevel::ClientStanza) => self.fail(steps, StreamErrorCondition::InvalidNamespace),
            (_, TopLevel::Dialback) => self.on_dialback(steps),
            (State::Established, _) => self.fail(steps, StreamErrorCondition::PolicyViolation),
            (_, TopLevel::Stanza(_)) => self.fail(steps, StreamErrorCondition::NotAuthorized),
            // STARTTLS is mandatory in this profile: request it whether or
            // not the receiving server advertised it, and never continue in
            // plaintext.
            (State::AwaitFeatures(Stage::PreTls), TopLevel::Features(_)) => {
                steps.push(Step::Send(elements::starttls_request()));
                self.state = State::AwaitProceed;
            }
            (State::AwaitProceed, TopLevel::Proceed) => {
                steps.push(Step::StartTls);
                self.state = State::TlsHandshake;
            }
            (State::AwaitProceed, TopLevel::TlsFailure) => {
                self.close(steps, CloseReason::TlsRefused)
            }
            (State::AwaitFeatures(Stage::Tls), TopLevel::Features(features)) => {
                self.on_tls_features(features, steps)
            }
            (
                State::AwaitSaslOutcome,
                TopLevel::Success(Ok(SaslPayload::Absent | SaslPayload::Empty)),
            ) => {
                steps.push(Step::RestartAfterSasl);
                steps.push(Step::Open(initial_header(&self.config)));
                self.state = State::AwaitHeader(Stage::Authenticated);
            }
            (State::AwaitSaslOutcome, TopLevel::SaslFailure(condition)) => {
                self.close(steps, CloseReason::SaslFailure(condition))
            }
            (State::AwaitFeatures(Stage::Authenticated), TopLevel::Features(features)) => {
                self.on_final_features(features, steps)
            }
            _ => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    /// Post-TLS features. Only the STARTTLS and SASL EXTERNAL offers
    /// matter; other children, including unknown or malformed ones, are
    /// ignored.
    fn on_tls_features(&mut self, features: Element, steps: &mut Vec<Step>) {
        if offers_starttls(&features) {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        if offered_mechanisms(&features).any(|mechanism| mechanism == EXTERNAL) {
            steps.push(Step::Send(elements::external_auth()));
            self.state = State::AwaitSaslOutcome;
        } else {
            self.weak_identity_fallback(steps);
        }
    }

    /// Post-SASL features: neither STARTTLS nor a SASL mechanism may still
    /// be offered; anything else is ignored.
    fn on_final_features(&mut self, features: Element, steps: &mut Vec<Step>) {
        if offers_starttls(&features) || offered_mechanisms(&features).next().is_some() {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        steps.push(Step::Established(NegotiatedStream {
            role: Role::Initiating,
            source: self.config.source.clone(),
            destination: self.config.destination.clone(),
        }));
        self.state = State::Established;
    }

    fn on_tls(&mut self, peer: TlsPeer, steps: &mut Vec<Step>) {
        if self.state != State::TlsHandshake {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        }
        match peer {
            TlsPeer::Verified(domain) if domain == self.config.destination => {
                steps.push(Step::Open(initial_header(&self.config)));
                self.state = State::AwaitHeader(Stage::Tls);
            }
            TlsPeer::Verified(_) | TlsPeer::NoCertificate => {
                self.fail(steps, StreamErrorCondition::PolicyViolation)
            }
        }
    }

    /// Decision point for XEP-0220 when SASL EXTERNAL is not offered.
    fn weak_identity_fallback(&mut self, steps: &mut Vec<Step>) {
        match self.config.dialback {
            DialbackMode::Disabled => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    /// Decision point for received XEP-0220 elements.
    fn on_dialback(&mut self, steps: &mut Vec<Step>) {
        match self.config.dialback {
            DialbackMode::Disabled => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    /// Close with a stream error. The initiating role always has its
    /// stream header open except during the TLS handshake, where the
    /// transport closes without XML.
    fn fail(&mut self, steps: &mut Vec<Step>, condition: StreamErrorCondition) {
        self.close(steps, CloseReason::StreamError(condition));
    }

    fn close(&mut self, steps: &mut Vec<Step>, reason: CloseReason) {
        steps.push(Step::Close(reason));
        self.state = State::Closed;
    }
}

/// Whether a features element offers STARTTLS (RFC 6120 §5.4.1).
fn offers_starttls(features: &Element) -> bool {
    features.children().any(|child| child.is("starttls", TLS))
}

/// The SASL mechanism names a features element offers (RFC 6120 §6.4.1).
fn offered_mechanisms(features: &Element) -> impl Iterator<Item = String> + '_ {
    features
        .children()
        .filter(|child| child.is("mechanisms", SASL))
        .flat_map(|mechanisms| mechanisms.children())
        .filter(|mechanism| mechanism.is("mechanism", SASL))
        .map(|mechanism| mechanism.text().trim().to_owned())
}

fn initial_header(config: &InitiatingConfig) -> StreamHeader {
    StreamHeader::initial(
        config.source.clone(),
        config.destination.clone(),
        config.language.clone(),
    )
}
