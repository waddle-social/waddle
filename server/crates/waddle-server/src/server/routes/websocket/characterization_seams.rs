//! Deterministic seams for the W1.6c characterization suite
//! (`tests/characterization_resume_muc.rs`, #2046).
//!
//! Compiled only with the `test-support` feature, which release images never
//! enable, and inert unless the spawned test server's environment names a seam.
//! The harness strips every inherited `WADDLE_*` variable, so only a suite that
//! passes one explicitly through `TestServer::start_*_with_extra_envs` arms it.
//! Each seam reproduces one interleaving or fault that the suite cannot force
//! over the wire; none changes what the production code does once it resumes.
//!
//! The hold seam records what it did in an events file the suite names, so a
//! test fails when the hold never engages (a missing registry entry, an
//! unbound stream, or a call site that a socket-loop refactor moved or
//! dropped) instead of silently degrading to the seam-free interleaving.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use jid::{BareJid, FullJid};

use super::WebSocketState;
use crate::ingress::append_authority::AppendAuthorityRejection;

/// Which response batch to hold: `sm-resumed` or `muc-join`.
const HOLD_BATCH_KIND_ENV: &str = "WADDLE_TEST_SEAM_HOLD_BATCH_KIND";
/// Bare JID of the account whose batches are held; other accounts are never held.
const HOLD_BATCH_ACCOUNT_ENV: &str = "WADDLE_TEST_SEAM_HOLD_BATCH_ACCOUNT";
/// File the hold seam appends one `<event> <kind> <jid>` line to per event.
/// Required whenever the hold seam is armed.
const EVENTS_FILE_ENV: &str = "WADDLE_TEST_SEAM_EVENTS_FILE";
/// `unavailable` fails every canonical read a detach drain makes.
const DETACH_CANONICAL_READ_ENV: &str = "WADDLE_TEST_SEAM_DETACH_CANONICAL_READ";
const FAILED_RESUME_ACCOUNT_ENV: &str = "WADDLE_TEST_SEAM_FAILED_RESUME_ACCOUNT";
const FAILED_RESUME_JID_ENV: &str = "WADDLE_TEST_SEAM_FAILED_RESUME_JID";
const FAILED_RESUME_ARM_FILE_ENV: &str = "WADDLE_TEST_SEAM_FAILED_RESUME_ARM_FILE";
const FAILED_RESUME_CONTINUE_FILE_ENV: &str = "WADDLE_TEST_SEAM_FAILED_RESUME_CONTINUE_FILE";
const FAILED_RESUME_CLEANUP_RELEASE_FILE_ENV: &str =
    "WADDLE_TEST_SEAM_FAILED_RESUME_CLEANUP_RELEASE_FILE";
const FAILED_RESUME_EVENTS_FILE_ENV: &str = "WADDLE_TEST_SEAM_FAILED_RESUME_EVENTS_FILE";
/// File the teardown observer appends one line to for every custody path a
/// connection's teardown takes for its stream-management session:
/// `durable-end <session> terminal=<bool>` when the durable authority was
/// asked to close the session, and `connection-promotion <jid>` when the
/// connection starts the promotion of its own queue. `teardown-finished`
/// follows once a connection's teardown has returned.
const TEARDOWN_CUSTODY_EVENTS_FILE_ENV: &str = "WADDLE_TEST_SEAM_TEARDOWN_CUSTODY_EVENTS_FILE";

/// Poll interval while a held batch waits for its release condition. The
/// condition, not the interval, is the synchronisation.
const HOLD_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// A handler response batch the connection writes inline, before it next
/// drains its outbound channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum HeldBatch {
    /// `<resumed/>` and its XEP-0198 replay, after the resumed stream is
    /// registered (`SmRegistrationFinalization::ReplaceWithResumed`).
    SmResumed,
    /// The XEP-0045 §7.1 join burst (§7.2.2 presence broadcast, §7.2.15
    /// subject), after the room admitted the occupant and its join presence
    /// went to the existing occupants. `handle_muc_join_unlocked` is also
    /// reached by server-originated joins (LiveKit webhook, Muji clear) and
    /// lib unit tests; arming this kind for an account those paths join
    /// would hold that task too.
    MucJoin,
}

impl HeldBatch {
    fn label(self) -> &'static str {
        match self {
            Self::SmResumed => "sm-resumed",
            Self::MucJoin => "muc-join",
        }
    }
}

/// One line in the events file.
#[derive(Clone, Copy)]
enum HoldEvent {
    /// Armed and matched: the batch is now held.
    Held,
    /// A stanza was queued on the connection; the batch is written next.
    Released,
    /// The connection's channel closed while the batch was held.
    Abandoned,
    /// Armed for this account and kind, but the batch could not be held.
    NotHeld,
}

