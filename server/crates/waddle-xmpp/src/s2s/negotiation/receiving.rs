//! Receiving-role negotiation: STARTTLS, then SASL EXTERNAL and the
//! authenticated restart, or XEP-0220 dialback over TLS as the Receiving
//! Server; and answers to `<db:verify/>` as the Authoritative Server.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use jid::Jid;
use minidom::rxml::Namespace;
use minidom::Element;

use crate::identity::{canonical_jid, parse_jid, CanonicalDomain};
use crate::s2s::condition::{SaslFailureCondition, StreamErrorCondition};
use crate::s2s::decoder::DecodeError;
use crate::s2s::header::{HeaderAddress, StreamHeader, StreamId, StreamVersion};

use super::classify::{classify, Mechanism, SaslPayload, TopLevel};
use super::dialback::{self, DialbackElement, DialbackType};
use super::elements;
use super::types::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, DestinationStatus, DialbackKey,
    DialbackOutcome, DialbackRequest, ExchangeId, NegotiatedStream, NegotiationInput,
    NegotiationPhase, ReceivingConfig, Role, Step, StreamAuthentication, TlsPeer,
};

/// Most `<db:verify/>` requests answered on one stream within
/// [`VERIFY_WINDOW`]. A Receiving Server asks once per stream it verifies,
/// and a long-lived stream may carry requests for as long as it lives; the
/// rate keeps one connection from using this server as an unbounded key
/// oracle without limiting how long it may keep asking.
const MAX_VERIFY_REQUESTS: usize = 8;

/// The window [`MAX_VERIFY_REQUESTS`] is counted over.
const VERIFY_WINDOW: Duration = Duration::from_secs(60);

