//! Closed synthetic authority for this integration test, not a serving store.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use jid::FullJid;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, UserClaimIdentity};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub claim: UserClaimIdentity,
    pub connection: ConnectionRef,
    pub binding: BindingRef,
}

impl Source {
    pub fn new(claim: UserClaimIdentity, binding: BindingRef) -> Self {
        Self {
            claim,
            connection: binding.connection_ref().clone(),
            binding,
        }
    }
}

/// Fabricated by the test caller: this is not an endpoint or actor receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyntheticReceipt {
    pub source: Source,
    pub generation: i64,
}

impl SyntheticReceipt {
    pub fn new(source: &Source, generation: i64) -> Self {
        Self {
            source: source.clone(),
            generation,
        }
    }
}

/// All four external outcomes are assertions by the synthetic test, not
/// evidence that any actual cleanup or delivery occurred.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyntheticRetirement {
    pub endpoint: SyntheticReceipt,
    pub actor: SyntheticReceipt,
    pub directed_presence: SyntheticReceipt,
    pub presentation: SyntheticReceipt,
    pub custody_known: bool,
}

impl SyntheticRetirement {
    pub fn complete(source: &Source, generation: i64) -> Self {
        let receipt = SyntheticReceipt::new(source, generation);
        Self {
            endpoint: receipt.clone(),
            actor: receipt.clone(),
            directed_presence: receipt.clone(),
            presentation: receipt,
            custody_known: true,
        }
    }

