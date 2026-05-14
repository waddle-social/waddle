//! Typed errors for the SFU bridge.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SfuError {
    #[error("invalid call id: {0}")]
    InvalidCallId(String),

    #[error("failed to sign LiveKit join JWT")]
    JwtSigning(#[source] jsonwebtoken::errors::Error),

    #[error("clock returned a pre-epoch timestamp")]
    Clock,

    #[error("failed to compute HMAC for TURN credentials")]
    HmacInit,
}
