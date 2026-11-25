//! What the call window says and offers for each phase of the call slot,
//! without widgets.

use crate::call::{format_elapsed, CallState};

/// What the media connection is doing, as far as the person on the call
/// can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaPhase {
    /// Not started, or still joining the room.
    #[default]
    Connecting,
    Connected,
    /// The connection dropped and is being restored.
    Reconnecting,
}

/// A button the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Answer,
    Decline,
    /// Take back a call being placed, or an answer on its way.
    Cancel,
    HangUp,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub status: String,
    /// In the order shown, the primary last.
    pub buttons: Vec<Button>,
    /// The microphone and device controls apply: the call is established.
    pub controls: bool,
}

/// `error` is the last failed send, shown under an ended call's summary.
pub fn screen(
    state: &CallState,
    phase: MediaPhase,
    elapsed_seconds: u64,
    error: Option<&str>,
) -> Screen {
    match state {
        CallState::Idle => Screen {
            status: String::new(),
            buttons: Vec::new(),
            controls: false,
        },
        CallState::Incoming(call) if call.accepting => Screen {
            status: "Connecting…".into(),
            buttons: vec![Button::Cancel],
            controls: false,
        },
        CallState::Incoming(_) => Screen {
            status: "Incoming voice call".into(),
            buttons: vec![Button::Decline, Button::Answer],
            controls: false,
        },
        CallState::Outgoing(call) => Screen {
            status: if call.responder.is_some() {
                "Connecting…"
            } else if call.ringing {
                "Ringing…"
            } else {
                "Calling…"
            }
            .into(),
            buttons: vec![Button::Cancel],
            controls: false,
        },
        CallState::Active(_) => Screen {
            status: match phase {
                MediaPhase::Connecting => "Connecting…".into(),
                MediaPhase::Connected => format_elapsed(elapsed_seconds),
                MediaPhase::Reconnecting => "Reconnecting…".into(),
            },
            buttons: vec![Button::HangUp],
            controls: true,
        },
        CallState::Ended { reason, .. } => Screen {
            status: match error {
                Some(error) => format!("{}. {error}", reason.summary()),
                None => reason.summary().to_string(),
            },
            buttons: vec![Button::Close],
            controls: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::{ActiveCall, CallEndReason, CallJoin, CallMedia, IncomingCall, OutgoingCall};
    use jid::{BareJid, Jid};

    fn jid(text: &str) -> Jid {
        Jid::new(text).unwrap()
    }

    fn outgoing(ringing: bool, answered: bool) -> CallState {
        CallState::Outgoing(OutgoingCall {
            to: BareJid::new("bob@example.org").unwrap(),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            initiator: jid("alice@example.org/laptop"),
            ringing,
            responder: answered.then(|| jid("bob@example.org/phone")),
        })
    }

    fn active() -> CallState {
        CallState::Active(ActiveCall {
            peer: jid("bob@example.org/phone"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            join: CallJoin {
                url: String::new(),
                room: String::new(),
                identity: String::new(),
                token: String::new(),
            },
        })
    }

    #[test]
    fn a_ringing_call_offers_answer_and_decline() {
        let state = CallState::Incoming(IncomingCall {
            from: jid("bob@example.org/phone"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            accepting: false,
        });
        let shown = screen(&state, MediaPhase::Connecting, 0, None);
        assert_eq!(shown.buttons, [Button::Decline, Button::Answer]);
        assert!(!shown.controls);
    }

    #[test]
    fn an_answer_on_its_way_can_only_be_taken_back() {
        let state = CallState::Incoming(IncomingCall {
            from: jid("bob@example.org/phone"),
            sid: "c1".into(),
            media: CallMedia::AUDIO,
            accepting: true,
        });
        let shown = screen(&state, MediaPhase::Connecting, 0, None);
        assert_eq!(shown.buttons, [Button::Cancel]);
        assert_eq!(shown.status, "Connecting…");
    }

    #[test]
    fn a_call_being_placed_reports_how_far_it_got() {
        let status = |state| screen(&state, MediaPhase::Connecting, 0, None).status;
        assert_eq!(status(outgoing(false, false)), "Calling…");
        assert_eq!(status(outgoing(true, false)), "Ringing…");
        assert_eq!(status(outgoing(true, true)), "Connecting…");
    }

    #[test]
    fn an_established_call_counts_time_once_its_media_is_up() {
        let state = active();
        assert_eq!(
            screen(&state, MediaPhase::Connecting, 5, None).status,
            "Connecting…"
        );
        let shown = screen(&state, MediaPhase::Connected, 65, None);
        assert_eq!(shown.status, "1:05");
        assert_eq!(shown.buttons, [Button::HangUp]);
        assert!(shown.controls);
        assert_eq!(
            screen(&state, MediaPhase::Reconnecting, 65, None).status,
            "Reconnecting…"
        );
    }

    #[test]
    fn an_ended_call_says_why_and_offers_to_close() {
        let state = CallState::Ended {
            sid: "c1".into(),
            reason: CallEndReason::Timeout,
        };
        let shown = screen(&state, MediaPhase::Connecting, 0, None);
        assert_eq!(shown.status, "No answer");
        assert_eq!(shown.buttons, [Button::Close]);
        let shown = screen(
            &CallState::Ended {
                sid: "c1".into(),
                reason: CallEndReason::Error,
            },
            MediaPhase::Connecting,
            0,
            Some("The call could not be placed."),
        );
        assert_eq!(
            shown.status,
            "Call could not connect. The call could not be placed."
        );
    }
}
