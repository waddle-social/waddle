//! Which stream-management authority a serving process uses.

use std::sync::Arc;

use thiserror::Error;
use waddle_xmpp::stream_management::durable::ExposureBounds;
use waddle_xmpp::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE;

use super::error::DurableSmError;
use super::store::DurableSmStore;
use crate::db::Database;

/// Environment variable naming the stream-management authority. The durable
/// session authority is the only one, so the variable may be unset or
/// `durable`.
pub const DURABLE_SM_MODE_ENV: &str = "WADDLE_XMPP_DURABLE_SM";

/// Retained payload bytes one session may hold under the default bounds.
const DEFAULT_MAX_OUTSTANDING_BYTES: u64 = 16 * 1024 * 1024;

/// The environment names a stream-management authority this binary does not
/// have. Startup refuses it rather than serve with another authority than
/// the one the operator named.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "{DURABLE_SM_MODE_ENV} must be unset or `durable`, not `{0}`: the durable session \
     authority is the only stream-management authority"
)]
pub struct InvalidDurableSmMode(String);

/// Check [`DURABLE_SM_MODE_ENV`]: unset and `durable` are accepted, any
/// other value, `legacy` included, is refused.
pub fn check_durable_sm_mode_env() -> Result<(), InvalidDurableSmMode> {
    match std::env::var(DURABLE_SM_MODE_ENV) {
        Ok(value) => check_durable_sm_mode(&value),
        Err(_) => Ok(()),
    }
}

/// Check one value of [`DURABLE_SM_MODE_ENV`].
pub fn check_durable_sm_mode(value: &str) -> Result<(), InvalidDurableSmMode> {
    match value.trim() {
        "durable" => Ok(()),
        other => Err(InvalidDurableSmMode(other.to_owned())),
    }
}

/// The stream-management authority startup selects: the durable session
/// store. Startup publishes it on the application state; a state that never
/// ran that selection has none, and every resumable stream-management
/// operation on it is refused.
#[derive(Clone)]
pub struct DurableSmSelection {
    store: Arc<DurableSmStore>,
}

impl DurableSmSelection {
    /// Open the durable store over the ingress database. On the attested
    /// runtime pool the store attaches to owner-prepared relations, whose
    /// sessions without an authority record the owner phase has drained.
    /// Otherwise the store creates its relations and drains those sessions
    /// itself.
    pub async fn open(db: &Database, clustered: bool) -> Result<Self, DurableSmError> {
        let bounds = default_bounds()?;
        let store = if db.uses_attested_postgres_pool() {
            DurableSmStore::attach_runtime(db.clone(), bounds, clustered).await?
        } else {
            let store = DurableSmStore::open(db.clone(), bounds, clustered).await?;
            let drained = super::legacy_drain::drain_legacy_sm_sessions(db)
                .await
                .map_err(Box::new)?;
            if drained.sessions > 0 {
                tracing::info!(
                    sessions = drained.sessions,
                    stored = drained.stored,
                    released = drained.released,
                    acknowledged = drained.acknowledged,
                    dropped = drained.dropped,
                    "moved stream-management sessions without a durable record to offline storage"
                );
            }
            store
        };
        Ok(Self {
            store: Arc::new(store),
        })
    }

    /// The durable store.
    pub fn store(&self) -> &Arc<DurableSmStore> {
        &self.store
    }
}

fn default_bounds() -> Result<ExposureBounds, DurableSmError> {
    let frames = u32::try_from(DEFAULT_MAX_UNACKED_QUEUE_SIZE).unwrap_or(u32::MAX);
    ExposureBounds::new(frames, DEFAULT_MAX_OUTSTANDING_BYTES).map_err(|_| {
        DurableSmError::Unsupported {
            operation: "the configured retained-frame bounds",
        }
    })
}
