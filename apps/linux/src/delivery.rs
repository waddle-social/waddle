//! The unsent queue and what each own message shows about its delivery.
//!
//! A message the user sends is written to disk before it is written to the
//! socket, and stays there until the server acknowledges it (XEP-0198), so
//! an app that is killed, or a connection that drops, loses nothing. It is
//! resent with the same XEP-0359 origin id, which the timeline and the
//! receiver already use to recognise one message, so a resend never shows
//! twice.
//!
//! The file holds what the user typed into the message and nothing else:
//! the body, who it is for, an optional quote and thread, and the mentions
//! the composer resolved. Uploaded files are not queued (their keys are not
//! written to disk); a failed file message can only be discarded.
//!
//! No GTK here. The window feeds events in and reads state out.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use waddle_client_session::Delivery;

use crate::composer::Mentionable;
use crate::session::{Draft, Kind, Quote};
use crate::threads::Thread;

/// Messages held at most. A queue that has grown past this is not a
/// connection problem; the oldest entries are dropped rather than the file.
const MAX_ENTRIES: usize = 200;

/// Where an unsent message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    /// Waiting for a connection (or a room join) to be written.
    Queued,
    /// Written; the server has not acknowledged it yet. After a restart or
    /// a dropped connection this is unconfirmed, and is sent again.
    Sending,
    /// The server refused it, or it could not be written while connected.
    /// Only the user resends it.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRecord {
    pub id: String,
    pub sender: String,
    pub own: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionRecord {
    pub nick: String,
    pub jid: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRecord {
    pub id: String,
    pub parent: Option<String>,
}

/// One message not yet acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The XEP-0359 origin id, kept across every resend.
    pub origin_id: String,
    /// The timeline the message belongs to (`Conversation::key`).
    pub key: String,
    /// The room or DM peer's bare JID.
    pub jid: String,
    pub direct: bool,
    pub thread: Option<ThreadRecord>,
    pub body: String,
    pub quote: Option<QuoteRecord>,
    pub mentions: Vec<MentionRecord>,
    pub created_ms: i64,
    pub state: EntryState,
    /// Writes tried so far.
    pub attempts: u32,
}

impl Pending {
    pub fn new(
        origin_id: String,
        key: String,
        jid: String,
        kind: Kind,
        draft: &Draft,
        created_ms: i64,
    ) -> Self {
        Self {
            origin_id,
            key,
            jid,
            direct: kind == Kind::Direct,
            thread: draft.thread.as_ref().map(|thread| ThreadRecord {
                id: thread.id.clone(),
                parent: thread.parent.clone(),
            }),
            body: draft.body.clone(),
            quote: draft.quote.as_ref().map(|quote| QuoteRecord {
                id: quote.id.clone(),
                sender: quote.sender.clone(),
                own: quote.own,
                text: quote.text.clone(),
            }),
            mentions: draft
                .mentionable
                .iter()
                .map(|mention| MentionRecord {
                    nick: mention.nick.clone(),
                    jid: mention.jid.clone(),
                    display_name: mention.display_name.clone(),
                })
                .collect(),
            created_ms,
            state: EntryState::Queued,
            attempts: 0,
        }
    }

    pub fn kind(&self) -> Kind {
        if self.direct {
            Kind::Direct
        } else {
            Kind::Room
        }
    }

