//! Role-independent entry point of the negotiation state machine.

use super::initiating::Initiating;
use super::receiving::Receiving;
use super::types::{
    InitiatingConfig, NegotiationInput, NegotiationPhase, ReceivingConfig, Role, Step,
};

/// The S2S negotiation state machine for one TCP connection.
///
/// Both roles follow RFC 6120 with the restricted federation profile:
/// plaintext header → mandatory STARTTLS → TLS → header → SASL EXTERNAL →
/// authenticated header → established. Exactly one TLS restart and one
/// SASL restart can occur because each role's states are strictly ordered.
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

    /// An initiating-role machine and the steps that open its stream.
    pub fn initiating(config: InitiatingConfig) -> (Self, Vec<Step>) {
        let (machine, steps) = Initiating::start(config);
        (
            Self {
                role: RoleMachine::Initiating(machine),
            },
            steps,
        )
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
