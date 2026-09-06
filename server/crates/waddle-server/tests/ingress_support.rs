use jid::{BareJid, Jid};
use uuid::Uuid;
use waddle_server::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseConfig, DatabaseDriver, IntoParams, MigrationRunner},
    ingress::{IngressStreamIdentity, IngressSubmission},
    ingress_uow::IngressUnitOfWork,
};
use waddle_xmpp::{
    auth::{AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch},
    ingress::{ConnectionGeneration, DigestContext, DigestInput, NormalizedTarget},
};
use xmpp_parsers::message::{Lang, Message, MessageType};

pub struct IngressFixture {
    pub db: Database,
    pub uow: IngressUnitOfWork,
    pub principal: AuthenticatedPrincipalRef,
    #[cfg(feature = "clustering")]
    lineage: LineageConfig,
    postgres: Option<(sqlx::PgPool, String)>,
    sqlite_directory: Option<tempfile::TempDir>,
}

impl IngressFixture {
    pub async fn sqlite() -> Self {
        let directory = tempfile::tempdir().expect("SQLite fixture directory");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("ingress.db").display()
        );
        waddle_xmpp::mam::SqlxMamStorage::open(&database_url)
            .await
            .expect("SQLite MAM schema");
        let config = DatabaseConfig::new(DatabaseDriver::Sqlite, database_url);
        let db = Database::from_config("ingress-test", &config)
            .await
            .expect("SQLite fixture database");
        let mut fixture = Self::initialize(db, None).await;
        fixture.sqlite_directory = Some(directory);
        fixture
    }

    pub async fn postgres(test_name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping {test_name}: WADDLE_TEST_POSTGRES_URL not set");
            return None;
        };
        let schema = format!(
            "waddle_test_ingress_{test_name}_{}",
            Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("Postgres admin");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("isolated schema");
        let mut url = url::Url::parse(&database_url).expect("Postgres URL");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained)
            .append_pair("options", &format!("-c search_path={schema}"));
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
        let db = Database::from_config("ingress-test", &config)
            .await
            .expect("Postgres database");
        waddle_xmpp::mam::SqlxMamStorage::open(url.as_str())
            .await
            .expect("Postgres MAM schema");
        Some(Self::initialize(db, Some((admin, schema))).await)
    }

    async fn initialize(db: Database, postgres: Option<(sqlx::PgPool, String)>) -> Self {
        waddle_server::inbox::DatabaseInboxStorage::open(Some(db.database_url()))
            .await
            .expect("inbox schema");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("ingress migrations");
        let lineage = LineageConfig {
            deployment_uuid: Some(lineage::DeploymentUuid(Uuid::new_v4())),
            action: None,
        };
        lineage::enroll(&db, &lineage)
            .await
            .expect("enroll lineage");
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
            #[cfg(feature = "clustering")]
            lineage,
        };
        fixture.execute("INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)", waddle_server::db_params![fixture.principal.bare_jid().to_string(), "romeo".to_string(), "romeo".to_string(), chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
        fixture.execute("INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)", waddle_server::db_params!["ingress-session".to_string(), fixture.principal.bare_jid().to_string(), "ingress-token".to_string(), fixture.principal.auth_context_id().as_uuid().to_string(), 3_i64, 5_i64, chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
        fixture
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
            principal: self.principal.clone(),
            target,
            plan: IngressPlan {
                plan: Vec::new(),
                intents: Vec::new(),
                sanitized_message: message,
                error_reply: None,
                rejection: None,
                room_execution: RoomExecutionPath::None,
            },
            digest_input,
            connection_generation: ConnectionGeneration::INITIAL,
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

    pub async fn close(self) {
        drop(self.uow);
        drop(self.db);
        drop(self.sqlite_directory);
        if let Some((admin, schema)) = self.postgres {
            sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop fixture schema");
        }
    }
}
