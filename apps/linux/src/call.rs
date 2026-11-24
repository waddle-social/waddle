//! Call signalling types and rules, without GTK or the network.
//!
//! A call is XEP-0353 Jingle Message Initiation (propose, ringing, proceed,
//! reject, retract, finish) followed by a XEP-0166 session whose transport
//! carries LiveKit credentials: the server hands each party a room URL and
//! a join token, and the media flows through the SFU. This module is the
//! single-slot state machine's vocabulary and its pure reducer; the engine
//! that sequences sends and timers around it is `call_engine.rs`.
//!
//! The rules follow the Apple client (`WaddleKit/Calls`) so both apps
//! interoperate on the same sessions: the lower session id wins a
//! simultaneous proposal, only the device that sent `<proceed/>` speaks for
//! the call, and an event changes the slot only when it names the slot's
//! session, comes from the party entitled to send it and fits the phase.

use std::cmp::Ordering;
use std::fmt;

use base64::Engine as _;
use jid::{BareJid, Jid};

/// The tracks a call invitation or session names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallMedia {
    pub audio: bool,
    pub video: bool,
}

impl CallMedia {
    pub const AUDIO: Self = Self {
        audio: true,
        video: false,
    };

    /// The tracks both sides named: a session never publishes a track the
    /// local user did not agree to when placing or accepting the call.
    pub fn bounded_by(self, agreed: Self) -> Self {
        Self {
            audio: self.audio && agreed.audio,
            video: self.video && agreed.video,
        }
    }
}

/// Server-issued SFU credentials from the Jingle transport.
#[derive(Clone, PartialEq, Eq)]
pub struct CallJoin {
    pub url: String,
    pub room: String,
    pub identity: String,
    pub token: String,
}

// The token is a bearer credential: never printed.
impl fmt::Debug for CallJoin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallJoin")
            .field("url", &self.url)
            .field("room", &self.room)
            .field("identity", &self.identity)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Why a join token cannot be used for the credentials it came with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinDefect {
    /// Not a three-part JWT with a JSON object payload.
    MalformedToken,
    /// The payload has no `video` grant.
    MissingGrant,
    /// `video.roomJoin` is not `true`.
    JoinNotGranted,
    /// `video.room` is absent or blank.
    MissingRoom,
    /// `video.room` names another room than the credentials do.
    RoomMismatch,
    /// The token's `sub` is not the identity in the credentials.
    IdentityMismatch,
}

impl CallJoin {
    /// None when the token grants joining exactly this room as this
    /// identity.
    ///
    /// A pre-flight check only: a mismatched token otherwise fails deep
    /// inside the media SDK with an opaque error. It reads the payload and
    /// verifies no signature; the media server authenticates the token.
    pub fn defect(&self) -> Option<JoinDefect> {
        let Some(payload) = self.token_payload() else {
            return Some(JoinDefect::MalformedToken);
        };
        let Some(grant) = payload.get("video").and_then(serde_json::Value::as_object) else {
            return Some(JoinDefect::MissingGrant);
        };
        if grant.get("roomJoin").and_then(serde_json::Value::as_bool) != Some(true) {
            return Some(JoinDefect::JoinNotGranted);
        }
        let granted = grant
            .get("room")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if granted.trim().is_empty() {
            return Some(JoinDefect::MissingRoom);
        }
        // Compared exactly: the media server joins the room the token names.
        if granted != self.room {
            return Some(JoinDefect::RoomMismatch);
        }
        match payload.get("sub").and_then(serde_json::Value::as_str) {
            Some(subject) if subject != self.identity => Some(JoinDefect::IdentityMismatch),
            _ => None,
        }
    }

    fn token_payload(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let mut segments = self.token.split('.');
        let (_, payload, _, None) = (
            segments.next(),
            segments.next()?,
            segments.next()?,
            segments.next(),
        ) else {
            return None;
        };
        // JWT segments are unpadded base64url (RFC 7515 section 2).
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?;
        match serde_json::from_slice(&bytes).ok()? {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        }
    }
}

/// The seventeen XEP-0166 termination conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JingleReason {
    AlternativeSession,
    Busy,
    Cancel,
    ConnectivityError,
    Decline,
    Expired,
    FailedApplication,
    FailedTransport,
    GeneralError,
    Gone,
    IncompatibleParameters,
    MediaError,
    SecurityError,
    Success,
    Timeout,
    UnsupportedApplications,
    UnsupportedTransports,
}

