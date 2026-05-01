//! Session Registry for XEP-0198 Stream Management
//!
//! This module provides server-side storage for detached stream sessions,
//! allowing streams to be resumed after disconnection.
//!
//! When a client disconnects with SM enabled and resumption requested,
//! the server stores the session state. When the client reconnects with
//! a resume request, the server can restore the session.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use thiserror::Error;
use tracing::debug;
use xmpp_parsers::presence::Show;

use crate::Stanza;

/// Default session timeout (5 minutes)
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 300;

/// Maximum number of sessions to store
pub const DEFAULT_MAX_SESSIONS: usize = 10000;

/// Error type for SM session registry operations.
#[derive(Debug, Error)]
pub enum SmRegistryError {
    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Session expired")]
    Expired,

    #[error("Registry at capacity")]
    AtCapacity,

    #[error("Internal error: {0}")]
    Internal(String),
}

/// A detached stream management session.
///
/// Contains all the state needed to resume a stream after disconnection.
#[derive(Debug, Clone)]
pub struct DetachedSession {
    /// The unique stream ID
    pub stream_id: String,
    /// Authenticated user identifier.
    pub user_id: String,
    /// The full JID of the session owner
    pub jid: FullJid,
    /// Server's inbound stanza count at detach time
    pub inbound_count: u32,
    /// Server's outbound stanza count at detach time
    pub outbound_count: u32,
    /// Last acknowledged outbound stanza count
    pub last_acked: u32,
    /// Unacknowledged stanzas (sequence, xml)
    pub unacked_stanzas: Vec<(u32, String)>,
    /// Maximum resumption time in seconds
    pub max_resume_time: Option<u32>,
    /// When the session was detached
    pub detached_at: Instant,
    /// XEP-0280 Message Carbons opt-in at detach time.
    ///
    /// XEP-0198 §5 defines `<resumed/>` as continuing the same stream, so any
    /// per-stream add-ons the client previously enabled (here: carbons) must
    /// survive resumption without requiring the client to re-negotiate them.
    pub carbons_enabled: bool,
    /// RFC 6121 roster-interest state at detach time.
    ///
    /// XEP-0198 resumption continues the same stream, so an already
    /// interested resource remains interested after a successful resume.
    pub roster_interested: bool,
    /// Whether the resource had sent available presence at detach time.
    ///
    /// Presence side effects required by RFC 6121 still apply to detached
    /// XEP-0198 streams that were available when the transport dropped.
    pub presence_available: bool,
    /// Last advertised show value while available.
    pub presence_show: Option<Show>,
    /// Last advertised status text while available.
    pub presence_status: Option<String>,
    /// Last advertised priority while available.
    pub presence_priority: i8,
}

impl DetachedSession {
    /// Check if the session has expired.
    pub fn is_expired(&self) -> bool {
        let max_time = self
            .max_resume_time
            .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS as u32);
        self.detached_at.elapsed() > Duration::from_secs(max_time as u64)
    }

    /// Get remaining time until expiration.
    pub fn remaining_time(&self) -> Duration {
        let max_time = Duration::from_secs(
            self.max_resume_time
                .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS as u32) as u64,
        );
        max_time.saturating_sub(self.detached_at.elapsed())
    }

    /// Get the number of stanzas that would need to be resent.
    ///
    /// `client_h` is what the client reports as last received.
    pub fn stanzas_to_resend_count(&self, client_h: u32) -> usize {
        self.unacked_stanzas
            .iter()
            .filter(|(seq, _)| sequence_gt(*seq, client_h))
            .count()
    }

    /// Get the XML payloads that must be resent to a client reporting `h`.
    pub fn stanzas_to_resend(&self, client_h: u32) -> Vec<String> {
        self.unacked_stanzas
            .iter()
            .filter(|(seq, _)| sequence_gt(*seq, client_h))
            .map(|(_, xml)| xml.clone())
            .collect()
    }

    /// Record an outbound stanza while this stream is detached.
    pub fn record_detached_outbound(&mut self, stanza_xml: String) {
        self.outbound_count = self.outbound_count.wrapping_add(1);
        if self.unacked_stanzas.len() >= super::DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            self.unacked_stanzas.remove(0);
        }
        self.unacked_stanzas.push((self.outbound_count, stanza_xml));
    }

    pub fn record_detached_outbound_at(&mut self, sequence: u32, stanza_xml: String) {
        self.outbound_count = self.outbound_count.max(sequence);
        if self
            .unacked_stanzas
            .iter()
            .any(|(existing_sequence, _)| *existing_sequence == sequence)
        {
            return;
        }
        if self.unacked_stanzas.len() >= super::DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            self.unacked_stanzas.remove(0);
        }
        self.unacked_stanzas.push((sequence, stanza_xml));
        self.unacked_stanzas
            .sort_by_key(|(existing_sequence, _)| *existing_sequence);
    }
}

