//! The call slot's engine: what to send, what to start and when to give
//! up, decided without a network or a clock.
//!
//! Every method takes one input (the user answering, a stanza arriving, a
//! send's outcome, a timer firing) and returns the effects the driver must
//! carry out, in order. The driver (`ui/calls.rs`) sends the wire effects,
//! runs the timers and starts or stops media, then reports back. Nothing
//! here is async, so each ordering rule of the Apple client's `CallStore`
//! is a plain test.
//!
//! Scope: one-to-one audio. A second proposal while the slot is busy is
//! refused; the Apple client migrates a same-peer proposal during an active
//! call (to upgrade it), which has no meaning for audio-only.

use std::collections::VecDeque;
use std::time::Duration;

use jid::{BareJid, Jid};

use crate::call::{
    incoming_wins_tie_break, reduce, ActiveCall, CallEndReason, CallEvent, CallEventKind, CallJoin,
    CallMedia, CallState, IncomingCall, JingleReason, OutgoingCall,
};

/// How long a call we placed rings before it is retracted.
pub const RING_TIMEOUT: Duration = Duration::from_secs(45);
/// How long an answered call waits for its Jingle session to be accepted.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a call to us rings with no word from the caller. A caller who
/// lost their connection never retracts, so this is a little longer than
/// the caller's own ring.
pub const UNANSWERED_TIMEOUT: Duration = Duration::from_secs(60);

/// How many finished and aborted session ids are remembered, so a late
/// duplicate proposal for a call that is already over does not ring again.
const FINISHED_MEMORY: usize = 32;
const ABORTED_MEMORY: usize = 256;

/// One stanza to send. Targets are the full or bare JIDs XEP-0353 and
/// XEP-0166 name for each verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    /// XEP-0353 `<propose/>` to the peer's bare JID, ringing every device.
    Propose {
        to: BareJid,
        sid: String,
        media: CallMedia,
    },
    Ringing {
        to: Jid,
        sid: String,
    },
    Proceed {
        to: Jid,
        sid: String,
    },
    Reject {
        to: Jid,
        sid: String,
        tie_break: bool,
    },
    Retract {
        to: BareJid,
        sid: String,
    },
    RetractTieBreak {
        to: Jid,
        sid: String,
    },
    Finish {
        to: Jid,
        sid: String,
        reason: Option<JingleReason>,
    },
    SessionInitiate {
        to: Jid,
        initiator: Jid,
        sid: String,
        media: CallMedia,
    },
    SessionAccept {
        to: Jid,
        responder: Jid,
        sid: String,
        media: CallMedia,
    },
}

impl Wire {
    /// What failed, for the call window.
    fn failure(&self) -> &'static str {
        match self {
            Self::Propose { .. } => "The call could not be placed.",
            Self::Ringing { .. } => "The call could not be signalled as ringing.",
            Self::Proceed { .. } => "The call could not be answered.",
            Self::Reject { .. } => "The call could not be declined.",
            Self::Retract { .. } | Self::RetractTieBreak { .. } => {
                "The call could not be cancelled."
            }
            Self::Finish { .. } => "The call could not be finished.",
            Self::SessionInitiate { .. } => "The call could not be started.",
            Self::SessionAccept { .. } => "The call could not be accepted.",
        }
    }
}

/// Which timer fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// An outgoing ring, or an incoming one nobody answered.
    Ring,
    /// The wait for an answered call's session.
    Session,
}

/// What the driver must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send, then report the outcome with [`CallEngine::sent`].
    Send(Wire),
    /// Send `<ringing/>` only if the caller is someone who receives our
    /// presence: a stranger's call is surfaced silently, so no automatic
    /// reply leaks that this device is online. Outcome needs no report.
    RingIfKnown { to: Jid, sid: String },
    /// End the Jingle session, then send the XEP-0353 finish. When the
    /// terminate fails outright the driver reports it with
    /// [`CallEngine::terminate_failed`] and sends the finish only if
    /// `finish_after_error`; a server that merely no longer knows the
    /// session always still gets the finish.
    Terminate {
        peer: Jid,
        sid: String,
        reason: JingleReason,
        finish_reason: Option<JingleReason>,
        finish_after_error: bool,
    },
    /// Call [`CallEngine::timer_fired`] after `after`, unless a newer
    /// start or a cancel superseded `token`.
    StartTimer {
        timer: Timer,
        token: u64,
        after: Duration,
    },
    /// Join the SFU room and publish the microphone.
    ConnectMedia {
        sid: String,
        join: CallJoin,
        media: CallMedia,
    },
    /// Leave the room and release the audio devices.
    DisconnectMedia,
}

/// The slot, plus what it remembers between calls.
pub struct CallEngine {
    /// Our bound full JID: the initiator of calls we place and the
    /// responder of calls we answer.
    own: Jid,
    state: CallState,
    last_error: Option<&'static str>,
    finished: VecDeque<String>,
    aborted: VecDeque<String>,
    ring_timer: Option<u64>,
    session_timer: Option<u64>,
    next_token: u64,
    out: Vec<Effect>,
}

impl CallEngine {
    pub fn new(own: Jid) -> Self {
        Self {
            own,
            state: CallState::Idle,
            last_error: None,
            finished: VecDeque::new(),
            aborted: VecDeque::new(),
            ring_timer: None,
            session_timer: None,
            next_token: 0,
            out: Vec::new(),
        }
    }

