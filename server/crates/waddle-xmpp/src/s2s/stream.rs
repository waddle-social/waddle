//! I/O-agnostic S2S connection driver combining codec and negotiation.

use minidom::Element;

use super::decoder::{Inbound, StreamDecoder, TlsHandoff};
use super::encoder::StreamEncoder;
use super::limits::CodecLimits;
use super::negotiation::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, DialbackOutcome, DialbackRequest,
    ExchangeId, InitiatingConfig, NegotiatedStream, Negotiation, NegotiationInput,
    NegotiationPhase, ReceivingConfig, Role, Signal, Step, StreamAuthentication, TlsEntry, TlsPeer,
    VerifyRequest, VerifyVerdict,
};

/// What the connection owner must do next, in order.
#[derive(Debug, PartialEq)]
pub enum Output {
    /// Write these bytes to the current transport (TCP before STARTTLS, the
    /// TLS session afterwards).
    Transmit(Vec<u8>),
    /// Start TLS after writing all earlier output. The handoff holds bytes
    /// already received after the boundary element; feed them to the TLS
    /// engine before reading the socket again. It is always empty in the
    /// receiving role, which refuses input sent after `<starttls/>` before
    /// `<proceed/>`, and on a direct-TLS connection, where this is the
    /// first output; in the initiating role it holds bytes that followed
    /// the peer's `<proceed/>`.
    StartTls(TlsHandoff),
    /// Decide SASL EXTERNAL authorization and answer with
    /// [`S2sStream::authorization`].
    Authorize(AuthorizationRequest),
    /// Negotiation completed for this directional domain pair.
    Established(NegotiatedStream),
    /// A stanza from the established receiving stream that passed the
    /// stream's address checks, for the connection owner to accept or
    /// refuse. Emitted only after [`S2sStream::deliver_stanzas`].
    Stanza(Element),
    /// Verify this dialback key with the Authoritative Server of its source
    /// domain and answer with [`S2sStream::dialback`].
    VerifyDialback(DialbackRequest),
    /// A verification connection received the Authoritative Server's
    /// answer; the stream closes next.
    VerifyVerdict(VerifyVerdict),
    /// This server answered a `<db:verify/>` request as the Authoritative
    /// Server.
    DialbackAnswered { valid: bool },
    /// Close the transport after writing all earlier output.
    Close(CloseReason),
}

/// One S2S connection's XML codec and negotiation, without sockets or TLS.
///
/// The connection owner feeds received plaintext with [`Self::receive`],
/// reports TLS completion and authorization decisions, and executes the
/// returned [`Output`]s in order.
///
/// The decoder runs in its pre-authentication mode
/// ([`StreamDecoder::pre_authentication`]) until SASL succeeds or dialback
/// establishes the stream: a
/// first-level element outside the stream, TLS, SASL and dialback
/// namespaces is refused when its head closes, and negotiation elements are
/// held to the control-element ceiling.
#[derive(Debug)]
pub struct S2sStream {
    decoder: StreamDecoder,
    /// Present while an outbound stream header is open and not closed.
    encoder: Option<StreamEncoder>,
    negotiation: Negotiation,
    closed: bool,
}

impl S2sStream {
    /// Accept an inbound connection; nothing is sent until the peer's
    /// initial stream header arrives.
    pub fn accept(config: ReceivingConfig, limits: CodecLimits) -> Self {
        Self {
            decoder: StreamDecoder::pre_authentication(limits),
            encoder: None,
            negotiation: Negotiation::receiving(config),
            closed: false,
        }
    }

    /// Start an outbound connection with STARTTLS; the output opens the
    /// initial stream.
    pub fn initiate(config: InitiatingConfig, limits: CodecLimits) -> (Self, Vec<Output>) {
        Self::initiate_with(config, TlsEntry::StartTls, limits)
    }

