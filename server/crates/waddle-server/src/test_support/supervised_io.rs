//! Test-only access to the production crate's isolated transport pump core.

pub use crate::server::routes::websocket::supervised_io::{
    spawn_supervised_io, AdmissionError, InboundExit, IoSupervisor, Joined, Limits, LimitsError,
    OutboundExit, QueueItem, RetirementMode, StopReason, StopSignal, TransportLimits,
    WriteProgress, WriteReceipt, WriteSender,
};
