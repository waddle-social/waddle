//! Shared fan-out of XEP-0060 publish (§7.1) and retract (§7.2.2.1) events.
//!
//! Called from both the generic `PubSubRequest::Publish` arm and
//! `handle_spaces_publish` after `publish_item` succeeds, so §7.1
//! emission is a single seam regardless of node type (PEP, regular
//! PubSub, Spaces).
//!
//! Behavior:
//!
//! - Reads `NodeConfig` once via `pubsub_storage::get_node` to honor
//!   `deliver_payloads` (XEP-0060 §7.1.3.5) at item-construction time.
//! - Queries `pubsub_storage::list_deliverable_subscribers` — outcasts
//!   and non-`Subscribed` entries are filtered by storage.
//! - Builds the event item with `<item id='X' [publisher='Y']>...</item>`
//!   per §7.1.5 (publisher attribute only when ≠ owner).
//! - Expands subscribers per §6.1.6 (bare JID → all live resources) and
//!   §6.1.7 (full JID → that resource).
//! - Falls back to the SM detached-resource stash (XEP-0198) when the
//!   live channel is closed or the recipient is not currently
//!   connected. Bare-JID subscribers with no live resources also have
//!   their detached resources enumerated explicitly so resumable
//!   sessions still receive the event on resume.
//! - Best-effort: storage errors are logged and swallowed; subscribers
//!   catch up via `<items/>` on reconnect (RFC 6121 §8.5.2.1.4 forbids
//!   offline storage of headline messages, which is the type used by
//!   `build_pubsub_event`).
//!
//! Race window: a subscriber may unsubscribe between the deliverable-list
//! query and per-resource dispatch; the trailing event is permitted by
//! §7.1 and not worth holding a lock across fan-out to prevent.

use jid::{BareJid, FullJid, Jid};
use std::collections::HashSet;
use tracing::{info, warn};
use waddle_xmpp::pubsub::{build_pubsub_event, AccessModel, PubSubEvent, PubSubItem};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::build_pubsub_retract_event;
use xmpp_parsers::message::Message;

use super::super::WebSocketState;

mod caps;

use caps::{owner_self_caps_fan_out, roster_caps_fan_out, CapsFanOutCtx};

mod implementation;
pub use implementation::fan_out_pep_retract;
pub use implementation::fan_out_publish;
pub use implementation::fan_out_retract;
use implementation::FanOutMetrics;
pub use implementation::FanOutRequest;
pub use implementation::FanOutRetractRequest;
