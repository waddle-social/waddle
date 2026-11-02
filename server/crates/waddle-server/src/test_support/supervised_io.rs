//! Test-only access to the production crate's isolated transport pump core.

pub use crate::server::routes::websocket::supervised_io::{
    spawn_io_custodian, spawn_supervised_io, AdmissionError, CustodianState, InboundExit,
    IoCustodianHandle, IoCustodianObserver, IoSupervisor, Joined, Limits, LimitsError,
    OutboundExit, QueueItem, RetirementMode, ShutdownProtocol, StopReason, StopSignal,
    TransportLimits, WriteProgress, WriteReceipt, WriteSender,
};
