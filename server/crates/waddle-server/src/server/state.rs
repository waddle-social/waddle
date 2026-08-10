use crate::channel_space_links::ChannelSpaceLinkStore;
use crate::permissions::PermissionActor;
use crate::server::bootstrap_membership;
use crate::spaces_metadata::SpacesMetadataStore;
use jid::{BareJid, DomainPart};
use kameo::actor::ActorRef;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::warn;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;

/// Server application state
pub struct AppState {
    /// Database pool for global and per-waddle databases
    pub db_pool: Arc<crate::db::DatabasePool>,
    /// Blob storage backend for file uploads (XEP-0363).
    pub blob_storage: Arc<dyn crate::storage::BlobStorage>,
    /// Shared Waddle inbox projection storage.
    pub inbox_storage: Arc<dyn InboxStorage>,
    /// Spaces metadata storage (`name`, `description`, `icon_url`) — read
    /// and written by the admin V2 `spaces:create` / `spaces:update`
    /// commands. XEP-0503 has no opinion on these human-facing fields;
    /// they live here as a Waddle-specific projection.
    pub spaces_metadata_store: Arc<dyn SpacesMetadataStore>,
    /// Channel → space link storage. Records which space (XEP-0503)
    /// owns a given channel (XEP-0045 room) so admin V2's
    /// `channels:list` `space_jid` filter can narrow results to the
    /// channels of a single space, and so `spaces:delete` can cascade
    /// destroy its linked channels.
    pub channel_space_link_store: Arc<dyn ChannelSpaceLinkStore>,
    /// Shared PubSub/PEP storage (XEP-0060/XEP-0163). Exposed on
    /// `AppState` so admin V2 `spaces:list` / `channels:list` handlers
    /// can enumerate pubsub-tree node membership without re-routing
    /// through the per-connection `ProtocolServices` graph.
    pub pubsub_storage: Arc<dyn PubSubStorage>,
    /// Actor-backed MUC room registry (XEP-0045). Exposed on `AppState`
    /// so admin V2 channel CRUD handlers (`channels:create`,
    /// `channels:update`, `channels:delete`, `channels:kick`,
    /// `channels:set-affiliation`) can issue room messages independent
    /// of any user's live WebSocket connection.
    pub room_registry: ActorRef<RoomRegistryActor>,
    /// Bare JID of the community spaces (XEP-0503) service for this
    /// deployment. Resolved from [`crate::server::XmppConfig::spaces_jid`]
    /// (env override `WADDLE_SPACES_JID`, defaulted to
    /// `spaces.<xmpp-domain>`). Admin V2 `spaces:*` handlers use this to
    /// scope pubsub-tree reads/writes.
    pub spaces_jid: BareJid,
    /// MUC (XEP-0045) service domain for this deployment. Resolved
    /// from [`crate::server::XmppConfig::muc_domain`] (env override
    /// `WADDLE_MUC_DOMAIN`, defaulted to `muc.<xmpp-domain>`). Admin V2
    /// `channels:create` constructs new managed room JIDs against this
    /// domain.
    pub muc_domain: DomainPart,
    /// Shared secret for XEP-0421 occupant-id generation.
    pub occupant_id_secret: OccupantIdSecret,
    /// Shared permission actor handle.
    pub permission_actor: ActorRef<PermissionActor>,
    /// Bare JIDs of server owners (resolved from
    /// `WADDLE_SERVER_OWNER_LOCALPARTS` + the XMPP user-bearing domain at
    /// startup). Used to seed `Affiliation::Owner` rows on Spaces PubSub
    /// nodes so XEP-0060 admin operations work for these accounts.
    pub server_owner_jids: Arc<[BareJid]>,
    /// Sole typed authority for HTTP readiness and WebSocket admission.
    /// Cluster self-fencing moves it to `FencedRecovering`; an unexpected
    /// critical registry death latches `Failed` until this process exits.
    pub node_lifecycle: crate::clustering::NodeLifecycle,
    /// Live `ClaimStore`/node-identity handles from the clustering
    /// subsystem (ADR-0017 Phase 3 Slice 4 follow-up plumbing note), used
    /// to select and construct the Postgres-fenced `SmPersistenceStorage`
    /// at WebSocket-state build time. Both fields are `None` whenever
    /// clustering is disabled or not compiled in — see
    /// [`crate::clustering::ClusteringHandles`].
    pub clustering_claims: crate::clustering::ClusteringHandles,
    /// Immutable database-lineage topology, published exactly once after all
    /// independently opened storage pools are available.
    pub lineage_registry: OnceLock<crate::db::lineage::LineageRegistry>,
    lineage_registry_builder: Mutex<Option<crate::db::lineage::LineageRegistryBuilder>>,
    pub lineage_config: crate::config::LineageConfig,
    pub clustering_enabled: bool,
    /// Result of the one startup attestation pass, published exactly once by
    /// `create_websocket_state` after the registry seals. `create_router`
    /// consults it to gate Serving promotion and the janitor spawns.
    pub lineage_startup: OnceLock<crate::db::lineage::LineageReport>,
    /// The report that LATCHED this node (startup-definitive or live drift
    /// detected at readiness). Preferred over `lineage_startup` in
    /// not-Serving readiness responses so the operator sees the failure
    /// that actually took the node down, not a stale startup snapshot.
    pub lineage_latched: OnceLock<crate::db::lineage::LineageReport>,
    /// Which `adopt=<uuid>` list entries actually matched a durable
    /// boundary. An entry that matched nothing is an operator typo (or a
    /// boundary that no longer exists) and must fail the startup gate loudly.
    lineage_adopt_matched: Mutex<std::collections::HashSet<crate::db::lineage::LineageUuid>>,
}

