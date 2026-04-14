use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

pub mod webrtc_rs_sfu;

pub use webrtc_rs_sfu::WebrtcRsSfuBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MediaBackendKind {
    #[default]
    Disabled,
    WebrtcRsSfu,
}

impl fmt::Display for MediaBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaBackendKind::Disabled => write!(f, "disabled"),
            MediaBackendKind::WebrtcRsSfu => write!(f, "webrtc-rs-sfu"),
        }
    }
}

impl FromStr for MediaBackendKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "disabled" | "none" => Ok(MediaBackendKind::Disabled),
            "webrtc-rs-sfu" | "webrtcrs-sfu" | "webrtc_sfu" => Ok(MediaBackendKind::WebrtcRsSfu),
            other => Err(format!("unsupported media backend: {}", other)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaConfig {
    pub backend: MediaBackendKind,
    pub public_base_url: String,
    pub webrtc_rs_sfu: WebrtcRsSfuConfig,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            backend: MediaBackendKind::Disabled,
            public_base_url: "http://localhost:3000".to_string(),
            webrtc_rs_sfu: WebrtcRsSfuConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebrtcRsSfuConfig {
    pub signaling_path: String,
    pub room_prefix: String,
    pub ice_servers: Vec<String>,
}

impl Default for WebrtcRsSfuConfig {
    fn default() -> Self {
        Self {
            signaling_path: "/v1/media/sfu".to_string(),
            room_prefix: "waddle".to_string(),
            ice_servers: vec!["stun:stun.l.google.com:19302".to_string()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaSessionRequest {
    pub room_id: String,
    pub participant_id: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaSession {
    pub backend: String,
    pub session_id: String,
    pub room_id: String,
    pub participant_id: String,
    pub role: String,
    pub join_url: String,
    pub ice_servers: Vec<String>,
}

#[derive(Debug, Error)]
pub enum MediaBackendError {
    #[error("media backend is disabled")]
    Disabled,
    #[error("invalid media request: {0}")]
    InvalidRequest(String),
}

pub trait MediaBackend: Send + Sync {
    fn kind(&self) -> MediaBackendKind;
    fn create_session(
        &self,
        request: MediaSessionRequest,
    ) -> Result<MediaSession, MediaBackendError>;
}

#[derive(Debug)]
struct DisabledMediaBackend;

impl MediaBackend for DisabledMediaBackend {
    fn kind(&self) -> MediaBackendKind {
        MediaBackendKind::Disabled
    }

    fn create_session(
        &self,
        _request: MediaSessionRequest,
    ) -> Result<MediaSession, MediaBackendError> {
        Err(MediaBackendError::Disabled)
    }
}

pub fn build_media_backend(config: &MediaConfig) -> Arc<dyn MediaBackend> {
    match config.backend {
        MediaBackendKind::Disabled => Arc::new(DisabledMediaBackend),
        MediaBackendKind::WebrtcRsSfu => Arc::new(WebrtcRsSfuBackend::new(config.clone())),
    }
}

pub fn next_media_session_id() -> String {
    Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_media_backend_kind_aliases() {
        assert_eq!(
            "webrtcrs-sfu".parse::<MediaBackendKind>().unwrap(),
            MediaBackendKind::WebrtcRsSfu
        );
        assert_eq!(
            "webrtc-rs-sfu".parse::<MediaBackendKind>().unwrap(),
            MediaBackendKind::WebrtcRsSfu
        );
        assert_eq!(
            "disabled".parse::<MediaBackendKind>().unwrap(),
            MediaBackendKind::Disabled
        );
    }

    #[test]
    fn disabled_backend_rejects_sessions() {
        let config = MediaConfig::default();
        let backend = build_media_backend(&config);
        let result = backend.create_session(MediaSessionRequest {
            room_id: "room-1".to_string(),
            participant_id: "user-1".to_string(),
            role: "publisher".to_string(),
        });

        assert!(matches!(result, Err(MediaBackendError::Disabled)));
    }
}
