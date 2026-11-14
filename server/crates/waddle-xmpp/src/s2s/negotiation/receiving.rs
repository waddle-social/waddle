//! Receiving-role negotiation: STARTTLS, SASL EXTERNAL, authenticated
//! restart.

use std::borrow::Cow;

use jid::Jid;
use minidom::rxml::Namespace;
use minidom::Element;

use crate::identity::{parse_jid, CanonicalDomain};
use crate::s2s::condition::{SaslFailureCondition, StreamErrorCondition};
use crate::s2s::decoder::DecodeError;
use crate::s2s::header::{HeaderAddress, StreamHeader, StreamVersion};

use super::classify::{classify, Mechanism, SaslPayload, TopLevel};
use super::elements;
use super::types::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, DestinationStatus, DialbackMode,
    ExchangeId, NegotiatedStream, NegotiationInput, NegotiationPhase, ReceivingConfig, Role, Step,
    TlsPeer,
};

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
        steps.push(Step::Open(StreamHeader::response(
            destination.clone(),
            Some(source.clone()),
            self.config.language.clone(),
        )));
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

    fn offer_sasl(&mut self, source: &CanonicalDomain, steps: &mut Vec<Step>) {
        match &self.tls_peer {
            Some(TlsPeer::Verified(verified)) if verified == source => {
                steps.push(Step::Send(elements::external_features()));
                self.state = State::Sasl(Sasl::AwaitAuth);
            }
            Some(TlsPeer::Verified(_)) => self.fail(steps, StreamErrorCondition::InvalidFrom),
            Some(TlsPeer::NoCertificate) | None => self.weak_identity_fallback(steps),
        }
    }

    /// Decision point for XEP-0220 when no certificate supports EXTERNAL.
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
            (_, TopLevel::Dialback) => self.on_dialback(steps),
            (State::Established, TopLevel::Stanza(stanza)) => match self.check_stanza(stanza) {
                Err(condition) => self.fail(steps, condition),
                Ok(stanza) if self.deliver => steps.push(Step::Deliver(stanza)),
                // Application dispatch is not enabled on this stream.
                Ok(_) => self.fail(steps, StreamErrorCondition::PolicyViolation),
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
    /// Any address the profile refuses closes the stream with
    /// `improper-addressing`, including one that is a syntactically valid
    /// JID in a spelling the profile does not accept, such as an uppercase
    /// localpart. The handshake design records that the routing slice is
    /// to answer such a stanza with a `jid-malformed` stanza error instead
    /// and keep the stream.
    fn check_stanza(&self, mut stanza: Element) -> Result<Element, StreamErrorCondition> {
        let Some((source, destination)) = &self.pair else {
            return Err(StreamErrorCondition::InternalServerError);
        };
        let (Some(to), Some(from)) = (
            stanza_address(&stanza, "to"),
            stanza_address(&stanza, "from"),
        ) else {
            return Err(StreamErrorCondition::ImproperAddressing);
        };
        if to.domain().as_str() != destination.as_str() {
            return Err(StreamErrorCondition::HostUnknown);
        }
        if from.domain().as_str() != source.as_str() {
            return Err(StreamErrorCondition::InvalidFrom);
        }
        set_address(&mut stanza, "to", to);
        set_address(&mut stanza, "from", from);
        Ok(stanza)
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

/// A stanza `to` or `from` address in canonical form, or `None` when it is
/// absent, empty or outside the identity profile.
fn stanza_address(stanza: &Element, name: &str) -> Option<Jid> {
    stanza
        .attr(name)
        .filter(|value| !value.is_empty())
        .and_then(|value| parse_jid(&lowercase_domainpart(value)).ok())
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