impl HoldEvent {
    fn label(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Released => "released",
            Self::Abandoned => "abandoned",
            Self::NotHeld => "not-held",
        }
    }
}

struct HoldBatch {
    kind: HeldBatch,
    account: BareJid,
    events: PathBuf,
}

static HOLD_BATCH: LazyLock<Option<HoldBatch>> = LazyLock::new(|| {
    let kind = match std::env::var(HOLD_BATCH_KIND_ENV).ok()?.as_str() {
        "sm-resumed" => HeldBatch::SmResumed,
        "muc-join" => HeldBatch::MucJoin,
        other => {
            tracing::error!(
                value = other,
                "unknown {HOLD_BATCH_KIND_ENV}; seam disarmed"
            );
            return None;
        }
    };
    let Some(events) = std::env::var_os(EVENTS_FILE_ENV).map(PathBuf::from) else {
        tracing::error!("{HOLD_BATCH_KIND_ENV} without {EVENTS_FILE_ENV}; seam disarmed");
        return None;
    };
    let account = std::env::var(HOLD_BATCH_ACCOUNT_ENV).ok()?;
    match account.parse::<BareJid>() {
        Ok(account) => Some(HoldBatch {
            kind,
            account,
            events,
        }),
        Err(error) => {
            tracing::error!(%error, "invalid {HOLD_BATCH_ACCOUNT_ENV}; seam disarmed");
            None
        }
    }
});

static DETACH_CANONICAL_READ_OUTAGE: LazyLock<bool> =
    LazyLock::new(
        || match std::env::var(DETACH_CANONICAL_READ_ENV).ok().as_deref() {
            None => false,
            Some("unavailable") => true,
            Some(other) => {
                tracing::error!(
                    value = other,
                    "unknown {DETACH_CANONICAL_READ_ENV}; seam disarmed"
                );
                false
            }
        },
    );

/// Per-server file rendezvous for the same-socket failed-resume regression.
/// `TestServer::start_with_failed_resume_seam` creates unique paths and passes
/// them only to that spawned process; matching also requires the exact full
/// JID and armed stream id.
#[derive(Clone)]
struct FailedResumeSeam {
    account: BareJid,
    jid: FullJid,
    arm_file: PathBuf,
    continue_file: PathBuf,
    cleanup_release_file: PathBuf,
    events: PathBuf,
}

static FAILED_RESUME_SEAM: LazyLock<Option<FailedResumeSeam>> = LazyLock::new(|| {
    let account = std::env::var(FAILED_RESUME_ACCOUNT_ENV).ok()?;
    let jid = std::env::var(FAILED_RESUME_JID_ENV).ok()?;
    let arm_file = std::env::var_os(FAILED_RESUME_ARM_FILE_ENV).map(PathBuf::from)?;
    let continue_file = std::env::var_os(FAILED_RESUME_CONTINUE_FILE_ENV).map(PathBuf::from)?;
    let cleanup_release_file =
        std::env::var_os(FAILED_RESUME_CLEANUP_RELEASE_FILE_ENV).map(PathBuf::from)?;
    let events = std::env::var_os(FAILED_RESUME_EVENTS_FILE_ENV).map(PathBuf::from)?;
    let account = match account.parse::<BareJid>() {
        Ok(account) => account,
        Err(error) => {
            tracing::error!(%error, "invalid {FAILED_RESUME_ACCOUNT_ENV}; seam disarmed");
            return None;
        }
    };
    let jid = match jid.parse::<FullJid>() {
        Ok(jid) if jid.to_bare() == account => jid,
        Ok(_) => {
            tracing::error!("{FAILED_RESUME_JID_ENV} does not belong to {FAILED_RESUME_ACCOUNT_ENV}; seam disarmed");
            return None;
        }
        Err(error) => {
            tracing::error!(%error, "invalid {FAILED_RESUME_JID_ENV}; seam disarmed");
            return None;
        }
    };
    Some(FailedResumeSeam {
        account,
        jid,
        arm_file,
        continue_file,
        cleanup_release_file,
        events,
    })
});

impl FailedResumeSeam {
    fn armed_stream(&self) -> Option<String> {
        std::fs::read_to_string(&self.arm_file)
            .ok()
            .and_then(|armed| armed.lines().next().map(str::trim).map(str::to_owned))
            .filter(|stream_id| !stream_id.is_empty())
    }

