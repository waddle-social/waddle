use kameo::actor::{ActorRef, Spawn};
use tokio::sync::mpsc;

use super::*;
use crate::registry::connection_registry::ConnectionEntry;
use crate::registry::user_actor::UpdatePresence;
use crate::registry::user_registry::RegisterUserResource;

fn full(user: &str, resource: &str) -> FullJid {
    format!("{user}@example.com/{resource}").parse().unwrap()
}

fn bare(user: &str) -> BareJid {
    format!("{user}@example.com").parse().unwrap()
}

async fn spawn_registry() -> ActorRef<UserRegistryActor> {
    UserRegistryActor::spawn(UserRegistryActor::new())
}

/// Register a resource through the SAME `RegisterUserResource` path
/// production dual-registration uses, so `GetUser` resolves it exactly like
/// a live connection would.
async fn register(
    registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
) -> mpsc::Receiver<crate::registry::connection_registry::OutboundStanza> {
    let (tx, rx) = mpsc::channel(16);
    registry
        .ask(RegisterUserResource {
            jid,
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");
    rx
}

async fn make_available(registry: &ActorRef<UserRegistryActor>, jid: FullJid, priority: i8) {
    let user_actor = registry
        .ask(GetUser {
            bare_jid: jid.to_bare(),
        })
        .await
        .expect("get user")
        .expect("user actor exists");
    user_actor
        .ask(UpdatePresence {
            jid,
            available: true,
            priority,
        })
        .await
        .expect("update presence");
}

#[tokio::test]
async fn get_resources_for_user_returns_every_registered_resource() {
    let registry = spawn_registry().await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let _phone_rx = register(&registry, phone.clone()).await;
    let _laptop_rx = register(&registry, laptop.clone()).await;

    let mut resources = get_resources_for_user(&registry, &bare("alice")).await;
    resources.sort_by_key(|j| j.to_string());
    assert_eq!(resources, vec![laptop, phone]);
}

#[tokio::test]
async fn routing_fallback_excludes_closed_receivers_but_cleanup_inventory_retains_them() {
    let registry = spawn_registry().await;
    let closing = full("alice", "closing");
    let available = full("alice", "available");
    let bound = full("alice", "bound");
    let mut closing_rx = register(&registry, closing.clone()).await;
    let mut available_rx = register(&registry, available.clone()).await;
    let mut bound_rx = register(&registry, bound.clone()).await;
    make_available(&registry, closing.clone(), 10).await;
    make_available(&registry, available.clone(), 5).await;
    closing_rx.close();

    let mut routing = get_resources_for_user(&registry, &bare("alice")).await;
    routing.sort();
    assert_eq!(
        routing,
        vec![available.clone(), bound.clone()],
        "routing fallback retains open resources even before their first presence"
    );
    let mut registered = try_get_resources_for_user(&registry, &bare("alice"))
        .await
        .expect("cleanup inventory");
    registered.sort();
    assert_eq!(registered, vec![available, bound, closing]);

    available_rx.close();
    bound_rx.close();
    assert!(get_resources_for_user(&registry, &bare("alice"))
        .await
        .is_empty());
    assert_eq!(
        try_get_resources_for_user(&registry, &bare("alice"))
            .await
            .expect("unfinished cleanup inventory")
            .len(),
        3
    );
}

#[tokio::test]
async fn get_resources_for_user_empty_when_no_actor() {
    let registry = spawn_registry().await;
    assert!(get_resources_for_user(&registry, &bare("ghost"))
        .await
        .is_empty());
}

#[tokio::test]
async fn select_routable_prefers_top_priority_and_excludes_lower_positive() {
    // RFC 6121 §8.5.2.1.2: a lower positive priority is NOT a destination
    // when a strictly-higher priority is available.
    let registry = spawn_registry().await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let _phone_rx = register(&registry, phone.clone()).await;
    let _laptop_rx = register(&registry, laptop.clone()).await;
    make_available(&registry, phone.clone(), 5).await;
    make_available(&registry, laptop.clone(), 3).await;

    assert_eq!(
        select_routable_resources_for_user(&registry, &bare("alice")).await,
        vec![phone],
        "only the strictly-highest-priority resource is a destination"
    );
}

#[tokio::test]
async fn select_routable_ties_at_top_priority_route_to_all() {
    let registry = spawn_registry().await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let _phone_rx = register(&registry, phone.clone()).await;
    let _laptop_rx = register(&registry, laptop.clone()).await;
    make_available(&registry, phone.clone(), 5).await;
    make_available(&registry, laptop.clone(), 5).await;

    let mut selected = select_routable_resources_for_user(&registry, &bare("alice")).await;
    selected.sort_by_key(|j| j.to_string());
    assert_eq!(selected, vec![laptop, phone]);
}

#[tokio::test]
async fn select_routable_excludes_negative_priority_resource() {
    let registry = spawn_registry().await;
    let phone = full("alice", "phone");
    let bot = full("alice", "bot");
    let _phone_rx = register(&registry, phone.clone()).await;
    let _bot_rx = register(&registry, bot.clone()).await;
    make_available(&registry, phone.clone(), 1).await;
    make_available(&registry, bot.clone(), -1).await;

    assert_eq!(
        select_routable_resources_for_user(&registry, &bare("alice")).await,
        vec![phone],
        "negative-priority resource must be excluded"
    );
}

#[tokio::test]
async fn select_routable_empty_when_only_negative_priority() {
    let registry = spawn_registry().await;
    let bot = full("alice", "bot");
    let _bot_rx = register(&registry, bot.clone()).await;
    make_available(&registry, bot, -1).await;

    assert!(
        select_routable_resources_for_user(&registry, &bare("alice"))
            .await
            .is_empty(),
        "all-negative-priority selects nothing → offline fallback"
    );
}

#[tokio::test]
async fn select_routable_empty_when_no_actor() {
    let registry = spawn_registry().await;
    assert!(
        select_routable_resources_for_user(&registry, &bare("ghost"))
            .await
            .is_empty()
    );
}
