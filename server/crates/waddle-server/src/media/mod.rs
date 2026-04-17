use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;

pub mod livekit;

pub use livekit::LivekitBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MediaBackendKind {
    #[default]
    Disabled,
    Livekit,
}

impl fmt::Display for MediaBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaBackendKind::Disabled => write!(f, "disabled"),
            MediaBackendKind::Livekit => write!(f, "livekit"),
        }
    }
}

impl FromStr for MediaBackendKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "disabled" | "none" => Ok(MediaBackendKind::Disabled),
            "livekit" => Ok(MediaBackendKind::Livekit),
            other => Err(format!("unsupported media backend: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaConfig {
    pub backend: MediaBackendKind,
    pub livekit: LivekitConfig,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            backend: MediaBackendKind::Disabled,
            livekit: LivekitConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LivekitConfig {
    pub url: String,
    pub api_key: String,
    pub api_secret: String,
    pub room_prefix: String,
    pub token_ttl_secs: i64,
}

impl Default for LivekitConfig {
    fn default() -> Self {
        Self {
            url: "ws://localhost:7880".to_string(),
            api_key: String::new(),
            api_secret: String::new(),
            room_prefix: "waddle".to_string(),
            token_ttl_secs: 3600,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Audio,
    Video,
}

impl MediaType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MediaType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "audio" => Ok(Self::Audio),
            "video" => Ok(Self::Video),
            other => Err(format!("unsupported media type: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaSessionRequest {
    pub waddle_id: String,
    pub channel_id: String,
    pub participant_id: String,
    pub participant_name: String,
    pub media_type: MediaType,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaSession {
    pub backend: String,
    pub room_name: String,
    pub participant_id: String,
    pub participant_name: String,
    pub media_type: MediaType,
    pub server_url: String,
    pub token: String,
    pub expires_at: String,
    pub can_publish: bool,
    pub can_publish_data: bool,
    pub can_subscribe: bool,
}

#[derive(Debug, Error)]
pub enum MediaBackendError {
    #[error("media backend is disabled")]
    Disabled,
    #[error("invalid media request: {0}")]
    InvalidRequest(String),
    #[error("media backend is misconfigured: {0}")]
    Misconfigured(String),
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
        MediaBackendKind::Livekit => Arc::new(LivekitBackend::new(config.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_media_backend_kind_aliases() {
        assert_eq!(
            "livekit".parse::<MediaBackendKind>().unwrap(),
            MediaBackendKind::Livekit
        );
        assert_eq!(
            "disabled".parse::<MediaBackendKind>().unwrap(),
            MediaBackendKind::Disabled
        );
    }

    #[test]
    fn parses_media_types() {
        assert_eq!("audio".parse::<MediaType>().unwrap(), MediaType::Audio);
        assert_eq!("video".parse::<MediaType>().unwrap(), MediaType::Video);
    }

    #[test]
    fn disabled_backend_rejects_sessions() {
        let config = MediaConfig::default();
        let backend = build_media_backend(&config);
        let result = backend.create_session(MediaSessionRequest {
            waddle_id: "waddle-1".to_string(),
            channel_id: "channel-1".to_string(),
            participant_id: "user-1".to_string(),
            participant_name: "alice".to_string(),
            media_type: MediaType::Video,
        });

        assert!(matches!(result, Err(MediaBackendError::Disabled)));
    }
}