    fn armed_mode(&self, expected: &str) -> bool {
        std::fs::read_to_string(&self.arm_file)
            .ok()
            .and_then(|armed| armed.lines().nth(1).map(str::to_owned))
            .is_some_and(|mode| mode == expected)
    }

    fn record(&self, event: &str, stream_id: &str, jid: &FullJid, details: &str) {
        let line = format!("{event} {stream_id} {jid} {details}\n");
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(error) = result {
            tracing::error!(%error, path = %self.events.display(), "failed-resume seam event not recorded");
        }
    }
}

/// Pause at the exact post-publication/pre-finalization boundary and inject a
/// replay gap into the already-claimed session. The test then resumes the
/// ordinary `complete_claim_if_resumable` path, which must produce the real
/// `ReplayWindowTruncated` finalization and binding rollback.
pub(in crate::server::routes::websocket) async fn pause_published_resume_finalization(
    state: &WebSocketState,
    conn: &super::state::WsConnState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) {
    let Some(seam) = FAILED_RESUME_SEAM.as_ref() else {
        return;
    };
    let Some(stream_id) = conn.pending_resume_stream_id.as_deref() else {
        return;
    };
    if seam.account != jid.to_bare()
        || seam.jid != *jid
        || seam.armed_stream().as_deref() != Some(stream_id)
    {
        return;
    }
    let lifecycle_is_running = conn.socket_lifecycle.as_ref().is_some_and(|lifecycle| {
        lifecycle.state() == waddle_xmpp::registry::SocketCleanupState::Running
    });
    let entry_is_published = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(jid, owner)
        .is_some();
    if !lifecycle_is_running || !entry_is_published {
        seam.record(
            "not-published",
            stream_id,
            jid,
            "lifecycle-or-entry-missing",
        );
        return;
    }
    if seam.armed_mode("hold-reservation") {
        match state
            .deps
            .protocol
            .connection_registry
            .try_reserve_pending_flush(jid, Some(owner))
        {
            Ok(reservation) => {
                seam.record("reservation-held", stream_id, jid, "old-binding");
                let seam = seam.clone();
                let registry = std::sync::Arc::clone(&state.deps.protocol.connection_registry);
                let jid = jid.clone();
                let stream_id = stream_id.to_owned();
                tokio::spawn(async move {
                    while !seam.cleanup_release_file.exists() {
                        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
                    }
                    let mut message = xmpp_parsers::message::Message::new(Some(jid.clone().into()));
                    message.id = Some(xmpp_parsers::message::Id(
                        "stale-reservation-must-not-arrive".to_owned(),
                    ));
                    let outcome = registry.send_reserved_pending_flush(
                        reservation,
                        waddle_xmpp::registry::OutboundStanza::new(waddle_xmpp::Stanza::Message(
                            message,
                        )),
                    );
                    seam.record(
                        "reservation-commit",
                        &stream_id,
                        &jid,
                        &format!("{outcome:?}"),
                    );
                });
            }
            Err(error) => {
                seam.record(
                    "reservation-not-held",
                    stream_id,
                    jid,
                    &format!("{error:?}"),
                );
            }
        }
    }
    if seam.armed_mode("queue-accepted-frame") {
        let mut message = xmpp_parsers::message::Message::new(Some(jid.clone().into()));
        message.id = Some(xmpp_parsers::message::Id(
            "accepted-before-failed-resume".to_owned(),
        ));
        let outcome = state
            .deps
            .protocol
            .connection_registry
            .try_send_outbound_if_owner(
                jid,
                owner,
                waddle_xmpp::registry::OutboundStanza::new(waddle_xmpp::Stanza::Message(message)),
            );
        seam.record("queue-attempt", stream_id, jid, &format!("{outcome:?}"));
    }
    seam.record("published", stream_id, jid, "lifecycle=running");
    while !seam.continue_file.exists() {
        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
    }
    let gap_through = conn.pending_resume_h.unwrap_or_default().wrapping_add(1);
    match state
        .deps
        .protocol
        .sm_session_registry
        .set_claimed_replay_gap_for_test(stream_id, gap_through)
    {
        Ok(true) => seam.record("gap-injected", stream_id, jid, "claimed-session"),
        Ok(false) => seam.record("gap-not-injected", stream_id, jid, "claim-missing"),
        Err(error) => seam.record("gap-not-injected", stream_id, jid, &error.to_string()),
    }
}

static TEARDOWN_CUSTODY_EVENTS: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var_os(TEARDOWN_CUSTODY_EVENTS_FILE_ENV).map(PathBuf::from));

fn record_teardown_custody(line: &str) {
    let Some(path) = TEARDOWN_CUSTODY_EVENTS.as_ref() else {
        return;
    };
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(error) = result {
        tracing::error!(%error, path = %path.display(), "teardown custody event not recorded");
    }
}

