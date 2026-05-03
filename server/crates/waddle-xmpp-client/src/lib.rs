//! Native XMPP client scaffold for Waddle.

pub mod avatar;
pub mod bootstrap;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod client;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod command;
pub mod config;
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
#[cfg(feature = "wasm")]
pub mod transport_wasm;
pub mod xep;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use avatar::AvatarExt;
pub use avatar::{Avatar, AvatarInfo};
pub use bootstrap::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, OAuthBearerRequest,
    RequiredStreamFeature, ResourceBindingRequest, ResourceBindingResult, SaslFailure,
    SaslFailureCondition, StreamFeatures, NS_BIND, NS_CLIENT, NS_SASL, NS_SESSION, NS_SM,
    NS_STREAMS,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use client::{ClientDriver, ClientHandle, XmppClient};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use command::XmppCommand;
pub use config::{
    AccessToken, AuthenticationConfig, ClientConfig, ClientResource, OAuthBearerConfig,
    SessionConfig, StreamManagementConfig, WebSocketConfig,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use discovery::DiscoveryExt;
pub use discovery::{
    DiscoFeature, DiscoIdentity, DiscoInfoResult, DiscoItem, DiscoveredChannel, DiscoveredWaddle,
    InboxEntry, UploadSlot,
};
pub use error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
pub use event::{
    ClientEvent, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent, StreamManagementEvent,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use mam::MamExt;
pub use mam::{ArchivedMessage, MamPage, RsmPageInfo};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use messaging::MessagingExt;
pub use messaging::{InboundMessage, InboundPresence, MessagingEvent};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
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
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use transport::{DefaultTransportFactory, WebSocketTransport, WebSocketTransportFactory};
#[cfg(feature = "wasm")]
pub use transport_wasm::{WasmTransportEvent, WasmWebSocket};
pub use waddle_xmpp_core::ConnectionConfig;
