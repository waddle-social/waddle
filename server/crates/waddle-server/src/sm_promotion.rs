//! XEP-0198 SM-expiry promotion (issue #209 slice (d) phase 4,
//! locked Q6 = B).
//!
//! When a detached XEP-0198 SM session's resume window closes (or
//! the server gracefully drains a live session at shutdown), the
//! server MUST treat its unacked stanzas the way XEP-0198 §5
//! line 364 prescribes:
//!
//! > "treat unacknowledged stanzas in the same way that it would
//! > treat a stanza sent to an unavailable resource, by either
//! > returning an error to the sender, delivery to an alternate
//! > resource, or committing the stanza to offline storage."
//!
//! The locked Q6 = B priority chain implements all three options in
//! priority order: **alt-resource → offline-storage → service-
//! unavailable error**. Each unacked stanza is re-run through the
//! [`classify_dm_intake`] classifier (locked Q6b: "promotion filter
//! delegates to classify_dm_intake" — single source of truth for
//! the type/hint matrix) and the resulting [`DmRouting`] gates which
//! branch fires.

use std::collections::HashMap;
mod custody;
pub(crate) use custody::{promote_ingress_custody, promote_session_with_custody};
mod live;
mod pending;
mod stanza;
#[cfg(test)]
mod tests;
mod types;

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use kameo::actor::ActorRef;
use tracing::{debug, instrument};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, DmRouting, LiveDecision, OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::{ConnectionRegistry, SendResult, UserRegistryActor};
use waddle_xmpp::stream_management::{
    DetachedSession, DetachedUnackedStanza, TOMBSTONE_CLOCK_SKEW_SLACK,
};
use waddle_xmpp::Stanza;

use live::{build_online_resources, collect_live_targets};
use pending::{insert_pending, promote_as_transient, DeliveryHandles, PromotionOrigin};
use stanza::{parse_stanza, promote_iq, promote_presence};
pub use types::{PromotedOutcome, PromotionFailureReason, PromotionSummary};

mod implementation;
pub(crate) use implementation::confirm_pending_promotion;
use implementation::detached_session_for_terminal_entry;
#[cfg(test)]
use implementation::is_senderless_inbox_projection;
pub(crate) use implementation::promote_displaced_sessions;
pub(crate) use implementation::promote_guarded_sessions;
use implementation::promote_one;
pub use implementation::promote_session_unacked;
use implementation::promote_session_unacked_with_policy;
pub(crate) use implementation::promote_terminal_overflow_entry;
pub(crate) use implementation::prune_promoted_sequences;
pub use implementation::prune_promoted_then_reinsert_for_retry;
pub use implementation::recent_tombstones_for_promotion;
pub use implementation::reinsert_failed_session_for_retry;
pub(crate) use implementation::release_row_backed_replay_copies;
pub use implementation::scrub_pending_for_tombstones_recorded_during_promotion;
pub use implementation::DisplacedPromotionDeps;
pub(crate) use implementation::DisplacedPromotionOutcome;
pub(crate) use implementation::PromotionBatchGuard;
use implementation::PromotionContext;
pub use implementation::PromotionScrubOutcome;
pub(crate) use implementation::PromotionSessionGuard;
pub(crate) use implementation::RowBackedReleaseOutcome;
use implementation::StreamPromotionDeps;
pub(crate) use implementation::TerminalOverflowPromotionDeps;
