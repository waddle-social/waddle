#![cfg(feature = "clustering")]

use std::time::{Duration, Instant};

use jid::{BareJid, Jid};
use uuid::Uuid;
use waddle_server::{
    clustering::claims::PostgresClaimStore,
    config::{IngressShadowConfig, LineageConfig},
    db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner},
    ingress_shadow::{
        IngressEffectCaptureSnapshot, IngressShadowDisposition, IngressShadowHandle,
        IngressShadowSubmission,
    },
    sm_persistence::DatabaseSmPersistence,
};
use waddle_xmpp::{
    auth::{AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch},
    ingress::{
        ConnectionGeneration, IngressEffectIntent, IngressOrdinal, MessageKey, NormalizedTarget,
    },
    ownership::{ClaimEpoch, ClaimStore, NodeIdentity, SharedNodeIdentity},
    pending_delivery::SmSessionId,
};
use xmpp_parsers::message::{Message, MessageType};

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub struct ShadowFixture {
    pub db: Database,
    pub handle: IngressShadowHandle,
    pub stream_id: SmSessionId,
    pub principal: AuthenticatedPrincipalRef,
    owner: NodeIdentity,
    claim_epoch: ClaimEpoch,
    target: BareJid,
    admin: sqlx::PgPool,
    schema: String,
}

