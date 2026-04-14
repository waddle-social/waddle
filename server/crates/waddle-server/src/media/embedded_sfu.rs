use super::{
    next_media_session_id, MediaBackend, MediaBackendError, MediaBackendKind, MediaConfig,
    MediaSession, MediaSessionRequest,
};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use url::Url;

#[derive(Debug)]
struct EmbeddedState {
    sessions: BTreeMap<String, String>,
    room_participants: BTreeMap<String, usize>,
}

#[derive(Debug)]
pub struct EmbeddedSfuBackend {
    config: MediaConfig,
    state: Mutex<EmbeddedState>,
}

impl EmbeddedSfuBackend {
    pub fn new(config: MediaConfig) -> Self {
        Self {
            config,
            state: Mutex::new(EmbeddedState {
                sessions: BTreeMap::new(),
                room_participants: BTreeMap::new(),
            }),
        }
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

    fn lock_state(&self) -> Result<MutexGuard<'_, EmbeddedState>, MediaBackendError> {
        self.state.lock().map_err(|_| {
            MediaBackendError::InvalidRequest("embedded sfu state lock poisoned".to_string())
        })
    }

    fn reserve_slot<'a>(
        &'a self,
        state: &'a mut EmbeddedState,
        room_id: &str,
    ) -> Result<(), MediaBackendError> {
        if state.sessions.len() >= self.config.embedded_sfu.max_sessions {
            return Err(MediaBackendError::CapacityExceeded(format!(
                "max sessions ({}) reached",
                self.config.embedded_sfu.max_sessions
            )));
        }

        if !state.room_participants.contains_key(room_id)
            && state.room_participants.len() >= self.config.embedded_sfu.max_rooms
        {
            return Err(MediaBackendError::CapacityExceeded(format!(
                "max rooms ({}) reached",
                self.config.embedded_sfu.max_rooms
            )));
        }

        let count = state.room_participants.get(room_id).copied().unwrap_or(0);
        if count >= self.config.embedded_sfu.max_participants_per_room {
            return Err(MediaBackendError::CapacityExceeded(format!(
                "max participants per room ({}) reached",
                self.config.embedded_sfu.max_participants_per_room
            )));
        }
        Ok(())
    }

    fn join_url(&self, room_id: &str, session_id: &str) -> Result<String, MediaBackendError> {
        let mut url = Url::parse(&self.config.public_base_url).map_err(|err| {
            MediaBackendError::InvalidRequest(format!("invalid public_base_url: {err}"))
        })?;

        let path_segments = self
            .config
            .embedded_sfu
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

impl MediaBackend for EmbeddedSfuBackend {
    fn kind(&self) -> MediaBackendKind {
        MediaBackendKind::EmbeddedSfu
    }

    fn create_session(
        &self,
        request: MediaSessionRequest,
    ) -> Result<MediaSession, MediaBackendError> {
        let room_id = Self::validate_identifier(&request.room_id, "room_id")?;
        let participant_id = Self::validate_identifier(&request.participant_id, "participant_id")?;
        let room_prefix =
            Self::validate_identifier(&self.config.embedded_sfu.room_prefix, "room_prefix")?;

        let normalized_room = format!("{room_prefix}-{room_id}");
        let session_id = next_media_session_id();

        let mut state = self.lock_state()?;
        self.reserve_slot(&mut state, &normalized_room)?;
        state
            .sessions
            .insert(session_id.clone(), normalized_room.clone());
        *state
            .room_participants
            .entry(normalized_room.clone())
            .or_insert(0) += 1;

        Ok(MediaSession {
            backend: self.kind().to_string(),
            session_id: session_id.clone(),
            room_id: normalized_room.clone(),
            participant_id,
            role: request.role,
            join_url: self.join_url(&normalized_room, &session_id)?,
            ice_servers: self.config.embedded_sfu.ice_servers.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_embedded_sfu_session_payload() {
        let mut config = MediaConfig::default();
        config.backend = MediaBackendKind::EmbeddedSfu;
        config.public_base_url = "https://xmpp.waddle.test".to_string();
        config.embedded_sfu.signaling_path = "/embedded/sfu".to_string();
        config.embedded_sfu.room_prefix = "room".to_string();

        let backend = EmbeddedSfuBackend::new(config);
        let session = backend
            .create_session(MediaSessionRequest {
                room_id: "team-sync".to_string(),
                participant_id: "user-42".to_string(),
                role: "publisher".to_string(),
            })
            .unwrap();

        assert_eq!(session.backend, "embedded-sfu");
        assert_eq!(session.room_id, "room-team-sync");
        assert_eq!(session.participant_id, "user-42");
        assert!(session
            .join_url
            .starts_with("https://xmpp.waddle.test/embedded/sfu/room-team-sync?session="));
        assert!(!session.ice_servers.is_empty());
    }

    #[test]
    fn rejects_capacity_overflow() {
        let mut config = MediaConfig::default();
        config.backend = MediaBackendKind::EmbeddedSfu;
        config.embedded_sfu.max_rooms = 1;
        config.embedded_sfu.max_participants_per_room = 1;
        config.embedded_sfu.max_sessions = 1;

        let backend = EmbeddedSfuBackend::new(config);
        backend
            .create_session(MediaSessionRequest {
                room_id: "room-1".to_string(),
                participant_id: "user-1".to_string(),
                role: "publisher".to_string(),
            })
            .unwrap();

        let result = backend.create_session(MediaSessionRequest {
            room_id: "room-2".to_string(),
            participant_id: "user-2".to_string(),
            role: "publisher".to_string(),
        });

        assert!(matches!(
            result,
            Err(MediaBackendError::CapacityExceeded(_))
        ));
    }
}