/// Trait for SM session registries.
///
/// Implementations can be in-memory (for single-node) or distributed
/// (for clustered deployments).
#[async_trait]
pub trait SmSessionRegistry: Send + Sync {
    /// Store a detached session.
    ///
    /// The session can be retrieved later using `take_session` with the stream_id.
    async fn store_session(&self, session: DetachedSession) -> Result<(), SmRegistryError>;

    /// Take (retrieve and remove) a session by stream ID.
    ///
    /// Returns the session if found and not expired, removing it from storage.
    /// This prevents the same session from being resumed twice.
    async fn take_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError>;

    /// Peek at a session without removing it.
    ///
    /// Useful for checking if a session exists before attempting resume.
    async fn peek_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError>;

    /// Clean up expired sessions.
    ///
    /// Returns the number of sessions removed.
    async fn cleanup_expired(&self) -> Result<usize, SmRegistryError>;

    /// Get the number of stored sessions.
    async fn session_count(&self) -> usize;

    /// Remove every unacked outbound `<message/>` stanza in stored
    /// sessions whose identity matches a XEP-0424 / XEP-0425 tombstone.
    /// Called when a tombstone is applied so a recipient mid-resume does
    /// not replay the pre-scrub stanza on the wire.
    ///
    /// `target_id` matches either the cached message's wire `id`
    /// attribute (typical for 1:1 retractions targeting the original
    /// message id) **or** any XEP-0359 `<stanza-id id='…'/>` child
    /// (typical for groupchat retractions that key by the room's
    /// stanza-id per the "archive id == wire stanza-id" invariant).
    ///
    /// `archive_jid` scopes the match to a specific conversation: a
    /// cached message is only removed if its `from` or `to` bare-equals
    /// `archive_jid`. This prevents cross-conversation collateral
    /// damage when two clients independently reuse a short message id
    /// in different chats — without scoping, retracting "msg-1" in one
    /// chat would silently delete unrelated "msg-1" stanzas queued for
    /// other recipients.
    ///
    /// Returns the number of stanza entries removed across all stored
    /// sessions. Default impl is a no-op so registry implementations
    /// can opt in incrementally; the in-memory implementation
    /// overrides it.
    async fn scrub_unacked_for_tombstone(
        &self,
        _target_id: &str,
        _archive_jid: &str,
    ) -> Result<usize, SmRegistryError> {
        Ok(0)
    }
}

#[derive(Debug, Clone)]
pub enum SmClaimCompletion {
    Resumed(DetachedSession),
    Expired(DetachedSession),
}

/// In-memory implementation of the SM session registry.
///
/// Suitable for single-node deployments. For clustered deployments,
/// use a distributed implementation backed by Redis or similar.
#[derive(Debug)]
pub struct InMemorySmSessionRegistry {
    sessions: RwLock<HashMap<String, DetachedSession>>,
    claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    max_sessions: usize,
}

