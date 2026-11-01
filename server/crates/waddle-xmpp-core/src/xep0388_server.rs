//! Dark, sans-I/O XEP-0388 server exchange for OAUTHBEARER.
//!
//! Nothing in this module advertises or dispatches SASL2, validates a token,
//! establishes TLS, or changes a live connection or session. The eventual
//! server adapter must establish trusted TLS without early data, gate *all*
//! traffic during an exchange, validate the bearer credential and its GS2
//! authorization identity, stream `from`, and known endpoint, and only then
//! supply a verifier assertion. Its success action is a wire value, not an
//! authentication authority. The adapter must immediately follow a success
//! with the post-authentication stream features on the same stream.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;

use crate::xep0388::{
    Sasl2Authenticate, Sasl2Challenge, Sasl2Data, Sasl2Failure, Sasl2FailureCondition,
    Sasl2Mechanism, Sasl2Response, Sasl2Success, NS_SASL2,
};

/// An already-classified input to the dark exchange.
///
/// A transport adapter must classify every received frame, not just known
/// SASL2 frames. A malformed, foreign, or otherwise unexpected frame is
/// [`Self::Other`]. `Abort` must only represent a SASL2 `<abort/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sasl2ServerInput {
    /// A parsed SASL2 authentication request.
    Authenticate(Sasl2Authenticate),
    /// A parsed SASL2 response.
    Response(Sasl2Response),
    /// A SASL2 abort.
    Abort,
    /// Any other traffic while the exchange is in progress.
    Other,
}

/// Credential provenance asserted by the eventual server verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sasl2VerifierProvenance {
    /// The verifier asserts that it validated OAUTHBEARER and the applicable
    /// identity and endpoint constraints for this exchange.
    OAuthBearer,
}

/// A verifier's assertion; this value is not cryptographic proof.
///
/// The caller is responsible for verifying the token, GS2 authzid, stream
/// `from`, and known endpoint before making this assertion. The core cannot
/// prove that those checks, TLS, or the absence of early data took place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sasl2VerifierAssertion {
    /// Bare JID the verifier asserts is authorized by the bearer credential.
    pub bare_jid: BareJid,
    /// The asserted credential provenance.
    pub provenance: Sasl2VerifierProvenance,
}

/// Result supplied by an external OAUTHBEARER verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sasl2Verification {
    /// The verifier asserts that a particular bare identity is authorized.
    Authorized(Sasl2VerifierAssertion),
    /// The verifier rejected the bearer credential.
    InvalidToken,
}

/// A typed SASL2 frame produced by the exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sasl2ServerFrame {
    /// SASL2 challenge, including an RFC 7628 error challenge when required.
    Challenge(Sasl2Challenge),
    /// Terminal SASL2 failure.
    Failure(Sasl2Failure),
    /// Terminal SASL2 success; this does not itself authorize a connection.
    Success(Sasl2Success),
}

/// An effect for the transport or external credential verifier to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sasl2ServerAction {
    /// Send the typed SASL2 frame.
    Send(Sasl2ServerFrame),
    /// Validate the mechanism payload using the trusted server verifier.
    VerifyOAuthBearer(Sasl2Data),
    /// Send this stream error for a new authentication after success, then close.
    StreamError(Element),
    /// Immediately disconnect because unexpected traffic interrupted SASL2.
    Disconnect,
}

/// An input arrived after the one-shot exchange ended or at the wrong stage.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum Sasl2ServerError {
    /// No more terminal outcome can be produced by this exchange.
    #[error("SASL2 exchange has already ended")]
    AlreadyFinished,
    /// A verifier result was supplied without an outstanding validation.
    #[error("SASL2 exchange is not awaiting a verifier result")]
    NotAwaitingVerification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    AwaitAuthenticate,
    AwaitInitialResponse,
    AwaitVerification,
    AwaitErrorAcknowledgement,
    Succeeded,
    Finished,
}

/// One non-inline SASL2 OAUTHBEARER exchange, with no I/O or session authority.
#[derive(Debug)]
pub struct Sasl2ServerExchange {
    stage: Stage,
}

impl Default for Sasl2ServerExchange {
    fn default() -> Self {
        Self::new()
    }
}

