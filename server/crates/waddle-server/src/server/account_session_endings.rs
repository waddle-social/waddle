//! A replica's part in ending retired accounts' sessions (ADR-0020 D8;
//! `crate::auth::session_endings`).
//!
//! Two tasks run on every serving replica:
//!
//! - [`ReplicaLease`] registers the replica's lease when it starts serving
//!   and renews it every [`SESSION_ENDING_RENEWAL`]. It runs apart from the
//!   work, so a pass that waits for a stream to close does not let the
//!   lease run out; it releases the lease when the process stops.
//! - [`RequestedEndings`] reads, every [`SESSION_ENDING_POLL`], the requests
//!   of the window this replica has not completed; the read writes
//!   nothing. It holds every requested account in the replica's
//!   [`RetiredAccounts`] first, so that a binding of any of them is refused
//!   from then on; then, for each request, it records that it observed the
//!   request, ends the account's live streams and detached sessions on this
//!   replica, empties the account's offline store, and records the request
//!   as completed once none is left. A pass that leaves something behind,
//!   or a statement that fails, leaves the request to the next poll, which
//!   adds what it ends to what the earlier passes ended.
//!
//! For a short while after it took a request up the replica also closes
//! any stream of the account it finds again, and empties its offline store
//! again: a binding that checked the set just before the account was added
//! can be published after the pass read the registry, and its teardown
//! stores what it had not delivered.
//!
//! [`RetiredAccounts`]: crate::server::routes::websocket::RetiredAccounts
//!
//! Log lines carry no JID.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::session_endings::{
    record_completed, record_observed, register_replica, release_replica, renew_replica,
    requests_to_complete, EndedSessions, SessionEndingError, SESSION_ENDING_POLL,
    SESSION_ENDING_RENEWAL, SESSION_ENDING_WINDOW,
};
use crate::db::Database;
use crate::permissions::host_grant::RateLimitedWarning;
use crate::server::routes::websocket::{
    close_live_streams, discard_offline_messages, end_account_sessions, WebSocketState,
};

/// How long after taking a request up the replica keeps closing the
/// account's streams it finds again.
const RESWEEP_AFTER_TAKE_UP: Duration = Duration::from_secs(60);
/// The longest name a lease row holds for its replica.
const NODE_LABEL_LIMIT: usize = 255;

/// At most one warning a minute that the lease could not be kept.
static LEASE_FAILED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that a pass over the requests failed.
static PASS_FAILED: RateLimitedWarning = RateLimitedWarning::new();

/// How this replica is named in the command's report: its clustering node
/// identity, or its host and process.
fn node_label(state: &WebSocketState) -> String {
    let label = match state.deps.app_state.clustering_claims.claim_pair() {
        Some((_, identity)) => {
            let identity = identity.current();
            [identity.node_id.as_str(), identity.node_epoch.as_str()].join("/")
        }
        None => {
            let host = std::env::var("HOSTNAME")
                .ok()
                .filter(|host| !host.trim().is_empty())
                .unwrap_or_else(|| "localhost".to_owned());
            [host, std::process::id().to_string()].join(":")
        }
    };
    bounded_label(label)
}

/// `label` cut to at most [`NODE_LABEL_LIMIT`] bytes on a character
/// boundary, or a placeholder for an empty one.
fn bounded_label(label: String) -> String {
    if label.is_empty() {
        return "unnamed".to_owned();
    }
    if label.len() <= NODE_LABEL_LIMIT {
        return label;
    }
    let mut end = NODE_LABEL_LIMIT;
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    label[..end].to_owned()
}

/// This replica's lease in the session-ending tables.
pub struct ReplicaLease {
    replica: Uuid,
    node: String,
    registered: bool,
}

impl ReplicaLease {
    /// A lease under a new replica identity, not yet registered.
    pub fn new(state: &WebSocketState) -> Self {
        Self {
            replica: Uuid::new_v4(),
            node: node_label(state),
            registered: false,
        }
    }

    /// The replica identity the lease and its acknowledgements carry.
    pub fn replica(&self) -> Uuid {
        self.replica
    }

    /// Register the lease, or renew it.
    pub async fn keep(&mut self, db: &Database) -> Result<(), SessionEndingError> {
        if self.registered {
            if renew_replica(db, self.replica).await? {
                return Ok(());
            }
            // The owner removed the row of a lease that had long ended:
            // this process did not renew it for a day.
            self.registered = false;
        }
        register_replica(db, self.replica, &self.node).await?;
        self.registered = true;
        Ok(())
    }

