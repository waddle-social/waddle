use crate::permissions::PermissionActor;
use crate::server::bootstrap_membership;
use crate::spaces_metadata::SpacesMetadataStore;
use jid::BareJid;
use kameo::actor::ActorRef;
use std::sync::Arc;
use tracing::warn;
use waddle_xmpp::inbox::storage::InboxStorage;

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
    /// Shared permission actor handle.
    pub permission_actor: ActorRef<PermissionActor>,
    /// Bare JIDs of server owners (resolved from
    /// `WADDLE_SERVER_OWNER_LOCALPARTS` + the XMPP user-bearing domain at
    /// startup). Used to seed `Affiliation::Owner` rows on Spaces PubSub
    /// nodes so XEP-0060 admin operations work for these accounts.
    pub server_owner_jids: Arc<[BareJid]>,
}

impl AppState {
    /// Test-only default constructor — uses a disabled media backend
    /// and the filesystem blob storage from `WADDLE_UPLOAD_DIR`.
    /// Production code should call [`Self::new_with_deps`] so each
    /// dependency is explicit.
    #[cfg(test)]
    pub fn new(db_pool: Arc<crate::db::DatabasePool>) -> Self {
        let blob_storage = crate::storage::build_blob_storage()
            .unwrap_or_else(|e| panic!("failed to initialize blob storage: {e}"));
        let permission_actor = kameo::spawn(PermissionActor::new_for_tests(Arc::new(
            db_pool.global().clone(),
        )));
        Self::new_with_deps(
            db_pool,
            blob_storage,
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
            Arc::new(crate::spaces_metadata::InMemorySpacesMetadataStore::new()),
            permission_actor,
            Arc::from(Vec::<BareJid>::new()),
        )
    }

    pub fn new_with_deps(
        db_pool: Arc<crate::db::DatabasePool>,
        blob_storage: Arc<dyn crate::storage::BlobStorage>,
        inbox_storage: Arc<dyn InboxStorage>,
        spaces_metadata_store: Arc<dyn SpacesMetadataStore>,
        permission_actor: ActorRef<PermissionActor>,
        server_owner_jids: Arc<[BareJid]>,
    ) -> Self {
        Self {
            db_pool,
            blob_storage,
            inbox_storage,
            spaces_metadata_store,
            permission_actor,
            server_owner_jids,
        }
    }
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
