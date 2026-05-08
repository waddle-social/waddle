use super::*;
use kameo::error::SendError;

fn bare(user: &str) -> BareJid {
    format!("{user}@example.com").parse().expect("valid JID")
}

fn full(user: &str, resource: &str) -> FullJid {
    format!("{user}@example.com/{resource}")
        .parse()
        .expect("valid JID")
}

async fn spawn_registry() -> ActorRef<UserRegistryActor> {
    kameo::spawn(UserRegistryActor::new())
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

    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            carbons_enabled: false,
        })
        .await
        .expect("register phone");

    let unregister = registry.ask(UnregisterUserResource { jid: phone });
    let register = registry.ask(RegisterUserResource {
        jid: laptop.clone(),
        carbons_enabled: true,
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