/// A parsed live XEP-0353 invitation or XEP-0166 session event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEvent {
    pub from: Jid,
    pub sid: String,
    pub kind: CallEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEventKind {
    Propose(CallMedia),
    Ringing,
    Proceed,
    Reject {
        reason: Option<JingleReason>,
        tie_break: bool,
    },
    Retract {
        reason: Option<JingleReason>,
        tie_break: bool,
    },
    Finish {
        reason: Option<JingleReason>,
    },
    SessionInitiate {
        join: CallJoin,
        media: CallMedia,
    },
    SessionAccept {
        join: CallJoin,
        media: CallMedia,
    },
    SessionTerminate {
        reason: Option<JingleReason>,
    },
}

/// The proposer's full JID is kept for every answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingCall {
    pub from: Jid,
    pub sid: String,
    pub media: CallMedia,
    /// Our `<proceed/>` is out (or on its way).
    pub accepting: bool,
}

/// Proposals target the bare peer; the initiator names our bound resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingCall {
    pub to: BareJid,
    pub sid: String,
    pub media: CallMedia,
    pub initiator: Jid,
    /// A device of theirs says it is ringing.
    pub ringing: bool,
    /// The peer device whose `<proceed/>` arrived first. Once set, the
    /// Jingle session is negotiated with it alone and the ring is answered.
    pub responder: Option<Jid>,
}

/// Jingle established a session with server-issued media credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCall {
    pub peer: Jid,
    pub sid: String,
    pub media: CallMedia,
    pub join: CallJoin,
}

/// Why the slot ended. `Expired` is a lost XEP-0353 tie-break; `Missed` is
/// a call to us that nobody answered before its ring ran out; `Finished`
/// carries the Jingle condition of a `<finish/>` or session-terminate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndReason {
    Rejected,
    Retracted,
    Expired,
    Timeout,
    Missed,
    Error,
    Finished(Option<JingleReason>),
}

impl CallEndReason {
    /// Why the call ended, in a few plain words for the person on it.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Rejected => "Call declined",
            Self::Retracted => "Call cancelled",
            Self::Expired => "Answered on another device",
            Self::Timeout => "No answer",
            Self::Missed => "Missed call",
            Self::Error => "Call could not connect",
            Self::Finished(reason) => match reason {
                None | Some(JingleReason::Success) => "Call ended",
                Some(JingleReason::Decline) => "Call declined",
                Some(JingleReason::Busy) => "Busy right now",
                Some(JingleReason::Cancel) => "Call cancelled",
                Some(JingleReason::Timeout | JingleReason::Expired) => "No answer",
                Some(JingleReason::AlternativeSession) => "Answered on another device",
                Some(
                    JingleReason::ConnectivityError
                    | JingleReason::FailedTransport
                    | JingleReason::MediaError
                    | JingleReason::Gone,
                ) => "Connection lost",
                Some(
                    JingleReason::FailedApplication
                    | JingleReason::GeneralError
                    | JingleReason::IncompatibleParameters
                    | JingleReason::SecurityError
                    | JingleReason::UnsupportedApplications
                    | JingleReason::UnsupportedTransports,
                ) => "Call could not connect",
            },
        }
    }
}

/// One signed-in session owns a single call slot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CallState {
    #[default]
    Idle,
    Incoming(IncomingCall),
    Outgoing(OutgoingCall),
    Active(ActiveCall),
    Ended {
        sid: String,
        reason: CallEndReason,
    },
}

impl CallState {
    /// The session the slot holds, live or ended; None when idle.
    pub fn sid(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Incoming(call) => Some(&call.sid),
            Self::Outgoing(call) => Some(&call.sid),
            Self::Active(call) => Some(&call.sid),
            Self::Ended { sid, .. } => Some(sid),
        }
    }

    /// A call is ringing, being placed or in progress: the slot is taken.
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Incoming(_) | Self::Outgoing(_) | Self::Active(_)
        )
    }

    pub fn is_incoming(&self) -> bool {
        matches!(self, Self::Incoming(_))
    }

    pub fn is_outgoing(&self) -> bool {
        matches!(self, Self::Outgoing(_))
    }
}

