//! XMPP-native media gateway state and LiveKit backend helpers.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use jid::{BareJid, FullJid, Jid};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use sha1::Sha1;
use std::{
    fmt,
    str::FromStr,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use url::Url;
use waddle_xmpp::{
    disco::Feature,
    xep::{
        self,
        xep0166::{self, JingleErrorCondition, JingleValidationError},
        xep0215::{
            ExtDiscoRequest, ExternalService, ExternalServiceTransport, ExternalServiceType,
        },
        xep0482::{self, CallInvitePayload, JoinMethod},
    },
};
use xmpp_parsers::jingle::Action;
use xmpp_parsers::{
    iq::Iq,
    stanza_error::{DefinedCondition, ErrorType},
};

// External TURN REST credentials are standardized around HMAC-SHA1; this is
// not used for password storage, LiveKit JWTs, or general Waddle request signing.
type HmacSha1 = Hmac<Sha1>;

/// Waddle-only operational metadata namespace for media gateway details.
pub const NS_WADDLE_MEDIA: &str = "urn:waddle:media:0";
const MAX_MEDIA_SESSION_ID_LEN: usize = 128;
const MAX_MEDIA_SESSIONS_TOTAL: usize = 10_000;
const MAX_MEDIA_SESSIONS_PER_CREATOR: usize = 32;
const MAX_MEDIA_SESSIONS_PER_CONVERSATION: usize = 8;
const MAX_MEDIA_ACTIVE_SESSION_IDLE_HOURS: i64 = 24;
const MAX_TURN_CREDENTIAL_REQUESTS_PER_TTL: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaSessionId(String);

impl MediaSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, MediaGatewayError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(MediaGatewayError::MissingSessionId);
        }
        if value.len() > MAX_MEDIA_SESSION_ID_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(MediaGatewayError::InvalidSessionId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MediaSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallInviteMessageId(String);

impl CallInviteMessageId {
    pub fn new(value: impl Into<String>) -> Result<Self, MediaGatewayError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(MediaGatewayError::MissingInviteReference);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveKitRoomName(String);

impl LiveKitRoomName {
    pub fn new(value: impl Into<String>) -> Result<Self, MediaGatewayError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(MediaGatewayError::MissingLiveKitRoom);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveKitIdentity(String);

impl LiveKitIdentity {
    fn for_participant(session_id: &MediaSessionId, jid: &FullJid) -> Self {
        Self(format!("waddle:{}:{}", session_id.as_str(), jid))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LiveKitApiKey(String);

impl LiveKitApiKey {
    fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LiveKitApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LiveKitApiKey(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LiveKitApiSecret(String);

impl LiveKitApiSecret {
    fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for LiveKitApiSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LiveKitApiSecret(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TurnSharedSecret(String);

impl TurnSharedSecret {
    fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for TurnSharedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TurnSharedSecret(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveKitRoomPrefix(String);

impl LiveKitRoomPrefix {
    fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        if value.trim().is_empty() {
            Self("waddle".to_string())
        } else {
            Self(value)
        }
    }

    fn room_name(&self, session_id: &MediaSessionId) -> Result<LiveKitRoomName, MediaGatewayError> {
        LiveKitRoomName::new(format!("{}-{}", self.0, session_id.as_str()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaSessionScope {
    Muc,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaSessionStatus {
    Invited,
    Active,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaParticipantRole {
    Creator,
    Participant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaParticipantState {
    Joined,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaIntent {
    pub audio: bool,
    pub video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSession {
    pub id: MediaSessionId,
    pub scope: MediaSessionScope,
    pub anchor_jid: Jid,
    pub livekit_room_name: LiveKitRoomName,
    pub creator_jid: FullJid,
    pub status: MediaSessionStatus,
    pub media_intent: MediaIntent,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallInviteConversationKey {
    Muc {
        room_jid: BareJid,
    },
    Direct {
        first_jid: BareJid,
        second_jid: BareJid,
    },
}

impl CallInviteConversationKey {
    pub fn muc(room_jid: BareJid) -> Self {
        Self::Muc { room_jid }
    }

    pub fn direct(first_jid: BareJid, second_jid: BareJid) -> Self {
        if first_jid.to_string() <= second_jid.to_string() {
            Self::Direct {
                first_jid,
                second_jid,
            }
        } else {
            Self::Direct {
                first_jid: second_jid,
                second_jid: first_jid,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CallInviteReferenceKey {
    conversation: CallInviteConversationKey,
    invite_id: CallInviteMessageId,
}

impl CallInviteReferenceKey {
    fn new(
        conversation: CallInviteConversationKey,
        invite_id: impl Into<String>,
    ) -> Result<Self, MediaGatewayError> {
        Ok(Self {
            conversation,
            invite_id: CallInviteMessageId::new(invite_id)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallInviteReferenceBinding {
    session_id: MediaSessionId,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaParticipant {
    pub session_id: MediaSessionId,
    pub full_jid: FullJid,
    pub livekit_identity: LiveKitIdentity,
    pub role: MediaParticipantRole,
    pub state: MediaParticipantState,
    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,
    pub livekit_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnServiceConfig {
    pub host: Option<String>,
    pub udp_port: Option<u16>,
    pub tcp_port: Option<u16>,
    pub shared_secret: Option<TurnSharedSecret>,
}

impl Default for TurnServiceConfig {
    fn default() -> Self {
        Self {
            host: None,
            udp_port: Some(3478),
            tcp_port: None,
            shared_secret: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveKitConfig {
    pub enabled: bool,
    pub ws_url: Option<Url>,
    pub api_key: Option<LiveKitApiKey>,
    pub api_secret: Option<LiveKitApiSecret>,
    pub room_prefix: LiveKitRoomPrefix,
    pub token_ttl: Duration,
    pub turn: TurnServiceConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaSessionLimits {
    max_total: usize,
    max_per_creator: usize,
    max_per_conversation: usize,
}

impl Default for MediaSessionLimits {
    fn default() -> Self {
        Self {
            max_total: MAX_MEDIA_SESSIONS_TOTAL,
            max_per_creator: MAX_MEDIA_SESSIONS_PER_CREATOR,
            max_per_conversation: MAX_MEDIA_SESSIONS_PER_CONVERSATION,
        }
    }
}

impl Default for LiveKitConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

impl LiveKitConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ws_url: None,
            api_key: None,
            api_secret: None,
            room_prefix: LiveKitRoomPrefix::new("waddle"),
            token_ttl: Duration::minutes(10),
            turn: TurnServiceConfig::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn enabled_for_tests() -> Self {
        Self {
            enabled: true,
            ws_url: Some("wss://livekit.example".parse().expect("test LiveKit URL")),
            api_key: LiveKitApiKey::new("devkey"),
            api_secret: LiveKitApiSecret::new("devsecret"),
            room_prefix: LiveKitRoomPrefix::new("test"),
            token_ttl: Duration::minutes(5),
            turn: TurnServiceConfig::default(),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let ws_url = optional_env_url("WADDLE_LIVEKIT_WS_URL")?;
        let api_key = optional_env("WADDLE_LIVEKIT_API_KEY").and_then(LiveKitApiKey::new);
        let api_secret = optional_env("WADDLE_LIVEKIT_API_SECRET").and_then(LiveKitApiSecret::new);
        let enabled = optional_env_bool("WADDLE_LIVEKIT_ENABLED")
            .unwrap_or_else(|| ws_url.is_some() || api_key.is_some() || api_secret.is_some());

        if enabled && ws_url.is_none() {
            return Err("WADDLE_LIVEKIT_WS_URL is required when LiveKit is enabled".to_string());
        }
        if enabled && api_key.is_none() {
            return Err("WADDLE_LIVEKIT_API_KEY is required when LiveKit is enabled".to_string());
        }
        if enabled && api_secret.is_none() {
            return Err(
                "WADDLE_LIVEKIT_API_SECRET is required when LiveKit is enabled".to_string(),
            );
        }

        let token_ttl_seconds = optional_env("WADDLE_LIVEKIT_TOKEN_TTL_SECONDS")
            .map(|value| {
                value
                    .parse::<i64>()
                    .map_err(|_| "WADDLE_LIVEKIT_TOKEN_TTL_SECONDS must be an integer".to_string())
            })
            .transpose()?
            .unwrap_or(600)
            .max(60);

        let turn = TurnServiceConfig {
            host: optional_env("WADDLE_LIVEKIT_TURN_HOST"),
            udp_port: optional_env("WADDLE_LIVEKIT_TURN_UDP_PORT")
                .map(|value| {
                    value
                        .parse::<u16>()
                        .map_err(|_| "WADDLE_LIVEKIT_TURN_UDP_PORT must be a port".to_string())
                })
                .transpose()?
                .or(Some(3478)),
            tcp_port: optional_env("WADDLE_LIVEKIT_TURN_TCP_PORT")
                .map(|value| {
                    value
                        .parse::<u16>()
                        .map_err(|_| "WADDLE_LIVEKIT_TURN_TCP_PORT must be a port".to_string())
                })
                .transpose()?,
            shared_secret: optional_env("WADDLE_LIVEKIT_TURN_SHARED_SECRET")
                .and_then(TurnSharedSecret::new),
        };

        Ok(Self {
            enabled,
            ws_url,
            api_key,
            api_secret,
            room_prefix: LiveKitRoomPrefix::new(
                optional_env("WADDLE_LIVEKIT_ROOM_PREFIX").unwrap_or_else(|| "waddle".to_string()),
            ),
            token_ttl: Duration::seconds(token_ttl_seconds),
            turn,
        })
    }

    fn token_inputs(&self) -> Result<(&LiveKitApiKey, &LiveKitApiSecret), MediaGatewayError> {
        if !self.enabled {
            return Err(MediaGatewayError::Disabled);
        }
        let api_key = self
            .api_key
            .as_ref()
            .ok_or(MediaGatewayError::LiveKitUnavailable)?;
        let api_secret = self
            .api_secret
            .as_ref()
            .ok_or(MediaGatewayError::LiveKitUnavailable)?;
        Ok((api_key, api_secret))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LiveKitAccessToken(String);

impl fmt::Debug for LiveKitAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LiveKitAccessToken(<redacted>)")
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MediaGatewayError {
    #[error("media gateway is disabled")]
    Disabled,
    #[error("missing media session id")]
    MissingSessionId,
    #[error("invalid media session id")]
    InvalidSessionId,
    #[error("missing call invite reference")]
    MissingInviteReference,
    #[error("missing LiveKit room name")]
    MissingLiveKitRoom,
    #[error("unknown media session")]
    UnknownSession,
    #[error("sender is not authorized for this media session")]
    Forbidden,
    #[error("invalid call invite")]
    InvalidInvite,
    #[error("unsupported call invite join method")]
    UnsupportedInviteMethod,
    #[error("invalid jingle payload: {0}")]
    InvalidJingle(JingleValidationError),
    #[error("media session has ended")]
    SessionEnded,
    #[error("Jingle media bridge is unavailable")]
    JingleBridgeUnavailable,
    #[error("LiveKit backend is unavailable")]
    LiveKitUnavailable,
    #[error("media session limit exceeded")]
    CapacityExceeded,
}

#[derive(Debug)]
pub struct MediaGateway {
    config: LiveKitConfig,
    limits: MediaSessionLimits,
    capacity_gate: Mutex<()>,
    sessions: dashmap::DashMap<MediaSessionId, MediaSession>,
    invite_references: dashmap::DashMap<CallInviteReferenceKey, CallInviteReferenceBinding>,
    participants: dashmap::DashMap<(MediaSessionId, FullJid), MediaParticipant>,
    turn_credential_requests: dashmap::DashMap<BareJid, Vec<DateTime<Utc>>>,
}

impl MediaGateway {
    pub fn new(config: LiveKitConfig) -> Self {
        Self {
            config,
            limits: MediaSessionLimits::default(),
            capacity_gate: Mutex::new(()),
            sessions: dashmap::DashMap::new(),
            invite_references: dashmap::DashMap::new(),
            participants: dashmap::DashMap::new(),
            turn_credential_requests: dashmap::DashMap::new(),
        }
    }

    #[cfg(test)]
    fn with_limits(config: LiveKitConfig, limits: MediaSessionLimits) -> Self {
        Self {
            config,
            limits,
            capacity_gate: Mutex::new(()),
            sessions: dashmap::DashMap::new(),
            invite_references: dashmap::DashMap::new(),
            participants: dashmap::DashMap::new(),
            turn_credential_requests: dashmap::DashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn service_features(&self) -> Vec<Feature> {
        if !self.enabled() {
            return Vec::new();
        }
        let mut features = vec![
            Feature::disco_info(),
            Feature::disco_items(),
            Feature::new(xep::xep0482::NS_CALL_INVITES),
            Feature::new(xep::xep0215::NS_EXTDISCO),
        ];
        features.extend(media_session_features());
        features.push(Feature::new(NS_WADDLE_MEDIA));
        features
    }

    pub fn session_features(&self, id: &MediaSessionId) -> Option<Vec<Feature>> {
        if !self.enabled() {
            return None;
        }
        self.sessions
            .get(id)
            .filter(|entry| entry.status != MediaSessionStatus::Ended)
            .map(|_| {
                let mut features = vec![Feature::disco_info(), Feature::disco_items()];
                features.extend(media_session_features());
                features.push(Feature::new(NS_WADDLE_MEDIA));
                features
            })
    }

    pub fn server_features(&self) -> Vec<Feature> {
        if !self.enabled() {
            return Vec::new();
        }
        vec![
            Feature::new(xep::xep0482::NS_CALL_INVITES),
            Feature::new(xep::xep0215::NS_EXTDISCO),
        ]
    }

    pub fn get_session(&self, id: &MediaSessionId) -> Option<MediaSession> {
        self.sessions.get(id).map(|entry| entry.value().clone())
    }

    pub fn discard_session(&self, id: &MediaSessionId) {
        self.remove_session(id);
    }

    pub fn cleanup_expired_sessions(&self) -> usize {
        self.cleanup_expired_sessions_inner()
    }

    pub fn get_session_for_invite_reference(
        &self,
        conversation: CallInviteConversationKey,
        invite_id: impl Into<String>,
    ) -> Result<Option<MediaSession>, MediaGatewayError> {
        let reference_key = CallInviteReferenceKey::new(conversation, invite_id)?;
        let Some(session_id) = self
            .invite_references
            .get(&reference_key)
            .and_then(|entry| self.get_session_for_reference_binding(entry.value()))
        else {
            return Ok(None);
        };
        Ok(Some(session_id))
    }

    pub fn bind_invite_reference(
        &self,
        session_id: &MediaSessionId,
        conversation: CallInviteConversationKey,
        invite_id: impl Into<String>,
    ) -> Result<(), MediaGatewayError> {
        let binding = self
            .sessions
            .get(session_id)
            .map(|entry| CallInviteReferenceBinding {
                session_id: session_id.clone(),
                created_at: entry.created_at,
            })
            .ok_or(MediaGatewayError::UnknownSession)?;
        let reference_key = CallInviteReferenceKey::new(conversation, invite_id)?;
        match self.invite_references.entry(reference_key) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                if entry.get() != &binding {
                    return Err(MediaGatewayError::Forbidden);
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(binding);
            }
        }
        Ok(())
    }

    pub fn ensure_invite_session(
        &self,
        message: &mut xmpp_parsers::message::Message,
        scope: MediaSessionScope,
        anchor_jid: Jid,
        creator_jid: &FullJid,
        media_domain: &str,
    ) -> Result<Option<MediaSessionId>, MediaGatewayError> {
        self.cleanup_expired_sessions_inner();

        let Some(payload_index) = message
            .payloads
            .iter()
            .position(xep0482::is_call_invite_element)
        else {
            return Ok(None);
        };

        if !self.enabled() {
            return Err(MediaGatewayError::Disabled);
        }

        let payload = xep0482::parse_call_invite_payload(&message.payloads[payload_index])
            .map_err(|_| MediaGatewayError::InvalidInvite)?;

        let CallInvitePayload::Invite(invite) = payload else {
            return Ok(None);
        };

        let waddle_jingle_methods = invite
            .methods
            .iter()
            .filter(|method| {
                matches!(
                    method,
                    JoinMethod::Jingle {
                        sid,
                        jid: Some(jid),
                    } if gateway_jingle_jid_matches(jid, media_domain, sid.as_str())
                )
            })
            .collect::<Vec<_>>();
        if waddle_jingle_methods.is_empty() {
            return Ok(None);
        }
        if waddle_jingle_methods.len() != 1 {
            return Err(MediaGatewayError::UnsupportedInviteMethod);
        }
        let JoinMethod::Jingle { sid, .. } = waddle_jingle_methods[0] else {
            return Err(MediaGatewayError::UnsupportedInviteMethod);
        };
        let session_id = MediaSessionId::new(sid.as_str())?;

        self.create_or_touch_session(
            session_id.clone(),
            scope,
            anchor_jid,
            creator_jid.clone(),
            MediaIntent {
                audio: invite.audio,
                video: invite.video,
            },
        )?;
        Ok(Some(session_id))
    }

    pub fn observe_call_lifecycle(
        &self,
        message: &xmpp_parsers::message::Message,
        conversation: CallInviteConversationKey,
        sender_jid: &FullJid,
    ) {
        let Some(payload) = xep0482::extract_call_invite_payload(message) else {
            return;
        };
        let Some(reference_id) = payload.reference_id() else {
            return;
        };
        let Ok(reference_key) = CallInviteReferenceKey::new(conversation, reference_id.as_str())
        else {
            return;
        };
        let Some(session_id) = self
            .invite_references
            .get(&reference_key)
            .map(|entry| entry.value().clone())
        else {
            return;
        };
        let Some(session) = self.get_session_for_reference_binding(&session_id) else {
            return;
        };
        if session.status == MediaSessionStatus::Ended {
            return;
        }

        match payload {
            CallInvitePayload::Accept { method, .. } => {
                if !accept_method_matches_session_id(&method, &session_id.session_id) {
                    return;
                }
                let _ = self.upsert_participant_for_reference_binding(
                    &session_id,
                    sender_jid,
                    MediaParticipantState::Joined,
                );
            }
            CallInvitePayload::Reject(_) | CallInvitePayload::Left(_) => {
                if session.scope == MediaSessionScope::Direct {
                    self.end_session_for_reference_binding(&session_id);
                } else {
                    self.mark_participant_left_for_reference_binding(&session_id, sender_jid);
                }
            }
            CallInvitePayload::Retract(_) => {
                self.end_session_for_reference_binding(&session_id);
            }
            CallInvitePayload::Invite(_) => {}
        }
    }

    pub fn mark_muc_participant_left(&self, room_jid: &BareJid, full_jid: &FullJid) {
        let session_ids = self
            .sessions
            .iter()
            .filter_map(|entry| {
                let session = entry.value();
                (session.scope == MediaSessionScope::Muc
                    && session.anchor_jid.to_bare() == *room_jid
                    && session.status == MediaSessionStatus::Active)
                    .then(|| entry.key().clone())
            })
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.mark_participant_left(&session_id, full_jid);
        }
    }

    pub fn mark_participant_disconnected(&self, full_jid: &FullJid) {
        let session_ids = self
            .participants
            .iter()
            .filter_map(|entry| {
                (entry.key().1 == *full_jid && entry.value().state == MediaParticipantState::Joined)
                    .then(|| entry.key().0.clone())
            })
            .collect::<Vec<_>>();
        for session_id in session_ids {
            if self
                .sessions
                .get(&session_id)
                .is_some_and(|session| session.scope == MediaSessionScope::Direct)
            {
                self.end_session(&session_id);
            } else {
                self.mark_participant_left(&session_id, full_jid);
            }
        }
        let creator_direct_session_ids = self
            .sessions
            .iter()
            .filter_map(|entry| {
                let session = entry.value();
                (session.scope == MediaSessionScope::Direct
                    && session.creator_jid == *full_jid
                    && session.status != MediaSessionStatus::Ended)
                    .then(|| entry.key().clone())
            })
            .collect::<Vec<_>>();
        for session_id in creator_direct_session_ids {
            self.end_session(&session_id);
        }
    }

    pub fn services_for_request(
        &self,
        request: &ExtDiscoRequest,
        requester: Option<&FullJid>,
    ) -> Vec<ExternalService> {
        if !self.enabled() {
            return Vec::new();
        }

        match request {
            ExtDiscoRequest::Services { service_type } => {
                self.configured_turn_services(service_type.as_ref(), requester, false)
            }
            ExtDiscoRequest::Credentials { service } => {
                if !matches!(service.service_type, ExternalServiceType::Turn) {
                    return Vec::new();
                }
                self.configured_turn_services(Some(&ExternalServiceType::Turn), requester, true)
                    .into_iter()
                    .filter(|candidate| service_matches_request(candidate, service))
                    .collect()
            }
        }
    }
    pub fn handle_jingle_iq(&self, iq: &Iq, sender_jid: &FullJid) -> Result<Iq, MediaGatewayError> {
        if !self.enabled() {
            return Err(MediaGatewayError::Disabled);
        }

        let target_session_id = iq
            .to
            .as_ref()
            .and_then(|jid| jid.clone().try_into_full().ok())
            .map(|jid| jid.resource().to_string())
            .map(MediaSessionId::new)
            .transpose()?
            .ok_or(MediaGatewayError::MissingSessionId)?;
        let session = self
            .get_session(&target_session_id)
            .ok_or(MediaGatewayError::UnknownSession)?;
        if session.status == MediaSessionStatus::Ended {
            return Err(MediaGatewayError::SessionEnded);
        }
        let jingle = xep0166::parse_jingle_iq(iq).map_err(MediaGatewayError::InvalidJingle)?;

        if jingle.sid.0 != target_session_id.as_str() {
            return Err(MediaGatewayError::UnknownSession);
        }

        if jingle.action == Action::SessionTerminate {
            if session.scope == MediaSessionScope::Direct {
                self.end_session(&target_session_id);
            } else {
                self.mark_participant_left(&target_session_id, sender_jid);
            }
            return Ok(xep0166::build_jingle_ack(iq));
        }

        // Until the WebRTC bridge is implemented, valid gateway-bound Jingle
        // actions are intentionally unavailable rather than malformed.
        Err(MediaGatewayError::JingleBridgeUnavailable)
    }

    pub fn build_jingle_error(&self, iq: &Iq, error: MediaGatewayError) -> Iq {
        match error {
            MediaGatewayError::UnknownSession
            | MediaGatewayError::MissingSessionId
            | MediaGatewayError::SessionEnded => xep0166::build_jingle_error(
                iq,
                ErrorType::Cancel,
                DefinedCondition::ItemNotFound,
                Some(JingleErrorCondition::UnknownSession),
                "Unknown media session.",
            ),
            MediaGatewayError::MissingInviteReference => xep0166::build_jingle_error(
                iq,
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                None,
                "Invalid call invite reference.",
            ),
            MediaGatewayError::Disabled
            | MediaGatewayError::LiveKitUnavailable
            | MediaGatewayError::JingleBridgeUnavailable
            | MediaGatewayError::CapacityExceeded => xep0166::build_jingle_error(
                iq,
                ErrorType::Cancel,
                DefinedCondition::ServiceUnavailable,
                None,
                "Media gateway is unavailable.",
            ),
            MediaGatewayError::Forbidden => xep0166::build_jingle_error(
                iq,
                ErrorType::Auth,
                DefinedCondition::Forbidden,
                None,
                "Sender is not authorized for this media session.",
            ),
            MediaGatewayError::InvalidJingle(_)
            | MediaGatewayError::InvalidInvite
            | MediaGatewayError::InvalidSessionId
            | MediaGatewayError::MissingLiveKitRoom
            | MediaGatewayError::UnsupportedInviteMethod => xep0166::build_jingle_error(
                iq,
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                None,
                "Invalid media gateway request.",
            ),
        }
    }

    fn create_or_touch_session(
        &self,
        session_id: MediaSessionId,
        scope: MediaSessionScope,
        anchor_jid: Jid,
        creator_jid: FullJid,
        media_intent: MediaIntent,
    ) -> Result<(), MediaGatewayError> {
        let now = Utc::now();
        let livekit_room_name = self.config.room_prefix.room_name(&session_id)?;
        let creator_identity = LiveKitIdentity::for_participant(&session_id, &creator_jid);
        self.create_internal_token(&livekit_room_name, &creator_identity, false)?;
        let new_session = MediaSession {
            id: session_id.clone(),
            scope,
            anchor_jid: anchor_jid.clone(),
            livekit_room_name: livekit_room_name.clone(),
            creator_jid: creator_jid.clone(),
            status: MediaSessionStatus::Invited,
            media_intent,
            created_at: now,
            updated_at: now,
            ended_at: None,
        };

        let _capacity_guard = self
            .capacity_gate
            .lock()
            .map_err(|_| MediaGatewayError::LiveKitUnavailable)?;
        if let Some(mut entry) = self.sessions.get_mut(&session_id) {
            let existing = entry.value();
            if existing.status == MediaSessionStatus::Ended {
                return Err(MediaGatewayError::SessionEnded);
            } else if existing.scope != scope
                || existing.anchor_jid != anchor_jid
                || existing.creator_jid != creator_jid
            {
                return Err(MediaGatewayError::Forbidden);
            } else {
                let session = entry.value_mut();
                session.livekit_room_name = livekit_room_name;
                session.media_intent = media_intent;
                session.updated_at = now;
            }
        } else {
            self.ensure_session_capacity(scope, &anchor_jid, &creator_jid)?;
            self.sessions.insert(session_id, new_session);
        }
        Ok(())
    }

    fn ensure_session_capacity(
        &self,
        scope: MediaSessionScope,
        anchor_jid: &Jid,
        creator_jid: &FullJid,
    ) -> Result<(), MediaGatewayError> {
        let creator_bare = creator_jid.to_bare();
        let mut total = 0usize;
        let mut creator_total = 0usize;
        let mut conversation_total = 0usize;

        for entry in &self.sessions {
            let session = entry.value();
            if session.status == MediaSessionStatus::Ended {
                continue;
            }
            total += 1;
            if session.creator_jid.to_bare() == creator_bare {
                creator_total += 1;
            }
            if session_matches_conversation(session, scope, anchor_jid, &creator_bare) {
                conversation_total += 1;
            }
        }

        if total >= self.limits.max_total
            || creator_total >= self.limits.max_per_creator
            || conversation_total >= self.limits.max_per_conversation
        {
            return Err(MediaGatewayError::CapacityExceeded);
        }
        Ok(())
    }

    #[cfg(test)]
    fn upsert_participant(
        &self,
        session_id: &MediaSessionId,
        full_jid: &FullJid,
        state: MediaParticipantState,
    ) -> Result<MediaParticipant, MediaGatewayError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or(MediaGatewayError::UnknownSession)?;
        if session.status == MediaSessionStatus::Ended {
            return Err(MediaGatewayError::SessionEnded);
        }
        let now = Utc::now();
        let role = if session.creator_jid == *full_jid {
            MediaParticipantRole::Creator
        } else {
            MediaParticipantRole::Participant
        };
        let participant = self
            .participants
            .get(&(session_id.clone(), full_jid.clone()))
            .map(|entry| {
                let mut participant = entry.value().clone();
                participant.state = state;
                participant.left_at = None;
                participant
            })
            .unwrap_or_else(|| MediaParticipant {
                session_id: session_id.clone(),
                full_jid: full_jid.clone(),
                livekit_identity: LiveKitIdentity::for_participant(session_id, full_jid),
                role,
                state,
                joined_at: now,
                left_at: None,
                livekit_token_expires_at: None,
            });
        self.participants
            .insert((session_id.clone(), full_jid.clone()), participant.clone());
        drop(session);
        self.mark_session_active(session_id);
        Ok(participant)
    }

    fn get_session_for_reference_binding(
        &self,
        binding: &CallInviteReferenceBinding,
    ) -> Option<MediaSession> {
        self.get_session(&binding.session_id)
            .filter(|session| session.created_at == binding.created_at)
    }

    fn upsert_participant_for_reference_binding(
        &self,
        binding: &CallInviteReferenceBinding,
        full_jid: &FullJid,
        state: MediaParticipantState,
    ) -> Result<MediaParticipant, MediaGatewayError> {
        let mut session = self
            .sessions
            .get_mut(&binding.session_id)
            .ok_or(MediaGatewayError::UnknownSession)?;
        if session.created_at != binding.created_at {
            return Err(MediaGatewayError::UnknownSession);
        }
        if session.status == MediaSessionStatus::Ended {
            return Err(MediaGatewayError::SessionEnded);
        }
        let now = Utc::now();
        let role = if session.creator_jid == *full_jid {
            MediaParticipantRole::Creator
        } else {
            MediaParticipantRole::Participant
        };
        let participant = self
            .participants
            .get(&(binding.session_id.clone(), full_jid.clone()))
            .map(|entry| {
                let mut participant = entry.value().clone();
                participant.state = state;
                participant.left_at = None;
                participant
            })
            .unwrap_or_else(|| MediaParticipant {
                session_id: binding.session_id.clone(),
                full_jid: full_jid.clone(),
                livekit_identity: LiveKitIdentity::for_participant(&binding.session_id, full_jid),
                role,
                state,
                joined_at: now,
                left_at: None,
                livekit_token_expires_at: None,
            });
        self.participants.insert(
            (binding.session_id.clone(), full_jid.clone()),
            participant.clone(),
        );
        session.status = MediaSessionStatus::Active;
        session.updated_at = now;
        session.ended_at = None;
        Ok(participant)
    }

    fn mark_participant_left_for_reference_binding(
        &self,
        binding: &CallInviteReferenceBinding,
        full_jid: &FullJid,
    ) {
        let now = Utc::now();
        let Some(session) = self.sessions.get(&binding.session_id) else {
            return;
        };
        if session.created_at != binding.created_at {
            return;
        }
        if let Some(mut entry) = self
            .participants
            .get_mut(&(binding.session_id.clone(), full_jid.clone()))
        {
            entry.state = MediaParticipantState::Left;
            entry.left_at = Some(now);
        }
        let should_check_muc =
            session.scope == MediaSessionScope::Muc && session.status == MediaSessionStatus::Active;
        drop(session);
        if let Some(mut session) = self.sessions.get_mut(&binding.session_id) {
            if session.status != MediaSessionStatus::Ended {
                session.updated_at = now;
            }
        }
        if should_check_muc {
            self.end_muc_session_if_empty_for_reference_binding(binding);
        }
    }

    fn end_session_for_reference_binding(&self, binding: &CallInviteReferenceBinding) {
        let Some(mut session) = self.sessions.get_mut(&binding.session_id) else {
            return;
        };
        if session.created_at != binding.created_at {
            return;
        }
        let now = Utc::now();
        session.status = MediaSessionStatus::Ended;
        session.updated_at = now;
        session.ended_at = Some(now);
        drop(session);
        self.remove_invite_references_for_session(&binding.session_id);
        self.remove_participants_for_session(&binding.session_id);
    }

    fn mark_participant_left(&self, session_id: &MediaSessionId, full_jid: &FullJid) {
        let now = Utc::now();
        if let Some(mut entry) = self
            .participants
            .get_mut(&(session_id.clone(), full_jid.clone()))
        {
            entry.state = MediaParticipantState::Left;
            entry.left_at = Some(now);
        }
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            if session.status != MediaSessionStatus::Ended {
                session.updated_at = now;
            }
        }
        self.end_muc_session_if_empty(session_id);
    }

    #[cfg(test)]
    fn mark_session_active(&self, session_id: &MediaSessionId) {
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            if session.status == MediaSessionStatus::Ended {
                return;
            }
            session.status = MediaSessionStatus::Active;
            session.updated_at = Utc::now();
            session.ended_at = None;
        }
    }

    fn end_session(&self, session_id: &MediaSessionId) {
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            let now = Utc::now();
            session.status = MediaSessionStatus::Ended;
            session.updated_at = now;
            session.ended_at = Some(now);
        }
        self.remove_invite_references_for_session(session_id);
        self.remove_participants_for_session(session_id);
    }

    fn end_muc_session_if_empty(&self, session_id: &MediaSessionId) {
        let Some(mut session) = self.sessions.get_mut(session_id) else {
            return;
        };
        if session.scope != MediaSessionScope::Muc
            || session.status != MediaSessionStatus::Active
            || self.session_has_joined_participants(session_id)
        {
            return;
        }
        let now = Utc::now();
        session.status = MediaSessionStatus::Ended;
        session.updated_at = now;
        session.ended_at = Some(now);
        drop(session);
        self.remove_invite_references_for_session(session_id);
        self.remove_participants_for_session(session_id);
    }

    fn end_muc_session_if_empty_for_reference_binding(&self, binding: &CallInviteReferenceBinding) {
        let Some(mut session) = self.sessions.get_mut(&binding.session_id) else {
            return;
        };
        if session.created_at != binding.created_at
            || session.scope != MediaSessionScope::Muc
            || session.status != MediaSessionStatus::Active
            || self.session_has_joined_participants(&binding.session_id)
        {
            return;
        }
        let now = Utc::now();
        session.status = MediaSessionStatus::Ended;
        session.updated_at = now;
        session.ended_at = Some(now);
        drop(session);
        self.remove_invite_references_for_session(&binding.session_id);
        self.remove_participants_for_session(&binding.session_id);
    }

    fn cleanup_expired_sessions_inner(&self) -> usize {
        let now = Utc::now();
        let terminal_cutoff = now - self.config.token_ttl;
        let active_cutoff = now - Duration::hours(MAX_MEDIA_ACTIVE_SESSION_IDLE_HOURS);
        let expired = self
            .sessions
            .iter()
            .filter_map(|entry| {
                let session = entry.value();
                ((session.status == MediaSessionStatus::Ended
                    && session
                        .ended_at
                        .is_some_and(|ended_at| ended_at <= terminal_cutoff))
                    || (session.status == MediaSessionStatus::Invited
                        && session.updated_at <= terminal_cutoff)
                    || (session.status == MediaSessionStatus::Active
                        && session.updated_at <= active_cutoff
                        && !self.session_has_joined_participants(entry.key())))
                    .then(|| entry.key().clone())
            })
            .collect::<Vec<_>>();
        let count = expired.len();
        for session_id in expired {
            self.remove_session(&session_id);
        }
        count
    }

    fn remove_session(&self, session_id: &MediaSessionId) {
        self.sessions.remove(session_id);
        self.remove_invite_references_for_session(session_id);
        self.remove_participants_for_session(session_id);
    }

    fn remove_participants_for_session(&self, session_id: &MediaSessionId) {
        let participants = self
            .participants
            .iter()
            .filter_map(|entry| (entry.key().0 == *session_id).then(|| entry.key().clone()))
            .collect::<Vec<_>>();
        for participant in participants {
            self.participants.remove(&participant);
        }
    }

    fn remove_invite_references_for_session(&self, session_id: &MediaSessionId) {
        let references = self
            .invite_references
            .iter()
            .filter_map(|entry| {
                (entry.value().session_id == *session_id).then(|| entry.key().clone())
            })
            .collect::<Vec<_>>();
        for reference in references {
            self.invite_references.remove(&reference);
        }
    }

    fn session_has_joined_participants(&self, session_id: &MediaSessionId) -> bool {
        self.participants.iter().any(|entry| {
            entry.key().0 == *session_id && entry.value().state == MediaParticipantState::Joined
        })
    }

    fn configured_turn_services(
        &self,
        requested_type: Option<&ExternalServiceType>,
        requester: Option<&FullJid>,
        credentials: bool,
    ) -> Vec<ExternalService> {
        if requested_type
            .is_some_and(|service_type| !matches!(service_type, ExternalServiceType::Turn))
        {
            return Vec::new();
        }

        let Some(host) = self.config.turn.host.as_deref() else {
            return Vec::new();
        };
        if !self.turn_credentials_available() {
            return Vec::new();
        }

        let mut services = Vec::new();
        if let Some(port) = self.config.turn.udp_port {
            let mut service = ExternalService::new(ExternalServiceType::Turn, host)
                .with_port(port)
                .with_transport(ExternalServiceTransport::Udp);
            service.restricted = Some(true);
            services.push(service);
        }
        if let Some(port) = self.config.turn.tcp_port {
            let mut service = ExternalService::new(ExternalServiceType::Turn, host)
                .with_port(port)
                .with_transport(ExternalServiceTransport::Tcp);
            service.restricted = Some(true);
            services.push(service);
        }

        if credentials {
            let Some(requester) = requester else {
                return Vec::new();
            };
            if !self.record_turn_credential_request(requester) {
                return Vec::new();
            }
            services
                .into_iter()
                .filter_map(|service| self.with_turn_credentials(service, requester).ok())
                .collect()
        } else {
            services
        }
    }

    fn with_turn_credentials(
        &self,
        mut service: ExternalService,
        requester: &FullJid,
    ) -> Result<ExternalService, MediaGatewayError> {
        let Some(shared_secret) = self.config.turn.shared_secret.as_ref() else {
            return Err(MediaGatewayError::LiveKitUnavailable);
        };
        let expires = Utc::now() + self.config.token_ttl;
        let username = format!("{}:{}", expires.timestamp(), requester);
        let mut mac = HmacSha1::new_from_slice(shared_secret.as_bytes())
            .map_err(|_| MediaGatewayError::LiveKitUnavailable)?;
        mac.update(username.as_bytes());
        let password = BASE64_STANDARD.encode(mac.finalize().into_bytes());
        service = service.with_credentials(username, password, Some(expires));
        service.restricted = Some(true);
        Ok(service)
    }

    fn turn_credentials_available(&self) -> bool {
        self.config.turn.shared_secret.is_some()
    }

    fn record_turn_credential_request(&self, requester: &FullJid) -> bool {
        let now = Utc::now();
        let cutoff = now - self.config.token_ttl;
        let mut entry = self
            .turn_credential_requests
            .entry(requester.to_bare())
            .or_default();
        entry.retain(|issued_at| *issued_at > cutoff);
        if entry.len() >= MAX_TURN_CREDENTIAL_REQUESTS_PER_TTL {
            return false;
        }
        entry.push(now);
        true
    }

    fn create_internal_token(
        &self,
        room_name: &LiveKitRoomName,
        livekit_identity: &LiveKitIdentity,
        can_publish: bool,
    ) -> Result<LiveKitAccessToken, MediaGatewayError> {
        let (api_key, api_secret) = self.config.token_inputs()?;
        let now = Utc::now();
        let exp = now + self.config.token_ttl;
        let claims = LiveKitClaims {
            issuer: api_key.as_str(),
            subject: livekit_identity.as_str(),
            not_before: now.timestamp(),
            expires_at: exp.timestamp(),
            video: LiveKitVideoGrant {
                room_join: true,
                room: room_name.as_str(),
                can_publish,
                can_subscribe: true,
                can_publish_data: false,
            },
        };
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(api_secret.as_bytes()),
        )
        .map_err(|_| MediaGatewayError::LiveKitUnavailable)?;
        Ok(LiveKitAccessToken(token))
    }
}

fn media_session_features() -> Vec<Feature> {
    vec![
        Feature::new(xep::xep0166::NS_JINGLE),
        Feature::new(xep::xep0167::NS_JINGLE_RTP),
        Feature::new(xep::xep0167::NS_JINGLE_RTP_AUDIO),
        Feature::new(xep::xep0167::NS_JINGLE_RTP_VIDEO),
        Feature::new(xep::xep0176::NS_JINGLE_ICE_UDP),
        Feature::new(xep::xep0320::NS_JINGLE_DTLS),
        Feature::new(xep::xep0338::NS_JINGLE_GROUPING),
        Feature::new(xep::xep0338::FEATURE_RFC5888_GROUPING),
    ]
}

fn gateway_jingle_jid_matches(jid: &Jid, media_domain: &str, sid: &str) -> bool {
    jid.clone().try_into_full().ok().is_some_and(|full| {
        full.to_bare().as_str() == media_domain && full.resource().to_string() == sid
    })
}

fn service_matches_request(candidate: &ExternalService, requested: &ExternalService) -> bool {
    candidate.service_type == requested.service_type
        && candidate.host == requested.host
        && requested
            .port
            .is_none_or(|port| candidate.port == Some(port))
        && requested
            .transport
            .as_ref()
            .is_none_or(|transport| candidate.transport.as_ref() == Some(transport))
}

fn session_matches_conversation(
    session: &MediaSession,
    scope: MediaSessionScope,
    anchor_jid: &Jid,
    creator_bare: &BareJid,
) -> bool {
    if session.scope != scope {
        return false;
    }
    match scope {
        MediaSessionScope::Muc => session.anchor_jid == *anchor_jid,
        MediaSessionScope::Direct => {
            let invitee_bare = anchor_jid.to_bare();
            let session_creator_bare = session.creator_jid.to_bare();
            let session_invitee_bare = session.anchor_jid.to_bare();
            CallInviteConversationKey::direct(session_creator_bare, session_invitee_bare)
                == CallInviteConversationKey::direct(creator_bare.clone(), invitee_bare)
        }
    }
}

fn accept_method_matches_session_id(method: &JoinMethod, session_id: &MediaSessionId) -> bool {
    matches!(method, JoinMethod::Jingle { sid, .. } if sid.as_str() == session_id.as_str())
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn optional_env_url(key: &str) -> Result<Option<Url>, String> {
    optional_env(key)
        .map(|value| Url::from_str(&value).map_err(|error| format!("{key} must be a URL: {error}")))
        .transpose()
}

fn optional_env_bool(key: &str) -> Option<bool> {
    optional_env(key)
        .map(|value| matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

#[derive(Debug, Serialize)]
struct LiveKitClaims<'a> {
    #[serde(rename = "iss")]
    issuer: &'a str,
    #[serde(rename = "sub")]
    subject: &'a str,
    #[serde(rename = "nbf")]
    not_before: i64,
    #[serde(rename = "exp")]
    expires_at: i64,
    video: LiveKitVideoGrant<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveKitVideoGrant<'a> {
    room_join: bool,
    room: &'a str,
    can_publish: bool,
    can_subscribe: bool,
    can_publish_data: bool,
}

pub type SharedMediaGateway = Arc<MediaGateway>;

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::xep::xep0482::{
        build_accept_element, build_invite_element, build_retract_element, CallInvite,
        CallInviteId, JingleSessionId, JoinMethod,
    };

    fn enabled_config() -> LiveKitConfig {
        LiveKitConfig {
            enabled: true,
            ws_url: Some("wss://livekit.example".parse().expect("url")),
            api_key: LiveKitApiKey::new("devkey"),
            api_secret: LiveKitApiSecret::new("devsecret"),
            room_prefix: LiveKitRoomPrefix::new("test"),
            token_ttl: Duration::minutes(5),
            turn: TurnServiceConfig {
                host: Some("turn.example".to_string()),
                udp_port: Some(3478),
                tcp_port: Some(3478),
                shared_secret: TurnSharedSecret::new("turn-secret"),
            },
        }
    }

    fn enabled_config_without_turn_shared_secret() -> LiveKitConfig {
        let mut config = enabled_config();
        config.turn.shared_secret = None;
        config
    }

    fn muc_conversation() -> CallInviteConversationKey {
        CallInviteConversationKey::muc("room@muc.example.test".parse().expect("jid"))
    }

    fn direct_conversation() -> CallInviteConversationKey {
        CallInviteConversationKey::direct(
            "alice@example.test".parse().expect("jid"),
            "bob@example.test".parse().expect("jid"),
        )
    }

    fn gateway_accept_method(session_id: &str) -> JoinMethod {
        JoinMethod::Jingle {
            sid: JingleSessionId::new(session_id).expect("sid"),
            jid: Some(
                format!("media.example.test/{session_id}")
                    .parse()
                    .expect("media jid"),
            ),
        }
    }

    fn gateway_invite_method(session_id: &str) -> JoinMethod {
        gateway_accept_method(session_id)
    }

    #[test]
    fn invite_session_accepts_explicit_media_component_jid() {
        let gateway = MediaGateway::new(enabled_config());
        let mut message = xmpp_parsers::message::Message::new(None);
        message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let anchor: Jid = "room@muc.example.test".parse().expect("jid");

        let session_id = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Muc,
                anchor,
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");

        assert_eq!(session_id.as_str(), "s1");
        let payload = xep0482::extract_call_invite_payload(&message).expect("payload");
        let CallInvitePayload::Invite(invite) = payload else {
            panic!("expected invite");
        };
        assert_eq!(
            invite.methods[0].jingle_jid().map(ToString::to_string),
            Some("media.example.test/s1".to_string())
        );
    }

    #[test]
    fn invite_session_preserves_additional_join_methods() {
        let gateway = MediaGateway::new(enabled_config());
        let mut message = xmpp_parsers::message::Message::new(None);
        message.payloads.push(build_invite_element(
            &CallInvite::new()
                .with_method(gateway_invite_method("s1"))
                .with_method(JoinMethod::External {
                    uri: "https://calls.example.test/s1".parse().expect("uri"),
                }),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");

        let session_id = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");

        assert_eq!(session_id.as_str(), "s1");
        let payload = xep0482::extract_call_invite_payload(&message).expect("payload");
        let CallInvitePayload::Invite(invite) = payload else {
            panic!("expected invite");
        };
        assert_eq!(invite.methods.len(), 2);
        assert!(matches!(
            invite.methods.get(1),
            Some(JoinMethod::External { .. })
        ));
    }

    #[test]
    fn invite_session_rejects_oversized_sid() {
        let gateway = MediaGateway::new(enabled_config());
        let oversized = "a".repeat(MAX_MEDIA_SESSION_ID_LEN + 1);
        let mut message = xmpp_parsers::message::Message::new(None);
        message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(JoinMethod::Jingle {
                sid: JingleSessionId::new(oversized.clone()).expect("sid"),
                jid: Some(
                    format!("media.example.test/{oversized}")
                        .parse()
                        .expect("media jid"),
                ),
            }),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");

        let error = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect_err("oversized session ids are rejected");

        assert_eq!(error, MediaGatewayError::InvalidSessionId);
    }

    #[test]
    fn invite_session_enforces_conversation_capacity() {
        let gateway = MediaGateway::with_limits(
            enabled_config(),
            MediaSessionLimits {
                max_total: 16,
                max_per_creator: 16,
                max_per_conversation: 1,
            },
        );
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let anchor: Jid = "room@muc.example.test".parse().expect("jid");

        for (sid, expected) in [
            ("s1", Ok(())),
            ("s2", Err(MediaGatewayError::CapacityExceeded)),
        ] {
            let mut message = xmpp_parsers::message::Message::new(None);
            message.payloads.push(build_invite_element(
                &CallInvite::new().with_method(gateway_invite_method(sid)),
            ));
            let result = gateway
                .ensure_invite_session(
                    &mut message,
                    MediaSessionScope::Muc,
                    anchor.clone(),
                    &creator,
                    "media.example.test",
                )
                .map(|_| ());
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn invite_session_capacity_is_atomic_under_parallel_creation() {
        let gateway = std::sync::Arc::new(MediaGateway::with_limits(
            enabled_config(),
            MediaSessionLimits {
                max_total: 16,
                max_per_creator: 16,
                max_per_conversation: 1,
            },
        ));
        let workers = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let handles = (0..workers)
            .map(|index| {
                let gateway = std::sync::Arc::clone(&gateway);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let sid = format!("race-{index}");
                    let mut message = xmpp_parsers::message::Message::new(None);
                    message.payloads.push(build_invite_element(
                        &CallInvite::new().with_method(gateway_invite_method(&sid)),
                    ));
                    let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
                    gateway
                        .ensure_invite_session(
                            &mut message,
                            MediaSessionScope::Muc,
                            "room@muc.example.test".parse().expect("jid"),
                            &creator,
                            "media.example.test",
                        )
                        .map(|session_id| session_id.is_some())
                })
            })
            .collect::<Vec<_>>();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("worker panicked"))
            .collect::<Vec<_>>();
        let successes = results
            .iter()
            .filter(|result| matches!(result, Ok(true)))
            .count();
        let capacity_denials = results
            .iter()
            .filter(|result| matches!(result, Err(MediaGatewayError::CapacityExceeded)))
            .count();

        assert_eq!(successes, 1);
        assert_eq!(capacity_denials, workers - 1);
    }

    #[test]
    fn cleanup_expired_sessions_removes_stale_invites_without_new_invite() {
        let mut config = enabled_config();
        config.token_ttl = Duration::seconds(-1);
        let gateway = MediaGateway::new(config);
        let mut message = xmpp_parsers::message::Message::new(None);
        message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("session id");

        assert!(gateway.get_session(&session_id).is_some());
        assert_eq!(gateway.cleanup_expired_sessions(), 1);
        assert!(gateway.get_session(&session_id).is_none());
    }

    #[test]
    fn cleanup_expired_sessions_removes_stale_active_sessions() {
        let gateway = MediaGateway::new(enabled_config());
        let mut message = xmpp_parsers::message::Message::new(None);
        message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("stale-active")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("session id");
        gateway
            .upsert_participant(&session_id, &creator, MediaParticipantState::Joined)
            .expect("participant");
        if let Some(mut session) = gateway.sessions.get_mut(&session_id) {
            session.updated_at =
                Utc::now() - Duration::hours(MAX_MEDIA_ACTIVE_SESSION_IDLE_HOURS + 1);
        }

        assert_eq!(gateway.cleanup_expired_sessions(), 0);
        assert!(gateway.get_session(&session_id).is_some());

        gateway.mark_participant_left(&session_id, &creator);

        assert_eq!(gateway.cleanup_expired_sessions(), 1);
        assert!(gateway.get_session(&session_id).is_none());
    }

    #[test]
    fn invite_session_rejects_multiple_waddle_jingle_methods() {
        let gateway = MediaGateway::new(enabled_config());
        let mut message = xmpp_parsers::message::Message::new(None);
        message.payloads.push(build_invite_element(
            &CallInvite::new()
                .with_method(gateway_invite_method("s1"))
                .with_method(gateway_invite_method("s2")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");

        let error = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect_err("multiple methods are unsupported");

        assert_eq!(error, MediaGatewayError::UnsupportedInviteMethod);
    }

    #[test]
    fn invite_session_ignores_non_gateway_join_jid() {
        let gateway = MediaGateway::new(enabled_config());
        let mut message = xmpp_parsers::message::Message::new(None);
        message
            .payloads
            .push(build_invite_element(&CallInvite::new().with_method(
                JoinMethod::Jingle {
                    sid: JingleSessionId::new("s1").expect("sid"),
                    jid: Some("other.example.test/s1".parse().expect("jid")),
                },
            )));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");

        let session_id = gateway
            .ensure_invite_session(
                &mut message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("non-gateway invite should pass through");

        assert!(session_id.is_none());
    }

    #[test]
    fn lifecycle_uses_call_invite_reference_id() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let anchor: Jid = "room@muc.example.test".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Muc,
                anchor,
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&session_id, muc_conversation(), "room-stanza-id")
            .expect("reference binding");

        let mut accept_message = xmpp_parsers::message::Message::new(None);
        let invite_id = CallInviteId::new("room-stanza-id").expect("invite id");
        accept_message.payloads.push(build_accept_element(
            &invite_id,
            &gateway_accept_method("s1"),
        ));
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");

        gateway.observe_call_lifecycle(&accept_message, muc_conversation(), &participant);

        assert!(gateway
            .participants
            .get(&(session_id, participant))
            .is_some_and(|entry| entry.state == MediaParticipantState::Joined));
    }

    #[test]
    fn retract_ends_session_and_invalidates_invite_reference() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let anchor: Jid = "room@muc.example.test".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Muc,
                anchor,
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        let invite_id = CallInviteId::new("room-stanza-id").expect("invite id");
        gateway
            .bind_invite_reference(&session_id, muc_conversation(), invite_id.as_str())
            .expect("reference binding");

        let mut retract_message = xmpp_parsers::message::Message::new(None);
        retract_message
            .payloads
            .push(build_retract_element(&invite_id));

        gateway.observe_call_lifecycle(&retract_message, muc_conversation(), &creator);

        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
        assert_eq!(
            gateway
                .get_session_for_invite_reference(muc_conversation(), invite_id.as_str())
                .expect("lookup"),
            None
        );
    }

    #[test]
    fn duplicate_sid_cannot_rescope_existing_session() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session");

        let mut second_invite = xmpp_parsers::message::Message::new(None);
        second_invite.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let other_creator: FullJid = "mallory@example.test/laptop".parse().expect("jid");
        let error = gateway
            .ensure_invite_session(
                &mut second_invite,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &other_creator,
                "media.example.test",
            )
            .expect_err("same sid cannot be re-scoped");

        assert_eq!(error, MediaGatewayError::Forbidden);
    }

    #[test]
    fn ended_session_id_is_reserved_until_terminal_cleanup() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("reuse-s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway.end_session(&session_id);

        let mut second_invite = xmpp_parsers::message::Message::new(None);
        second_invite.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("reuse-s1")),
        ));
        let immediate_error = gateway
            .ensure_invite_session(
                &mut second_invite,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect_err("terminal session should reserve sid until cleanup");
        assert_eq!(immediate_error, MediaGatewayError::SessionEnded);

        if let Some(mut session) = gateway.sessions.get_mut(&session_id) {
            session.ended_at = Some(Utc::now() - gateway.config.token_ttl - Duration::seconds(1));
        }

        let mut cleaned_up_invite = xmpp_parsers::message::Message::new(None);
        cleaned_up_invite.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("reuse-s1")),
        ));
        let recreated = gateway
            .ensure_invite_session(
                &mut cleaned_up_invite,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");

        assert_eq!(recreated.as_str(), "reuse-s1");
        assert_eq!(
            gateway.get_session(&recreated).expect("session").status,
            MediaSessionStatus::Invited
        );
    }

    #[test]
    fn direct_reject_ends_session_and_invalidates_reference() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("direct-s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        let invite_id = CallInviteId::new("origin-id-1").expect("invite id");
        gateway
            .bind_invite_reference(&session_id, direct_conversation(), invite_id.as_str())
            .expect("reference binding");

        let mut reject_message = xmpp_parsers::message::Message::new(None);
        reject_message
            .payloads
            .push(xep0482::build_reject_element(&invite_id));
        let invitee: FullJid = "bob@example.test/laptop".parse().expect("jid");

        gateway.observe_call_lifecycle(&reject_message, direct_conversation(), &invitee);

        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
        assert_eq!(
            gateway
                .get_session_for_invite_reference(direct_conversation(), invite_id.as_str())
                .expect("lookup"),
            None
        );
    }

    #[test]
    fn lifecycle_observation_uses_scoped_invite_reference() {
        let gateway = MediaGateway::new(enabled_config());
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let invite_id = CallInviteId::new("same-reference-id").expect("invite id");

        let mut muc_invite = xmpp_parsers::message::Message::new(None);
        muc_invite.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("muc-same-reference")),
        ));
        let muc_session_id = gateway
            .ensure_invite_session(
                &mut muc_invite,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&muc_session_id, muc_conversation(), invite_id.as_str())
            .expect("muc binding");

        let mut direct_invite = xmpp_parsers::message::Message::new(None);
        direct_invite.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("direct-same-reference")),
        ));
        let direct_session_id = gateway
            .ensure_invite_session(
                &mut direct_invite,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(
                &direct_session_id,
                direct_conversation(),
                invite_id.as_str(),
            )
            .expect("direct binding");

        let mut retract_message = xmpp_parsers::message::Message::new(None);
        retract_message
            .payloads
            .push(build_retract_element(&invite_id));

        gateway.observe_call_lifecycle(&retract_message, direct_conversation(), &creator);

        assert_eq!(
            gateway
                .get_session(&direct_session_id)
                .expect("direct session")
                .status,
            MediaSessionStatus::Ended
        );
        assert_eq!(
            gateway
                .get_session(&muc_session_id)
                .expect("muc session")
                .status,
            MediaSessionStatus::Invited
        );
    }

    #[test]
    fn extdisco_credentials_use_turn_rest_hmac_sha1_shape() {
        let gateway = MediaGateway::new(enabled_config());
        let requester: FullJid = "alice@example.test/phone".parse().expect("jid");
        let request = ExtDiscoRequest::Services {
            service_type: Some(ExternalServiceType::Turn),
        };
        let services = gateway.services_for_request(&request, Some(&requester));
        assert_eq!(services.len(), 2);
        assert!(services
            .iter()
            .all(|service| service.restricted == Some(true)));

        let credential_request = ExtDiscoRequest::Credentials {
            service: services[0].clone(),
        };
        let credentials = gateway.services_for_request(&credential_request, Some(&requester));
        assert_eq!(credentials.len(), 1);
        assert!(credentials[0]
            .username
            .as_ref()
            .is_some_and(|username| username.as_str().ends_with(":alice@example.test/phone")));
        assert!(credentials[0].password.is_some());
        assert_eq!(credentials[0].restricted, Some(true));
    }

    #[test]
    fn extdisco_omits_turn_services_without_shared_secret() {
        let gateway = MediaGateway::new(enabled_config_without_turn_shared_secret());
        let requester: FullJid = "alice@example.test/phone".parse().expect("jid");
        let request = ExtDiscoRequest::Services {
            service_type: Some(ExternalServiceType::Turn),
        };
        let services = gateway.services_for_request(&request, Some(&requester));
        assert!(services.is_empty());
    }

    #[test]
    fn extdisco_turn_credentials_are_rate_limited_per_requester() {
        let gateway = MediaGateway::new(enabled_config());
        let requester: FullJid = "alice@example.test/phone".parse().expect("jid");
        let service_request = ExtDiscoRequest::Services {
            service_type: Some(ExternalServiceType::Turn),
        };
        let service = gateway
            .services_for_request(&service_request, Some(&requester))
            .into_iter()
            .next()
            .expect("turn service");
        let credential_request = ExtDiscoRequest::Credentials { service };

        for _ in 0..MAX_TURN_CREDENTIAL_REQUESTS_PER_TTL {
            assert_eq!(
                gateway
                    .services_for_request(&credential_request, Some(&requester))
                    .len(),
                1
            );
        }
        assert!(gateway
            .services_for_request(&credential_request, Some(&requester))
            .is_empty());
    }

    #[test]
    fn jingle_session_terminate_ends_session_and_returns_ack() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        let iq = Iq {
            from: Some(jid::Jid::from(creator.clone())),
            to: Some("media.example.test/s1".parse().expect("jid")),
            id: "jingle-end-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(
                xmpp_parsers::minidom::Element::builder("jingle", xep0166::NS_JINGLE)
                    .attr("action", "session-terminate")
                    .attr("sid", "s1")
                    .build(),
            ),
        };

        let response = gateway.handle_jingle_iq(&iq, &creator).expect("ack");

        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(None)
        ));
        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
    }

    #[test]
    fn unsupported_jingle_actions_return_bridge_unavailable_not_bad_request() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("unsupported-app")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        let unsupported_description =
            xmpp_parsers::minidom::Element::builder("description", "urn:example:jingle:app:0")
                .build();
        let transport =
            xmpp_parsers::minidom::Element::builder("transport", xmpp_parsers::ns::JINGLE_ICE_UDP)
                .attr("ufrag", "u")
                .attr("pwd", "p")
                .build();
        let iq = Iq {
            from: Some(jid::Jid::from(creator.clone())),
            to: Some("media.example.test/unsupported-app".parse().expect("jid")),
            id: "jingle-init-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(
                xmpp_parsers::minidom::Element::builder("jingle", xep0166::NS_JINGLE)
                    .attr("action", "session-initiate")
                    .attr("sid", "unsupported-app")
                    .append(
                        xmpp_parsers::minidom::Element::builder("content", xep0166::NS_JINGLE)
                            .attr("creator", "initiator")
                            .attr("name", "audio")
                            .append(unsupported_description)
                            .append(transport)
                            .build(),
                    )
                    .build(),
            ),
        };

        let error = gateway
            .handle_jingle_iq(&iq, &creator)
            .expect_err("bridge should be unavailable");

        assert_eq!(error, MediaGatewayError::JingleBridgeUnavailable);
    }

    #[test]
    fn muc_jingle_session_terminate_does_not_end_room_call() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("muc-s1")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&session_id, muc_conversation(), "room-stanza-id")
            .expect("reference binding");
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");
        gateway
            .upsert_participant(&session_id, &participant, MediaParticipantState::Joined)
            .expect("participant");
        let remaining_participant: FullJid = "carol@example.test/laptop".parse().expect("jid");
        gateway
            .upsert_participant(
                &session_id,
                &remaining_participant,
                MediaParticipantState::Joined,
            )
            .expect("remaining participant");
        let iq = Iq {
            from: Some(jid::Jid::from(participant.clone())),
            to: Some("media.example.test/muc-s1".parse().expect("jid")),
            id: "jingle-end-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(
                xmpp_parsers::minidom::Element::builder("jingle", xep0166::NS_JINGLE)
                    .attr("action", "session-terminate")
                    .attr("sid", "muc-s1")
                    .build(),
            ),
        };

        let response = gateway.handle_jingle_iq(&iq, &participant).expect("ack");

        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(None)
        ));
        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Active
        );
        assert!(gateway
            .participants
            .get(&(session_id, participant))
            .is_some_and(|entry| entry.state == MediaParticipantState::Left));
    }

    #[test]
    fn muc_call_ends_when_last_participant_leaves() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("muc-last")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Muc,
                "room@muc.example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&session_id, muc_conversation(), "room-stanza-id")
            .expect("reference binding");
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");
        gateway
            .upsert_participant(&session_id, &participant, MediaParticipantState::Joined)
            .expect("participant");

        gateway.mark_muc_participant_left(
            &"room@muc.example.test".parse().expect("jid"),
            &participant,
        );

        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
        assert_eq!(
            gateway
                .get_session_for_invite_reference(muc_conversation(), "room-stanza-id")
                .expect("lookup"),
            None
        );
    }

    #[test]
    fn ended_session_cannot_be_reactivated_by_participant_upsert() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("ended-upsert")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");

        gateway.end_session(&session_id);
        let error = gateway
            .upsert_participant(&session_id, &participant, MediaParticipantState::Joined)
            .expect_err("ended session rejects participants");

        assert_eq!(error, MediaGatewayError::SessionEnded);
        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
        assert!(gateway
            .participants
            .get(&(session_id, participant))
            .is_none());
    }

    #[test]
    fn stale_invite_reference_binding_cannot_affect_recreated_sid() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("stale-binding")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&session_id, direct_conversation(), "origin-stale")
            .expect("reference binding");
        let reference_key =
            CallInviteReferenceKey::new(direct_conversation(), "origin-stale").expect("key");
        let stale_binding = gateway
            .invite_references
            .get(&reference_key)
            .expect("binding")
            .value()
            .clone();

        gateway.end_session(&session_id);
        if let Some(mut session) = gateway.sessions.get_mut(&session_id) {
            session.ended_at = Some(Utc::now() - gateway.config.token_ttl - Duration::seconds(1));
        }
        let mut recreated_invite = xmpp_parsers::message::Message::new(None);
        recreated_invite.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("stale-binding")),
        ));
        gateway
            .ensure_invite_session(
                &mut recreated_invite,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("recreated")
            .expect("session");

        let error = gateway
            .upsert_participant_for_reference_binding(
                &stale_binding,
                &participant,
                MediaParticipantState::Joined,
            )
            .expect_err("stale binding must not upsert recreated session");

        assert_eq!(error, MediaGatewayError::UnknownSession);
        assert!(gateway
            .participants
            .get(&(session_id, participant))
            .is_none());
    }

    #[test]
    fn direct_call_ends_when_creator_disconnects() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("direct-creator-disconnect")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&session_id, direct_conversation(), "origin-id-creator")
            .expect("reference binding");
        gateway
            .upsert_participant(&session_id, &participant, MediaParticipantState::Joined)
            .expect("participant");

        gateway.mark_participant_disconnected(&creator);

        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
        assert_eq!(
            gateway
                .get_session_for_invite_reference(direct_conversation(), "origin-id-creator")
                .expect("lookup"),
            None
        );
    }

    #[test]
    fn direct_call_ends_when_joined_participant_disconnects() {
        let gateway = MediaGateway::new(enabled_config());
        let mut invite_message = xmpp_parsers::message::Message::new(None);
        invite_message.payloads.push(build_invite_element(
            &CallInvite::new().with_method(gateway_invite_method("direct-disconnect")),
        ));
        let creator: FullJid = "alice@example.test/phone".parse().expect("jid");
        let participant: FullJid = "bob@example.test/laptop".parse().expect("jid");
        let session_id = gateway
            .ensure_invite_session(
                &mut invite_message,
                MediaSessionScope::Direct,
                "bob@example.test".parse().expect("jid"),
                &creator,
                "media.example.test",
            )
            .expect("session")
            .expect("created");
        gateway
            .bind_invite_reference(&session_id, direct_conversation(), "origin-id-1")
            .expect("reference binding");
        gateway
            .upsert_participant(&session_id, &participant, MediaParticipantState::Joined)
            .expect("participant");

        gateway.mark_participant_disconnected(&participant);

        assert_eq!(
            gateway.get_session(&session_id).expect("session").status,
            MediaSessionStatus::Ended
        );
        assert_eq!(
            gateway
                .get_session_for_invite_reference(direct_conversation(), "origin-id-1")
                .expect("lookup"),
            None
        );
    }

    #[test]
    fn livekit_token_contains_room_scoped_video_grant() {
        let gateway = MediaGateway::new(enabled_config());
        let room = LiveKitRoomName::new("test-s1").expect("room");
        let identity = LiveKitIdentity("waddle:s1:alice@example.test/phone".to_string());
        let token = gateway
            .create_internal_token(&room, &identity, true)
            .expect("token");
        let mut validation = jsonwebtoken::Validation::new(Algorithm::HS256);
        validation.insecure_disable_signature_validation();
        let decoded = jsonwebtoken::decode::<serde_json::Value>(
            &token.0,
            &jsonwebtoken::DecodingKey::from_secret(&[]),
            &validation,
        )
        .expect("decode");
        assert_eq!(decoded.claims["iss"], "devkey");
        assert_eq!(decoded.claims["video"]["room"], "test-s1");
        assert_eq!(decoded.claims["video"]["roomJoin"], true);
    }
}
