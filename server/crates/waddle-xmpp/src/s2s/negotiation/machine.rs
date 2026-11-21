//! Role-independent entry point of the negotiation state machine.

use super::initiating::Initiating;
use super::receiving::Receiving;
use super::types::{
    InitiatingConfig, NegotiationInput, NegotiationPhase, ReceivingConfig, Role, Step, TlsEntry,
    VerifyRequest,
};

/// The S2S negotiation state machine for one TCP connection.
///
/// Both roles follow RFC 6120 with the restricted federation profile:
/// plaintext header → mandatory STARTTLS → TLS → header → SASL EXTERNAL →
/// authenticated header → established. An initiating stream may instead
/// start with XEP-0368 direct TLS, which replaces the plaintext header and
/// STARTTLS. Where SASL EXTERNAL is not possible and the owner enables it,
/// XEP-0220 dialback over TLS replaces SASL and the authenticated restart.
/// At most one TLS restart and one SASL restart can occur because each
/// role's states are strictly ordered.
/// The machine performs no I/O; it consumes typed inputs and returns
/// [`Step`]s to execute in order.
#[derive(Debug)]
pub struct Negotiation {
    role: RoleMachine,
}

#[derive(Debug)]
enum RoleMachine {
    Receiving(Receiving),
    Initiating(Initiating),
}

impl Negotiation {
    /// A receiving-role machine awaiting the peer's initial stream header.
    pub fn receiving(config: ReceivingConfig) -> Self {
        Self {
            role: RoleMachine::Receiving(Receiving::new(config)),
        }
    }

    /// An initiating-role machine that reaches TLS with STARTTLS, and the
    /// steps that open its stream.
    pub fn initiating(config: InitiatingConfig) -> (Self, Vec<Step>) {
        Self::initiating_with(config, TlsEntry::StartTls)
    }

    /// An initiating-role machine that reaches TLS by `entry`, and the
    /// steps that start it: the plaintext header for STARTTLS, or
    /// [`Step::StartTls`] at once for direct TLS.
    pub fn initiating_with(config: InitiatingConfig, entry: TlsEntry) -> (Self, Vec<Step>) {
        Self::wrap(Initiating::start(config, entry))
    }

    /// A verification connection from `config.source`, as Receiving
    /// Server, to the Authoritative Server of `config.destination`
    /// (XEP-0220 §2.1.2), and the steps that start it.
    pub fn verifying(
        config: InitiatingConfig,
        request: VerifyRequest,
        entry: TlsEntry,
    ) -> (Self, Vec<Step>) {
        Self::wrap(Initiating::verify(config, request, entry))
    }

    fn wrap((machine, steps): (Initiating, Vec<Step>)) -> (Self, Vec<Step>) {
        (
            Self {
                role: RoleMachine::Initiating(machine),
            },
            steps,
        )
    }

    /// Hand stanzas that pass the address checks of an established
    /// receiving stream to the connection owner as [`Step::Deliver`],
    /// instead of refusing them with `policy-violation`. Has no effect in
    /// the initiating role, where streams are directional and every
    /// received stanza is refused.
    pub fn deliver_stanzas(&mut self) {
        if let RoleMachine::Receiving(machine) = &mut self.role {
            machine.deliver_stanzas();
        }
    }

    /// Advance the machine.
    pub fn handle(&mut self, input: NegotiationInput) -> Vec<Step> {
        match &mut self.role {
            RoleMachine::Receiving(machine) => machine.handle(input),
            RoleMachine::Initiating(machine) => machine.handle(input),
        }
    }

    pub fn role(&self) -> Role {
        match self.role {
            RoleMachine::Receiving(_) => Role::Receiving,
            RoleMachine::Initiating(_) => Role::Initiating,
        }
    }

    pub fn phase(&self) -> NegotiationPhase {
        match &self.role {
            RoleMachine::Receiving(machine) => machine.phase(),
            RoleMachine::Initiating(machine) => machine.phase(),
        }
    }
}