/// An event changes the slot only when it names the slot's session, comes
/// from the party entitled to send it and fits the slot's phase. Anything
/// else (a replay, a late answer from another device, a stranger who
/// learned the sid) leaves the slot as it is.
///
/// `own` marks a carbon of something another of our own devices sent.
pub fn reduce(current: &CallState, event: &CallEvent, own: bool) -> CallState {
    if let CallEventKind::Propose(media) = &event.kind {
        // A redelivered proposal for the session the slot already holds,
        // live or ended, is not a new call.
        if own || current.is_live() || current.sid() == Some(&event.sid) {
            return current.clone();
        }
        return CallState::Incoming(IncomingCall {
            from: event.from.clone(),
            sid: event.sid.clone(),
            media: *media,
            accepting: false,
        });
    }
    if current.sid() != Some(&event.sid) {
        return current.clone();
    }
    if own {
        reduce_sibling(current, event)
    } else {
        reduce_peer(current, event)
    }
}

/// Another of our devices answered, declined or ended the call this device
/// is still showing.
fn reduce_sibling(current: &CallState, event: &CallEvent) -> CallState {
    let ended = |reason| CallState::Ended {
        sid: event.sid.clone(),
        reason,
    };
    match (&event.kind, current) {
        (CallEventKind::Proceed, CallState::Incoming(_)) => CallState::Idle,
        (CallEventKind::Reject { reason, tie_break }, CallState::Incoming(_)) => {
            ended(abort_reason(*reason, *tie_break, CallEndReason::Rejected))
        }
        // A sibling that answered and hung up also ends a ring this device
        // kept, e.g. after it missed the sibling's proceed while offline.
        (CallEventKind::Finish { reason }, CallState::Incoming(_) | CallState::Active(_)) => {
            ended(CallEndReason::Finished(*reason))
        }
        (CallEventKind::SessionTerminate { reason }, CallState::Active(_)) => {
            ended(CallEndReason::Finished(*reason))
        }
        _ => current.clone(),
    }
}

fn reduce_peer(current: &CallState, event: &CallEvent) -> CallState {
    match current {
        CallState::Incoming(call) => {
            if call.from.to_bare() != event.from.to_bare() {
                return current.clone();
            }
            let ended = |reason| CallState::Ended {
                sid: call.sid.clone(),
                reason,
            };
            match &event.kind {
                CallEventKind::Retract { reason, tie_break } => {
                    ended(abort_reason(*reason, *tie_break, CallEndReason::Retracted))
                }
                CallEventKind::SessionInitiate { join, media } => {
                    // Media starts only for an invitation this device
                    // accepted, from the device that proposed it, with no
                    // track beyond those agreed.
                    if !call.accepting || call.from != event.from {
                        return current.clone();
                    }
                    CallState::Active(ActiveCall {
                        peer: event.from.clone(),
                        sid: call.sid.clone(),
                        media: media.bounded_by(call.media),
                        join: join.clone(),
                    })
                }
                CallEventKind::Finish { reason } | CallEventKind::SessionTerminate { reason } => {
                    ended(CallEndReason::Finished(*reason))
                }
                _ => current.clone(),
            }
        }
        CallState::Outgoing(call) => {
            // Once one of the peer's devices has answered, only that device
            // speaks for the call.
            if call.to != event.from.to_bare()
                || call.responder.as_ref().is_some_and(|r| *r != event.from)
            {
                return current.clone();
            }
            let ended = |reason| CallState::Ended {
                sid: call.sid.clone(),
                reason,
            };
            match &event.kind {
                CallEventKind::Ringing => {
                    let mut call = call.clone();
                    call.ringing = true;
                    CallState::Outgoing(call)
                }
                CallEventKind::Proceed => {
                    if call.responder.is_some() {
                        return current.clone();
                    }
                    let mut call = call.clone();
                    call.responder = Some(event.from.clone());
                    CallState::Outgoing(call)
                }
                CallEventKind::Reject { reason, tie_break } => {
                    ended(abort_reason(*reason, *tie_break, CallEndReason::Rejected))
                }
                CallEventKind::SessionAccept { join, media } => CallState::Active(ActiveCall {
                    peer: event.from.clone(),
                    sid: call.sid.clone(),
                    media: media.bounded_by(call.media),
                    join: join.clone(),
                }),
                CallEventKind::Finish { reason } | CallEventKind::SessionTerminate { reason } => {
                    ended(CallEndReason::Finished(*reason))
                }
                _ => current.clone(),
            }
        }
        CallState::Active(call) => {
            // An established session ends only on its own end events, from
            // the device it was established with.
            if call.peer != event.from {
                return current.clone();
            }
            match &event.kind {
                CallEventKind::Finish { reason } | CallEventKind::SessionTerminate { reason } => {
                    CallState::Ended {
                        sid: call.sid.clone(),
                        reason: CallEndReason::Finished(*reason),
                    }
                }
                _ => current.clone(),
            }
        }
        CallState::Idle | CallState::Ended { .. } => current.clone(),
    }
}

