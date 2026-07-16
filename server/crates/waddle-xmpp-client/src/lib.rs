//! Native XMPP client scaffold for Waddle.

pub mod avatar;
pub mod bootstrap;
pub mod caps;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod client;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod command;
pub mod config;
pub mod discovery;
pub mod error;
pub mod event;
pub mod inbox;
pub mod mam;
pub mod mds;
pub mod messaging;
pub mod notification_settings;
pub mod pep;
pub mod pin;
pub mod pubsub_event;
pub mod push;
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
    build_muc_admin_affiliation_list_iq, build_muc_admin_affiliation_set_iq, build_roster_get_iq,
    build_user_search_form_iq, build_user_search_iq, build_waddle_inbox_mark_read_iq,
    parse_muc_admin_affiliation_query, parse_roster_result, parse_user_search_form,
    parse_user_search_result, DiscoFeature, DiscoIdentity, DiscoInfoResult, DiscoItem,
    DiscoveredChannel, MucAdminAffiliationItem, RosterResult, UploadSlot, UserSearchForm,
    UserSearchItem, UserSearchQuery, UserSearchResult, WaddleInboxMarkRead, MUC_ADMIN_NS,
    USER_SEARCH_NS, WADDLE_INBOX_NS,
};
pub use error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
pub use event::{
    ClientEvent, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent, StreamErrorCondition,
    StreamErrorDetail, StreamManagementEvent,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use mam::MamExt;
pub use mam::{
    build_mam_iq, ArchivedMessage, ArchivedPayload, MamIqBuilder, MamPage, RsmPageInfo,
    MAM_END_FIELD, MAM_START_FIELD,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use messaging::MessagingExt;
pub use messaging::{
    build_chat_state_message, build_correction_message, build_displayed_message,
    build_moderation_message, build_outbound_message, build_pinned_chat_message,
    build_reaction_message, build_retraction_message, build_unpinned_chat_message,
    parse_chat_state_payload, parse_correction_payload, parse_displayed_marker_payload,
    parse_moderation_payload, parse_reaction_payload, parse_retraction_payload, ChatStatePayload,
    CorrectionPayload, DisplayedMarkerPayload, InboundMessage, InboundPresence, MessagingEvent,
    ModerationPayload, ReactionPayload, RetractionPayload, NS_CHAT_MARKERS, NS_CHAT_STATES,
    NS_MESSAGE_CORRECT, NS_MESSAGE_MODERATE, NS_MESSAGE_RETRACT, NS_REACTIONS,
};
pub use messaging::{MarkupSpan, MarkupSpanType};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use pep::PepExt;
pub use pep::{
    build_pep_items_iq, build_publish_activity_iq, build_publish_mood_iq, build_publish_tune_iq,
    build_retract_activity_iq, build_retract_mood_iq, build_retract_tune_iq, parse_pep_activity,
    parse_pep_mood, parse_pep_tune, PepItem, UserActivity, UserMood, UserTune, NS_ACTIVITY,
    NS_MOOD, NS_TUNE,
};
pub use pubsub_event::{
    AttachmentSummaryEvent, PubsubEvent, PubsubEventItem, PubsubEventPayload, ReactionSummaryEvent,
};
pub use request::{
    ClientRequest, PendingRequest, RequestCorrelation, RequestId, RequestKind, RequestTracker,
    StanzaId,
};
pub use runtime::{RuntimeStatus, XmppRuntime};
pub use state::{ClientState, SessionBinding, SessionPhase, SessionSnapshot, StreamId};
pub use stream_management::{SmResumeEntry, SmResumeState};
pub use transport::{
    decode_message, encode_message, StreamClose, StreamOpen, TransportCapabilities, TransportEvent,
    TransportKind, TransportMessage, TransportState,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use transport::{DefaultTransportFactory, WebSocketTransport, WebSocketTransportFactory};
#[cfg(feature = "wasm")]
pub use transport_wasm::{WasmTransportEvent, WasmWebSocket};
pub use waddle_xmpp_core::roster::{AskType, RosterItem, Subscription, ROSTER_NS};
pub use waddle_xmpp_core::{
    mam::{FULLTEXT_MAM_FIELD, WADDLE_MAM_THREAD_FIELD},
    ConnectionConfig,
};
pub use xep::xep0292::{
    build_fetch_vcard4_iq, build_publish_vcard4_iq, build_vcard4_element, parse_pep_vcard4,
    parse_vcard4_element, VCard4, NS_VCARD4, PEP_NODE_VCARD4,
};
