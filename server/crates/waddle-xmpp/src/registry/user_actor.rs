//! Per-user actor managing connection state and presence.
//!
//! One `UserActor` exists per bare JID. It owns all mutable per-user state
//! (connected resources, presence, pending subscriptions, carbons) and
//! processes messages sequentially, removing the need for external locking.

use std::collections::{HashMap, HashSet};

use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use tracing::debug;

use crate::connection::Stanza;
use crate::registry::connection_registry::PresenceState;

/// Actor that manages per-user connection state.
///
/// Each connected bare JID gets exactly one `UserActor`. The actor tracks all
/// connected resources, their presence, carbons state, and any pending
/// subscription stanzas that arrived while the user was offline.
#[derive(Actor)]
#[actor(mailbox = bounded(2048))]
pub struct UserActor {
    bare_jid: BareJid,
    resources: HashSet<FullJid>,
    presence_available: HashMap<FullJid, bool>,
    presence_priority: HashMap<FullJid, i8>,
    presence_states: HashMap<FullJid, PresenceState>,
    pending_subscriptions: Vec<Stanza>,
    carbons_enabled: HashMap<FullJid, bool>,
}

impl UserActor {
    /// Create a new `UserActor` for the given bare JID.
    pub fn new(bare_jid: BareJid) -> Self {
        Self {
            bare_jid,
            resources: HashSet::new(),
            presence_available: HashMap::new(),
            presence_priority: HashMap::new(),
            presence_states: HashMap::new(),
            pending_subscriptions: Vec::new(),
            carbons_enabled: HashMap::new(),
        }
    }

    /// The bare JID this actor manages.
    pub fn bare_jid(&self) -> &BareJid {
        &self.bare_jid
    }

    /// Remove all state associated with a resource.
    fn remove_resource(&mut self, jid: &FullJid) {
        self.resources.remove(jid);
        self.presence_available.remove(jid);
        self.presence_priority.remove(jid);
        self.presence_states.remove(jid);
        self.carbons_enabled.remove(jid);
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Register a new connection (resource) for this user.
pub struct RegisterConnection {
    pub jid: FullJid,
}

impl kameo::message::Message<RegisterConnection> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterConnection,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(jid = %msg.jid, bare = %self.bare_jid, "Registering connection");
        self.resources.insert(msg.jid.clone());
        self.presence_available.insert(msg.jid.clone(), false);
        self.presence_priority.insert(msg.jid.clone(), 0);
        self.carbons_enabled.insert(msg.jid, false);
    }
}

/// Register a new connection with explicit carbons initial state.
pub struct RegisterConnectionWithCarbons {
    pub jid: FullJid,
    pub carbons_enabled: bool,
}

impl kameo::message::Message<RegisterConnectionWithCarbons> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterConnectionWithCarbons,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(
            jid = %msg.jid,
            bare = %self.bare_jid,
            carbons = msg.carbons_enabled,
            "Registering connection with explicit carbons state"
        );
        self.resources.insert(msg.jid.clone());
        self.presence_available.insert(msg.jid.clone(), false);
        self.presence_priority.insert(msg.jid.clone(), 0);
        self.carbons_enabled.insert(msg.jid, msg.carbons_enabled);
    }
}

/// Unregister a connection (resource) for this user.
pub struct UnregisterConnection {
    pub jid: FullJid,
}

impl kameo::message::Message<UnregisterConnection> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UnregisterConnection,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(jid = %msg.jid, bare = %self.bare_jid, "Unregistering connection");
        self.remove_resource(&msg.jid);
    }
}

/// Unregister a connection and return whether the user has no resources left.
pub struct UnregisterConnectionAndReportEmpty {
    pub jid: FullJid,
}

impl kameo::message::Message<UnregisterConnectionAndReportEmpty> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: UnregisterConnectionAndReportEmpty,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(
            jid = %msg.jid,
            bare = %self.bare_jid,
            "Unregistering connection and checking emptiness"
        );
        self.remove_resource(&msg.jid);
        self.resources.is_empty()
    }
}

/// Get all connected resource JIDs.
pub struct GetResources;

impl kameo::message::Message<GetResources> for UserActor {
    type Reply = Vec<FullJid>;

