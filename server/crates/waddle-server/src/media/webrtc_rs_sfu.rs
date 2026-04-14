use super::{
    next_media_session_id, MediaBackend, MediaBackendError, MediaBackendKind, MediaConfig,
    MediaSession, MediaSessionRequest,
};
use url::Url;

#[derive(Debug, Clone)]
pub struct WebrtcRsSfuBackend {
    config: MediaConfig,
}

impl WebrtcRsSfuBackend {
    pub fn new(config: MediaConfig) -> Self {
        Self { config }
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

    fn join_url(&self, room_id: &str, session_id: &str) -> Result<String, MediaBackendError> {
        let mut url = Url::parse(&self.config.public_base_url).map_err(|err| {
            MediaBackendError::InvalidRequest(format!("invalid public_base_url: {err}"))
        })?;

        let path_segments = self
            .config
            .webrtc_rs_sfu
            .signaling_path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let mut segments = url.path_segments_mut().map_err(|_| {
            MediaBackendError::InvalidRequest(
                "public_base_url must be an absolute URL with a hierarchical path".to_string(),
            )
        })?;

        segments.clear();
        for segment in &path_segments {
            segments.push(segment);
        }
        segments.push(room_id);
        drop(segments);

        url.query_pairs_mut().append_pair("session", session_id);
        Ok(url.to_string())
    }
}

impl MediaBackend for WebrtcRsSfuBackend {
    fn kind(&self) -> MediaBackendKind {
        MediaBackendKind::WebrtcRsSfu
    }

    fn create_session(
        &self,
        request: MediaSessionRequest,
    ) -> Result<MediaSession, MediaBackendError> {
        let room_id = Self::validate_identifier(&request.room_id, "room_id")?;
        let participant_id = Self::validate_identifier(&request.participant_id, "participant_id")?;
        let room_prefix =
            Self::validate_identifier(&self.config.webrtc_rs_sfu.room_prefix, "room_prefix")?;

        let normalized_room = format!("{room_prefix}-{room_id}");
        let session_id = next_media_session_id();

        Ok(MediaSession {
            backend: self.kind().to_string(),
            session_id: session_id.clone(),
            room_id: normalized_room.clone(),
            participant_id,
            role: request.role,
            join_url: self.join_url(&normalized_room, &session_id)?,
            ice_servers: self.config.webrtc_rs_sfu.ice_servers.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_webrtc_rs_sfu_session_payload() {
        let mut config = MediaConfig::default();
        config.backend = MediaBackendKind::WebrtcRsSfu;
        config.public_base_url = "https://calls.waddle.test".to_string();
        config.webrtc_rs_sfu.signaling_path = "/sfu/signal".to_string();
        config.webrtc_rs_sfu.room_prefix = "room".to_string();

        let backend = WebrtcRsSfuBackend::new(config);
        let session = backend
            .create_session(MediaSessionRequest {
                room_id: "team-sync".to_string(),
                participant_id: "user-42".to_string(),
                role: "publisher".to_string(),
            })
            .unwrap();

        assert_eq!(session.backend, "webrtc-rs-sfu");
        assert_eq!(session.room_id, "room-team-sync");
        assert_eq!(session.participant_id, "user-42");
        assert!(session
            .join_url
            .starts_with("https://calls.waddle.test/sfu/signal/room-team-sync?session="));
        assert!(!session.ice_servers.is_empty());
    }

    #[test]
    fn rejects_unsafe_room_id() {
        let mut config = MediaConfig::default();
        config.backend = MediaBackendKind::WebrtcRsSfu;

        let backend = WebrtcRsSfuBackend::new(config);
        let result = backend.create_session(MediaSessionRequest {
            room_id: "team/sync".to_string(),
            participant_id: "user-42".to_string(),
            role: "publisher".to_string(),
        });

        assert!(matches!(result, Err(MediaBackendError::InvalidRequest(_))));
    }

    #[test]
    fn rejects_unsafe_participant_id() {
        let mut config = MediaConfig::default();
        config.backend = MediaBackendKind::WebrtcRsSfu;

        let backend = WebrtcRsSfuBackend::new(config);
        let result = backend.create_session(MediaSessionRequest {
            room_id: "team-sync".to_string(),
            participant_id: "user/42".to_string(),
            role: "publisher".to_string(),
        });

        assert!(matches!(result, Err(MediaBackendError::InvalidRequest(_))));
    }
}