impl Default for InMemorySmSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySmSessionRegistry {
    /// Create a new in-memory registry with default settings.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            claimed_sessions: RwLock::new(HashMap::new()),
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
            max_sessions,
        }
    }
}

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(&self, session: DetachedSession) -> Result<(), SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let stream_id = session.stream_id.clone();
        let jid = session.jid.clone();
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        sessions.retain(|existing_stream_id, existing| {
            existing_stream_id == &stream_id || existing.jid != jid
        });
        claimed.retain(|existing_stream_id, existing| {
            existing_stream_id != &stream_id && existing.jid != jid
        });

        if sessions.len() >= self.max_sessions {
            // Remove oldest session
            if let Some(oldest_key) = sessions
                .iter()
                .min_by_key(|(_, s)| s.detached_at)
                .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest_key);
                debug!(stream_id = %oldest_key, "Evicted oldest SM session to make room");
            }
        }

        sessions.insert(stream_id.clone(), session);
        let count = sessions.len();
        drop(claimed);
        drop(sessions);

        debug!(stream_id = %stream_id, count = count, "Stored detached SM session");
        Ok(())
    }

    async fn take_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let removed = match sessions.remove(stream_id) {
            Some(session) => {
                if session.is_expired() {
                    debug!(stream_id = %stream_id, "SM session found but expired");
                    None
                } else {
                    debug!(stream_id = %stream_id, "Retrieved and removed SM session");
                    Some(session)
                }
            }
            None => {
                debug!(stream_id = %stream_id, "SM session not found");
                None
            }
        };
        drop(sessions);
        self.claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        Ok(removed)
    }

    async fn peek_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        match sessions.get(stream_id) {
            Some(session) => {
                if session.is_expired() {
                    Ok(None)
                } else {
                    Ok(Some(session.clone()))
                }
            }
            None => Ok(None),
        }
    }

    async fn cleanup_expired(&self) -> Result<usize, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        Ok(drain_expired_internal(&mut sessions).len())
    }

    async fn session_count(&self) -> usize {
        self.sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    async fn scrub_unacked_for_tombstone(
        &self,
        target_id: &str,
        archive_jid: &str,
    ) -> Result<usize, SmRegistryError> {
        let mut removed_total = 0usize;
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            removed_total += scrub_session_unacked(session, target_id, archive_jid);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            removed_total += scrub_session_unacked(session, target_id, archive_jid);
        }
        Ok(removed_total)
    }
}

/// Strip every unacked outbound `<message/>` entry that matches a
/// XEP-0424 / XEP-0425 tombstone. Returns the number of entries
/// removed.
///
/// A cached message is removed iff:
///   1. it is a `<message>` element,
///   2. its `from` or `to` attribute bare-equals `archive_jid` (scope
///      guard — prevents cross-conversation collateral damage when
///      short message ids collide across chats), AND
///   3. either its wire `id` attribute matches `target_id` (1:1 case)
///      or any child `<stanza-id id='…'/>` matches `target_id`
///      (groupchat case where the retraction keyed by the room's
///      XEP-0359 stamp per the "archive id == wire stanza-id"
///      invariant).
///
/// Parse errors and non-message frames are skipped silently — only
/// matching messages are removed.
fn scrub_session_unacked(
    session: &mut DetachedSession,
    target_id: &str,
    archive_jid: &str,
) -> usize {
    let before = session.unacked_stanzas.len();
    session
        .unacked_stanzas
        .retain(|(_, xml)| match xml.parse::<minidom::Element>() {
            Ok(el) => !cached_message_matches_tombstone(&el, target_id, archive_jid),
            Err(_) => true,
        });
    before - session.unacked_stanzas.len()
}

fn cached_message_matches_tombstone(
    el: &minidom::Element,
    target_id: &str,
    archive_jid: &str,
) -> bool {
    if el.name() != "message" {
        return false;
    }
    let in_scope = el
        .attr("from")
        .map(|s| jid_bare_equals(s, archive_jid))
        .unwrap_or(false)
        || el
            .attr("to")
            .map(|s| jid_bare_equals(s, archive_jid))
            .unwrap_or(false);
    if !in_scope {
        return false;
    }
    if el.attr("id") == Some(target_id) {
        return true;
    }
    el.children()
        .any(|c| c.name() == "stanza-id" && c.attr("id") == Some(target_id))
}