    async fn handle(
        &mut self,
        _msg: GetResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.resources.iter().cloned().collect()
    }
}

/// Get available resources with their priorities.
pub struct GetAvailableResources;

impl kameo::message::Message<GetAvailableResources> for UserActor {
    type Reply = Vec<(FullJid, i8)>;

    async fn handle(
        &mut self,
        _msg: GetAvailableResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.resources
            .iter()
            .filter(|jid| self.presence_available.get(*jid).copied().unwrap_or(false))
            .map(|jid| {
                let priority = self.presence_priority.get(jid).copied().unwrap_or(0);
                (jid.clone(), priority)
            })
            .collect()
    }
}

/// Get all connected resources except a specific one.
pub struct GetOtherResources {
    pub exclude: FullJid,
}

impl kameo::message::Message<GetOtherResources> for UserActor {
    type Reply = Vec<FullJid>;

    async fn handle(
        &mut self,
        msg: GetOtherResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.resources
            .iter()
            .filter(|jid| **jid != msg.exclude)
            .cloned()
            .collect()
    }
}

/// Update presence availability and priority for a resource.
pub struct UpdatePresence {
    pub jid: FullJid,
    pub available: bool,
    pub priority: i8,
}

impl kameo::message::Message<UpdatePresence> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: UpdatePresence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.resources.contains(&msg.jid) {
            self.presence_available
                .insert(msg.jid.clone(), msg.available);
            self.presence_priority.insert(msg.jid, msg.priority);
            true
        } else {
            false
        }
    }
}

/// Update full presence state (show/status/priority) for a resource.
pub struct UpdatePresenceState {
    pub jid: FullJid,
    pub show: Option<String>,
    pub status: Option<String>,
    pub priority: i8,
}

impl kameo::message::Message<UpdatePresenceState> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UpdatePresenceState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.presence_states.insert(
            msg.jid,
            PresenceState {
                show: msg.show,
                status: msg.status,
                priority: msg.priority,
            },
        );
    }
}

/// Get the stored presence state for a resource.
pub struct GetPresenceState {
    pub jid: FullJid,
}

impl kameo::message::Message<GetPresenceState> for UserActor {
    type Reply = Option<PresenceState>;

    async fn handle(
        &mut self,
        msg: GetPresenceState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.presence_states.get(&msg.jid).cloned()
    }
}

/// Clear stored presence state for a resource.
pub struct ClearPresenceState {
    pub jid: FullJid,
}

impl kameo::message::Message<ClearPresenceState> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ClearPresenceState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.presence_states.remove(&msg.jid);
    }
}

/// Queue a subscription stanza for later delivery.
pub struct QueuePendingSubscription {
    pub stanza: Stanza,
}

impl kameo::message::Message<QueuePendingSubscription> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: QueuePendingSubscription,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.pending_subscriptions.push(msg.stanza);
    }
}

/// Drain and return all pending subscription stanzas.
pub struct DrainPendingSubscriptions;

impl kameo::message::Message<DrainPendingSubscriptions> for UserActor {
    type Reply = Vec<Stanza>;

    async fn handle(
        &mut self,
        _msg: DrainPendingSubscriptions,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        std::mem::take(&mut self.pending_subscriptions)
    }
}

/// Check if a specific resource is connected.
pub struct IsConnected {
    pub jid: FullJid,
}

impl kameo::message::Message<IsConnected> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsConnected,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.resources.contains(&msg.jid)
    }
}

/// Set carbons enabled/disabled for a resource.
pub struct SetCarbonsEnabled {
    pub jid: FullJid,
    pub enabled: bool,
}

impl kameo::message::Message<SetCarbonsEnabled> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SetCarbonsEnabled,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.carbons_enabled.insert(msg.jid, msg.enabled);
    }
}

/// Check if carbons is enabled for a resource.
pub struct IsCarbonsEnabled {
    pub jid: FullJid,
}

impl kameo::message::Message<IsCarbonsEnabled> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsCarbonsEnabled,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.carbons_enabled.get(&msg.jid).copied().unwrap_or(false)
    }
}

/// Get the number of connected resources.
pub struct ResourceCount;

