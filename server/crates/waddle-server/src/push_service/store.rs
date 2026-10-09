//! `DatabasePushServiceStore`: construction, schema initialization,
//! Web Push provider wiring, capability snapshot, and shared DB plumbing
//! (raw execute/query plus per-owner and per-node advisory locks).

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::push::apns::{ApnsProviderTokenSource, ApnsSender, ApnsTopic};
use waddle_xmpp::push::types::VapidSub;
use waddle_xmpp::push::vapid::VapidSigner;
use waddle_xmpp::push::WebPushSender;
use waddle_xmpp::XmppError;

use crate::db::{Database, IntoParams};

use super::apns_dispatch::ApnsProvider;
use super::secrets::PushSecretCipher;

#[derive(Clone)]
pub struct DatabasePushServiceStore {
    pub(super) db: Database,
    /// Shared unread projection used to refresh the APNs app badge at send
    /// time, so queued retries do not replay a stale absolute count.
    pub(super) inbox_storage: Option<Arc<dyn InboxStorage>>,
    pub(super) secrets: Arc<PushSecretCipher>,
    pub(super) pubsub_boundary: Option<PushServicePubSubBoundary>,
    /// VAPID signer for outbound Web Push delivery. `None` when the
    /// process boots without a configured push service (legacy test
    /// path); the publish-job worker treats absence as
    /// [`waddle_xmpp::push::WebPushCapability::Degraded`] and short-
    /// circuits Web Push fan-out without falling back to plaintext.
    pub(super) vapid_signer: Option<Arc<dyn VapidSigner>>,
    /// Transport-layer sender for outbound Web Push HTTPS posts. Paired
    /// with [`Self::vapid_signer`] — both Some or both None.
    pub(super) web_push_sender: Option<Arc<dyn WebPushSender>>,
    /// VAPID `sub` claim (RFC 8292 §2) used when minting JWTs for outbound
    /// Web Push delivery. Set together with [`Self::vapid_signer`] and
    /// [`Self::web_push_sender`]; all three are Some or all three are None.
    pub(super) vapid_sub: Option<VapidSub>,
    /// APNs provider (#529). `None` when `WADDLE_APNS_*` is unset:
    /// Apple devices then record `apns-not-configured`.
    pub(super) apns: Option<ApnsProvider>,
}

#[derive(Clone)]
pub(super) struct PushServicePubSubBoundary {
    pub(super) service_jid: BareJid,
    pub(super) storage: Arc<dyn PubSubStorage>,
}

pub(super) async fn lock_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    now_ms: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        INSERT INTO push_owner_locks (owner_bare_jid, updated_at_ms)
        VALUES (?, ?)
        ON CONFLICT(owner_bare_jid) DO UPDATE SET updated_at_ms = excluded.updated_at_ms
        "#,
        crate::db_params![owner_bare_jid.to_string(), now_ms],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn lock_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        INSERT INTO push_node_locks (node, updated_at_ms)
        VALUES (?, ?)
        ON CONFLICT(node) DO UPDATE SET updated_at_ms = excluded.updated_at_ms
        "#,
        crate::db_params![node, now_ms],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

impl DatabasePushServiceStore {
    #[cfg(test)]
    pub async fn new(db: Database) -> Result<Self, XmppError> {
        Self::new_with_secret_key(db, b"waddle-push-service-test-secret-key").await
    }

    pub async fn new_with_secret_key(db: Database, secret_key: &[u8]) -> Result<Self, XmppError> {
        let store = Self {
            db,
            inbox_storage: None,
            secrets: Arc::new(PushSecretCipher::new(secret_key)),
            pubsub_boundary: None,
            vapid_signer: None,
            web_push_sender: None,
            vapid_sub: None,
            apns: None,
        };
        store.initialize().await?;
        Ok(store)
    }

    pub async fn new_with_secret_key_and_pubsub(
        db: Database,
        secret_key: &[u8],
        service_jid: BareJid,
        pubsub_storage: Arc<dyn PubSubStorage>,
    ) -> Result<Self, XmppError> {
        let store = Self {
            db,
            inbox_storage: None,
            secrets: Arc::new(PushSecretCipher::new(secret_key)),
            pubsub_boundary: Some(PushServicePubSubBoundary {
                service_jid,
                storage: pubsub_storage,
            }),
            vapid_signer: None,
            web_push_sender: None,
            vapid_sub: None,
            apns: None,
        };
        store.initialize().await?;
        Ok(store)
    }