fn jid_bare_equals(jid_str: &str, archive_jid: &str) -> bool {
    match jid_str.parse::<jid::Jid>() {
        Ok(jid) => jid.to_bare().to_string() == archive_jid,
        Err(_) => false,
    }
}

impl InMemorySmSessionRegistry {
    fn stanza_to_replay_xml(stanza: &Stanza) -> String {
        let element = stanza.to_element();
        let mut buffer = Vec::new();
        element
            .write_to(&mut buffer)
            .expect("serializing typed stanza should not fail");
        String::from_utf8(buffer).expect("serialized typed stanza is UTF-8")
    }

    /// Remove every expired session and return the detached state in full.
    ///
    /// Callers (notably the server-side janitor) need the JID and stream id
    /// of each expired session so they can run associated cleanup —
    /// removing MUC occupants, evicting routing entries, and discarding
    /// sidecar auth context. `cleanup_expired` only returns a count, which
    /// isn't enough for that work.
    pub async fn drain_expired(&self) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        Ok(drain_expired_internal(&mut sessions))
    }

    /// Atomically claim a resumable session for a single resume attempt.
    ///
    /// Claimed sessions stay writable by detached fanout so stanzas routed
    /// during the claim-to-registration handoff can be merged into the final
    /// replay batch before the claim is completed.
    pub async fn claim_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let Some(session) = sessions.remove(stream_id) else {
            return Ok(None);
        };
        if session.is_expired() {
            return Ok(None);
        }
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if claimed.contains_key(stream_id) {
            sessions.insert(stream_id.to_string(), session);
            return Ok(None);
        }
        claimed.insert(stream_id.to_string(), session.clone());
        Ok(Some(session))
    }

    /// Release a previously claimed session without consuming it.
    pub async fn release_claim(&self, stream_id: &str) -> Result<(), SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let session = claimed.remove(stream_id);
        if let Some(session) = session {
            if !session.is_expired() {
                sessions.insert(stream_id.to_string(), session);
            }
        }
        Ok(())
    }

    /// Complete a previously claimed session, returning the claimed copy with
    /// any stanzas recorded during the handoff and removing detached replay
    /// eligibility from the registry.
    pub async fn complete_claim(
        &self,
        stream_id: &str,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        Ok(self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id)
            .map(|session| {
                if session.is_expired() {
                    SmClaimCompletion::Expired(session)
                } else {
                    SmClaimCompletion::Resumed(session)
                }
            }))
    }

    /// Remove a stored detached session only if it has not been claimed by a
    /// resume attempt.
    pub async fn remove_stored_session_if_unclaimed(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .contains_key(stream_id)
        {
            return Ok(None);
        }
        Ok(sessions.remove(stream_id))
    }

    /// Invalidate detached sessions for a FullJID after a fresh bind has
    /// replaced that stream identity.
    pub async fn invalidate_sessions_for_jid(
        &self,
        jid: &FullJid,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let mut removed = Vec::new();
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let matching_streams: Vec<_> = sessions
            .iter()
            .filter(|(_, session)| session.jid == *jid)
            .map(|(stream_id, _)| stream_id.clone())
            .collect();
        for stream_id in matching_streams {
            if let Some(session) = sessions.remove(&stream_id) {
                removed.push(session);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let matching_streams: Vec<_> = claimed
            .iter()
            .filter(|(_, session)| session.jid == *jid)
            .map(|(stream_id, _)| stream_id.clone())
            .collect();
        for stream_id in matching_streams {
            if let Some(session) = claimed.remove(&stream_id) {
                removed.push(session);
            }
        }
        Ok(removed)
    }

    /// List detached resources for `bare_jid` that had requested the roster.
    pub async fn interested_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.roster_interested
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.roster_interested
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// Record a stanza for one detached interested resource.
    async fn record_outbound_for_detached_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            if !session.is_expired() && session.roster_interested && session.jid == *jid {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            if !session.is_expired() && session.roster_interested && session.jid == *jid {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record a typed stanza for one detached interested resource.
    pub async fn record_stanza_for_detached_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_resource(jid, Self::stanza_to_replay_xml(stanza))
            .await
    }

    /// Record a typed stanza for one detached resource by exact FullJID,
    /// regardless of roster-interest or presence-availability flags.
    pub async fn record_stanza_for_detached_bound_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
    ) -> Result<bool, SmRegistryError> {
        let stanza_xml = Self::stanza_to_replay_xml(stanza);
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            if !session.is_expired() && session.jid == *jid {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            if !session.is_expired() && session.jid == *jid {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record a stanza directly against a detached stream id, regardless of
    /// roster-interest or presence-availability flags.
    pub async fn record_outbound_for_detached_stream(
        &self,
        stream_id: &str,
        stanza_xml: String,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
            return Ok(false);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = claimed.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
            return Ok(false);
        }
        Ok(false)
    }

    pub async fn record_outbound_for_detached_stream_at(
        &self,
        stream_id: &str,
        sequence: u32,
        stanza_xml: String,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        if let Some(session) = sessions.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound_at(sequence, stanza_xml);
                return Ok(true);
            }
            return Ok(false);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = claimed.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound_at(sequence, stanza_xml);
                return Ok(true);
            }
            return Ok(false);
        }
        Ok(false)
    }

    /// List all detached resources for a bare JID, including resources that
    /// were not available at detach time.
    pub async fn detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| !session.is_expired() && session.jid.to_bare() == *bare_jid)
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| !session.is_expired() && session.jid.to_bare() == *bare_jid)
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for a bare JID that had XEP-0280 carbons enabled.
    pub async fn detached_carbon_resources_for_user(
        &self,
        bare_jid: &BareJid,
        except: &FullJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                session.carbons_enabled
                    && !session.is_expired()
                    && session.jid.to_bare() == *bare_jid
                    && session.jid != *except
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    session.carbons_enabled
                        && !session.is_expired()
                        && session.jid.to_bare() == *bare_jid
                        && session.jid != *except
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for `bare_jid` that were available at detach.
    pub async fn available_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.presence_available
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// Record a stanza for one detached resource that was available at detach.
    async fn record_outbound_for_detached_available_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            if !session.is_expired() && session.presence_available && session.jid == *jid {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            if !session.is_expired() && session.presence_available && session.jid == *jid {
                session.record_detached_outbound(stanza_xml);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record a typed stanza for one detached resource that was available at detach.
    pub async fn record_stanza_for_detached_available_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_available_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
        )
        .await
    }

    /// Return last known rich presence state for a detached available resource.
    pub async fn detached_presence_state(
        &self,
        jid: &FullJid,
    ) -> Result<Option<(Option<Show>, Option<String>, i8)>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.values().find(|session| {
            !session.is_expired() && session.presence_available && session.jid == *jid
        }) {
            return Ok(Some((
                session.presence_show.clone(),
                session.presence_status.clone(),
                session.presence_priority,
            )));
        }
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .values()
            .find(|session| {
                !session.is_expired() && session.presence_available && session.jid == *jid
            })
            .map(|session| {
                (
                    session.presence_show.clone(),
                    session.presence_status.clone(),
                    session.presence_priority,
                )
            }))
    }

    /// Return last known rich presence state for every detached available
    /// resource owned by `bare_jid`.
    pub async fn available_detached_presence_states_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<(FullJid, Option<Show>, Option<String>, i8)>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut states: Vec<(FullJid, Option<Show>, Option<String>, i8)> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| {
                (
                    session.jid.clone(),
                    session.presence_show.clone(),
                    session.presence_status.clone(),
                    session.presence_priority,
                )
            })
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        states.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.presence_available
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| {
                    (
                        session.jid.clone(),
                        session.presence_show.clone(),
                        session.presence_status.clone(),
                        session.presence_priority,
                    )
                }),
        );
        Ok(states)
    }
}

