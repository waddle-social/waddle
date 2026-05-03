//! Native XMPP client scaffold for Waddle.

#[cfg(feature = "native")]
pub mod avatar;
pub mod bootstrap;
#[cfg(feature = "native")]
pub mod client;
#[cfg(feature = "native")]
pub mod command;
pub mod config;
#[cfg(feature = "native")]
pub mod discovery;
pub mod error;
pub mod event;
pub mod mam;
pub mod messaging;
pub mod pep;
pub mod request;
pub mod runtime;
pub mod state;
pub mod stream_management;
pub mod transport;
pub mod xep;

#[cfg(feature = "native")]
pub use avatar::{Avatar, AvatarExt, AvatarInfo};
pub use bootstrap::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, OAuthBearerRequest,
    RequiredStreamFeature, ResourceBindingRequest, ResourceBindingResult, SaslFailure,
    SaslFailureCondition, StreamFeatures, NS_BIND, NS_CLIENT, NS_SASL, NS_SESSION, NS_SM,
    NS_STREAMS,
};
#[cfg(feature = "native")]
pub use client::{ClientDriver, ClientHandle, XmppClient};
#[cfg(feature = "native")]
pub use command::XmppCommand;
pub use config::{
    AccessToken, AuthenticationConfig, ClientConfig, ClientResource, OAuthBearerConfig,
    SessionConfig, StreamManagementConfig, WebSocketConfig,
};
#[cfg(feature = "native")]
pub use discovery::{
    DiscoFeature, DiscoIdentity, DiscoInfoResult, DiscoItem, DiscoveredChannel, DiscoveredWaddle,
    DiscoveryExt, InboxEntry, UploadSlot,
};
pub use error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
pub use event::{
    ClientEvent, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent, StreamManagementEvent,
};
#[cfg(feature = "native")]
pub use mam::MamExt;
pub use mam::{ArchivedMessage, MamPage, RsmPageInfo};
#[cfg(feature = "native")]
pub use messaging::MessagingExt;
pub use messaging::{InboundMessage, InboundPresence, MessagingEvent};
#[cfg(feature = "native")]
pub use pep::PepExt;
pub use pep::{PepItem, UserActivity, UserMood, UserTune};
pub use request::{
    ClientRequest, PendingRequest, RequestCorrelation, RequestId, RequestKind, RequestTracker,
    StanzaId,
};
pub use runtime::{RuntimeStatus, XmppRuntime};
pub use state::{ClientState, SessionBinding, SessionPhase, SessionSnapshot, StreamId};
pub use transport::{
    decode_message, encode_message, StreamClose, StreamOpen, TransportCapabilities, TransportEvent,
    TransportKind, TransportMessage, TransportState,
};
#[cfg(feature = "native")]
pub use transport::{DefaultTransportFactory, WebSocketTransport, WebSocketTransportFactory};
pub use waddle_xmpp_core::ConnectionConfig;
