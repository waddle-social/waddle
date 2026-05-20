//! User Registry Actor.
//!
//! A Kameo actor that maps bare JIDs to per-user `UserActor` instances.
//! One `UserRegistryActor` exists for the entire server, replacing the
//! DashMap-based lookup portion of `ConnectionRegistry` for user-level
//! concerns (Phase 2 of the actor-model migration).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use jid::{BareJid, FullJid};
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::message::Context;
use kameo::Actor;
use thiserror::Error;
use tracing::{debug, info};

use super::user_actor::{
    RegisterConnectionWithCarbons, UnregisterConnectionAndReportEmpty, UserActor,
};
use crate::metrics;

const CHILD_ACTOR_TIMEOUT: Duration = Duration::from_secs(2);

/// Server-wide registry that maps bare JIDs to their `UserActor`.
///
/// All mutations are serialised through the actor mailbox, so no
/// external synchronisation is required.
#[derive(Actor)]
pub struct UserRegistryActor {
    users: HashMap<BareJid, ActorRef<UserActor>>,
    poisoned_users: HashSet<BareJid>,
}

impl UserRegistryActor {
    /// Create an empty registry.
    pub fn new() -> Self {
        info!("Creating user registry actor");
        Self {
            users: HashMap::new(),
            poisoned_users: HashSet::new(),
        }
    }

    fn spawn_user_actor(&mut self, bare_jid: BareJid) -> ActorRef<UserActor> {
        let actor = UserActor::new(bare_jid.clone());
        let actor_ref = UserActor::spawn(actor);
        self.users.insert(bare_jid, actor_ref.clone());
        actor_ref
    }

    fn mark_actor_state_lost(&mut self, bare_jid: &BareJid) -> UserRegistryError {
        self.users.remove(bare_jid);
        self.poisoned_users.insert(bare_jid.clone());
        metrics::record_actor_restart("user_actor", "detected_dead_actor_fail_fast");
        UserRegistryError::UserActorStateLost(bare_jid.clone())
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

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum UserRegistryError {
    #[error("user actor state for {0} was lost; explicit cleanup required before recreating")]
    UserActorStateLost(BareJid),
    #[error("user actor for {0} is temporarily overloaded")]
    UserActorBusy(BareJid),
}

/// Return the `UserActor` for the given bare JID, spawning one if it does not
/// already exist.
pub struct GetOrCreateUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetOrCreateUser> for UserRegistryActor {
    type Reply = Result<ActorRef<UserActor>, UserRegistryError>;

    async fn handle(
        &mut self,
        msg: GetOrCreateUser,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.poisoned_users.contains(&msg.bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(msg.bare_jid));
        }

        if let Some(actor_ref) = self.users.get(&msg.bare_jid) {
            if actor_ref.is_alive() {
                debug!(jid = %msg.bare_jid, "Returning existing UserActor");
                return Ok(actor_ref.clone());
            }
            debug!(jid = %msg.bare_jid, "Detected dead UserActor; failing fast");
            return Err(self.mark_actor_state_lost(&msg.bare_jid));
        }

        debug!(jid = %msg.bare_jid, "Spawning new UserActor");
        let actor_ref = self.spawn_user_actor(msg.bare_jid);
        Ok(actor_ref)
    }
}

/// Look up an existing `UserActor` without creating one.
pub struct GetUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetUser> for UserRegistryActor {
    type Reply = Result<Option<ActorRef<UserActor>>, UserRegistryError>;

    async fn handle(&mut self, msg: GetUser, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        if self.poisoned_users.contains(&msg.bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(msg.bare_jid));
        }
        if let Some(actor_ref) = self.users.get(&msg.bare_jid) {
            if actor_ref.is_alive() {
                return Ok(Some(actor_ref.clone()));
            }
            debug!(jid = %msg.bare_jid, "Detected dead UserActor; failing fast");
            return Err(self.mark_actor_state_lost(&msg.bare_jid));
        }
        Ok(None)
    }
}

/// Register a user resource through the registry actor, serializing user lifecycle mutations.
pub struct RegisterUserResource {
    pub jid: FullJid,
    pub carbons_enabled: bool,
}

impl kameo::message::Message<RegisterUserResource> for UserRegistryActor {
    type Reply = Result<(), UserRegistryError>;

    async fn handle(
        &mut self,
        msg: RegisterUserResource,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        let user_actor = if let Some(actor_ref) = self.users.get(&bare_jid) {
            if actor_ref.is_alive() {
                actor_ref.clone()
            } else {
                return Err(self.mark_actor_state_lost(&bare_jid));
            }
        } else {
            self.spawn_user_actor(bare_jid.clone())
        };

        match user_actor
            .ask(RegisterConnectionWithCarbons {
                jid: msg.jid.clone(),
                carbons_enabled: msg.carbons_enabled,
            })
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(()) => {}
            Err(SendError::MailboxFull(_) | SendError::Timeout(_)) => {
                return Err(UserRegistryError::UserActorBusy(bare_jid));
            }
            Err(_) => return Err(self.mark_actor_state_lost(&bare_jid)),
        }

        Ok(())
    }
}

/// Unregister a user resource atomically in the actor-owned path and prune empty users.
pub struct UnregisterUserResource {
    pub jid: FullJid,
}

impl kameo::message::Message<UnregisterUserResource> for UserRegistryActor {
    type Reply = Result<(), UserRegistryError>;

    async fn handle(
        &mut self,
        msg: UnregisterUserResource,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        let Some(user_actor) = self.users.get(&bare_jid).cloned() else {
            return Ok(());
        };
        if !user_actor.is_alive() {
            return Err(self.mark_actor_state_lost(&bare_jid));
        }

        let is_empty = match user_actor
            .ask(UnregisterConnectionAndReportEmpty { jid: msg.jid })
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(is_empty) => is_empty,
            Err(SendError::MailboxFull(_) | SendError::Timeout(_)) => {
                return Err(UserRegistryError::UserActorBusy(bare_jid));
            }
            Err(_) => return Err(self.mark_actor_state_lost(&bare_jid)),
        };

        if is_empty {
            self.users.remove(&bare_jid);
            self.poisoned_users.remove(&bare_jid);
        }

        Ok(())
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
        let cleared_poison = self.poisoned_users.remove(&msg.bare_jid);
        if removed {
            debug!(jid = %msg.bare_jid, "Removed user from registry");
        }
        removed || cleared_poison
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
mod tests;
