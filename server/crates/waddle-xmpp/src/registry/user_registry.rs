//! User Registry Actor.
//!
//! A Kameo actor that maps bare JIDs to per-user `UserActor` instances.
//! One `UserRegistryActor` exists for the entire server, replacing the
//! DashMap-based lookup portion of `ConnectionRegistry` for user-level
//! concerns (Phase 2 of the actor-model migration).

use std::collections::HashMap;
use std::convert::Infallible;

use jid::BareJid;
use kameo::actor::ActorRef;
use kameo::message::Context;
use kameo::Actor;
use tracing::{debug, info};

use super::user_actor::UserActor;

/// Server-wide registry that maps bare JIDs to their `UserActor`.
///
/// All mutations are serialised through the actor mailbox, so no
/// external synchronisation is required.
#[derive(Actor)]
pub struct UserRegistryActor {
    users: HashMap<BareJid, ActorRef<UserActor>>,
}

impl UserRegistryActor {
    /// Create an empty registry.
    pub fn new() -> Self {
        info!("Creating user registry actor");
        Self {
            users: HashMap::new(),
        }
    }
}

impl Default for UserRegistryActor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Return the `UserActor` for the given bare JID, spawning one if it does not
/// already exist.
pub struct GetOrCreateUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetOrCreateUser> for UserRegistryActor {
    type Reply = Result<ActorRef<UserActor>, Infallible>;

    async fn handle(
        &mut self,
        msg: GetOrCreateUser,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.users.get(&msg.bare_jid) {
            debug!(jid = %msg.bare_jid, "Returning existing UserActor");
            return Ok(actor_ref.clone());
        }

        debug!(jid = %msg.bare_jid, "Spawning new UserActor");
        let actor = UserActor::new(msg.bare_jid.clone());
        let actor_ref = kameo::spawn(actor);
        self.users.insert(msg.bare_jid, actor_ref.clone());
        Ok(actor_ref)
    }
}

/// Look up an existing `UserActor` without creating one.
pub struct GetUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetUser> for UserRegistryActor {
    type Reply = Result<Option<ActorRef<UserActor>>, Infallible>;

    async fn handle(&mut self, msg: GetUser, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        Ok(self.users.get(&msg.bare_jid).cloned())
    }
}

/// Remove a user from the registry.
///
/// Returns `true` if the user was present and removed.
pub struct RemoveUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<RemoveUser> for UserRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RemoveUser,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let removed = self.users.remove(&msg.bare_jid).is_some();
        if removed {
            debug!(jid = %msg.bare_jid, "Removed user from registry");
        }
        removed
    }
}

/// List all bare JIDs that currently have a `UserActor`.
pub struct ListUsers;

impl kameo::message::Message<ListUsers> for UserRegistryActor {
    type Reply = Vec<BareJid>;

    async fn handle(
        &mut self,
        _msg: ListUsers,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.users.keys().cloned().collect()
    }
}

/// Return the number of tracked users.
pub struct UserCount;

impl kameo::message::Message<UserCount> for UserRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: UserCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.users.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(user: &str) -> BareJid {
        format!("{user}@example.com").parse().expect("valid JID")
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
}
