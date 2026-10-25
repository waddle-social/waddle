//! Transport-independent authentication, session, request, and stream-management coordination.

use crate::bootstrap::NS_STREAMS;
use crate::event::SmAckRequestReason;
use crate::request::RequestId;
use crate::request::RequestKind;
use crate::stream_management::NS_SM;
use crate::OAuthBearerConfig;
use crate::ResourceBindingResult;
use crate::SaslFailure;
use crate::StreamFeatures;
use std::collections::VecDeque;

use crate::bootstrap::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, RequiredStreamFeature,
    ResourceBindingRequest,
};
use crate::config::ClientConfig;
use crate::error::{ClientError, ClientResult};
use crate::event::{
    ClientEvent, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent, StreamManagementEvent,
};
use crate::request::{ClientRequest, PendingRequest, RequestTracker, StanzaId};
use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
use crate::stream_management::{SmAcknowledgementClockAction, SmResumeState, SmState};
use crate::transport::{StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState};
use crate::AuthenticationConfig;
use chrono::{DateTime, Utc};
use minidom::Element;

mod app_stanza;
mod sm;
#[cfg(test)]
mod tests;

mod implementation;
use implementation::BootstrapState;
pub use implementation::PagehideAckRequest;
pub use implementation::RuntimeStatus;
use implementation::SmNegotiationState;
pub use implementation::XmppRuntime;
