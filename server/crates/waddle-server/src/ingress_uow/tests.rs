use sqlx::Connection;
use uuid::Uuid;
use waddle_xmpp::{
    auth::{AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch},
    ingress::{
        AliasOutcome, AliasResolution, DeliveryKey, IngressOrdinal, MessageKey, NormalizedTarget,
        ProtocolEpoch, SemanticDigest, SmIngressId,
    },
    ownership::{ClaimEpoch, ClaimStore, EntityType, NodeIdentity},
    pending_delivery::SmSessionId,
};
use waddle_xmpp_core::xep0359::OriginId;

use super::{
    CanonicalMessageRepository, DeliveryEffectRepository, IngressUowError,
    PostgresIngressUnitOfWork, SmIngressRepository,
};
#[cfg(feature = "clustering")]
use super::{
    ClaimRepository, HandledFrontierOutcome, HandledFrontierRepository, IngressUowTransaction,
};
use crate::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseConfig, DatabaseDriver, IntoParams, MigrationRunner},
};

#[tokio::test]
async fn open_rejects_sqlite() {
    let db = Database::in_memory("ingress_uow_sqlite")
        .await
        .expect("open sqlite database");
    assert!(matches!(
        PostgresIngressUnitOfWork::open(db, LineageConfig::default()),
        Err(IngressUowError::PostgresRequired)
    ));
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn spanning_proof_commits_exact_cross_store_values() {
    let Some(fixture) = Fixture::open("spanning_proof").await else {
        return;
    };
    let values = FixtureValues::new("spanning-proof");
    fixture.seed_claim_and_session(&values, 0).await;

    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("exact claim mints fence");
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 1)
            .await
            .expect("advance handled frontier"),
        HandledFrontierOutcome::Advanced
    );
    write_spanning_rows(&mut transaction, &values).await;
    transaction.commit().await.expect("commit spanning proof");

    fixture.assert_spanning_rows(&values, 1).await;
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn dropping_uow_rolls_back_spanning_writes() {
    let Some(fixture) = Fixture::open("atomicity").await else {
        return;
    };
    let values = FixtureValues::new("atomicity");
    fixture.seed_claim_and_session(&values, 0).await;

    {
        let mut transaction = fixture.begin().await;
        let fence = ClaimRepository::assert_sm_claim(
            &mut transaction,
            &values.stream_id,
            &values.owner,
            values.claim_epoch,
        )
        .await
        .expect("exact claim mints fence");
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 1)
            .await
            .expect("advance handled frontier");
        write_spanning_rows(&mut transaction, &values).await;
    }

    fixture.assert_spanning_rows(&values, 0).await;
    fixture.assert_frontier(&values.stream_id, 0).await;
    fixture.close().await;
}

#[tokio::test]
async fn epoch_one_uow_write_succeeds_and_raw_write_is_rejected() {
    let Some(fixture) = Fixture::open("guard_interaction").await else {
        return;
    };
    fixture.advance_epoch_to_one().await;
    let uow = fixture.uow();
    let mut transaction = uow.begin().await.expect("begin epoch-one uow");
    CanonicalMessageRepository::record(&mut transaction, MessageKey::new(), &digest(1))
        .await
        .expect("UoW carries epoch proof");
    transaction.commit().await.expect("commit UoW write");

    let raw = fixture
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, ?, ?)",
            crate::db_params![MessageKey::new().to_storage().to_string(), 1_i64, vec![2_u8; 32]],
        )
        .await;
    assert!(raw.is_err(), "the V1009 trigger rejects unproven writes");
    fixture.close().await;
}

#[tokio::test]
async fn lineage_mismatch_and_missing_row_fail_closed() {
    let Some(fixture) = Fixture::open("lineage_negatives").await else {
        return;
    };
    let mismatch = LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90002"
                .parse()
                .expect("valid mismatch deployment UUID"),
        ),
        action: None,
    };
    let mismatch_uow =
        PostgresIngressUnitOfWork::open(fixture.db.clone(), mismatch).expect("open mismatch UoW");
    assert!(matches!(
        mismatch_uow.begin().await,
        Err(IngressUowError::Lineage(crate::db::DatabaseError::Lineage(
            lineage::LineageError::DeploymentUuidMismatch { .. }
        )))
    ));

    fixture
        .execute("DELETE FROM _lineage", ())
        .await
        .expect("delete lineage row");
    let uow = fixture.uow();
    assert!(matches!(
        uow.begin().await,
        Err(IngressUowError::Lineage(crate::db::DatabaseError::Lineage(
            lineage::LineageError::MissingRow
        )))
    ));
    fixture.close().await;
}