    /// The message again, as the session sends it.
    pub fn draft(&self) -> Draft {
        Draft {
            body: self.body.clone(),
            origin_id: self.origin_id.clone(),
            quote: self.quote.as_ref().map(|quote| Quote {
                id: quote.id.clone(),
                sender: quote.sender.clone(),
                own: quote.own,
                text: quote.text.clone(),
            }),
            files: Vec::new(),
            mentionable: self
                .mentions
                .iter()
                .map(|mention| Mentionable {
                    nick: mention.nick.clone(),
                    jid: mention.jid.clone(),
                    display_name: mention.display_name.clone(),
                })
                .collect(),
            thread: self.thread.as_ref().map(|thread| Thread {
                id: thread.id.clone(),
                parent: thread.parent.clone(),
            }),
            sticker: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    entries: Vec<Pending>,
}

const FORMAT_VERSION: u32 = 1;

/// The queue of one account, backed by one file.
#[derive(Debug, Default)]
pub struct Outbox {
    entries: Vec<Pending>,
    path: Option<PathBuf>,
}

impl Outbox {
    /// A queue that is never written (tests, or no state directory).
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Read the account's file. A file that is missing is an empty queue;
    /// one that cannot be read as a queue is moved aside (so it can be
    /// looked at) and the queue starts empty. Anything that was written
    /// and not acknowledged is unconfirmed: it is queued to be sent again.
    pub fn load(path: PathBuf) -> Self {
        let entries = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<FileFormat>(&bytes) {
                Ok(file) if file.version == FORMAT_VERSION => file.entries,
                _ => {
                    tracing::warn!(path = %path.display(), "unsent queue is unreadable; moved aside");
                    let mut aside = path.clone().into_os_string();
                    aside.push(".corrupt");
                    let _ = fs::rename(&path, PathBuf::from(aside));
                    Vec::new()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "could not read the unsent queue");
                Vec::new()
            }
        };
        let mut outbox = Self {
            entries,
            path: Some(path),
        };
        outbox.connection_lost();
        outbox
    }

    pub fn entries(&self) -> &[Pending] {
        &self.entries
    }

    pub fn get(&self, origin_id: &str) -> Option<&Pending> {
        self.entries
            .iter()
            .find(|entry| entry.origin_id == origin_id)
    }

    pub fn state(&self, origin_id: &str) -> Option<EntryState> {
        self.get(origin_id).map(|entry| entry.state)
    }

    /// Hold a new message. It goes to disk now, before anything is sent.
    pub fn enqueue(&mut self, entry: Pending) {
        if self.get(&entry.origin_id).is_some() {
            return;
        }
        self.entries.push(entry);
        while self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.save();
    }

    /// The stanza was written to the socket.
    pub fn written(&mut self, origin_id: &str) {
        if self.change(origin_id, |entry| {
            entry.state = EntryState::Sending;
            entry.attempts = entry.attempts.saturating_add(1);
        }) {
            self.save();
        }
    }

    /// The stanza could not be written. While there is no connection it
    /// waits; with one, something is wrong with the message and the user
    /// decides.
    pub fn not_written(&mut self, origin_id: &str, connected: bool) {
        let state = if connected {
            EntryState::Failed
        } else {
            EntryState::Queued
        };
        if self.change(origin_id, |entry| entry.state = state) {
            self.save();
        }
    }

    /// The server acknowledged it (XEP-0198): it is no longer ours to
    /// keep. True when it was held.
    pub fn acknowledged(&mut self, origin_id: &str) -> bool {
        self.remove(origin_id)
    }

    /// The transport or the server refused it.
    pub fn failed(&mut self, origin_id: &str) {
        if self.change(origin_id, |entry| entry.state = EntryState::Failed) {
            self.save();
        }
    }

    /// The user chose Retry: it goes out again with the same origin id.
    pub fn retry(&mut self, origin_id: &str) -> bool {
        let found = self.change(origin_id, |entry| entry.state = EntryState::Queued);
        if found {
            self.save();
        }
        found
    }

    /// The user chose Discard (or a conversation went away).
    pub fn discard(&mut self, origin_id: &str) -> bool {
        self.remove(origin_id)
    }

    /// The connection dropped: what was written and not acknowledged is
    /// unconfirmed, so it waits to be sent again.
    pub fn connection_lost(&mut self) {
        let mut changed = false;
        for entry in &mut self.entries {
            if entry.state == EntryState::Sending {
                entry.state = EntryState::Queued;
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    /// What to write next, oldest first.
    pub fn due(&self) -> Vec<Pending> {
        let mut due: Vec<Pending> = self
            .entries
            .iter()
            .filter(|entry| entry.state == EntryState::Queued)
            .cloned()
            .collect();
        due.sort_by_key(|entry| entry.created_ms);
        due
    }

    /// The messages of one timeline, oldest first.
    pub fn for_timeline(&self, key: &str) -> Vec<Pending> {
        let mut found: Vec<Pending> = self
            .entries
            .iter()
            .filter(|entry| entry.key == key)
            .cloned()
            .collect();
        found.sort_by_key(|entry| entry.created_ms);
        found
    }

    fn change(&mut self, origin_id: &str, edit: impl FnOnce(&mut Pending)) -> bool {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.origin_id == origin_id)
        {
            Some(entry) => {
                edit(entry);
                true
            }
            None => false,
        }
    }

    fn remove(&mut self, origin_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.origin_id != origin_id);
        let removed = self.entries.len() != before;
        if removed {
            self.save();
        }
        removed
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = FileFormat {
            version: FORMAT_VERSION,
            entries: self.entries.clone(),
        };
        let result = serde_json::to_vec(&file)
            .map_err(io::Error::other)
            .and_then(|bytes| write_atomically(path, &bytes));
        if let Err(error) = result {
            tracing::warn!(%error, "could not save the unsent queue");
        }
    }
}

/// Write to a sibling file, flush it to disk, then rename over the target,
/// so a crash leaves the old file or the new one and never half of one.
/// The file is private to the user: it holds message text.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

/// `$XDG_STATE_HOME/waddle`, or `~/.local/state/waddle`.
pub fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })?;
    Some(base.join("waddle"))
}

