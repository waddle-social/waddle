use super::{
    next_media_session_id, MediaBackend, MediaBackendError, MediaBackendKind, MediaConfig,
    MediaSession, MediaSessionRequest,
};

#[derive(Debug, Clone)]
pub struct WebrtcRsSfuBackend {
    config: MediaConfig,
}

impl WebrtcRsSfuBackend {
    pub fn new(config: MediaConfig) -> Self {
        Self { config }
    }

    fn join_url(&self, room_id: &str, session_id: &str) -> String {
        let base = self.config.public_base_url.trim_end_matches('/');
        let path = self
            .config
            .webrtc_rs_sfu
            .signaling_path
            .trim_start_matches('/');
        format!("{base}/{path}/{room_id}?session={session_id}")
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
        if request.room_id.trim().is_empty() {
            return Err(MediaBackendError::InvalidRequest(
                "room_id cannot be empty".to_string(),
            ));
        }

        let normalized_room = format!(
            "{}-{}",
            self.config.webrtc_rs_sfu.room_prefix,
            request.room_id.trim()
        );
        let session_id = next_media_session_id();

        Ok(MediaSession {
            backend: self.kind().to_string(),
            session_id: session_id.clone(),
            room_id: normalized_room.clone(),
            participant_id: request.participant_id,
            role: request.role,
            join_url: self.join_url(&normalized_room, &session_id),
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
}