#[tokio::test]
async fn epoch_fence_accepts_live_zero_and_one_and_rejects_future_epoch() {
    let Some(fixture) = Fixture::open("epoch_fence").await else {
        return;
    };
    let transaction = fixture.begin().await;
    assert_eq!(transaction.protocol_epoch(), ProtocolEpoch::ZERO);
    transaction.commit().await.expect("commit epoch-zero proof");

    fixture.advance_epoch_to_one().await;
    let transaction = fixture.begin().await;
    assert_eq!(transaction.protocol_epoch(), ProtocolEpoch::from_storage(1));
    transaction.commit().await.expect("commit epoch-one proof");

    fixture.set_epoch(2).await;
    let uow = fixture.uow();
    assert!(matches!(
        uow.begin().await,
        Err(IngressUowError::EpochUnsupported {
            live,
            supported: _
        }) if live == ProtocolEpoch::from_storage(2)
    ));
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn claim_fence_requires_the_exact_owner_incarnation_and_epoch() {
    let Some(fixture) = Fixture::open("claim_fence_negatives").await else {
        return;
    };
    let values = FixtureValues::new("claim-fence");
    fixture.seed_claim_and_session(&values, 0).await;
    let absent = SmSessionId::new("missing-stream");
    let wrong_owner = NodeIdentity::new("other-node", values.owner.node_epoch.clone());
    let wrong_incarnation = NodeIdentity::new(values.owner.node_id.clone(), "other-epoch");

    for (stream_id, owner, claim_epoch) in [
        (&absent, &values.owner, values.claim_epoch),
        (&values.stream_id, &wrong_owner, values.claim_epoch),
        (&values.stream_id, &wrong_incarnation, values.claim_epoch),
        (
            &values.stream_id,
            &values.owner,
            ClaimEpoch(values.claim_epoch.0 + 1),
        ),
    ] {
        let mut transaction = fixture.begin().await;
        assert!(matches!(
            ClaimRepository::assert_sm_claim(&mut transaction, stream_id, owner, claim_epoch).await,
            Err(IngressUowError::ClaimFenceMissing)
        ));
    }
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn claim_fence_blocks_a_concurrent_claim_update_until_commit() {
    let Some(fixture) = Fixture::open("claim_fence_concurrency").await else {
        return;
    };
    let values = FixtureValues::new("claim-fence-concurrency");
    fixture.seed_claim_and_session(&values, 0).await;
    let mut transaction = fixture.begin().await;
    let _fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("hold exact claim fence");

    let schema_url = fixture.schema_url.clone();
    let entity = sm_claim_entity(&values.stream_id);
    let update = tokio::spawn(async move {
        let mut connection = sqlx::PgConnection::connect(&schema_url)
            .await
            .expect("open competing claim connection");
        sqlx::query("UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = $1")
            .bind(entity)
            .execute(&mut connection)
            .await
            .expect("claim update completes after fence release");
    });
    wait_for_lock_waiter(&fixture.admin, "UPDATE clustering_claims SET claim_epoch").await;
    assert!(!update.is_finished(), "claim update must still be waiting");
    transaction.commit().await.expect("release claim fence");
    update.await.expect("join competing claim update");
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn handled_frontier_uses_wrapping_single_step_cas() {
    let Some(fixture) = Fixture::open("frontier_cas").await else {
        return;
    };
    let values = FixtureValues::new("frontier-cas");
    fixture.seed_claim_and_session(&values, u32::MAX).await;
    let mut transaction = fixture.begin().await;
    let fence = ClaimRepository::assert_sm_claim(
        &mut transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint fence");
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, u32::MAX)
            .await
            .expect("equal frontier is idempotent"),
        HandledFrontierOutcome::Idempotent
    );
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 0)
            .await
            .expect("wrapping frontier advances"),
        HandledFrontierOutcome::Advanced
    );
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 1)
            .await
            .expect("next frontier advances"),
        HandledFrontierOutcome::Advanced
    );
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &values.stream_id, 3).await,
        Err(IngressUowError::FrontierStale {
            stored: 1,
            offered: 3
        })
    ));
    let missing = SmSessionId::new("missing-frontier-stream");
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &missing, 1).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    transaction
        .commit()
        .await
        .expect("commit frontier outcomes");

    let mut missing_transaction = fixture.begin().await;
    let missing_fence = ClaimRepository::assert_sm_claim(
        &mut missing_transaction,
        &values.stream_id,
        &values.owner,
        values.claim_epoch,
    )
    .await
    .expect("mint fence for missing stream proof");
    fixture.delete_session(&values.stream_id).await;
    assert!(matches!(
        HandledFrontierRepository::advance(
            &mut missing_transaction,
            &missing_fence,
            &values.stream_id,
            1,
        )
        .await,
        Err(IngressUowError::StreamMissing)
    ));
    drop(missing_transaction);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
