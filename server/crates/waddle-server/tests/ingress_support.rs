use jid::{BareJid, Jid};
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::{
    lineage, Database, DatabaseConfig, DatabaseDriver, IntoParams, MigrationRunner,
};
use waddle_server::ingress::{IngressStreamIdentity, IngressSubmission};
use waddle_server::ingress_uow::IngressUnitOfWork;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::{ConnectionGeneration, DigestContext, DigestInput, NormalizedTarget};
use xmpp_parsers::message::{Lang, Message, MessageType};

/// A PostgreSQL database that one test creates and drops.
///
/// Migrations and store bootstrap take database-scoped advisory locks (the
/// catalog mutation fence and the per-store schema gates) with bounded
/// try-locks. Tests sharing one database therefore contend with each other
/// even when their tables live in separate schemas; a private database gives
/// each test its own advisory-lock namespace as well as its own catalog.
pub struct IsolatedPostgresDatabase {
    cluster: sqlx::PgPool,
    cluster_url: String,
    name: String,
    url: String,
    /// Cleared once `drop_database` has removed the database.
    owns_database: bool,
}

impl IsolatedPostgresDatabase {
    /// Returns `None` when no test database is configured and the lane does
    /// not require one. `label` only makes the database name recognisable.
    pub async fn create(label: &str) -> Option<Self> {
        let Some(cluster_url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping {label}: WADDLE_TEST_POSTGRES_URL not set");
            return None;
        };
        // PostgreSQL identifiers hold 63 bytes: a 4-byte prefix, at most 26
        // bytes of the ASCII label, a separator and the 32-byte UUID. The
        // server folds the unquoted name to lower case, and the connection
        // URL must name the database exactly as stored.
        let name = format!(
            "wti_{:.26}_{}",
            label.to_ascii_lowercase(),
            Uuid::new_v4().simple()
        );
        let cluster = sqlx::PgPool::connect(&cluster_url)
            .await
            .expect("Postgres cluster");
        // template0 accepts no connections, so a session attached to the
        // default template cannot make concurrent creations fail.
        sqlx::query(&format!("CREATE DATABASE {name} TEMPLATE template0"))
            .execute(&cluster)
            .await
            .expect("isolated database");
        let mut url = url::Url::parse(&cluster_url).expect("Postgres URL");
        url.set_path(&format!("/{name}"));
        Some(Self {
            cluster,
            cluster_url,
            name,
            url: url.to_string(),
            owns_database: true,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Dropped pools close their connections asynchronously, so sessions of
    /// the finished test can still be attached; `FORCE` ends them.
    pub async fn drop_database(mut self) {
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.name))
            .execute(&self.cluster)
            .await
            .expect("drop isolated database");
        self.owns_database = false;
        self.cluster.close().await;
    }
}

/// A test that panics, or returns without `drop_database`, still removes its
/// database. The test's own runtime may be shutting down, so the bounded,
/// best-effort drop runs on a separate thread and runtime.
impl Drop for IsolatedPostgresDatabase {
    fn drop(&mut self) {
        if !self.owns_database {
            return;
        }
        let cluster_url = self.cluster_url.clone();
        let name = self.name.clone();
        let cleanup = std::thread::Builder::new()
            .name("isolated-database-cleanup".to_owned())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    eprintln!("could not create the cleanup runtime for database {name}");
                    return;
                };
                runtime.block_on(async move {
                    let cleanup = async {
                        let pool = sqlx::postgres::PgPoolOptions::new()
                            .max_connections(1)
                            .acquire_timeout(std::time::Duration::from_secs(3))
                            .connect(&cluster_url)
                            .await?;
                        sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                            .execute(&pool)
                            .await?;
                        pool.close().await;
                        Ok::<(), sqlx::Error>(())
                    };
                    match tokio::time::timeout(std::time::Duration::from_secs(15), cleanup).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => eprintln!("could not drop database {name}: {error}"),
                        Err(_) => eprintln!("dropping database {name} timed out"),
                    }
                });
            });
        match cleanup {
            Ok(thread) => {
                if thread.join().is_err() {
                    eprintln!("isolated database cleanup thread panicked");
                }
            }
            Err(error) => eprintln!("could not start isolated database cleanup: {error}"),
        }
    }
}

pub struct IngressFixture {
    pub db: Database,
    pub uow: IngressUnitOfWork,
    pub principal: AuthenticatedPrincipalRef,
    lineage: LineageConfig,
    postgres: Option<IsolatedPostgresDatabase>,
    sqlite_directory: Option<tempfile::TempDir>,
}