impl Sasl2ServerExchange {
    /// Start a fresh, one-shot exchange. A retry needs a new instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stage: Stage::AwaitAuthenticate,
        }
    }

    /// Process one classified client input.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2ServerError::AlreadyFinished`] once the exchange has
    /// ended. A second authentication after success instead emits a stream
    /// error action once, as required by XEP-0388.
    pub fn on_input(
        &mut self,
        input: Sasl2ServerInput,
    ) -> Result<Sasl2ServerAction, Sasl2ServerError> {
        match (self.stage, input) {
            (Stage::AwaitAuthenticate, Sasl2ServerInput::Authenticate(request)) => {
                Ok(self.start(request))
            }
            (Stage::AwaitInitialResponse, Sasl2ServerInput::Response(response)) => {
                self.stage = Stage::AwaitVerification;
                Ok(Sasl2ServerAction::VerifyOAuthBearer(response.data))
            }
            (Stage::AwaitErrorAcknowledgement, Sasl2ServerInput::Response(response)) => {
                let condition = if response.data.as_bytes() == [0x01] {
                    Sasl2FailureCondition::NotAuthorized
                } else {
                    Sasl2FailureCondition::MalformedRequest
                };
                Ok(self.failure(condition))
            }
            (
                Stage::AwaitInitialResponse
                | Stage::AwaitVerification
                | Stage::AwaitErrorAcknowledgement,
                Sasl2ServerInput::Abort,
            ) => Ok(self.failure(Sasl2FailureCondition::Aborted)),
            (Stage::Succeeded, Sasl2ServerInput::Authenticate(_)) => {
                self.stage = Stage::Finished;
                Ok(Sasl2ServerAction::StreamError(
                    Element::builder("error", xmpp_parsers::ns::STREAM)
                        .append(
                            Element::builder("not-authorized", xmpp_parsers::ns::XMPP_STREAMS)
                                .build(),
                        )
                        .build(),
                ))
            }
            (Stage::Succeeded | Stage::Finished, _) => Err(Sasl2ServerError::AlreadyFinished),
            _ => {
                self.stage = Stage::Finished;
                Ok(Sasl2ServerAction::Disconnect)
            }
        }
    }

    /// Complete an outstanding external credential-verification request.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2ServerError::NotAwaitingVerification`] unless the most
    /// recent action requested verification. A late result cannot authenticate
    /// an aborted, failed, or disconnected exchange.
    pub fn on_verification(
        &mut self,
        result: Sasl2Verification,
    ) -> Result<Sasl2ServerAction, Sasl2ServerError> {
        if self.stage != Stage::AwaitVerification {
            return Err(Sasl2ServerError::NotAwaitingVerification);
        }
        match result {
            Sasl2Verification::Authorized(assertion) => {
                let Sasl2VerifierAssertion {
                    bare_jid,
                    provenance: Sasl2VerifierProvenance::OAuthBearer,
                } = assertion;
                self.stage = Stage::Succeeded;
                Ok(Sasl2ServerAction::Send(Sasl2ServerFrame::Success(
                    Sasl2Success {
                        additional_data: None,
                        authorization_identifier: bare_jid.into(),
                        inline_results: Vec::new(),
                    },
                )))
            }
            Sasl2Verification::InvalidToken => {
                self.stage = Stage::AwaitErrorAcknowledgement;
                Ok(Sasl2ServerAction::Send(Sasl2ServerFrame::Challenge(
                    Sasl2Challenge {
                        data: Sasl2Data::new(br#"{"status":"invalid_token"}"#),
                    },
                )))
            }
        }
    }

    fn start(&mut self, request: Sasl2Authenticate) -> Sasl2ServerAction {
        if request.mechanism != Sasl2Mechanism::OAuthBearer {
            return self.failure(Sasl2FailureCondition::InvalidMechanism);
        }
        if request
            .additional_children()
            .iter()
            .any(|child| !child.is("user-agent", NS_SASL2))
        {
            return self.failure(Sasl2FailureCondition::MalformedRequest);
        }
        if let Some(initial_response) = request.initial_response {
            self.stage = Stage::AwaitVerification;
            Sasl2ServerAction::VerifyOAuthBearer(initial_response)
        } else {
            self.stage = Stage::AwaitInitialResponse;
            Sasl2ServerAction::Send(Sasl2ServerFrame::Challenge(Sasl2Challenge {
                data: Sasl2Data::new([]),
            }))
        }
    }

    fn failure(&mut self, condition: Sasl2FailureCondition) -> Sasl2ServerAction {
        self.stage = Stage::Finished;
        Sasl2ServerAction::Send(Sasl2ServerFrame::Failure(Sasl2Failure { condition }))
    }
}