    /// End the lease now, so that no command waits for this replica.
    pub async fn release(&mut self, db: &Database) -> Result<(), SessionEndingError> {
        if !self.registered {
            return Ok(());
        }
        release_replica(db, self.replica).await?;
        self.registered = false;
        Ok(())
    }
}

/// The requests this replica works on, and what it ended for each one it
/// has not completed yet.
pub struct RequestedEndings {
    replica: Uuid,
    progress: HashMap<Uuid, EndedSessions>,
}

impl RequestedEndings {
    /// The work of the replica the lease `replica` names.
    pub fn new(replica: Uuid) -> Self {
        Self {
            replica,
            progress: HashMap::new(),
        }
    }

    /// One pass: take up every request of the window this replica has not
    /// completed, end the account's sessions on this replica, and record
    /// each request that left nothing behind as completed. Returns how many
    /// it completed.
    pub async fn run(&mut self, state: &WebSocketState) -> Result<usize, SessionEndingError> {
        let db = state.deps.app_state.db_pool.global();
        let retired = &state.deps.protocol.retired_accounts;
        let window_start = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(i64::try_from(SESSION_ENDING_WINDOW.as_millis()).unwrap_or(i64::MAX));
        retired.forget_requested_before(window_start);
        let requests = requests_to_complete(db, self.replica).await?;
        self.progress
            .retain(|id, _| requests.iter().any(|request| request.id == *id));
        // Every requested account is held before any session is ended: a
        // binding of any of them that registers after this point is
        // refused, and one that registered before is in the registry the
        // pass reads, also while the pass waits for another account's
        // stream to close.
        for request in &requests {
            retired.insert(request.account.clone(), request.requested_at_ms);
        }
        let mut completed = 0;
        for request in requests {
            if !self.progress.contains_key(&request.id) {
                if let Err(error) = record_observed(db, request.id, self.replica).await {
                    debug!(%error, "a session-ending request's observation was not recorded");
                }
            }
            let pass = end_account_sessions(state, &request.account).await;
            let total = {
                let total = self.progress.entry(request.id).or_default();
                total.add(pass.ended);
                *total
            };
            if !pass.complete {
                debug!("a retired account still has sessions here; the next poll tries again");
                continue;
            }
            record_completed(db, request.id, self.replica, total).await?;
            self.progress.remove(&request.id);
            completed += 1;
            waddle_xmpp::counter_add!(
                "waddle.account_retirement.requests_completed",
                "{request}",
                "Requests to end a retired account's sessions this replica completed",
                1,
            );
        }
        for account in retired.taken_up_within(RESWEEP_AFTER_TAKE_UP) {
            let (closed, none_left) = close_live_streams(state, &account).await;
            if closed > 0 {
                debug!(
                    closed,
                    "closed streams a retired account bound while its request was taken up"
                );
            }
            if none_left {
                discard_offline_messages(state, &account).await;
            }
        }
        Ok(completed)
    }
}

/// Start this replica's lease and its work on the requests. Called once,
/// when the replica starts serving; both stop with the process.
pub(crate) fn spawn_account_session_endings(websocket_state: &Arc<WebSocketState>) {
    let mut lease = ReplicaLease::new(websocket_state);
    let mut endings = RequestedEndings::new(lease.replica());
    let stop = websocket_state.deps.shutdown.stop_token();

    let weak = Arc::downgrade(websocket_state);
    let lease_stop = stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SESSION_ENDING_RENEWAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = lease_stop.cancelled() => break,
                _ = ticker.tick() => {}
            }
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Err(error) = lease.keep(state.deps.app_state.db_pool.global()).await {
                if LEASE_FAILED.due() {
                    warn!(%error, "the session-ending lease could not be kept; retrying");
                }
            }
        }
        // The process stops serving: no command waits for this replica.
        if let Some(state) = weak.upgrade() {
            if let Err(error) = lease.release(state.deps.app_state.db_pool.global()).await {
                debug!(%error, "the session-ending lease was not released; it runs out");
            }
        }
    });

    let weak = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SESSION_ENDING_POLL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                _ = ticker.tick() => {}
            }
            let Some(state) = weak.upgrade() else {
                break;
            };
            if let Err(error) = endings.run(&state).await {
                if PASS_FAILED.due() {
                    warn!(%error, "a pass over the session-ending requests failed; retrying");
                }
            }
        }
    });
}
