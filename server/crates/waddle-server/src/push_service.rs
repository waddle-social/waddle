//! First-party XMPP Push Service storage and fake provider dispatch.
//!
//! This module is the Push Service side of XEP-0357. It deliberately does not
//! store user-server `<enable/>` registration state; that remains in
//! [`crate::push_registrations`]. Provider endpoints and tokens live here,
//! behind the `push.<domain>` service boundary.

use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use jid::BareJid;
use minidom::Element;
use sha2::Sha256;
use tracing::warn;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pubsub::{PubSubItem, PubSubRequest};
use waddle_xmpp::push::{PushError, PushSubscription};
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;
use xmpp_parsers::iq::{Iq, IqType};

use crate::db::{Database, IntoParams};

pub const WADDLE_PUSH_SERVICE_NS: &str = "urn:waddle:push-service:0";

type HmacSha256 = Hmac<Sha256>;

const NODE_STATUS_ACTIVE: &str = "active";
const NODE_STATUS_DISABLED: &str = "disabled";
const DEVICE_STATUS_ACTIVE: &str = "active";
const DEVICE_STATUS_DISABLED: &str = "disabled";
const ATTEMPT_STATUS_FAKE_SENT: &str = "fake-sent";
const PUBLISH_JOB_STATUS_QUEUED: &str = "queued";
const PUBLISH_JOB_STATUS_IN_PROGRESS: &str = "in-progress";
const PUBLISH_JOB_STATUS_PUBLISHED: &str = "published";
const PUBLISH_JOB_STATUS_FAILED: &str = "failed";
const PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES: &str = "Push node has no active devices";
const SEALED_PROVIDER_VALUE_PREFIX: &str = "waddle-push-secret-v1";

const MAX_PUSH_NODES_PER_OWNER: i64 = 16;
const MAX_PUSH_DEVICES_PER_NODE: i64 = 32;
const MAX_RETAINED_DISABLED_NODES_PER_OWNER: i64 = 64;
const MAX_RETAINED_DISABLED_DEVICES_PER_NODE: i64 = 128;
const MAX_DELIVERY_ATTEMPTS_PER_NODE: i64 = 10_000;
const MAX_PUBLISH_JOBS_PER_NODE: i64 = 10_000;
const MAX_APP_ID_LEN: usize = 128;
const MAX_NODE_ID_LEN: usize = 256;
const MAX_DEVICE_ID_LEN: usize = 128;
const MAX_ENVIRONMENT_LEN: usize = 64;
const MAX_PROVIDER_ENDPOINT_LEN: usize = 2_048;
const MAX_PROVIDER_TOKEN_LEN: usize = 4_096;
const MAX_PROVIDER_KEY_MATERIAL_LEN: usize = 4_096;
const MAX_PUBSUB_ITEM_ID_LEN: usize = 256;
const PUBLISH_JOB_RETRY_DELAY_MS: i64 = 60_000;
const PUBLISH_JOB_CLAIM_TIMEOUT_MS: i64 = 300_000;
const PUSH_RECONCILIATION_SCAN_FACTOR: usize = 16;

#[derive(Clone)]
pub struct DatabasePushServiceStore {
    db: Database,
    secrets: PushSecretCipher,
}

#[derive(Clone)]
struct PushSecretCipher {
    enc_key: Vec<u8>,
    mac_key: Vec<u8>,
}

impl PushSecretCipher {
    fn new(root_key: &[u8]) -> Self {
        Self {
            enc_key: derive_secret_key(root_key, b"waddle:push-service:provider-secret:enc:v1"),
            mac_key: derive_secret_key(root_key, b"waddle:push-service:provider-secret:mac:v1"),
        }
    }

    fn seal_optional(&self, value: Option<String>) -> Result<Option<String>, XmppError> {
        value.map(|value| self.seal(value.as_bytes())).transpose()
    }

    fn open_optional(&self, value: Option<String>) -> Result<Option<String>, XmppError> {
        value.map(|value| self.open(&value)).transpose()
    }

    fn seal(&self, plaintext: &[u8]) -> Result<String, XmppError> {
        let nonce: [u8; 16] = rand::random();
        let ciphertext = xor_keystream(&self.enc_key, &nonce, plaintext);
        let tag = provider_secret_tag(&self.mac_key, &nonce, &ciphertext);
        Ok(format!(
            "{SEALED_PROVIDER_VALUE_PREFIX}:{}:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext),
            URL_SAFE_NO_PAD.encode(tag)
        ))
    }

    fn open(&self, stored: &str) -> Result<String, XmppError> {
        let mut parts = stored.split(':');
        let prefix = parts.next();
        let nonce = parts.next();
        let ciphertext = parts.next();
        let tag = parts.next();
        if prefix != Some(SEALED_PROVIDER_VALUE_PREFIX) || parts.next().is_some() {
            return Err(XmppError::internal(
                "Push Service provider secret is not sealed",
            ));
        }
        let nonce = nonce
            .ok_or_else(|| XmppError::internal("Push Service provider secret missing nonce"))
            .and_then(decode_sealed_part)?;
        let ciphertext = ciphertext
            .ok_or_else(|| XmppError::internal("Push Service provider secret missing ciphertext"))
            .and_then(decode_sealed_part)?;
        let tag = tag
            .ok_or_else(|| XmppError::internal("Push Service provider secret missing tag"))
            .and_then(decode_sealed_part)?;
        let nonce: [u8; 16] = nonce.try_into().map_err(|_| {
            XmppError::internal("Push Service provider secret nonce has invalid length")
        })?;
        let expected = provider_secret_tag(&self.mac_key, &nonce, &ciphertext);
        if !constant_time_eq(&expected, &tag) {
            return Err(XmppError::internal(
                "Push Service provider secret authentication failed",
            ));
        }
        let plaintext = xor_keystream(&self.enc_key, &nonce, &ciphertext);
        String::from_utf8(plaintext).map_err(|error| {
            XmppError::internal(format!(
                "Push Service provider secret is invalid UTF-8: {error}"
            ))
        })
    }
}

fn derive_secret_key(root_key: &[u8], label: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(root_key).expect("HMAC supports any key length");
    mac.update(label);
    mac.finalize().into_bytes().to_vec()
}

fn xor_keystream(key: &[u8], nonce: &[u8; 16], input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut counter = 0_u64;
    while output.len() < input.len() {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key length");
        mac.update(nonce);
        mac.update(&counter.to_be_bytes());
        let block = mac.finalize().into_bytes();
        for byte in block {
            if output.len() == input.len() {
                break;
            }
            output.push(input[output.len()] ^ byte);
        }
        counter += 1;
    }
    output
}

fn provider_secret_tag(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key length");
    mac.update(SEALED_PROVIDER_VALUE_PREFIX.as_bytes());
    mac.update(nonce);
    mac.update(ciphertext);
    mac.finalize().into_bytes().to_vec()
}

fn decode_sealed_part(part: &str) -> Result<Vec<u8>, XmppError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|error| XmppError::internal(format!("Invalid sealed provider secret: {error}")))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushDevicePlatform {
    Web,
    Apns,
    Fcm,
}

impl PushDevicePlatform {
    fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Apns => "apns",
            Self::Fcm => "fcm",
        }
    }

    pub fn parse(value: &str) -> Result<Self, XmppError> {
        match value {
            "web" => Ok(Self::Web),
            "apns" => Ok(Self::Apns),
            "fcm" => Ok(Self::Fcm),
            other => Err(XmppError::bad_request(Some(format!(
                "Unsupported push device platform '{other}'"
            )))),
        }
    }
}

impl fmt::Display for PushDevicePlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushNodeStatus {
    Active,
    Disabled,
}

