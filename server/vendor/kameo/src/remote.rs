//! # Remote Actors in Kameo
//!
//! The `remote` module in Kameo provides tools for managing distributed actors across nodes,
//! enabling actors to communicate seamlessly in a peer-to-peer (P2P) network. By leveraging
//! the [libp2p](https://libp2p.io) library, Kameo allows you to register actors under unique
//! names and send messages between actors on different nodes as though they were local.
//!
//! ## Key Features
//!
//! - **Composable Architecture**: The [`Behaviour`] struct implements libp2p's `NetworkBehaviour`,
//!   allowing seamless integration with existing libp2p applications and other protocols.
//! - **Actor Registration & Discovery**: Actors can be registered under unique names and looked up
//!   across the network using [`RemoteActorRef`](crate::actor::RemoteActorRef).
//! - **Reliable Messaging**: Ensures reliable message delivery between nodes using a combination
//!   of Kademlia DHT for discovery and request-response protocols for communication.
//! - **Modular Design**: Separate [`messaging`] and [`registry`] modules handle different aspects
//!   of distributed actor communication.
//!
//! ## Getting Started
//!
//! Compose the actor behaviour into the application-owned swarm:
//!
//! ```no_run
//! use kameo::remote;
//! use libp2p::swarm::NetworkBehaviour;
//!
//! #[derive(NetworkBehaviour)]
//! struct MyBehaviour {
//!     kameo: remote::Behaviour,
//!     // Add other libp2p behaviors as needed
//! }
//!
//! // Create custom libp2p swarm with full control over
//! // transports, discovery, and protocol composition
//! ```

use std::{
    any,
    collections::HashMap,
    str,
    sync::{Arc, LazyLock},
};

use tokio::sync::Mutex;

use crate::{
    Actor,
    actor::{ActorId, ActorRef, WeakActorRef},
    error::{RegistryError, RemoteSendError},
    links::Links,
    mailbox::SignalMailbox,
};

#[doc(hidden)]
pub mod _internal;
mod behaviour;
pub mod messaging;
pub mod registry;
mod swarm;

pub use behaviour::*;
pub use swarm::*;

pub(crate) static REMOTE_REGISTRY: LazyLock<Mutex<HashMap<ActorId, RemoteRegistryActorRef>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct RemoteRegistryActorRef {
    actor_ref: BoxRegisteredActorRef,
    pub(crate) name: Option<Arc<str>>,
    pub(crate) signal_mailbox: Box<dyn SignalMailbox>,
    pub(crate) links: Links,
}

impl RemoteRegistryActorRef {
    pub(crate) fn new<A: Actor>(actor_ref: ActorRef<A>, name: Option<Arc<str>>) -> Self {
        let signal_mailbox = actor_ref.weak_signal_mailbox();
        let links = actor_ref.links.clone();
        RemoteRegistryActorRef {
            actor_ref: BoxRegisteredActorRef::Strong(Box::new(actor_ref)),
            name,
            signal_mailbox,
            links,
        }
    }

    pub(crate) fn new_weak<A: Actor>(actor_ref: WeakActorRef<A>, name: Option<Arc<str>>) -> Self {
        let signal_mailbox = actor_ref.weak_signal_mailbox();
        let links = actor_ref.links.clone();
        RemoteRegistryActorRef {
            actor_ref: BoxRegisteredActorRef::Weak(Box::new(actor_ref)),
            name,
            signal_mailbox,
            links,
        }
    }

    pub(crate) fn downcast<A: Actor>(
        &self,
    ) -> Result<ActorRef<A>, DowncastRegsiteredActorRefError> {
        match &self.actor_ref {
            BoxRegisteredActorRef::Strong(any) => any
                .downcast_ref::<ActorRef<A>>()
                .ok_or(DowncastRegsiteredActorRefError::BadActorType)
                .cloned(),
            BoxRegisteredActorRef::Weak(any) => any
                .downcast_ref::<WeakActorRef<A>>()
                .ok_or(DowncastRegsiteredActorRefError::BadActorType)?
                .upgrade()
                .ok_or(DowncastRegsiteredActorRefError::ActorNotRunning),
        }
    }
}

pub(crate) enum DowncastRegsiteredActorRefError {
    BadActorType,
    ActorNotRunning,
}

impl<E> From<DowncastRegsiteredActorRefError> for RemoteSendError<E> {
    fn from(err: DowncastRegsiteredActorRefError) -> Self {
        match err {
            DowncastRegsiteredActorRefError::BadActorType => RemoteSendError::BadActorType,
            DowncastRegsiteredActorRefError::ActorNotRunning => RemoteSendError::ActorNotRunning,
        }
    }
}

pub(crate) enum BoxRegisteredActorRef {
    Strong(Box<dyn any::Any + Send + Sync>),
    Weak(Box<dyn any::Any + Send + Sync>),
}

/// `RemoteActor` is a trait for identifying actors remotely.
///
/// Each remote actor must implement this trait and provide a unique identifier string (`REMOTE_ID`).
/// The identifier is essential to distinguish between different actor types during remote communication.
///
/// ## Example with Derive
///
/// ```
/// use kameo::{Actor, RemoteActor};
///
/// #[derive(Actor, RemoteActor)]
/// pub struct MyActor;
/// ```
///
/// ## Example Manual Implementation
///
/// ```
/// use kameo::remote::RemoteActor;
///
/// pub struct MyActor;
///
/// impl RemoteActor for MyActor {
///     const REMOTE_ID: &'static str = "my_actor_id";
/// }
/// ```
pub trait RemoteActor {
    /// The remote identifier string.
    const REMOTE_ID: &'static str;
}

/// `RemoteMessage` is a trait for identifying messages that are sent between remote actors.
///
/// Each remote message type must implement this trait and provide a unique identifier string (`REMOTE_ID`).
/// The unique ID ensures that each message type is recognized correctly during message passing between nodes.
///
/// This trait is typically implemented automatically with the [`#[remote_message]`](crate::remote_message) macro.
pub trait RemoteMessage<M> {
    /// The remote identifier string.
    const REMOTE_ID: &'static str;
}

/// Unregisters an actor within the swarm.
///
/// This will only unregister an actor previously registered by the current node.
pub async fn unregister(name: impl Into<Arc<str>>) -> Result<(), RegistryError> {
    ActorSwarm::get()
        .ok_or(RegistryError::SwarmNotBootstrapped)?
        .unregister(name.into())
        .await;
    Ok(())
}