impl IngressFixture {
    /// Attach ingress repositories to an already migrated, enrolled database,
    /// retaining any fixture-owned PostgreSQL database for cleanup.
    pub async fn existing(
        db: Database,
        lineage: LineageConfig,
        postgres: Option<IsolatedPostgresDatabase>,
    ) -> Self {
        let uow =
            IngressUnitOfWork::open(db.clone(), lineage.clone()).expect("ingress unit of work");
        let principal = AuthenticatedPrincipalRef::new(
            "romeo@example.com".parse().expect("sender"),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(3),
            PrincipalAuthEpoch::new(5),
        );
        let fixture = Self {
            db,
            uow,
            principal,
            postgres,
            sqlite_directory: None,
            lineage,
        };
        fixture.execute("INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)", waddle_server::db_params![fixture.principal.bare_jid().to_string(), "romeo".to_string(), "romeo".to_string(), "romeo".to_string(), chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
        fixture.execute("INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)", waddle_server::db_params!["ingress-session".to_string(), fixture.principal.bare_jid().to_string(), "ingress-token".to_string(), fixture.principal.auth_context_id().as_uuid().to_string(), 3_i64, 5_i64, chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
        fixture
    }

    pub async fn sqlite() -> Self {
        let directory = tempfile::tempdir().expect("SQLite fixture directory");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("ingress.db").display()
        );
        let config = DatabaseConfig::new(DatabaseDriver::Sqlite, database_url);
        let db = Database::from_config("ingress-test", &config)
            .await
            .expect("SQLite fixture database");
        let mut fixture = Self::initialize(db, None).await;
        fixture.sqlite_directory = Some(directory);
        fixture
    }

    pub async fn postgres(test_name: &str) -> Option<Self> {
        Self::postgres_with_pool(test_name, 10).await
    }

    pub async fn postgres_with_pool(test_name: &str, pool_size: u32) -> Option<Self> {
        let database = IsolatedPostgresDatabase::create(test_name).await?;
        let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, database.url().to_owned());
        config.pool_size = pool_size;
        let db = Database::from_config("ingress-test", &config)
            .await
            .expect("Postgres database");
        Some(Self::initialize(db, Some(database)).await)
    }

    /// Build the schema from the baseline, then open the stores whose
    /// constructors check their own tables. The baseline expects an empty
    /// schema, so it runs first.
    async fn initialize(db: Database, postgres: Option<IsolatedPostgresDatabase>) -> Self {
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("ingress migrations");
        waddle_xmpp::mam::SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM schema");
        waddle_server::inbox::DatabaseInboxStorage::open(Some(db.database_url()))
            .await
            .expect("inbox schema");
        waddle_server::notification_activity::NotificationActivityStore::try_new(db.clone())
            .await
            .expect("activity schema");
        let lineage = LineageConfig {
            deployment_uuid: Some(lineage::DeploymentUuid(Uuid::new_v4())),
            action: None,
        };
        lineage::enroll(&db, &lineage)
            .await
            .expect("enroll lineage");
        Self::existing(db, lineage, postgres).await
    }

    pub fn submission(&self, origin: Option<&str>, body: &str) -> IngressSubmission {
        use waddle_server::ingress::{IngressPlan, RoomExecutionPath};
        let target: BareJid = "juliet@example.com".parse().expect("recipient");
        let mut message = Message::new(Some(Jid::from(target.clone())));
        message.from = Some("romeo@example.com/phone".parse().expect("sender resource"));
        message.type_ = MessageType::Chat;
        message.bodies.insert(Lang::new(), body.to_owned());
        if let Some(origin) = origin {
            waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
        }
        let target = NormalizedTarget::Bare(target);
        let digest_input = DigestInput::from_parsed(
            &message,
            &DigestContext {
                target: target.clone(),
                server_authorities: vec![self.principal.bare_jid().clone()],
                stanza_lang: None,
            },
        )
        .expect("digest");
        IngressSubmission {
            sender: "romeo@example.com/phone".parse().expect("sender"),
            identity: IngressStreamIdentity::Ephemeral {
                principal: self.principal.clone(),
            },
            principal: waddle_server::ingress::IngressPrincipal::Authenticated(
                self.principal.clone(),
            ),
            target,
            plan: IngressPlan {
                failure: None,
                plan: Vec::new(),
                intents: Vec::new(),
                room_canonical_message: None,
                sanitized_message: message,
                error_reply: None,
                rejection: None,
                room_execution: RoomExecutionPath::None,
            },
            digest_input,
            connection_generation: waddle_xmpp::ingress::TransportGeneration::Connection(
                ConnectionGeneration::INITIAL,
            ),
        }
    }

    pub async fn execute(&self, sql: &str, params: impl IntoParams) {
        self.db
            .guard()
            .await
            .expect("database guard")
            .execute(sql, params)
            .await
            .expect("fixture SQL");
    }

    pub async fn count(&self, table: &str) -> i64 {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .expect("row count");
        rows.next()
            .await
            .expect("count row")
            .expect("count exists")
            .get(0)
            .expect("count integer")
    }

    /// Read one nullable text column from the first row of `sql`.
    pub async fn optional_text(&self, sql: &str) -> Option<String> {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn.query(sql, ()).await.expect("text query");
        rows.next()
            .await
            .expect("text row")
            .expect("row exists")
            .get::<Option<String>>(0)
            .expect("text column")
    }

    #[cfg(feature = "clustering")]
    pub async fn room_fence(&mut self, room: &BareJid) -> waddle_xmpp::muc::RoomClaimFenceContext {
        use waddle_xmpp::ownership::{
            ClaimEpoch, ClaimStore, Entity, NodeIdentity, SharedNodeIdentity,
        };
        let owner = NodeIdentity::new("ingress-owner", "ingress-incarnation");
        let epoch = ClaimEpoch(17);
        waddle_server::clustering::claims::PostgresClaimStore::new(self.db.clone())
            .ensure_schema()
            .await
            .expect("claim schema");
        self.execute("INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)", waddle_server::db_params![format!("room_actor:{room}"), "room_actor".to_string(), owner.node_id.clone(), owner.node_epoch.clone(), epoch.0]).await;
        self.uow = IngressUnitOfWork::open_with_node_identity(
            self.db.clone(),
            self.lineage.clone(),
            SharedNodeIdentity::new(owner.clone()),
        )
        .expect("clustered room UoW");
        waddle_xmpp::muc::RoomClaimFenceContext::new(
            Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            ),
            owner,
            epoch,
        )
    }

    pub async fn authority(&self) -> waddle_server::ingress::IngressAuthority {
        waddle_server::ingress::IngressAuthority::try_new(
            Default::default(),
            self.db.clone(),
            self.lineage.clone(),
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .expect("restarted authority")
    }

    pub async fn close(self) {
        drop(self.uow);
        drop(self.db);
        drop(self.sqlite_directory);
        if let Some(database) = self.postgres {
            database.drop_database().await;
        }
    }
}
