pub mod connection;
pub mod error;
pub mod pipeline;
pub mod sasl;
pub mod stream_management;
pub mod transport;

pub use connection::{ConnectionConfig, ConnectionManager, ConnectionState};
pub use error::{ConnectionError, PipelineError};
pub use pipeline::{ProcessorContext, ProcessorResult, StanzaPipeline, StanzaProcessor};
pub use sasl::SelectedMechanism;
pub use stream_management::{
    StreamManagementAction, StreamManagementState, StreamManager, decode_nonza, encode_nonza,
};
pub use transport::XmppTransport;