async fn write_spanning_rows(transaction: &mut IngressUowTransaction<'_>, values: &FixtureValues) {
    insert_mam_identity(transaction, values).await;
    insert_inbox_entry(transaction, values).await;
    // The alias miss path mints the canonical message row itself; recording
    // `values.message_key` separately first would collide on the primary key.
    assert!(matches!(
        CanonicalMessageRepository::resolve_and_record_alias(
            transaction,
            &values.sender,
            &values.target,
            &values.origin_id,
            &values.digest,
            || values.message_key,
        )
        .await
        .expect("record origin alias"),
        AliasResolution::Aliased(AliasOutcome::Inserted(key)) if key == values.message_key
    ));
    SmIngressRepository::insert(
        transaction,
        values.sm_ingress_id,
        values.ordinal,
        values.message_key,
    )
    .await
    .expect("record SM ingress reference");
    DeliveryEffectRepository::record(transaction, values.delivery_key, values.message_key)
        .await
        .expect("record delivery identity");
}

#[cfg(feature = "clustering")]
async fn insert_mam_identity(transaction: &mut IngressUowTransaction<'_>, values: &FixtureValues) {
    transaction
        .transaction_mut()
        .execute(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, message_type) VALUES (?, ?, now(), ?, ?, ?, ?)",
            crate::db_params![
                values.mam_id.clone(),
                "room@example.com".to_string(),
                values.sender.to_string(),
                "juliet@example.com".to_string(),
                "proof message".to_string(),
                "chat".to_string(),
            ],
        )
        .await
        .expect("seed MAM identity in UoW");
}

#[cfg(feature = "clustering")]
async fn insert_inbox_entry(transaction: &mut IngressUowTransaction<'_>, values: &FixtureValues) {
    transaction
        .transaction_mut()
        .execute(
            "INSERT INTO inbox_entries (user_jid, partner_jid, thread_id, kind, last_stanza_id, last_updated) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![
                values.principal.bare_jid().to_string(),
                "juliet@example.com".to_string(),
                "proof-thread".to_string(),
                "direct".to_string(),
                values.message_key.to_storage().to_string(),
                1_i64,
            ],
        )
        .await
        .expect("seed inbox entry in UoW");
}

#[cfg(feature = "clustering")]
struct FixtureValues {
    stream_id: SmSessionId,
    owner: NodeIdentity,
    claim_epoch: ClaimEpoch,
    principal: AuthenticatedPrincipalRef,
    sender: jid::BareJid,
    target: NormalizedTarget,
    origin_id: OriginId,
    digest: SemanticDigest,
    message_key: MessageKey,
    sm_ingress_id: SmIngressId,
    ordinal: IngressOrdinal,
    delivery_key: DeliveryKey,
    mam_id: String,
}

#[cfg(feature = "clustering")]
impl FixtureValues {
    fn new(name: &str) -> Self {
        let bare_jid: jid::BareJid = "romeo@example.com".parse().expect("valid fixture bare JID");
        Self {
            stream_id: SmSessionId::new(format!("stream-{name}")),
            owner: NodeIdentity::new("node-a", "node-a-epoch"),
            claim_epoch: ClaimEpoch(11),
            principal: AuthenticatedPrincipalRef::new(
                bare_jid.clone(),
                AuthContextId::new(
                    Uuid::parse_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90011")
                        .expect("valid auth context UUID"),
                ),
                AuthContextVersion::new(7),
                PrincipalAuthEpoch::new(9),
            ),
            sender: bare_jid,
            target: NormalizedTarget::Bare(
                "juliet@example.com"
                    .parse()
                    .expect("valid fixture target JID"),
            ),
            origin_id: OriginId::new(format!("origin-{name}")),
            digest: digest(42),
            message_key: MessageKey::new(),
            sm_ingress_id: SmIngressId::new(),
            ordinal: IngressOrdinal::FIRST,
            delivery_key: DeliveryKey::new(),
            mam_id: format!("mam-{name}"),
        }
    }
}

