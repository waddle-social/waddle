use std::sync::Arc;

use kameo::actor::ActorRef;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::XmppError;

use crate::auth::{NativeUserStore, SessionManager};
use crate::db::actor::{DbActor, GetDatabase};
use crate::db::Database;
use crate::permissions::{Object, PermissionActor, Subject};
use crate::vcard::VCardStore;

/// XMPP application state that bridges waddle-xmpp to server services.
pub struct XmppAppState {
    pub(super) domain: String,
    pub(super) session_manager: SessionManager,
    pub(super) permission_actor: ActorRef<PermissionActor>,
    pub(super) native_user_store: NativeUserStore,
    pub(super) vcard_store: VCardStore,
    pub(super) global_db_actor: ActorRef<DbActor>,
    pub(super) space_db_actor: Option<ActorRef<DbActor>>,
    pub(super) inbox_storage: Option<Arc<dyn InboxStorage>>,
}

impl XmppAppState {
    pub fn new(
        domain: String,
        db: Arc<Database>,
        db_actor: ActorRef<DbActor>,
        permission_actor: ActorRef<PermissionActor>,
        encryption_key: Option<&[u8]>,
    ) -> Self {
        let session_manager = SessionManager::new(db_actor.clone(), encryption_key);
        let native_user_store = NativeUserStore::new(db_actor.clone());
        let vcard_store = VCardStore::new(Arc::clone(&db));

        Self {
            domain,
            session_manager,
            permission_actor,
            native_user_store,
            vcard_store,
            global_db_actor: db_actor,
            space_db_actor: None,
            inbox_storage: None,
        }
    }

    pub(super) fn parse_resource(resource: &str) -> Result<Object, XmppError> {
        super::xmpp_permission_state::parse_resource(resource)
    }

    pub(super) fn parse_subject(subject: &str) -> Result<Subject, XmppError> {
        super::xmpp_permission_state::parse_subject(subject)
    }

    pub(super) async fn global_database(&self) -> Result<Database, XmppError> {
        self.global_db_actor
            .ask(GetDatabase)
            .await
            .map_err(|e| XmppError::internal(format!("Failed to access global database: {}", e)))
    }
}