/// XEP-0353 tie-breaking marks the losing proposal `<expired/>` with
/// `<tie-break/>`.
fn abort_reason(
    reason: Option<JingleReason>,
    tie_break: bool,
    otherwise: CallEndReason,
) -> CallEndReason {
    if tie_break && reason == Some(JingleReason::Expired) {
        CallEndReason::Expired
    } else {
        otherwise
    }
}

/// XEP-0353 tie-break-1 uses RFC 4790 `i;octet`: unsigned UTF-8 bytes,
/// with the shorter string first when all shared bytes match.
pub fn compare_octets(left: &str, right: &str) -> Ordering {
    left.as_bytes().cmp(right.as_bytes())
}

/// The lower session id wins a simultaneous proposal to the same peer.
/// Identical session ids defer to the full JIDs under the same octet order.
pub fn incoming_wins_tie_break(
    incoming_sid: &str,
    outgoing_sid: &str,
    incoming_from: &Jid,
    own_full: &Jid,
) -> bool {
    match compare_octets(incoming_sid, outgoing_sid) {
        Ordering::Equal => {
            compare_octets(incoming_from.as_str(), own_full.as_str()) == Ordering::Less
        }
        order => order == Ordering::Less,
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn format_elapsed(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(text: &str) -> Jid {
        Jid::new(text).unwrap()
    }

    fn join(room: &str, identity: &str) -> CallJoin {
        CallJoin {
            url: "wss://sfu.example".into(),
            room: room.into(),
            identity: identity.into(),
            token: token(&format!(
                r#"{{"sub":"{identity}","video":{{"roomJoin":true,"room":"{room}"}}}}"#
            )),
        }
    }

    fn token(payload: &str) -> String {
        let encode =
            |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes());
        format!("{}.{}.sig", encode(r#"{"alg":"HS256"}"#), encode(payload))
    }

    fn event(from: &str, sid: &str, kind: CallEventKind) -> CallEvent {
        CallEvent {
            from: jid(from),
            sid: sid.into(),
            kind,
        }
    }

    fn outgoing(responder: Option<&str>) -> OutgoingCall {
        OutgoingCall {
            to: BareJid::new("bob@example.org").unwrap(),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            initiator: jid("alice@example.org/laptop"),
            ringing: false,
            responder: responder.map(jid),
        }
    }

    fn incoming(accepting: bool) -> IncomingCall {
        IncomingCall {
            from: jid("bob@example.org/phone"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            accepting,
        }
    }

    #[test]
    fn a_proposal_rings_an_idle_slot() {
        let proposed = event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Propose(CallMedia::AUDIO),
        );
        assert_eq!(
            reduce(&CallState::Idle, &proposed, false),
            CallState::Incoming(incoming(false))
        );
    }

    #[test]
    fn a_redelivered_or_own_or_second_proposal_changes_nothing() {
        let proposed = event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Propose(CallMedia::AUDIO),
        );
        let ringing = CallState::Incoming(incoming(false));
        assert_eq!(reduce(&ringing, &proposed, false), ringing);
        let ended = CallState::Ended {
            sid: "c1".into(),
            reason: CallEndReason::Missed,
        };
        assert_eq!(reduce(&ended, &proposed, false), ended);
        assert_eq!(reduce(&CallState::Idle, &proposed, true), CallState::Idle);
        let other = event(
            "carol@example.org/phone",
            "c2",
            CallEventKind::Propose(CallMedia::AUDIO),
        );
        assert_eq!(reduce(&ringing, &other, false), ringing);
    }

    #[test]
    fn events_for_another_session_are_ignored() {
        let state = CallState::Outgoing(outgoing(None));
        let stray = event("bob@example.org/phone", "other", CallEventKind::Ringing);
        assert_eq!(reduce(&state, &stray, false), state);
    }

    #[test]
    fn an_outgoing_call_rings_then_binds_to_the_first_proceed() {
        let state = CallState::Outgoing(outgoing(None));
        let ringing = reduce(
            &state,
            &event("bob@example.org/phone", "c1", CallEventKind::Ringing),
            false,
        );
        let CallState::Outgoing(call) = &ringing else {
            panic!("still outgoing");
        };
        assert!(call.ringing);
        let proceeded = reduce(
            &ringing,
            &event("bob@example.org/phone", "c1", CallEventKind::Proceed),
            false,
        );
        let CallState::Outgoing(call) = &proceeded else {
            panic!("still outgoing");
        };
        assert_eq!(call.responder, Some(jid("bob@example.org/phone")));
        // A second device's proceed, or anything it says, is not heard.
        for kind in [
            CallEventKind::Proceed,
            CallEventKind::Reject {
                reason: None,
                tie_break: false,
            },
        ] {
            assert_eq!(
                reduce(
                    &proceeded,
                    &event("bob@example.org/tablet", "c1", kind),
                    false
                ),
                proceeded
            );
        }
    }

    #[test]
    fn a_session_accept_activates_with_the_agreed_tracks_only() {
        let state = CallState::Outgoing(outgoing(Some("bob@example.org/phone")));
        let accepted = reduce(
            &state,
            &event(
                "bob@example.org/phone",
                "c1",
                CallEventKind::SessionAccept {
                    join: join("room", "alice"),
                    media: CallMedia {
                        audio: true,
                        video: true,
                    },
                },
            ),
            false,
        );
        let CallState::Active(call) = accepted else {
            panic!("active");
        };
        assert_eq!(call.media, CallMedia::AUDIO);
        assert_eq!(call.peer, jid("bob@example.org/phone"));
    }

    #[test]
    fn media_starts_only_for_an_accepted_invitation_from_the_proposer() {
        let initiate = |from: &str| {
            event(
                from,
                "c1",
                CallEventKind::SessionInitiate {
                    join: join("room", "alice"),
                    media: CallMedia::AUDIO,
                },
            )
        };
        let ringing = CallState::Incoming(incoming(false));
        assert_eq!(
            reduce(&ringing, &initiate("bob@example.org/phone"), false),
            ringing
        );
        let accepting = CallState::Incoming(incoming(true));
        assert_eq!(
            reduce(&accepting, &initiate("bob@example.org/tablet"), false),
            accepting
        );
        assert!(matches!(
            reduce(&accepting, &initiate("bob@example.org/phone"), false),
            CallState::Active(_)
        ));
    }

    #[test]
    fn a_retract_or_finish_ends_a_ringing_call() {
        let ringing = CallState::Incoming(incoming(false));
        let retracted = reduce(
            &ringing,
            &event(
                "bob@example.org/phone",
                "c1",
                CallEventKind::Retract {
                    reason: None,
                    tie_break: false,
                },
            ),
            false,
        );
        assert_eq!(
            retracted,
            CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Retracted
            }
        );
        // From someone who is not the caller: nothing.
        assert_eq!(
            reduce(
                &ringing,
                &event(
                    "mallory@example.org/x",
                    "c1",
                    CallEventKind::Finish { reason: None }
                ),
                false
            ),
            ringing
        );
    }

    #[test]
    fn a_tie_break_retract_reads_as_expired() {
        let state = CallState::Outgoing(outgoing(None));
        let lost = reduce(
            &state,
            &event(
                "bob@example.org/phone",
                "c1",
                CallEventKind::Reject {
                    reason: Some(JingleReason::Expired),
                    tie_break: true,
                },
            ),
            false,
        );
        assert_eq!(
            lost,
            CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Expired
            }
        );
    }

    #[test]
    fn an_established_call_ends_only_from_its_own_device() {
        let active = CallState::Active(ActiveCall {
            peer: jid("bob@example.org/phone"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            join: join("room", "alice"),
        });
        let terminate = CallEventKind::SessionTerminate {
            reason: Some(JingleReason::Success),
        };
        assert_eq!(
            reduce(
                &active,
                &event("bob@example.org/tablet", "c1", terminate.clone()),
                false
            ),
            active
        );
        assert_eq!(
            reduce(
                &active,
                &event("bob@example.org/phone", "c1", terminate),
                false
            ),
            CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Finished(Some(JingleReason::Success))
            }
        );
    }

    #[test]
    fn a_sibling_device_answering_clears_our_ring() {
        let ringing = CallState::Incoming(incoming(false));
        let proceed = event("alice@example.org/phone", "c1", CallEventKind::Proceed);
        assert_eq!(reduce(&ringing, &proceed, true), CallState::Idle);
        let reject = event(
            "alice@example.org/phone",
            "c1",
            CallEventKind::Reject {
                reason: None,
                tie_break: false,
            },
        );
        assert!(matches!(
            reduce(&ringing, &reject, true),
            CallState::Ended {
                reason: CallEndReason::Rejected,
                ..
            }
        ));
    }

    #[test]
    fn the_lower_session_id_wins_and_equal_ids_compare_jids() {
        let theirs = jid("bob@example.org/phone");
        let ours = jid("alice@example.org/laptop");
        assert!(incoming_wins_tie_break("a", "b", &theirs, &ours));
        assert!(!incoming_wins_tie_break("b", "a", &theirs, &ours));
        // "alice..." sorts before "bob...", so the equal-sid proposal from
        // the higher JID loses.
        assert!(!incoming_wins_tie_break("c", "c", &theirs, &ours));
        assert!(incoming_wins_tie_break("c", "c", &ours, &theirs));
        // A prefix sorts first.
        assert!(incoming_wins_tie_break("c1", "c10", &theirs, &ours));
    }

    #[test]
    fn a_join_token_must_grant_exactly_this_room_and_identity() {
        assert_eq!(join("room", "alice").defect(), None);

        let mut bad = join("room", "alice");
        bad.token = "not-a-jwt".into();
        assert_eq!(bad.defect(), Some(JoinDefect::MalformedToken));
        bad.token = token("[1]");
        assert_eq!(bad.defect(), Some(JoinDefect::MalformedToken));
        bad.token = token(r#"{"sub":"alice"}"#);
        assert_eq!(bad.defect(), Some(JoinDefect::MissingGrant));
        bad.token = token(r#"{"video":{"room":"room"}}"#);
        assert_eq!(bad.defect(), Some(JoinDefect::JoinNotGranted));
        bad.token = token(r#"{"video":{"roomJoin":true,"room":" "}}"#);
        assert_eq!(bad.defect(), Some(JoinDefect::MissingRoom));
        bad.token = token(r#"{"video":{"roomJoin":true,"room":"other"}}"#);
        assert_eq!(bad.defect(), Some(JoinDefect::RoomMismatch));
        bad.token = token(r#"{"sub":"mallory","video":{"roomJoin":true,"room":"room"}}"#);
        assert_eq!(bad.defect(), Some(JoinDefect::IdentityMismatch));
        // No subject is accepted: the server decides who joins.
        bad.token = token(r#"{"video":{"roomJoin":true,"room":"room"}}"#);
        assert_eq!(bad.defect(), None);
    }

    #[test]
    fn the_token_never_appears_in_debug_output() {
        let text = format!("{:?}", join("room", "alice"));
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("sig"));
    }

    #[test]
    fn elapsed_time_reads_as_a_clock() {
        assert_eq!(format_elapsed(0), "0:00");
        assert_eq!(format_elapsed(65), "1:05");
        assert_eq!(format_elapsed(3599), "59:59");
        assert_eq!(format_elapsed(3600), "1:00:00");
        assert_eq!(format_elapsed(3725), "1:02:05");
    }

    #[test]
    fn end_summaries_say_what_happened_in_plain_words() {
        assert_eq!(CallEndReason::Missed.summary(), "Missed call");
        assert_eq!(CallEndReason::Finished(None).summary(), "Call ended");
        assert_eq!(
            CallEndReason::Finished(Some(JingleReason::Busy)).summary(),
            "Busy right now"
        );
        assert_eq!(
            CallEndReason::Finished(Some(JingleReason::Gone)).summary(),
            "Connection lost"
        );
    }
}