    /// Start an outbound connection that reaches TLS by `entry`. For direct
    /// TLS the only output is [`Output::StartTls`] with an empty handoff:
    /// the handshake comes first, then the stream header.
    pub fn initiate_with(
        config: InitiatingConfig,
        entry: TlsEntry,
        limits: CodecLimits,
    ) -> (Self, Vec<Output>) {
        Self::start(Negotiation::initiating_with(config, entry), limits)
    }

    /// Start a verification connection to the Authoritative Server of
    /// `config.destination` (XEP-0220 §2.1.2). It reports the answer as
    /// [`Output::VerifyVerdict`] and closes.
    pub fn verify(
        config: InitiatingConfig,
        request: VerifyRequest,
        entry: TlsEntry,
        limits: CodecLimits,
    ) -> (Self, Vec<Output>) {
        Self::start(Negotiation::verifying(config, request, entry), limits)
    }

    fn start(started: (Negotiation, Vec<Step>), limits: CodecLimits) -> (Self, Vec<Output>) {
        let (negotiation, steps) = started;
        let mut stream = Self {
            decoder: StreamDecoder::pre_authentication(limits),
            encoder: None,
            negotiation,
            closed: false,
        };
        let mut out = Vec::new();
        stream.run(steps, &mut out);
        (stream, out)
    }

    /// Hand stanzas that pass an established receiving stream's address
    /// checks to the connection owner as [`Output::Stanza`] instead of
    /// refusing them. The owner refuses one with
    /// [`Signal::PolicyViolation`]. Has no effect in the initiating role.
    pub fn deliver_stanzas(&mut self) {
        self.negotiation.deliver_stanzas();
    }

    pub fn role(&self) -> Role {
        self.negotiation.role()
    }

