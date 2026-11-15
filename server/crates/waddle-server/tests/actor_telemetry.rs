//! The uniform `waddle.actor.*` instruments, driven through the real kameo
//! observer hook.

use std::convert::Infallible;
use std::time::Duration;

use kameo::actor::{ActorRef, Spawn};
use kameo::message::{Context, Message};
use kameo::Actor;
use waddle_xmpp::telemetry::attributes::ActorKind;
use waddle_xmpp::telemetry::test_support;

use waddle_server::actor_telemetry::{actor_kind, install};

struct Probe;

impl Actor for Probe {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(args: Self, _: ActorRef<Self>) -> Result<Self, Infallible> {
        Ok(args)
    }
}

struct Work;

impl Message<Work> for Probe {
    type Reply = ();

    async fn handle(&mut self, _: Work, _: &mut Context<Self, Self::Reply>) {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

struct Explode;

impl Message<Explode> for Probe {
    type Reply = ();

    async fn handle(&mut self, _: Explode, _: &mut Context<Self, Self::Reply>) {
        panic!("probe handler panic");
    }
}

struct Fail;

impl Message<Fail> for Probe {
    type Reply = Result<(), std::io::Error>;

    async fn handle(&mut self, _: Fail, _: &mut Context<Self, Self::Reply>) -> Self::Reply {
        Err(std::io::Error::other("probe handler error"))
    }
}

struct SlowStart;

impl Actor for SlowStart {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(args: Self, _: ActorRef<Self>) -> Result<Self, Infallible> {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(args)
    }
}

impl Message<Work> for SlowStart {
    type Reply = ();

    async fn handle(&mut self, _: Work, _: &mut Context<Self, Self::Reply>) {}
}

struct NeverStarts;

impl Actor for NeverStarts {
    type Args = Self;
    type Error = std::io::Error;

    async fn on_start(_: Self, _: ActorRef<Self>) -> Result<Self, Self::Error> {
        Err(std::io::Error::other("probe start error"))
    }
}

const OTHER: &[(&str, &str)] = &[("actor", "other")];

#[tokio::test]
async fn every_handled_message_reports_queue_wait_and_handler_time() {
    let metrics = test_support::acquire().await;
    install();

    let probe = Probe::spawn(Probe);
    probe.ask(Work).await.expect("first ask");
    probe.ask(Work).await.expect("second ask");

    assert_eq!(
        metrics.histogram_count("waddle.actor.message.queue_wait", OTHER),
        Some(2)
    );
    assert_eq!(
        metrics.histogram_count("waddle.actor.message.handle.duration", OTHER),
        Some(2)
    );
    assert_eq!(
        metrics
            .histogram_bounds("waddle.actor.message.handle.duration")
            .and_then(|bounds| bounds.first().copied()),
        Some(0.0001),
        "handlers finish well under a millisecond, so the lowest bucket must be finer"
    );
}

#[tokio::test]
async fn a_clean_stop_is_counted_apart_from_a_panic() {
    let metrics = test_support::acquire().await;
    install();

    let clean = Probe::spawn(Probe);
    clean.wait_for_startup().await;
    clean.stop_gracefully().await.expect("stop signal");
    clean.wait_for_shutdown().await;

    let crashing = Probe::spawn(Probe);
    let _ = crashing.ask(Explode).await;
    crashing.wait_for_shutdown().await;

    assert_eq!(metrics.counter_sum("waddle.actor.started", OTHER), Some(2));
    assert_eq!(
        metrics.counter_sum(
            "waddle.actor.stopped",
            &[("actor", "other"), ("reason", "stopped")]
        ),
        Some(1)
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.actor.stopped",
            &[("actor", "other"), ("reason", "panicked")]
        ),
        Some(1)
    );
}

fn name_of<A: Actor>() -> &'static str {
    A::name()
}

#[tokio::test]
async fn an_unanswered_handler_error_counts_as_a_panicked_stop() {
    let metrics = test_support::acquire().await;
    install();

    let probe = Probe::spawn(Probe);
    probe.tell(Fail).await.expect("tell");
    probe.wait_for_shutdown().await;

    assert_eq!(
        metrics.counter_sum(
            "waddle.actor.stopped",
            &[("actor", "other"), ("reason", "panicked")]
        ),
        Some(1)
    );
}

#[tokio::test]
async fn a_message_sent_during_startup_waits_out_on_start() {
    let metrics = test_support::acquire().await;
    install();

    let actor = SlowStart::spawn(SlowStart);
    actor.ask(Work).await.expect("ask");

    assert_eq!(
        metrics.histogram_count("waddle.actor.message.queue_wait", OTHER),
        Some(1)
    );
    let waited = metrics
        .histogram_sum("waddle.actor.message.queue_wait", OTHER)
        .expect("queue wait sum");
    assert!(
        waited >= 0.1,
        "the wait must include the 150 ms on_start, got {waited}"
    );
}

#[tokio::test]
async fn a_scheduled_send_does_not_count_its_delay_as_queue_wait() {
    let metrics = test_support::acquire().await;
    install();

    let probe = Probe::spawn(Probe);
    probe.wait_for_startup().await;
    probe
        .tell(Work)
        .send_after(Duration::from_millis(150))
        .await
        .expect("scheduled task")
        .expect("scheduled send");
    probe
        .ask(Work)
        .await
        .expect("ask after the scheduled message");

    let waited = metrics
        .histogram_sum("waddle.actor.message.queue_wait", OTHER)
        .expect("queue wait sum");
    assert!(
        waited < 0.1,
        "the 150 ms timer is not mailbox congestion, got {waited}"
    );
}

#[tokio::test]
async fn an_actor_that_fails_to_start_is_neither_started_nor_stopped() {
    let metrics = test_support::acquire().await;
    install();

    let failed = NeverStarts::spawn(NeverStarts);
    failed.wait_for_shutdown().await;
    // A started probe proves the instruments export at all.
    let probe = Probe::spawn(Probe);
    probe.wait_for_startup().await;

    assert_eq!(metrics.counter_sum("waddle.actor.started", OTHER), Some(1));
    assert_eq!(metrics.counter_sum("waddle.actor.stopped", OTHER), None);
}

#[test]
fn production_actors_map_to_their_own_kind() {
    use waddle_xmpp::muc::room_actor::RoomActor;
    use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
    use waddle_xmpp::registry::user_actor::UserActor;
    use waddle_xmpp::registry::UserRegistryActor;

    let mapped = [
        (name_of::<RoomRegistryActor>(), ActorKind::RoomRegistry),
        (name_of::<RoomActor>(), ActorKind::Room),
        (name_of::<UserRegistryActor>(), ActorKind::UserRegistry),
        (name_of::<UserActor>(), ActorKind::User),
        (
            name_of::<waddle_server::permissions::actor::PermissionActor>(),
            ActorKind::Permission,
        ),
        (
            name_of::<waddle_server::db::actor::DbActor>(),
            ActorKind::Db,
        ),
        #[cfg(feature = "clustering")]
        (
            name_of::<waddle_server::clustering::relay::RelayActor>(),
            ActorKind::Relay,
        ),
        // The two private actors assert their own mapping beside their types.
    ];
    for (name, kind) in mapped {
        assert_eq!(actor_kind(name), kind, "{name}");
    }
    assert_eq!(actor_kind("some::other::ProbeActor"), ActorKind::Other);
    assert_eq!(
        actor_kind("a::RoomActor<b::Storage>"),
        ActorKind::Room,
        "a type parameter must not hide the actor's own name"
    );
}