impl PushNodeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => NODE_STATUS_ACTIVE,
            Self::Disabled => NODE_STATUS_DISABLED,
        }
    }

    fn parse(value: &str) -> Result<Self, XmppError> {
        match value {
            NODE_STATUS_ACTIVE => Ok(Self::Active),
            NODE_STATUS_DISABLED => Ok(Self::Disabled),
            other => Err(XmppError::internal(format!(
                "Invalid push node status '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PushServiceNode {
    node: String,
    owner_bare_jid: BareJid,
    app_id: String,
    status: PushNodeStatus,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl PushServiceNode {
    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn owner_bare_jid(&self) -> &BareJid {
        &self.owner_bare_jid
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    pub fn updated_at_ms(&self) -> i64 {
        self.updated_at_ms
    }
}

#[derive(Debug, Clone)]
pub struct PushDeviceRegistration {
    device_id: String,
    node: String,
    platform: PushDevicePlatform,
    environment: String,
    provider_endpoint: Option<String>,
    provider_token: Option<String>,
    provider_key_material: Option<String>,
}

impl PushDeviceRegistration {
    pub fn new(
        device_id: impl Into<String>,
        node: impl Into<String>,
        platform: PushDevicePlatform,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            node: node.into(),
            platform,
            environment: environment.into(),
            provider_endpoint: None,
            provider_token: None,
            provider_key_material: None,
        }
    }

    pub fn with_provider_endpoint(mut self, provider_endpoint: Option<String>) -> Self {
        self.provider_endpoint = provider_endpoint;
        self
    }

    pub fn with_provider_token(mut self, provider_token: Option<String>) -> Self {
        self.provider_token = provider_token;
        self
    }

    pub fn with_provider_key_material(mut self, provider_key_material: Option<String>) -> Self {
        self.provider_key_material = provider_key_material;
        self
    }
}

#[derive(Debug, Clone)]
pub struct PushServiceDevice {
    device_id: String,
    node: String,
    platform: PushDevicePlatform,
    environment: String,
    provider_endpoint: Option<String>,
    #[cfg(test)]
    provider_token: Option<String>,
    #[cfg(test)]
    provider_key_material: Option<String>,
}

impl PushServiceDevice {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn provider_endpoint(&self) -> Option<&str> {
        self.provider_endpoint.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn provider_token(&self) -> Option<&str> {
        self.provider_token.as_deref()
    }

    pub fn platform(&self) -> PushDevicePlatform {
        self.platform
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    #[cfg(test)]
    pub(crate) fn provider_key_material(&self) -> Option<&str> {
        self.provider_key_material.as_deref()
    }
}

#[derive(Debug, Clone)]
pub struct PushFanoutResult {
    item_id: String,
    attempted_devices: usize,
}

impl PushFanoutResult {
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn attempted_devices(&self) -> usize {
        self.attempted_devices
    }
}

#[derive(Debug, Clone)]
pub struct PushDeliveryAttempt {
    attempt_id: String,
    node: String,
    device_id: String,
    item_id: String,
    status: String,
}

#[derive(Debug, Clone)]
pub struct PushPublishJob {
    job_id: String,
    owner_bare_jid: BareJid,
    node: String,
    item_id: String,
    push_service_jid: Option<String>,
    status: String,
}

#[derive(Debug, Clone)]
pub struct PushRegistrationCursor {
    owner_bare_jid: String,
    node: String,
}

#[derive(Debug, Clone)]
pub struct PushReconciliationResult {
    scanned_registrations: usize,
    enqueued_jobs: usize,
    next_cursor: Option<PushRegistrationCursor>,
}

#[derive(Debug, Clone)]
struct PushPublishJobRegistration {
    owner_bare_jid: BareJid,
    node: String,
    publish_options: Option<Element>,
}

#[derive(Debug, Clone)]
struct PushPublishJobEnqueue {
    item_id: String,
    queued: bool,
}

#[derive(Debug, Clone, Copy)]
enum PushPublishJobConflictMode {
    RefreshRetryable,
    InsertMissingOnly,
}

#[derive(Debug, Clone)]
struct PushDeliveryTarget {
    device_id: String,
    platform: PushDevicePlatform,
}

impl PushDeliveryAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn status(&self) -> &str {
        &self.status
    }
}

impl PushPublishJob {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    fn owner_bare_jid(&self) -> &BareJid {
        &self.owner_bare_jid
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    fn push_service_jid(&self) -> Option<&str> {
        self.push_service_jid.as_deref()
    }

    pub fn status(&self) -> &str {
        &self.status
    }
}

impl PushRegistrationCursor {
    pub fn owner_bare_jid(&self) -> &str {
        &self.owner_bare_jid
    }

    pub fn node(&self) -> &str {
        &self.node
    }
}

impl PushReconciliationResult {
    pub fn scanned_registrations(&self) -> usize {
        self.scanned_registrations
    }

    pub fn enqueued_jobs(&self) -> usize {
        self.enqueued_jobs
    }

    pub fn next_cursor(&self) -> Option<&PushRegistrationCursor> {
        self.next_cursor.as_ref()
    }
}

impl DatabasePushServiceStore {
    #[cfg(test)]
    pub async fn new(db: Database) -> Result<Self, XmppError> {
        Self::new_with_secret_key(db, b"waddle-push-service-test-secret-key").await
    }

    pub async fn new_with_secret_key(db: Database, secret_key: &[u8]) -> Result<Self, XmppError> {
        let store = Self {
            db,
            secrets: PushSecretCipher::new(secret_key),
        };
        store.initialize().await?;
        Ok(store)
    }

    pub fn database(&self) -> Database {
        self.db.clone()
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
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS push_publish_jobs (
                    job_id TEXT PRIMARY KEY,
                    owner_bare_jid TEXT NOT NULL,
                    push_service_jid TEXT,
                    node TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    payload_xml TEXT NOT NULL,
                    publish_options_xml TEXT,
                    status TEXT NOT NULL CHECK (status IN ('queued', 'in-progress', 'published', 'failed')),
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    next_retry_at_ms {i64_type},
                    claimed_at_ms {i64_type},
                    created_at_ms {i64_type} NOT NULL,
                    updated_at_ms {i64_type} NOT NULL,
                    published_at_ms {i64_type},
                    UNIQUE (node, item_id),
                    FOREIGN KEY (node) REFERENCES push_nodes(node) ON DELETE CASCADE
                )
                "#
            ),
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
        Ok(())
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

    pub async fn ensure_node(
        &self,
        owner_bare_jid: &BareJid,
        app_id: &str,
    ) -> Result<PushServiceNode, XmppError> {
        if app_id.is_empty() {
            return Err(XmppError::bad_request(Some(
                "Push Service app-id is required".to_string(),
            )));
        }
        validate_len("Push Service app-id", app_id, MAX_APP_ID_LEN)?;
        if let Some(node) = self.find_node_by_owner_app(owner_bare_jid, app_id).await? {
            if node.status == PushNodeStatus::Active {
                return Ok(node);
            }
        }

        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;
        if let Some(node) = find_node_by_owner_app_tx(&mut tx, owner_bare_jid, app_id).await? {
            if node.status == PushNodeStatus::Active {
                tx.commit()
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                return Ok(node);
            }
            if count_active_nodes_for_owner_tx(&mut tx, owner_bare_jid).await?
                >= MAX_PUSH_NODES_PER_OWNER
            {
                return Err(XmppError::bad_request(Some(format!(
                    "Push Service active node quota exceeded; max {MAX_PUSH_NODES_PER_OWNER} active nodes per owner"
                ))));
            }
            tx.execute(
                r#"
                UPDATE push_nodes
                SET status = ?, updated_at_ms = ?
                WHERE node = ?
                "#,
                crate::db_params![PushNodeStatus::Active.as_str(), now_ms, node.node()],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
            let node = get_node_tx(&mut tx, node.node())
                .await?
                .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(node);
        }
        prune_disabled_nodes_for_owner_tx(
            &mut tx,
            owner_bare_jid,
            MAX_RETAINED_DISABLED_NODES_PER_OWNER,
        )
        .await?;
        if count_active_nodes_for_owner_tx(&mut tx, owner_bare_jid).await?
            >= MAX_PUSH_NODES_PER_OWNER
        {
            return Err(XmppError::bad_request(Some(format!(
                "Push Service active node quota exceeded; max {MAX_PUSH_NODES_PER_OWNER} active nodes per owner"
            ))));
        }

        let node_name = format!("urn:waddle:push-node:{}", uuid::Uuid::new_v4());
        tx.execute(
            r#"
            INSERT INTO push_nodes (
                node,
                owner_bare_jid,
                app_id,
                status,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(owner_bare_jid, app_id) DO NOTHING
            "#,
            crate::db_params![
                node_name,
                owner_bare_jid.to_string(),
                app_id,
                PushNodeStatus::Active.as_str(),
                now_ms,
                now_ms,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;

        let node = find_node_by_owner_app_tx(&mut tx, owner_bare_jid, app_id)
            .await?
            .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(node)
    }

    pub async fn get_node(&self, node: &str) -> Result<Option<PushServiceNode>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
                FROM push_nodes
                WHERE node = ?
                "#,
                crate::db_params![node],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_node(&row)?))
    }

    pub async fn get_node_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
    ) -> Result<Option<PushServiceNode>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
                FROM push_nodes
                WHERE owner_bare_jid = ? AND node = ? AND status = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), node, NODE_STATUS_ACTIVE],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_node(&row)?))
    }

    pub async fn validate_first_party_enable_node(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
    ) -> Result<(), XmppError> {
        validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        validate_first_party_enable_node_tx(&mut tx, owner_bare_jid, node, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }

    pub async fn register_first_party_node_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        service_jid: &str,
        node: &str,
        publish_options: Option<&Element>,
    ) -> Result<(), XmppError> {
        validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;
        validate_first_party_enable_node_tx(&mut tx, owner_bare_jid, node, now_ms).await?;
        crate::push_registrations::register_subscription_tx(
            &mut tx,
            &PushSubscription {
                user_jid: owner_bare_jid.to_string(),
                service_jid: service_jid.to_string(),
                node: Some(node.to_string()),
                publish_options: publish_options.cloned(),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            },
        )
        .await
        .map_err(push_error_to_xmpp_error)?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }

    pub async fn list_node_names_for_owner(
        &self,
        owner_bare_jid: &BareJid,
    ) -> Result<Vec<String>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node
                FROM push_nodes
                WHERE owner_bare_jid = ? AND status = ?
                ORDER BY node ASC
                "#,
                crate::db_params![owner_bare_jid.to_string(), NODE_STATUS_ACTIVE],
            )
            .await?;
        let mut nodes = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            nodes.push(
                row.get(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
        Ok(nodes)
    }

    pub async fn upsert_device(
        &self,
        owner_bare_jid: &BareJid,
        registration: PushDeviceRegistration,
    ) -> Result<PushServiceDevice, XmppError> {
        if registration.device_id.is_empty()
            || registration.node.is_empty()
            || registration.environment.is_empty()
        {
            return Err(XmppError::bad_request(Some(
                "Push Service device-id, node, and environment are required".to_string(),
            )));
        }
        validate_device_registration(&registration)?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if get_node_tx(&mut tx, &registration.node).await?.is_none() {
            return Err(XmppError::item_not_found(Some(
                "Push node not found".to_string(),
            )));
        }
        lock_node_tx(&mut tx, &registration.node, now_ms).await?;
        let push_node = get_node_tx(&mut tx, &registration.node)
            .await?
            .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
        if push_node.owner_bare_jid != *owner_bare_jid {
            return Err(XmppError::forbidden(Some(
                "Push node belongs to another user".to_string(),
            )));
        }
        if push_node.status != PushNodeStatus::Active {
            return Err(XmppError::item_not_found(Some(
                "Push node not active".to_string(),
            )));
        }
        prune_disabled_devices_for_node_tx(
            &mut tx,
            &registration.node,
            MAX_RETAINED_DISABLED_DEVICES_PER_NODE,
        )
        .await?;
        if !active_device_exists_on_node_tx(&mut tx, &registration.node, &registration.device_id)
            .await?
            && count_active_devices_for_node_tx(&mut tx, &registration.node).await?
                >= MAX_PUSH_DEVICES_PER_NODE
        {
            return Err(XmppError::bad_request(Some(format!(
                "Push Service active device quota exceeded; max {MAX_PUSH_DEVICES_PER_NODE} active devices per node"
            ))));
        }

        let sealed_provider_endpoint = self
            .secrets
            .seal_optional(registration.provider_endpoint.clone())?;
        let sealed_provider_token = self
            .secrets
            .seal_optional(registration.provider_token.clone())?;
        let sealed_provider_key_material = self
            .secrets
            .seal_optional(registration.provider_key_material.clone())?;
        let device_id = registration.device_id.clone();
        tx.execute(
            r#"
            INSERT INTO push_devices (
                device_id,
                node,
                platform,
                environment,
                provider_endpoint,
                provider_token,
                provider_key_material,
                status,
                failure_count,
                last_error,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, ?)
            ON CONFLICT(node, device_id) DO UPDATE SET
                platform = excluded.platform,
                environment = excluded.environment,
                provider_endpoint = excluded.provider_endpoint,
                provider_token = excluded.provider_token,
                provider_key_material = excluded.provider_key_material,
                status = excluded.status,
                failure_count = 0,
                last_error = NULL,
                updated_at_ms = excluded.updated_at_ms
            "#,
            crate::db_params![
                device_id.clone(),
                registration.node.clone(),
                registration.platform.to_string(),
                registration.environment.clone(),
                sealed_provider_endpoint,
                sealed_provider_token,
                sealed_provider_key_material,
                DEVICE_STATUS_ACTIVE,
                now_ms,
                now_ms,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
        wake_queued_publish_jobs_for_node_tx(&mut tx, &registration.node, now_ms).await?;
        let device = get_device_for_owner_on_node_tx(
            &mut tx,
            owner_bare_jid,
            &registration.node,
            &device_id,
            &self.secrets,
        )
        .await?
        .ok_or_else(|| XmppError::internal("Push Service device was not persisted"))?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(device)
    }

    pub async fn disable_device_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
        device_id: &str,
        error: Option<&str>,
    ) -> Result<bool, XmppError> {
        validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        validate_len("Push Service device-id", device_id, MAX_DEVICE_ID_LEN)?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if get_node_tx(&mut tx, node).await?.is_none() {
            return Err(XmppError::item_not_found(Some(
                "Push node not found".to_string(),
            )));
        }
        lock_node_tx(&mut tx, node, now_ms).await?;
        let push_node = get_node_tx(&mut tx, node)
            .await?
            .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
        if push_node.owner_bare_jid != *owner_bare_jid {
            return Err(XmppError::forbidden(Some(
                "Push node belongs to another user".to_string(),
            )));
        }
        if push_node.status != PushNodeStatus::Active {
            return Ok(false);
        }
        let affected = tx
            .execute(
                r#"
                UPDATE push_devices
                SET status = ?,
                    provider_endpoint = NULL,
                    provider_token = NULL,
                    provider_key_material = NULL,
                    last_error = ?,
                    updated_at_ms = ?
                WHERE device_id = ?
                  AND node = ?
                "#,
                crate::db_params![
                    DEVICE_STATUS_DISABLED,
                    error.map(str::to_owned),
                    now_ms,
                    device_id,
                    node,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(affected > 0)
    }

    #[cfg(test)]
    pub async fn get_device_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        device_id: &str,
    ) -> Result<Option<PushServiceDevice>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT d.device_id, d.node, d.platform, d.environment,
                       d.provider_endpoint, d.provider_token, d.provider_key_material
                FROM push_devices d
                JOIN push_nodes n ON n.node = d.node
                WHERE n.owner_bare_jid = ? AND d.device_id = ?
                ORDER BY d.node ASC
                LIMIT 1
                "#,
                crate::db_params![owner_bare_jid.to_string(), device_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_device(&row, &self.secrets)?))
    }

    pub async fn disable_nodes_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: Option<&str>,
    ) -> Result<u64, XmppError> {
        if let Some(node) = node {
            validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        }
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;

        let nodes = match node {
            Some(node) => {
                if get_node_for_owner_tx(&mut tx, owner_bare_jid, node)
                    .await?
                    .is_some()
                {
                    vec![node.to_string()]
                } else {
                    tx.commit()
                        .await
                        .map_err(|error| XmppError::internal(error.to_string()))?;
                    return Ok(0);
                }
            }
            None => node_names_for_owner_tx(&mut tx, owner_bare_jid).await?,
        };

        let mut affected_devices = 0;
        for node in &nodes {
            lock_node_tx(&mut tx, node, now_ms).await?;
            affected_devices += tx
                .execute(
                    r#"
                    UPDATE push_devices
                    SET status = ?,
                        provider_endpoint = NULL,
                        provider_token = NULL,
                        provider_key_material = NULL,
                        last_error = ?,
                        updated_at_ms = ?
                    WHERE node = ?
                    "#,
                    crate::db_params![
                        DEVICE_STATUS_DISABLED,
                        "disabled via Push Service admin",
                        now_ms,
                        node,
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.execute(
                r#"
                UPDATE push_nodes
                SET status = ?, updated_at_ms = ?
                WHERE node = ?
                "#,
                crate::db_params![PushNodeStatus::Disabled.as_str(), now_ms, node],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(affected_devices)
    }

    pub async fn remove_registered_nodes_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        service_jid: &str,
        node: Option<&str>,
    ) -> Result<u64, XmppError> {
        if let Some(node) = node {
            validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        }
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;

        let registered_nodes = crate::push_registrations::registered_nodes_for_disable_tx(
            &mut tx,
            owner_bare_jid,
            service_jid,
            node,
        )
        .await
        .map_err(push_error_to_xmpp_error)?;
        if registered_nodes.is_empty() {
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(0);
        }

        let mut registered_push_nodes = registered_nodes
            .iter()
            .filter(|node| !node.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        registered_push_nodes.sort();
        registered_push_nodes.dedup();
        for node in &registered_push_nodes {
            lock_node_tx(&mut tx, node, now_ms).await?;
            delete_retryable_publish_jobs_for_node_tx(&mut tx, owner_bare_jid, node).await?;
        }
        let removed = crate::push_registrations::remove_subscription_tx(
            &mut tx,
            owner_bare_jid,
            service_jid,
            node,
        )
        .await
        .map_err(push_error_to_xmpp_error)?;

        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(removed)
    }

    #[cfg(test)]
    async fn get_device_for_owner_on_node(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
        device_id: &str,
    ) -> Result<Option<PushServiceDevice>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT d.device_id, d.node, d.platform, d.environment,
                       d.provider_endpoint, d.provider_token, d.provider_key_material
                FROM push_devices d
                JOIN push_nodes n ON n.node = d.node
                WHERE n.owner_bare_jid = ? AND d.node = ? AND d.device_id = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), node, device_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_device(&row, &self.secrets)?))
    }

    /// Enqueue and immediately try a trusted user-server XEP-0357 publish job.
    ///
    /// Client stanza ingress MUST NOT call this directly. XEP-0357 warns Push
    /// Services not to accept publishes from third-party client full JIDs; the
    /// caller is expected to be the durable user-server notification publisher.
    pub async fn publish_notification_from_user_server(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
    ) -> Result<PushFanoutResult, XmppError> {
        self.publish_notification_from_user_server_with_publish_options(node, item, publisher, None)
            .await
    }

    pub async fn publish_notification_from_user_server_with_publish_options(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        publish_options: Option<&Element>,
    ) -> Result<PushFanoutResult, XmppError> {
        self.publish_notification_from_user_server_with_retention_limit(
            node,
            item,
            publisher,
            None,
            publish_options,
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
    }

    pub async fn publish_registered_notification_from_user_server_with_publish_options(
        &self,
        push_service_jid: &str,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        publish_options: Option<&Element>,
    ) -> Result<PushFanoutResult, XmppError> {
        self.publish_notification_from_user_server_with_retention_limit(
            node,
            item,
            publisher,
            Some(push_service_jid),
            publish_options,
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
    }

    pub async fn publish_xep0357_pubsub_iq_from_user_server(
        &self,
        push_service_jid: &str,
        iq: &Iq,
        publisher: &BareJid,
    ) -> Result<PushFanoutResult, XmppError> {
        if !matches!(iq.payload, IqType::Set(_)) {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish requires an IQ set".to_string(),
            )));
        }
        if iq
            .from
            .as_ref()
            .is_some_and(|from| from.to_bare() != *publisher)
        {
            return Err(XmppError::forbidden(Some(
                "XEP-0357 Push Service publish sender does not match publisher".to_string(),
            )));
        }
        if iq
            .to
            .as_ref()
            .is_some_and(|to| to.to_string() != push_service_jid)
        {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish target does not match service".to_string(),
            )));
        }

        match waddle_xmpp::pubsub::parse_pubsub_iq(iq)
            .map_err(|error| XmppError::bad_request(Some(error.to_string())))?
        {
            PubSubRequest::Publish {
                node,
                item,
                publish_options,
            } => {
                self.publish_registered_notification_from_user_server_with_publish_options(
                    push_service_jid,
                    &node,
                    &item,
                    publisher,
                    publish_options.as_deref(),
                )
                .await
            }
            _ => Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish requires a PubSub publish request".to_string(),
            ))),
        }
    }

    async fn publish_notification_from_user_server_with_retention_limit(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
        retention_limit: i64,
    ) -> Result<PushFanoutResult, XmppError> {
        let enqueue = self
            .enqueue_notification_publish_job_from_user_server_with_publish_options(
                node,
                item,
                publisher,
                push_service_jid,
                publish_options,
            )
            .await?;

        match self
            .process_publish_job_by_node_item_with_retention_limit(
                node,
                &enqueue.item_id,
                retention_limit,
            )
            .await
        {
            Ok(Some(result)) => Ok(result),
            Ok(None) => Ok(PushFanoutResult {
                item_id: enqueue.item_id,
                attempted_devices: 0,
            }),
            Err(error) => {
                self.record_publish_job_failure(node, &enqueue.item_id, &error.to_string())
                    .await?;
                Err(error)
            }
        }
    }

    #[cfg(test)]
    async fn enqueue_notification_publish_job_from_user_server(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        self.enqueue_notification_publish_job_from_user_server_with_publish_options(
            node, item, publisher, None, None,
        )
        .await
    }

    async fn enqueue_notification_publish_job_from_user_server_with_publish_options(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        self.enqueue_notification_publish_job_with_conflict_mode(
            node,
            item,
            publisher,
            push_service_jid,
            publish_options,
            PushPublishJobConflictMode::RefreshRetryable,
        )
        .await
    }

    async fn enqueue_missing_notification_publish_job_from_user_server(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        self.enqueue_notification_publish_job_with_conflict_mode(
            node,
            item,
            publisher,
            push_service_jid,
            publish_options,
            PushPublishJobConflictMode::InsertMissingOnly,
        )
        .await
    }

    async fn enqueue_notification_publish_job_with_conflict_mode(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
        conflict_mode: PushPublishJobConflictMode,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let now_ms = crate::time::now_ms();
        lock_owner_tx(&mut tx, publisher, now_ms).await?;
        lock_node_tx(&mut tx, node, now_ms).await?;
        let push_node = get_node_tx(&mut tx, node)
            .await?
            .ok_or_else(|| XmppError::item_not_found(Some("Push node not found".to_string())))?;
        if push_node.status != PushNodeStatus::Active {
            return Err(XmppError::item_not_found(Some(
                "Push node not active".to_string(),
            )));
        }
        if push_node.owner_bare_jid != *publisher {
            return Err(XmppError::forbidden(Some(
                "Only the node owner may publish Push Service notifications".to_string(),
            )));
        }
        if let Some(push_service_jid) = push_service_jid {
            ensure_active_registration_tx(&mut tx, publisher, push_service_jid, node).await?;
        }
        validate_xep0357_notification(item)?;
        if let Some(item_id) = item.id.as_deref() {
            validate_len("XEP-0060 item id", item_id, MAX_PUBSUB_ITEM_ID_LEN)?;
        }

        let item_id = item
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let payload_xml = item
            .payload
            .as_ref()
            .map(String::from)
            .ok_or_else(|| XmppError::internal("validated XEP-0357 item missing payload"))?;
        let publish_options_xml = publish_options.map(String::from);
        let job_id = uuid::Uuid::new_v4().to_string();
        let changed = match conflict_mode {
            PushPublishJobConflictMode::RefreshRetryable => tx
                .execute(
                    r#"
                    INSERT INTO push_publish_jobs (
                        job_id,
                        owner_bare_jid,
                        push_service_jid,
                        node,
                        item_id,
                        payload_xml,
                        publish_options_xml,
                        status,
                        attempt_count,
                        last_error,
                        next_retry_at_ms,
                        claimed_at_ms,
                        created_at_ms,
                        updated_at_ms,
                        published_at_ms
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL, NULL, ?, ?, NULL)
                    ON CONFLICT(node, item_id) DO UPDATE SET
                        push_service_jid = excluded.push_service_jid,
                        payload_xml = excluded.payload_xml,
                        publish_options_xml = excluded.publish_options_xml,
                        status = ?,
                        last_error = NULL,
                        next_retry_at_ms = NULL,
                        claimed_at_ms = NULL,
                        updated_at_ms = excluded.updated_at_ms,
                        published_at_ms = NULL
                    WHERE push_publish_jobs.status IN (?, ?)
                    "#,
                    crate::db_params![
                        job_id,
                        publisher.to_string(),
                        push_service_jid,
                        node,
                        item_id.clone(),
                        payload_xml,
                        publish_options_xml.clone(),
                        PUBLISH_JOB_STATUS_QUEUED,
                        now_ms,
                        now_ms,
                        PUBLISH_JOB_STATUS_QUEUED,
                        PUBLISH_JOB_STATUS_QUEUED,
                        PUBLISH_JOB_STATUS_FAILED,
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?,
            PushPublishJobConflictMode::InsertMissingOnly => tx
                .execute(
                    r#"
                    INSERT INTO push_publish_jobs (
                        job_id,
                        owner_bare_jid,
                        push_service_jid,
                        node,
                        item_id,
                        payload_xml,
                        publish_options_xml,
                        status,
                        attempt_count,
                        last_error,
                        next_retry_at_ms,
                        claimed_at_ms,
                        created_at_ms,
                        updated_at_ms,
                        published_at_ms
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL, NULL, ?, ?, NULL)
                    ON CONFLICT(node, item_id) DO NOTHING
                    "#,
                    crate::db_params![
                        job_id,
                        publisher.to_string(),
                        push_service_jid,
                        node,
                        item_id.clone(),
                        payload_xml,
                        publish_options_xml,
                        PUBLISH_JOB_STATUS_QUEUED,
                        now_ms,
                        now_ms,
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?,
        };
        prune_publish_jobs_tx(&mut tx, node, MAX_PUBLISH_JOBS_PER_NODE).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

        Ok(PushPublishJobEnqueue {
            item_id,
            queued: changed > 0,
        })
    }

    pub async fn drain_queued_notification_publish_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<PushFanoutResult>, XmppError> {
        self.drain_queued_notification_publish_jobs_with_retention_limit(
            limit,
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
    }

    async fn drain_queued_notification_publish_jobs_with_retention_limit(
        &self,
        limit: usize,
        retention_limit: i64,
    ) -> Result<Vec<PushFanoutResult>, XmppError> {
        self.recover_stale_publish_job_claims().await?;
        let now_ms = crate::time::now_ms();
        let mut rows = self
            .query(
                r#"
                SELECT job_id
                FROM push_publish_jobs
                WHERE status = ?
                  AND (next_retry_at_ms IS NULL OR next_retry_at_ms <= ?)
                ORDER BY created_at_ms ASC, job_id ASC
                LIMIT ?
                "#,
                crate::db_params![PUBLISH_JOB_STATUS_QUEUED, now_ms, limit as i64],
            )
            .await?;
        let mut job_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            job_ids.push(
                row.get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }

        let mut results = Vec::new();
        for job_id in job_ids {
            match self
                .process_publish_job_with_retention_limit(&job_id, retention_limit)
                .await
            {
                Ok(Some(result)) => results.push(result),
                Ok(None) => {}
                Err(error) => {
                    self.record_publish_job_failure_by_id(&job_id, &error.to_string())
                        .await?;
                }
            }
        }
        Ok(results)
    }

    async fn recover_stale_publish_job_claims(&self) -> Result<(), XmppError> {
        let now_ms = crate::time::now_ms();
        let retry_at_ms = retry_at_ms(now_ms);
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                last_error = ?,
                next_retry_at_ms = ?,
                claimed_at_ms = NULL,
                updated_at_ms = ?
            WHERE status = ?
              AND claimed_at_ms IS NOT NULL
              AND claimed_at_ms <= ?
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_QUEUED,
                "Push publish job claim expired before completion",
                retry_at_ms,
                now_ms,
                PUBLISH_JOB_STATUS_IN_PROGRESS,
                now_ms - PUBLISH_JOB_CLAIM_TIMEOUT_MS,
            ],
        )
        .await?;
        Ok(())
    }

    async fn recover_stale_publish_job_claim_by_id(&self, job_id: &str) -> Result<(), XmppError> {
        let now_ms = crate::time::now_ms();
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                last_error = ?,
                next_retry_at_ms = NULL,
                claimed_at_ms = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claimed_at_ms IS NOT NULL
              AND claimed_at_ms <= ?
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_QUEUED,
                "Push publish job claim expired before direct publish retry",
                now_ms,
                job_id,
                PUBLISH_JOB_STATUS_IN_PROGRESS,
                now_ms - PUBLISH_JOB_CLAIM_TIMEOUT_MS,
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn reconcile_pending_delivery_notification_jobs(
        &self,
        pending_delivery_storage: &dyn PendingDeliveryStorage,
        first_party_service_jid: &str,
        after: Option<&PushRegistrationCursor>,
        registration_limit: usize,
        pending_rows_per_registration: usize,
    ) -> Result<PushReconciliationResult, XmppError> {
        let registration_limit = registration_limit.clamp(1, 1_000);
        let pending_rows_per_registration = pending_rows_per_registration.clamp(1, 1_000);
        let pending_scan_limit = pending_rows_per_registration
            .saturating_mul(PUSH_RECONCILIATION_SCAN_FACTOR)
            .clamp(pending_rows_per_registration, 10_000);
        let mut registrations = self
            .active_first_party_registrations(
                first_party_service_jid,
                after,
                registration_limit + 1,
            )
            .await?;
        let has_more_registrations = registrations.len() > registration_limit;
        if has_more_registrations {
            registrations.truncate(registration_limit);
        }
        let next_cursor = has_more_registrations
            .then(|| {
                let registration = registrations.last()?;
                Some(PushRegistrationCursor {
                    owner_bare_jid: registration.owner_bare_jid.to_string(),
                    node: registration.node.clone(),
                })
            })
            .flatten();
        let scanned_registrations = registrations.len();
        let mut enqueued_jobs = 0_usize;

        for registration in registrations {
            let mut enqueued_jobs_for_registration = 0_usize;
            let mut after_pending_row: Option<waddle_xmpp::pending_delivery::PendingRowId> = None;
            'pending_pages: loop {
                let pending_rows = match pending_delivery_storage
                    .list_unclaimed_after(
                        &registration.owner_bare_jid,
                        after_pending_row.as_ref(),
                        pending_scan_limit,
                    )
                    .await
                {
                    Ok(rows) => rows,
                    Err(error) => {
                        warn!(
                            owner_bare_jid = %registration.owner_bare_jid,
                            node = %registration.node,
                            error = %error,
                            "Skipping first-party push registration after pending_delivery read failure"
                        );
                        break;
                    }
                };
                if pending_rows.is_empty() {
                    break;
                }
                let page_len = pending_rows.len();
                for row in pending_rows.into_iter() {
                    after_pending_row = Some(row.id.clone());
                    if row.flushed_in_session.is_some() {
                        continue;
                    }
                    let notification = Element::builder("notification", NS_PUSH).build();
                    let item =
                        PubSubItem::new(Some(row.id.as_str().to_string()), Some(notification));
                    let enqueue = match self
                        .enqueue_missing_notification_publish_job_from_user_server(
                            &registration.node,
                            &item,
                            &registration.owner_bare_jid,
                            Some(first_party_service_jid),
                            registration.publish_options.as_ref(),
                        )
                        .await
                    {
                        Ok(enqueue) => enqueue,
                        Err(error) => {
                            warn!(
                                owner_bare_jid = %registration.owner_bare_jid,
                                node = %registration.node,
                                item_id = %row.id.as_str(),
                                error = %error,
                                "Skipping stale first-party push registration during reconciliation"
                            );
                            break 'pending_pages;
                        }
                    };
                    if enqueue.queued {
                        enqueued_jobs += 1;
                        enqueued_jobs_for_registration += 1;
                        if enqueued_jobs_for_registration >= pending_rows_per_registration {
                            break 'pending_pages;
                        }
                    }
                }
                if page_len < pending_scan_limit {
                    break;
                }
            }
        }

        Ok(PushReconciliationResult {
            scanned_registrations,
            enqueued_jobs,
            next_cursor,
        })
    }

    async fn process_publish_job_by_node_item_with_retention_limit(
        &self,
        node: &str,
        item_id: &str,
        retention_limit: i64,
    ) -> Result<Option<PushFanoutResult>, XmppError> {
        let Some(job_id) = self.publish_job_id_for_node_item(node, item_id).await? else {
            return Ok(None);
        };
        self.recover_stale_publish_job_claim_by_id(&job_id).await?;
        self.process_publish_job_with_retention_limit(&job_id, retention_limit)
            .await
    }

    async fn process_publish_job_with_retention_limit(
        &self,
        job_id: &str,
        retention_limit: i64,
    ) -> Result<Option<PushFanoutResult>, XmppError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let Some(lock_target) = get_publish_job_tx(&mut tx, job_id).await? else {
            return Ok(None);
        };
        lock_owner_tx(&mut tx, lock_target.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, lock_target.node(), now_ms).await?;
        let Some(job) = claim_publish_job_tx(&mut tx, job_id, now_ms).await? else {
            return Ok(None);
        };
        let Some(push_node) = get_node_tx(&mut tx, job.node()).await? else {
            mark_publish_job_failed_tx(&mut tx, job.job_id(), "Push node not found", now_ms)
                .await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            }));
        };
        if push_node.status != PushNodeStatus::Active {
            mark_publish_job_failed_tx(&mut tx, job.job_id(), "Push node not active", now_ms)
                .await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            }));
        }
        if push_node.owner_bare_jid != *job.owner_bare_jid() {
            return Err(XmppError::forbidden(Some(
                "Push publish job owner does not match node owner".to_string(),
            )));
        }
        if let Some(push_service_jid) = job.push_service_jid() {
            if let Err(error) = ensure_active_registration_tx(
                &mut tx,
                job.owner_bare_jid(),
                push_service_jid,
                job.node(),
            )
            .await
            {
                if matches!(
                    error,
                    XmppError::Stanza {
                        condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
                        ..
                    }
                ) {
                    mark_publish_job_failed_tx(
                        &mut tx,
                        job.job_id(),
                        "XEP-0357 registration not active",
                        now_ms,
                    )
                    .await?;
                    tx.commit()
                        .await
                        .map_err(|error| XmppError::internal(error.to_string()))?;
                    return Ok(Some(PushFanoutResult {
                        item_id: job.item_id().to_string(),
                        attempted_devices: 0,
                    }));
                }
                return Err(error);
            }
        }

        let active_devices = active_devices_for_node_tx(&mut tx, job.node()).await?;
        if active_devices.is_empty() {
            let retry_at_ms = retry_at_ms(now_ms);
            tx.execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    attempt_count = attempt_count + 1,
                    last_error = ?,
                    next_retry_at_ms = ?,
                    claimed_at_ms = NULL,
                    updated_at_ms = ?
                WHERE job_id = ? AND status = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_QUEUED,
                    PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES,
                    retry_at_ms,
                    now_ms,
                    job.job_id().to_string(),
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            }));
        }
        for device in &active_devices {
            tx.execute(
                r#"
                INSERT INTO push_delivery_attempts (
                    attempt_id,
                    node,
                    device_id,
                    platform,
                    item_id,
                    status,
                    last_error,
                    created_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, NULL, ?)
                "#,
                crate::db_params![
                    uuid::Uuid::new_v4().to_string(),
                    job.node().to_string(),
                    device.device_id.clone(),
                    device.platform.to_string(),
                    job.item_id().to_string(),
                    ATTEMPT_STATUS_FAKE_SENT,
                    now_ms,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }
        tx.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                attempt_count = attempt_count + 1,
                last_error = NULL,
                next_retry_at_ms = NULL,
                claimed_at_ms = NULL,
                updated_at_ms = ?,
                published_at_ms = ?
            WHERE job_id = ? AND status = ?
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_PUBLISHED,
                now_ms,
                now_ms,
                job.job_id().to_string(),
                PUBLISH_JOB_STATUS_IN_PROGRESS,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
        prune_delivery_attempts_tx(&mut tx, job.node(), retention_limit).await?;
        prune_publish_jobs_tx(&mut tx, job.node(), MAX_PUBLISH_JOBS_PER_NODE).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

        Ok(Some(PushFanoutResult {
            item_id: job.item_id().to_string(),
            attempted_devices: active_devices.len(),
        }))
    }

    async fn publish_job_id_for_node_item(
        &self,
        node: &str,
        item_id: &str,
    ) -> Result<Option<String>, XmppError> {
        let mut rows = self
            .query(
                "SELECT job_id FROM push_publish_jobs WHERE node = ? AND item_id = ?",
                crate::db_params![node, item_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        row.get(0)
            .map(Some)
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn record_publish_job_failure(
        &self,
        node: &str,
        item_id: &str,
        error: &str,
    ) -> Result<(), XmppError> {
        let Some(job_id) = self.publish_job_id_for_node_item(node, item_id).await? else {
            return Ok(());
        };
        self.record_publish_job_failure_by_id(&job_id, error).await
    }

    async fn record_publish_job_failure_by_id(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<(), XmppError> {
        let now_ms = crate::time::now_ms();
        let next_retry_at_ms = retry_at_ms(now_ms);
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                attempt_count = attempt_count + 1,
                last_error = ?,
                next_retry_at_ms = ?,
                claimed_at_ms = NULL,
                updated_at_ms = ?
            WHERE job_id = ? AND status IN (?, ?)
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_QUEUED,
                error,
                next_retry_at_ms,
                now_ms,
                job_id,
                PUBLISH_JOB_STATUS_QUEUED,
                PUBLISH_JOB_STATUS_IN_PROGRESS,
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn queued_publish_jobs(&self) -> Result<Vec<PushPublishJob>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status
                FROM push_publish_jobs
                WHERE status = ?
                ORDER BY created_at_ms ASC, job_id ASC
                "#,
                crate::db_params![PUBLISH_JOB_STATUS_QUEUED],
            )
            .await?;
        let mut jobs = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            jobs.push(decode_publish_job(&row)?);
        }
        Ok(jobs)
    }

    pub async fn delivery_attempts_for_node(
        &self,
        node: &str,
    ) -> Result<Vec<PushDeliveryAttempt>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT attempt_id, node, device_id, item_id, status
                FROM push_delivery_attempts
                WHERE node = ?
                ORDER BY created_at_ms ASC, attempt_id ASC
                "#,
                crate::db_params![node],
            )
            .await?;
        let mut attempts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            attempts.push(decode_attempt(&row)?);
        }
        Ok(attempts)
    }

    async fn active_first_party_registrations(
        &self,
        first_party_service_jid: &str,
        after: Option<&PushRegistrationCursor>,
        limit: usize,
    ) -> Result<Vec<PushPublishJobRegistration>, XmppError> {
        let mut rows = if let Some(cursor) = after {
            self.query(
                r#"
                SELECT owner_bare_jid, node, publish_options_xml
                FROM push_registrations
                WHERE push_service_jid = ?
                  AND status = ?
                  AND node != ''
                  AND (
                      owner_bare_jid > ?
                      OR (owner_bare_jid = ? AND node > ?)
                  )
                ORDER BY owner_bare_jid ASC, node ASC
                LIMIT ?
                "#,
                crate::db_params![
                    first_party_service_jid,
                    crate::push_registrations::STATUS_ENABLED,
                    cursor.owner_bare_jid(),
                    cursor.owner_bare_jid(),
                    cursor.node(),
                    limit as i64,
                ],
            )
            .await?
        } else {
            self.query(
                r#"
                SELECT owner_bare_jid, node, publish_options_xml
                FROM push_registrations
                WHERE push_service_jid = ?
                  AND status = ?
                  AND node != ''
                ORDER BY owner_bare_jid ASC, node ASC
                LIMIT ?
                "#,
                crate::db_params![
                    first_party_service_jid,
                    crate::push_registrations::STATUS_ENABLED,
                    limit as i64,
                ],
            )
            .await?
        };
        let mut registrations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            let owner_bare_jid: String = row
                .get(0)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            registrations.push(PushPublishJobRegistration {
                owner_bare_jid: owner_bare_jid.parse().map_err(|error| {
                    XmppError::internal(format!(
                        "Invalid stored first-party push registration owner JID: {error}"
                    ))
                })?,
                node: row
                    .get(1)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
                publish_options: row
                    .get::<Option<String>>(2)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    .map(|xml| {
                        xml.parse::<Element>()
                            .map_err(|error| XmppError::internal(error.to_string()))
                    })
                    .transpose()?,
            });
        }
        Ok(registrations)
    }

    async fn find_node_by_owner_app(
        &self,
        owner_bare_jid: &BareJid,
        app_id: &str,
    ) -> Result<Option<PushServiceNode>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
                FROM push_nodes
                WHERE owner_bare_jid = ? AND app_id = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), app_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_node(&row)?))
    }

    async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    async fn query(
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

fn validate_xep0357_notification(item: &PubSubItem) -> Result<(), XmppError> {
    let Some(payload) = item.payload.as_ref() else {
        return Err(XmppError::bad_request(Some(
            "XEP-0357 PubSub publish requires a notification payload".to_string(),
        )));
    };
    if payload.name() != "notification" || payload.ns() != NS_PUSH {
        return Err(XmppError::bad_request(Some(
            "XEP-0357 PubSub publish payload must be notification in urn:xmpp:push:0".to_string(),
        )));
    }
    Ok(())
}

fn validate_device_registration(registration: &PushDeviceRegistration) -> Result<(), XmppError> {
    validate_len(
        "Push Service device-id",
        &registration.device_id,
        MAX_DEVICE_ID_LEN,
    )?;
    validate_len("Push Service node", &registration.node, MAX_NODE_ID_LEN)?;
    validate_len(
        "Push Service environment",
        &registration.environment,
        MAX_ENVIRONMENT_LEN,
    )?;
    validate_optional_len(
        "Push Service provider endpoint",
        registration.provider_endpoint.as_deref(),
        MAX_PROVIDER_ENDPOINT_LEN,
    )?;
    validate_optional_len(
        "Push Service provider token",
        registration.provider_token.as_deref(),
        MAX_PROVIDER_TOKEN_LEN,
    )?;
    validate_optional_len(
        "Push Service provider key material",
        registration.provider_key_material.as_deref(),
        MAX_PROVIDER_KEY_MATERIAL_LEN,
    )?;
    Ok(())
}

fn validate_optional_len(field: &str, value: Option<&str>, max: usize) -> Result<(), XmppError> {
    if let Some(value) = value {
        validate_len(field, value, max)?;
    }
    Ok(())
}

fn validate_len(field: &str, value: &str, max: usize) -> Result<(), XmppError> {
    if value.len() > max {
        return Err(XmppError::bad_request(Some(format!(
            "{field} exceeds {max} bytes"
        ))));
    }
    Ok(())
}

fn push_error_to_xmpp_error(error: PushError) -> XmppError {
    match error {
        PushError::StorageError(message)
            if message.contains("provider credential fields are not allowed") =>
        {
            XmppError::bad_request(Some(message))
        }
        other => XmppError::internal(other.to_string()),
    }
}

async fn lock_owner_tx(
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

async fn lock_node_tx(
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

async fn ensure_active_registration_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    push_service_jid: &str,
    node: &str,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT 1
            FROM push_registrations
            WHERE owner_bare_jid = ?
              AND push_service_jid = ?
              AND node = ?
              AND status = ?
            LIMIT 1
            "#,
            crate::db_params![
                owner_bare_jid.to_string(),
                push_service_jid,
                node,
                crate::push_registrations::STATUS_ENABLED,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    if rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    Err(XmppError::item_not_found(Some(
        "XEP-0357 registration not active".to_string(),
    )))
}

async fn validate_first_party_enable_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    if get_node_tx(tx, node).await?.is_none() {
        return Err(XmppError::item_not_found(Some(
            "Push node not found".to_string(),
        )));
    }
    lock_node_tx(tx, node, now_ms).await?;
    let push_node = get_node_tx(tx, node)
        .await?
        .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
    if push_node.owner_bare_jid != *owner_bare_jid {
        return Err(XmppError::forbidden(Some(
            "Push node belongs to another user".to_string(),
        )));
    }
    if push_node.status != PushNodeStatus::Active {
        return Err(XmppError::item_not_found(Some(
            "Push node not active".to_string(),
        )));
    }
    if active_devices_for_node_tx(tx, node).await?.is_empty() {
        return Err(XmppError::bad_request(Some(
            "Push node has no active registered devices".to_string(),
        )));
    }
    Ok(())
}

async fn get_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<Option<PushServiceNode>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
            FROM push_nodes
            WHERE node = ?
            "#,
            crate::db_params![node],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_node(&row)?))
}

