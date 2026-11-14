//! I/O-agnostic S2S connection driver combining codec and negotiation.

use super::decoder::{Inbound, StreamDecoder, TlsHandoff};
use super::encoder::StreamEncoder;
use super::limits::CodecLimits;
use super::negotiation::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, ExchangeId, InitiatingConfig,
    NegotiatedStream, Negotiation, NegotiationInput, NegotiationPhase, ReceivingConfig, Role,
    Signal, Step, TlsPeer,
};

/// What the connection owner must do next, in order.
#[derive(Debug, PartialEq)]
pub enum Output {
    /// Write these bytes to the current transport (TCP before STARTTLS, the
    /// TLS session afterwards).
    Transmit(Vec<u8>),
    /// Start TLS after writing all earlier output. The handoff holds bytes
    /// already received after the boundary element; feed them to the TLS
    /// engine before reading the socket again.
    StartTls(TlsHandoff),
    /// Decide SASL EXTERNAL authorization and answer with
    /// [`S2sStream::authorization`].
    Authorize(AuthorizationRequest),
    /// Negotiation completed for this directional domain pair.
    Established(NegotiatedStream),
    /// Close the transport after writing all earlier output.
    Close(CloseReason),
}

/// One S2S connection's XML codec and negotiation, without sockets or TLS.
///
/// The connection owner feeds received plaintext with [`Self::receive`],
/// reports TLS completion and authorization decisions, and executes the
/// returned [`Output`]s in order.
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
            decoder: StreamDecoder::new(limits),
            encoder: None,
            negotiation: Negotiation::receiving(config),
            closed: false,
        }
    }

    /// Start an outbound connection; the output opens the initial stream.
    pub fn initiate(config: InitiatingConfig, limits: CodecLimits) -> (Self, Vec<Output>) {
        let (negotiation, steps) = Negotiation::initiating(config);
        let mut stream = Self {
            decoder: StreamDecoder::new(limits),
            encoder: None,
            negotiation,
            closed: false,
        };
        let mut out = Vec::new();
        stream.run(steps, &mut out);
        (stream, out)
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
                Ok(Some(Inbound::Element(element))) => NegotiationInput::Element(element),
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
                Step::Established(stream) => out.push(Output::Established(stream)),
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