    pub fn state(&self) -> &CallState {
        &self.state
    }

    /// The last failure, shown until the next call or transition.
    pub fn last_error(&self) -> Option<&'static str> {
        self.last_error
    }

    pub fn is_finished(&self, sid: &str) -> bool {
        self.finished.iter().any(|known| known == sid)
    }

    fn take(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.out)
    }

    // MARK: Local actions

    /// Claim the slot and propose. None when a call already holds it.
    pub fn start_call(&mut self, peer: BareJid, sid: String) -> Option<Vec<Effect>> {
        if self.state.is_live() {
            return None;
        }
        self.cancel_timers();
        self.last_error = None;
        self.transition(CallState::Outgoing(OutgoingCall {
            to: peer.clone(),
            sid: sid.clone(),
            media: CallMedia::AUDIO,
            initiator: self.own.clone(),
            ringing: false,
            responder: None,
        }));
        self.out.push(Effect::Send(Wire::Propose {
            to: peer,
            sid,
            media: CallMedia::AUDIO,
        }));
        Some(self.take())
    }

    /// Proceed is claimed before sending, so a second Accept cannot answer
    /// twice.
    pub fn accept(&mut self) -> Vec<Effect> {
        let CallState::Incoming(call) = &self.state else {
            return Vec::new();
        };
        if call.accepting {
            return Vec::new();
        }
        let mut call = call.clone();
        call.accepting = true;
        let wire = Wire::Proceed {
            to: call.from.clone(),
            sid: call.sid.clone(),
        };
        self.state = CallState::Incoming(call);
        self.out.push(Effect::Send(wire));
        self.take()
    }

    /// An unanswered invitation is rejected to the proposer's full
    /// resource; once our `<proceed/>` is out, the abandon verb is
    /// `<finish/>` with `<cancel/>`, since a reject would contradict it.
    pub fn decline(&mut self) -> Vec<Effect> {
        let CallState::Incoming(call) = self.state.clone() else {
            return Vec::new();
        };
        self.cancel_timers();
        self.transition(CallState::Idle);
        self.answer_declined(&call);
        self.take()
    }

    fn answer_declined(&mut self, call: &IncomingCall) {
        self.out.push(Effect::Send(if call.accepting {
            Wire::Finish {
                to: call.from.clone(),
                sid: call.sid.clone(),
                reason: Some(JingleReason::Cancel),
            }
        } else {
            Wire::Reject {
                to: call.from.clone(),
                sid: call.sid.clone(),
                tie_break: false,
            }
        }));
    }

    /// Whatever the slot holds goes idle first, so a local hang-up wins any
    /// event that arrives while its sends are on the wire.
    pub fn hang_up(&mut self) -> Vec<Effect> {
        let claimed = self.state.clone();
        self.cancel_timers();
        self.transition(CallState::Idle);
        match claimed {
            CallState::Incoming(call) => self.answer_declined(&call),
            CallState::Outgoing(call) => match call.responder {
                None => self.out.push(Effect::Send(Wire::Retract {
                    to: call.to,
                    sid: call.sid,
                })),
                // A device has answered and may already be in the session,
                // where a retract means nothing: end the Jingle session,
                // then finish. The finish goes out even when the server
                // does not know the session yet, since it is all a
                // still-accepting device sees.
                Some(responder) => self.out.push(Effect::Terminate {
                    peer: responder,
                    sid: call.sid,
                    reason: JingleReason::Success,
                    finish_reason: None,
                    finish_after_error: true,
                }),
            },
            CallState::Active(call) => self.out.push(Effect::Terminate {
                peer: call.peer,
                sid: call.sid,
                reason: JingleReason::Success,
                finish_reason: None,
                finish_after_error: false,
            }),
            CallState::Idle | CallState::Ended { .. } => {}
        }
        self.take()
    }

    /// Dismiss only a terminal banner; a live call needs a wire-answering
    /// action.
    pub fn dismiss(&mut self) {
        if matches!(self.state, CallState::Ended { .. }) {
            self.cancel_timers();
            self.transition(CallState::Idle);
        }
    }

    // MARK: Stanzas

    /// Reduce an arrival, then answer it. Only events from other people
    /// are answered.
    pub fn handle(&mut self, event: &CallEvent) -> Vec<Effect> {
        // A proposal for a call that is already over, or that its caller
        // already withdrew (a retract can overtake its proposal), is not
        // a new call.
        if matches!(event.kind, CallEventKind::Propose(_))
            && (self.is_finished(&event.sid) || self.aborted.contains(&event.sid))
        {
            return Vec::new();
        }
        // This client carries audio only: a proposal with no audio track
        // could never connect, so it is refused rather than rung.
        if matches!(&event.kind, CallEventKind::Propose(media) if !media.audio)
            && event.from.to_bare() != self.own.to_bare()
        {
            self.out.push(Effect::Send(Wire::Reject {
                to: event.from.clone(),
                sid: event.sid.clone(),
                tie_break: false,
            }));
            return self.take();
        }
        let before = self.state.clone();
        if matches!(
            event.kind,
            CallEventKind::Reject { .. } | CallEventKind::Retract { .. }
        ) && before.sid() != Some(&event.sid)
            && !self.aborted.contains(&event.sid)
        {
            self.aborted.push_back(event.sid.clone());
            if self.aborted.len() > ABORTED_MEMORY {
                self.aborted.pop_front();
            }
        }
        let own = event.from.to_bare() == self.own.to_bare();
        let next = reduce(&before, event, own);
        if next != before {
            self.transition(next);
        }
        // The ring is answered only when the reducer took this proceed's
        // sender as the responder; an ignored proceed leaves the timeout
        // running.
        if let (CallState::Outgoing(was), CallState::Outgoing(now)) = (&before, &self.state) {
            if was.responder.is_none() && now.responder.is_some() {
                self.ring_timer = None;
            }
        }
        if own {
            return self.take();
        }
        if let (CallEventKind::Propose(_), CallState::Incoming(call)) = (&event.kind, &self.state) {
            if call.sid == event.sid && !(before.is_incoming() && before.sid() == Some(&call.sid)) {
                self.out.push(Effect::RingIfKnown {
                    to: event.from.clone(),
                    sid: event.sid.clone(),
                });
            }
        }
        self.remote_effects(event, &before);
        self.take()
    }

    /// What an event from the peer asks of us beyond the state change.
    fn remote_effects(&mut self, event: &CallEvent, before: &CallState) {
        match &event.kind {
            CallEventKind::Propose(media) => {
                // A redelivered proposal for the slot's own session needs
                // no answer.
                if before.sid() == Some(&event.sid) {
                    return;
                }
                match before {
                    CallState::Outgoing(call) if call.to == event.from.to_bare() => {
                        self.tie_break(event, *media, call);
                    }
                    // Busy: another call holds the slot.
                    live if live.is_live() => self.out.push(Effect::Send(Wire::Reject {
                        to: event.from.clone(),
                        sid: event.sid.clone(),
                        tie_break: false,
                    })),
                    _ => {}
                }
            }
            CallEventKind::Proceed => {
                // A second device's proceed must not open a second session.
                let CallState::Outgoing(call) = before else {
                    return;
                };
                if call.sid != event.sid
                    || call.responder.is_some()
                    || call.to != event.from.to_bare()
                    || !self.still_live(&event.sid)
                {
                    return;
                }
                self.out.push(Effect::Send(Wire::SessionInitiate {
                    to: event.from.clone(),
                    initiator: self.own.clone(),
                    sid: event.sid.clone(),
                    media: call.media,
                }));
            }
            CallEventKind::SessionInitiate { media, .. } => {
                let CallState::Incoming(call) = before else {
                    return;
                };
                if call.sid != event.sid
                    || !call.accepting
                    || call.from != event.from
                    || !self.still_live(&event.sid)
                {
                    return;
                }
                self.out.push(Effect::Send(Wire::SessionAccept {
                    to: event.from.clone(),
                    responder: self.own.clone(),
                    sid: event.sid.clone(),
                    media: media.bounded_by(call.media).bounded_by(CallMedia::AUDIO),
                }));
            }
            CallEventKind::Finish { .. } | CallEventKind::SessionTerminate { .. } => {
                // XEP-0353 has both parties send `<finish/>`. Ours answers
                // the first end event for a session we were in, so our
                // other devices and the archive learn it ended even if the
                // peer drops offline.
                if let CallState::Active(call) = before {
                    if call.sid == event.sid && call.peer == event.from {
                        self.out.push(Effect::Send(Wire::Finish {
                            to: call.peer.clone(),
                            sid: call.sid.clone(),
                            reason: Some(JingleReason::Success),
                        }));
                    }
                }
            }
            _ => {}
        }
    }

    /// XEP-0353 tie-break-1: both sides proposed to each other at once. The
    /// lower session id wins; the loser retracts its own proposal and rings
    /// for the winner's.
    fn tie_break(&mut self, event: &CallEvent, media: CallMedia, previous: &OutgoingCall) {
        if incoming_wins_tie_break(&event.sid, &previous.sid, &event.from, &self.own) {
            self.out.push(Effect::Send(Wire::RetractTieBreak {
                to: event.from.clone(),
                sid: previous.sid.clone(),
            }));
            self.cancel_timers();
            self.transition(CallState::Incoming(IncomingCall {
                from: event.from.clone(),
                sid: event.sid.clone(),
                media,
                accepting: false,
            }));
            self.out.push(Effect::RingIfKnown {
                to: event.from.clone(),
                sid: event.sid.clone(),
            });
        } else {
            self.out.push(Effect::Send(Wire::Reject {
                to: event.from.clone(),
                sid: event.sid.clone(),
                tie_break: true,
            }));
        }
    }

    fn still_live(&self, sid: &str) -> bool {
        self.state.sid() == Some(sid) && self.state.is_live()
    }

    // MARK: Outcomes

    /// A [`Effect::Send`] finished.
    pub fn sent(&mut self, wire: &Wire, ok: bool) -> Vec<Effect> {
        match (wire, ok) {
            (Wire::Propose { sid, .. }, true) => self.schedule_ring_timeout(sid),
            (Wire::Propose { sid, .. }, false) => {
                if matches!(&self.state, CallState::Outgoing(call) if call.sid == *sid) {
                    self.transition(CallState::Ended {
                        sid: sid.clone(),
                        reason: CallEndReason::Error,
                    });
                }
                self.last_error = Some(wire.failure());
            }
            (Wire::Proceed { sid, .. }, true) => self.schedule_incoming_timeout(sid),
            (Wire::Proceed { sid, .. }, false) => {
                if let CallState::Incoming(call) = &self.state {
                    if call.sid == *sid {
                        let mut call = call.clone();
                        call.accepting = false;
                        self.state = CallState::Incoming(call);
                        // The ring is back, and its first timer may have
                        // come due while the answer was on its way.
                        self.schedule_unanswered_timeout(sid);
                    }
                }
                self.last_error = Some(wire.failure());
            }
            (Wire::SessionInitiate { sid, .. }, true) => {
                self.schedule_session_timeout(sid);
            }
            (Wire::SessionInitiate { sid, .. } | Wire::SessionAccept { sid, .. }, false) => {
                self.fail_call(sid, wire.failure());
            }
            (_, false) => self.last_error = Some(wire.failure()),
            (_, true) => {}
        }
        self.take()
    }

    /// A [`Effect::Terminate`] could not end the session.
    pub fn terminate_failed(&mut self) {
        self.last_error = Some("The call could not be ended cleanly.");
    }

    fn fail_call(&mut self, sid: &str, message: &'static str) {
        if !self.still_live(sid) {
            return;
        }
        self.cancel_timers();
        self.transition(CallState::Ended {
            sid: sid.to_string(),
            reason: CallEndReason::Error,
        });
        self.last_error = Some(message);
    }

    /// The media plane lost the call or could not join it. Nobody chose
    /// this ending, so the slot keeps it on show as an ended call.
    pub fn media_ended(&mut self, sid: &str, reason: JingleReason) -> Vec<Effect> {
        let CallState::Active(call) = self.state.clone() else {
            return Vec::new();
        };
        if call.sid != sid {
            return Vec::new();
        }
        self.cancel_timers();
        self.transition(CallState::Ended {
            sid: call.sid.clone(),
            reason: CallEndReason::Finished(Some(reason)),
        });
        self.out.push(Effect::Terminate {
            peer: call.peer,
            sid: call.sid,
            reason,
            finish_reason: None,
            finish_after_error: false,
        });
        self.take()
    }

    // MARK: Timers

    pub fn timer_fired(&mut self, timer: Timer, token: u64) -> Vec<Effect> {
        let slot = match timer {
            Timer::Ring => &mut self.ring_timer,
            Timer::Session => &mut self.session_timer,
        };
        if *slot != Some(token) {
            return Vec::new();
        }
        *slot = None;
        let state = self.state.clone();
        match (timer, state) {
            // Nobody picked up: retract, which also tells the peer's
            // other devices.
            (Timer::Ring, CallState::Outgoing(call)) if call.responder.is_none() => {
                self.transition(CallState::Ended {
                    sid: call.sid.clone(),
                    reason: CallEndReason::Timeout,
                });
                self.out.push(Effect::Send(Wire::Retract {
                    to: call.to,
                    sid: call.sid,
                }));
            }
            // A call to us nobody answered: it ends by itself, and nobody
            // is told since there is nobody left to tell.
            (Timer::Ring, CallState::Incoming(call)) if !call.accepting => {
                self.transition(CallState::Ended {
                    sid: call.sid,
                    reason: CallEndReason::Missed,
                });
            }
            (Timer::Session, CallState::Outgoing(call)) => {
                self.transition(CallState::Ended {
                    sid: call.sid.clone(),
                    reason: CallEndReason::Timeout,
                });
                if let Some(peer) = call.responder {
                    // The terminate is an IQ only the answering device
                    // sees; the finish tells the peer's other devices the
                    // call is over.
                    self.out.push(Effect::Terminate {
                        peer,
                        sid: call.sid,
                        reason: JingleReason::Timeout,
                        finish_reason: Some(JingleReason::Timeout),
                        finish_after_error: true,
                    });
                }
            }
            (Timer::Session, CallState::Incoming(call)) => {
                self.transition(CallState::Ended {
                    sid: call.sid.clone(),
                    reason: CallEndReason::Timeout,
                });
                self.out.push(Effect::Send(Wire::Finish {
                    to: call.from,
                    sid: call.sid,
                    reason: Some(JingleReason::Timeout),
                }));
            }
            _ => {}
        }
        self.take()
    }

    fn start_timer(&mut self, timer: Timer, after: Duration) {
        self.next_token += 1;
        let token = self.next_token;
        match timer {
            Timer::Ring => self.ring_timer = Some(token),
            Timer::Session => self.session_timer = Some(token),
        }
        self.out.push(Effect::StartTimer {
            timer,
            token,
            after,
        });
    }

    fn schedule_ring_timeout(&mut self, sid: &str) {
        // An answered ring is bounded by the session timeout instead.
        if matches!(&self.state, CallState::Outgoing(call) if call.sid == sid && call.responder.is_none())
        {
            self.start_timer(Timer::Ring, RING_TIMEOUT);
        }
    }

    fn schedule_unanswered_timeout(&mut self, sid: &str) {
        if matches!(&self.state, CallState::Incoming(call) if call.sid == sid && !call.accepting) {
            self.start_timer(Timer::Ring, UNANSWERED_TIMEOUT);
        }
    }

    fn schedule_session_timeout(&mut self, sid: &str) {
        if matches!(&self.state, CallState::Outgoing(call) if call.sid == sid) {
            self.start_timer(Timer::Session, SESSION_TIMEOUT);
        }
    }

    fn schedule_incoming_timeout(&mut self, sid: &str) {
        if self.state.is_incoming() && self.state.sid() == Some(sid) {
            self.start_timer(Timer::Session, SESSION_TIMEOUT);
        }
    }

    fn cancel_timers(&mut self) {
        self.ring_timer = None;
        self.session_timer = None;
    }

    // MARK: Transitions

    /// Every state change goes through here: it remembers finished calls,
    /// keeps the timers to the phase and starts or stops media.
    fn transition(&mut self, next: CallState) {
        if self.state == next {
            return;
        }
        let previous = std::mem::replace(&mut self.state, next);
        if (previous.is_outgoing() && !self.state.is_outgoing())
            || (previous.is_incoming() && !self.state.is_incoming())
        {
            self.cancel_timers();
        }
        self.last_error = None;
        if previous.is_live() && (previous.sid() != self.state.sid() || !self.state.is_live()) {
            if let Some(sid) = previous.sid() {
                self.mark_finished(sid.to_string());
            }
        }
        if let CallState::Ended { sid, .. } = &self.state {
            self.mark_finished(sid.clone());
        }
        // Every way a call starts ringing here bounds the ring: a proposal,
        // a tie-break won.
        if let CallState::Incoming(call) = &self.state {
            let sid = call.sid.clone();
            let was_ringing = previous.is_incoming() && previous.sid() == Some(&sid);
            if !call.accepting && !was_ringing {
                self.schedule_unanswered_timeout(&sid);
            }
        }
        let connecting = match (&previous, &self.state) {
            (CallState::Active(old), CallState::Active(now)) if old.sid == now.sid => None,
            (_, CallState::Active(now)) => Some(now.clone()),
            _ => None,
        };
        if let Some(call) = connecting {
            if let Some(defect) = call.join.defect() {
                tracing::warn!(?defect, "call join credentials are unusable");
                self.reject_credentials(&call);
            } else {
                self.out.push(Effect::ConnectMedia {
                    sid: call.sid,
                    join: call.join,
                    media: call.media.bounded_by(CallMedia::AUDIO),
                });
            }
        } else if previous_was_active_media(&previous, &self.state) {
            self.out.push(Effect::DisconnectMedia);
        }
    }

    /// Credentials that cannot join the room end the call as a failure to
    /// connect, as the Apple client does with `gone`. No media was started.
    fn reject_credentials(&mut self, call: &ActiveCall) {
        self.state = CallState::Ended {
            sid: call.sid.clone(),
            reason: CallEndReason::Finished(Some(JingleReason::Gone)),
        };
        self.mark_finished(call.sid.clone());
        self.out.push(Effect::Terminate {
            peer: call.peer.clone(),
            sid: call.sid.clone(),
            reason: JingleReason::Gone,
            finish_reason: None,
            finish_after_error: false,
        });
    }

    fn mark_finished(&mut self, sid: String) {
        if self.finished.contains(&sid) {
            return;
        }
        self.finished.push_back(sid);
        if self.finished.len() > FINISHED_MEMORY {
            self.finished.pop_front();
        }
    }
}

