use super::{
    LivekitConfig, MediaBackend, MediaBackendError, MediaBackendKind, MediaConfig, MediaSession,
    MediaSessionRequest,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct LivekitBackend {
    config: MediaConfig,
}

impl LivekitBackend {
    pub fn new(config: MediaConfig) -> Self {
        Self { config }
    }

    fn livekit(&self) -> &LivekitConfig {
        &self.config.livekit
    }

    fn validate_identifier(value: &str, field_name: &str) -> Result<String, MediaBackendError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(MediaBackendError::InvalidRequest(format!(
                "{field_name} cannot be empty"
            )));
        }
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(MediaBackendError::InvalidRequest(format!(
                "{field_name} must only contain ASCII letters, numbers, '-' or '_'"
            )));
        }

        Ok(trimmed.to_owned())
    }

    fn validate_config(&self) -> Result<(), MediaBackendError> {
        if self.livekit().url.trim().is_empty() {
            return Err(MediaBackendError::Misconfigured(
                "WADDLE_MEDIA_LIVEKIT_URL must be set".to_string(),
            ));
        }
        if self.livekit().api_key.trim().is_empty() {
            return Err(MediaBackendError::Misconfigured(
                "WADDLE_MEDIA_LIVEKIT_API_KEY must be set".to_string(),
            ));
        }
        if self.livekit().api_secret.trim().is_empty() {
            return Err(MediaBackendError::Misconfigured(
                "WADDLE_MEDIA_LIVEKIT_API_SECRET must be set".to_string(),
            ));
        }
        Ok(())
    }

    fn room_name(&self, waddle_id: &str, channel_id: &str) -> Result<String, MediaBackendError> {
        let room_prefix = Self::validate_identifier(&self.livekit().room_prefix, "room_prefix")?;
        let waddle_id = Self::validate_identifier(waddle_id, "waddle_id")?;
        let channel_id = Self::validate_identifier(channel_id, "channel_id")?;
        Ok(format!("{room_prefix}-{waddle_id}-{channel_id}"))
    }

    fn create_token(
        &self,
        room_name: &str,
        participant_id: &str,
        participant_name: &str,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<String, MediaBackendError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct VideoGrant<'a> {
            room: &'a str,
            room_join: bool,
            can_publish: bool,
            can_publish_data: bool,
            can_subscribe: bool,
        }

        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            sub: &'a str,
            name: &'a str,
            nbf: i64,
            exp: i64,
            video: VideoGrant<'a>,
        }

        let claims = Claims {
            iss: self.livekit().api_key.as_str(),
            sub: participant_id,
            name: participant_name,
            nbf: Utc::now().timestamp(),
            exp: expires_at.timestamp(),
            video: VideoGrant {
                room: room_name,
                room_join: true,
                can_publish: true,
                can_publish_data: true,
                can_subscribe: true,
            },
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.livekit().api_secret.as_bytes()),
        )
        .map_err(|err| MediaBackendError::Misconfigured(format!("failed to sign LiveKit token: {err}")))
    }
}

impl MediaBackend for LivekitBackend {
    fn kind(&self) -> MediaBackendKind {
        MediaBackendKind::Livekit
    }

    fn create_session(
        &self,
        request: MediaSessionRequest,
    ) -> Result<MediaSession, MediaBackendError> {
        self.validate_config()?;

        let room_name = self.room_name(&request.waddle_id, &request.channel_id)?;
        let participant_id =
            Self::validate_identifier(&request.participant_id, "participant_id")?;
        let participant_name = request.participant_name.trim();
        if participant_name.is_empty() {
            return Err(MediaBackendError::InvalidRequest(
                "participant_name cannot be empty".to_string(),
            ));
        }

        let ttl = self.livekit().token_ttl_secs.max(60);
        let expires_at = Utc::now() + Duration::seconds(ttl);
        let token =
            self.create_token(&room_name, &participant_id, participant_name, expires_at)?;

        Ok(MediaSession {
            backend: self.kind().to_string(),
            room_name,
            participant_id,
            participant_name: participant_name.to_string(),
            media_type: request.media_type,
            server_url: self.livekit().url.clone(),
            token,
            expires_at: expires_at.to_rfc3339(),
            can_publish: true,
            can_publish_data: true,
            can_subscribe: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaBackendKind, MediaType};
    use jsonwebtoken::{decode, DecodingKey, Validation};
    use serde::Deserialize;

    fn configured_backend() -> LivekitBackend {
        let mut config = MediaConfig::default();
        config.backend = MediaBackendKind::Livekit;
        config.livekit.url = "wss://livekit.waddle.test".to_string();
        config.livekit.api_key = "devkey".to_string();
        config.livekit.api_secret = "super-secret".to_string();
        config.livekit.room_prefix = "room".to_string();
        LivekitBackend::new(config)
    }

    #[derive(Debug, Deserialize)]
    struct TokenClaims {
        iss: String,
        sub: String,
        name: String,
        video: serde_json::Value,
    }

    #[test]
    fn creates_livekit_session_payload() {
        let backend = configured_backend();
        let session = backend
            .create_session(MediaSessionRequest {
                waddle_id: "penguins".to_string(),
                channel_id: "general".to_string(),
                participant_id: "user_42".to_string(),
                participant_name: "alice".to_string(),
                media_type: MediaType::Video,
            })
            .unwrap();

        assert_eq!(session.backend, "livekit");
        assert_eq!(session.room_name, "room-penguins-general");
        assert_eq!(session.server_url, "wss://livekit.waddle.test");
        assert_eq!(session.participant_id, "user_42");
        assert_eq!(session.media_type, MediaType::Video);
    }

    #[test]
    fn signs_livekit_access_token() {
        let backend = configured_backend();
        let session = backend
            .create_session(MediaSessionRequest {
                waddle_id: "penguins".to_string(),
                channel_id: "general".to_string(),
                participant_id: "user_42".to_string(),
                participant_name: "alice".to_string(),
                media_type: MediaType::Audio,
            })
            .unwrap();

        let token = decode::<TokenClaims>(
            &session.token,
            &DecodingKey::from_secret(b"super-secret"),
            &Validation::new(Algorithm::HS256),
        )
        .unwrap();

        assert_eq!(token.claims.iss, "devkey");
        assert_eq!(token.claims.sub, "user_42");
        assert_eq!(token.claims.name, "alice");
        assert_eq!(token.claims.video["room"], "room-penguins-general");
        assert_eq!(token.claims.video["roomJoin"], true);
    }

    #[test]
    fn rejects_unsafe_identifiers() {
        let backend = configured_backend();
        let result = backend.create_session(MediaSessionRequest {
            waddle_id: "penguins/../../etc".to_string(),
            channel_id: "general".to_string(),
            participant_id: "user_42".to_string(),
            participant_name: "alice".to_string(),
            media_type: MediaType::Video,
        });

        assert!(matches!(result, Err(MediaBackendError::InvalidRequest(_))));
    }
}
