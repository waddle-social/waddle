//! Initiating-role negotiation: STARTTLS or XEP-0368 direct TLS, then SASL
//! EXTERNAL and the authenticated restart, or XEP-0220 dialback as the
//! Initiating Server, either where EXTERNAL is not offered or after the
//! receiving server refused it (XEP-0178 §3, step 9b); and verification
//! connections, on which a Receiving Server asks an Authoritative Server
//! about one dialback key.

use minidom::Element;

use crate::ns::{DIALBACK_FEATURES, SASL, TLS};
use crate::s2s::condition::{SaslFailureCondition, StreamErrorCondition};
use crate::s2s::decoder::DecodeError;
use crate::s2s::header::{HeaderAddress, StreamHeader, StreamId, StreamVersion};

use super::classify::{classify, SaslPayload, TopLevel};
use super::dialback::{self, DialbackElement, DialbackType};
use super::elements::{self, EXTERNAL};
use super::types::{
    CloseReason, InitiatingConfig, NegotiatedStream, NegotiationInput, NegotiationPhase, Role,
    Step, StreamAuthentication, TlsEntry, TlsPeer, VerifyRequest,
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
    /// A dialback key was sent; awaiting the Receiving Server's result.
    AwaitDialbackResult,
    /// A verification request was sent; awaiting the answer.
    AwaitVerifyResult,
    Established,
    Closed,
}

/// What the connection is for.
#[derive(Debug, Clone)]
enum Purpose {
    /// Authenticate the source domain to the destination and carry its
    /// stanzas.
    Stream,
    /// Ask the destination's Authoritative Server about one key and close.
    Verify(VerifyRequest),
}

#[derive(Debug)]
pub(crate) struct Initiating {
    config: InitiatingConfig,
    purpose: Purpose,
    state: State,
    /// The ID of the latest response header the receiving server sent.
    stream_id: Option<StreamId>,
    /// Whether the receiving server's post-TLS response header declared
    /// the dialback namespace (XEP-0220 §2.4.1).
    header_declares_dialback: bool,
    /// Whether the receiving server's post-TLS features offered dialback
    /// with error handling (XEP-0220 §2.4.2).
    features_offer_dialback: bool,
}

impl Initiating {
    /// Start a stream that reaches TLS by `entry`. With STARTTLS the
    /// returned steps open the plaintext stream header; with direct TLS
    /// they start TLS at once, and the first header follows the handshake.
    pub(crate) fn start(config: InitiatingConfig, entry: TlsEntry) -> (Self, Vec<Step>) {
        Self::begin(config, Purpose::Stream, entry)
    }

    /// Start a verification connection (XEP-0220 §2.1.2): once TLS has
    /// verified the destination, send `request` as `<db:verify/>`, report
    /// the answer as [`Step::VerifyVerdict`] and close. SASL is never
    /// attempted on it.
    pub(crate) fn verify(
        config: InitiatingConfig,
        request: VerifyRequest,
        entry: TlsEntry,
    ) -> (Self, Vec<Step>) {
        Self::begin(config, Purpose::Verify(request), entry)
    }

    fn begin(config: InitiatingConfig, purpose: Purpose, entry: TlsEntry) -> (Self, Vec<Step>) {
        let mut machine = Self {
            config,
            purpose,
            state: State::AwaitHeader(Stage::PreTls),
            stream_id: None,
            header_declares_dialback: false,
            features_offer_dialback: false,
        };
        let steps = match entry {
            TlsEntry::StartTls => vec![Step::Open(machine.header())],
            // XEP-0368: TLS from the first byte; no plaintext stream and
            // no `<starttls/>`.
            TlsEntry::DirectTls => {
                machine.state = State::TlsHandshake;
                vec![Step::StartTls]
            }
        };
        (machine, steps)
    }

    /// Whether emitted headers declare the dialback namespace: on every
    /// verification connection, and on streams that may use dialback.
    fn declares_dialback(&self) -> bool {
        matches!(self.purpose, Purpose::Verify(_)) || self.config.dialback.keys().is_some()
    }

