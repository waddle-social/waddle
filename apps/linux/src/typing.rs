//! Typing indicators (XEP-0085): what the composer sends, and who is
//! shown as typing.
//!
//! Sending follows the web and Apple clients: `composing` once when the
//! user starts typing, `paused` after five quiet seconds, `active` when
//! the draft is emptied or left behind. A sent message ends the state
//! implicitly (the message carries `active`). Receiving shows a name until
//! its `paused`/`active`/message, or until 30 seconds pass without a fresh
//! `composing`, so a lost stanza never leaves a ghost.
//!
//! Time is passed in, so none of this needs a clock or GTK.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use waddle_xmpp_client_ffi::{WaddleChatState, WaddleMessage};

/// Quiet time after which `composing` becomes `paused`.
pub const PAUSE_AFTER: Duration = Duration::from_secs(5);
/// How long a `composing` is believed without being renewed.
pub const SHOWN_FOR: Duration = Duration::from_secs(30);

/// A chat state to send to a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Send {
    /// The timeline key the state was typed in.
    pub key: String,
    pub state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Composing,
    Paused,
    Active,
}

impl State {
    pub fn wire(self) -> WaddleChatState {
        match self {
            Self::Composing => WaddleChatState::Composing,
            Self::Paused => WaddleChatState::Paused,
            Self::Active => WaddleChatState::Active,
        }
    }
}

/// What the user's composer has told the other side.
#[derive(Debug, Default)]
pub struct Sender {
    /// The conversation we are composing in, and when to say `paused`.
    composing: Option<(String, Instant)>,
    /// A `paused` has been sent for this one and not yet superseded.
    paused: Option<String>,
}

impl Sender {
    /// The composer text changed in conversation `key`.
    pub fn edited(&mut self, key: &str, text_is_empty: bool, now: Instant) -> Vec<Send> {
        let mut out = Vec::new();
        // Typing somewhere else ends the earlier conversation's state.
        if let Some((held, _)) = &self.composing {
            if held != key {
                out.push(Send {
                    key: held.clone(),
                    state: State::Active,
                });
                self.composing = None;
            }
        }
        if text_is_empty {
            let was_composing = self.composing.take().is_some();
            if was_composing || self.paused.as_deref() == Some(key) {
                out.push(Send {
                    key: key.to_string(),
                    state: State::Active,
                });
            }
            self.paused = None;
            return out;
        }
        if self.composing.is_none() {
            out.push(Send {
                key: key.to_string(),
                state: State::Composing,
            });
        }
        self.paused = None;
        self.composing = Some((key.to_string(), now + PAUSE_AFTER));
        out
    }

