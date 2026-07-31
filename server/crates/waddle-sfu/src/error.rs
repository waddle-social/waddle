//! Typed errors for the SFU bridge.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SfuError {
    #[error("invalid call id: {0}")]
    InvalidCallId(String),

    #[error("invalid LiveKit room sid")]
    InvalidRoomSid,

    #[error("invalid LiveKit participant sid")]
    InvalidParticipantSid,

    #[error("failed to sign LiveKit join JWT")]
    JwtSigning(#[source] jsonwebtoken::errors::Error),

    #[error("clock returned a pre-epoch timestamp")]
    Clock,

    #[error("failed to compute HMAC for TURN credentials")]
    HmacInit,

    #[error("failed to build LiveKit admin HTTP client")]
    AdminHttpInit(#[source] reqwest::Error),

    #[error("failed to build LiveKit admin URL")]
    AdminUrl(#[source] url::ParseError),

    #[error("unexpected LiveKit websocket URL scheme for admin REST derivation: {0}")]
    AdminScheme(String),

    #[error("LiveKit admin request failed")]
    AdminRequest(#[source] reqwest::Error),

    #[error("LiveKit admin call returned status {status}: {body}")]
    AdminCallFailed { status: u16, body: String },
}
