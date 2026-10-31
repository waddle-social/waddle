//! LiveKit SFU bridge for Waddle's XMPP-native A/V calling.
//!
//! This crate is the *only* place where LiveKit-specific concerns
//! live. Its public API speaks typed Waddle values
//! ([`CallId`], [`Identity`], [`JoinToken`], [`TurnCredential`]) so the
//! XMPP layer can stay LiveKit-agnostic.
//!
//! Two layers are exposed:
//!
//! 1. The [`SfuService`] trait — the abstract surface consumed by
//!    [`waddle_xmpp`]'s Jingle / `extdisco:2` handlers. It is sync
//!    except for an explicit preparation seam that discovers connected media
//!    before Muji authorization. Publication itself uses a synchronous CAS.
//! 2. The [`LiveKitSfu`] impl — the concrete bridge that signs JWTs
//!    against the `livekit-sfu-api-keys` Secret and tracks MUC focus
//!    state in-memory.
//!
//! See `server/crates/waddle-xmpp/src/protocol/handlers/jingle.rs` for
//! the consumer.

#![deny(unsafe_code)]

use std::future::Future;
use std::pin::Pin;
use waddle_xmpp_core::OccupancySessionGeneration;

mod admin;
mod call;
mod config;
mod correlation;
mod error;
mod livekit;
mod token;
mod turn;
mod webhook;

pub use admin::{
    AdminCallObserver, AdminOp, ListedRoom, ListedRoomName, LiveKitAdmin, LiveParticipant,
    RoomOccupancy,
};
pub use call::{
    CallGeneration, CallId, CallState, CallTeardownIntentLite, Identity, MediaCapabilities,
    ObservedCallSids, ParticipantAttributes, ParticipantSid, RoomSid, SessionBinding,
    SessionScopedTeardown, SidObservationDirection, SidObservationDisposition, TeardownDisposition,
    TeardownTargetLite,
};
pub use config::{ApiKey, ApiSecret, FromEnvError, SfuConfig, TurnSharedSecret, WebsocketUrl};
pub use correlation::{CallCorrelationId, CORRELATION_ID_HEX_LEN};
pub use error::SfuError;
pub use livekit::{
    LiveKitSfu, LiveKitTeardownExecutor, TeardownExecution, TeardownFailureSink,
    RECONCILE_CONCURRENCY, RECONCILE_GRACE_SECONDS,
};
pub use token::{JoinToken, Jti, Jwt, VideoGrant};
pub use turn::{TurnCredential, TurnHost, TurnPassword, TurnUsername};
pub use webhook::{
    verify_webhook_signature, LiveKitWebhookEvent, ParticipantEnvelope, ParticipantInfo,
    RoomEnvelope, RoomInfo, WebhookVerifyError,
};

mod service;
pub use service::LiveParticipantsFuture;
pub use service::ParticipantPreparationError;
pub use service::ParticipantPreparationFuture;
pub use service::ParticipantRegistrationExpectation;
pub use service::PendingMediaRetirement;
pub use service::ReconcileFuture;
pub use service::ReconcilePassSummary;
pub use service::SfuReconciler;
pub use service::SfuService;
pub use service::SidEvidence;
pub use service::UnboundOccupantPolicy;