    /// Install the shared XMPP inbox projection used to calculate the
    /// account-wide APNs badge immediately before provider dispatch.
    pub fn with_inbox_storage(mut self, inbox_storage: Arc<dyn InboxStorage>) -> Self {
        self.inbox_storage = Some(inbox_storage);
        self
    }

    /// Install the VAPID signer + Web Push transport + VAPID `sub` claim
    /// for outbound delivery. Called once at boot from `server::http`. All
    /// three arguments are paired — Web Push cannot dispatch without any
    /// of them, and the publish-job worker treats absence of any one as
    /// [`waddle_xmpp::push::WebPushCapability::Degraded`].
    pub fn with_web_push_provider(
        mut self,
        vapid_signer: Arc<dyn VapidSigner>,
        web_push_sender: Arc<dyn WebPushSender>,
        vapid_sub: VapidSub,
    ) -> Self {
        self.vapid_signer = Some(vapid_signer);
        self.web_push_sender = Some(web_push_sender);
        self.vapid_sub = Some(vapid_sub);
        self
    }

    /// Install the APNs provider token source, HTTP/2 transport and the
    /// configured bundle id (`apns-topic`). Called once at boot from
    /// `server::http` when `WADDLE_APNS_*` is configured.
    pub fn with_apns_provider(
        mut self,
        tokens: Arc<dyn ApnsProviderTokenSource>,
        sender: Arc<dyn ApnsSender>,
        topic: ApnsTopic,
    ) -> Self {
        self.apns = Some(ApnsProvider {
            tokens,
            sender,
            topic,
        });
        self
    }

    pub fn database(&self) -> Database {
        self.db.clone()
    }

    /// Typed advertisement of the Push Service's active VAPID public key.
    /// Returned only when a [`VapidSigner`] is installed via
    /// [`Self::with_web_push_provider`]; legacy test paths that boot the
    /// store without a signer return `None`, and the disco handler
    /// suppresses the XEP-0128 form rather than synthesizing a placeholder.
    pub fn vapid_advertisement(&self) -> Option<waddle_xmpp::push::disco::VapidAdvertisement> {
        let signer = self.vapid_signer.as_ref()?;
        Some(waddle_xmpp::push::disco::VapidAdvertisement::new(
            signer.current_public_key(),
            signer.current_kid(),
        ))
    }

