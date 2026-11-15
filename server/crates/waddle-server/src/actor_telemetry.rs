//! Bridges kameo's actor observer to the uniform `waddle.actor.*` instruments.
//!
//! [`install`] runs once, after the meter provider is set: an observation
//! recorded earlier would bind its instrument to the noop provider.

use std::time::Duration;

use kameo::error::ActorStopReason;
use kameo::observer::ActorObserver;
use waddle_xmpp::telemetry::actor::{
    record_actor_message, record_actor_started, record_actor_stopped,
};
use waddle_xmpp::telemetry::attributes::{ActorKind, ActorStopKind};

struct TelemetryObserver;

static OBSERVER: TelemetryObserver = TelemetryObserver;

impl ActorObserver for TelemetryObserver {
    fn actor_started(&self, actor: &'static str) {
        record_actor_started(actor_kind(actor));
    }

    fn actor_stopped(&self, actor: &'static str, reason: &ActorStopReason) {
        let reason = match reason {
            ActorStopReason::Panicked(_) => ActorStopKind::Panicked,
            _ => ActorStopKind::Stopped,
        };
        record_actor_stopped(actor_kind(actor), reason);
    }

    fn message_handled(
        &self,
        actor: &'static str,
        queue_wait: Duration,
        handle_duration: Duration,
    ) {
        record_actor_message(actor_kind(actor), queue_wait, handle_duration);
    }
}

/// Installs the process-wide actor observer. Idempotent.
pub fn install() {
    kameo::observer::set_actor_observer(&OBSERVER);
}

/// Maps `Actor::name()` to its allowlisted kind by the type's own name, so
/// moving an actor between modules does not change its label. The name is a
/// full type path by default and the bare type name under `#[derive(Actor)]`.
pub fn actor_kind(name: &str) -> ActorKind {
    let type_name = name.split('<').next().unwrap_or(name);
    match type_name.rsplit("::").next().unwrap_or(type_name) {
        "RoomRegistryActor" => ActorKind::RoomRegistry,
        "RoomActor" => ActorKind::Room,
        "UserRegistryActor" => ActorKind::UserRegistry,
        "UserActor" => ActorKind::User,
        "PermissionActor" => ActorKind::Permission,
        "AccountCoordinatorActor" => ActorKind::AccountCoordinator,
        "DbActor" => ActorKind::Db,
        "RelayActor" => ActorKind::Relay,
        "LiveConnectionActor" => ActorKind::Connection,
        _ => ActorKind::Other,
    }
}