struct Fixture {
    db: Database,
    uow: PostgresIngressUnitOfWork,
    admin: sqlx::PgPool,
    schema: String,
    schema_url: String,
}

impl Fixture {
    async fn open(test_name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress uow)");
            return None;
        };
        let schema = format!(
            "waddle_test_ingress_uow_{test_name}_{}",
            Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");
        let schema_url = postgres_url_with_search_path(&database_url, &schema);
        let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
        config.pool_size = 10;
        let db = Database::from_config("ingress-uow-test", &config)
            .await
            .expect("open isolated postgres database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply migrations to isolated schema");
        initialize_existing_store_schemas(&db, &schema_url).await;
        let lineage = fixture_lineage_config();
        lineage::enroll(&db, &lineage)
            .await
            .expect("enroll fixture lineage before UoW use");
        let uow =
            PostgresIngressUnitOfWork::open(db.clone(), lineage.clone()).expect("open fixture UoW");
        Some(Self {
            db,
            uow,
            admin,
            schema,
            schema_url,
        })
    }

    fn uow(&self) -> PostgresIngressUnitOfWork {
        self.uow.clone()
    }

    async fn begin(&self) -> super::IngressUowTransaction<'_> {
        self.uow.begin().await.expect("begin fixture UoW")
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, crate::db::DatabaseError> {
        let conn = self.db.guard().await?;
        conn.execute(sql, params).await
    }

    #[cfg(feature = "clustering")]
    async fn seed_claim_and_session(&self, values: &FixtureValues, inbound_count: u32) {
        self.execute(
                "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    sm_claim_entity(&values.stream_id),
                    EntityType::SmSession.as_db_str().to_string(),
                    values.owner.node_id.clone(),
                    values.owner.node_epoch.clone(),
                    values.claim_epoch.0,
                ],
            )
            .await
            .expect("seed exact SM claim");
        self.execute(
            r#"INSERT INTO sm_sessions (
                    stream_id, user_id, full_jid, inbound_count, outbound_count, last_acked,
                    detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested,
                    blocklist_interested, presence_available, presence_priority, bare_jid,
                    auth_context_id, auth_context_version, principal_auth_epoch
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            crate::db_params![
                values.stream_id.as_str().to_string(),
                "romeo".to_string(),
                "romeo@example.com/phone".to_string(),
                i64::from(inbound_count),
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                values.principal.bare_jid().to_string(),
                values.principal.auth_context_id().as_uuid().to_string(),
                i64::try_from(values.principal.auth_context_version().get())
                    .expect("version fits i64"),
                i64::try_from(values.principal.auth_epoch().get()).expect("epoch fits i64"),
            ],
        )
        .await
        .expect("seed SM session with typed principal identity");
    }

    async fn set_epoch(&self, epoch: u32) {
        self.execute(
                "UPDATE ingress_protocol_epoch SET epoch = ?, activated_at = now(), lineage_uuid = ?::uuid WHERE id = 1",
                crate::db_params![
                    i64::from(epoch),
                    "8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92".to_string(),
                ],
            )
            .await
            .expect("set live protocol epoch");
    }

    async fn advance_epoch_to_one(&self) {
        self.set_epoch(1).await;
    }

    #[cfg(feature = "clustering")]
    async fn assert_spanning_rows(&self, values: &FixtureValues, expected_count: i64) {
        for table in [
            "ingress_messages",
            "ingress_origin_aliases",
            "ingress_sm_refs",
            "ingress_deliveries",
            "mam_messages",
            "inbox_entries",
        ] {
            assert_eq!(
                self.count(table).await,
                expected_count,
                "{table} visibility"
            );
        }
        if expected_count == 1 {
            let conn = self.db.guard().await.expect("fresh database connection");
            let mut rows = conn
                .query(
                    "SELECT inbound_count, bare_jid, auth_context_id, auth_context_version, principal_auth_epoch FROM sm_sessions WHERE stream_id = ?",
                    crate::db_params![values.stream_id.as_str().to_string()],
                )
                .await
                .expect("read committed SM principal row");
            let row = rows
                .next()
                .await
                .expect("read SM principal row")
                .expect("SM row exists");
            assert_eq!(row.get::<i64>(0).expect("decode frontier"), 1);
            assert_eq!(
                row.get::<String>(1).expect("decode bare JID"),
                values.principal.bare_jid().to_string()
            );
            assert_eq!(
                row.get::<String>(2).expect("decode context ID"),
                values.principal.auth_context_id().as_uuid().to_string()
            );
            assert_eq!(row.get::<i64>(3).expect("decode context version"), 7);
            assert_eq!(row.get::<i64>(4).expect("decode auth epoch"), 9);
            assert!(
                self.row_exists(
                    "SELECT 1 FROM ingress_origin_aliases WHERE message_key = ?::uuid",
                    crate::db_params![values.message_key.to_storage().to_string()],
                )
                .await,
                "origin alias retains the canonical message key"
            );
            assert!(
                self.row_exists(
                    "SELECT 1 FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric AND message_key = ?::uuid",
                    crate::db_params![
                        values.sm_ingress_id.to_storage().to_string(),
                        values.ordinal.to_storage().to_string(),
                        values.message_key.to_storage().to_string(),
                    ],
                )
                .await,
                "SM ingress reference retains its exact typed identity"
            );
            assert!(
                self.row_exists(
                    "SELECT 1 FROM ingress_deliveries WHERE delivery_key = ?::uuid AND message_key = ?::uuid",
                    crate::db_params![
                        values.delivery_key.to_storage().to_string(),
                        values.message_key.to_storage().to_string(),
                    ],
                )
                .await,
                "delivery identity retains the canonical message key"
            );
            assert!(
                self.row_exists(
                    "SELECT 1 FROM mam_messages WHERE id = ?",
                    crate::db_params![values.mam_id.clone()],
                )
                .await,
                "MAM identity is committed"
            );
        }
    }

    #[cfg(feature = "clustering")]
    async fn row_exists(&self, sql: &str, params: impl IntoParams) -> bool {
        let conn = self.db.guard().await.expect("fresh database connection");
        let mut rows = conn.query(sql, params).await.expect("query committed row");
        rows.next().await.expect("read committed row").is_some()
    }

    async fn count(&self, table: &str) -> i64 {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .expect("count rows");
        rows.next()
            .await
            .expect("read count row")
            .expect("count row exists")
            .get(0)
            .expect("decode count")
    }

    #[cfg(feature = "clustering")]
    async fn assert_frontier(&self, stream_id: &SmSessionId, expected: u32) {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(
                "SELECT inbound_count FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await
            .expect("read frontier");
        let row = rows
            .next()
            .await
            .expect("read frontier row")
            .expect("frontier row exists");
        assert_eq!(
            row.get::<i64>(0).expect("decode frontier"),
            i64::from(expected)
        );
    }

    #[cfg(feature = "clustering")]
    async fn delete_session(&self, stream_id: &SmSessionId) {
        self.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .expect("delete fixture session");
    }

    async fn close(self) {
        let Self {
            db,
            uow,
            admin,
            schema,
            ..
        } = self;
        drop(uow);
        drop(db);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated postgres schema");
    }
}

