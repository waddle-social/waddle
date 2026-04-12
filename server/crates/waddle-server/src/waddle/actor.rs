//! Kameo actor representing a single Waddle community.
//!
//! The `WaddleActor` supervises one waddle (community). It tracks the set of
//! channels (rooms) that belong to this waddle and holds a reference to the
//! waddle's `DbActor` for database operations.
//!
//! This is Phase 3 of the actor-model migration: one `WaddleActor` per
//! community, sitting between the global registry and the per-room actors
//! that live in waddle-xmpp's `RoomRegistryActor`.

use std::collections::HashSet;

use kameo::actor::ActorRef;
use kameo::message::Context;
use kameo::Actor;

use crate::db::actor::DbActor;

// ---------------------------------------------------------------------------
// Reply wrappers
// ---------------------------------------------------------------------------

/// Newtype wrapper around `ActorRef<DbActor>` that implements `kameo::Reply`.
#[derive(Clone, kameo::Reply)]
pub struct DbActorRef(pub ActorRef<DbActor>);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for a Waddle community.
#[derive(Debug, Clone, Default, kameo::Reply)]
pub struct WaddleConfig {
    /// Human-readable name of the waddle.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Whether this waddle is publicly discoverable.
    pub is_public: bool,
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Actor representing a single Waddle community.
///
/// Each waddle has its own `DbActor` for database isolation. The set of
/// channel IDs is maintained here; the actual room actors are managed by
/// waddle-xmpp's `RoomRegistryActor` (cross-crate boundary).
#[derive(Actor)]
pub struct WaddleActor {
    /// Unique identifier for this waddle community.
    waddle_id: String,
    /// Database actor scoped to this waddle.
    db_actor: ActorRef<DbActor>,
    /// Channel IDs belonging to this waddle.
    channels: HashSet<String>,
    /// Waddle configuration (name, description, visibility).
    config: WaddleConfig,
}

impl WaddleActor {
    /// Create a new `WaddleActor`.
    pub fn new(waddle_id: String, db_actor: ActorRef<DbActor>, config: WaddleConfig) -> Self {
        Self {
            waddle_id,
            db_actor,
            channels: HashSet::new(),
            config,
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Retrieve the current waddle configuration.
pub struct GetConfig;

impl kameo::message::Message<GetConfig> for WaddleActor {
    type Reply = WaddleConfig;

    async fn handle(
        &mut self,
        _msg: GetConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.config.clone()
    }
}

/// Replace the waddle configuration.
pub struct UpdateConfig {
    pub config: WaddleConfig,
}

impl kameo::message::Message<UpdateConfig> for WaddleActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UpdateConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.config = msg.config;
    }
}

/// Register a channel (room) as belonging to this waddle.
pub struct RegisterChannel {
    pub channel_id: String,
}

impl kameo::message::Message<RegisterChannel> for WaddleActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterChannel,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.channels.insert(msg.channel_id);
    }
}

/// Remove a channel from this waddle.
pub struct UnregisterChannel {
    pub channel_id: String,
}

impl kameo::message::Message<UnregisterChannel> for WaddleActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UnregisterChannel,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.channels.remove(&msg.channel_id);
    }
}

/// List all channel IDs registered to this waddle.
pub struct ListChannels;

impl kameo::message::Message<ListChannels> for WaddleActor {
    type Reply = Vec<String>;

    async fn handle(
        &mut self,
        _msg: ListChannels,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.channels.iter().cloned().collect()
    }
}

/// Retrieve the waddle's unique ID.
pub struct GetWaddleId;

impl kameo::message::Message<GetWaddleId> for WaddleActor {
    type Reply = String;

    async fn handle(
        &mut self,
        _msg: GetWaddleId,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.waddle_id.clone()
    }
}

/// Retrieve a reference to the waddle's database actor.
pub struct GetDbActor;

impl kameo::message::Message<GetDbActor> for WaddleActor {
    type Reply = DbActorRef;