    async fn initialize(&self) -> Result<(), XmppError> {
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_owner_locks (
                    owner_bare_jid TEXT PRIMARY KEY,
                    updated_at_ms {i64_type} NOT NULL
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_nodes (
                    node TEXT PRIMARY KEY,
                    owner_bare_jid TEXT NOT NULL,
                    app_id TEXT NOT NULL,
                    status TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
                    created_at_ms {i64_type} NOT NULL,
                    updated_at_ms {i64_type} NOT NULL,
                    UNIQUE (owner_bare_jid, app_id)
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_node_locks (
                    node TEXT PRIMARY KEY,
                    updated_at_ms {i64_type} NOT NULL,
                    FOREIGN KEY (node) REFERENCES push_nodes(node) ON DELETE CASCADE
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_nodes_owner \
             ON push_nodes (owner_bare_jid, status)",
            (),
        )
        .await?;
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_devices (
                    device_id TEXT NOT NULL,
                    node TEXT NOT NULL,
                    platform TEXT NOT NULL CHECK (platform IN ('web', 'apns', 'fcm')),
                    environment TEXT NOT NULL,
                    provider_endpoint TEXT,
                    provider_token TEXT,
                    provider_key_material TEXT,
                    status TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
                    failure_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    created_at_ms {i64_type} NOT NULL,
                    updated_at_ms {i64_type} NOT NULL,
                    PRIMARY KEY (node, device_id),
                    FOREIGN KEY (node) REFERENCES push_nodes(node) ON DELETE CASCADE
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_devices_node_status \
             ON push_devices (node, status)",
            (),
        )
        .await?;
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_delivery_attempts (
                    attempt_id TEXT PRIMARY KEY,
                    node TEXT NOT NULL,
                    device_id TEXT NOT NULL,
                    platform TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    last_error TEXT,
                    created_at_ms {i64_type} NOT NULL,
                    FOREIGN KEY (node) REFERENCES push_nodes(node) ON DELETE CASCADE,
                    FOREIGN KEY (node, device_id) REFERENCES push_devices(node, device_id) ON DELETE CASCADE
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_delivery_attempts_node_created \
             ON push_delivery_attempts (node, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_delivery_attempts_device_created \
             ON push_delivery_attempts (device_id, created_at_ms)",
            (),
        )
        .await?;
        // #1123 retry-path idempotency lookup:
        // `delivery_attempts_for_node` retains this legacy wire-item index.
        // Retry dedup uses the scoped acceptance index installed below.
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_delivery_attempts_item_status \
             ON push_delivery_attempts (node, item_id, status, device_id)",
            (),
        )
        .await?;
        self.execute(
            &push_publish_jobs_table_sql(i64_type, "push_publish_jobs", true),
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_publish_jobs_status_created \
             ON push_publish_jobs (status, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_push_publish_jobs_node_status \
             ON push_publish_jobs (node, status)",
            (),
        )
        .await?;
        self.add_column_if_missing("push_publish_jobs", "publish_options_xml TEXT")
            .await?;
        self.add_column_if_missing("push_publish_jobs", "push_service_jid TEXT")
            .await?;
        // `claim_token` is the at-most-once delivery interlock: phase 1
        // writes a fresh UUID; phase 3's UPDATE checks `claim_token = ?`
        // so a recovered-and-re-claimed job (which holds a different
        // token) cannot persist attempts from the original worker.
        self.add_column_if_missing("push_publish_jobs", "claim_token TEXT")
            .await?;
        self.add_column_if_missing("push_publish_jobs", "ancestry_job_id TEXT")
            .await?;
        self.add_column_if_missing(
            "push_publish_jobs",
            "acceptance_scope TEXT NOT NULL DEFAULT 'legacy'",
        )
        .await?;
        self.add_column_if_missing(
            "push_publish_jobs",
            &format!("backing_published_at_ms {i64_type}"),
        )
        .await?;
        self.add_column_if_missing(
            "push_publish_jobs",
            &format!("publication_order {i64_type} NOT NULL DEFAULT 0"),
        )
        .await?;
        self.add_column_if_missing(
            "push_publish_jobs",
            "backing_state TEXT NOT NULL DEFAULT 'pending'",
        )
        .await?;
        self.execute(&format!("CREATE TABLE IF NOT EXISTS push_publication_orders (node TEXT PRIMARY KEY, next_order {i64_type} NOT NULL)"), ()).await?;
        self.add_column_if_missing("push_delivery_attempts", "publish_job_id TEXT")
            .await?;
        self.migrate_publish_acceptance_scope().await?;
        self.adopt_notification_ancestry().await?;
        Ok(())
    }

    async fn migrate_publish_acceptance_scope(&self) -> Result<(), XmppError> {
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        // The old unique node/item pair proves which pending acceptance owns
        // legacy attempts. Preserve it before removing that uniqueness.
        tx.execute("UPDATE push_delivery_attempts SET publish_job_id = (SELECT job_id FROM push_publish_jobs WHERE push_publish_jobs.node = push_delivery_attempts.node AND push_publish_jobs.item_id = push_delivery_attempts.item_id AND push_publish_jobs.acceptance_scope = 'legacy' LIMIT 1) WHERE publish_job_id IS NULL", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        if tx.driver() == crate::db::DatabaseDriver::Postgres {
            tx.execute("ALTER TABLE push_publish_jobs DROP CONSTRAINT IF EXISTS push_publish_jobs_node_item_id_key", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        } else {
            let mut rows = tx.query("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'push_publish_jobs'", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
            let legacy_unique = match rows
                .next()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?
            {
                Some(row) => row
                    .get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    .contains("UNIQUE (node, item_id)"),
                None => false,
            };
            if legacy_unique {
                tx.execute(
                    &push_publish_jobs_table_sql("INTEGER", "push_publish_jobs_rebuild", false),
                    (),
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute("INSERT INTO push_publish_jobs_rebuild (job_id, owner_bare_jid, push_service_jid, node, item_id, payload_xml, publish_options_xml, ancestry_job_id, acceptance_scope, backing_published_at_ms, publication_order, backing_state, status, attempt_count, last_error, next_retry_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms) SELECT job_id, owner_bare_jid, push_service_jid, node, item_id, payload_xml, publish_options_xml, ancestry_job_id, acceptance_scope, backing_published_at_ms, publication_order, backing_state, status, attempt_count, last_error, next_retry_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms, published_at_ms FROM push_publish_jobs", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute("DROP TABLE push_publish_jobs", ())
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute(
                    "ALTER TABLE push_publish_jobs_rebuild RENAME TO push_publish_jobs",
                    (),
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            }
        }
        loop {
            let mut rows = tx.query("SELECT job_id, node FROM push_publish_jobs WHERE publication_order = 0 ORDER BY created_at_ms, job_id LIMIT 128", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
            let mut pending = Vec::new();
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?
            {
                pending.push((
                    row.get::<String>(0)
                        .map_err(|error| XmppError::internal(error.to_string()))?,
                    row.get::<String>(1)
                        .map_err(|error| XmppError::internal(error.to_string()))?,
                ));
            }
            if pending.is_empty() {
                break;
            }
            for (job, node) in pending {
                let order =
                    super::publish_jobs::allocate_publication_order_tx(&mut tx, &node).await?;
                tx.execute("UPDATE push_publish_jobs SET publication_order = ?, backing_state = CASE WHEN acceptance_scope = 'legacy' THEN 'published' ELSE backing_state END, backing_published_at_ms = CASE WHEN acceptance_scope = 'legacy' THEN created_at_ms ELSE backing_published_at_ms END WHERE job_id = ? AND publication_order = 0",
                    crate::db_params![order, job]).await.map_err(|error| XmppError::internal(error.to_string()))?;
            }
        }
        tx.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_push_publish_jobs_canonical_delivery ON push_publish_jobs (ancestry_job_id) WHERE ancestry_job_id IS NOT NULL", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        tx.execute("CREATE INDEX IF NOT EXISTS idx_push_publish_jobs_status_created ON push_publish_jobs (status, created_at_ms)", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        tx.execute("CREATE INDEX IF NOT EXISTS idx_push_publish_jobs_node_status ON push_publish_jobs (node, status)", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        tx.execute("CREATE INDEX IF NOT EXISTS idx_push_delivery_attempts_acceptance_status ON push_delivery_attempts (publish_job_id, status, device_id)", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn add_column_if_missing(&self, table: &str, column_def: &str) -> Result<(), XmppError> {
        let alter_sql = match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {column_def}")
            }
            crate::db::DatabaseDriver::Sqlite => {
                format!("ALTER TABLE {table} ADD COLUMN {column_def}")
            }
        };
        if let Err(error) = self.execute(&alter_sql, ()).await {
            let msg = error.to_string().to_lowercase();
            if msg.contains("duplicate column") || msg.contains("already exists") {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    /// `true` when all three Web Push provider slots are wired
    /// (`vapid_signer`, `web_push_sender`, `vapid_sub`). Without them,
    /// Web devices record the transient `web-not-configured` marker.
    pub(super) fn web_push_provider_ready(&self) -> bool {
        self.vapid_signer.is_some() && self.web_push_sender.is_some() && self.vapid_sub.is_some()
    }

    /// `true` when any real provider (Web Push or APNs) is wired, i.e.
    /// when the worker needs the parsed XEP-0357 payload to build a
    /// provider request.
    pub(super) fn any_provider_ready(&self) -> bool {
        self.web_push_provider_ready() || self.apns.is_some()
    }

    /// Public snapshot of the Web Push capability — `Ready` when the
    /// store has a VAPID signer, transport, and sub all wired;
    /// `Degraded { Xep0357PushServiceDegraded }` otherwise.
    ///
    /// The T1 push-gate (in `notification_outbox`) is expected to
    /// consult this before falling back to any in-band channel, so a
    /// degraded push service cannot accidentally leak content over a
    /// plaintext path. No fallback path exists today, but the typed
    /// suppression reason is plumbed end-to-end so the day someone
    /// adds one, the safety invariant holds by construction.
    pub fn web_push_capability(&self) -> waddle_xmpp::push::WebPushCapability {
        if self.web_push_provider_ready() {
            waddle_xmpp::push::WebPushCapability::Ready
        } else {
            waddle_xmpp::push::WebPushCapability::Degraded {
                reason: waddle_xmpp::push::SuppressionReason::Xep0357PushServiceDegraded,
            }
        }
    }

    pub(super) async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    pub(super) async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::push_service::test_support::{assert_item_not_found, notification_item, owner};
    use crate::push_service::types::PushNodeStatus;
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};

    #[tokio::test]
    async fn push_service_store_survives_reopen_with_disabled_cleanup() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("push-service.sqlite3");
        let owner = owner();
        let node_id;
        {
            let db = Database::open_local("push-service-reopen", &path)
                .await
                .expect("database");
            let store = DatabasePushServiceStore::new(db).await.expect("store");
            let node = store.ensure_node(&owner, "web").await.expect("node");
            node_id = node.node().to_string();
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        "web-1",
                        node.node(),
                        PushDevicePlatform::Web,
                        "test",
                    )
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
                )
                .await
                .expect("device");
            assert_eq!(
                store
                    .disable_nodes_for_owner(&owner, Some(node.node()))
                    .await
                    .expect("disable node"),
                1
            );
        }

        let reopened_db = Database::open_local("push-service-reopen-again", &path)
            .await
            .expect("reopened database");
        let reopened = DatabasePushServiceStore::new(reopened_db)
            .await
            .expect("reopened store");
        let node = reopened
            .get_node(&node_id)
            .await
            .expect("node lookup")
            .expect("node");
        let device = reopened
            .get_device_for_owner_on_node(&owner, &node_id, "web-1")
            .await
            .expect("device lookup")
            .expect("device");
        let publish_err = reopened
            .publish_notification_from_user_server(
                &node_id,
                &notification_item("after-reopen"),
                &owner,
            )
            .await
            .expect_err("disabled node rejects publish after reopen");

        assert_eq!(node.status, PushNodeStatus::Disabled);
        assert_eq!(device.provider_endpoint(), None);
        assert_eq!(device.provider_token(), None);
        assert_eq!(device.provider_key_material(), None);
        assert_item_not_found(publish_err);
    }
}

fn push_publish_jobs_table_sql(i64_type: &str, table: &str, if_not_exists: bool) -> String {
    let exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        r#"CREATE TABLE {exists}{table} (
        job_id TEXT PRIMARY KEY,
        owner_bare_jid TEXT NOT NULL,
        push_service_jid TEXT,
        node TEXT NOT NULL,
        item_id TEXT NOT NULL,
        payload_xml TEXT NOT NULL,
        publish_options_xml TEXT,
        ancestry_job_id TEXT,
        acceptance_scope TEXT NOT NULL DEFAULT 'legacy' CHECK (acceptance_scope IN ('legacy', 'wire', 'canonical')),
        backing_published_at_ms {i64_type},
        publication_order {i64_type} NOT NULL DEFAULT 0,
        backing_state TEXT NOT NULL DEFAULT 'pending' CHECK (backing_state IN ('pending', 'published', 'superseded', 'not-configured')),
        status TEXT NOT NULL CHECK (status IN ('queued', 'in-progress', 'published', 'failed')),
        attempt_count INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        next_retry_at_ms {i64_type},
        claimed_at_ms {i64_type},
        claim_token TEXT,
        created_at_ms {i64_type} NOT NULL,
        updated_at_ms {i64_type} NOT NULL,
        published_at_ms {i64_type},
        FOREIGN KEY (node) REFERENCES push_nodes(node) ON DELETE CASCADE
    )"#
    )
}
