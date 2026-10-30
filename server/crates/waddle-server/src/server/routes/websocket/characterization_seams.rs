//! Deterministic seams for the W1.6c characterization suite
//! (`tests/characterization_resume_muc.rs`, #2046).
//!
//! Compiled only with the `test-support` feature, which release images never
//! enable, and inert unless the spawned test server's environment names a seam.
//! The harness strips every inherited `WADDLE_*` variable, so only a suite that
//! passes one explicitly through `TestServer::start_*_with_extra_envs` arms it.
//! Each seam reproduces one interleaving or fault that the suite cannot force
//! over the wire; none changes what the production code does once it resumes.

use std::sync::LazyLock;
use std::time::Duration;

use jid::{BareJid, FullJid};

use super::WebSocketState;
use crate::ingress::append_authority::AppendAuthorityRejection;

/// Which response batch to hold: `sm-resumed` or `muc-join`.
const HOLD_BATCH_KIND_ENV: &str = "WADDLE_TEST_SEAM_HOLD_BATCH_KIND";
/// Bare JID of the account whose batches are held; other accounts are never held.
const HOLD_BATCH_ACCOUNT_ENV: &str = "WADDLE_TEST_SEAM_HOLD_BATCH_ACCOUNT";
/// `unavailable` fails every detach-time canonical read.
const DETACH_CANONICAL_READ_ENV: &str = "WADDLE_TEST_SEAM_DETACH_CANONICAL_READ";

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
    /// The XEP-0045 §7.1 join burst (§7.2.2 presence broadcast, §7.2.15 subject), after the room admitted the occupant
    /// and its join presence went to the existing occupants.
    MucJoin,
}

struct HoldBatch {
    kind: HeldBatch,
    account: BareJid,
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
    let account = std::env::var(HOLD_BATCH_ACCOUNT_ENV).ok()?;
    match account.parse::<BareJid>() {
        Ok(account) => Some(HoldBatch { kind, account }),
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

/// Hold an armed batch for `jid` until another task has queued at least one
/// stanza on that connection's outbound channel.
///
/// The batch is written inline after this returns, and the queued stanza is
/// written only when the connection loop next drains its channel, so the
/// suite observes the order the connection produces when a live stanza is
/// queued while the batch is still unwritten. Not bounded: a release
/// condition that never arrives surfaces as the client's receive timeout.
pub(in crate::server::routes::websocket) async fn hold_until_live_stanza_queued(
    state: &WebSocketState,
    jid: &FullJid,
    batch: HeldBatch,
) {
    let Some(hold) = HOLD_BATCH.as_ref() else {
        return;
    };
    if hold.kind != batch || hold.account != jid.to_bare() {
        return;
    }
    let Some(entry) = state
        .deps
        .protocol
        .connection_registry
        .entry(jid)
        .filter(|entry| entry.is_locally_hosted())
    else {
        tracing::error!(%jid, ?batch, "held batch has no local registry entry; not holding");
        return;
    };
    tracing::info!(%jid, ?batch, "characterization seam holding response batch");
    while entry.sender.capacity() == entry.sender.max_capacity() && !entry.sender.is_closed() {
        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
    }
    tracing::info!(%jid, ?batch, "characterization seam released response batch");
}

/// A detach-time canonical read, failed the way a database error fails it
/// (`CanonicalReadFailed`) while the outage seam is armed.
pub(in crate::server::routes::websocket) fn detach_canonical_read(
) -> Result<(), AppendAuthorityRejection> {
    if *DETACH_CANONICAL_READ_OUTAGE {
        Err(AppendAuthorityRejection::CanonicalReadFailed)
    } else {
        Ok(())
    }
}
