//! Deterministic fault seams for the two-node remote-room acceptance suite
//! (`tests/xep0045_xep0198_remote_acceptance.rs`, #1948).
//!
//! Compiled only with the `test-support` feature, which release images never
//! enable, and inert unless the spawned server's environment names a control
//! directory. The harness strips every inherited `WADDLE_*` variable, so only
//! a suite that passes the directory to one spawned process arms that process.
//!
//! The seams sit on the room owner's side of a relayed XEP-0045 groupchat
//! message, around the transaction in which the owner accepts it, and of a
//! room request (join, presence update, leave or relayed IQ), around the
//! transaction in which the owner decides it:
//!
//! - `hold-before-commit`: while this file exists, the owner waits before its
//!   ingress commit, so nothing about the message is durable on the owner.
//! - `hold-after-commit`: while this file exists, the owner waits after its
//!   ingress commit and before it executes effects or builds its relay reply.
//! - `drop-reply`: consumed by the next acknowledged groupchat relay reply,
//!   which is then never sent. The owner has recorded the delivery as
//!   complete; the origin sees only its own ask fail.
//!
//! A suite arms a seam by creating the file in the control directory and
//! releases a hold by removing it. Every engagement appends one
//! `<event> <point> <stanza id>` line to `events` in the same directory, so a
//! test fails when a seam never engages instead of silently observing the
//! seam-free interleaving. None of the seams changes what the owner does once
//! it resumes.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use xmpp_parsers::message::Message;

use super::ordered_relay::{
    OrderedRelayMucProxyKind, OrderedRelayPayload, OrderedRelayReply, RemoteStanzaEnvelope,
};

/// Directory holding the arm files and the `events` log for this process.
const DIRECTORY_ENV: &str = "WADDLE_TEST_SEAM_REMOTE_ACCEPTANCE_DIRECTORY";
const EVENTS_FILE: &str = "events";
const DROP_REPLY_FILE: &str = "drop-reply";

/// Poll interval while a hold waits for its arm file to be removed. The
/// file, not the interval, is the synchronisation.
const HOLD_POLL_INTERVAL: Duration = Duration::from_millis(2);

static DIRECTORY: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var_os(DIRECTORY_ENV).map(PathBuf::from));

/// Where the owner's acceptance of a relayed groupchat message can be held.
#[derive(Clone, Copy, Debug)]
pub(crate) enum OwnerAcceptancePoint {
    /// Before the owner's ingress commit.
    BeforeCommit,
    /// After the owner's ingress commit, before effects and the relay reply.
    AfterCommit,
}

impl OwnerAcceptancePoint {
    fn label(self) -> &'static str {
        match self {
            Self::BeforeCommit => "before-commit",
            Self::AfterCommit => "after-commit",
        }
    }

    fn arm_file(self) -> &'static str {
        match self {
            Self::BeforeCommit => "hold-before-commit",
            Self::AfterCommit => "hold-after-commit",
        }
    }
}

fn record(directory: &std::path::Path, event: &str, point: &str, stanza_id: Option<&str>) {
    let line = format!("{event} {point} {}\n", stanza_id.unwrap_or("-"));
    let path = directory.join(EVENTS_FILE);
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(error) = written {
        tracing::error!(%error, path = %path.display(), "acceptance seam event not recorded");
    }
}

/// Records `abandoned` when a held delivery is cancelled before its release,
/// which is what the receiver's own delivery budget does to a long hold.
struct HoldGuard<'a> {
    directory: &'a std::path::Path,
    point: OwnerAcceptancePoint,
    stanza_id: Option<&'a str>,
    released: bool,
}

impl Drop for HoldGuard<'_> {
    fn drop(&mut self) {
        let event = if self.released {
            "released"
        } else {
            "abandoned"
        };
        record(self.directory, event, self.point.label(), self.stanza_id);
    }
}

/// Wait at `point` for as long as its arm file exists.
///
/// Not bounded: the enclosing receiver delivery budget (six seconds for the
/// claim check, plan and commit together) cancels a hold that outlasts it,
/// exactly as it cancels a slow commit.
pub(crate) async fn hold_owner_acceptance(point: OwnerAcceptancePoint, message: &Message) {
    hold_owner(point, message.id.as_ref().map(|id| id.0.as_str())).await;
}

/// Hold the room owner around the transaction in which it decides a room
/// request, identified in the event log by the request stanza's `id`.
pub(crate) async fn hold_owner_request(point: OwnerAcceptancePoint, request: &waddle_xmpp::Stanza) {
    let stanza_id = match request {
        waddle_xmpp::Stanza::Presence(presence) => presence.id.as_deref(),
        waddle_xmpp::Stanza::Iq(iq) => Some(iq.id()),
        waddle_xmpp::Stanza::Message(message) => message.id.as_ref().map(|id| id.0.as_str()),
    };
    hold_owner(point, stanza_id).await;
}

async fn hold_owner(point: OwnerAcceptancePoint, stanza_id: Option<&str>) {
    let Some(directory) = DIRECTORY.as_deref() else {
        return;
    };
    let arm_file = directory.join(point.arm_file());
    if !arm_file.exists() {
        return;
    }
    record(directory, "held", point.label(), stanza_id);
    let mut guard = HoldGuard {
        directory,
        point,
        stanza_id,
        released: false,
    };
    while arm_file.exists() {
        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
    }
    guard.released = true;
}

/// The relayed groupchat message in `envelope`, if that is what it carries.
pub(crate) fn relayed_groupchat(envelope: &RemoteStanzaEnvelope) -> Option<Message> {
    DIRECTORY.as_ref()?;
    match &envelope.payload {
        OrderedRelayPayload::MucProxy {
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            stanza,
            ..
        } => match &stanza.0 {
            waddle_xmpp::Stanza::Message(message) => Some(message.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Withhold an acknowledged groupchat relay reply forever when `drop-reply`
/// is armed, consuming the arm file so the origin's next attempt is answered.
///
/// The receiver state already holds the completed reply and its receipts, so
/// the owner is in the state a reply lost on the wire leaves it in.
pub(crate) async fn drop_owner_reply(groupchat: Option<&Message>, reply: &OrderedRelayReply) {
    let Some(directory) = DIRECTORY.as_deref() else {
        return;
    };
    let (Some(message), OrderedRelayReply::Ack(_)) = (groupchat, reply) else {
        return;
    };
    // Removing the arm file is the claim: concurrent replies drop only one.
    if std::fs::remove_file(directory.join(DROP_REPLY_FILE)).is_err() {
        return;
    }
    record(
        directory,
        "dropped",
        "reply",
        message.id.as_ref().map(|id| id.0.as_str()),
    );
    std::future::pending::<()>().await;
}