    async fn handle(
        &mut self,
        _msg: GetDbActor,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        DbActorRef(self.db_actor.clone())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, MigrationRunner};

    async fn spawn_test_waddle() -> ActorRef<WaddleActor> {
        let db = Database::in_memory("test-waddle-actor").await.expect("db");
        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("migrations");

        let db_actor = kameo::spawn(DbActor::new(db));

        let config = WaddleConfig {
            name: "Test Waddle".to_string(),
            description: Some("A test community".to_string()),
            is_public: true,
        };

        kameo::spawn(WaddleActor::new("waddle-123".to_string(), db_actor, config))
    }

    #[tokio::test]
    async fn test_get_waddle_id() {
        let actor = spawn_test_waddle().await;
        let id: String = actor.ask(GetWaddleId).await.expect("ask");
        assert_eq!(id, "waddle-123");
    }

    #[tokio::test]
    async fn test_get_config() {
        let actor = spawn_test_waddle().await;
        let config: WaddleConfig = actor.ask(GetConfig).await.expect("ask");
        assert_eq!(config.name, "Test Waddle");
        assert_eq!(config.description.as_deref(), Some("A test community"));
        assert!(config.is_public);
    }

    #[tokio::test]
    async fn test_update_config() {
        let actor = spawn_test_waddle().await;

        let new_config = WaddleConfig {
            name: "Renamed Waddle".to_string(),
            description: None,
            is_public: false,
        };
        actor
            .ask(UpdateConfig { config: new_config })
            .await
            .expect("ask");

        let config: WaddleConfig = actor.ask(GetConfig).await.expect("ask");
        assert_eq!(config.name, "Renamed Waddle");
        assert!(config.description.is_none());
        assert!(!config.is_public);
    }

    #[tokio::test]
    async fn test_register_and_list_channels() {
        let actor = spawn_test_waddle().await;

        // Initially empty
        let channels: Vec<String> = actor.ask(ListChannels).await.expect("ask");
        assert!(channels.is_empty());

        // Register two channels
        actor
            .ask(RegisterChannel {
                channel_id: "general".to_string(),
            })
            .await
            .expect("ask");
        actor
            .ask(RegisterChannel {
                channel_id: "random".to_string(),
            })
            .await
            .expect("ask");

        let mut channels: Vec<String> = actor.ask(ListChannels).await.expect("ask");
        channels.sort();
        assert_eq!(channels, vec!["general", "random"]);
    }

    #[tokio::test]
    async fn test_unregister_channel() {
        let actor = spawn_test_waddle().await;

        actor
            .ask(RegisterChannel {
                channel_id: "general".to_string(),
            })
            .await
            .expect("ask");
        actor
            .ask(RegisterChannel {
                channel_id: "random".to_string(),
            })
            .await
            .expect("ask");

        actor
            .ask(UnregisterChannel {
                channel_id: "general".to_string(),
            })
            .await
            .expect("ask");

        let channels: Vec<String> = actor.ask(ListChannels).await.expect("ask");
        assert_eq!(channels, vec!["random"]);
    }

    #[tokio::test]
    async fn test_duplicate_channel_registration() {
        let actor = spawn_test_waddle().await;

        actor
            .ask(RegisterChannel {
                channel_id: "general".to_string(),
            })
            .await
            .expect("ask");
        actor
            .ask(RegisterChannel {
                channel_id: "general".to_string(),
            })
            .await
            .expect("ask");

        let channels: Vec<String> = actor.ask(ListChannels).await.expect("ask");
        assert_eq!(channels.len(), 1);
    }

    #[tokio::test]
    async fn test_get_db_actor() {
        let actor = spawn_test_waddle().await;
        let db_ref: DbActorRef = actor.ask(GetDbActor).await.expect("ask");
        // Verify the db actor is alive by sending a health check
        let healthy: Result<bool, _> = db_ref.0.ask(crate::db::actor::DbHealthCheck).await;
        assert!(healthy.expect("health check"));
    }
}
