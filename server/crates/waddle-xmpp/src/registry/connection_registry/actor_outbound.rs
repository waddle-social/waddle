//! How far one outbound frame has come toward the socket.

/// The write stage of one outbound frame, as the transport reports it. A
/// flushed frame was handed to the socket; nothing here proves the peer read
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorOutboundStage {
    Queued,
    Attempting,
    NotAttempted,
    AttemptedUncertain,
    Flushed,
}