    /// A tick: say `paused` once the quiet time has passed.
    pub fn tick(&mut self, now: Instant) -> Vec<Send> {
        match &self.composing {
            Some((key, due)) if now >= *due => {
                let key = key.clone();
                self.composing = None;
                self.paused = Some(key.clone());
                vec![Send {
                    key,
                    state: State::Paused,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// The message was sent: it carried the state, so there is nothing
    /// more to say.
    pub fn message_sent(&mut self, key: &str) {
        if self.composing.as_ref().is_some_and(|(held, _)| held == key) {
            self.composing = None;
        }
        if self.paused.as_deref() == Some(key) {
            self.paused = None;
        }
    }

    /// The user left the conversation (switched away, window hidden).
    pub fn left(&mut self, key: &str) -> Vec<Send> {
        let was = self.composing.as_ref().is_some_and(|(held, _)| held == key)
            || self.paused.as_deref() == Some(key);
        self.message_sent(key);
        if was {
            vec![Send {
                key: key.to_string(),
                state: State::Active,
            }]
        } else {
            Vec::new()
        }
    }

    /// Whatever was going on, wherever it was, is over.
    pub fn end(&mut self) -> Vec<Send> {
        let key = self
            .composing
            .take()
            .map(|(key, _)| key)
            .or_else(|| self.paused.take());
        self.paused = None;
        key.map(|key| Send {
            key,
            state: State::Active,
        })
        .into_iter()
        .collect()
    }

    /// Whether a tick is worth scheduling.
    pub fn is_composing(&self) -> bool {
        self.composing.is_some()
    }

    /// Sign-out or a lost connection: nothing can be said, nothing is owed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Someone else's chat state, from a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    /// The timeline key (room or peer bare JID).
    pub conversation: String,
    /// Who: the room nick, or the DM peer's bare JID.
    pub who: String,
    pub state: Option<State>,
}

/// The chat state a message carries, or that it ended one by being a real
/// message. Own messages (carbons, room reflections) say nothing.
pub fn heard(message: &WaddleMessage, own_bare: &str, own_nick: &str) -> Option<Heard> {
    let from: jid::Jid = message.from.as_deref()?.parse().ok()?;
    let has_body = message.body.as_deref().is_some_and(|b| !b.is_empty());
    let state = message.chat_state.map(|state| match state {
        WaddleChatState::Composing => State::Composing,
        WaddleChatState::Paused => State::Paused,
        _ => State::Active,
    });
    // A message with no state still ends the sender's composing.
    if state.is_none() && !has_body {
        return None;
    }
    // A real message ends composing whatever else it says.
    let state = if has_body { Some(State::Active) } else { state };
    match message.message_type.as_str() {
        "groupchat" => {
            let nick = from.resource()?.as_str().to_string();
            if nick == own_nick {
                return None;
            }
            Some(Heard {
                conversation: from.to_bare().to_string(),
                who: nick,
                state,
            })
        }
        "chat" if !message.muc_user && !message.is_muc => {
            let bare = from.to_bare();
            if bare.as_str() == own_bare {
                return None;
            }
            Some(Heard {
                conversation: bare.to_string(),
                who: bare.to_string(),
                state,
            })
        }
        _ => None,
    }
}

/// A message that only carries a chat state: nothing to put in a timeline.
pub fn is_state_only(message: &WaddleMessage) -> bool {
    message.chat_state.is_some()
        && message.body.as_deref().is_none_or(str::is_empty)
        && message.reaction_target_id.is_none()
        && message.retracts_id.is_none()
        && message.replaces_id.is_none()
        && message.moderation_target_id.is_none()
        && message.safety_scores.is_none()
        && message.pin_event.is_none()
        && message.call_thread.is_none()
        && message.call_thread_ended.is_none()
        && message.shared_files.is_empty()
}

/// Who is typing, per conversation, with expiry.
#[derive(Debug, Default)]
pub struct Book {
    typing: HashMap<String, HashMap<String, Instant>>,
}

impl Book {
    /// Apply what was heard. True when who is shown for that conversation
    /// changed.
    pub fn apply(&mut self, heard: &Heard, now: Instant) -> bool {
        match heard.state {
            Some(State::Composing) => {
                let people = self.typing.entry(heard.conversation.clone()).or_default();
                let new = !people.contains_key(&heard.who);
                people.insert(heard.who.clone(), now + SHOWN_FOR);
                new
            }
            _ => self.stop(&heard.conversation, &heard.who),
        }
    }

    fn stop(&mut self, conversation: &str, who: &str) -> bool {
        let Some(people) = self.typing.get_mut(conversation) else {
            return false;
        };
        let removed = people.remove(who).is_some();
        if people.is_empty() {
            self.typing.remove(conversation);
        }
        removed
    }

    /// Drop what has expired. The conversations that changed.
    pub fn sweep(&mut self, now: Instant) -> Vec<String> {
        let mut changed = Vec::new();
        self.typing.retain(|conversation, people| {
            let before = people.len();
            people.retain(|_, until| *until > now);
            if people.len() != before {
                changed.push(conversation.clone());
            }
            !people.is_empty()
        });
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.typing.is_empty()
    }

    pub fn clear(&mut self) {
        self.typing.clear();
    }

    /// Names typing in `conversation`, in a stable order. `name` turns a
    /// key into what is shown (a DM peer's JID into their name).
    pub fn names(&self, conversation: &str, name: impl Fn(&str) -> String) -> Vec<String> {
        let mut names: Vec<String> = self
            .typing
            .get(conversation)
            .map(|people| people.keys().map(|who| name(who)).collect())
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// "Ann is typing…", "Ann and Bo are typing…", "Several people are
/// typing…". Empty when nobody is.
pub fn line(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("{one} is typing…"),
        [one, two] => format!("{one} and {two} are typing…"),
        _ => "Several people are typing…".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Instant {
        // A fixed base so tests read as a timeline.
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_secs(seconds)
    }

    fn states(sends: &[Send]) -> Vec<(&str, State)> {
        sends
            .iter()
            .map(|send| (send.key.as_str(), send.state))
            .collect()
    }

    #[test]
    fn composing_is_sent_once_then_paused_after_the_quiet_time() {
        let mut sender = Sender::default();
        assert_eq!(
            states(&sender.edited("room", false, at(0))),
            [("room", State::Composing)]
        );
        // Further keystrokes only push the pause back.
        assert!(sender.edited("room", false, at(2)).is_empty());
        assert!(sender.edited("room", false, at(4)).is_empty());
        assert!(sender.tick(at(8)).is_empty());
        assert_eq!(states(&sender.tick(at(9))), [("room", State::Paused)]);
        assert!(sender.tick(at(30)).is_empty(), "paused is said once");
        // Typing again after a pause starts a new composing.
        assert_eq!(
            states(&sender.edited("room", false, at(31))),
            [("room", State::Composing)]
        );
    }

    #[test]
    fn clearing_the_draft_says_active_only_if_something_was_said() {
        let mut sender = Sender::default();
        assert!(sender.edited("room", true, at(0)).is_empty());
        sender.edited("room", false, at(0));
        assert_eq!(
            states(&sender.edited("room", true, at(1))),
            [("room", State::Active)]
        );
        assert!(!sender.is_composing());
        // After a pause the other side still shows nothing, but a cleared
        // draft is the end of the state all the same.
        sender.edited("room", false, at(2));
        sender.tick(at(8));
        assert_eq!(
            states(&sender.edited("room", true, at(9))),
            [("room", State::Active)]
        );
        assert!(sender.edited("room", true, at(10)).is_empty());
    }

    #[test]
    fn moving_to_another_conversation_ends_the_first() {
        let mut sender = Sender::default();
        sender.edited("a", false, at(0));
        assert_eq!(
            states(&sender.edited("b", false, at(1))),
            [("a", State::Active), ("b", State::Composing)]
        );
    }

    #[test]
    fn a_sent_message_ends_the_state_without_a_stanza() {
        let mut sender = Sender::default();
        sender.edited("room", false, at(0));
        sender.message_sent("room");
        assert!(sender.tick(at(60)).is_empty());
        assert!(sender.left("room").is_empty());
    }

    #[test]
    fn ending_closes_whatever_was_open() {
        let mut sender = Sender::default();
        assert!(sender.end().is_empty());
        sender.edited("a", false, at(0));
        assert_eq!(states(&sender.end()), [("a", State::Active)]);
        assert!(sender.end().is_empty());
        sender.edited("b", false, at(1));
        sender.tick(at(7));
        assert_eq!(states(&sender.end()), [("b", State::Active)]);
    }

    #[test]
    fn leaving_says_active_and_reset_says_nothing() {
        let mut sender = Sender::default();
        sender.edited("room", false, at(0));
        assert_eq!(states(&sender.left("room")), [("room", State::Active)]);
        sender.edited("room", false, at(1));
        sender.reset();
        assert!(sender.tick(at(60)).is_empty());
    }

    fn message(
        kind: &str,
        from: &str,
        state: Option<WaddleChatState>,
        body: bool,
    ) -> WaddleMessage {
        let mut message = crate::session::tests::wire(from, kind, "hi");
        message.chat_state = state;
        if !body {
            message.body = None;
        }
        message
    }

    #[test]
    fn a_room_state_names_the_nick_and_ignores_our_own() {
        let heard_ann = heard(
            &message(
                "groupchat",
                "room@muc.example.com/ann",
                Some(WaddleChatState::Composing),
                false,
            ),
            "me@example.com",
            "me",
        )
        .unwrap();
        assert_eq!(heard_ann.conversation, "room@muc.example.com");
        assert_eq!(heard_ann.who, "ann");
        assert_eq!(heard_ann.state, Some(State::Composing));
        assert!(heard(
            &message(
                "groupchat",
                "room@muc.example.com/me",
                Some(WaddleChatState::Composing),
                false
            ),
            "me@example.com",
            "me",
        )
        .is_none());
    }

    #[test]
    fn a_dm_state_names_the_peer_and_ignores_our_other_devices() {
        let heard_bo = heard(
            &message(
                "chat",
                "bo@example.com/phone",
                Some(WaddleChatState::Paused),
                false,
            ),
            "me@example.com",
            "me",
        )
        .unwrap();
        assert_eq!(heard_bo.conversation, "bo@example.com");
        assert_eq!(heard_bo.state, Some(State::Paused));
        assert!(heard(
            &message(
                "chat",
                "me@example.com/laptop",
                Some(WaddleChatState::Composing),
                false
            ),
            "me@example.com",
            "me",
        )
        .is_none());
        // Room private messages are not a DM.
        let mut private = message(
            "chat",
            "room@muc.example.com/ann",
            Some(WaddleChatState::Composing),
            false,
        );
        private.muc_user = true;
        assert!(heard(&private, "me@example.com", "me").is_none());
    }

    #[test]
    fn a_real_message_ends_composing_and_a_bare_stanza_says_nothing() {
        let ended = heard(
            &message(
                "chat",
                "bo@example.com/phone",
                Some(WaddleChatState::Composing),
                true,
            ),
            "me@example.com",
            "me",
        )
        .unwrap();
        assert_eq!(ended.state, Some(State::Active));
        assert!(heard(
            &message("chat", "bo@example.com", None, false),
            "me@example.com",
            "me"
        )
        .is_none());
        let plain = heard(
            &message("chat", "bo@example.com", None, true),
            "me@example.com",
            "me",
        )
        .unwrap();
        assert_eq!(plain.state, Some(State::Active));
    }

    fn typing(who: &str) -> Heard {
        Heard {
            conversation: "room".into(),
            who: who.into(),
            state: Some(State::Composing),
        }
    }

    #[test]
    fn the_book_shows_composing_until_it_stops_or_expires() {
        let mut book = Book::default();
        assert!(book.apply(&typing("ann"), at(0)));
        assert!(
            !book.apply(&typing("ann"), at(10)),
            "a renewal changes nothing shown"
        );
        assert!(book.apply(&typing("bo"), at(11)));
        assert_eq!(book.names("room", str::to_string), ["ann", "bo"]);

        // Ann's renewal at 10 holds until 40; Bo's from 11 until 41.
        assert!(book.sweep(at(39)).is_empty());
        assert_eq!(book.sweep(at(40)), ["room"]);
        assert_eq!(book.names("room", str::to_string), ["bo"]);

        let stop = Heard {
            state: Some(State::Paused),
            ..typing("bo")
        };
        assert!(book.apply(&stop, at(41)));
        assert!(book.is_empty());
        assert!(!book.apply(&stop, at(42)));
    }

    #[test]
    fn expiry_reports_each_conversation_that_changed() {
        let mut book = Book::default();
        book.apply(&typing("ann"), at(0));
        book.apply(
            &Heard {
                conversation: "other".into(),
                ..typing("bo")
            },
            at(20),
        );
        assert_eq!(book.sweep(at(31)), ["room"]);
        assert!(!book.is_empty());
        book.clear();
        assert!(book.is_empty());
    }

    #[test]
    fn the_line_reads_naturally() {
        assert_eq!(line(&[]), "");
        assert_eq!(line(&["Ann".into()]), "Ann is typing…");
        assert_eq!(line(&["Ann".into(), "Bo".into()]), "Ann and Bo are typing…");
        assert_eq!(
            line(&["A".into(), "B".into(), "C".into()]),
            "Several people are typing…"
        );
    }
}