/// Internal helper: remove expired sessions and return them.
fn drain_expired_internal(sessions: &mut HashMap<String, DetachedSession>) -> Vec<DetachedSession> {
    let expired_keys: Vec<String> = sessions
        .iter()
        .filter_map(|(k, s)| {
            if s.is_expired() {
                Some(k.clone())
            } else {
                None
            }
        })
        .collect();

    let mut drained = Vec::with_capacity(expired_keys.len());
    for key in &expired_keys {
        if let Some(session) = sessions.remove(key) {
            drained.push(session);
        }
    }

    if !drained.is_empty() {
        debug!(
            removed = drained.len(),
            remaining = sessions.len(),
            "Cleaned up expired SM sessions"
        );
    }

    drained
}

/// Check if sequence a > b, handling wrap-around.
fn sequence_gt(a: u32, b: u32) -> bool {
    if a == b {
        return false;
    }
    let diff = a.wrapping_sub(b);
    diff < 0x8000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jid() -> FullJid {
        "user@example.com/resource".parse().unwrap()
    }

    fn make_test_session(stream_id: &str) -> DetachedSession {
        make_test_session_for_jid(stream_id, make_test_jid())
    }

    fn make_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: "user@example.com".to_string(),
            jid,
            inbound_count: 10,
            outbound_count: 15,
            last_acked: 12,
            unacked_stanzas: vec![
                (13, "<msg1/>".to_string()),
                (14, "<msg2/>".to_string()),
                (15, "<msg3/>".to_string()),
            ],
            max_resume_time: Some(300),
            detached_at: Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        }
    }

    fn make_test_session_with_unacked(
        stream_id: &str,
        unacked: Vec<(u32, String)>,
    ) -> DetachedSession {
        let mut s = make_test_session(stream_id);
        s.unacked_stanzas = unacked;
        s
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_removes_matching_1on1_message() {
        // XEP-0424 §"prevent further distribution" + XEP-0198 resume
        // safety: when a tombstone is applied, the original
        // `<message id='target'>` must not replay on a recipient's
        // resume. Locks the matcher against false negatives (matching
        // messages must be removed) and false positives (non-matching
        // messages and non-message frames must be preserved). Scoped
        // by the recipient's bare JID so the matcher cannot reach
        // outside the conversation.
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-tomb",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='target' type='chat'><body>secret</body><thread parent='root'>child</thread></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='other' type='chat'><body>safe</body></message>"
                        .to_string(),
                ),
                (3, "<presence/>".to_string()),
                (4, "<iq type='result' id='not-a-message'/>".to_string()),
            ],
        );
        registry.store_session(session).await.unwrap();

        let removed = registry
            .scrub_unacked_for_tombstone("target", "user@example.com")
            .await
            .unwrap();
        assert_eq!(removed, 1, "exactly one matching message should be removed");

        let again = registry
            .peek_session("stream-tomb")
            .await
            .unwrap()
            .expect("session still present");
        assert_eq!(again.unacked_stanzas.len(), 3);
        assert!(
            !again
                .unacked_stanzas
                .iter()
                .any(|(_, xml)| xml.contains("id='target'")),
            "scrubbed message must not appear in queue"
        );
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|(_, xml)| xml.contains("id='other'")),
            "non-matching message must remain"
        );
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|(_, xml)| xml.contains("<presence")),
            "presence frame must remain (not a message)"
        );
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|(_, xml)| xml.contains("<iq")),
            "iq frame must remain (not a message)"
        );
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_matches_groupchat_stanza_id() {
        // Groupchat retractions key off the room's XEP-0359 stanza-id
        // per the "archive id == wire stanza-id" invariant
        // (`archive_groupchat_message`). The cached reflection
        // preserves the sender's original `message.id` AND carries
        // `<stanza-id by='room' id='canonical'/>`; the retraction
        // request targets `canonical`, not the sender's id. The
        // matcher must therefore check stanza-id children too —
        // surfaced by Copilot review on PR #305.
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-muc",
            vec![(
                1,
                "<message xmlns='jabber:client' from='room@conf.example.com/alice' to='user@example.com/resource' id='sender-wire-id' type='groupchat'><body>moderated</body><stanza-id xmlns='urn:xmpp:sid:0' by='room@conf.example.com' id='canonical-archive-id'/></message>"
                    .to_string(),
            )],
        );
        registry.store_session(session).await.unwrap();

        let removed = registry
            .scrub_unacked_for_tombstone("canonical-archive-id", "room@conf.example.com")
            .await
            .unwrap();
        assert_eq!(
            removed, 1,
            "groupchat tombstone keyed by stanza-id must scrub the reflection"
        );
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_does_not_cross_conversations() {
        // Two clients independently use `id='msg-1'` in different
        // conversations. Retracting in conversation A must not delete
        // the queued message in conversation B that happens to share
        // the same wire id. Codex P1 review on PR #305.
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-cross",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-A</body></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='carol@elsewhere.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-B</body></message>"
                        .to_string(),
                ),
            ],
        );
        registry.store_session(session).await.unwrap();

        // Tombstone is scoped to alice@example.com (the sender of
        // conversation A's archive context). The matcher must NOT
        // remove the carol→user message even though it shares the
        // wire id, because alice is neither its `from` nor `to`.
        let removed = registry
            .scrub_unacked_for_tombstone("msg-1", "alice@example.com")
            .await
            .unwrap();
        assert_eq!(
            removed, 1,
            "only the alice-scoped message should be removed"
        );

        let again = registry
            .peek_session("stream-cross")
            .await
            .unwrap()
            .expect("session still present");
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|(_, xml)| xml.contains("conv-B")),
            "conversation B's message must survive — different scope"
        );
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_handles_no_match() {
        let registry = InMemorySmSessionRegistry::new();
        registry
            .store_session(make_test_session_with_unacked(
                "stream-nomatch",
                vec![(
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com' id='other' type='chat'><body>x</body></message>"
                        .to_string(),
                )],
            ))
            .await
            .unwrap();
        let removed = registry
            .scrub_unacked_for_tombstone("not-here", "user@example.com")
            .await
            .unwrap();
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn test_store_and_take_session() {
        let registry = InMemorySmSessionRegistry::new();

        let session = make_test_session("stream-123");
        registry.store_session(session).await.unwrap();

        assert_eq!(registry.session_count().await, 1);

        // Take the session
        let retrieved = registry.take_session("stream-123").await.unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.stream_id, "stream-123");
        assert_eq!(retrieved.outbound_count, 15);

        // Session should be gone now
        assert_eq!(registry.session_count().await, 0);
        let again = registry.take_session("stream-123").await.unwrap();
        assert!(again.is_none());
    }

    #[tokio::test]
    async fn test_store_session_replaces_existing_session_for_same_full_jid() {
        let registry = InMemorySmSessionRegistry::new();
        let mut first = make_test_session("stream-old");
        first.roster_interested = true;
        let mut second = make_test_session("stream-new");
        second.roster_interested = true;

        registry.store_session(first).await.unwrap();
        registry.store_session(second).await.unwrap();

        assert!(registry.take_session("stream-old").await.unwrap().is_none());
        let current = registry
            .take_session("stream-new")
            .await
            .unwrap()
            .expect("newer detached session should remain");
        assert_eq!(current.stream_id, "stream-new");
    }

    #[tokio::test]
    async fn test_peek_session() {
        let registry = InMemorySmSessionRegistry::new();

        let session = make_test_session("stream-456");
        registry.store_session(session).await.unwrap();

        // Peek should not remove
        let peeked = registry.peek_session("stream-456").await.unwrap();
        assert!(peeked.is_some());
        assert_eq!(registry.session_count().await, 1);

        // Peek again
        let peeked2 = registry.peek_session("stream-456").await.unwrap();
        assert!(peeked2.is_some());
    }

    #[tokio::test]
    async fn test_claimed_session_remains_writable_for_handoff_fanout() {
        let registry = InMemorySmSessionRegistry::new();

        let mut session = make_test_session("stream-claimed");
        session.roster_interested = true;
        let jid = session.jid.clone();
        registry.store_session(session).await.unwrap();

        let claimed = registry
            .claim_session("stream-claimed")
            .await
            .unwrap()
            .expect("claim");
        assert_eq!(claimed.stream_id, "stream-claimed");
        assert_eq!(
            registry.session_count().await,
            0,
            "claimed sessions must move out of the normal detached map"
        );

        assert!(
            registry
                .record_stanza_for_detached_resource(&jid, &{
                    let mut presence =
                        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
                    presence
                        .statuses
                        .insert(String::new(), "during-claim".to_string());
                    Stanza::Presence(presence)
                })
                .await
                .unwrap(),
            "fanout during resume handoff must write to the claimed session"
        );

        let completed = registry
            .complete_claim("stream-claimed")
            .await
            .unwrap()
            .expect("completed claim");
        match completed {
            SmClaimCompletion::Resumed(completed) => {
                assert!(
                    completed
                        .unacked_stanzas
                        .iter()
                        .any(|(_, stanza)| stanza.contains("during-claim")),
                    "completed claim must include fanout recorded during handoff"
                );
            }
            SmClaimCompletion::Expired(_) => panic!("claim should still be resumable"),
        }
    }

    #[tokio::test]
    async fn test_session_not_found() {
        let registry = InMemorySmSessionRegistry::new();

        let result = registry.take_session("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_session_expired() {
        let registry = InMemorySmSessionRegistry::new();

        // Create an already-expired session
        let mut session = make_test_session("stream-expired");
        session.max_resume_time = Some(0); // 0 seconds means expired immediately

        registry.store_session(session).await.unwrap();

        // Wait a tiny bit to ensure expiration
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Should return None because expired
        let result = registry.take_session("stream-expired").await.unwrap();
        assert!(result.is_none());
        assert_eq!(registry.session_count().await, 0);
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let registry = InMemorySmSessionRegistry::new();

        // Store some sessions
        let mut expired = make_test_session("stream-exp1");
        expired.max_resume_time = Some(0);
        registry.store_session(expired).await.unwrap();

        let valid =
            make_test_session_for_jid("stream-valid", "user@example.com/valid".parse().unwrap());
        registry.store_session(valid).await.unwrap();

        // Wait for expiration
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cleanup
        let removed = registry.cleanup_expired().await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(registry.session_count().await, 1);

        // Valid session should still be there
        let result = registry.take_session("stream-valid").await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_capacity_limit() {
        let registry = InMemorySmSessionRegistry::with_capacity(3);

        // Store 3 sessions
        for i in 0..3 {
            let session = make_test_session_for_jid(
                &format!("stream-{}", i),
                format!("user@example.com/resource-{i}").parse().unwrap(),
            );
            registry.store_session(session).await.unwrap();
        }

        assert_eq!(registry.session_count().await, 3);

        // Store a 4th - should evict oldest
        let session = make_test_session_for_jid(
            "stream-new",
            "user@example.com/resource-new".parse().unwrap(),
        );
        registry.store_session(session).await.unwrap();

        assert_eq!(registry.session_count().await, 3);

        // stream-0 should be gone (oldest)
        let result = registry.take_session("stream-0").await.unwrap();
        assert!(result.is_none());

        // stream-new should be there
        let result = registry.take_session("stream-new").await.unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_stanzas_to_resend_count() {
        let session = make_test_session("test");

        // Client says h=12, we have 13, 14, 15 - all 3 need resending
        assert_eq!(session.stanzas_to_resend_count(12), 3);

        // Client says h=14, we have 13, 14, 15 - only 15 needs resending
        assert_eq!(session.stanzas_to_resend_count(14), 1);

        // Client says h=15, we have 13, 14, 15 - none need resending
        assert_eq!(session.stanzas_to_resend_count(15), 0);
    }

    #[test]
    fn test_remaining_time() {
        let session = make_test_session("test");

        let remaining = session.remaining_time();
        assert!(remaining.as_secs() <= 300);
        assert!(remaining.as_secs() >= 299); // Should be close to 300
    }
}