/// The queue file of the account `jid` (a file name safe for any JID).
pub fn outbox_path(dir: &Path, jid: &str) -> PathBuf {
    let name: String = jid
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("outbox-{name}.json"))
}

/// What an own message shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// Waiting for a connection.
    Queued,
    /// Written, not yet acknowledged.
    Sending,
    /// The server has it (XEP-0198).
    Sent,
    /// The other side has read it (XEP-0333 displayed).
    Read,
    /// Not sent; Retry or Discard.
    Failed,
}

impl Shown {
    /// The note after the message text, for the states that carry one.
    /// `latest` is whether this is the newest own message: Sent and Read
    /// are only worth a line there, not under every message.
    pub fn note(self, latest: bool) -> Option<&'static str> {
        match self {
            Self::Queued => Some("· Waiting to send"),
            Self::Sending => Some("· Sending…"),
            Self::Failed => Some("· Not sent"),
            Self::Sent if latest => Some("· Sent"),
            Self::Read if latest => Some("· Read"),
            Self::Sent | Self::Read => None,
        }
    }

    pub fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Sending)
    }
}

/// Combine the timeline's delivery, the queue's entry for the message and
/// whether the other side has displayed it.
pub fn shown(delivery: Delivery, entry: Option<EntryState>, read: bool) -> Shown {
    match (delivery, entry) {
        (Delivery::Failed, _) | (_, Some(EntryState::Failed)) => Shown::Failed,
        (Delivery::Sent, _) if read => Shown::Read,
        (Delivery::Sent, _) => Shown::Sent,
        (_, Some(EntryState::Queued)) => Shown::Queued,
        (Delivery::Sending, _) | (_, Some(EntryState::Sending)) => Shown::Sending,
        _ if read => Shown::Read,
        _ => Shown::Sent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(body: &str) -> Draft {
        Draft {
            body: body.into(),
            origin_id: String::new(),
            quote: Some(Quote {
                id: "q1".into(),
                sender: "Ann".into(),
                own: false,
                text: "hi".into(),
            }),
            files: Vec::new(),
            mentionable: vec![Mentionable {
                nick: "ann".into(),
                jid: "ann@example.com".into(),
                display_name: None,
            }],
            thread: Some(Thread {
                id: "t1".into(),
                parent: None,
            }),
            sticker: None,
        }
    }

    fn pending(origin: &str, at: i64) -> Pending {
        Pending::new(
            origin.into(),
            "room@example.com".into(),
            "room@example.com".into(),
            Kind::Room,
            &draft("hello"),
            at,
        )
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "waddle-delivery-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("outbox.json")
    }

    #[test]
    fn a_message_walks_queued_sending_acknowledged() {
        let mut outbox = Outbox::in_memory();
        outbox.enqueue(pending("o1", 1));
        assert_eq!(outbox.state("o1"), Some(EntryState::Queued));
        outbox.written("o1");
        assert_eq!(outbox.state("o1"), Some(EntryState::Sending));
        assert_eq!(outbox.get("o1").unwrap().attempts, 1);
        assert!(outbox.acknowledged("o1"));
        assert_eq!(outbox.state("o1"), None);
        assert!(!outbox.acknowledged("o1"));
    }

    #[test]
    fn a_write_that_fails_waits_offline_and_fails_online() {
        let mut outbox = Outbox::in_memory();
        outbox.enqueue(pending("o1", 1));
        outbox.enqueue(pending("o2", 2));
        outbox.not_written("o1", false);
        outbox.not_written("o2", true);
        assert_eq!(outbox.state("o1"), Some(EntryState::Queued));
        assert_eq!(outbox.state("o2"), Some(EntryState::Failed));
        // Only the queued one is due; the failed one waits for the user.
        let due: Vec<_> = outbox.due().into_iter().map(|e| e.origin_id).collect();
        assert_eq!(due, ["o1"]);
        assert!(outbox.retry("o2"));
        assert_eq!(outbox.due().len(), 2);
    }

    #[test]
    fn a_dropped_connection_requeues_what_was_unconfirmed_in_order() {
        let mut outbox = Outbox::in_memory();
        outbox.enqueue(pending("late", 9));
        outbox.enqueue(pending("early", 3));
        outbox.written("late");
        outbox.written("early");
        outbox.failed("late");
        outbox.connection_lost();
        assert_eq!(outbox.state("late"), Some(EntryState::Failed));
        assert_eq!(outbox.state("early"), Some(EntryState::Queued));
        outbox.retry("late");
        let due: Vec<_> = outbox.due().into_iter().map(|e| e.origin_id).collect();
        assert_eq!(due, ["early", "late"]);
    }

    #[test]
    fn enqueueing_the_same_origin_id_twice_keeps_one() {
        let mut outbox = Outbox::in_memory();
        outbox.enqueue(pending("o1", 1));
        outbox.enqueue(pending("o1", 2));
        assert_eq!(outbox.entries().len(), 1);
        assert_eq!(outbox.get("o1").unwrap().created_ms, 1);
    }

    #[test]
    fn discard_removes_and_the_queue_is_bounded() {
        let mut outbox = Outbox::in_memory();
        for at in 0..(MAX_ENTRIES + 5) {
            outbox.enqueue(pending(&format!("o{at}"), at as i64));
        }
        assert_eq!(outbox.entries().len(), MAX_ENTRIES);
        assert!(outbox.get("o0").is_none());
        assert!(outbox.discard("o10"));
        assert!(!outbox.discard("o10"));
    }

    #[test]
    fn the_file_round_trips_and_resends_what_was_unconfirmed() {
        let path = scratch("round-trip");
        let mut outbox = Outbox::load(path.clone());
        outbox.enqueue(pending("o1", 1));
        outbox.enqueue(pending("o2", 2));
        outbox.written("o2");
        outbox.not_written("o1", true);
        drop(outbox);

        let loaded = Outbox::load(path.clone());
        assert_eq!(loaded.entries().len(), 2);
        // Failed stays for the user; the unconfirmed write is queued again,
        // with the same origin id and everything the user typed.
        assert_eq!(loaded.state("o1"), Some(EntryState::Failed));
        assert_eq!(loaded.state("o2"), Some(EntryState::Queued));
        let again = loaded.get("o2").unwrap().draft();
        assert_eq!(again.origin_id, "o2");
        assert_eq!(again.body, "hello");
        assert_eq!(again.quote.unwrap().id, "q1");
        assert_eq!(again.thread.unwrap().id, "t1");
        assert_eq!(again.mentionable[0].jid, "ann@example.com");
        assert_eq!(again.files.len(), 0);

        let mut loaded = loaded;
        assert!(loaded.acknowledged("o2"));
        let reloaded = Outbox::load(path);
        assert_eq!(reloaded.entries().len(), 1);
    }

    #[test]
    fn a_missing_file_is_an_empty_queue() {
        let outbox = Outbox::load(scratch("missing"));
        assert!(outbox.entries().is_empty());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_the_queue_starts_empty() {
        for (name, content) in [
            ("garbage", b"not json".as_slice()),
            (
                "truncated",
                br#"{"version":1,"entries":[{"origin"#.as_slice(),
            ),
            ("future", br#"{"version":99,"entries":[]}"#.as_slice()),
        ] {
            let path = scratch(name);
            fs::write(&path, content).unwrap();
            let mut outbox = Outbox::load(path.clone());
            assert!(outbox.entries().is_empty(), "{name}");
            let mut aside = path.clone().into_os_string();
            aside.push(".corrupt");
            assert!(PathBuf::from(aside).exists(), "{name}");
            // And it is usable: the next save writes a good file.
            outbox.enqueue(pending("o1", 1));
            assert_eq!(Outbox::load(path).entries().len(), 1, "{name}");
        }
    }

    #[test]
    fn writes_are_atomic_and_private() {
        let path = scratch("atomic");
        write_atomically(&path, b"one").unwrap();
        write_atomically(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let mut temporary = path.clone().into_os_string();
        temporary.push(".tmp");
        assert!(!PathBuf::from(temporary).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    #[test]
    fn the_file_name_is_safe_for_any_jid() {
        let path = outbox_path(Path::new("/state"), "a/b c@example.com");
        assert_eq!(path, Path::new("/state/outbox-a_b_c@example.com.json"));
    }

    #[test]
    fn shown_states_follow_the_queue_then_the_timeline() {
        use Delivery::{Failed, Sending, Sent};
        assert_eq!(
            shown(Sending, Some(EntryState::Queued), false),
            Shown::Queued
        );
        assert_eq!(
            shown(Sending, Some(EntryState::Sending), false),
            Shown::Sending
        );
        assert_eq!(shown(Sending, None, false), Shown::Sending);
        assert_eq!(shown(Sent, None, false), Shown::Sent);
        assert_eq!(shown(Sent, None, true), Shown::Read);
        assert_eq!(shown(Failed, None, true), Shown::Failed);
        assert_eq!(
            shown(Sending, Some(EntryState::Failed), false),
            Shown::Failed
        );
        // An ack outranks a stale queue entry.
        assert_eq!(shown(Sent, Some(EntryState::Queued), false), Shown::Sent);
    }

    #[test]
    fn notes_mark_the_unsettled_states_and_the_latest_settled_one() {
        assert_eq!(Shown::Queued.note(false), Some("· Waiting to send"));
        assert_eq!(Shown::Failed.note(false), Some("· Not sent"));
        assert_eq!(Shown::Sent.note(false), None);
        assert_eq!(Shown::Sent.note(true), Some("· Sent"));
        assert_eq!(Shown::Read.note(true), Some("· Read"));
        assert!(Shown::Sending.is_pending());
        assert!(!Shown::Failed.is_pending());
    }
}
