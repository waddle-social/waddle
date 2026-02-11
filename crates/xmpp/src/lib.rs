pub mod connection;
pub mod error;
pub mod pipeline;
pub mod sasl;
pub mod transport;

pub use connection::{ConnectionConfig, ConnectionManager, ConnectionState};
pub use error::{ConnectionError, PipelineError};
pub use pipeline::{ProcessorContext, ProcessorResult, StanzaPipeline, StanzaProcessor};
pub use sasl::SelectedMechanism;
pub use transport::XmppTransport;