    pub fn phase(&self) -> NegotiationPhase {
        self.negotiation.phase()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Bytes retained by the decoder.
    pub fn retained_bytes(&self) -> usize {
        self.decoder.retained_bytes()
    }

    /// Process received plaintext bytes.
    ///
    /// Each call must carry at most `max_buffered_bytes - max_stanza_bytes`
    /// bytes of the configured [`CodecLimits`] (see
    /// [`StreamDecoder::push`]); one TLS record's plaintext always fits.
    pub fn receive(&mut self, bytes: &[u8]) -> Vec<Output> {
        let mut out = Vec::new();
        if self.closed {
            return out;
        }
        match self.decoder.push(bytes) {
            Ok(()) => self.drain(&mut out),
            Err(error) => self.apply(NegotiationInput::DecodeFailed(error), &mut out),
        }
        out
    }

    /// Report a completed, verified TLS handshake.
    pub fn tls_established(&mut self, peer: TlsPeer) -> Vec<Output> {
        self.input(NegotiationInput::TlsEstablished(peer))
    }

    /// Answer an [`Output::Authorize`] request.
    pub fn authorization(
        &mut self,
        exchange: ExchangeId,
        decision: AuthorizationDecision,
    ) -> Vec<Output> {
        self.input(NegotiationInput::Authorization { exchange, decision })
    }

    /// Answer an [`Output::VerifyDialback`] request.
    pub fn dialback(&mut self, outcome: DialbackOutcome) -> Vec<Output> {
        self.input(NegotiationInput::Dialback(outcome))
    }

    /// Report a deadline, expiry, drain or resource event.
    pub fn signal(&mut self, signal: Signal) -> Vec<Output> {
        self.input(NegotiationInput::Signal(signal))
    }

    fn input(&mut self, input: NegotiationInput) -> Vec<Output> {
        let mut out = Vec::new();
        if !self.closed {
            self.apply(input, &mut out);
            self.drain(&mut out);
        }
        out
    }

    /// Hand complete items to the negotiation until input runs out or the
    /// stream closes. The decoder parses one item per call, so a restart
    /// requested by an item takes effect before any later byte is parsed.
    fn drain(&mut self, out: &mut Vec<Output>) {
        while !self.closed {
            let input = match self.decoder.next_item() {
                Ok(Some(Inbound::Header(header))) => NegotiationInput::Header(header),
                Ok(Some(Inbound::Element(element))) => {
                    // Bytes still retained after a complete item are input
                    // the peer sent before reading any reply to it.
                    if self.decoder.retained_bytes() > 0 {
                        NegotiationInput::PipelinedElement(element)
                    } else {
                        NegotiationInput::Element(element)
                    }
                }
                Ok(Some(Inbound::End)) => NegotiationInput::PeerEnd,
                Ok(None) => return,
                Err(error) => {
                    // Decoding errors are terminal for the stream.
                    self.apply(NegotiationInput::DecodeFailed(error), out);
                    return;
                }
            };
            self.apply(input, out);
        }
    }

    fn apply(&mut self, input: NegotiationInput, out: &mut Vec<Output>) {
        let steps = self.negotiation.handle(input);
        self.run(steps, out);
    }

    fn run(&mut self, steps: Vec<Step>, out: &mut Vec<Output>) {
        let mut premature = false;
        for step in steps {
            if self.closed {
                break;
            }
            match step {
                Step::Open(header) => match StreamEncoder::open(&header) {
                    Ok((encoder, bytes)) => {
                        self.encoder = Some(encoder);
                        transmit(out, bytes);
                    }
                    Err(_) => self.local_failure(out),
                },
                Step::Send(element) => {
                    let encoded = self
                        .encoder
                        .as_mut()
                        .map(|encoder| encoder.element(&element));
                    match encoded {
                        Some(Ok(bytes)) => transmit(out, bytes),
                        Some(Err(_)) | None => self.local_failure(out),
                    }
                }
                Step::StartTls => {
                    // The plaintext stream is replaced without a closing tag
                    // (RFC 6120 §4.3.3).
                    self.encoder = None;
                    out.push(Output::StartTls(self.decoder.restart_after_tls()));
                }
                Step::RestartAfterSasl => {
                    self.encoder = None;
                    premature |= !self.decoder.restart_after_sasl().is_empty();
                }
                Step::Authorize(request) => out.push(Output::Authorize(request)),
                Step::Established(stream) => {
                    // Dialback authenticates without a restart, so the
                    // pre-authentication mode ends here instead.
                    if stream.authentication == StreamAuthentication::Dialback {
                        self.decoder.end_pre_authentication();
                    }
                    out.push(Output::Established(stream));
                }
                Step::Deliver(stanza) => out.push(Output::Stanza(stanza)),
                Step::VerifyDialback(request) => out.push(Output::VerifyDialback(request)),
                Step::VerifyVerdict(verdict) => out.push(Output::VerifyVerdict(verdict)),
                Step::DialbackAnswered { valid } => out.push(Output::DialbackAnswered { valid }),
                Step::Close(reason) => self.close(reason, out),
            }
        }
        if premature && !self.closed {
            self.apply(NegotiationInput::PrematureInput, out);
        }
    }

    fn close(&mut self, reason: CloseReason, out: &mut Vec<Output>) {
        if let Some(mut encoder) = self.encoder.take() {
            let bytes = match reason.stream_error() {
                Some(condition) => encoder.stream_error(condition),
                None => encoder.close(),
            };
            if let Ok(bytes) = bytes {
                transmit(out, bytes);
            }
        }
        self.closed = true;
        out.push(Output::Close(reason));
    }

    fn local_failure(&mut self, out: &mut Vec<Output>) {
        self.encoder = None;
        self.closed = true;
        out.push(Output::Close(CloseReason::LocalFailure));
    }
}

/// Append bytes, merging with a directly preceding transmission.
fn transmit(out: &mut Vec<Output>, bytes: Vec<u8>) {
    if let Some(Output::Transmit(previous)) = out.last_mut() {
        previous.extend(bytes);
    } else {
        out.push(Output::Transmit(bytes));
    }
}
