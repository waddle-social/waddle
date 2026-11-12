//! Which stream-management authority a serving process uses.

use std::str::FromStr;
use std::sync::Arc;

use thiserror::Error;
use waddle_xmpp::stream_management::durable::ExposureBounds;
use waddle_xmpp::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE;

use super::error::DurableSmError;
use super::store::DurableSmStore;
use crate::db::Database;

/// Environment variable naming the [`DurableSmMode`].
pub const DURABLE_SM_MODE_ENV: &str = "WADDLE_XMPP_DURABLE_SM";

/// Retained payload bytes one session may hold under the default bounds.
const DEFAULT_MAX_OUTSTANDING_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{DURABLE_SM_MODE_ENV} must be `legacy` or `durable`, not `{0}`")]
pub struct InvalidDurableSmMode(String);

/// The configured stream-management authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurableSmMode {
    /// The in-memory session registry with detach-time persistence.
    #[default]
    Legacy,
    /// The durable session authority in [`crate::sm_authority`].
    Durable,
}

impl FromStr for DurableSmMode {
    type Err = InvalidDurableSmMode;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "legacy" => Ok(Self::Legacy),
            "durable" => Ok(Self::Durable),
            other => Err(InvalidDurableSmMode(other.to_owned())),
        }
    }
}

impl DurableSmMode {
    /// Read the mode from the environment. An unset variable is `Legacy`; a
    /// value that is neither mode is an error, so a typo cannot silently
    /// select the other authority.
    pub fn from_env() -> Result<Self, InvalidDurableSmMode> {
        match std::env::var(DURABLE_SM_MODE_ENV) {
            Ok(value) => value.parse(),
            Err(_) => Ok(Self::Legacy),
        }
    }
}

/// The authority a serving path consults. `Legacy` carries no store, so a
/// caller cannot reach durable operations unless the mode selected them.
#[derive(Clone, Default)]
pub enum DurableSmSelection {
    #[default]
    Legacy,
    Durable(Arc<DurableSmStore>),
}

impl DurableSmSelection {
    /// Build the selection for `mode` over the ingress database. On the
    /// attested runtime pool the store attaches to owner-prepared relations;
    /// otherwise it creates them.
    pub async fn open(
        mode: DurableSmMode,
        db: &Database,
        clustered: bool,
    ) -> Result<Self, DurableSmError> {
        match mode {
            DurableSmMode::Legacy => Ok(Self::Legacy),
            DurableSmMode::Durable => {
                let bounds = default_bounds()?;
                let store = if db.uses_attested_postgres_pool() {
                    DurableSmStore::attach_runtime(db.clone(), bounds, clustered).await?
                } else {
                    DurableSmStore::open(db.clone(), bounds, clustered).await?
                };
                Ok(Self::Durable(Arc::new(store)))
            }
        }
    }

    /// The durable store, when the durable authority is selected.
    pub fn store(&self) -> Option<&DurableSmStore> {
        match self {
            Self::Legacy => None,
            Self::Durable(store) => Some(store),
        }
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
