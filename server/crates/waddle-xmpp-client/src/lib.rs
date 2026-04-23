//! Native XMPP client scaffold for Waddle.

pub mod avatar;
pub mod bootstrap;
pub mod client;
pub mod command;
pub mod config;
pub mod discovery;
pub mod error;
pub mod event;
pub mod mam;
pub mod messaging;
pub mod pep;
pub mod push;
pub mod request;
pub mod runtime;
pub mod state;
pub mod stream_management;
pub mod transport;
pub mod xep;

pub use avatar::{Avatar, AvatarExt, AvatarInfo};
pub use bootstrap::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, OAuthBearerRequest,
    RequiredStreamFeature, ResourceBindingRequest, ResourceBindingResult, SaslFailure,
    SaslFailureCondition, StreamFeatures, NS_BIND, NS_CLIENT, NS_SASL, NS_SESSION, NS_SM,
    NS_STREAMS,
};
pub use client::{ClientDriver, ClientHandle, XmppClient};
pub use command::XmppCommand;
pub use config::{
    AccessToken, AuthenticationConfig, ClientConfig, ClientResource, OAuthBearerConfig,
    SessionConfig, StreamManagementConfig, WebSocketConfig,
};
pub use error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
pub use event::{ClientEvent, ConnectionEvent, LifecycleEvent, StreamManagementEvent};
pub use mam::{ArchivedMessage, MamExt, MamPage, RsmPageInfo};
pub use messaging::{InboundMessage, InboundPresence, MessagingEvent, MessagingExt};
pub use pep::{PepExt, PepItem, UserActivity, UserMood, UserTune};
pub use push::PushExt;
pub use request::{
    ClientRequest, PendingRequest, RequestCorrelation, RequestId, RequestKind, RequestTracker,
    StanzaId,
};
pub use runtime::{RuntimeStatus, XmppRuntime};
pub use state::{ClientState, SessionBinding, SessionPhase, SessionSnapshot, StreamId};
pub use transport::{
    DefaultTransportFactory, StreamClose, StreamOpen, TransportCapabilities, TransportEvent,
    TransportKind, TransportMessage, TransportState, WebSocketTransport, WebSocketTransportFactory,
};
pub use waddle_xmpp_core::ConnectionConfig;