async fn get_node_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
) -> Result<Option<PushServiceNode>, XmppError> {
    let Some(push_node) = get_node_tx(tx, node).await? else {
        return Ok(None);
    };
    if push_node.owner_bare_jid == *owner_bare_jid {
        Ok(Some(push_node))
    } else {
        Ok(None)
    }
}

async fn find_node_by_owner_app_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    app_id: &str,
) -> Result<Option<PushServiceNode>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
            FROM push_nodes
            WHERE owner_bare_jid = ? AND app_id = ?
            "#,
            crate::db_params![owner_bare_jid.to_string(), app_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_node(&row)?))
}

async fn count_active_nodes_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
) -> Result<i64, XmppError> {
    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM push_nodes WHERE owner_bare_jid = ? AND status = ?",
            crate::db_params![owner_bare_jid.to_string(), NODE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .ok_or_else(|| XmppError::internal("Push node count query returned no row"))?;
    row.get(0)
        .map_err(|error| XmppError::internal(error.to_string()))
}

async fn node_names_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
) -> Result<Vec<String>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node
            FROM push_nodes
            WHERE owner_bare_jid = ?
            ORDER BY node ASC
            "#,
            crate::db_params![owner_bare_jid.to_string()],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        nodes.push(
            row.get(0)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        );
    }
    Ok(nodes)
}

