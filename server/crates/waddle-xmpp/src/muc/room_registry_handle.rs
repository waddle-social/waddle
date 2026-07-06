//! Instrumented, bounded handle to the [`RoomRegistryActor`].
//!
//! The #757 production incident was a wedged `RoomRegistryActor`: callers that
//! `.ask()`ed it blocked forever, freezing the per-connection frame loop with no
//! actionable signal. This handle is fix-3 of that incident series. It wraps a
//! plain [`ActorRef<RoomRegistryActor>`] and makes every request:
//!
//! - **bounded in time** — a [`ROOM_REGISTRY_REPLY_TIMEOUT`] reply timeout turns
//!   an indefinite hang into a typed [`RoomRegistryError::Timeout`] in seconds;
//! - **observable** — per-request latency is recorded
//!   ([`metrics::record_actor_mailbox_latency`]) and a `warn!` fires when a
//!   request exceeds [`ROOM_REGISTRY_SLOW_ASK_WARN`];
//! - **typed on failure** — mailbox/transport failures map to typed
//!   [`RoomRegistryError`] variants rather than stringly-typed diagnostics.
//!
//! The actor is spawned with an **explicit named bounded mailbox**
//! ([`ROOM_REGISTRY_MAILBOX_CAPACITY`]); a saturated mailbox produces a typed
//! error instead of unbounded growth, and the remaining capacity feeds the
//! periodic mailbox-depth gauge.
//!
//! This handle is the *fast, specific* fail-path for the known actor wedge,
//! complementary to the coarse per-stanza frame backstop (#808): see
//! `docs/adr/008-stanza-handler-wedge-backstop.md`.

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use tokio::time::Instant;
use tracing::warn;

use super::affiliation::DurableMembershipSource;
use super::room_actor::{RoomActor, SealGuard};
use super::room_registry_actor::{
    CreateInstantRoom, CreateRoom, DestroyRoom, DestroyRoomIfInactive, GetOrCreateRoom, GetRoom,
    IsMucJid, ListRooms, RoomCount, RoomExists, RoomRegistryActor, RoomRegistryError,
};
use super::RoomConfig;
use crate::metrics;
use crate::xep::xep0421::OccupantIdSecret;

/// Explicit bounded mailbox capacity for the `RoomRegistryActor`.
///
/// kameo's implicit default is 64; we name it (and raise it slightly) so the
/// value is reviewable and the depth gauge has a stable denominator. 128 gives
/// burst headroom for reconnection storms while keeping a wedged actor's backlog
/// bounded and observable rather than growing without limit.
pub const ROOM_REGISTRY_MAILBOX_CAPACITY: usize = 128;

/// Hard fail-fast budget for a single registry request.
///
/// Chosen well above normal handler latency (sub-millisecond in practice) so it
/// never trips on healthy load, yet comfortably below the #808 per-stanza frame
/// backstop (15s) so a wedged registry surfaces as a typed error long before the
/// coarse frame timeout would fire.
pub const ROOM_REGISTRY_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard fail-fast budget for *enqueuing* a request into the bounded mailbox.
///
/// `reply_timeout` only bounds the wait for the actor's reply — not the wait for
/// free mailbox capacity. When a wedged actor's 128-slot mailbox saturates, a
/// caller would otherwise block indefinitely on `send` before its request is
/// even enqueued. Bounding the enqueue wait too means a saturated mailbox
/// surfaces as a typed [`RoomRegistryError`] instead of freezing the caller.
/// Same budget as the reply timeout; worst-case total stays below the #808 15s
/// frame backstop.
pub const ROOM_REGISTRY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(5);

/// Latency above which a single registry request is logged at `warn!` — a
/// leading indicator of saturation before the hard [`ROOM_REGISTRY_REPLY_TIMEOUT`].
pub const ROOM_REGISTRY_SLOW_ASK_WARN: Duration = Duration::from_millis(500);

/// Metrics actor label for the registry (kept stable for dashboards/alerts).
const ACTOR_LABEL: &str = "room_registry";

/// Cheap-clone, instrumented handle to the MUC room registry actor.
#[derive(Clone)]
pub struct RoomRegistry {
    inner: ActorRef<RoomRegistryActor>,
    max_capacity: usize,
}

impl RoomRegistry {
    /// Spawn the registry actor behind an explicit bounded mailbox and return an
    /// instrumented handle to it.
    ///
    /// `membership_source` hydrates every freshly spawned `RoomActor`'s
    /// durable inbox recipient set from the deployment's durable
    /// membership store (#1135). Pass `None` only when no durable
    /// membership store exists (tests, tools); production deployments
    /// must wire one or offline members drop out of groupchat inbox
    /// fan-out after each room-actor respawn.
    pub fn spawn(
        muc_domain: String,
        occupant_id_secret: OccupantIdSecret,
        membership_source: Option<Arc<dyn DurableMembershipSource>>,
    ) -> Self {
        let mut actor = RoomRegistryActor::new(muc_domain, occupant_id_secret);
        if let Some(source) = membership_source {
            actor = actor.with_membership_source(source);
        }
        let inner = RoomRegistryActor::spawn_with_mailbox(
            actor,
            mailbox::bounded(ROOM_REGISTRY_MAILBOX_CAPACITY),
        );
        Self {
            inner,
            max_capacity: ROOM_REGISTRY_MAILBOX_CAPACITY,
        }
    }

