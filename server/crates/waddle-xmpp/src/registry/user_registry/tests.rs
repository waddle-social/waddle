use super::*;
use crate::registry::connection_registry::{ConnectionEntry, OutboundStanza};
use kameo::actor::Spawn;
use kameo::error::SendError;
use tokio::sync::mpsc;

/// A bounded outbound channel for a test registration. Returns the sender to
/// register and the receiver, which the caller keeps alive so the channel
/// does not report closed.
fn outbound_channel() -> (mpsc::Sender<OutboundStanza>, mpsc::Receiver<OutboundStanza>) {
    mpsc::channel(16)
}

fn bare(user: &str) -> BareJid {
    format!("{user}@example.com").parse().expect("valid JID")
}

fn full(user: &str, resource: &str) -> FullJid {
    format!("{user}@example.com/{resource}")
        .parse()
        .expect("valid JID")
}

async fn spawn_registry() -> ActorRef<UserRegistryActor> {
    UserRegistryActor::spawn(UserRegistryActor::new())
}

#[tokio::test]
async fn test_get_or_create_spawns_new_user() {
    let registry = spawn_registry().await;

    let actor_ref = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    // Asking again should return the same actor (by id).
    let actor_ref2 = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    assert_eq!(actor_ref.id(), actor_ref2.id());
}

#[tokio::test]
async fn test_get_user_returns_none_when_absent() {
    let registry = spawn_registry().await;

    let result = registry
        .ask(GetUser {
            bare_jid: bare("ghost"),
        })
        .await
        .expect("ask failed");

    assert!(result.is_none());
}

#[tokio::test]
async fn test_get_user_returns_some_after_create() {
    let registry = spawn_registry().await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    let result = registry
        .ask(GetUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    assert!(result.is_some());
}

#[tokio::test]
async fn test_remove_user() {
    let registry = spawn_registry().await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("carol"),
        })
        .await
        .expect("ask failed");

    let removed = registry
        .ask(RemoveUser {
            bare_jid: bare("carol"),
        })
        .await
        .expect("ask failed");

    assert!(removed);

    // Removing again should return false.
    let removed_again = registry
        .ask(RemoveUser {
            bare_jid: bare("carol"),
        })
        .await
        .expect("ask failed");

    assert!(!removed_again);
}

#[tokio::test]
async fn test_list_users() {
    let registry = spawn_registry().await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    let mut users = registry.ask(ListUsers).await.expect("ask failed");

    users.sort_by_key(|a| a.to_string());

    assert_eq!(users.len(), 2);
    assert_eq!(users[0], bare("alice"));
    assert_eq!(users[1], bare("bob"));
}

#[tokio::test]
async fn test_user_count() {
    let registry = spawn_registry().await;

    let count = registry.ask(UserCount).await.expect("ask failed");
    assert_eq!(count, 0);

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    let count = registry.ask(UserCount).await.expect("ask failed");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_different_users_get_different_actors() {
    let registry = spawn_registry().await;

    let alice = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    let bob = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    assert_ne!(alice.id(), bob.id());
}

#[tokio::test]
async fn test_get_or_create_fails_fast_for_dead_actor_until_explicit_cleanup() {
    let registry = spawn_registry().await;
    let bare_jid = bare("restart");

    let first = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("ask failed");
    first.kill();
    tokio::task::yield_now().await;

    let result = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));

    let removed = registry
        .ask(RemoveUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("remove should clear poisoned user");
    assert!(removed);

    let recreated = registry
        .ask(GetOrCreateUser { bare_jid })
        .await
        .expect("actor should be recreated after explicit cleanup");
    assert!(recreated.is_alive());
}