async fn prune_disabled_nodes_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    retain: i64,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node
            FROM push_nodes
            WHERE owner_bare_jid = ? AND status = ?
            ORDER BY updated_at_ms DESC, node DESC
            "#,
            crate::db_params![owner_bare_jid.to_string(), NODE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut seen = 0_i64;
    let mut stale_nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        seen += 1;
        if seen > retain {
            stale_nodes.push(
                row.get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
    }
    for node in stale_nodes {
        tx.execute(
            "DELETE FROM push_nodes WHERE owner_bare_jid = ? AND node = ? AND status = ?",
            crate::db_params![owner_bare_jid.to_string(), node, NODE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn active_device_exists_on_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    device_id: &str,
) -> Result<bool, XmppError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM push_devices WHERE node = ? AND device_id = ? AND status = ? LIMIT 1",
            crate::db_params![node, device_id, DEVICE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .is_some())
}

async fn count_active_devices_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<i64, XmppError> {
    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM push_devices WHERE node = ? AND status = ?",
            crate::db_params![node, DEVICE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .ok_or_else(|| XmppError::internal("Push device count query returned no row"))?;
    row.get(0)
        .map_err(|error| XmppError::internal(error.to_string()))
}

async fn prune_disabled_devices_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    retain: i64,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT device_id
            FROM push_devices
            WHERE node = ? AND status = ?
            ORDER BY updated_at_ms DESC, device_id DESC
            "#,
            crate::db_params![node, DEVICE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut seen = 0_i64;
    let mut stale_devices = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        seen += 1;
        if seen > retain {
            stale_devices.push(
                row.get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
    }
    for device_id in stale_devices {
        tx.execute(
            "DELETE FROM push_devices WHERE node = ? AND device_id = ? AND status = ?",
            crate::db_params![node, device_id, DEVICE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn active_devices_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<Vec<PushDeliveryTarget>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT device_id, platform
            FROM push_devices
            WHERE node = ? AND status = ?
            ORDER BY device_id ASC
            "#,
            crate::db_params![node, DEVICE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut devices = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        devices.push(decode_delivery_target(&row)?);
    }
    Ok(devices)
}

async fn wake_queued_publish_jobs_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        UPDATE push_publish_jobs
        SET next_retry_at_ms = NULL,
            updated_at_ms = ?
        WHERE node = ?
          AND status = ?
          AND last_error = ?
        "#,
        crate::db_params![
            now_ms,
            node,
            PUBLISH_JOB_STATUS_QUEUED,
            PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn claim_publish_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
    now_ms: i64,
) -> Result<Option<PushPublishJob>, XmppError> {
    let changed = tx
        .execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                claimed_at_ms = ?,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND (next_retry_at_ms IS NULL OR next_retry_at_ms <= ?)
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_IN_PROGRESS,
                now_ms,
                now_ms,
                job_id,
                PUBLISH_JOB_STATUS_QUEUED,
                now_ms,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    if changed == 0 {
        return Ok(None);
    }
    get_publish_job_tx(tx, job_id).await
}

async fn get_publish_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<Option<PushPublishJob>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status
            FROM push_publish_jobs
            WHERE job_id = ?
            "#,
            crate::db_params![job_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_publish_job(&row)?))
}

async fn mark_publish_job_failed_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
    error: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        UPDATE push_publish_jobs
        SET status = ?,
            attempt_count = attempt_count + 1,
            last_error = ?,
            next_retry_at_ms = NULL,
            claimed_at_ms = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
        "#,
        crate::db_params![PUBLISH_JOB_STATUS_FAILED, error, now_ms, job_id],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn get_device_for_owner_on_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
    device_id: &str,
    secrets: &PushSecretCipher,
) -> Result<Option<PushServiceDevice>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT d.device_id, d.node, d.platform, d.environment,
                   d.provider_endpoint, d.provider_token, d.provider_key_material
            FROM push_devices d
            JOIN push_nodes n ON n.node = d.node
            WHERE n.owner_bare_jid = ? AND d.node = ? AND d.device_id = ?
            "#,
            crate::db_params![owner_bare_jid.to_string(), node, device_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_device(&row, secrets)?))
}

async fn prune_delivery_attempts_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    limit: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        DELETE FROM push_delivery_attempts
        WHERE node = ?
          AND attempt_id NOT IN (
              SELECT attempt_id
              FROM push_delivery_attempts
              WHERE node = ?
              ORDER BY created_at_ms DESC, attempt_id DESC
              LIMIT ?
          )
        "#,
        crate::db_params![node, node, limit],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn prune_publish_jobs_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    limit: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        DELETE FROM push_publish_jobs
        WHERE node = ?
          AND status != ?
          AND job_id NOT IN (
              SELECT job_id
              FROM push_publish_jobs
              WHERE node = ?
              ORDER BY created_at_ms DESC, job_id DESC
              LIMIT ?
        )
        "#,
        crate::db_params![node, PUBLISH_JOB_STATUS_IN_PROGRESS, node, limit,],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn delete_retryable_publish_jobs_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        DELETE FROM push_publish_jobs
        WHERE owner_bare_jid = ?
          AND node = ?
          AND status IN (?, ?)
        "#,
        crate::db_params![
            owner_bare_jid.to_string(),
            node,
            PUBLISH_JOB_STATUS_QUEUED,
            PUBLISH_JOB_STATUS_IN_PROGRESS,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

fn retry_at_ms(now_ms: i64) -> i64 {
    now_ms.saturating_add(PUBLISH_JOB_RETRY_DELAY_MS)
}

fn decode_node(row: &crate::db::Row) -> Result<PushServiceNode, XmppError> {
    let owner_bare_jid: String = row
        .get(1)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(PushServiceNode {
        node: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        owner_bare_jid: owner_bare_jid.parse().map_err(|error| {
            XmppError::internal(format!("Invalid stored push owner JID: {error}"))
        })?,
        app_id: row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        status: PushNodeStatus::parse(
            &row.get::<String>(3)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
        created_at_ms: row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        updated_at_ms: row
            .get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    })
}

fn decode_publish_job(row: &crate::db::Row) -> Result<PushPublishJob, XmppError> {
    let owner_bare_jid: String = row
        .get(1)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(PushPublishJob {
        job_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        owner_bare_jid: owner_bare_jid.parse().map_err(|error| {
            XmppError::internal(format!(
                "Invalid stored push publish job owner JID: {error}"
            ))
        })?,
        node: row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        item_id: row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        push_service_jid: row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        status: row
            .get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    })
}

fn decode_device(
    row: &crate::db::Row,
    secrets: &PushSecretCipher,
) -> Result<PushServiceDevice, XmppError> {
    let provider_endpoint = secrets.open_optional(
        row.get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    )?;
    #[cfg(test)]
    let provider_token = secrets.open_optional(
        row.get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    )?;
    #[cfg(test)]
    let provider_key_material = secrets.open_optional(
        row.get(6)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    )?;
    Ok(PushServiceDevice {
        device_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        node: row
            .get(1)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        platform: PushDevicePlatform::parse(
            &row.get::<String>(2)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
        environment: row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        provider_endpoint,
        #[cfg(test)]
        provider_token,
        #[cfg(test)]
        provider_key_material,
    })
}

fn decode_attempt(row: &crate::db::Row) -> Result<PushDeliveryAttempt, XmppError> {
    Ok(PushDeliveryAttempt {
        attempt_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        node: row
            .get(1)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        device_id: row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        item_id: row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        status: row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    })
}

fn decode_delivery_target(row: &crate::db::Row) -> Result<PushDeliveryTarget, XmppError> {
    Ok(PushDeliveryTarget {
        device_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        platform: PushDevicePlatform::parse(
            &row.get::<String>(1)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use std::collections::HashMap;
    use tempfile::tempdir;
    use waddle_xmpp::pending_delivery::storage::PendingStorageError;
    use waddle_xmpp::pending_delivery::{PendingRow, PendingRowId, SmSessionId};
    use waddle_xmpp::push::{PushSubscription, PushSubscriptionStore};

    struct ReconciliationPendingStorage {
        failing_recipient: BareJid,
        rows: HashMap<BareJid, Vec<PendingRow>>,
        max_limit: usize,
    }

    #[async_trait::async_trait]
    impl PendingDeliveryStorage for ReconciliationPendingStorage {
        async fn insert(
            &self,
            _row: PendingRow,
        ) -> Result<waddle_xmpp::pending_delivery::InsertOutcome, PendingStorageError> {
            panic!("insert is not used by reconciliation tests")
        }

        async fn list(&self, _recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
            Err(PendingStorageError::Other(
                "unbounded list must not be used by reconciliation".to_string(),
            ))
        }

        async fn list_unclaimed_after(
            &self,
            recipient: &BareJid,
            after: Option<&PendingRowId>,
            limit: usize,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            assert!(
                limit <= self.max_limit,
                "reconciliation requested an unexpectedly large pending page: {limit}"
            );
            if *recipient == self.failing_recipient {
                return Err(PendingStorageError::Other(
                    "forced recipient read failure".to_string(),
                ));
            }
            let after = after.map(PendingRowId::as_str);
            Ok(self
                .rows
                .get(recipient)
                .into_iter()
                .flat_map(|rows| rows.iter())
                .filter(|row| row.flushed_in_session.is_none())
                .filter(|row| after.is_none_or(|after| row.id.as_str() > after))
                .take(limit)
                .cloned()
                .collect())
        }

        async fn claim_for_session(
            &self,
            _recipient: &BareJid,
            _session: &SmSessionId,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            panic!("claim_for_session is not used by reconciliation tests")
        }

        async fn delete_claimed(&self, _session: &SmSessionId) -> Result<u64, PendingStorageError> {
            panic!("delete_claimed is not used by reconciliation tests")
        }

        async fn delete_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
            panic!("delete_row is not used by reconciliation tests")
        }

        async fn release_claim(&self, _session: &SmSessionId) -> Result<u64, PendingStorageError> {
            panic!("release_claim is not used by reconciliation tests")
        }

        async fn release_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
            panic!("release_row is not used by reconciliation tests")
        }

        async fn record_pushed_at(
            &self,
            _id: &PendingRowId,
            _sequence: u32,
        ) -> Result<u64, PendingStorageError> {
            panic!("record_pushed_at is not used by reconciliation tests")
        }

        async fn delete_acked_through(
            &self,
            _session: &SmSessionId,
            _sequence_max: u32,
        ) -> Result<u64, PendingStorageError> {
            panic!("delete_acked_through is not used by reconciliation tests")
        }

        async fn list_orphaned_claims(
            &self,
            _live_sessions: &[SmSessionId],
        ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
            panic!("list_orphaned_claims is not used by reconciliation tests")
        }

        async fn count(&self, _recipient: &BareJid) -> Result<u32, PendingStorageError> {
            panic!("count is not used by reconciliation tests")
        }

        async fn delete_older_than(
            &self,
            _cutoff: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, PendingStorageError> {
            panic!("delete_older_than is not used by reconciliation tests")
        }
    }

    async fn store() -> DatabasePushServiceStore {
        DatabasePushServiceStore::new(
            Database::in_memory("push-service")
                .await
                .expect("push service db"),
        )
        .await
        .expect("push service store")
    }

    fn owner() -> BareJid {
        "alice@example.com".parse().expect("owner jid")
    }

    fn notification_item(item_id: &str) -> PubSubItem {
        PubSubItem::new(
            Some(item_id.to_string()),
            Some(Element::builder("notification", NS_PUSH).build()),
        )
    }

    fn publish_options_with_field(var: &str, value: &str) -> Element {
        Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
            .attr("type", "submit")
            .append(
                Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                    .attr("var", "FORM_TYPE")
                    .append(
                        Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                            .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                    .attr("var", var)
                    .append(
                        Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                            .append(value)
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn xep0357_pubsub_publish_iq(
        push_service_jid: &str,
        publisher: &BareJid,
        node: &str,
        item: &PubSubItem,
        publish_options: Option<&Element>,
    ) -> Iq {
        let publish = Element::builder("publish", waddle_xmpp::pubsub::NS_PUBSUB)
            .attr("node", node)
            .append(item.to_element(waddle_xmpp::pubsub::NS_PUBSUB))
            .build();
        let mut pubsub = Element::builder("pubsub", waddle_xmpp::pubsub::NS_PUBSUB).append(publish);
        if let Some(publish_options) = publish_options {
            pubsub = pubsub.append(
                Element::builder("publish-options", waddle_xmpp::pubsub::NS_PUBSUB)
                    .append(publish_options.clone())
                    .build(),
            );
        }
        Iq {
            from: Some(publisher.clone().into()),
            to: Some(push_service_jid.parse().expect("push service jid")),
            id: "push-publish-test".to_string(),
            payload: IqType::Set(pubsub.build()),
        }
    }

    async fn scalar_i64(
        store: &DatabasePushServiceStore,
        sql: &str,
        params: impl IntoParams,
    ) -> i64 {
        let mut rows = store.query(sql, params).await.expect("scalar query");
        let row = rows.next().await.expect("scalar row").expect("scalar row");
        row.get(0).expect("scalar value")
    }

    async fn scalar_optional_i64(
        store: &DatabasePushServiceStore,
        sql: &str,
        params: impl IntoParams,
    ) -> Option<i64> {
        let mut rows = store.query(sql, params).await.expect("scalar query");
        let row = rows.next().await.expect("scalar row").expect("scalar row");
        row.get(0).expect("scalar optional value")
    }

    async fn insert_pending_message(
        pending_storage: &crate::pending_delivery::DatabasePendingDeliveryStorage,
        recipient: &BareJid,
        row_id: &str,
    ) {
        assert!(matches!(
            pending_storage
                .insert(waddle_xmpp::pending_delivery::PendingRow {
                    id: waddle_xmpp::pending_delivery::PendingRowId::new(row_id),
                    recipient: recipient.clone(),
                    original_receipt_at: chrono::Utc::now(),
                    payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                        xmpp_parsers::message::Message::new(None::<jid::Jid>),
                    )),
                    flushed_in_session: None,
                    outbound_sequence: None,
                })
                .await
                .expect("pending insert"),
            waddle_xmpp::pending_delivery::InsertOutcome::Inserted
        ));
    }

    fn assert_bad_request(error: XmppError) {
        assert!(matches!(
            error,
            XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::BadRequest,
                ..
            }
        ));
    }

    fn assert_item_not_found(error: XmppError) {
        assert!(matches!(
            error,
            XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn ensure_node_reuses_one_node_per_user_app() {
        let store = store().await;
        let first = store
            .ensure_node(&owner(), "ios")
            .await
            .expect("first node");
        let second = store
            .ensure_node(&owner(), "ios")
            .await
            .expect("second node");

        assert_eq!(first.node(), second.node());
        assert_eq!(first.owner_bare_jid(), &owner());
        assert_eq!(first.app_id(), "ios");
        assert_eq!(
            store
                .list_node_names_for_owner(&owner())
                .await
                .expect("nodes")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn publish_notification_fans_out_to_active_devices_only() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/one".to_string())),
            )
            .await
            .expect("device one");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-2", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/two".to_string())),
            )
            .await
            .expect("device two");
        store
            .disable_device_for_owner(&owner, node.node(), "web-2", Some("expired"))
            .await
            .expect("disable device");

        let result = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("push-1"),
                &owner,
            )
            .await
            .expect("publish");

        assert_eq!(result.item_id(), "push-1");
        assert_eq!(result.attempted_devices(), 1);
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].device_id(), "web-1");
        assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FAKE_SENT);
    }

    #[tokio::test]
    async fn publish_notification_requires_xep0357_payload() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        let item = PubSubItem::new(
            Some("bad".to_string()),
            Some(Element::builder("x", "urn:waddle:test").build()),
        );

        let err = store
            .publish_notification_from_user_server(node.node(), &item, &owner)
            .await
            .expect_err("reject wrong payload");
        assert_bad_request(err);
    }

    #[tokio::test]
    async fn provider_credentials_live_in_push_service_not_xep0357_registration_store() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        let device = store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
            )
            .await
            .expect("device");

        assert_eq!(
            device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(device.provider_token(), Some("provider-secret"));
        assert_eq!(device.provider_key_material(), Some("provider-key"));

        let mut raw_rows = store
            .query(
                "SELECT provider_endpoint, provider_token, provider_key_material \
                 FROM push_devices WHERE node = ? AND device_id = ?",
                crate::db_params![node.node(), "web-1"],
            )
            .await
            .expect("raw provider secret query");
        let raw_row = raw_rows
            .next()
            .await
            .expect("raw provider secret row")
            .expect("raw provider secret row");
        for idx in 0..3 {
            let raw: String = raw_row.get(idx).expect("sealed provider secret column");
            assert!(
                raw.starts_with(SEALED_PROVIDER_VALUE_PREFIX),
                "provider secret should be sealed at rest: {raw}"
            );
            assert!(
                !raw.contains("push.example.com/endpoint")
                    && !raw.contains("provider-secret")
                    && !raw.contains("provider-key"),
                "provider secret should not be persisted in plaintext: {raw}"
            );
        }

        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: None,
                endpoint: Some("https://legacy.example.com/should-not-persist".to_string()),
                p256dh: Some("legacy-key".to_string()),
                auth_key: Some("legacy-auth".to_string()),
            })
            .await
            .expect("register");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");

        assert_eq!(registrations.len(), 1);
        assert!(registrations[0].endpoint.is_none());
        assert!(registrations[0].p256dh.is_none());
        assert!(registrations[0].auth_key.is_none());
    }

    #[tokio::test]
    async fn first_party_enable_rolls_back_when_registration_insert_fails() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_token(Some("provider-secret".to_string())),
            )
            .await
            .expect("device");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        let owner_lock_updated_before = scalar_i64(
            &store,
            "SELECT updated_at_ms FROM push_owner_locks WHERE owner_bare_jid = ?",
            crate::db_params![owner.to_string()],
        )
        .await;
        let node_lock_updated_before = scalar_i64(
            &store,
            "SELECT updated_at_ms FROM push_node_locks WHERE node = ?",
            crate::db_params![node.node()],
        )
        .await;
        store
            .execute(
                r#"
                CREATE TRIGGER fail_push_registration_insert
                BEFORE INSERT ON push_registrations
                BEGIN
                    SELECT RAISE(ABORT, 'forced push registration insert failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        store
            .register_first_party_node_for_owner(&owner, "push.example.com", node.node(), None)
            .await
            .expect_err("forced insert failure should abort first-party enable transaction");

        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");
        let owner_lock_updated_after = scalar_i64(
            &store,
            "SELECT updated_at_ms FROM push_owner_locks WHERE owner_bare_jid = ?",
            crate::db_params![owner.to_string()],
        )
        .await;
        let node_lock_updated_after = scalar_i64(
            &store,
            "SELECT updated_at_ms FROM push_node_locks WHERE node = ?",
            crate::db_params![node.node()],
        )
        .await;
        let publish = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("after-enable-rollback"),
                &owner,
            )
            .await
            .expect("push node remains usable after rollback");

        assert!(registrations.is_empty());
        assert_eq!(owner_lock_updated_after, owner_lock_updated_before);
        assert_eq!(node_lock_updated_after, node_lock_updated_before);
        assert_eq!(publish.attempted_devices(), 1);
    }

    #[tokio::test]
    async fn first_party_disable_rolls_back_when_registration_delete_fails() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
            )
            .await
            .expect("device");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: Some(publish_options_with_field("secret", "server-secret")),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        store
            .execute(
                r#"
                CREATE TRIGGER fail_push_registration_delete
                BEFORE DELETE ON push_registrations
                BEGIN
                    SELECT RAISE(ABORT, 'forced push registration delete failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");

        store
            .remove_registered_nodes_for_owner(&owner, "push.example.com", Some(node.node()))
            .await
            .expect_err("forced delete failure should abort first-party disable transaction");

        let active_node = store
            .get_node_for_owner(&owner, node.node())
            .await
            .expect("active node lookup")
            .expect("node should remain active after rollback");
        let active_device = store
            .get_device_for_owner_on_node(&owner, node.node(), "web-1")
            .await
            .expect("device lookup")
            .expect("device should remain after rollback");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");
        let publish = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("after-disable-rollback"),
                &owner,
            )
            .await
            .expect("push node remains usable after rollback");

        assert_eq!(active_node.status, PushNodeStatus::Active);
        assert_eq!(
            active_device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(active_device.provider_token(), Some("provider-secret"));
        assert_eq!(active_device.provider_key_material(), Some("provider-key"));
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].node.as_deref(), Some(node.node()));
        assert_eq!(publish.attempted_devices(), 1);
    }

    #[tokio::test]
    async fn first_party_disable_preserves_device_state_and_retires_queued_jobs() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
            )
            .await
            .expect("device");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: Some(publish_options_with_field("secret", "server-secret")),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("stale-before-disable"),
                &owner,
            )
            .await
            .expect("enqueue stale job");

        let removed = store
            .remove_registered_nodes_for_owner(&owner, "push.example.com", Some(node.node()))
            .await
            .expect("remove first-party registration");
        let active_node = store
            .get_node_for_owner(&owner, node.node())
            .await
            .expect("node lookup")
            .expect("node remains active");
        let active_device = store
            .get_device_for_owner_on_node(&owner, node.node(), "web-1")
            .await
            .expect("device lookup")
            .expect("device remains provisioned");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");
        let reactivated = store.ensure_node(&owner, "web").await.expect("same node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device refresh");
        let drained = store
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("drain after disable");

        assert_eq!(removed, 1);
        assert_eq!(active_node.status, PushNodeStatus::Active);
        assert_eq!(reactivated.node(), node.node());
        assert_eq!(
            active_device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(active_device.provider_token(), Some("provider-secret"));
        assert_eq!(active_device.provider_key_material(), Some("provider-key"));
        assert!(registrations.is_empty());
        assert!(store
            .queued_publish_jobs()
            .await
            .expect("queued")
            .is_empty());
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn first_party_enable_preserves_xep0357_publish_options_in_registration_and_jobs() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        let publish_options = publish_options_with_field("secret", "server-secret");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        store
            .register_first_party_node_for_owner(
                &owner,
                "push.example.com",
                node.node(),
                Some(&publish_options),
            )
            .await
            .expect("first-party registration");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");

        store
            .publish_notification_from_user_server_with_publish_options(
                node.node(),
                &notification_item("publish-options-job"),
                &owner,
                registrations[0].publish_options.as_ref(),
            )
            .await
            .expect("publish with options");
        let mut rows = store
            .query(
                "SELECT publish_options_xml FROM push_publish_jobs WHERE node = ? AND item_id = ?",
                crate::db_params![node.node(), "publish-options-job"],
            )
            .await
            .expect("job options query");
        let row = rows
            .next()
            .await
            .expect("job options row")
            .expect("job options row");
        let job_options_xml: Option<String> = row.get(0).expect("job options xml");

        assert_eq!(registrations.len(), 1);
        assert!(registrations[0].publish_options.is_some());
        assert!(job_options_xml
            .expect("job publish options")
            .contains("server-secret"));
    }

    #[tokio::test]
    async fn xep0357_pubsub_iq_publish_requires_live_first_party_registration() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
            .await
            .expect("registration store");
        let publish_options = publish_options_with_field("secret", "server-secret");
        store
            .register_first_party_node_for_owner(
                &owner,
                "push.example.com",
                node.node(),
                Some(&publish_options),
            )
            .await
            .expect("first-party registration");
        let iq = xep0357_pubsub_publish_iq(
            "push.example.com",
            &owner,
            node.node(),
            &notification_item("xep-pubsub-iq"),
            Some(&publish_options),
        );

        let result = store
            .publish_xep0357_pubsub_iq_from_user_server("push.example.com", &iq, &owner)
            .await
            .expect("server-origin PubSub publish");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let mut rows = store
            .query(
                "SELECT push_service_jid, publish_options_xml FROM push_publish_jobs \
                 WHERE node = ? AND item_id = ?",
                crate::db_params![node.node(), "xep-pubsub-iq"],
            )
            .await
            .expect("job query");
        let row = rows.next().await.expect("job row").expect("job row");
        let stored_service: Option<String> = row.get(0).expect("service jid");
        let stored_options: Option<String> = row.get(1).expect("publish options");

        assert_eq!(result.attempted_devices(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(stored_service.as_deref(), Some("push.example.com"));
        assert!(stored_options
            .expect("stored publish options")
            .contains("server-secret"));

        store
            .remove_registered_nodes_for_owner(&owner, "push.example.com", Some(node.node()))
            .await
            .expect("disable registration");
        let stale_iq = xep0357_pubsub_publish_iq(
            "push.example.com",
            &owner,
            node.node(),
            &notification_item("after-registration-disable"),
            Some(&publish_options),
        );
        let error = store
            .publish_xep0357_pubsub_iq_from_user_server("push.example.com", &stale_iq, &owner)
            .await
            .expect_err("disabled registration rejects stale publish snapshot");

        assert_item_not_found(error);
        assert!(store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts after disable")
            .iter()
            .all(|attempt| attempt.item_id() != "after-registration-disable"));
    }

    #[tokio::test]
    async fn device_ids_are_scoped_to_push_node_not_global_user_boundary() {
        let store = store().await;
        let alice: BareJid = "alice@example.com".parse().expect("alice");
        let bob: BareJid = "bob@example.com".parse().expect("bob");
        let alice_node = store.ensure_node(&alice, "web").await.expect("alice node");
        let bob_node = store.ensure_node(&bob, "web").await.expect("bob node");

        store
            .upsert_device(
                &alice,
                PushDeviceRegistration::new(
                    "shared-device-id",
                    alice_node.node(),
                    PushDevicePlatform::Web,
                    "test",
                )
                .with_provider_token(Some("alice-secret".to_string())),
            )
            .await
            .expect("alice device");
        store
            .upsert_device(
                &bob,
                PushDeviceRegistration::new(
                    "shared-device-id",
                    bob_node.node(),
                    PushDevicePlatform::Web,
                    "test",
                )
                .with_provider_token(Some("bob-secret".to_string())),
            )
            .await
            .expect("bob device");

        let alice_device = store
            .get_device_for_owner_on_node(&alice, alice_node.node(), "shared-device-id")
            .await
            .expect("alice lookup")
            .expect("alice device");
        let bob_device = store
            .get_device_for_owner_on_node(&bob, bob_node.node(), "shared-device-id")
            .await
            .expect("bob lookup")
            .expect("bob device");

        assert_eq!(alice_device.provider_token(), Some("alice-secret"));
        assert_eq!(bob_device.provider_token(), Some("bob-secret"));
        assert_eq!(alice_device.node(), alice_node.node());
        assert_eq!(bob_device.node(), bob_node.node());
    }

    #[tokio::test]
    async fn disable_device_is_scoped_to_one_push_node() {
        let store = store().await;
        let owner = owner();
        let first_node = store.ensure_node(&owner, "web").await.expect("first node");
        let second_node = store
            .ensure_node(&owner, "mobile")
            .await
            .expect("second node");
        for node in [first_node.node(), second_node.node()] {
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        "shared-device-id",
                        node,
                        PushDevicePlatform::Web,
                        "test",
                    )
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
                )
                .await
                .expect("device");
        }

        assert!(store
            .disable_device_for_owner(&owner, first_node.node(), "shared-device-id", None)
            .await
            .expect("disable first node device"));

        let first_result = store
            .publish_notification_from_user_server(
                first_node.node(),
                &notification_item("push-first"),
                &owner,
            )
            .await
            .expect("first publish");
        let second_result = store
            .publish_notification_from_user_server(
                second_node.node(),
                &notification_item("push-second"),
                &owner,
            )
            .await
            .expect("second publish");

        assert_eq!(first_result.attempted_devices(), 0);
        assert_eq!(second_result.attempted_devices(), 1);
        let disabled_device = store
            .get_device_for_owner_on_node(&owner, first_node.node(), "shared-device-id")
            .await
            .expect("disabled device lookup")
            .expect("disabled device");
        let active_device = store
            .get_device_for_owner_on_node(&owner, second_node.node(), "shared-device-id")
            .await
            .expect("active device lookup")
            .expect("active device");
        assert_eq!(disabled_device.provider_endpoint(), None);
        assert_eq!(disabled_device.provider_token(), None);
        assert_eq!(disabled_device.provider_key_material(), None);
        assert_eq!(
            active_device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(active_device.provider_token(), Some("provider-secret"));
    }

    #[tokio::test]
    async fn disable_nodes_for_owner_disables_node_and_clears_provider_credentials() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
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

        let disabled_node = store
            .get_node(node.node())
            .await
            .expect("node lookup")
            .expect("node");
        let disabled_device = store
            .get_device_for_owner_on_node(&owner, node.node(), "web-1")
            .await
            .expect("device lookup")
            .expect("device");
        let publish_err = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("disabled-node"),
                &owner,
            )
            .await
            .expect_err("disabled node rejects publish");

        assert_eq!(disabled_node.status, PushNodeStatus::Disabled);
        assert_eq!(disabled_device.provider_endpoint(), None);
        assert_eq!(disabled_device.provider_token(), None);
        assert_eq!(disabled_device.provider_key_material(), None);
        assert!(matches!(
            publish_err,
            XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
                ..
            }
        ));
        assert_item_not_found(
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        "web-2",
                        node.node(),
                        PushDevicePlatform::Web,
                        "test",
                    )
                    .with_provider_token(Some("stale-secret".to_string())),
                )
                .await
                .expect_err("disabled node rejects stale device registration"),
        );

        let reenabled_node = store
            .ensure_node(&owner, "web")
            .await
            .expect("reenable node");
        let publish_result = store
            .publish_notification_from_user_server(
                reenabled_node.node(),
                &notification_item("reenabled-node"),
                &owner,
            )
            .await
            .expect("reenabled publish");

        assert_eq!(reenabled_node.node(), node.node());
        assert_eq!(publish_result.attempted_devices(), 0);
    }

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

    #[tokio::test]
    async fn push_delivery_attempts_survive_store_reopen() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("push-service-attempts.sqlite3");
        let owner = owner();
        let node_id;
        {
            let db = Database::open_local("push-service-attempts", &path)
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
                    ),
                )
                .await
                .expect("device");
            store
                .publish_notification_from_user_server(
                    node.node(),
                    &notification_item("durable-attempt"),
                    &owner,
                )
                .await
                .expect("publish");
        }

        let reopened_db = Database::open_local("push-service-attempts-reopen", &path)
            .await
            .expect("reopened database");
        let reopened = DatabasePushServiceStore::new(reopened_db)
            .await
            .expect("reopened store");
        let attempts = reopened
            .delivery_attempts_for_node(&node_id)
            .await
            .expect("attempts");

        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].device_id(), "web-1");
        assert_eq!(attempts[0].item_id(), "durable-attempt");
        assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FAKE_SENT);
    }

    #[tokio::test]
    async fn queued_publish_job_survives_reopen_and_retries_after_dispatch_failure() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("push-service-publish-jobs.sqlite3");
        let owner = owner();
        let node_id;
        {
            let db = Database::open_local("push-service-jobs", &path)
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
                    ),
                )
                .await
                .expect("device");
            store
                .execute(
                    r#"
                    CREATE TRIGGER fail_push_delivery_attempt_insert
                    BEFORE INSERT ON push_delivery_attempts
                    BEGIN
                        SELECT RAISE(ABORT, 'forced push delivery attempt failure');
                    END
                    "#,
                    (),
                )
                .await
                .expect("failure trigger");

            store
                .publish_notification_from_user_server(
                    node.node(),
                    &notification_item("retry-after-failure"),
                    &owner,
                )
                .await
                .expect_err("forced dispatch failure keeps job queued");
            store
                .execute("DROP TRIGGER fail_push_delivery_attempt_insert", ())
                .await
                .expect("drop failure trigger");
            let queued = store.queued_publish_jobs().await.expect("queued jobs");
            let attempts = store
                .delivery_attempts_for_node(node.node())
                .await
                .expect("attempts");
            assert_eq!(queued.len(), 1);
            assert_eq!(queued[0].item_id(), "retry-after-failure");
            assert!(attempts.is_empty());
        }

        let reopened_db = Database::open_local("push-service-jobs-reopen", &path)
            .await
            .expect("reopened database");
        let reopened = DatabasePushServiceStore::new(reopened_db)
            .await
            .expect("reopened store");
        reopened
            .execute(
                "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE item_id = ?",
                crate::db_params!["retry-after-failure"],
            )
            .await
            .expect("make queued job retryable");
        let results = reopened
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("drain queued publish job");
        let attempts = reopened
            .delivery_attempts_for_node(&node_id)
            .await
            .expect("attempts after retry");
        let queued = reopened.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item_id(), "retry-after-failure");
        assert_eq!(results[0].attempted_devices(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "retry-after-failure");
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn device_registration_wakes_only_no_device_retry_jobs() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        store
            .execute(
                r#"
                CREATE TRIGGER fail_push_delivery_attempt_insert
                BEFORE INSERT ON push_delivery_attempts
                BEGIN
                    SELECT RAISE(ABORT, 'forced push delivery attempt failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");
        store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("retry-after-transient-failure"),
                &owner,
            )
            .await
            .expect_err("forced dispatch failure keeps job queued");
        store
            .execute("DROP TRIGGER fail_push_delivery_attempt_insert", ())
            .await
            .expect("drop failure trigger");
        let retry_before = scalar_optional_i64(
            &store,
            "SELECT next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
            crate::db_params!["retry-after-transient-failure"],
        )
        .await
        .expect("transient retry deadline");

        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device refresh");
        let retry_after = scalar_optional_i64(
            &store,
            "SELECT next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
            crate::db_params!["retry-after-transient-failure"],
        )
        .await
        .expect("transient retry deadline after device refresh");

        assert_eq!(retry_after, retry_before);
    }

    #[tokio::test]
    async fn publish_job_claim_is_exclusive_after_first_claim_commits() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("exclusive-claim"),
                &owner,
            )
            .await
            .expect("enqueue");
        let job_id = store.queued_publish_jobs().await.expect("queued jobs")[0]
            .job_id()
            .to_string();

        let now_ms = crate::time::now_ms();
        let mut first_tx = store.db.begin().await.expect("first tx");
        assert!(claim_publish_job_tx(&mut first_tx, &job_id, now_ms)
            .await
            .expect("first claim")
            .is_some());
        first_tx.commit().await.expect("first commit");

        let mut second_tx = store.db.begin().await.expect("second tx");
        assert!(claim_publish_job_tx(&mut second_tx, &job_id, now_ms + 1)
            .await
            .expect("second claim")
            .is_none());
        second_tx.commit().await.expect("second commit");

        assert_eq!(
            scalar_i64(
                &store,
                "SELECT COUNT(*) FROM push_publish_jobs WHERE status = ?",
                crate::db_params![PUBLISH_JOB_STATUS_IN_PROGRESS],
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn concurrent_publish_job_drains_claim_each_job_once() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");

        let enqueue = store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("claim-once"),
                &owner,
            )
            .await
            .expect("enqueue");
        assert!(enqueue.queued);

        let left_store = store.clone();
        let right_store = store.clone();
        let (left, right) = tokio::join!(
            async move { left_store.drain_queued_notification_publish_jobs(16).await },
            async move { right_store.drain_queued_notification_publish_jobs(16).await },
        );
        let left = left.expect("left drain");
        let right = right.expect("right drain");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(left.len() + right.len(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "claim-once");
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn drain_continues_after_retryable_publish_job_failure() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        store
            .execute(
                r#"
                CREATE TRIGGER fail_poison_push_delivery_attempt
                BEFORE INSERT ON push_delivery_attempts
                WHEN NEW.item_id = 'poison'
                BEGIN
                    SELECT RAISE(ABORT, 'forced poison push delivery attempt failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");
        for item_id in ["poison", "deliver-after-poison"] {
            store
                .enqueue_notification_publish_job_from_user_server(
                    node.node(),
                    &notification_item(item_id),
                    &owner,
                )
                .await
                .expect("enqueue");
        }

        let results = store
            .drain_queued_notification_publish_jobs(2)
            .await
            .expect("drain batch");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item_id(), "deliver-after-poison");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "deliver-after-poison");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "poison");
    }

    #[tokio::test]
    async fn direct_publish_recovers_expired_claim_before_retry() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("recover-direct-claim"),
                &owner,
            )
            .await
            .expect("enqueue");
        store
            .execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    claimed_at_ms = ?,
                    updated_at_ms = ?
                WHERE node = ? AND item_id = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    crate::time::now_ms() - PUBLISH_JOB_CLAIM_TIMEOUT_MS - 1,
                    crate::time::now_ms() - PUBLISH_JOB_CLAIM_TIMEOUT_MS - 1,
                    node.node(),
                    "recover-direct-claim",
                ],
            )
            .await
            .expect("force stale claim");

        let result = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("recover-direct-claim"),
                &owner,
            )
            .await
            .expect("direct publish retry");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(result.item_id(), "recover-direct-claim");
        assert_eq!(result.attempted_devices(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "recover-direct-claim");
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn zero_device_retry_backoff_does_not_block_newer_jobs() {
        let store = store().await;
        let owner = owner();
        let zero_device_node = store.ensure_node(&owner, "web").await.expect("zero node");
        let live_node = store.ensure_node(&owner, "ios").await.expect("live node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    "ios-1",
                    live_node.node(),
                    PushDevicePlatform::Apns,
                    "test",
                ),
            )
            .await
            .expect("live device");
        store
            .enqueue_notification_publish_job_from_user_server(
                zero_device_node.node(),
                &notification_item("zero-device-oldest"),
                &owner,
            )
            .await
            .expect("enqueue zero-device");
        store
            .enqueue_notification_publish_job_from_user_server(
                live_node.node(),
                &notification_item("eligible-newer"),
                &owner,
            )
            .await
            .expect("enqueue eligible");

        let first = store
            .drain_queued_notification_publish_jobs(1)
            .await
            .expect("drain oldest");
        let second = store
            .drain_queued_notification_publish_jobs(1)
            .await
            .expect("drain next eligible");
        let live_attempts = store
            .delivery_attempts_for_node(live_node.node())
            .await
            .expect("live attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].item_id(), "zero-device-oldest");
        assert_eq!(first[0].attempted_devices(), 0);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].item_id(), "eligible-newer");
        assert_eq!(live_attempts.len(), 1);
        assert_eq!(live_attempts[0].item_id(), "eligible-newer");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "zero-device-oldest");
    }

    #[tokio::test]
    async fn publish_job_pruning_bounds_old_queued_jobs_per_node() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        for item_id in ["queued-1", "queued-2", "queued-3"] {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            store
                .enqueue_notification_publish_job_from_user_server(
                    node.node(),
                    &notification_item(item_id),
                    &owner,
                )
                .await
                .expect("enqueue");
        }

        let mut tx = store.db.begin().await.expect("tx");
        prune_publish_jobs_tx(&mut tx, node.node(), 2)
            .await
            .expect("prune jobs");
        tx.commit().await.expect("commit");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");
        let item_ids = queued
            .iter()
            .map(|job| job.item_id().to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            item_ids,
            vec!["queued-2".to_string(), "queued-3".to_string()]
        );
    }

    #[tokio::test]
    async fn reconciliation_derives_missing_publish_jobs_from_pending_delivery_rows() {
        let dir = tempdir().expect("tempdir");
        let push_path = dir.path().join("push-service-reconcile.sqlite3");
        let pending_path = dir.path().join("pending-delivery-reconcile.sqlite3");
        let pending_url = format!("sqlite://{}", pending_path.to_string_lossy());
        let db = Database::open_local("push-service-reconcile", &push_path)
            .await
            .expect("push database");
        let store = DatabasePushServiceStore::new(db).await.expect("store");
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        let pending_storage = crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&pending_url),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage");
        let pending_id = waddle_xmpp::pending_delivery::PendingRowId::new("pending-row-1");
        let pending_row = waddle_xmpp::pending_delivery::PendingRow {
            id: pending_id.clone(),
            recipient: owner.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                xmpp_parsers::message::Message::new(None::<jid::Jid>),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        assert!(matches!(
            pending_storage
                .insert(pending_row)
                .await
                .expect("pending insert"),
            waddle_xmpp::pending_delivery::InsertOutcome::Inserted
        ));

        let reconciled = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                1,
                16,
            )
            .await
            .expect("reconcile");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(reconciled.scanned_registrations(), 1);
        assert_eq!(reconciled.enqueued_jobs(), 1);
        assert!(reconciled.next_cursor().is_none());
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), pending_id.as_str());

        let results = store
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("drain queued");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item_id(), pending_id.as_str());
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), pending_id.as_str());
    }

    #[tokio::test]
    async fn reconciliation_scans_past_already_jobbed_pending_rows() {
        let dir = tempdir().expect("tempdir");
        let push_path = dir.path().join("push-service-reconcile-prefix.sqlite3");
        let pending_path = dir.path().join("pending-delivery-reconcile-prefix.sqlite3");
        let pending_url = format!("sqlite://{}", pending_path.to_string_lossy());
        let db = Database::open_local("push-service-reconcile-prefix", &push_path)
            .await
            .expect("push database");
        let store = DatabasePushServiceStore::new(db).await.expect("store");
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
            .await
            .expect("registration store")
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        let pending_storage = crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&pending_url),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage");
        let already_jobbed_prefix = PUSH_RECONCILIATION_SCAN_FACTOR + 1;
        for idx in 0..=already_jobbed_prefix {
            let id = format!("pending-row-{idx:03}");
            let pending_row = waddle_xmpp::pending_delivery::PendingRow {
                id: waddle_xmpp::pending_delivery::PendingRowId::new(id.clone()),
                recipient: owner.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                    xmpp_parsers::message::Message::new(None::<jid::Jid>),
                )),
                flushed_in_session: None,
                outbound_sequence: None,
            };
            assert!(matches!(
                pending_storage
                    .insert(pending_row)
                    .await
                    .expect("pending insert"),
                waddle_xmpp::pending_delivery::InsertOutcome::Inserted
            ));
        }
        for idx in 0..already_jobbed_prefix {
            let id = format!("pending-row-{idx:03}");
            store
                .publish_notification_from_user_server(node.node(), &notification_item(&id), &owner)
                .await
                .expect("publish already-jobbed pending row");
        }
        let expected_missing_id = format!("pending-row-{already_jobbed_prefix:03}");

        let reconciled = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                16,
                1,
            )
            .await
            .expect("reconcile");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(reconciled.enqueued_jobs(), 1);
        assert!(reconciled.next_cursor().is_none());
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), expected_missing_id);
    }

    #[tokio::test]
    async fn reconciliation_preserves_retry_backoff_and_scans_past_existing_jobs() {
        let dir = tempdir().expect("tempdir");
        let push_path = dir
            .path()
            .join("push-service-reconcile-retry-backoff.sqlite3");
        let pending_path = dir
            .path()
            .join("pending-delivery-reconcile-retry-backoff.sqlite3");
        let pending_url = format!("sqlite://{}", pending_path.to_string_lossy());
        let db = Database::open_local("push-service-reconcile-retry-backoff", &push_path)
            .await
            .expect("push database");
        let store = DatabasePushServiceStore::new(db).await.expect("store");
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
            .await
            .expect("registration store")
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        let pending_storage = crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&pending_url),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage");
        insert_pending_message(&pending_storage, &owner, "poison-row").await;

        let first_reconcile = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                16,
                1,
            )
            .await
            .expect("first reconcile");
        let first_queued = store.queued_publish_jobs().await.expect("queued jobs");
        assert_eq!(first_reconcile.enqueued_jobs(), 1);
        assert_eq!(first_queued.len(), 1);
        assert_eq!(first_queued[0].item_id(), "poison-row");

        store
            .execute(
                r#"
                CREATE TRIGGER fail_poison_push_delivery_attempt
                BEFORE INSERT ON push_delivery_attempts
                WHEN NEW.item_id = 'poison-row'
                BEGIN
                    SELECT RAISE(ABORT, 'forced poison push delivery attempt failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");
        let results = store
            .drain_queued_notification_publish_jobs(1)
            .await
            .expect("drain poison");
        assert!(results.is_empty());

        let retry_before = scalar_optional_i64(
            &store,
            "SELECT next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
            crate::db_params!["poison-row"],
        )
        .await
        .expect("poison retry deadline");
        insert_pending_message(&pending_storage, &owner, "later-row").await;

        let second_reconcile = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                16,
                1,
            )
            .await
            .expect("second reconcile");
        let retry_after = scalar_optional_i64(
            &store,
            "SELECT next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
            crate::db_params!["poison-row"],
        )
        .await
        .expect("poison retry deadline after reconcile");
        let item_ids = store
            .queued_publish_jobs()
            .await
            .expect("queued jobs")
            .iter()
            .map(|job| job.item_id().to_string())
            .collect::<Vec<_>>();

        assert_eq!(second_reconcile.enqueued_jobs(), 1);
        assert_eq!(retry_after, retry_before);
        assert_eq!(
            item_ids,
            vec!["poison-row".to_string(), "later-row".to_string()]
        );
    }

    #[tokio::test]
    async fn reconciliation_skips_stale_registration_and_continues() {
        let dir = tempdir().expect("tempdir");
        let push_path = dir
            .path()
            .join("push-service-reconcile-stale-registration.sqlite3");
        let pending_path = dir
            .path()
            .join("pending-delivery-reconcile-stale-registration.sqlite3");
        let pending_url = format!("sqlite://{}", pending_path.to_string_lossy());
        let db = Database::open_local("push-service-reconcile-stale-registration", &push_path)
            .await
            .expect("push database");
        let store = DatabasePushServiceStore::new(db).await.expect("store");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        let stale_owner = "alice@example.com".parse::<BareJid>().expect("alice jid");
        let live_owner = "bob@example.com".parse::<BareJid>().expect("bob jid");
        let live_node = store.ensure_node(&live_owner, "web").await.expect("node");
        registration_store
            .register(PushSubscription {
                user_jid: stale_owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some("missing-node".to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("stale registration");
        registration_store
            .register(PushSubscription {
                user_jid: live_owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(live_node.node().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("live registration");
        let pending_storage = crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&pending_url),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage");
        insert_pending_message(&pending_storage, &stale_owner, "stale-row").await;
        insert_pending_message(&pending_storage, &live_owner, "live-row").await;

        let reconciled = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                16,
                1,
            )
            .await
            .expect("reconcile");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(reconciled.scanned_registrations(), 2);
        assert_eq!(reconciled.enqueued_jobs(), 1);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "live-row");
    }

    #[tokio::test]
    async fn reconciliation_skips_pending_read_failure_and_uses_bounded_pages() {
        let store = store().await;
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        let failing_owner = "alice@example.com".parse::<BareJid>().expect("alice jid");
        let live_owner = "bob@example.com".parse::<BareJid>().expect("bob jid");
        let failing_node = store
            .ensure_node(&failing_owner, "web")
            .await
            .expect("node");
        let live_node = store.ensure_node(&live_owner, "web").await.expect("node");
        for (owner, node) in [
            (&failing_owner, failing_node.node()),
            (&live_owner, live_node.node()),
        ] {
            registration_store
                .register(PushSubscription {
                    user_jid: owner.to_string(),
                    service_jid: "push.example.com".to_string(),
                    node: Some(node.to_string()),
                    publish_options: None,
                    endpoint: None,
                    p256dh: None,
                    auth_key: None,
                })
                .await
                .expect("registration");
        }
        let live_row = PendingRow {
            id: PendingRowId::new("live-row"),
            recipient: live_owner.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                xmpp_parsers::message::Message::new(None::<jid::Jid>),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        let pending_storage = ReconciliationPendingStorage {
            failing_recipient: failing_owner,
            rows: HashMap::from([(live_owner, vec![live_row])]),
            max_limit: PUSH_RECONCILIATION_SCAN_FACTOR,
        };

        let reconciled = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                16,
                1,
            )
            .await
            .expect("reconcile");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(reconciled.scanned_registrations(), 2);
        assert_eq!(reconciled.enqueued_jobs(), 1);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "live-row");
    }

    #[tokio::test]
    async fn reconciliation_cursor_resumes_next_registration_without_repeat() {
        let dir = tempdir().expect("tempdir");
        let push_path = dir.path().join("push-service-reconcile-cursor.sqlite3");
        let pending_path = dir.path().join("pending-delivery-reconcile-cursor.sqlite3");
        let pending_url = format!("sqlite://{}", pending_path.to_string_lossy());
        let db = Database::open_local("push-service-reconcile-cursor", &push_path)
            .await
            .expect("push database");
        let store = DatabasePushServiceStore::new(db).await.expect("store");
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        let pending_storage = crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&pending_url),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage");

        let owners = [
            "alice@example.com".parse::<BareJid>().expect("alice jid"),
            "bob@example.com".parse::<BareJid>().expect("bob jid"),
            "carol@example.com".parse::<BareJid>().expect("carol jid"),
        ];
        for (index, owner) in owners.iter().enumerate() {
            let node = store
                .ensure_node(owner, &format!("web-{index}"))
                .await
                .expect("node");
            registration_store
                .register(PushSubscription {
                    user_jid: owner.to_string(),
                    service_jid: "push.example.com".to_string(),
                    node: Some(node.node().to_string()),
                    publish_options: None,
                    endpoint: None,
                    p256dh: None,
                    auth_key: None,
                })
                .await
                .expect("registration");
            let pending_row_id =
                waddle_xmpp::pending_delivery::PendingRowId::new(format!("cursor-row-{index}"));
            assert!(matches!(
                pending_storage
                    .insert(waddle_xmpp::pending_delivery::PendingRow {
                        id: pending_row_id,
                        recipient: owner.clone(),
                        original_receipt_at: chrono::Utc::now(),
                        payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(
                            Box::new(xmpp_parsers::message::Message::new(None::<jid::Jid>))
                        ),
                        flushed_in_session: None,
                        outbound_sequence: None,
                    })
                    .await
                    .expect("pending insert"),
                waddle_xmpp::pending_delivery::InsertOutcome::Inserted
            ));
        }

        let first_page = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                None,
                1,
                16,
            )
            .await
            .expect("first page");
        let first_cursor = first_page.next_cursor().expect("first cursor").clone();
        let second_page = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                Some(&first_cursor),
                1,
                16,
            )
            .await
            .expect("second page");
        let second_cursor = second_page.next_cursor().expect("second cursor").clone();
        let third_page = store
            .reconcile_pending_delivery_notification_jobs(
                &pending_storage,
                "push.example.com",
                Some(&second_cursor),
                1,
                16,
            )
            .await
            .expect("third page");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");
        let item_ids = queued
            .iter()
            .map(|job| job.item_id().to_string())
            .collect::<Vec<_>>();

        assert_eq!(first_page.enqueued_jobs(), 1);
        assert_eq!(second_page.enqueued_jobs(), 1);
        assert_eq!(third_page.enqueued_jobs(), 1);
        assert!(third_page.next_cursor().is_none());
        assert_eq!(
            item_ids,
            vec![
                "cursor-row-0".to_string(),
                "cursor-row-1".to_string(),
                "cursor-row-2".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn zero_device_publish_job_remains_retryable_until_device_returns() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        store
            .disable_device_for_owner(&owner, node.node(), "web-1", None)
            .await
            .expect("disable device");

        let result = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("retry-when-device-returns"),
                &owner,
            )
            .await
            .expect("publish with no active devices stays queued");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(result.attempted_devices(), 0);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "retry-when-device-returns");

        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("reenable device");
        let retried = store
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("drain queued");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued_after = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].attempted_devices(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "retry-when-device-returns");
        assert!(queued_after.is_empty());
    }

    #[tokio::test]
    async fn ensure_node_rejects_oversized_app_id() {
        let store = store().await;
        let err = store
            .ensure_node(&owner(), &"x".repeat(MAX_APP_ID_LEN + 1))
            .await
            .expect_err("oversized app-id rejected");

        assert_bad_request(err);
    }

    #[tokio::test]
    async fn node_quota_limits_new_nodes_per_owner() {
        let store = store().await;
        let owner = owner();
        for idx in 0..MAX_PUSH_NODES_PER_OWNER {
            store
                .ensure_node(&owner, &format!("app-{idx}"))
                .await
                .expect("node within quota");
        }

        let err = store
            .ensure_node(&owner, "app-over-quota")
            .await
            .expect_err("node over quota rejected");

        assert_bad_request(err);
    }

    #[tokio::test]
    async fn node_quota_counts_active_nodes_not_retired_nodes() {
        let store = store().await;
        let owner = owner();
        for idx in 0..MAX_PUSH_NODES_PER_OWNER {
            let node = store
                .ensure_node(&owner, &format!("app-{idx}"))
                .await
                .expect("node within quota");
            store
                .disable_nodes_for_owner(&owner, Some(node.node()))
                .await
                .expect("disable retained node");
        }

        let fresh = store
            .ensure_node(&owner, "app-after-retired-quota")
            .await
            .expect("retired disabled nodes must not permanently exhaust active quota");

        assert_eq!(fresh.status, PushNodeStatus::Active);
    }

    #[tokio::test]
    async fn upsert_device_rejects_oversized_provider_token() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        let err = store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_token(Some("x".repeat(MAX_PROVIDER_TOKEN_LEN + 1))),
            )
            .await
            .expect_err("oversized token rejected");

        assert_bad_request(err);
    }

    #[tokio::test]
    async fn upsert_device_rejects_oversized_device_registration_fields() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        let cases = [
            PushDeviceRegistration::new(
                "x".repeat(MAX_DEVICE_ID_LEN + 1),
                node.node(),
                PushDevicePlatform::Web,
                "test",
            ),
            PushDeviceRegistration::new(
                "web-1",
                "x".repeat(MAX_NODE_ID_LEN + 1),
                PushDevicePlatform::Web,
                "test",
            ),
            PushDeviceRegistration::new(
                "web-1",
                node.node(),
                PushDevicePlatform::Web,
                "x".repeat(MAX_ENVIRONMENT_LEN + 1),
            ),
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_endpoint(Some("x".repeat(MAX_PROVIDER_ENDPOINT_LEN + 1))),
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_key_material(Some("x".repeat(MAX_PROVIDER_KEY_MATERIAL_LEN + 1))),
        ];

        for registration in cases {
            let err = store
                .upsert_device(&owner, registration)
                .await
                .expect_err("oversized registration field rejected");
            assert_bad_request(err);
        }
    }

    #[tokio::test]
    async fn publish_notification_rejects_oversized_pubsub_item_id() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        let err = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item(&"x".repeat(MAX_PUBSUB_ITEM_ID_LEN + 1)),
                &owner,
            )
            .await
            .expect_err("oversized item id rejected");

        assert_bad_request(err);
    }

    #[tokio::test]
    async fn device_quota_limits_new_devices_per_node() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        for idx in 0..MAX_PUSH_DEVICES_PER_NODE {
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        format!("web-{idx}"),
                        node.node(),
                        PushDevicePlatform::Web,
                        "test",
                    ),
                )
                .await
                .expect("device within quota");
        }

        let err = store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    "web-over-quota",
                    node.node(),
                    PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect_err("device over quota rejected");

        assert_bad_request(err);
    }

    #[tokio::test]
    async fn device_quota_counts_active_devices_not_retired_devices() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        for idx in 0..MAX_PUSH_DEVICES_PER_NODE {
            let device_id = format!("web-{idx}");
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        device_id.as_str(),
                        node.node(),
                        PushDevicePlatform::Web,
                        "test",
                    )
                    .with_provider_token(Some(format!("provider-secret-{idx}"))),
                )
                .await
                .expect("device within quota");
            store
                .disable_device_for_owner(&owner, node.node(), &device_id, None)
                .await
                .expect("disable retained device");
        }

        let fresh = store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    "web-after-retired-quota",
                    node.node(),
                    PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("retired disabled devices must not permanently exhaust active quota");

        assert_eq!(fresh.device_id(), "web-after-retired-quota");
    }

    #[tokio::test]
    async fn publish_notification_prunes_attempts_on_publish_path() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");

        let retention_limit = 3;
        store
            .publish_notification_from_user_server_with_retention_limit(
                node.node(),
                &notification_item("item-0"),
                &owner,
                None,
                None,
                retention_limit,
            )
            .await
            .expect("first publish");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        for idx in 1..=retention_limit {
            store
                .publish_notification_from_user_server_with_retention_limit(
                    node.node(),
                    &notification_item(&format!("item-{idx}")),
                    &owner,
                    None,
                    None,
                    retention_limit,
                )
                .await
                .expect("publish over retention limit");
        }

        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");

        assert_eq!(attempts.len(), retention_limit as usize);
        assert!(
            attempts.iter().all(|attempt| attempt.item_id() != "item-0"),
            "oldest publish-path attempt should be pruned"
        );
    }

    #[tokio::test]
    async fn delivery_attempt_pruning_keeps_newest_attempts_per_node() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");
        for idx in 0..5 {
            store
                .execute(
                    r#"
                    INSERT INTO push_delivery_attempts (
                        attempt_id,
                        node,
                        device_id,
                        platform,
                        item_id,
                        status,
                        last_error,
                        created_at_ms
                    ) VALUES (?, ?, ?, ?, ?, ?, NULL, ?)
                    "#,
                    crate::db_params![
                        format!("attempt-{idx}"),
                        node.node(),
                        "web-1",
                        PushDevicePlatform::Web.to_string(),
                        format!("item-{idx}"),
                        ATTEMPT_STATUS_FAKE_SENT,
                        idx as i64,
                    ],
                )
                .await
                .expect("attempt row");
        }

        let db = store.database();
        let mut tx = db.begin().await.expect("transaction");
        prune_delivery_attempts_tx(&mut tx, node.node(), 3)
            .await
            .expect("prune attempts");
        tx.commit().await.expect("commit prune");

        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let item_ids = attempts
            .iter()
            .map(|attempt| attempt.item_id())
            .collect::<Vec<_>>();

        assert_eq!(item_ids, vec!["item-2", "item-3", "item-4"]);
    }
}