    fn header(&self) -> StreamHeader {
        let mut header = initial_header(&self.config);
        header.declare_dialback = self.declares_dialback();
        header
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
            State::AwaitDialbackResult => NegotiationPhase::DialbackAwaitResult,
            State::AwaitVerifyResult => NegotiationPhase::VerifyAwaitResult,
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
            // The initiating role never requests an authorization or
            // dialback decision.
            NegotiationInput::Authorization { .. } | NegotiationInput::Dialback(_) => {}
            NegotiationInput::Signal(signal) => self.fail(&mut steps, signal.condition()),
        }
        steps
    }

    fn on_header(&mut self, header: &StreamHeader, steps: &mut Vec<Step>) {
        let State::AwaitHeader(stage) = self.state else {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        };
        match self.check_response_header(header) {
            Ok(()) => {
                self.stream_id = header.id.clone();
                if stage == Stage::Tls {
                    self.header_declares_dialback = header.declare_dialback;
                }
                self.state = State::AwaitFeatures(stage);
            }
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
            (_, TopLevel::Dialback(element)) => self.on_dialback(element, steps),
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
                steps.push(Step::Open(self.header()));
                self.state = State::AwaitHeader(Stage::Authenticated);
            }
            (State::AwaitSaslOutcome, TopLevel::SaslFailure(condition)) => {
                self.on_sasl_failure(condition, steps)
            }
            (State::AwaitFeatures(Stage::Authenticated), TopLevel::Features(features)) => {
                self.on_final_features(features, steps)
            }
            _ => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    /// Post-TLS features. Only the STARTTLS and SASL EXTERNAL offers
    /// matter; other children, including unknown or malformed ones, are
    /// ignored. A verification connection sends its request whatever is
    /// offered: it never authenticates.
    fn on_tls_features(&mut self, features: Element, steps: &mut Vec<Step>) {
        if offers_starttls(&features) {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        if let Purpose::Verify(request) = &self.purpose {
            // `from` is this (Receiving) server, `to` the Sender Domain
            // whose Authoritative Server this connection reached, and `id`
            // the stream the key arrived on (XEP-0220 §2.1.2).
            steps.push(Step::Send(dialback::verify_request(
                &self.config.source,
                &self.config.destination,
                &request.stream_id,
                &request.key,
            )));
            self.state = State::AwaitVerifyResult;
            return;
        }
        self.features_offer_dialback = offers_dialback(&features);
        if offered_mechanisms(&features).any(|mechanism| mechanism == EXTERNAL) {
            steps.push(Step::Send(elements::external_auth()));
            self.state = State::AwaitSaslOutcome;
        } else {
            self.weak_identity_fallback(steps);
        }
    }

    /// The receiving server refused SASL EXTERNAL. A receiving server that
    /// cannot verify this server's certificate answers EXTERNAL with a
    /// failure and may continue with dialback on the same stream (XEP-0178
    /// §3, step 9b; RFC 6120 §6.4.5 leaves the stream open after a SASL
    /// failure). Where the Initiating Server role runs and the receiving
    /// server advertised dialback, its key is sent once, exactly as when
    /// EXTERNAL is not offered: TLS has verified the receiving server, so
    /// this is not the inbound case ADR-0020 D2 rules out, where a
    /// certificate the peer presented failed this server's verification.
    /// Otherwise the stream closes with the failure.
    fn on_sasl_failure(&mut self, condition: SaslFailureCondition, steps: &mut Vec<Step>) {
        if self.config.dialback.keys().is_some() && self.dialback_advertised() {
            self.weak_identity_fallback(steps);
        } else {
            self.close(steps, CloseReason::SaslFailure(condition));
        }
    }

    /// Whether the receiving server advertised dialback, by declaring its
    /// namespace on the post-TLS response header (XEP-0220 §2.4.1) or by
    /// the dialback stream feature (§2.4.2).
    fn dialback_advertised(&self) -> bool {
        self.header_declares_dialback || self.features_offer_dialback
    }

    /// Post-SASL features: neither STARTTLS nor a SASL mechanism may still
    /// be offered; anything else is ignored.
    fn on_final_features(&mut self, features: Element, steps: &mut Vec<Step>) {
        if offers_starttls(&features) || offered_mechanisms(&features).next().is_some() {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        self.establish(StreamAuthentication::SaslExternal, steps);
    }

    fn establish(&mut self, authentication: StreamAuthentication, steps: &mut Vec<Step>) {
        steps.push(Step::Established(NegotiatedStream {
            role: Role::Initiating,
            source: self.config.source.clone(),
            destination: self.config.destination.clone(),
            authentication,
        }));
        self.state = State::Established;
    }

    fn on_tls(&mut self, peer: TlsPeer, steps: &mut Vec<Step>) {
        if self.state != State::TlsHandshake {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        }
        match peer {
            TlsPeer::Verified(domain) if domain == self.config.destination => {
                steps.push(Step::Open(self.header()));
                self.state = State::AwaitHeader(Stage::Tls);
            }
            TlsPeer::Verified(_) | TlsPeer::NoCertificate => {
                self.fail(steps, StreamErrorCondition::PolicyViolation)
            }
        }
    }

    /// Decision point for XEP-0220 when SASL EXTERNAL is not offered, or
    /// was refused: send a dialback key as the Initiating Server (XEP-0220
    /// §2.1.1), computed for the stream ID of the receiving server's
    /// post-TLS header, but only to a receiving server that advertised
    /// dialback, by declaring its namespace on that header or by the
    /// dialback stream feature (XEP-0220 §2.4). A key is never sent to a
    /// server that did not ask for one. TLS has already verified the
    /// receiving server's certificate.
    fn weak_identity_fallback(&mut self, steps: &mut Vec<Step>) {
        let Some(keys) = self.config.dialback.keys() else {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        };
        if !self.dialback_advertised() {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        let Some(stream_id) = self.stream_id.as_ref() else {
            return self.fail(steps, StreamErrorCondition::BadFormat);
        };
        let Some(key) = keys.key(&self.config.destination, &self.config.source, stream_id) else {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        };
        steps.push(Step::Send(dialback::result_request(
            &self.config.source,
            &self.config.destination,
            &key,
        )));
        self.state = State::AwaitDialbackResult;
    }

    /// Received XEP-0220 elements. Only the answer this connection asked
    /// for is accepted: a `<db:result/>` result for the stream's pair after
    /// a key was sent, or a `<db:verify/>` answer for the pending request
    /// on a verification connection. Anything else, including requests,
    /// answers for another pair or stream ID and anything unsolicited, ends
    /// the stream with `policy-violation` (XEP-0220 §4.1).
    fn on_dialback(&mut self, element: DialbackElement, steps: &mut Vec<Step>) {
        match (self.state, element) {
            (
                State::AwaitDialbackResult,
                DialbackElement::Result {
                    from: HeaderAddress::Domain(from),
                    to: HeaderAddress::Domain(to),
                    kind,
                    ..
                },
            ) if from == self.config.destination && to == self.config.source => match kind {
                DialbackType::Valid => self.establish(StreamAuthentication::Dialback, steps),
                DialbackType::Invalid => self.close(steps, CloseReason::DialbackRefused),
                DialbackType::Error => self.close(steps, CloseReason::DialbackFailed),
                DialbackType::Request | DialbackType::Unknown => {
                    self.fail(steps, StreamErrorCondition::PolicyViolation)
                }
            },
            (
                State::AwaitVerifyResult,
                DialbackElement::Verify {
                    from: HeaderAddress::Domain(from),
                    to: HeaderAddress::Domain(to),
                    id: Some(id),
                    kind,
                    ..
                },
            ) if from == self.config.destination
                && to == self.config.source
                && self.is_pending_request(&id) =>
            {
                match dialback::verdict(kind) {
                    Some(verdict) => {
                        steps.push(Step::VerifyVerdict(verdict));
                        self.close(steps, CloseReason::VerificationComplete);
                    }
                    None => self.fail(steps, StreamErrorCondition::PolicyViolation),
                }
            }
            _ => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    fn is_pending_request(&self, id: &StreamId) -> bool {
        matches!(&self.purpose, Purpose::Verify(request) if request.stream_id == *id)
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

/// Whether a features element offers dialback with error handling
/// (XEP-0220 §2.4.2).
fn offers_dialback(features: &Element) -> bool {
    features
        .children()
        .any(|child| child.is("dialback", DIALBACK_FEATURES))
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
