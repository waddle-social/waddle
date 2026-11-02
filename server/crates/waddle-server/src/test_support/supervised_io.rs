//! Test-only access to the production crate's isolated transport pump core.

pub use crate::server::routes::websocket::supervised_io::{
    spawn_io_custodian, spawn_supervised_io, AdmissionError, CustodianInventoryError,
    CustodianState, CustodianSummary, InboundExit, InboundTerminal, IoCustodianHandle,
    IoCustodianInventory, IoCustodianObserver, IoCustodianReservation, IoSupervisor, Joined,
    Limits, LimitsError, OutboundExit, OutboundTerminal, QueueItem, ReservedIoStartConfig,
    ReservedStartStage, RetirementMode, ShutdownProtocol, StopReason, StopSignal, TransportLimits,
    WriteProgress, WriteReceipt, WriteSender,
};
