use std::sync::atomic::{AtomicU64, Ordering};

const MESSAGE_ARCHIVE_ATTEMPTS_METRIC: &str = "waddle_message_archive_attempts_total";
const MESSAGE_ARCHIVE_CHAIN_INVALID_METRIC: &str = "waddle_message_archive_chain_invalid_total";

/// A closed, privacy-safe classification for logical message archive attempts.
/// No archive JID, room JID, sender, recipient, stanza ID, or message content is
/// ever used as a metric label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageArchiveKind {
    Direct,
    Room,
}

impl MessageArchiveKind {
    const ALL: [Self; 2] = [Self::Direct, Self::Room];

    const fn index(self) -> usize {
        match self {
            Self::Direct => 0,
            Self::Room => 1,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Direct => "dm",
            Self::Room => "room",
        }
    }
}

/// Terminal outcome of one typed sender-pass DM attempt or room archive
/// storage attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageArchiveOutcome {
    Committed,
    StorageError,
    OwnershipLost,
    ChainInvalid,
}

impl MessageArchiveOutcome {
    const ALL: [Self; 4] = [
        Self::Committed,
        Self::StorageError,
        Self::OwnershipLost,
        Self::ChainInvalid,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Committed => 0,
            Self::StorageError => 1,
            Self::OwnershipLost => 2,
            Self::ChainInvalid => 3,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::StorageError => "storage_error",
            Self::OwnershipLost => "ownership_lost",
            Self::ChainInvalid => "chain_invalid",
        }
    }
}

struct ArchiveAttempts {
    counters: [[AtomicU64; 4]; 2],
    chain_invalid: AtomicU64,
}

impl ArchiveAttempts {
    const fn new() -> Self {
        Self {
            counters: [
                [
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                ],
                [
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                ],
            ],
            chain_invalid: AtomicU64::new(0),
        }
    }

    fn increment(&self, kind: MessageArchiveKind, outcome: MessageArchiveOutcome) {
        self.counters[kind.index()][outcome.index()].fetch_add(1, Ordering::Relaxed);
        if outcome == MessageArchiveOutcome::ChainInvalid {
            self.chain_invalid.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> ([[u64; 4]; 2], u64) {
        let counters = std::array::from_fn(|kind| {
            std::array::from_fn(|outcome| self.counters[kind][outcome].load(Ordering::Relaxed))
        });
        (counters, self.chain_invalid.load(Ordering::Relaxed))
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn reset(&self) {
        for outcomes in &self.counters {
            for counter in outcomes {
                counter.store(0, Ordering::Release);
            }
        }
        self.chain_invalid.store(0, Ordering::Release);
    }
}

static ATTEMPTS: ArchiveAttempts = ArchiveAttempts::new();

pub fn increment_message_archive_attempt(kind: MessageArchiveKind, outcome: MessageArchiveOutcome) {
    ATTEMPTS.increment(kind, outcome);
}

pub(super) fn render(out: &mut String) {
    let (counts, chain_invalid) = ATTEMPTS.snapshot();
    render_counts(out, &counts, chain_invalid);
}

fn render_counts(out: &mut String, counts: &[[u64; 4]; 2], chain_invalid: u64) {
    out.push_str("# HELP ");
    out.push_str(MESSAGE_ARCHIVE_ATTEMPTS_METRIC);
    out.push_str(" Typed sender-pass DM attempts and room archive storage attempts by terminal outcome. Room attempts include server-authored room system messages; chain_invalid is a permanent room archive-chain invariant failure. A committed result means the message is durably present or an idempotent retry found the existing archive row; it is not a client-visibility signal.\n# TYPE ");
    out.push_str(MESSAGE_ARCHIVE_ATTEMPTS_METRIC);
    out.push_str(" counter\n");
    for kind in MessageArchiveKind::ALL {
        for outcome in MessageArchiveOutcome::ALL {
            out.push_str(MESSAGE_ARCHIVE_ATTEMPTS_METRIC);
            out.push_str("{kind=\"");
            out.push_str(kind.label());
            out.push_str("\",outcome=\"");
            out.push_str(outcome.label());
            out.push_str("\"} ");
            out.push_str(&counts[kind.index()][outcome.index()].to_string());
            out.push('\n');
        }
    }
    out.push_str("# HELP ");
    out.push_str(MESSAGE_ARCHIVE_CHAIN_INVALID_METRIC);
    out.push_str(" Permanent room archive-chain invariant failures: a groupchat archive event reached persistence without its room stanza-id or reflection from JID. Any non-zero value is a loss/corruption safety incident.\n# TYPE ");
    out.push_str(MESSAGE_ARCHIVE_CHAIN_INVALID_METRIC);
    out.push_str(" counter\n");
    out.push_str(MESSAGE_ARCHIVE_CHAIN_INVALID_METRIC);
    out.push(' ');
    out.push_str(&chain_invalid.to_string());
    out.push('\n');
}

#[cfg(any(test, feature = "test-utils"))]
pub(super) fn reset() {
    ATTEMPTS.reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_family_renders_only_closed_privacy_safe_labels() {
        let attempts = ArchiveAttempts::new();
        attempts.increment(MessageArchiveKind::Direct, MessageArchiveOutcome::Committed);
        attempts.increment(
            MessageArchiveKind::Room,
            MessageArchiveOutcome::StorageError,
        );
        attempts.increment(
            MessageArchiveKind::Room,
            MessageArchiveOutcome::OwnershipLost,
        );
        attempts.increment(
            MessageArchiveKind::Room,
            MessageArchiveOutcome::ChainInvalid,
        );

        let mut rendered = String::new();
        let (counts, chain_invalid) = attempts.snapshot();
        render_counts(&mut rendered, &counts, chain_invalid);

        assert!(rendered.contains("# TYPE waddle_message_archive_attempts_total counter"));
        assert!(rendered.contains(
            "waddle_message_archive_attempts_total{kind=\"dm\",outcome=\"committed\"} 1"
        ));
        assert!(rendered.contains(
            "waddle_message_archive_attempts_total{kind=\"room\",outcome=\"storage_error\"} 1"
        ));
        assert!(rendered.contains(
            "waddle_message_archive_attempts_total{kind=\"room\",outcome=\"ownership_lost\"} 1"
        ));
        assert!(rendered.contains(
            "waddle_message_archive_attempts_total{kind=\"room\",outcome=\"chain_invalid\"} 1"
        ));
        assert!(rendered.contains("waddle_message_archive_chain_invalid_total 1"));
        for forbidden in ["jid=", "room=", "user=", "message_id=", "session="] {
            assert!(!rendered.contains(forbidden), "forbidden label {forbidden}");
        }
    }
}