    fn matches(&self, source: &Source, generation: i64) -> bool {
        let expected = SyntheticReceipt::new(source, generation);
        self.custody_known
            && self.endpoint == expected
            && self.actor == expected
            && self.directed_presence == expected
            && self.presentation == expected
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    Prepare,
    Publish(SyntheticReceipt),
    Register(SyntheticReceipt),
    ReplaceClaim(UserClaimIdentity),
    Adopt {
        previous: UserClaimIdentity,
        registration: SyntheticReceipt,
    },
    BeginRetirement {
        custody_known: bool,
    },
    CompleteRetirement(SyntheticRetirement),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub id: Uuid,
    pub full_jid: FullJid,
    pub source: Source,
    pub revision: i64,
    pub generation: i64,
    pub action: Action,
}

impl Request {
    pub fn new(full_jid: &FullJid, source: &Source, slot: &Slot, action: Action) -> Self {
        Self {
            id: Uuid::new_v4(),
            full_jid: full_jid.clone(),
            source: source.clone(),
            revision: slot.revision,
            generation: slot.generation,
            action,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Rejection {
    UnsupportedSlot,
    Claim,
    InconsistentSource,
    RevisionOrGeneration,
    Binding,
    BindingClaim,
    Lifecycle,
    Receipt,
    CustodyUnknown,
    ReusedBinding,
    ReusedClaim,
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    Committed { revision: i64, generation: i64 },
    Rejected(Rejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Execution {
    Applied(Outcome),
    Replayed(Outcome),
    PayloadConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Recorded(Outcome),
    PayloadConflict,
    /// Absence cannot establish that a possibly in-flight COMMIT rolled back.
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Vacant,
    Prepared,
    Published,
    Active,
    Retiring,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Vacant => "vacant",
            Self::Prepared => "prepared",
            Self::Published => "published",
            Self::Active => "active",
            Self::Retiring => "retiring",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "vacant" => Ok(Self::Vacant),
            "prepared" => Ok(Self::Prepared),
            "published" => Ok(Self::Published),
            "active" => Ok(Self::Active),
            "retiring" => Ok(Self::Retiring),
            _ => bail!("unknown fixture slot state {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub revision: i64,
    pub generation: i64,
    pub state: State,
    pub binding_claim: Option<UserClaimIdentity>,
    pub connection: Option<ConnectionRef>,
    pub binding: Option<BindingRef>,
}

impl Slot {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self> {
        fn decode<T: serde::de::DeserializeOwned>(
            value: Option<serde_json::Value>,
        ) -> Result<Option<T>> {
            value
                .map(serde_json::from_value)
                .transpose()
                .map_err(Into::into)
        }
        Ok(Self {
            revision: row.try_get("revision")?,
            generation: row.try_get("generation")?,
            state: State::parse(row.try_get("state")?)?,
            binding_claim: decode(row.try_get("binding_claim")?)?,
            connection: decode(row.try_get("connection_ref")?)?,
            binding: decode(row.try_get("binding_ref")?)?,
        })
    }

    pub fn is_current(&self, source: &Source, generation: i64) -> bool {
        self.state == State::Active
            && self.generation == generation
            && self.binding_claim.as_ref() == Some(&source.claim)
            && self.connection.as_ref() == Some(&source.connection)
            && self.binding.as_ref() == Some(&source.binding)
    }
}

pub struct Fixture {
    pub pool: PgPool,
    pub schema: String,
    admin: PgPool,
    owned_schema: OwnedSchema,
}

fn validate_schema_name(schema: &str) -> Result<()> {
    if !schema.starts_with("waddle_claim_lab_")
        || !schema
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!("invalid fixture schema");
    }
    Ok(())
}

/// Creation, marker installation, and identity capture are atomic. The
/// cleanup path checks the original namespace OID, owner, marker relation OID,
/// marker owner, and random marker before dropping the schema.
struct OwnedSchema {
    name: String,
    namespace_oid: i64,
    owner_oid: i64,
    marker_oid: i64,
    marker: Uuid,
}

impl OwnedSchema {
    async fn create(admin: &PgPool, name: String) -> Result<Self> {
        validate_schema_name(&name)?;
        let marker = Uuid::new_v4();
        let mut tx = admin.begin().await?;
        Self::lock_name(&mut tx, &name).await?;
        sqlx::query(&format!("CREATE SCHEMA {name}"))
            .execute(&mut *tx)
            .await?;
        sqlx::query(&format!("CREATE TABLE {name}.lab_fixture_marker (singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton), token UUID NOT NULL)"))
            .execute(&mut *tx)
            .await?;
        sqlx::query(&format!(
            "INSERT INTO {name}.lab_fixture_marker (token) VALUES ($1)"
        ))
        .bind(marker)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query("SELECT n.oid::bigint AS namespace_oid, n.nspowner::bigint AS owner_oid, c.oid::bigint AS marker_oid, c.relowner::bigint AS marker_owner FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid WHERE n.nspname = $1 AND c.relname = 'lab_fixture_marker' AND c.relkind = 'r'")
            .bind(&name)
            .fetch_one(&mut *tx)
            .await?;
        let owner_oid: i64 = row.try_get("owner_oid")?;
        if row.try_get::<i64, _>("marker_owner")? != owner_oid {
            bail!("fixture marker owner differs from schema owner");
        }
        let owned = Self {
            name,
            namespace_oid: row.try_get("namespace_oid")?,
            owner_oid,
            marker_oid: row.try_get("marker_oid")?,
            marker,
        };
        if let Err(error) = tx.commit().await {
            // A failed COMMIT response is uncertain. Only the exact captured
            // identity and marker authorize a cleanup attempt.
            owned
                .remove(admin)
                .await
                .context("cleanup after uncertain schema creation")?;
            return Err(error.into());
        }
        Ok(owned)
    }

    async fn remove(&self, admin: &PgPool) -> Result<()> {
        validate_schema_name(&self.name)?;
        let mut tx = admin.begin().await?;
        Self::lock_name(&mut tx, &self.name).await?;
        let row = sqlx::query("SELECT n.oid::bigint AS namespace_oid, n.nspowner::bigint AS owner_oid, c.oid::bigint AS marker_oid, c.relowner::bigint AS marker_owner FROM pg_catalog.pg_namespace n LEFT JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = 'lab_fixture_marker' AND c.relkind = 'r' WHERE n.nspname = $1")
            .bind(&self.name)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(());
        };
        if row.try_get::<i64, _>("namespace_oid")? != self.namespace_oid
            || row.try_get::<i64, _>("owner_oid")? != self.owner_oid
            || row.try_get::<Option<i64>, _>("marker_oid")? != Some(self.marker_oid)
            || row.try_get::<Option<i64>, _>("marker_owner")? != Some(self.owner_oid)
        {
            bail!("fixture schema identity or ownership changed; refusing cleanup");
        }
        let marker: Option<Uuid> = sqlx::query_scalar(&format!(
            "SELECT token FROM {}.lab_fixture_marker WHERE singleton",
            self.name
        ))
        .fetch_optional(&mut *tx)
        .await?;
        if marker != Some(self.marker) {
            bail!("fixture marker changed; refusing cleanup");
        }
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.name))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn lock_name(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, name: &str) -> Result<()> {
        // If schema-creation COMMIT loses its response, this waits for that
        // transaction to settle before deciding whether the schema exists.
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut **tx)
            .await?;
        sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended($1, 0))")
            .bind(name)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}

pub async fn pool_in_schema(url: &str, schema: &str) -> Result<PgPool> {
    validate_schema_name(schema)?;
    let sql = format!("SET search_path TO {schema}");
    Ok(PgPoolOptions::new()
        .max_connections(5)
        .after_connect(move |connection, _| {
            let sql = sql.clone();
            Box::pin(async move {
                sqlx::query(&sql).execute(connection).await?;
                Ok(())
            })
        })
        .connect(url)
        .await?)
}

impl Fixture {
    pub async fn new(full_jid: &FullJid, claim: &UserClaimIdentity) -> Result<Option<Self>> {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping synthetic claim/binding lab: PostgreSQL URL not set");
            return Ok(None);
        };
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await?;
        let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
            .fetch_one(&admin)
            .await?;
        if !(170_000..180_000).contains(&version) {
            bail!("fixture requires PostgreSQL 17, got {version}");
        }
        if full_jid.to_bare() != *claim.bare_jid() {
            bail!("fixture claim account differs from full JID");
        }
        let schema = format!("waddle_claim_lab_{}", Uuid::new_v4().simple());
        Self::initialize(admin, &url, schema, full_jid, claim, false)
            .await
            .map(Some)
    }

    async fn initialize(
        admin: PgPool,
        url: &str,
        schema: String,
        full_jid: &FullJid,
        claim: &UserClaimIdentity,
        inject_failure: bool,
    ) -> Result<Self> {
        let owned_schema = OwnedSchema::create(&admin, schema.clone()).await?;
        let pool = match pool_in_schema(url, &schema).await {
            Ok(pool) => pool,
            Err(error) => {
                owned_schema
                    .remove(&admin)
                    .await
                    .context("cleanup after pool setup failure")?;
                return Err(error);
            }
        };
        let setup = Self::seed(&pool, full_jid, claim, inject_failure).await;
        if let Err(error) = setup {
            pool.close().await;
            owned_schema
                .remove(&admin)
                .await
                .context("cleanup after fixture setup failure")?;
            return Err(error);
        }
        Ok(Self {
            pool,
            schema,
            admin,
            owned_schema,
        })
    }

    async fn seed(
        pool: &PgPool,
        full_jid: &FullJid,
        claim: &UserClaimIdentity,
        inject_failure: bool,
    ) -> Result<()> {
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(pool)
            .await?;
        if inject_failure {
            bail!("injected failure after fixture schema creation");
        }
        let claim_json = serde_json::to_value(claim)?;
        sqlx::query("INSERT INTO lab_claim VALUES ($1, $2)")
            .bind(claim.bare_jid().to_string())
            .bind(&claim_json)
            .execute(pool)
            .await?;
        sqlx::query("INSERT INTO lab_claim_history VALUES ($1, $2)")
            .bind(claim.bare_jid().to_string())
            .bind(claim_json)
            .execute(pool)
            .await?;
        sqlx::query("INSERT INTO lab_slot (full_jid, bare_jid, revision, generation, state) VALUES ($1, $2, 0, 0, 'vacant')")
            .bind(full_jid.to_string())
            .bind(claim.bare_jid().to_string())
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn check_marker_fence(&self) -> Result<()> {
        sqlx::query("UPDATE lab_fixture_marker SET token = $1")
            .bind(Uuid::new_v4())
            .execute(&self.pool)
            .await?;
        let removal = self.owned_schema.remove(&self.admin).await;
        sqlx::query("UPDATE lab_fixture_marker SET token = $1")
            .bind(self.owned_schema.marker)
            .execute(&self.pool)
            .await?;
        match removal {
            Err(error) if error.to_string().contains("fixture marker changed") => {}
            Err(error) => return Err(error.context("unexpected marker-fence failure")),
            Ok(()) => bail!("cleanup accepted a mismatched marker"),
        }
        Ok(())
    }

    pub async fn close(self) -> Result<()> {
        self.pool.close().await;
        self.owned_schema.remove(&self.admin).await?;
        self.admin.close().await;
        Ok(())
    }
}

pub async fn check_failed_setup_cleanup(
    full_jid: &FullJid,
    claim: &UserClaimIdentity,
) -> Result<()> {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return Ok(());
    };
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?;
    let schema = format!("waddle_claim_lab_{}", Uuid::new_v4().simple());
    let result =
        Fixture::initialize(admin.clone(), &url, schema.clone(), full_jid, claim, true).await;
    let error = match result {
        Err(error) => error,
        Ok(fixture) => {
            fixture.close().await?;
            bail!("injected setup failure unexpectedly succeeded");
        }
    };
    if !error
        .to_string()
        .contains("injected failure after fixture schema creation")
    {
        return Err(error.context("injected setup did not reach its failure point"));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
    )
    .bind(&schema)
    .fetch_one(&admin)
    .await?;
    if exists {
        bail!("schema survived injected setup failure");
    }
    admin.close().await;
    Ok(())
}

pub async fn slot(pool: &PgPool, full_jid: &FullJid) -> Result<Slot> {
    let row = sqlx::query("SELECT * FROM lab_slot WHERE full_jid = $1")
        .bind(full_jid.to_string())
        .fetch_one(pool)
        .await?;
    Slot::from_row(&row)
}

/// Re-check the current claim as well as the current slot before treating a
/// previously returned publication outcome as current. This read is a single
/// database snapshot; it is not authorization for any later external effect.
pub async fn current(
    pool: &PgPool,
    full_jid: &FullJid,
    source: &Source,
    generation: i64,
) -> Result<bool> {
    let row = sqlx::query("SELECT s.*, c.identity AS current_claim FROM lab_slot s JOIN lab_claim c USING (bare_jid) WHERE s.full_jid = $1")
        .bind(full_jid.to_string())
        .fetch_one(pool)
        .await?;
    let claim: UserClaimIdentity = serde_json::from_value(row.try_get("current_claim")?)?;
    Ok(claim == source.claim && Slot::from_row(&row)?.is_current(source, generation))
}

pub async fn lookup(pool: &PgPool, request: &Request) -> Result<Lookup> {
    let row = sqlx::query("SELECT request, outcome FROM lab_operation WHERE operation_id = $1")
        .bind(request.id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(Lookup::Indeterminate);
    };
    let stored: Request = serde_json::from_value(row.try_get("request")?)?;
    if &stored != request {
        return Ok(Lookup::PayloadConflict);
    }
    Ok(Lookup::Recorded(serde_json::from_value(
        row.try_get("outcome")?,
    )?))
}

#[derive(Debug, Clone)]
pub enum Pause {
    None,
    /// Publish the contender's PostgreSQL backend PID before attempting locks.
    ReportBackend(PathBuf),
    /// Signal after the journal and state writes but before COMMIT, then wait
    /// for the test parent to terminate this process.
    BeforeCommit(PathBuf),
    /// Hold the transaction until the parent creates the release file.
    BeforeCommitUntilRelease {
        ready: PathBuf,
        release: PathBuf,
    },
    /// The database COMMIT succeeded; simulate losing the caller response.
    AfterCommit(PathBuf),
}

fn signal_backend(path: &std::path::Path, backend_pid: i32) -> Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, backend_pid.to_string()).context("write fixture backend PID")?;
    std::fs::rename(temporary, path).context("signal fixture parent")?;
    Ok(())
}