    /// Wrap an existing actor ref (e.g. one already stored in shared state) so
    /// callers can use the instrumented typed methods without re-spawning.
    pub fn from_actor_ref(inner: ActorRef<RoomRegistryActor>, max_capacity: usize) -> Self {
        Self {
            inner,
            max_capacity,
        }
    }

    /// Wrap a shared `ActorRef` with the deployment's default mailbox capacity.
    ///
    /// Convenience for production call sites that hold the raw
    /// `ActorRef<RoomRegistryActor>` from shared state and want the instrumented
    /// typed methods (reply + mailbox timeout, typed errors, latency metrics) per
    /// request, without threading the capacity constant through every site. The
    /// capacity only feeds [`RoomRegistry::mailbox_depth`] (the gauge), so the
    /// spawn-time default is correct for the ask path.
    pub fn wrap(inner: ActorRef<RoomRegistryActor>) -> Self {
        Self::from_actor_ref(inner, ROOM_REGISTRY_MAILBOX_CAPACITY)
    }

    /// The underlying actor ref, for the few call sites that still need it
    /// directly (e.g. test fixtures).
    pub fn actor_ref(&self) -> &ActorRef<RoomRegistryActor> {
        &self.inner
    }

    /// Whether the registry actor is still running.
    pub fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    /// The mailbox's configured capacity (denominator for the depth gauge).
    pub fn max_capacity(&self) -> i64 {
        self.max_capacity as i64
    }

    /// Current mailbox depth (queued messages), or `None` if the mailbox is
    /// unbounded (never the case for the spawned registry). Depth is
    /// `capacity - remaining`, where the bounded `MailboxSender` reports the
    /// remaining free slots.
    pub fn mailbox_depth(&self) -> Option<i64> {
        self.inner
            .mailbox_sender()
            .capacity()
            .map(|remaining| self.max_capacity().saturating_sub(remaining as i64))
    }

    /// Record latency / slow-warn on success and classify a request outcome into
    /// a typed [`RoomRegistryError`]. `elapsed` is measured by the caller around
    /// the `.ask(..).reply_timeout(..).await`.
    ///
    /// Generic over the message param `M` and handler error `E`: kameo's
    /// `ask().reply_timeout()` yields `SendError<Msg, RoomRegistryError>` for the
    /// `Result`-reply handlers and `SendError<Msg, Infallible>` for the plain-reply
    /// ones, so `E: Into<RoomRegistryError>` (with the `From<Infallible>` impl
    /// below) unifies both.
    fn classify<R, M, E>(
        operation: &'static str,
        elapsed: Duration,
        result: Result<R, SendError<M, E>>,
    ) -> Result<R, RoomRegistryError>
    where
        E: Into<RoomRegistryError>,
    {
        // A completed round-trip (the actor processed the request and replied,
        // with a value OR a typed handler error) is a representative latency
        // sample; record both so a slow handler that *errors* still appears on
        // P95/P99 dashboards. Only Timeout/transport-drop — where the actor
        // never replied — are excluded.
        let record_round_trip = |outcome: &str| {
            metrics::record_actor_mailbox_latency(
                ACTOR_LABEL,
                operation,
                "ask",
                elapsed.as_secs_f64() * 1000.0,
            );
            if elapsed >= ROOM_REGISTRY_SLOW_ASK_WARN {
                warn!(
                    operation,
                    outcome,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "RoomRegistryActor request slow"
                );
            }
        };

        match result {
            Ok(reply) => {
                record_round_trip("ok");
                Ok(reply)
            }
            Err(SendError::HandlerError(error)) => {
                record_round_trip("handler_error");
                Err(error.into())
            }
            Err(SendError::Timeout(_)) => {
                metrics::record_actor_request_timeout(ACTOR_LABEL, operation, "ask");
                warn!(
                    operation,
                    timeout_ms = ROOM_REGISTRY_REPLY_TIMEOUT.as_millis() as u64,
                    "RoomRegistryActor request timed out"
                );
                Err(RoomRegistryError::Timeout)
            }
            Err(other) => {
                metrics::record_actor_request_dropped(
                    ACTOR_LABEL,
                    operation,
                    "ask",
                    send_error_reason(&other),
                );
                warn!(operation, "RoomRegistryActor request dropped");
                Err(RoomRegistryError::Unavailable)
            }
        }
    }
}

/// Plain-reply registry handlers (`bool`/`usize`/`()`) carry kameo's own
/// [`kameo::error::Infallible`] (an uninhabited enum, distinct from
/// `std::convert::Infallible`) as the handler-error type. This lets
/// [`RoomRegistry::classify`] accept both those and the `Result`-reply handlers
/// (`RoomRegistryError`) under one `E: Into<RoomRegistryError>` bound.
impl From<kameo::error::Infallible> for RoomRegistryError {
    fn from(never: kameo::error::Infallible) -> Self {
        match never {}
    }
}