impl AppState {
    /// Test-only default constructor — uses a disabled media backend
    /// and the filesystem blob storage from `WADDLE_UPLOAD_DIR`.
    /// Production code should call [`Self::new_with_deps`] so each
    /// dependency is explicit.
    #[cfg(test)]
    pub fn new(db_pool: Arc<crate::db::DatabasePool>) -> Self {
        use kameo::actor::Spawn;
        use std::str::FromStr;
        use waddle_xmpp::pubsub::InMemoryPubSubStorage;
        use waddle_xmpp::xep::xep0421::OccupantIdSecret;

        let blob_storage = crate::storage::build_blob_storage()
            .unwrap_or_else(|e| panic!("failed to initialize blob storage: {e}"));
        let permission_actor = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
            db_pool.global().clone(),
        )));
        let muc_domain = DomainPart::from_str("muc.localhost")
            .expect("test MUC domain 'muc.localhost' parses as DomainPart");
        let occupant_id_secret =
            OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
                .expect("test occupant-id secret meets length floor");
        let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            muc_domain.to_string(),
            occupant_id_secret.clone(),
        ));
        let spaces_jid: BareJid = "spaces.localhost"
            .parse()
            .expect("test spaces JID 'spaces.localhost' parses as BareJid");
        let state = Self::new_with_deps(AppStateDeps {
            db_pool,
            blob_storage,
            inbox_storage: Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
            spaces_metadata_store: Arc::new(
                crate::spaces_metadata::InMemorySpacesMetadataStore::new(),
            ),
            channel_space_link_store: Arc::new(
                crate::channel_space_links::InMemoryChannelSpaceLinkStore::new(),
            ),
            pubsub_storage: Arc::new(InMemoryPubSubStorage::new()),
            room_registry,
            spaces_jid,
            muc_domain,
            occupant_id_secret,
            permission_actor,
            server_owner_jids: Arc::from(Vec::<BareJid>::new()),
            node_lifecycle: crate::clustering::NodeLifecycle::new(),
            clustering_claims: crate::clustering::ClusteringHandles::default(),
            lineage_config: crate::config::LineageConfig::default(),
            clustering_enabled: false,
        });
        let _ = state
            .lineage_registry
            .set(crate::db::lineage::LineageRegistry::new(Vec::new()));
        state
    }

    /// Construct an `AppState` from its explicit dependencies. Callers
    /// pass [`AppStateDeps`] so adding a new dependency does not widen
    /// the constructor signature.
    pub fn new_with_deps(deps: AppStateDeps) -> Self {
        let AppStateDeps {
            db_pool,
            blob_storage,
            inbox_storage,
            spaces_metadata_store,
            channel_space_link_store,
            pubsub_storage,
            room_registry,
            spaces_jid,
            muc_domain,
            occupant_id_secret,
            permission_actor,
            server_owner_jids,
            node_lifecycle,
            clustering_claims,
            lineage_config,
            clustering_enabled,
        } = deps;
        Self {
            db_pool,
            blob_storage,
            inbox_storage,
            spaces_metadata_store,
            channel_space_link_store,
            pubsub_storage,
            room_registry,
            spaces_jid,
            muc_domain,
            occupant_id_secret,
            permission_actor,
            server_owner_jids,
            node_lifecycle,
            clustering_claims,
            lineage_registry: OnceLock::new(),
            lineage_registry_builder: Mutex::new(Some(
                crate::db::lineage::LineageRegistryBuilder::default(),
            )),
            lineage_config,
            clustering_enabled,
            lineage_startup: OnceLock::new(),
            lineage_latched: OnceLock::new(),
            lineage_adopt_matched: Mutex::new(std::collections::HashSet::new()),
        }
    }
}

impl AppState {
    pub fn register_lineage_database(
        &self,
        store: crate::db::lineage::DurableStore,
        database: crate::db::Database,
    ) {
        if let Ok(mut builder) = self.lineage_registry_builder.lock() {
            if let Some(builder) = builder.as_mut() {
                builder.register_database(store, database);
            }
        }
    }

    pub fn register_lineage_probe(
        &self,
        store: crate::db::lineage::DurableStore,
        attestor: Arc<dyn crate::db::lineage::LineageAttestor>,
    ) {
        if let Ok(mut builder) = self.lineage_registry_builder.lock() {
            if let Some(builder) = builder.as_mut() {
                builder.register_probe(store, attestor);
            }
        }
    }