/// Record that a connection's teardown asked the durable authority to close
/// its session, and whether the session is terminal there afterwards.
pub(in crate::server::routes::websocket) fn observe_durable_session_end(
    session: waddle_xmpp::ownership::SmSessionKey,
    terminal: bool,
) {
    record_teardown_custody(&format!(
        "durable-end {} terminal={terminal}\n",
        session.as_uuid()
    ));
}

/// Record that a connection's teardown starts the promotion of its own
/// stream-management queue.
pub(in crate::server::routes::websocket) fn observe_connection_promotion(jid: &FullJid) {
    record_teardown_custody(&format!("connection-promotion {jid}\n"));
}

/// Record that a connection's teardown has returned: no custody event of
/// that connection follows.
pub(in crate::server::routes::websocket) fn observe_teardown_finished() {
    record_teardown_custody("teardown-finished\n");
}

/// Record the accepted-frame handback decision while the socket still owns
/// its receiver and before the failed-resume handler returns to the loop.
pub(in crate::server::routes::websocket) fn observe_failed_resume_handoff(
    jid: &FullJid,
    stream_id: &str,
    blocked: bool,
) {
    let Some(seam) = FAILED_RESUME_SEAM.as_ref() else {
        return;
    };
    if seam.jid == *jid
        && seam.armed_stream().as_deref() == Some(stream_id)
        && seam.armed_mode("queue-accepted-frame")
    {
        seam.record("handoff", stream_id, jid, &format!("blocked={blocked}"));
    }
}

/// Retain the failed binding's old owner token and run an owner-gated stale
/// unregister only after the test releases it. This models cleanup arriving
/// after a successor has published on the same physical socket; it must not
/// remove the successor's registry entry.
pub(in crate::server::routes::websocket) fn delay_failed_resume_cleanup(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    stream_id: &str,
    entry: waddle_xmpp::registry::ConnectionEntry,
) {
    let Some(seam) = FAILED_RESUME_SEAM.as_ref() else {
        return;
    };
    if seam.account != jid.to_bare()
        || seam.jid != *jid
        || seam.armed_stream().as_deref() != Some(stream_id)
    {
        return;
    }
    seam.record("detached", stream_id, jid, "lifecycle=detached");
    let seam = seam.clone();
    let registry = std::sync::Arc::clone(&state.deps.protocol.connection_registry);
    let jid = jid.clone();
    let owner = *owner;
    let stream_id = stream_id.to_owned();
    tokio::spawn(async move {
        while !seam.cleanup_release_file.exists() {
            tokio::time::sleep(HOLD_POLL_INTERVAL).await;
        }
        let removed = registry.unregister_if_owner(&jid, &owner).is_some();
        let replacement_is_current = registry
            .entry(&jid)
            .is_some_and(|entry| entry.binding_id() != owner);
        seam.record(
            "cleanup-finished",
            &stream_id,
            &jid,
            &format!("removed={removed} replacement={replacement_is_current}"),
        );
        let (ack, _reply) = tokio::sync::oneshot::channel();
        let outcome =
            entry.try_force_detach_and_interrupt(waddle_xmpp::registry::ForceDetachRequest {
                origin: waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement,
                target_binding: None,
                requester_bare_jid: jid.to_bare(),
                ack,
            });
        let status = match outcome {
            Ok(waddle_xmpp::registry::ForceDetachInterruption::Stopped) => "stopped",
            Ok(waddle_xmpp::registry::ForceDetachInterruption::QueuedAfterRetirement) => {
                "queued-after-retirement"
            }
            Err(_) => "not-enqueued",
        };
        seam.record("stale-interrupt", &stream_id, &jid, status);
    });
}

impl HoldBatch {
    /// Append one event line. A failed write is logged; the suite then sees
    /// a missing line and fails.
    fn record(&self, event: HoldEvent, jid: Option<&FullJid>) {
        let jid = jid.map_or_else(|| "-".to_owned(), ToString::to_string);
        let line = format!(
            "{event} {kind} {jid}\n",
            event = event.label(),
            kind = self.kind.label()
        );
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(error) = written {
            tracing::error!(%error, path = %self.events.display(), "seam event not recorded");
        }
    }
}