#[tokio::test]
async fn test_unregister_and_register_are_serialized_without_user_loss() {
    let registry = spawn_registry().await;
    let bare_jid = bare("alice");
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");

    let (phone_tx, _phone_rx) = outbound_channel();
    let (laptop_tx, _laptop_rx) = outbound_channel();

    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            entry: ConnectionEntry::new(phone_tx),
        })
        .await
        .expect("register phone");

    let unregister = registry.ask(UnregisterUserResource {
        jid: phone,
        owner: None,
    });
    let register = registry.ask(RegisterUserResource {
        jid: laptop.clone(),
        entry: ConnectionEntry::new(laptop_tx),
    });
    let (unregister_done, register_done) = tokio::join!(unregister, register);
    unregister_done.expect("unregister");
    register_done.expect("register replacement");

    let user_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("user actor should still exist with replacement resource");
    let resources = user_actor
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![laptop]);

    let count = registry.ask(UserCount).await.expect("count");
    assert_eq!(count, 1);
}

fn sample_stanza(to: &FullJid) -> crate::Stanza {
    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(to.clone())));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hi".to_string());
    crate::Stanza::Message(msg)
}

/// ADR-0017 Phase 1 Slice 2 (Copilot review on PR #1177): the delivery cutover
/// makes `try_deliver`'s closed-channel eviction reachable in production. When
/// it removes a user's *last* resource, the actor is left empty but still
/// registered (the explicit unregister-prune path did not run). The reaper must
/// remove such an orphaned empty actor.
#[tokio::test]
async fn test_reap_user_if_empty_removes_orphaned_empty_actor() {
    use crate::registry::connection_registry::BroadcastOutcome;
    use crate::registry::TrySendPeer;

    let registry = spawn_registry().await;
    let bare_jid = bare("alice");
    let phone = full("alice", "phone");

    let (phone_tx, phone_rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            entry: ConnectionEntry::new(phone_tx),
        })
        .await
        .expect("register phone");

    // Close the channel, then drive one delivery so `try_deliver` evicts the
    // last resource — exactly the production path that orphans an empty actor.
    drop(phone_rx);
    let user_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("actor exists");
    let outcome = user_actor
        .ask(TrySendPeer {
            jid: phone.clone(),
            stanza: sample_stanza(&phone),
        })
        .await
        .expect("try send");
    assert_eq!(outcome, BroadcastOutcome::DroppedClosed);

    // The actor is now empty but still registered.
    assert_eq!(registry.ask(UserCount).await.expect("count"), 1);

    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reap");
    assert!(reaped, "an empty orphaned actor must be reaped");
    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
    assert!(registry
        .ask(GetUser { bare_jid })
        .await
        .expect("get user")
        .is_none());
}

/// The reaper must never remove a user that still has a live resource — the
/// race the atomic check-and-remove guards against.
#[tokio::test]
async fn test_reap_user_if_empty_keeps_nonempty_actor() {
    let registry = spawn_registry().await;
    let bare_jid = bare("alice");
    let phone = full("alice", "phone");

    let (phone_tx, _phone_rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            entry: ConnectionEntry::new(phone_tx),
        })
        .await
        .expect("register phone");

    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reap");
    assert!(!reaped, "a user with a live resource must not be reaped");
    assert_eq!(registry.ask(UserCount).await.expect("count"), 1);
    let resources = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("actor still present")
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![phone]);
}

/// Reaping an unknown bare JID is a no-op that reports nothing reaped.
#[tokio::test]
async fn test_reap_user_if_empty_absent_is_false() {
    let registry = spawn_registry().await;
    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare("ghost"),
        })
        .await
        .expect("reap");
    assert!(!reaped);
    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
}

/// A dead `UserActor` is a state-lost condition, not an empty one: the reaper
/// must fold it into the poison path (so `poisoned_users` stays the single
/// source of dead-actor truth) and report nothing reaped, leaving
/// `GetOrCreateUser` failing fast until explicit cleanup.
#[tokio::test]
async fn test_reap_user_if_empty_poisons_dead_actor() {
    let registry = spawn_registry().await;
    let bare_jid = bare("restart");

    let actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("create");
    actor.kill();
    tokio::task::yield_now().await;

    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reap");
    assert!(!reaped, "a dead actor is poisoned, not reaped");

    // The dead actor is now poisoned: GetOrCreateUser fails fast until cleanup.
    let result = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));
}
