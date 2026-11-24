//! A conversation's pinned messages (`urn:waddle:pin:0`).
//!
//! The list is fetched when a conversation opens and kept current by the
//! `<pin-event/>` broadcasts. Events that arrive while the fetch is in
//! flight are held and replayed on top of it, so a pin made at that moment
//! is not lost.

use waddle_client_session::TimelineItem;

/// One pinned message, with the preview frozen when it was pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The pinned message's archive stanza id.
    pub target: String,
    /// Who pinned it, as a bare JID.
    pub pinner: String,
    /// When the author wrote it (RFC 3339).
    pub message_at: String,
    pub author_nick: Option<String>,
    pub author_jid: String,
    pub text: String,
}

impl Pin {
    /// Whom to name for the author.
    pub fn author(&self) -> &str {
        self.author_nick
            .as_deref()
            .filter(|nick| !nick.is_empty())
            .unwrap_or_else(|| {
                self.author_jid
                    .split('@')
                    .next()
                    .unwrap_or(&self.author_jid)
            })
    }
}

/// A pin or unpin broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinChange {
    /// With the preview, when the broadcast carried one.
    Pinned {
        target: String,
        pin: Option<Pin>,
    },
    Unpinned {
        target: String,
    },
}

#[derive(Debug, Default)]
pub struct PinBook {
    /// Newest pin first, as the server lists them.
    entries: Vec<Pin>,
    hydrated: bool,
    held: Vec<PinChange>,
}

impl PinBook {
    pub fn entries(&self) -> &[Pin] {
        &self.entries
    }

    pub fn hydrated(&self) -> bool {
        self.hydrated
    }

    /// Whether the message with this archive id is pinned.
    pub fn contains(&self, target: &str) -> bool {
        self.entries.iter().any(|pin| pin.target == target)
    }

    /// Whether the row is pinned.
    pub fn is_pinned(&self, item: &TimelineItem) -> bool {
        crate::message_actions::pin_target(item).is_some_and(|target| self.contains(target))
    }

    /// Take the server's list, then replay what arrived meanwhile. True
    /// when a replayed pin had no preview and the list should be fetched
    /// again.
    pub fn hydrate(&mut self, entries: Vec<Pin>) -> bool {
        self.entries = entries;
        self.hydrated = true;
        let mut refetch = false;
        for change in std::mem::take(&mut self.held) {
            refetch |= self.apply(change);
        }
        refetch
    }

    /// Apply a broadcast. True when the list should be fetched again,
    /// because a pin arrived without its preview.
    pub fn apply(&mut self, change: PinChange) -> bool {
        if !self.hydrated {
            self.held.push(change);
            return false;
        }
        match change {
            PinChange::Pinned { target, pin } => {
                self.entries.retain(|held| held.target != target);
                match pin {
                    Some(pin) => {
                        self.entries.insert(0, pin);
                        false
                    }
                    None => true,
                }
            }
            PinChange::Unpinned { target } => {
                self.entries.retain(|held| held.target != target);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(target: &str) -> Pin {
        Pin {
            target: target.into(),
            pinner: "admin@waddle.test".into(),
            message_at: "2026-05-08T11:55:00+00:00".into(),
            author_nick: Some("ann".into()),
            author_jid: "ann@waddle.test".into(),
            text: format!("text of {target}"),
        }
    }

    fn pinned(target: &str) -> PinChange {
        PinChange::Pinned {
            target: target.into(),
            pin: Some(pin(target)),
        }
    }

    fn unpinned(target: &str) -> PinChange {
        PinChange::Unpinned {
            target: target.into(),
        }
    }

    fn targets(book: &PinBook) -> Vec<&str> {
        book.entries().iter().map(|p| p.target.as_str()).collect()
    }

    #[test]
    fn a_new_pin_goes_first_and_a_repin_does_not_duplicate() {
        let mut book = PinBook::default();
        book.hydrate(vec![pin("a"), pin("b")]);
        book.apply(pinned("c"));
        assert_eq!(targets(&book), ["c", "a", "b"]);
        book.apply(pinned("a"));
        assert_eq!(targets(&book), ["a", "c", "b"]);
    }

    #[test]
    fn an_unpin_removes_the_entry() {
        let mut book = PinBook::default();
        book.hydrate(vec![pin("a"), pin("b")]);
        book.apply(unpinned("a"));
        assert_eq!(targets(&book), ["b"]);
        assert!(!book.contains("a"));
        assert!(book.contains("b"));
    }

    #[test]
    fn events_during_the_fetch_replay_on_top_of_it() {
        let mut book = PinBook::default();
        assert!(!book.apply(pinned("new")));
        assert!(!book.apply(unpinned("old")));
        assert!(book.entries().is_empty());
        assert!(!book.hydrated());
        book.hydrate(vec![pin("old"), pin("keep")]);
        assert_eq!(targets(&book), ["new", "keep"]);
    }

    #[test]
    fn a_pin_without_a_preview_asks_for_a_refetch() {
        let mut book = PinBook::default();
        book.hydrate(vec![pin("a")]);
        assert!(book.apply(PinChange::Pinned {
            target: "b".into(),
            pin: None
        }));
        assert_eq!(targets(&book), ["a"]);
        // The same during hydration surfaces when it replays.
        let mut early = PinBook::default();
        early.apply(PinChange::Pinned {
            target: "b".into(),
            pin: None,
        });
        assert!(early.hydrate(vec![]));
    }

    #[test]
    fn authors_fall_back_to_the_jid_local_part() {
        let mut anonymous = pin("a");
        anonymous.author_nick = None;
        assert_eq!(anonymous.author(), "ann");
        assert_eq!(pin("a").author(), "ann");
    }
}