/// Which stream header the receiving role awaits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    PreTls,
    Tls,
    Authenticated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sasl {
    AwaitAuth,
    AwaitResponse(ExchangeId),
    Authorizing(ExchangeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitHeader(Stage),
    AwaitStartTls,
    TlsHandshake,
    Sasl(Sasl),
    /// TLS completed without a certificate usable for SASL EXTERNAL and
    /// dialback is possible: awaiting a dialback key or verification
    /// request.
    Unauthenticated,
    /// A dialback key was handed to the owner for verification.
    DialbackVerifying,
    Established,
    Closed,
}

#[derive(Debug)]
pub(crate) struct Receiving {
    config: ReceivingConfig,
    state: State,
    /// Whether a response stream header is open on the current stream.
    outbound_open: bool,
    /// The most recent served header recipient, used as the `from` of an
    /// error-carrying response header.
    served: Option<CanonicalDomain>,
    /// The `(source, destination)` pair fixed by the first accepted header.
    pair: Option<(CanonicalDomain, CanonicalDomain)>,
    tls_peer: Option<TlsPeer>,
    auth_starts: u8,
    aborts: u8,
    last_exchange: Option<ExchangeId>,
    /// Whether address-checked stanzas on the established stream go to the
    /// connection owner instead of being refused here.
    deliver: bool,
    /// The ID of the latest response header sent.
    stream_id: Option<StreamId>,
    /// Whether the owner permits dialback for the pair, asked once TLS has
    /// completed.
    dialback_permitted: bool,
    /// Whether a dialback key was received; one pair, one attempt.
    dialback_used: bool,
    /// When the verification requests answered within the last
    /// [`VERIFY_WINDOW`] arrived, oldest first.
    verify_requests: VecDeque<Instant>,
}

impl Receiving {
    pub(crate) fn new(config: ReceivingConfig) -> Self {
        Self {
            config,
            state: State::AwaitHeader(Stage::PreTls),
            outbound_open: false,
            served: None,
            pair: None,
            tls_peer: None,
            auth_starts: 0,
            aborts: 0,
            last_exchange: None,
            deliver: false,
            stream_id: None,
            dialback_permitted: false,
            dialback_used: false,
            verify_requests: VecDeque::new(),
        }
    }

    pub(crate) fn deliver_stanzas(&mut self) {
        self.deliver = true;
    }

    pub(crate) fn phase(&self) -> NegotiationPhase {
        match self.state {
            State::AwaitHeader(Stage::PreTls) | State::AwaitStartTls => NegotiationPhase::PreTls,
            State::TlsHandshake => NegotiationPhase::TlsHandshake,
            State::AwaitHeader(Stage::Tls) => NegotiationPhase::TlsStream,
            State::Sasl(Sasl::AwaitAuth) => NegotiationPhase::SaslAwaitAuth,
            State::Sasl(Sasl::AwaitResponse(_)) => NegotiationPhase::SaslAwaitResponse,
            State::Sasl(Sasl::Authorizing(_)) => NegotiationPhase::SaslAuthorizing,
            State::AwaitHeader(Stage::Authenticated) => NegotiationPhase::AuthenticatedRestart,
            State::Unauthenticated => NegotiationPhase::DialbackAwaitKey,
            State::DialbackVerifying => NegotiationPhase::DialbackVerifying,
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
            NegotiationInput::Header(header) => self.on_header(header, &mut steps),
            NegotiationInput::Element(element) => self.on_element(element, false, &mut steps),
            NegotiationInput::PipelinedElement(element) => {
                self.on_element(element, true, &mut steps)
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
            NegotiationInput::Authorization { exchange, decision } => {
                self.on_authorization(exchange, decision, &mut steps)
            }
            NegotiationInput::Dialback(outcome) => self.on_dialback_outcome(outcome, &mut steps),
            NegotiationInput::Signal(signal) => self.fail(&mut steps, signal.condition()),
        }
        steps
    }

    fn on_header(&mut self, header: StreamHeader, steps: &mut Vec<Step>) {
        let State::AwaitHeader(stage) = self.state else {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        };
        let (source, destination) = match self.accept_header(&header) {
            Ok(pair) => pair,
            Err(condition) => return self.fail(steps, condition),
        };
        let mut response = StreamHeader::response(
            destination.clone(),
            Some(source.clone()),
            self.config.language.clone(),
        );
        response.declare_dialback = self.config.dialback.keys().is_some()
            || self
                .config
                .admission
                .permits_dialback(&source, &destination);
        self.stream_id = response.id.clone();
        steps.push(Step::Open(response));
        self.outbound_open = true;
        match stage {
            Stage::PreTls => {
                steps.push(Step::Send(elements::starttls_features()));
                self.state = State::AwaitStartTls;
            }
            Stage::Tls => self.offer_sasl(&source, steps),
            Stage::Authenticated => {
                steps.push(Step::Send(elements::empty_features()));
                steps.push(Step::Established(NegotiatedStream {
                    role: Role::Receiving,
                    source,
                    destination,
                    authentication: StreamAuthentication::SaslExternal,
                }));
                self.state = State::Established;
            }
        }
    }

    /// Check an initial stream header and return its `(source, destination)`.
    fn accept_header(
        &mut self,
        header: &StreamHeader,
    ) -> Result<(CanonicalDomain, CanonicalDomain), StreamErrorCondition> {
        if !header.version.is_some_and(StreamVersion::supports_xmpp_1_0) {
            return Err(StreamErrorCondition::UnsupportedVersion);
        }
        // A spelling the identity profile refuses names no domain this
        // server could serve.
        let destination = match &header.to {
            HeaderAddress::Domain(domain) => domain.clone(),
            HeaderAddress::Refused => return Err(StreamErrorCondition::HostUnknown),
            HeaderAddress::Absent | HeaderAddress::Invalid => {
                return Err(StreamErrorCondition::BadFormat)
            }
        };
        match self.config.admission.destination(&destination) {
            DestinationStatus::Served => {}
            DestinationStatus::Gone => return Err(StreamErrorCondition::HostGone),
            DestinationStatus::Unknown => return Err(StreamErrorCondition::HostUnknown),
        }
        self.served = Some(destination.clone());
        // This profile requires `from` on every header, including the
        // plaintext one: it selects the configured pair and is the TLS
        // reference identity.
        // A spelling the identity profile refuses cannot be a reference
        // identity or an admitted domain.
        let source = match &header.from {
            HeaderAddress::Domain(domain) => domain.clone(),
            HeaderAddress::Refused => return Err(StreamErrorCondition::InvalidFrom),
            HeaderAddress::Absent | HeaderAddress::Invalid => {
                return Err(StreamErrorCondition::BadFormat)
            }
        };
        if let Some((pinned_source, pinned_destination)) = &self.pair {
            if *pinned_destination != destination {
                return Err(StreamErrorCondition::HostUnknown);
            }
            if *pinned_source != source {
                return Err(StreamErrorCondition::InvalidFrom);
            }
        }
        // A refused pair receives `policy-violation`, the same condition
        // whichever list or rule refused it.
        if !self.config.admission.admits(&source, &destination) {
            return Err(StreamErrorCondition::PolicyViolation);
        }
        self.pair = Some((source.clone(), destination.clone()));
        Ok((source, destination))
    }

    /// Post-TLS features. A certificate verified for the source supports
    /// SASL EXTERNAL; dialback is offered beside it, and instead of it when
    /// no certificate was presented, only where the owner permits dialback
    /// for the pair. A presented certificate that failed verification never
    /// reaches this point: the TLS handshake fails (ADR-0020 D2).
    fn offer_sasl(&mut self, source: &CanonicalDomain, steps: &mut Vec<Step>) {
        let Some((_, destination)) = self.pair.clone() else {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        };
        self.dialback_permitted = self.config.admission.permits_dialback(source, &destination);
        match &self.tls_peer {
            Some(TlsPeer::Verified(verified)) if verified == source => {
                let mut features = elements::external_features();
                if self.dialback_permitted {
                    features.append_child(dialback::feature());
                }
                steps.push(Step::Send(features));
                self.state = State::Sasl(Sasl::AwaitAuth);
            }
            Some(TlsPeer::Verified(_)) => self.fail(steps, StreamErrorCondition::InvalidFrom),
            Some(TlsPeer::NoCertificate) | None => self.weak_identity_fallback(steps),
        }
    }

    /// Decision point for XEP-0220 when no certificate supports EXTERNAL:
    /// await a dialback key where the owner permits dialback for the pair,
    /// or verification requests where this server answers them as the
    /// Authoritative Server; otherwise close.
    fn weak_identity_fallback(&mut self, steps: &mut Vec<Step>) {
        if self.dialback_permitted {
            let mut features = elements::empty_features();
            features.append_child(dialback::feature());
            steps.push(Step::Send(features));
            self.state = State::Unauthenticated;
        } else if self.config.dialback.keys().is_some() {
            steps.push(Step::Send(elements::empty_features()));
            self.state = State::Unauthenticated;
        } else {
            self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
    }

    /// Received XEP-0220 elements. An inbound stream carries only requests:
    /// a `type` on either element is an answer nothing here asked for, and
    /// is refused as unsolicited (XEP-0220 §2.2 and §4.1).
    fn on_dialback(&mut self, element: DialbackElement, steps: &mut Vec<Step>) {
        match element {
            DialbackElement::Result {
                from,
                to,
                kind: DialbackType::Request,
                key,
            } => self.on_result_request(&from, &to, key, steps),
            DialbackElement::Verify {
                from,
                to,
                id,
                kind: DialbackType::Request,
                key,
            } => self.on_verify_request(&from, &to, id, key, steps),
            DialbackElement::Result { .. }
            | DialbackElement::Verify { .. }
            | DialbackElement::Other => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    /// Check a dialback element's `from` and `to` against the stream's
    /// pair: one stream carries one pair, so a dialback element for any
    /// other pair (sender or target multiplexing) ends the stream, with the
    /// condition a stanza for that pair would get.
    fn check_pair(
        &self,
        from: &HeaderAddress,
        to: &HeaderAddress,
    ) -> Result<(CanonicalDomain, CanonicalDomain), StreamErrorCondition> {
        let Some((source, destination)) = &self.pair else {
            return Err(StreamErrorCondition::InternalServerError);
        };
        match from {
            HeaderAddress::Domain(from) if from == source => {}
            HeaderAddress::Domain(_) | HeaderAddress::Refused => {
                return Err(StreamErrorCondition::InvalidFrom)
            }
            HeaderAddress::Absent | HeaderAddress::Invalid => {
                return Err(StreamErrorCondition::ImproperAddressing)
            }
        }
        match to {
            HeaderAddress::Domain(to) if to == destination => {}
            HeaderAddress::Domain(_) | HeaderAddress::Refused => {
                return Err(StreamErrorCondition::HostUnknown)
            }
            HeaderAddress::Absent | HeaderAddress::Invalid => {
                return Err(StreamErrorCondition::ImproperAddressing)
            }
        }
        Ok((source.clone(), destination.clone()))
    }

    /// A dialback key from the Initiating Server (XEP-0220 §2.2.1): only
    /// over TLS, after the post-TLS features, where the owner permits
    /// dialback for the pair, and once per stream. The owner verifies it.
    fn on_result_request(
        &mut self,
        from: &HeaderAddress,
        to: &HeaderAddress,
        key: Option<DialbackKey>,
        steps: &mut Vec<Step>,
    ) {
        let awaiting = matches!(
            self.state,
            State::Unauthenticated | State::Sasl(Sasl::AwaitAuth)
        );
        if !awaiting || !self.dialback_permitted || self.dialback_used {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        let (source, destination) = match self.check_pair(from, to) {
            Ok(pair) => pair,
            Err(condition) => return self.fail(steps, condition),
        };
        let (Some(key), Some(stream_id)) = (key, self.stream_id.clone()) else {
            return self.fail(steps, StreamErrorCondition::BadFormat);
        };
        self.dialback_used = true;
        self.state = State::DialbackVerifying;
        steps.push(Step::VerifyDialback(DialbackRequest {
            source,
            destination,
            stream_id,
            key,
        }));
    }

    /// A verification request to this server as the Authoritative Server
    /// (XEP-0220 §2.2.2): only over TLS, after the post-TLS features, from
    /// the stream's source about a key this deployment issued to it from
    /// the stream's destination. Keys are checked without state, so a key
    /// issued by any replica holding the deployment's secret verifies.
    fn on_verify_request(
        &mut self,
        from: &HeaderAddress,
        to: &HeaderAddress,
        id: Option<StreamId>,
        key: Option<DialbackKey>,
        steps: &mut Vec<Step>,
    ) {
        let after_features = matches!(
            self.state,
            State::Sasl(_) | State::Unauthenticated | State::DialbackVerifying | State::Established
        );
        let Some(keys) = self.config.dialback.keys().cloned() else {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        };
        if !after_features {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        let (receiving, originating) = match self.check_pair(from, to) {
            Ok(pair) => pair,
            Err(condition) => return self.fail(steps, condition),
        };
        let (Some(id), Some(key)) = (id, key) else {
            return self.fail(steps, StreamErrorCondition::BadFormat);
        };
        if !self.admit_verify_request() {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        let valid = keys.verify(&receiving, &originating, &id, &key);
        steps.push(Step::Send(dialback::verify_answer(
            &originating,
            &receiving,
            &id,
            valid,
        )));
        steps.push(Step::DialbackAnswered { valid });
    }

    /// Count one more verification request, unless [`MAX_VERIFY_REQUESTS`]
    /// were answered within the last [`VERIFY_WINDOW`].
    fn admit_verify_request(&mut self) -> bool {
        let now = self.config.clock.now();
        while self
            .verify_requests
            .front()
            .is_some_and(|answered| now.saturating_duration_since(*answered) >= VERIFY_WINDOW)
        {
            self.verify_requests.pop_front();
        }
        if self.verify_requests.len() >= MAX_VERIFY_REQUESTS {
            return false;
        }
        self.verify_requests.push_back(now);
        true
    }

    /// The owner's verification of the stream's dialback key. A valid key
    /// establishes the stream without a restart (XEP-0220 §2.2.1); an
    /// invalid one, or one that could not be verified, is answered and the
    /// stream closes, as it carries no other pair.
    fn on_dialback_outcome(&mut self, outcome: DialbackOutcome, steps: &mut Vec<Step>) {
        if self.state != State::DialbackVerifying {
            return;
        }
        let Some((source, destination)) = self.pair.clone() else {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        };
        steps.push(Step::Send(dialback::result_answer(
            &destination,
            &source,
            outcome,
        )));
        match outcome {
            DialbackOutcome::Valid => {
                steps.push(Step::Established(NegotiatedStream {
                    role: Role::Receiving,
                    source,
                    destination,
                    authentication: StreamAuthentication::Dialback,
                }));
                self.state = State::Established;
            }
            DialbackOutcome::Invalid => self.close(steps, CloseReason::DialbackRefused),
            DialbackOutcome::Error(_) => self.close(steps, CloseReason::DialbackFailed),
        }
    }

    fn on_tls(&mut self, peer: TlsPeer, steps: &mut Vec<Step>) {
        if self.state != State::TlsHandshake {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        }
        self.tls_peer = Some(peer);
        self.state = State::AwaitHeader(Stage::Tls);
    }

    fn on_element(&mut self, element: Element, pipelined: bool, steps: &mut Vec<Step>) {
        let element = classify(element, self.config.sasl.max_authzid_bytes);
        match (self.state, element) {
            (_, TopLevel::PeerStreamError(condition)) => {
                self.close(steps, CloseReason::PeerStreamError(condition))
            }
            (_, TopLevel::ClientStanza) => self.fail(steps, StreamErrorCondition::InvalidNamespace),
            (_, TopLevel::Dialback(element)) => self.on_dialback(element, steps),
            (State::Established, TopLevel::Stanza(stanza)) => match self.check_stanza(stanza) {
                Err(condition) => self.fail(steps, condition),
                Ok(Checked::Accepted(stanza)) if self.deliver => steps.push(Step::Deliver(stanza)),
                Ok(Checked::Malformed(stanza)) if self.deliver => {
                    steps.push(Step::DeliverMalformed(stanza))
                }
                // Without application dispatch no stanza error can be
                // returned, so a refused spelling ends the stream.
                Ok(Checked::Malformed(_)) => {
                    self.fail(steps, StreamErrorCondition::ImproperAddressing)
                }
                // Application dispatch is not enabled on this stream.
                Ok(Checked::Accepted(_)) => self.fail(steps, StreamErrorCondition::PolicyViolation),
            },
            (State::Established, TopLevel::Other) => {
                self.fail(steps, StreamErrorCondition::UnsupportedStanzaType)
            }
            (_, TopLevel::Stanza(_)) => self.fail(steps, StreamErrorCondition::NotAuthorized),
            // Input sent after `<starttls/>` without waiting for `<proceed/>`
            // is refused rather than handed to TLS (RFC 6120 §5.4.3.1).
            (State::AwaitStartTls, TopLevel::StartTls) if pipelined => {
                self.fail(steps, StreamErrorCondition::PolicyViolation)
            }
            (State::AwaitStartTls, TopLevel::StartTls) => {
                steps.push(Step::Send(elements::proceed()));
                steps.push(Step::StartTls);
                self.outbound_open = false;
                self.state = State::TlsHandshake;
            }
            (State::Sasl(_), TopLevel::Auth { mechanism, payload }) => {
                self.on_auth(mechanism, payload, steps)
            }
            (State::Sasl(Sasl::AwaitResponse(exchange)), TopLevel::Response(payload)) => {
                self.on_payload(exchange, payload, steps)
            }
            (State::Sasl(_), TopLevel::Abort) => self.on_abort(steps),
            _ => self.fail(steps, StreamErrorCondition::PolicyViolation),
        }
    }

    fn on_auth(
        &mut self,
        mechanism: Mechanism,
        payload: Result<SaslPayload, SaslFailureCondition>,
        steps: &mut Vec<Step>,
    ) {
        if self.auth_starts >= self.config.sasl.max_auth_starts {
            return self.fail(steps, StreamErrorCondition::PolicyViolation);
        }
        self.auth_starts += 1;
        // A new exchange replaces any unfinished one (RFC 6120 §6.4.2); its
        // ID makes a pending decision for the replaced exchange stale.
        let exchange = self.next_exchange();
        if mechanism == Mechanism::Unsupported {
            return self.sasl_failure(SaslFailureCondition::InvalidMechanism, steps);
        }
        match payload {
            Ok(SaslPayload::Absent) => {
                steps.push(Step::Send(elements::empty_challenge()));
                self.state = State::Sasl(Sasl::AwaitResponse(exchange));
            }
            payload => self.on_payload(exchange, payload, steps),
        }
    }

    /// Handle an initial response or a response to the empty challenge.
    fn on_payload(
        &mut self,
        exchange: ExchangeId,
        payload: Result<SaslPayload, SaslFailureCondition>,
        steps: &mut Vec<Step>,
    ) {
        let authzid = match payload {
            Err(condition) => return self.sasl_failure(condition, steps),
            Ok(SaslPayload::Absent | SaslPayload::Empty) => None,
            Ok(SaslPayload::Data(bytes)) => Some(bytes),
        };
        let Some((source, destination)) = self.pair.clone() else {
            return self.fail(steps, StreamErrorCondition::InternalServerError);
        };
        if let Some(bytes) = authzid {
            // A supplied authorization identity grants nothing extra: it is
            // tolerated only when it names the verified source domain.
            let Ok(authzid) = String::from_utf8(bytes) else {
                return self.sasl_failure(SaslFailureCondition::IncorrectEncoding, steps);
            };
            if HeaderAddress::parse(Some(&authzid)).domain() != Some(&source) {
                return self.sasl_failure(SaslFailureCondition::InvalidAuthzid, steps);
            }
        }
        self.state = State::Sasl(Sasl::Authorizing(exchange));
        steps.push(Step::Authorize(AuthorizationRequest {
            exchange,
            source,
            destination,
        }));
    }

    fn on_abort(&mut self, steps: &mut Vec<Step>) {
        self.aborts = self.aborts.saturating_add(1);
        steps.push(Step::Send(SaslFailureCondition::Aborted.to_element()));
        let exhausted = self.aborts >= self.config.sasl.max_aborts
            || self.auth_starts >= self.config.sasl.max_auth_starts;
        if exhausted {
            self.close(steps, CloseReason::SaslExhausted);
        } else {
            self.state = State::Sasl(Sasl::AwaitAuth);
        }
    }

    fn on_authorization(
        &mut self,
        exchange: ExchangeId,
        decision: AuthorizationDecision,
        steps: &mut Vec<Step>,
    ) {
        if self.state != State::Sasl(Sasl::Authorizing(exchange)) {
            // A decision for a replaced, aborted or finished exchange.
            return;
        }
        match decision {
            AuthorizationDecision::Granted => {
                steps.push(Step::Send(elements::success()));
                steps.push(Step::RestartAfterSasl);
                self.outbound_open = false;
                self.state = State::AwaitHeader(Stage::Authenticated);
            }
            AuthorizationDecision::Denied => {
                self.sasl_failure(SaslFailureCondition::NotAuthorized, steps)
            }
        }
    }

    /// RFC 6120 §4.9.3.7, §4.9.3.6 and §4.9.3.9, in that order. Each
    /// address is read from its raw text with the identity profile's parser
    /// ([`parse_jid`]), never through JID preparation, which would fold some
    /// spellings and refuse labels outside Unicode 3.2; ASCII uppercase in
    /// the domainpart is mapped to lowercase first, as in stream headers. A
    /// stanza that passes is returned with both addresses rewritten in
    /// canonical form, so its domains compare equal to the stream's pair
    /// wherever it goes next.
    ///
    /// An address that is absent, empty or not a valid address at all
    /// closes the stream with `improper-addressing`. A valid address written
    /// in a spelling the profile refuses, such as an uppercase localpart,
    /// is read through RFC 7622 preparation instead, only to compare its
    /// domain with the pair and to address a reply: its domain must still
    /// be the stream's, and the stanza is returned as
    /// [`Checked::Malformed`], with both addresses in the canonical form of
    /// what preparation made of them, for the connection owner to answer
    /// with a `jid-malformed` stanza error.
    fn check_stanza(&self, mut stanza: Element) -> Result<Checked, StreamErrorCondition> {
        let Some((source, destination)) = &self.pair else {
            return Err(StreamErrorCondition::InternalServerError);
        };
        let (Some(to), Some(from)) = (
            stanza_address(&stanza, "to"),
            stanza_address(&stanza, "from"),
        ) else {
            return Err(StreamErrorCondition::ImproperAddressing);
        };
        if to.address.domain().as_str() != destination.as_str() {
            return Err(StreamErrorCondition::HostUnknown);
        }
        if from.address.domain().as_str() != source.as_str() {
            return Err(StreamErrorCondition::InvalidFrom);
        }
        let malformed = to.refused || from.refused;
        set_address(&mut stanza, "to", to.address);
        set_address(&mut stanza, "from", from.address);
        Ok(if malformed {
            Checked::Malformed(stanza)
        } else {
            Checked::Accepted(stanza)
        })
    }

    fn next_exchange(&mut self) -> ExchangeId {
        let exchange = self
            .last_exchange
            .map_or_else(ExchangeId::first, ExchangeId::next);
        self.last_exchange = Some(exchange);
        exchange
    }

    fn sasl_failure(&mut self, condition: SaslFailureCondition, steps: &mut Vec<Step>) {
        steps.push(Step::Send(condition.to_element()));
        self.close(steps, CloseReason::SaslFailure(condition));
    }

    /// Close with a stream error. RFC 6120 §4.9.1.2 requires a response
    /// header before the error when none is open on the current stream;
    /// during the TLS handshake no XML may be sent at all.
    fn fail(&mut self, steps: &mut Vec<Step>, condition: StreamErrorCondition) {
        if !self.outbound_open && self.state != State::TlsHandshake {
            let from = self
                .served
                .clone()
                .unwrap_or_else(|| self.config.default_domain.clone());
            steps.push(Step::Open(StreamHeader::response(
                from,
                None,
                self.config.language.clone(),
            )));
            self.outbound_open = true;
        }
        self.close(steps, CloseReason::StreamError(condition));
    }

    fn close(&mut self, steps: &mut Vec<Step>, reason: CloseReason) {
        steps.push(Step::Close(reason));
        self.state = State::Closed;
    }
}

/// The outcome of [`Receiving::check_stanza`] for a stanza whose
/// addresses belong to the stream's pair.
enum Checked {
    /// Both addresses are in the identity profile.
    Accepted(Element),
    /// At least one address is valid but written in a spelling the profile
    /// refuses.
    Malformed(Element),
}

/// A stanza address read from its raw text.
struct StanzaAddress {
    /// The canonical form: of the address itself, or of its RFC 7622
    /// preparation when `refused`.
    address: Jid,
    /// Whether the text is a spelling the identity profile refuses.
    refused: bool,
}

/// A stanza `to` or `from` address, or `None` when it is absent, empty or
/// not a valid address.
fn stanza_address(stanza: &Element, name: &str) -> Option<StanzaAddress> {
    let value = stanza.attr(name).filter(|value| !value.is_empty())?;
    let value = lowercase_domainpart(value);
    if let Ok(address) = parse_jid(&value) {
        return Some(StanzaAddress {
            address,
            refused: false,
        });
    }
    let prepared = Jid::new(&value).ok()?;
    let address = canonical_jid(&prepared).ok()?;
    Some(StanzaAddress {
        address,
        refused: true,
    })
}

/// `address` with ASCII uppercase letters in its domainpart mapped to
/// lowercase. DNS treats names that differ only in ASCII case as one name,
/// so the mapping joins nothing DNS keeps apart; no other mapping is
/// applied, and the localpart and resourcepart are left exactly as sent.
/// The parts are split as [`parse_jid`] splits them (RFC 7622 §3.2): the
/// first `/` starts the resourcepart, and an `@` before it ends the
/// localpart.
fn lowercase_domainpart(address: &str) -> Cow<'_, str> {
    let bare_end = address.find('/').unwrap_or(address.len());
    let domain_start = address[..bare_end].find('@').map_or(0, |at| at + 1);
    let domain = &address[domain_start..bare_end];
    if !domain.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Cow::Borrowed(address);
    }
    let mut mapped = String::with_capacity(address.len());
    mapped.push_str(&address[..domain_start]);
    mapped.push_str(&domain.to_ascii_lowercase());
    mapped.push_str(&address[bare_end..]);
    Cow::Owned(mapped)
}

/// Replace the unprefixed attribute `name`, which is present, with the
/// canonical text of `address`.
fn set_address(stanza: &mut Element, name: &str, address: Jid) {
    if let Some(value) = stanza.attrs_mut().get_mut(&Namespace::NONE, name) {
        *value = address.into_inner();
    }
}