async fn initialize_existing_store_schemas(db: &Database, schema_url: &str) {
    #[cfg(feature = "clustering")]
    {
        let claims = crate::clustering::claims::PostgresClaimStore::new(db.clone());
        claims
            .ensure_schema()
            .await
            .expect("initialize claims schema");
    }
    crate::sm_persistence::DatabaseSmPersistence::open(Some(schema_url))
        .await
        .expect("initialize SM persistence schema");
    crate::inbox::DatabaseInboxStorage::open(Some(schema_url))
        .await
        .expect("initialize inbox schema");
    waddle_xmpp::mam::SqlxMamStorage::open(schema_url)
        .await
        .expect("initialize MAM schema");
}

fn fixture_lineage_config() -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                .parse()
                .expect("valid fixture deployment UUID"),
        ),
        action: None,
    }
}

#[cfg(feature = "clustering")]
fn sm_claim_entity(stream_id: &SmSessionId) -> String {
    format!(
        "{}:{}",
        EntityType::SmSession.as_db_str(),
        stream_id.as_str()
    )
}

fn digest(byte: u8) -> SemanticDigest {
    SemanticDigest::from_storage(1, [byte; 32]).expect("valid fixture semantic digest")
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
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

#[cfg(feature = "clustering")]
async fn wait_for_lock_waiter(admin: &sqlx::PgPool, fragment: &str) {
    for _ in 0..400 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE $1",
        )
        .bind(format!("%{fragment}%"))
        .fetch_one(admin)
        .await
        .expect("poll pg_stat_activity for a lock waiter");
        if waiting > 0 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("no blocked backend appeared for query fragment {fragment:?}");
}