    pub fn register_lineage_control_plane(
        &self,
        store: crate::db::lineage::DurableStore,
        database: crate::db::Database,
    ) {
        if let Ok(mut builder) = self.lineage_registry_builder.lock() {
            if let Some(builder) = builder.as_mut() {
                builder.register_control_plane(store, database);
            }
        }
    }

    pub fn register_lineage_alias(
        &self,
        store: crate::db::lineage::DurableStore,
        of: crate::db::lineage::DurableStore,
    ) {
        if let Ok(mut builder) = self.lineage_registry_builder.lock() {
            if let Some(builder) = builder.as_mut() {
                builder.register_alias(store, of);
            }
        }
    }

    pub fn register_lineage_ephemeral(&self, store: crate::db::lineage::DurableStore) {
        if let Ok(mut builder) = self.lineage_registry_builder.lock() {
            if let Some(builder) = builder.as_mut() {
                builder.register_ephemeral(store);
            }
        }
    }

    pub fn note_lineage_adopt_match(&self, matched: crate::db::lineage::LineageUuid) {
        if let Ok(mut set) = self.lineage_adopt_matched.lock() {
            set.insert(matched);
        }
    }

    /// `adopt=` list entries that matched no boundary this run. Non-empty is
    /// NOT a readiness failure: after a successful adoption (or a pod restart
    /// while the one-shot action is still rendered) every row verifies and
    /// the replayed action legitimately matches nothing; a genuine restore
    /// that still needs adoption keeps failing attestation on its own
    /// identity mismatch. The startup gate logs these loudly instead.
    pub fn lineage_adopt_unmatched(&self) -> Vec<crate::db::lineage::LineageUuid> {
        let Some(crate::db::lineage::LineageAction::Adopt(expected)) =
            self.lineage_config.action.as_ref()
        else {
            return Vec::new();
        };
        let matched = self
            .lineage_adopt_matched
            .lock()
            .map(|set| set.clone())
            .unwrap_or_default();
        expected
            .iter()
            .filter(|uuid| !matched.contains(uuid))
            .copied()
            .collect()
    }

    pub fn seal_lineage_registry(&self) {
        let registry = self
            .lineage_registry_builder
            .lock()
            .ok()
            .and_then(|mut builder| {
                builder
                    .take()
                    .map(crate::db::lineage::LineageRegistryBuilder::seal)
            });
        if let Some(registry) = registry {
            let _ = self.lineage_registry.set(registry);
        }
    }
}

/// Explicit `AppState` dependency bundle. Construction grouped into a
/// single struct so adding a new dependency does not widen the
/// `new_with_deps` argument list (clippy `too_many_arguments`).
pub struct AppStateDeps {
    pub db_pool: Arc<crate::db::DatabasePool>,
    pub blob_storage: Arc<dyn crate::storage::BlobStorage>,
    pub inbox_storage: Arc<dyn InboxStorage>,
    pub spaces_metadata_store: Arc<dyn SpacesMetadataStore>,
    pub channel_space_link_store: Arc<dyn ChannelSpaceLinkStore>,
    pub pubsub_storage: Arc<dyn PubSubStorage>,
    pub room_registry: ActorRef<RoomRegistryActor>,
    pub spaces_jid: BareJid,
    pub muc_domain: DomainPart,
    pub occupant_id_secret: OccupantIdSecret,
    pub permission_actor: ActorRef<PermissionActor>,
    pub server_owner_jids: Arc<[BareJid]>,
    pub node_lifecycle: crate::clustering::NodeLifecycle,
    pub clustering_claims: crate::clustering::ClusteringHandles,
    pub lineage_config: crate::config::LineageConfig,
    pub clustering_enabled: bool,
}

/// Resolve `WADDLE_SERVER_OWNER_LOCALPARTS` localparts into bare JIDs against
/// `xmpp_domain`. Bad localparts produce a `warn!` and are skipped; they do
/// not block startup.
pub fn resolve_server_owner_jids(
    config: &bootstrap_membership::BootstrapMembershipConfig,
    xmpp_domain: &str,
) -> Arc<[BareJid]> {
    let mut jids = Vec::new();
    for localpart in config.owner_localparts() {
        let raw = format!("{localpart}@{xmpp_domain}");
        match raw.parse::<BareJid>() {
            Ok(jid) => jids.push(jid),
            Err(error) => warn!(
                localpart = %localpart,
                xmpp_domain = %xmpp_domain,
                error = %error,
                "skipping invalid server-owner localpart for spaces affiliation seeding",
            ),
        }
    }
    Arc::from(jids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};

    #[tokio::test]
    async fn new_with_deps_populates_admin_v2_dependencies() {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        let state = AppState::new(Arc::new(db_pool));

        // pubsub_storage and room_registry are concrete handles — their
        // mere existence is the contract, since admin V2 talks to them
        // via the trait / actor protocol.
        assert!(Arc::strong_count(&state.pubsub_storage) >= 1);
        assert!(
            state.room_registry.is_alive(),
            "room registry actor should be live immediately after spawn"
        );
        assert_eq!(state.spaces_jid.to_string(), "spaces.localhost");
        assert_eq!(state.muc_domain.as_str(), "muc.localhost");
    }
}