/// Generates an instrumented async method per registry message: each issues the
/// `.ask(..)` under [`ROOM_REGISTRY_REPLY_TIMEOUT`], times it, and routes the
/// outcome through [`RoomRegistry::classify`]. Keeping this declarative avoids a
/// generic abstraction over kameo's message-specific request builders.
macro_rules! registry_method {
    (
        $(#[$meta:meta])*
        $name:ident ( $( $arg:ident : $arg_ty:ty ),* ) -> $reply:ty,
        $op:literal,
        $msg:expr
    ) => {
        $(#[$meta])*
        pub async fn $name(&self, $( $arg : $arg_ty ),* ) -> Result<$reply, RoomRegistryError> {
            let started = Instant::now();
            let result = self
                .inner
                .ask($msg)
                .mailbox_timeout(ROOM_REGISTRY_MAILBOX_TIMEOUT)
                .reply_timeout(ROOM_REGISTRY_REPLY_TIMEOUT)
                .await;
            Self::classify($op, started.elapsed(), result)
        }
    };
}

impl RoomRegistry {
    registry_method!(
        /// Look up a room actor by JID.
        get_room(room_jid: BareJid) -> Option<ActorRef<RoomActor>>,
        "get_room",
        GetRoom { room_jid }
    );

    registry_method!(
        /// Get an existing room or create one if absent.
        get_or_create_room(
            room_jid: BareJid,
            waddle_id: String,
            channel_id: String,
            config: RoomConfig
        ) -> ActorRef<RoomActor>,
        "get_or_create_room",
        GetOrCreateRoom { room_jid, waddle_id, channel_id, config }
    );

    registry_method!(
        /// Create a room, failing if one with the same JID already exists.
        create_room(
            room_jid: BareJid,
            waddle_id: String,
            channel_id: String,
            config: RoomConfig
        ) -> ActorRef<RoomActor>,
        "create_room",
        CreateRoom { room_jid, waddle_id, channel_id, config }
    );

    registry_method!(
        /// Create an instant room per XEP-0045.
        create_instant_room(room_jid: BareJid) -> ActorRef<RoomActor>,
        "create_instant_room",
        CreateInstantRoom { room_jid }
    );

    registry_method!(
        /// Destroy a room, returning whether it existed.
        destroy_room(room_jid: BareJid) -> bool,
        "destroy_room",
        DestroyRoom { room_jid }
    );

    registry_method!(
        /// Destroy a room only if it is still inactive at the expected
        /// occupancy revision (#1108). Returns whether it was destroyed.
        destroy_room_if_inactive(
            room_jid: BareJid,
            expected_occupancy_revision: u64,
            guard: SealGuard
        ) -> bool,
        "destroy_room_if_inactive",
        DestroyRoomIfInactive { room_jid, expected_occupancy_revision, guard }
    );

    registry_method!(
        /// Whether a room exists.
        room_exists(room_jid: BareJid) -> bool,
        "room_exists",
        RoomExists { room_jid }
    );

    registry_method!(
        /// Whether a bare JID belongs to this MUC service domain.
        is_muc_jid(jid: BareJid) -> bool,
        "is_muc_jid",
        IsMucJid { jid }
    );

    registry_method!(
        /// List all live room JIDs.
        list_rooms() -> Vec<BareJid>,
        "list_rooms",
        ListRooms
    );

    registry_method!(
        /// Count active rooms.
        room_count() -> usize,
        "room_count",
        RoomCount
    );

    /// Test-only: route a never-returning message through the same instrumented
    /// path as the public methods, so the reply-timeout → typed-error mapping is
    /// exercised against real wrapper code rather than a duplicated stub.
    #[cfg(test)]
    pub(crate) async fn hang_forever(&self) -> Result<(), RoomRegistryError> {
        let started = Instant::now();
        let result = self
            .inner
            .ask(super::room_registry_actor::HangForever)
            .mailbox_timeout(ROOM_REGISTRY_MAILBOX_TIMEOUT)
            .reply_timeout(ROOM_REGISTRY_REPLY_TIMEOUT)
            .await;
        Self::classify("hang_forever", started.elapsed(), result)
    }
}

/// A short, stable reason label for [`metrics::record_actor_request_dropped`].
///
/// Only the transport-failure variants reach this helper: [`RoomRegistry::classify`]
/// matches `HandlerError` and `Timeout` before its `Err(other)` arm calls this,
/// so those map through the catch-all rather than dedicated arms (which would be
/// dead code from the sole call site).
fn send_error_reason<M, E>(error: &SendError<M, E>) -> &'static str {
    match error {
        SendError::ActorNotRunning(_) => "actor_not_running",
        SendError::ActorStopped => "actor_stopped",
        SendError::MailboxFull(_) => "mailbox_full",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests;
