//! Typed Phase-A rejection reasons; no shadow decision markers are emitted.
use waddle_xmpp::ingress::FrozenStanzaError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizationDeniedReason {
    BlockedSender,
    Forbidden,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticMalformedReason {
    ClientAuthoredFrameworkEnvelope,
    ClientAuthoredInboxPayload,
    MalformedPayload,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDeniedReason {
    OperationalFenceLoss,
    StanzaError(FrozenStanzaError),
    CaptureOverflow,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanRejection {
    MissingRoomStanzaId,
    AuthorizationDenied(AuthorizationDeniedReason),
    SemanticMalformed(SemanticMalformedReason),
    PolicyDenied(PolicyDeniedReason),
}