/// Hold an armed batch for `jid` until another task has queued at least one
/// stanza on that connection's outbound channel.
///
/// The batch is written inline after this returns, and the queued stanza is
/// written only when the connection loop next drains its channel, so the
/// suite observes the order the connection produces when a live stanza is
/// queued while the batch is still unwritten. Not bounded: a release
/// condition that never arrives surfaces as the client's receive timeout.
///
/// `owner` is the connection's registry ownership token when the caller has
/// one; the hold then watches only that connection's own channel. Armed for
/// this kind, an unbound stream (`jid` is `None`) or a matching account
/// without its local registry entry records `not-held`.
pub(in crate::server::routes::websocket) async fn hold_until_live_stanza_queued(
    state: &WebSocketState,
    jid: Option<&FullJid>,
    owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
    batch: HeldBatch,
) {
    let Some(hold) = HOLD_BATCH.as_ref() else {
        return;
    };
    if hold.kind != batch {
        return;
    }
    let Some(jid) = jid else {
        tracing::error!(?batch, "held batch has no bound JID; not holding");
        hold.record(HoldEvent::NotHeld, None);
        return;
    };
    if hold.account != jid.to_bare() {
        return;
    }
    let registry = &state.deps.protocol.connection_registry;
    let entry = match owner {
        Some(owner) => registry.entry_if_owner(jid, owner),
        None => registry.entry(jid),
    }
    .filter(|entry| entry.is_locally_hosted());
    let Some(entry) = entry else {
        tracing::error!(%jid, ?batch, "held batch has no local registry entry; not holding");
        hold.record(HoldEvent::NotHeld, Some(jid));
        return;
    };
    hold.record(HoldEvent::Held, Some(jid));
    while entry.outbound_queue_is_empty_for_test() {
        if entry.outbound_queue_is_closed_for_test() {
            hold.record(HoldEvent::Abandoned, Some(jid));
            return;
        }
        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
    }
    hold.record(HoldEvent::Released, Some(jid));
}

/// One canonical read in `DrainAuthority::authorize`, wrapped only where
/// production actually reads: while the outage seam is armed the read is
/// not started and fails the way a database error fails it
/// (`CanonicalReadFailed`). The read-free binding checks, the exhausted-budget
/// short-circuit and the carbon arm are untouched.
pub(in crate::server::routes::websocket) async fn detach_canonical_read(
    read: impl std::future::Future<Output = Result<(), AppendAuthorityRejection>>,
) -> Result<(), AppendAuthorityRejection> {
    if *DETACH_CANONICAL_READ_OUTAGE {
        Err(AppendAuthorityRejection::CanonicalReadFailed)
    } else {
        read.await
    }
}

/// The armed seam never lets an enable through after a setup error or timeout.
pub(in crate::server::routes::websocket) enum SmEnableHoldOutcome {
    Continue,
    Refused,
}

/// Hold one resumable enable immediately before its transport attempt. The
/// test waits for the real binding revocation, then lets normal code proceed.
pub(in crate::server::routes::websocket) async fn hold_sm_enable_before_start(
    jid: Option<&FullJid>,
    stream: &waddle_xmpp::pending_delivery::SmSessionId,
    permit: &waddle_xmpp::registry::LocalBindingPermit,
) -> SmEnableHoldOutcome {
    let Some(directory) =
        std::env::var_os("WADDLE_TEST_SEAM_SM_ENABLE_DIRECTORY").map(PathBuf::from)
    else {
        return SmEnableHoldOutcome::Continue;
    };
    let Ok(target) = std::env::var("WADDLE_TEST_SEAM_SM_ENABLE_JID") else {
        tracing::error!("SM enable hold lacks its target JID");
        return SmEnableHoldOutcome::Refused;
    };
    if !jid.is_some_and(|jid| jid.to_string() == target) {
        return SmEnableHoldOutcome::Continue;
    }
    if let Err(error) = std::fs::write(directory.join("held"), stream.as_str()) {
        tracing::error!(%error, "could not record SM enable hold");
        return SmEnableHoldOutcome::Refused;
    }
    let release = directory.join("release");
    let wait = async {
        loop {
            if release.exists() {
                return Ok::<(), std::io::Error>(());
            }
            tokio::select! {
                _ = permit.revoked() => {
                    std::fs::write(directory.join("revoked"), stream.as_str())?;
                    while !release.exists() {
                        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
                    }
                    return Ok(());
                }
                _ = tokio::time::sleep(HOLD_POLL_INTERVAL) => {}
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(30), wait).await {
        Ok(Ok(())) => SmEnableHoldOutcome::Continue,
        Ok(Err(error)) => {
            tracing::error!(%error, "could not record SM enable revocation");
            SmEnableHoldOutcome::Refused
        }
        Err(_) => {
            tracing::error!("SM enable hold timed out");
            SmEnableHoldOutcome::Refused
        }
    }
}