async fn signal_and_wait(path: PathBuf, backend_pid: i32) -> Result<()> {
    signal_backend(&path, backend_pid)?;
    tokio::time::sleep(Duration::from_secs(60)).await;
    bail!("fixture parent did not terminate paused child")
}

async fn signal_and_wait_for_release(
    ready: PathBuf,
    release: PathBuf,
    backend_pid: i32,
) -> Result<()> {
    signal_backend(&ready, backend_pid)?;
    tokio::time::timeout(Duration::from_secs(20), async {
        while !release.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("fixture parent did not release child")?;
    Ok(())
}

/// One fixture transaction. It always locks claim then slot and writes the
/// terminal journal entry with the transition. A returned database error or
/// missing journal entry after a lost COMMIT reply is not a rejection result.
pub async fn execute(pool: &PgPool, request: &Request, pause: Pause) -> Result<Execution> {
    let mut tx = pool.begin().await?;
    let backend_pid = if matches!(&pause, Pause::None) {
        None
    } else {
        Some(
            sqlx::query_scalar::<_, i32>("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(&mut *tx)
                .await?,
        )
    };
    if let Pause::ReportBackend(path) = &pause {
        signal_backend(path, backend_pid.context("missing fixture backend PID")?)?;
    }
    // This laboratory has one immutable enrolled slot. Resolve its key before
    // locking so a conflicting replay with a different requested JID still
    // reaches the same operation journal under the same claim/slot locks.
    let enrolled_jid: String = sqlx::query_scalar("SELECT full_jid FROM lab_slot WHERE singleton")
        .fetch_one(&mut *tx)
        .await?;
    let enrolled: FullJid = enrolled_jid.parse()?;
    let row = sqlx::query("SELECT identity FROM lab_claim WHERE bare_jid = $1 FOR UPDATE")
        .bind(enrolled.to_bare().to_string())
        .fetch_one(&mut *tx)
        .await?;
    let mut claim: UserClaimIdentity = serde_json::from_value(row.try_get("identity")?)?;
    let row = sqlx::query("SELECT * FROM lab_slot WHERE full_jid = $1 FOR UPDATE")
        .bind(&enrolled_jid)
        .fetch_one(&mut *tx)
        .await?;
    let mut state = Slot::from_row(&row)?;

    // Each fixture contains one account/slot; its locks serialize all operation
    // IDs before the journal check, including concurrent identical requests.
    let journal = sqlx::query("SELECT request, outcome FROM lab_operation WHERE operation_id = $1")
        .bind(request.id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(row) = journal {
        let previous: Request = serde_json::from_value(row.try_get("request")?)?;
        let result = if previous == *request {
            Execution::Replayed(serde_json::from_value(row.try_get("outcome")?)?)
        } else {
            Execution::PayloadConflict
        };
        tx.commit().await?;
        return Ok(result);
    }

    let source = &request.source;
    let rejection = if request.full_jid != enrolled {
        Some(Rejection::UnsupportedSlot)
    } else if source.claim != claim || source.claim.bare_jid() != &request.full_jid.to_bare() {
        Some(Rejection::Claim)
    } else if source.binding.connection_ref() != &source.connection {
        Some(Rejection::InconsistentSource)
    } else if request.revision != state.revision || request.generation != state.generation {
        Some(Rejection::RevisionOrGeneration)
    } else if state.revision == i64::MAX {
        Some(Rejection::Exhausted)
    } else if !matches!(&request.action, Action::Prepare)
        && (state.connection.as_ref() != Some(&source.connection)
            || state.binding.as_ref() != Some(&source.binding))
    {
        Some(Rejection::Binding)
    } else {
        None
    };

    let rejection = if rejection.is_some() {
        rejection
    } else {
        match &request.action {
            Action::Prepare => {
                if state.state != State::Vacant {
                    Some(Rejection::Lifecycle)
                } else if let Some(next) = state.generation.checked_add(1) {
                    let inserted = sqlx::query("INSERT INTO lab_used_binding (full_jid, generation, binding_ref) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                        .bind(request.full_jid.to_string())
                        .bind(next)
                        .bind(serde_json::to_value(&source.binding)?)
                        .execute(&mut *tx)
                        .await?
                        .rows_affected();
                    if inserted == 0 {
                        Some(Rejection::ReusedBinding)
                    } else {
                        state.generation = next;
                        state.state = State::Prepared;
                        state.binding_claim = Some(source.claim.clone());
                        state.connection = Some(source.connection.clone());
                        state.binding = Some(source.binding.clone());
                        None
                    }
                } else {
                    Some(Rejection::Exhausted)
                }
            }
            Action::Publish(receipt) | Action::Register(receipt) => {
                let expected_state = if matches!(&request.action, Action::Publish(_)) {
                    State::Prepared
                } else {
                    State::Published
                };
                if state.binding_claim.as_ref() != Some(&claim) {
                    Some(Rejection::BindingClaim)
                } else if state.state != expected_state {
                    Some(Rejection::Lifecycle)
                } else if *receipt != SyntheticReceipt::new(source, state.generation) {
                    Some(Rejection::Receipt)
                } else {
                    state.state = if expected_state == State::Prepared {
                        State::Published
                    } else {
                        State::Active
                    };
                    None
                }
            }
            Action::ReplaceClaim(next) => {
                if state.state == State::Retiring || state.state == State::Vacant {
                    Some(Rejection::Lifecycle)
                } else if state.binding_claim.as_ref() != Some(&claim) {
                    Some(Rejection::BindingClaim)
                } else if next.bare_jid() != claim.bare_jid() {
                    Some(Rejection::Claim)
                } else {
                    let inserted = sqlx::query(
                        "INSERT INTO lab_claim_history VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    )
                    .bind(next.bare_jid().to_string())
                    .bind(serde_json::to_value(next)?)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                    if inserted == 0 {
                        Some(Rejection::ReusedClaim)
                    } else {
                        claim = next.clone();
                        None
                    }
                }
            }
            Action::Adopt {
                previous,
                registration,
            } => {
                if state.binding_claim.as_ref() != Some(previous) || previous == &claim {
                    Some(Rejection::BindingClaim)
                } else if state.state != State::Active && state.state != State::Published {
                    Some(Rejection::Lifecycle)
                } else if *registration != SyntheticReceipt::new(source, state.generation) {
                    Some(Rejection::Receipt)
                } else {
                    state.binding_claim = Some(claim.clone());
                    state.state = State::Active;
                    None
                }
            }
            Action::BeginRetirement { custody_known } => {
                if state.state != State::Active {
                    Some(Rejection::Lifecycle)
                } else if state.binding_claim.as_ref() != Some(&claim) {
                    Some(Rejection::BindingClaim)
                } else if !custody_known {
                    Some(Rejection::CustodyUnknown)
                } else {
                    state.state = State::Retiring;
                    None
                }
            }
            Action::CompleteRetirement(receipt) => {
                if state.state != State::Retiring {
                    Some(Rejection::Lifecycle)
                } else if state.binding_claim.as_ref() != Some(&claim) {
                    Some(Rejection::BindingClaim)
                } else if !receipt.matches(source, state.generation) {
                    Some(Rejection::Receipt)
                } else {
                    state.state = State::Vacant;
                    state.binding_claim = None;
                    state.connection = None;
                    state.binding = None;
                    None
                }
            }
        }
    };
    let outcome = if let Some(reason) = rejection {
        Outcome::Rejected(reason)
    } else {
        // The terminal exhaustion check happens before history writes above.
        state.revision = state
            .revision
            .checked_add(1)
            .context("fixture revision exhausted")?;
        sqlx::query("UPDATE lab_claim SET identity = $2 WHERE bare_jid = $1")
            .bind(request.full_jid.to_bare().to_string())
            .bind(serde_json::to_value(&claim)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE lab_slot SET revision = $2, generation = $3, state = $4, binding_claim = $5, connection_ref = $6, binding_ref = $7 WHERE full_jid = $1")
            .bind(request.full_jid.to_string())
            .bind(state.revision)
            .bind(state.generation)
            .bind(state.state.as_str())
            .bind(state.binding_claim.as_ref().map(serde_json::to_value).transpose()?)
            .bind(state.connection.as_ref().map(serde_json::to_value).transpose()?)
            .bind(state.binding.as_ref().map(serde_json::to_value).transpose()?)
            .execute(&mut *tx)
            .await?;
        Outcome::Committed {
            revision: state.revision,
            generation: state.generation,
        }
    };
    sqlx::query("INSERT INTO lab_operation VALUES ($1, $2, $3)")
        .bind(request.id)
        .bind(serde_json::to_value(request)?)
        .bind(serde_json::to_value(&outcome)?)
        .execute(&mut *tx)
        .await?;
    if let Pause::BeforeCommit(path) = &pause {
        signal_and_wait(
            path.clone(),
            backend_pid.context("missing fixture backend PID")?,
        )
        .await?;
    }
    if let Pause::BeforeCommitUntilRelease { ready, release } = &pause {
        signal_and_wait_for_release(
            ready.clone(),
            release.clone(),
            backend_pid.context("missing fixture backend PID")?,
        )
        .await?;
    }
    tx.commit().await?;
    if let Pause::AfterCommit(path) = pause {
        signal_and_wait(path, backend_pid.context("missing fixture backend PID")?).await?;
    }
    Ok(Execution::Applied(outcome))
}