impl ShadowFixture {
    pub async fn open(test_name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress shadow support)");
            return None;
        };
        let schema = format!(
            "waddle_test_ingress_shadow_{test_name}_{}",
            Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect PostgreSQL admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated PostgreSQL schema");
        let schema_url = postgres_url_with_search_path(&database_url, &schema);
        let mut database_config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
        database_config.pool_size = 8;
        let db = Database::from_config("ingress-shadow-test", &database_config)
            .await
            .expect("open isolated PostgreSQL database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply PostgreSQL migrations");
        PostgresClaimStore::new(db.clone())
            .ensure_schema()
            .await
            .expect("initialize claims schema");
        DatabaseSmPersistence::open(Some(&schema_url))
            .await
            .expect("initialize SM persistence schema");

        let lineage = LineageConfig {
            deployment_uuid: Some(
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a9f003"
                    .parse()
                    .expect("valid fixture lineage UUID"),
            ),
            action: None,
        };
        lineage::enroll(&db, &lineage)
            .await
            .expect("enroll fixture lineage");

        let owner = NodeIdentity::new("shadow-node", "shadow-incarnation");
        let stream_id = SmSessionId::new(format!("shadow-stream-{}", Uuid::new_v4().simple()));
        let principal = AuthenticatedPrincipalRef::new(
            "romeo@example.com".parse().expect("fixture bare JID"),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(3),
            PrincipalAuthEpoch::new(5),
        );
        let target: BareJid = "juliet@example.com"
            .parse()
            .expect("fixture target bare JID");
        let claim_epoch = ClaimEpoch(17);
        let handle = IngressShadowHandle::new(
            IngressShadowConfig {
                enabled: true,
                queue_capacity: 16,
                pool_size: 4,
                retry_attempts: 5,
            },
            db.clone(),
            lineage,
            Some(SharedNodeIdentity::new(owner.clone())),
        )
        .await;
        assert!(
            handle.is_enabled(),
            "clustering shadow worker must be enabled"
        );

        let fixture = Self {
            db,
            handle,
            stream_id,
            principal,
            owner,
            claim_epoch,
            target,
            admin,
            schema,
        };
        fixture.seed_principal_and_claim().await;
        assert_eq!(
            fixture.handle.try_enroll_stream(fixture.stream_id.clone()),
            IngressShadowDisposition::Enqueued,
            "fresh SM stream enrollment must enter the shadow worker"
        );
        fixture.wait_for_enrollment().await;
        Some(fixture)
    }

    pub fn submission_with_intents(
        &self,
        ordinal: u64,
        origin: Option<&str>,
        body: &str,
        intents: Vec<IngressEffectIntent>,
    ) -> IngressShadowSubmission {
        let mut message = Message::new(Some(Jid::from(self.target.clone())));
        message.type_ = MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
        if let Some(origin) = origin {
            waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
        }
        IngressShadowSubmission {
            stream_id: self.stream_id.clone(),
            owner: self.owner.clone(),
            claim_epoch: self.claim_epoch,
            handled_ordinal: IngressOrdinal::from_storage(ordinal).expect("valid ingress ordinal"),
            principal: self.principal.clone(),
            target: NormalizedTarget::Bare(self.target.clone()),
            message,
            capture: IngressEffectCaptureSnapshot {
                stanza_lang: None,
                sanitized_message: None,
                room_fence: None,
                intents,
                markers: Vec::new(),
            },
            connection_generation: Some(ConnectionGeneration::INITIAL),
        }
    }

    pub async fn enqueue(&self, submission: IngressShadowSubmission) {
        assert_eq!(
            self.handle.try_submit(submission),
            IngressShadowDisposition::Enqueued,
            "shadow submission must enqueue"
        );
    }

    pub async fn wait_for_frontier(&self, expected: u64) {
        self.wait_until(|| async { self.frontier().await == Some(expected) })
            .await;
    }

    pub async fn frontier(&self) -> Option<u64> {
        let conn = self.db.guard().await.expect("database connection");
        let mut rows = conn
            .query(
                "SELECT handled_ordinal::text FROM ingress_sm_streams WHERE stream_id = ?",
                waddle_server::db_params![self.stream_id.as_str().to_string()],
            )
            .await
            .expect("read shadow frontier");
        rows.next().await.expect("read frontier row").map(|row| {
            row.get::<String>(0)
                .expect("decode frontier")
                .parse()
                .expect("frontier is u64")
        })
    }

    pub async fn message_key_for_ordinal(&self, ordinal: u64) -> Option<MessageKey> {
        let conn = self.db.guard().await.expect("database connection");
        let mut rows = conn
            .query(
                "SELECT message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = (SELECT sm_ingress_id FROM ingress_sm_streams WHERE stream_id = ?) AND ingress_ordinal = ?::numeric",
                waddle_server::db_params![
                    self.stream_id.as_str().to_string(),
                    ordinal.to_string(),
                ],
            )
            .await
            .expect("read ordinal message key");
        rows.next().await.expect("read ordinal row").map(|row| {
            MessageKey::from_storage(
                row.get::<String>(0)
                    .expect("decode message key")
                    .parse()
                    .expect("message key UUID"),
            )
        })
    }

    pub async fn close(self) {
        let Self {
            db,
            handle,
            admin,
            schema,
            ..
        } = self;
        drop(handle);
        drop(db);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated PostgreSQL schema");
    }

    async fn seed_principal_and_claim(&self) {
        let suffix = Uuid::new_v4().simple().to_string();
        self.execute(
            "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, now(), now())",
            waddle_server::db_params![
                self.principal.bare_jid().to_string(),
                format!("shadow-{suffix}"),
                format!("shadow-{suffix}"),
            ],
        )
        .await
        .expect("seed fixture user");
        self.execute(
            "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, now(), now())",
            waddle_server::db_params![
                format!("shadow-session-{suffix}"),
                self.principal.bare_jid().to_string(),
                format!("shadow-token-{suffix}"),
                self.principal.auth_context_id().as_uuid().to_string(),
                i64::try_from(self.principal.auth_context_version().get()).expect("version fits"),
                i64::try_from(self.principal.auth_epoch().get()).expect("epoch fits"),
            ],
        )
        .await
        .expect("seed fixture authenticated session");
        self.execute(
            "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
            waddle_server::db_params![
                format!("sm_session:{}", self.stream_id.as_str()),
                "sm_session".to_string(),
                self.owner.node_id.clone(),
                self.owner.node_epoch.clone(),
                self.claim_epoch.0,
            ],
        )
        .await
        .expect("seed exact SM claim");
    }

    async fn wait_for_enrollment(&self) {
        self.wait_until(|| async { self.frontier().await == Some(0) })
            .await;
    }

    async fn wait_until<F, Fut>(&self, mut condition: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            if condition().await {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for ingress shadow worker progress"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl waddle_server::db::IntoParams,
    ) -> Result<u64, waddle_server::db::DatabaseError> {
        let conn = self.db.guard().await?;
        conn.execute(sql, params).await
    }
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}
