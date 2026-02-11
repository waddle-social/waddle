pub mod carbons;
pub mod connection;
pub mod csi;
pub mod error;
pub mod outbound;
pub mod pipeline;
pub mod processors;
pub mod sasl;
pub mod stanza;
pub mod stream_management;
pub mod transport;

pub use carbons::{CarbonDirection, CarbonsManager, CarbonsState, UnwrappedCarbon};
pub use connection::{ConnectionConfig, ConnectionManager, ConnectionState};
pub use csi::{ClientState, CsiManager};
pub use error::{ConnectionError, PipelineError};
pub use outbound::{OutboundRouter, OutboundRouterError};
pub use pipeline::{
    ProcessorContext, ProcessorResult, StanzaDirection, StanzaPipeline, StanzaProcessor,
};
#[cfg(debug_assertions)]
pub use processors::DebugProcessor;
pub use processors::{
    ChatStateProcessor, MamProcessor, MessageProcessor, MucProcessor, PresenceProcessor,
    RosterProcessor,
};
pub use sasl::SelectedMechanism;
pub use stanza::{Stanza, parse_stanza, serialize_stanza};
pub use stream_management::{
    StreamManagementAction, StreamManagementState, StreamManager, decode_nonza, encode_nonza,
};
pub use transport::XmppTransport;
