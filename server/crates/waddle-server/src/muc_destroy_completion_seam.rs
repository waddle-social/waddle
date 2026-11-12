//! Deterministic seam for the owner-IQ room destroy reply.
//!
//! Compiled only with the `test-support` feature, which release images never
//! enable, and inert unless the spawned server's environment names a control
//! directory. The harness strips every inherited `WADDLE_*` variable, so only
//! a suite that passes the directory to one spawned process arms that process.
//!
//! `hold-inline-claim`: while this file exists, the owner-IQ handler waits
//! after its destroy has committed and before it claims the completion it
//! then drains inline. Every destroy completion janitor in the cluster ticks
//! during a hold of more than its interval, so a suite can show which
//! executor ends up delivering the completion and what the owner receives.
//!
//! A suite arms the seam by creating the file in the control directory and
//! releases it by removing the file. Each engagement appends
//! `held <attempt>` and then `released <attempt>` to `events` in the same
//! directory, so a test fails when the seam never engages. The seam changes
//! nothing about what the handler does once it resumes.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use waddle_xmpp::muc::DestroyAttemptId;

/// Directory holding the arm file and the `events` log for this process.
const DIRECTORY_ENV: &str = "WADDLE_TEST_SEAM_DESTROY_COMPLETION_DIRECTORY";
const EVENTS_FILE: &str = "events";
const HOLD_INLINE_CLAIM_FILE: &str = "hold-inline-claim";

/// Poll interval while the hold waits for its arm file to be removed. The
/// file, not the interval, is the synchronisation.
const HOLD_POLL_INTERVAL: Duration = Duration::from_millis(2);

static DIRECTORY: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var_os(DIRECTORY_ENV).map(PathBuf::from));

fn record(directory: &Path, event: &str, attempt: DestroyAttemptId) {
    let line = format!("{event} {}\n", attempt.as_uuid());
    let path = directory.join(EVENTS_FILE);
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(error) = written {
        tracing::error!(%error, path = %path.display(), "destroy completion seam event not recorded");
    }
}

/// Wait before the inline completion claim for as long as the arm file
/// exists.
pub(crate) async fn hold_inline_claim(attempt: DestroyAttemptId) {
    let Some(directory) = DIRECTORY.as_deref() else {
        return;
    };
    let arm_file = directory.join(HOLD_INLINE_CLAIM_FILE);
    if !arm_file.exists() {
        return;
    }
    record(directory, "held", attempt);
    while arm_file.exists() {
        tokio::time::sleep(HOLD_POLL_INTERVAL).await;
    }
    record(directory, "released", attempt);
}