/// Media was running for `previous` and is not for `next`.
fn previous_was_active_media(previous: &CallState, next: &CallState) -> bool {
    matches!(previous, CallState::Active(_)) && !matches!(next, CallState::Active(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn jid(text: &str) -> Jid {
        Jid::new(text).unwrap()
    }

    fn bare(text: &str) -> BareJid {
        BareJid::new(text).unwrap()
    }

    fn engine() -> CallEngine {
        CallEngine::new(jid("alice@example.org/laptop"))
    }

    fn token(room: &str, identity: &str) -> String {
        let encode =
            |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes());
        let payload =
            format!(r#"{{"sub":"{identity}","video":{{"roomJoin":true,"room":"{room}"}}}}"#);
        format!("{}.{}.sig", encode("{}"), encode(&payload))
    }

    fn join() -> CallJoin {
        CallJoin {
            url: "wss://sfu.example".into(),
            room: "room".into(),
            identity: "alice".into(),
            token: token("room", "alice"),
        }
    }

    fn event(from: &str, sid: &str, kind: CallEventKind) -> CallEvent {
        CallEvent {
            from: jid(from),
            sid: sid.into(),
            kind,
        }
    }

    fn propose(from: &str, sid: &str) -> CallEvent {
        event(from, sid, CallEventKind::Propose(CallMedia::AUDIO))
    }

    fn sends(effects: &[Effect]) -> Vec<&Wire> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send(wire) => Some(wire),
                _ => None,
            })
            .collect()
    }

    fn timer(effects: &[Effect], which: Timer) -> Option<u64> {
        effects.iter().find_map(|effect| match effect {
            Effect::StartTimer { timer, token, .. } if *timer == which => Some(*token),
            _ => None,
        })
    }

    /// An engine holding an answered, connected call to bob.
    fn active_engine() -> CallEngine {
        let mut engine = engine();
        engine.start_call(bare("bob@example.org"), "c1".into());
        engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Proceed,
        ));
        engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::SessionAccept {
                join: join(),
                media: CallMedia::AUDIO,
            },
        ));
        assert!(matches!(engine.state(), CallState::Active(_)));
        engine
    }

    #[test]
    fn placing_a_call_claims_the_slot_and_proposes_to_the_bare_peer() {
        let mut engine = engine();
        let effects = engine
            .start_call(bare("bob@example.org"), "c1".into())
            .unwrap();
        assert_eq!(
            effects,
            vec![Effect::Send(Wire::Propose {
                to: bare("bob@example.org"),
                sid: "c1".into(),
                media: CallMedia::AUDIO
            })]
        );
        assert!(matches!(engine.state(), CallState::Outgoing(_)));
        // The slot is taken.
        assert!(engine
            .start_call(bare("carol@example.org"), "c2".into())
            .is_none());
    }

    #[test]
    fn the_ring_times_out_into_a_retract_once_the_propose_is_out() {
        let mut engine = engine();
        let wire = Wire::Propose {
            to: bare("bob@example.org"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
        };
        engine.start_call(bare("bob@example.org"), "c1".into());
        let effects = engine.sent(&wire, true);
        let token = timer(&effects, Timer::Ring).expect("ring timer");
        let effects = engine.timer_fired(Timer::Ring, token);
        assert_eq!(
            sends(&effects),
            vec![&Wire::Retract {
                to: bare("bob@example.org"),
                sid: "c1".into()
            }]
        );
        assert_eq!(
            engine.state(),
            &CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Timeout
            }
        );
    }

    #[test]
    fn a_failed_propose_ends_the_call_with_an_error() {
        let mut engine = engine();
        let wire = Wire::Propose {
            to: bare("bob@example.org"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
        };
        engine.start_call(bare("bob@example.org"), "c1".into());
        engine.sent(&wire, false);
        assert_eq!(
            engine.state(),
            &CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Error
            }
        );
        assert!(engine.last_error().is_some());
    }

    #[test]
    fn a_stale_or_cancelled_timer_does_nothing() {
        let mut engine = engine();
        let wire = Wire::Propose {
            to: bare("bob@example.org"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
        };
        engine.start_call(bare("bob@example.org"), "c1".into());
        let token = timer(&engine.sent(&wire, true), Timer::Ring).unwrap();
        engine.hang_up();
        assert!(engine.timer_fired(Timer::Ring, token).is_empty());
        assert_eq!(engine.state(), &CallState::Idle);
    }

    #[test]
    fn the_first_proceed_starts_the_session_and_stops_the_ring_timer() {
        let mut engine = engine();
        let wire = Wire::Propose {
            to: bare("bob@example.org"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
        };
        engine.start_call(bare("bob@example.org"), "c1".into());
        let ring = timer(&engine.sent(&wire, true), Timer::Ring).unwrap();
        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Proceed,
        ));
        assert_eq!(
            sends(&effects),
            vec![&Wire::SessionInitiate {
                to: jid("bob@example.org/phone"),
                initiator: jid("alice@example.org/laptop"),
                sid: "c1".into(),
                media: CallMedia::AUDIO
            }]
        );
        assert!(engine.timer_fired(Timer::Ring, ring).is_empty());
        // A second device's proceed opens no second session.
        let again = engine.handle(&event(
            "bob@example.org/tablet",
            "c1",
            CallEventKind::Proceed,
        ));
        assert!(again.is_empty());
        // The session has its own bound.
        let initiate = Wire::SessionInitiate {
            to: jid("bob@example.org/phone"),
            initiator: jid("alice@example.org/laptop"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
        };
        let session = timer(&engine.sent(&initiate, true), Timer::Session).unwrap();
        let effects = engine.timer_fired(Timer::Session, session);
        assert_eq!(
            effects,
            vec![Effect::Terminate {
                peer: jid("bob@example.org/phone"),
                sid: "c1".into(),
                reason: JingleReason::Timeout,
                finish_reason: Some(JingleReason::Timeout),
                finish_after_error: true
            }]
        );
    }

    #[test]
    fn a_session_accept_connects_media_and_the_end_disconnects_it() {
        let mut engine = engine();
        engine.start_call(bare("bob@example.org"), "c1".into());
        engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Proceed,
        ));
        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::SessionAccept {
                join: join(),
                media: CallMedia::AUDIO,
            },
        ));
        assert_eq!(
            effects,
            vec![Effect::ConnectMedia {
                sid: "c1".into(),
                join: join(),
                media: CallMedia::AUDIO
            }]
        );
        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::SessionTerminate {
                reason: Some(JingleReason::Success),
            },
        ));
        assert_eq!(
            effects,
            vec![
                Effect::DisconnectMedia,
                Effect::Send(Wire::Finish {
                    to: jid("bob@example.org/phone"),
                    sid: "c1".into(),
                    reason: Some(JingleReason::Success)
                })
            ]
        );
        assert!(engine.is_finished("c1"));
    }

    #[test]
    fn unusable_credentials_end_the_call_instead_of_connecting() {
        let mut engine = engine();
        engine.start_call(bare("bob@example.org"), "c1".into());
        engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Proceed,
        ));
        let mut bad = join();
        bad.room = "other".into();
        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::SessionAccept {
                join: bad,
                media: CallMedia::AUDIO,
            },
        ));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::ConnectMedia { .. })));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Terminate {
                reason: JingleReason::Gone,
                ..
            }
        )));
        assert!(matches!(engine.state(), CallState::Ended { .. }));
    }

    #[test]
    fn hanging_up_an_active_call_terminates_then_finishes() {
        let mut engine = active_engine();
        let effects = engine.hang_up();
        assert_eq!(
            effects,
            vec![
                Effect::DisconnectMedia,
                Effect::Terminate {
                    peer: jid("bob@example.org/phone"),
                    sid: "c1".into(),
                    reason: JingleReason::Success,
                    finish_reason: None,
                    finish_after_error: false
                }
            ]
        );
        assert_eq!(engine.state(), &CallState::Idle);
    }

    #[test]
    fn hanging_up_before_an_answer_retracts_and_after_one_terminates() {
        let mut engine = engine();
        engine.start_call(bare("bob@example.org"), "c1".into());
        assert_eq!(
            engine.hang_up(),
            vec![Effect::Send(Wire::Retract {
                to: bare("bob@example.org"),
                sid: "c1".into()
            })]
        );

        engine.start_call(bare("bob@example.org"), "c2".into());
        engine.handle(&event(
            "bob@example.org/phone",
            "c2",
            CallEventKind::Proceed,
        ));
        assert_eq!(
            engine.hang_up(),
            vec![Effect::Terminate {
                peer: jid("bob@example.org/phone"),
                sid: "c2".into(),
                reason: JingleReason::Success,
                finish_reason: None,
                finish_after_error: true
            }]
        );
    }

    #[test]
    fn an_incoming_call_rings_and_is_answered_with_proceed_then_accept() {
        let mut engine = engine();
        let effects = engine.handle(&propose("bob@example.org/phone", "c1"));
        assert!(effects.contains(&Effect::RingIfKnown {
            to: jid("bob@example.org/phone"),
            sid: "c1".into()
        }));
        assert!(timer(&effects, Timer::Ring).is_some());
        assert!(matches!(engine.state(), CallState::Incoming(_)));

        let effects = engine.accept();
        assert_eq!(
            effects,
            vec![Effect::Send(Wire::Proceed {
                to: jid("bob@example.org/phone"),
                sid: "c1".into()
            })]
        );
        // A second tap answers nothing.
        assert!(engine.accept().is_empty());

        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::SessionInitiate {
                join: join(),
                media: CallMedia::AUDIO,
            },
        ));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Send(Wire::SessionAccept { responder, .. })
                if *responder == jid("alice@example.org/laptop")
        )));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::ConnectMedia { .. })));
    }

    #[test]
    fn a_ringing_call_is_declined_with_reject_and_an_accepted_one_with_finish_cancel() {
        let mut engine = engine();
        engine.handle(&propose("bob@example.org/phone", "c1"));
        assert_eq!(
            engine.decline(),
            vec![Effect::Send(Wire::Reject {
                to: jid("bob@example.org/phone"),
                sid: "c1".into(),
                tie_break: false
            })]
        );
        assert_eq!(engine.state(), &CallState::Idle);

        engine.handle(&propose("bob@example.org/phone", "c2"));
        engine.accept();
        assert_eq!(
            engine.decline(),
            vec![Effect::Send(Wire::Finish {
                to: jid("bob@example.org/phone"),
                sid: "c2".into(),
                reason: Some(JingleReason::Cancel)
            })]
        );
    }

    #[test]
    fn a_failed_answer_puts_the_ring_back() {
        let mut engine = engine();
        engine.handle(&propose("bob@example.org/phone", "c1"));
        engine.accept();
        let wire = Wire::Proceed {
            to: jid("bob@example.org/phone"),
            sid: "c1".into(),
        };
        let effects = engine.sent(&wire, false);
        assert!(timer(&effects, Timer::Ring).is_some());
        assert!(matches!(
            engine.state(),
            CallState::Incoming(IncomingCall {
                accepting: false,
                ..
            })
        ));
        assert!(engine.last_error().is_some());
    }

    #[test]
    fn an_unanswered_ring_becomes_a_missed_call_without_telling_anyone() {
        let mut engine = engine();
        let effects = engine.handle(&propose("bob@example.org/phone", "c1"));
        let token = timer(&effects, Timer::Ring).unwrap();
        assert!(engine.timer_fired(Timer::Ring, token).is_empty());
        assert_eq!(
            engine.state(),
            &CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Missed
            }
        );
    }

    #[test]
    fn an_answered_call_whose_session_never_arrives_finishes_with_a_timeout() {
        let mut engine = engine();
        engine.handle(&propose("bob@example.org/phone", "c1"));
        engine.accept();
        let proceed = Wire::Proceed {
            to: jid("bob@example.org/phone"),
            sid: "c1".into(),
        };
        let token = timer(&engine.sent(&proceed, true), Timer::Session).unwrap();
        let effects = engine.timer_fired(Timer::Session, token);
        assert_eq!(
            sends(&effects),
            vec![&Wire::Finish {
                to: jid("bob@example.org/phone"),
                sid: "c1".into(),
                reason: Some(JingleReason::Timeout)
            }]
        );
    }

    #[test]
    fn a_proposal_while_busy_is_rejected_and_a_replay_of_a_finished_call_is_ignored() {
        let mut engine = active_engine();
        let effects = engine.handle(&propose("carol@example.org/phone", "c9"));
        assert_eq!(
            effects,
            vec![Effect::Send(Wire::Reject {
                to: jid("carol@example.org/phone"),
                sid: "c9".into(),
                tie_break: false
            })]
        );
        // A same-peer proposal during a call is also refused (no upgrade).
        let effects = engine.handle(&propose("bob@example.org/phone", "c10"));
        assert_eq!(sends(&effects).len(), 1);

        let mut idle = self::engine();
        idle.handle(&propose("bob@example.org/phone", "c1"));
        idle.decline();
        assert!(idle
            .handle(&propose("bob@example.org/phone", "c1"))
            .is_empty());
        assert_eq!(idle.state(), &CallState::Idle);
    }

    #[test]
    fn a_retracted_proposal_never_rings_if_it_arrives_late() {
        let mut engine = engine();
        engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Retract {
                reason: None,
                tie_break: false,
            },
        ));
        // The retract named no live slot, but its sid is remembered.
        assert!(engine
            .handle(&propose("bob@example.org/phone", "c1"))
            .is_empty());
        assert_eq!(engine.state(), &CallState::Idle);
    }

    #[test]
    fn the_lower_sid_wins_a_simultaneous_proposal() {
        // Ours is "c5"; theirs "c1" is lower and wins: we retract ours
        // with the tie-break marker and ring for theirs.
        let mut engine = engine();
        engine.start_call(bare("bob@example.org"), "c5".into());
        let effects = engine.handle(&propose("bob@example.org/phone", "c1"));
        assert_eq!(
            sends(&effects),
            vec![&Wire::RetractTieBreak {
                to: jid("bob@example.org/phone"),
                sid: "c5".into()
            }]
        );
        assert!(effects.contains(&Effect::RingIfKnown {
            to: jid("bob@example.org/phone"),
            sid: "c1".into()
        }));
        assert!(matches!(
            engine.state(),
            CallState::Incoming(IncomingCall { sid, .. }) if sid == "c1"
        ));

        // Ours is "c1"; theirs "c5" loses and is rejected as a tie-break.
        let mut engine = self::engine();
        engine.start_call(bare("bob@example.org"), "c1".into());
        let effects = engine.handle(&propose("bob@example.org/phone", "c5"));
        assert_eq!(
            sends(&effects),
            vec![&Wire::Reject {
                to: jid("bob@example.org/phone"),
                sid: "c5".into(),
                tie_break: true
            }]
        );
        assert!(matches!(engine.state(), CallState::Outgoing(_)));
    }

    #[test]
    fn a_video_call_is_answered_with_audio_only_and_one_without_audio_is_refused() {
        let mut engine = engine();
        engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::Propose(CallMedia {
                audio: true,
                video: true,
            }),
        ));
        engine.accept();
        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c1",
            CallEventKind::SessionInitiate {
                join: join(),
                media: CallMedia {
                    audio: true,
                    video: true,
                },
            },
        ));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Send(Wire::SessionAccept { media, .. }) if *media == CallMedia::AUDIO
        )));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::ConnectMedia { media, .. } if *media == CallMedia::AUDIO
        )));

        let mut engine = self::engine();
        let effects = engine.handle(&event(
            "bob@example.org/phone",
            "c2",
            CallEventKind::Propose(CallMedia {
                audio: false,
                video: true,
            }),
        ));
        assert_eq!(
            sends(&effects),
            vec![&Wire::Reject {
                to: jid("bob@example.org/phone"),
                sid: "c2".into(),
                tie_break: false
            }]
        );
        assert_eq!(engine.state(), &CallState::Idle);
    }

    #[test]
    fn our_own_devices_events_change_state_but_are_never_answered() {
        let mut engine = engine();
        engine.handle(&propose("bob@example.org/phone", "c1"));
        let effects = engine.handle(&event(
            "alice@example.org/phone",
            "c1",
            CallEventKind::Proceed,
        ));
        assert!(effects.is_empty());
        assert_eq!(engine.state(), &CallState::Idle);
    }

    #[test]
    fn losing_the_media_connection_ends_the_active_call_and_names_the_condition() {
        let mut engine = active_engine();
        let effects = engine.media_ended("c1", JingleReason::ConnectivityError);
        assert_eq!(
            effects,
            vec![
                Effect::DisconnectMedia,
                Effect::Terminate {
                    peer: jid("bob@example.org/phone"),
                    sid: "c1".into(),
                    reason: JingleReason::ConnectivityError,
                    finish_reason: None,
                    finish_after_error: false
                }
            ]
        );
        assert_eq!(
            engine.state(),
            &CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Finished(Some(JingleReason::ConnectivityError))
            }
        );
        // Not for another session's media, nor twice.
        let mut engine = active_engine();
        assert!(engine.media_ended("c9", JingleReason::Gone).is_empty());
    }

    #[test]
    fn dismissing_clears_only_an_ended_banner() {
        let mut engine = active_engine();
        engine.dismiss();
        assert!(matches!(engine.state(), CallState::Active(_)));
        engine.media_ended("c1", JingleReason::Gone);
        engine.dismiss();
        assert_eq!(engine.state(), &CallState::Idle);
    }
}