impl kameo::message::Message<ResourceCount> for UserActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: ResourceCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.resources.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::ActorRef;
    use kameo::error::SendError;
    use kameo::request::TryMessageSend;
    use tokio::sync::oneshot;

    struct HoldMailboxUntilReleased {
        release_rx: oneshot::Receiver<()>,
    }

    impl kameo::message::Message<HoldMailboxUntilReleased> for UserActor {
        type Reply = ();

        async fn handle(
            &mut self,
            msg: HoldMailboxUntilReleased,
            _ctx: &mut Context<Self, Self::Reply>,
        ) -> Self::Reply {
            let _ = msg.release_rx.await;
        }
    }

    fn bare(user: &str) -> BareJid {
        format!("{user}@example.com").parse().expect("bare jid")
    }

    fn full(user: &str, resource: &str) -> FullJid {
        format!("{user}@example.com/{resource}")
            .parse()
            .expect("full jid")
    }

    async fn spawn_actor(user: &str) -> ActorRef<UserActor> {
        kameo::spawn(UserActor::new(bare(user)))
    }

    #[tokio::test]
    async fn test_register_and_resource_count() {
        let actor = spawn_actor("alice").await;

        actor
            .ask(RegisterConnection {
                jid: full("alice", "phone"),
            })
            .await
            .expect("register");

        let count: usize = actor.ask(ResourceCount).await.expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_unregister_cleans_up() {
        let actor = spawn_actor("alice").await;
        let jid = full("alice", "phone");

        actor
            .ask(RegisterConnection { jid: jid.clone() })
            .await
            .expect("register");

        actor
            .ask(UnregisterConnection { jid: jid.clone() })
            .await
            .expect("unregister");

        let count: usize = actor.ask(ResourceCount).await.expect("count");
        assert_eq!(count, 0);

        let connected: bool = actor.ask(IsConnected { jid }).await.expect("connected");
        assert!(!connected);
    }

    #[tokio::test]
    async fn test_unregister_and_report_empty_is_atomic_per_user_actor() {
        let actor = spawn_actor("alice").await;
        let jid = full("alice", "phone");

        actor
            .ask(RegisterConnection { jid: jid.clone() })
            .await
            .expect("register");

        let is_empty: bool = actor
            .ask(UnregisterConnectionAndReportEmpty { jid })
            .await
            .expect("unregister+check");
        assert!(is_empty);
    }

    #[tokio::test]
    async fn test_get_resources() {
        let actor = spawn_actor("alice").await;

        actor
            .ask(RegisterConnection {
                jid: full("alice", "phone"),
            })
            .await
            .expect("register");

        actor
            .ask(RegisterConnection {
                jid: full("alice", "laptop"),
            })
            .await
            .expect("register");

        let resources: Vec<FullJid> = actor.ask(GetResources).await.expect("resources");
        assert_eq!(resources.len(), 2);
    }

    #[tokio::test]
    async fn test_get_other_resources() {
        let actor = spawn_actor("alice").await;
        let phone = full("alice", "phone");
        let laptop = full("alice", "laptop");

        actor
            .ask(RegisterConnection { jid: phone.clone() })
            .await
            .expect("register");

        actor
            .ask(RegisterConnection {
                jid: laptop.clone(),
            })
            .await
            .expect("register");

        let others: Vec<FullJid> = actor
            .ask(GetOtherResources {
                exclude: phone.clone(),
            })
            .await
            .expect("others");

        assert_eq!(others.len(), 1);
        assert_eq!(others[0], laptop);
    }

    #[tokio::test]
    async fn test_presence_update_and_available_resources() {
        let actor = spawn_actor("alice").await;
        let phone = full("alice", "phone");
        let laptop = full("alice", "laptop");

        actor
            .ask(RegisterConnection { jid: phone.clone() })
            .await
            .expect("register");

        actor
            .ask(RegisterConnection {
                jid: laptop.clone(),
            })
            .await
            .expect("register");

        // Initially no resources are available
        let available: Vec<(FullJid, i8)> =
            actor.ask(GetAvailableResources).await.expect("available");
        assert!(available.is_empty());

        // Make phone available with priority 5
        let updated: bool = actor
            .ask(UpdatePresence {
                jid: phone.clone(),
                available: true,
                priority: 5,
            })
            .await
            .expect("update");
        assert!(updated);

        let available: Vec<(FullJid, i8)> =
            actor.ask(GetAvailableResources).await.expect("available");
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].0, phone);
        assert_eq!(available[0].1, 5);
    }

    #[tokio::test]
    async fn test_update_presence_missing_resource() {
        let actor = spawn_actor("alice").await;
        let missing = full("alice", "missing");

        let updated: bool = actor
            .ask(UpdatePresence {
                jid: missing,
                available: true,
                priority: 0,
            })
            .await
            .expect("update");
        assert!(!updated);
    }

    #[tokio::test]
    async fn test_presence_state() {
        let actor = spawn_actor("alice").await;
        let jid = full("alice", "phone");

        // No state before setting
        let state: Option<PresenceState> = actor
            .ask(GetPresenceState { jid: jid.clone() })
            .await
            .expect("get");
        assert!(state.is_none());

        // Set state
        actor
            .ask(UpdatePresenceState {
                jid: jid.clone(),
                show: Some("away".to_string()),
                status: Some("Gone fishing".to_string()),
                priority: 3,
            })
            .await
            .expect("update");

        let state: Option<PresenceState> = actor
            .ask(GetPresenceState { jid: jid.clone() })
            .await
            .expect("get");
        let state = state.expect("should have state");
        assert_eq!(state.show.as_deref(), Some("away"));
        assert_eq!(state.status.as_deref(), Some("Gone fishing"));
        assert_eq!(state.priority, 3);

        // Clear state
        actor
            .ask(ClearPresenceState { jid: jid.clone() })
            .await
            .expect("clear");

        let state: Option<PresenceState> = actor.ask(GetPresenceState { jid }).await.expect("get");
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn test_pending_subscriptions() {
        let actor = spawn_actor("alice").await;

        let subscribe =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
        actor
            .ask(QueuePendingSubscription {
                stanza: Stanza::Presence(subscribe),
            })
            .await
            .expect("queue");

        let drained: Vec<Stanza> = actor.ask(DrainPendingSubscriptions).await.expect("drain");
        assert_eq!(drained.len(), 1);

        // Second drain should be empty
        let drained: Vec<Stanza> = actor.ask(DrainPendingSubscriptions).await.expect("drain");
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn test_carbons() {
        let actor = spawn_actor("alice").await;
        let jid = full("alice", "phone");

        actor
            .ask(RegisterConnection { jid: jid.clone() })
            .await
            .expect("register");

        // Default is disabled
        let enabled: bool = actor
            .ask(IsCarbonsEnabled { jid: jid.clone() })
            .await
            .expect("check");
        assert!(!enabled);

        // Enable
        actor
            .ask(SetCarbonsEnabled {
                jid: jid.clone(),
                enabled: true,
            })
            .await
            .expect("set");

        let enabled: bool = actor.ask(IsCarbonsEnabled { jid }).await.expect("check");
        assert!(enabled);
    }

    #[tokio::test]
    async fn test_mailbox_backpressure_marks_best_effort_as_dropped() {
        let actor = spawn_actor("alice").await;
        assert_eq!(actor.max_capacity(), 2048);

        let (release_tx, release_rx) = oneshot::channel();
        actor
            .tell(HoldMailboxUntilReleased { release_rx })
            .await
            .expect("hold message should enqueue");

        let jid = full("alice", "phone");
        let mut saw_mailbox_full = false;
        for _ in 0..(actor.max_capacity() * 2) {
            let send_result = actor
                .tell(SetCarbonsEnabled {
                    jid: jid.clone(),
                    enabled: true,
                })
                .try_send()
                .await;
            if matches!(send_result, Err(SendError::MailboxFull(_))) {
                saw_mailbox_full = true;
                break;
            }
            send_result.expect("best-effort tell should either enqueue or return MailboxFull");
        }
        assert!(
            saw_mailbox_full,
            "bounded mailbox should eventually apply backpressure"
        );

        let _ = release_tx.send(());
    }
}
