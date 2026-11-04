//! Synthetic allocation witness, deliberately disconnected from SM and sockets.
//!
//! Every countable fixture stanza uses this same counter. Payloads are typed
//! stanza serializations, not evidence of exact transport bytes. A returned
//! witness, including a replay, never authorizes an enqueue or proves a write.
//! Fencing assumes all fixture writers use this typed API: the test runtime
//! role can modify the mutable stream state with raw SQL. Admission payloads
//! and slot payloads have independent quotas, so their combined retention may
//! reach twice the configured byte limit. Lookup needs the complete original
//! frame and is not an autonomous post-restart inventory.

use super::{
    capacity, encode_payload, identical_admission, lookup_postgres, lookup_sqlite, prepare_v1009,
    quote_identifier, read_sqlite_epoch, Backend, DarkIngressAuthorityError, DirectAdmission,
    DirectAdmissionLab, DirectAdmissionLabError, DirectAdmissionLimits, FixtureConnection,
    PhysicalAttestationConfig, MAX_FRAME_BYTES, MAX_ROWS,
};
use sqlx::{Postgres, Row, Transaction};
use thiserror::Error;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::Stanza;

const MAX_FIXTURE_CAPABILITIES: i64 = 4096;
const MAX_CAPABILITIES_PER_STREAM: i64 = 64;

const POSTGRES_DDL: &str = r#"
CREATE TABLE __SCHEMA__.direct_fixture_registry (id INTEGER PRIMARY KEY CHECK (id = 1));
INSERT INTO __SCHEMA__.direct_fixture_registry VALUES (1);
CREATE TABLE __SCHEMA__.direct_fixture_stream (
    stream_id UUID PRIMARY KEY,
    resource TEXT NOT NULL,
    connection_generation UUID NOT NULL,
    sm_session_id TEXT NOT NULL,
    capability UUID NOT NULL,
    previous_capability UUID,
    ordinal BIGINT NOT NULL CHECK (ordinal >= 0),
    pending BIGINT,
    retired BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (resource, connection_generation, sm_session_id),
    CHECK (pending IS NULL OR pending = ordinal)
);
CREATE TABLE __SCHEMA__.direct_fixture_capability (
    stream_id UUID NOT NULL REFERENCES __SCHEMA__.direct_fixture_stream (stream_id),
    capability UUID NOT NULL,
    PRIMARY KEY (stream_id, capability)
);
CREATE TABLE __SCHEMA__.direct_fixture_slot (
    frame_id UUID PRIMARY KEY,
    admission_id UUID UNIQUE REFERENCES __SCHEMA__.direct_admission_lab (admission_id),
    stream_id UUID NOT NULL REFERENCES __SCHEMA__.direct_fixture_stream (stream_id),
    ordinal BIGINT NOT NULL CHECK (ordinal > 0),
    kind INTEGER NOT NULL CHECK (kind IN (1, 2)),
    payload TEXT NOT NULL CHECK (octet_length(payload) BETWEEN 1 AND 262144),
    UNIQUE (stream_id, ordinal),
    CHECK ((kind = 1 AND admission_id IS NOT NULL AND admission_id = frame_id) OR (kind = 2 AND admission_id IS NULL))
);
CREATE TABLE __SCHEMA__.direct_fixture_resolution (
    stream_id UUID NOT NULL,
    ordinal BIGINT NOT NULL,
    PRIMARY KEY (stream_id, ordinal),
    FOREIGN KEY (stream_id, ordinal) REFERENCES __SCHEMA__.direct_fixture_slot (stream_id, ordinal)
);
CREATE FUNCTION __SCHEMA__.direct_fixture_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN RAISE EXCEPTION 'fixture evidence is immutable'; END $$;
CREATE TRIGGER direct_fixture_capability_immutable BEFORE UPDATE OR DELETE ON __SCHEMA__.direct_fixture_capability
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE TRIGGER direct_fixture_capability_no_truncate BEFORE TRUNCATE ON __SCHEMA__.direct_fixture_capability
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE TRIGGER direct_fixture_slot_immutable BEFORE UPDATE OR DELETE ON __SCHEMA__.direct_fixture_slot
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE TRIGGER direct_fixture_slot_no_truncate BEFORE TRUNCATE ON __SCHEMA__.direct_fixture_slot
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE TRIGGER direct_fixture_resolution_immutable BEFORE UPDATE OR DELETE ON __SCHEMA__.direct_fixture_resolution
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE TRIGGER direct_fixture_resolution_no_truncate BEFORE TRUNCATE ON __SCHEMA__.direct_fixture_resolution
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE TRIGGER direct_fixture_stream_no_delete BEFORE DELETE OR TRUNCATE ON __SCHEMA__.direct_fixture_stream
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.direct_fixture_immutable();
CREATE FUNCTION __SCHEMA__.direct_fixture_identity_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
  IF (NEW.stream_id, NEW.resource, NEW.connection_generation, NEW.sm_session_id)
      IS DISTINCT FROM (OLD.stream_id, OLD.resource, OLD.connection_generation, OLD.sm_session_id)
     OR (OLD.retired AND NOT NEW.retired) THEN
    RAISE EXCEPTION 'fixture stream identity is immutable';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER direct_fixture_identity_immutable BEFORE UPDATE ON __SCHEMA__.direct_fixture_stream
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.direct_fixture_identity_immutable();
"#;

/// Unregistered DDL to install only in a disposable attested test schema.
pub fn allocation_lab_postgres_ddl(config: &PhysicalAttestationConfig) -> String {
    POSTGRES_DDL.replace(
        "__SCHEMA__",
        &quote_identifier(&config.postgres().schema.name.0),
    )
}

/// Unregistered DDL for the shared SQLite test database.
pub const ALLOCATION_LAB_SQLITE_DDL: &str = r#"
CREATE TABLE main.direct_fixture_registry (id INTEGER PRIMARY KEY CHECK (id = 1));
INSERT INTO main.direct_fixture_registry VALUES (1);
CREATE TABLE main.direct_fixture_stream (
    stream_id TEXT PRIMARY KEY,
    resource TEXT NOT NULL,
    connection_generation TEXT NOT NULL,
    sm_session_id TEXT NOT NULL,
    capability TEXT NOT NULL,
    previous_capability TEXT,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    pending INTEGER,
    retired INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
    UNIQUE (resource, connection_generation, sm_session_id),
    CHECK (pending IS NULL OR pending = ordinal)
) WITHOUT ROWID;
CREATE TABLE main.direct_fixture_capability (
    stream_id TEXT NOT NULL REFERENCES direct_fixture_stream (stream_id),
    capability TEXT NOT NULL,
    PRIMARY KEY (stream_id, capability)
) WITHOUT ROWID;
CREATE TABLE main.direct_fixture_slot (
    frame_id TEXT PRIMARY KEY,
    admission_id TEXT UNIQUE REFERENCES direct_admission_lab (admission_id),
    stream_id TEXT NOT NULL REFERENCES direct_fixture_stream (stream_id),
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    kind INTEGER NOT NULL CHECK (kind IN (1, 2)),
    payload TEXT NOT NULL CHECK (length(CAST(payload AS BLOB)) BETWEEN 1 AND 262144),
    UNIQUE (stream_id, ordinal),
    CHECK ((kind = 1 AND admission_id IS NOT NULL AND admission_id = frame_id) OR (kind = 2 AND admission_id IS NULL))
) WITHOUT ROWID;
CREATE TABLE main.direct_fixture_resolution (
    stream_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (stream_id, ordinal),
    FOREIGN KEY (stream_id, ordinal) REFERENCES direct_fixture_slot (stream_id, ordinal)
) WITHOUT ROWID;
CREATE TRIGGER main.direct_fixture_capability_no_replace BEFORE INSERT ON direct_fixture_capability
WHEN EXISTS (SELECT 1 FROM main.direct_fixture_capability WHERE stream_id = NEW.stream_id AND capability = NEW.capability)
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_capability_no_update BEFORE UPDATE ON direct_fixture_capability
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_capability_no_delete BEFORE DELETE ON direct_fixture_capability
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_slot_no_replace BEFORE INSERT ON direct_fixture_slot
WHEN EXISTS (SELECT 1 FROM main.direct_fixture_slot WHERE frame_id = NEW.frame_id
 OR (NEW.admission_id IS NOT NULL AND admission_id = NEW.admission_id)
 OR (stream_id = NEW.stream_id AND ordinal = NEW.ordinal))
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_slot_no_update BEFORE UPDATE ON direct_fixture_slot
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_slot_no_delete BEFORE DELETE ON direct_fixture_slot
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_resolution_no_replace BEFORE INSERT ON direct_fixture_resolution
WHEN EXISTS (SELECT 1 FROM main.direct_fixture_resolution WHERE stream_id = NEW.stream_id AND ordinal = NEW.ordinal)
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_resolution_no_update BEFORE UPDATE ON direct_fixture_resolution
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_resolution_no_delete BEFORE DELETE ON direct_fixture_resolution
BEGIN SELECT RAISE(ABORT, 'fixture evidence is immutable'); END;
CREATE TRIGGER main.direct_fixture_stream_no_replace BEFORE INSERT ON direct_fixture_stream
WHEN EXISTS (SELECT 1 FROM main.direct_fixture_stream WHERE stream_id = NEW.stream_id OR
 (resource = NEW.resource AND connection_generation = NEW.connection_generation AND sm_session_id = NEW.sm_session_id))
BEGIN SELECT RAISE(ABORT, 'fixture stream identity is immutable'); END;
CREATE TRIGGER main.direct_fixture_stream_no_delete BEFORE DELETE ON direct_fixture_stream
BEGIN SELECT RAISE(ABORT, 'fixture stream identity is immutable'); END;
CREATE TRIGGER main.direct_fixture_stream_identity BEFORE UPDATE ON direct_fixture_stream
WHEN NEW.stream_id != OLD.stream_id OR NEW.resource != OLD.resource OR
 NEW.connection_generation != OLD.connection_generation OR NEW.sm_session_id != OLD.sm_session_id OR
 (OLD.retired = 1 AND NEW.retired != 1)
BEGIN SELECT RAISE(ABORT, 'fixture stream identity is immutable'); END;
"#;

/// A synthetic stream identity, not a captured runtime stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureStream {
    pub id: uuid::Uuid,
    pub connection: FixtureConnection,
}

/// One countable fixture stanza. The non-ingress variant shares the very same
/// counter as admissions and stores its complete typed serialization.
pub enum FixtureFrame<'a> {
    Admission(&'a DirectAdmission),
    Other { id: uuid::Uuid, stanza: &'a Stanza },
}

/// This is a fixture-only database capability, not permission to write a socket.
#[derive(Debug, Clone)]
pub struct FixtureAllocationCapability {
    stream: FixtureStream,
    token: uuid::Uuid,
}

/// Created only after the old fixture capability has been fenced under the
/// stream lock. A stale copy stops working as soon as another fence commits.
#[derive(Debug, Clone)]
pub struct FencedFixtureCapability(FixtureAllocationCapability);

impl FencedFixtureCapability {
    pub fn capability(&self) -> &FixtureAllocationCapability {
        &self.0
    }
}

/// Immutable fixture binding. `h` is the low 32 bits of the non-wrapping
/// ordinal; neither value asserts that an XMPP stream wrote or acknowledged it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureAllocationWitness {
    pub stream: FixtureStream,
    pub frame_id: uuid::Uuid,
    pub ordinal: u64,
    pub h: u32,
}

#[derive(Debug, Error)]
pub enum FixtureAllocationError {
    #[error("the fixture stream, capability, or admission was rejected")]
    Rejected,
    #[error("the fixture identity or payload conflicts with immutable evidence")]
    Conflict,
    #[error("the previous fixture slot remains unresolved")]
    Unresolved,
    #[error("the fixture stream is retired")]
    Retired,
    #[error("the non-wrapping fixture ordinal is exhausted")]
    Overflow,
    #[error("the fixture allocation capacity is exhausted")]
    Capacity,
    #[error(transparent)]
    Admission(#[from] DirectAdmissionLabError),
    #[error(transparent)]
    Authority(#[from] DarkIngressAuthorityError),
}

struct FrameInput {
    id: uuid::Uuid,
    kind: i32,
    payload: String,
}

impl FixtureFrame<'_> {
    fn input(&self, connection: &FixtureConnection) -> Result<FrameInput, FixtureAllocationError> {
        let (id, kind, payload) = match self {
            Self::Admission(a) => {
                if &a.connection != connection {
                    return Err(FixtureAllocationError::Rejected);
                }
                (
                    a.pin.admission_id,
                    1,
                    encode_payload(&a.pin.payload).map_err(|_| FixtureAllocationError::Rejected)?,
                )
            }
            Self::Other { id, stanza } => (*id, 2, encode_other(stanza)?),
        };
        if id.is_nil() || payload.is_empty() {
            return Err(FixtureAllocationError::Rejected);
        }
        if payload.len() > MAX_FRAME_BYTES {
            return Err(FixtureAllocationError::Capacity);
        }
        Ok(FrameInput { id, kind, payload })
    }
}

// Reject a typed stanza if serialization cannot reproduce it. This preserves
// fixture semantics without claiming the serialization is the socket's bytes.
fn encode_other(stanza: &Stanza) -> Result<String, FixtureAllocationError> {
    if let Stanza::Message(message) = stanza {
        return encode_payload(message).map_err(|_| FixtureAllocationError::Rejected);
    }
    let encoded = waddle_xmpp::parser::element_to_string(&stanza.to_element())
        .map_err(|_| FixtureAllocationError::Rejected)?;
    let element: minidom::Element = encoded
        .parse()
        .map_err(|_| FixtureAllocationError::Rejected)?;
    let same = match stanza {
        Stanza::Presence(expected) => xmpp_parsers::presence::Presence::try_from(element)
            .is_ok_and(|actual| &actual == expected),
        Stanza::Iq(expected) => {
            xmpp_parsers::iq::Iq::try_from(element).is_ok_and(|actual| &actual == expected.as_ref())
        }
        Stanza::Message(_) => unreachable!(),
    };
    if !same {
        return Err(FixtureAllocationError::Rejected);
    }
    Ok(encoded)
}

struct StreamRow {
    resource: String,
    generation: uuid::Uuid,
    session: String,
    capability: uuid::Uuid,
    previous: Option<uuid::Uuid>,
    ordinal: i64,
    pending: Option<i64>,
    retired: bool,
}

impl StreamRow {
    fn matches(&self, stream: &FixtureStream) -> bool {
        self.resource == stream.connection.resource.to_string()
            && self.generation == stream.connection.generation
            && self.session == stream.connection.sm_session.as_str()
    }
    fn check(&self, cap: &FixtureAllocationCapability) -> Result<(), FixtureAllocationError> {
        if !self.matches(&cap.stream) || self.capability != cap.token {
            return Err(FixtureAllocationError::Rejected);
        }
        Ok(())
    }
}

struct SlotRow {
    stream: uuid::Uuid,
    ordinal: i64,
    kind: i32,
    payload: String,
}

fn witness(
    stream: &FixtureStream,
    id: uuid::Uuid,
    ordinal: i64,
) -> Result<FixtureAllocationWitness, FixtureAllocationError> {
    let ordinal = u64::try_from(ordinal).map_err(|_| DarkIngressAuthorityError::Database)?;
    if ordinal == 0 {
        return Err(DarkIngressAuthorityError::Database.into());
    }
    Ok(FixtureAllocationWitness {
        stream: stream.clone(),
        frame_id: id,
        ordinal,
        h: ordinal as u32,
    })
}

fn check_slot(
    row: SlotRow,
    stream: &FixtureStream,
    input: &FrameInput,
) -> Result<FixtureAllocationWitness, FixtureAllocationError> {
    if row.stream != stream.id || row.kind != input.kind || row.payload != input.payload {
        return Err(FixtureAllocationError::Conflict);
    }
    witness(stream, input.id, row.ordinal)
}

fn valid_stream(stream: &FixtureStream, expected: &FixtureConnection) -> bool {
    stream.id != uuid::Uuid::nil()
        && &stream.connection == expected
        && stream.connection.generation != uuid::Uuid::nil()
        && !stream.connection.sm_session.as_str().is_empty()
        && SmSessionId::try_from_wire(stream.connection.sm_session.as_str()).is_ok()
}

#[derive(Clone, Copy)]
enum Action<'a> {
    Open {
        stream: &'a FixtureStream,
        token: uuid::Uuid,
    },
    Fence {
        old: &'a FixtureAllocationCapability,
        next: uuid::Uuid,
    },
    Allocate {
        cap: &'a FixtureAllocationCapability,
        frame: &'a FixtureFrame<'a>,
        input: &'a FrameInput,
    },
    Lookup {
        cap: &'a FixtureAllocationCapability,
        frame: &'a FixtureFrame<'a>,
        input: &'a FrameInput,
    },
    Reconcile {
        cap: &'a FixtureAllocationCapability,
        witness: &'a FixtureAllocationWitness,
    },
    Retire {
        cap: &'a FixtureAllocationCapability,
    },
}

enum Reply {
    Done,
    Allocation(Option<FixtureAllocationWitness>),
}

enum LabTx<'a, 'b> {
    Postgres(&'a mut Transaction<'b, Postgres>, &'a str),
    Sqlite(&'a mut crate::db::Transaction<'b>),
}

impl DirectAdmissionLab {
    /// Open a synthetic stream with a caller-chosen fresh capability. An exact
    /// retry is safe after an uncertain fixture setup commit.
    pub async fn open_fixture_stream(
        &self,
        stream: &FixtureStream,
        token: uuid::Uuid,
    ) -> Result<FixtureAllocationCapability, FixtureAllocationError> {
        if !valid_stream(stream, &self.connection) || token.is_nil() {
            return Err(FixtureAllocationError::Rejected);
        }
        self.fixture_action(Action::Open { stream, token }).await?;
        Ok(FixtureAllocationCapability {
            stream: stream.clone(),
            token,
        })
    }

    /// Fence the old capability while holding the same lock as allocation.
    /// After cancellation or an unknown COMMIT, retry this exact rotation until
    /// it succeeds before looking up the allocation. The new token must be fresh.
    pub async fn fence_fixture_capability(
        &self,
        old: &FixtureAllocationCapability,
        next: uuid::Uuid,
    ) -> Result<FencedFixtureCapability, FixtureAllocationError> {
        if next.is_nil() || next == old.token || !valid_stream(&old.stream, &self.connection) {
            return Err(FixtureAllocationError::Rejected);
        }
        self.fixture_action(Action::Fence { old, next }).await?;
        Ok(FencedFixtureCapability(FixtureAllocationCapability {
            stream: old.stream.clone(),
            token: next,
        }))
    }

    /// Allocate one slot or recover the identical binding. All countable fixture
    /// stanzas must pass here, including stanzas unrelated to an ingress admission.
    pub async fn allocate_fixture(
        &self,
        cap: &FixtureAllocationCapability,
        frame: &FixtureFrame<'_>,
    ) -> Result<FixtureAllocationWitness, FixtureAllocationError> {
        if !valid_stream(&cap.stream, &self.connection) {
            return Err(FixtureAllocationError::Rejected);
        }
        let input = frame.input(&self.connection)?;
        match self
            .fixture_action(Action::Allocate {
                cap,
                frame,
                input: &input,
            })
            .await?
        {
            Reply::Allocation(Some(w)) => Ok(w),
            _ => Err(DarkIngressAuthorityError::Database.into()),
        }
    }

    /// Lookup is synchronized with allocation and requires a successfully
    /// fenced old capability. Absence is only a point-in-time observation: any
    /// holder of the new capability must also be stopped before relying on it.
    pub async fn lookup_fixture(
        &self,
        fenced: &FencedFixtureCapability,
        frame: &FixtureFrame<'_>,
    ) -> Result<Option<FixtureAllocationWitness>, FixtureAllocationError> {
        let cap = &fenced.0;
        if !valid_stream(&cap.stream, &self.connection) {
            return Err(FixtureAllocationError::Rejected);
        }
        let input = frame.input(&self.connection)?;
        match self
            .fixture_action(Action::Lookup {
                cap,
                frame,
                input: &input,
            })
            .await?
        {
            Reply::Allocation(w) => Ok(w),
            _ => Err(DarkIngressAuthorityError::Database.into()),
        }
    }

    /// Record only that this synthetic slot was reconciled in the model. This
    /// is not evidence of enqueue, exposure, delivery, or acknowledgement.
    pub async fn reconcile_fixture_slot(
        &self,
        cap: &FixtureAllocationCapability,
        binding: &FixtureAllocationWitness,
    ) -> Result<(), FixtureAllocationError> {
        if !valid_stream(&cap.stream, &self.connection) {
            return Err(FixtureAllocationError::Rejected);
        }
        self.fixture_action(Action::Reconcile {
            cap,
            witness: binding,
        })
        .await?;
        Ok(())
    }

    /// Permanently retire a synthetic stream, including any unresolved slot.
    pub async fn retire_fixture_stream(
        &self,
        cap: &FixtureAllocationCapability,
    ) -> Result<(), FixtureAllocationError> {
        if !valid_stream(&cap.stream, &self.connection) {
            return Err(FixtureAllocationError::Rejected);
        }
        self.fixture_action(Action::Retire { cap }).await?;
        Ok(())
    }

    async fn fixture_action(&self, action: Action<'_>) -> Result<Reply, FixtureAllocationError> {
        match &self.authority.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut conn = self.authority.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.authority.bounds.acquisition,
                    prepare_v1009(&mut conn, schema, *schema_oid, self.authority.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let reply = run_action(
                    &mut LabTx::Postgres(&mut prepared.transaction, schema),
                    action,
                    self.limits,
                )
                .await?;
                prepared.commit().await?;
                Ok(reply)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let reply = run_action(&mut LabTx::Sqlite(&mut tx), action, self.limits).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(reply)
            }
        }
    }
}

async fn run_action(
    tx: &mut LabTx<'_, '_>,
    action: Action<'_>,
    limits: DirectAdmissionLimits,
) -> Result<Reply, FixtureAllocationError> {
    let stream = match &action {
        Action::Open { stream, .. } => *stream,
        Action::Fence { old, .. } => &old.stream,
        Action::Allocate { cap, .. }
        | Action::Lookup { cap, .. }
        | Action::Reconcile { cap, .. }
        | Action::Retire { cap } => &cap.stream,
    };
    // Open alone takes registry -> stream, so concurrent absent-row opens
    // serialize. Allocation takes stream -> quota; admission takes canonical
    // and attempt -> quota and never the stream. No path takes quota -> stream.
    if matches!(action, Action::Open { .. }) {
        tx.lock_registry().await?;
    }
    let row = tx.stream(stream.id).await?;
    if let Action::Open { stream, token } = action {
        if let Some(row) = row {
            if !row.matches(stream) || row.capability != token || row.retired {
                return Err(FixtureAllocationError::Conflict);
            }
        } else {
            tx.reserve_identity_capacity(stream.id, true).await?;
            tx.open(stream, token).await?;
            tx.remember_capability(stream.id, token).await?;
        }
        return Ok(Reply::Done);
    }
    let row = row.ok_or(FixtureAllocationError::Rejected)?;
    match action {
        Action::Open { .. } => unreachable!(),
        Action::Fence { old, next } => {
            if !row.matches(&old.stream) {
                return Err(FixtureAllocationError::Rejected);
            }
            if row.capability == old.token {
                if tx.seen_capability(stream.id, next).await? {
                    return Err(FixtureAllocationError::Rejected);
                }
                tx.reserve_identity_capacity(stream.id, false).await?;
                tx.remember_capability(stream.id, next).await?;
                tx.fence(stream.id, old.token, next).await?;
            } else if row.capability != next || row.previous != Some(old.token) {
                return Err(FixtureAllocationError::Rejected);
            }
            Ok(Reply::Done)
        }
        Action::Allocate { cap, frame, input } | Action::Lookup { cap, frame, input } => {
            row.check(cap)?;
            if let FixtureFrame::Admission(expected) = frame {
                let stored = tx
                    .admission(input.id)
                    .await?
                    .ok_or(FixtureAllocationError::Rejected)?;
                if !identical_admission(&stored, expected) {
                    return Err(FixtureAllocationError::Conflict);
                }
            }
            let existing = tx.slot(input.id).await?;
            if let Some(existing) = existing {
                let binding = check_slot(existing, stream, input)?;
                if matches!(action, Action::Allocate { .. }) && row.retired {
                    return Err(FixtureAllocationError::Retired);
                }
                return Ok(Reply::Allocation(Some(binding)));
            }
            if matches!(action, Action::Lookup { .. }) {
                return Ok(Reply::Allocation(None));
            }
            if row.retired {
                return Err(FixtureAllocationError::Retired);
            }
            if row.pending.is_some() {
                return Err(FixtureAllocationError::Unresolved);
            }
            let ordinal = row
                .ordinal
                .checked_add(1)
                .ok_or(FixtureAllocationError::Overflow)?;
            tx.reserve_capacity(input.payload.len(), limits).await?;
            // A different stream may have inserted this global frame ID while
            // this transaction waited for the shared capacity lock.
            if let Some(existing) = tx.slot(input.id).await? {
                return Ok(Reply::Allocation(Some(check_slot(
                    existing, stream, input,
                )?)));
            }
            tx.allocate(stream.id, ordinal, input).await?;
            Ok(Reply::Allocation(Some(witness(stream, input.id, ordinal)?)))
        }
        Action::Reconcile {
            cap,
            witness: binding,
        } => {
            row.check(cap)?;
            if binding.stream != *stream
                || binding.ordinal == 0
                || binding.h != binding.ordinal as u32
            {
                return Err(FixtureAllocationError::Rejected);
            }
            let ordinal =
                i64::try_from(binding.ordinal).map_err(|_| FixtureAllocationError::Rejected)?;
            let slot = tx
                .slot(binding.frame_id)
                .await?
                .ok_or(FixtureAllocationError::Rejected)?;
            if slot.stream != stream.id || slot.ordinal != ordinal {
                return Err(FixtureAllocationError::Rejected);
            }
            if tx.resolved(stream.id, ordinal).await? {
                return Ok(Reply::Done);
            }
            if row.retired {
                return Err(FixtureAllocationError::Retired);
            }
            if row.pending != Some(ordinal) {
                return Err(FixtureAllocationError::Rejected);
            }
            tx.reconcile(stream.id, ordinal).await?;
            Ok(Reply::Done)
        }
        Action::Retire { cap } => {
            row.check(cap)?;
            if !row.retired {
                tx.retire(stream.id).await?;
            }
            Ok(Reply::Done)
        }
    }
}

impl LabTx<'_, '_> {
    async fn lock_registry(&mut self) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                sqlx::query(&format!(
                    "SELECT id FROM {}.direct_fixture_registry WHERE id = 1 FOR UPDATE",
                    quote_identifier(schema)
                ))
                .fetch_optional(&mut ***tx)
                .await
                .map_err(super::super::super::postgres_error)?
                .ok_or(DarkIngressAuthorityError::Database)?;
            }
            Self::Sqlite(tx) => {
                let mut rows = tx
                    .query(
                        "SELECT id FROM main.direct_fixture_registry WHERE id = 1",
                        (),
                    )
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?;
                rows.next()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?
                    .ok_or(DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }

    async fn stream(
        &mut self,
        id: uuid::Uuid,
    ) -> Result<Option<StreamRow>, FixtureAllocationError> {
        fn bad<E>(_: E) -> DarkIngressAuthorityError {
            DarkIngressAuthorityError::Database
        }
        match self {
            Self::Postgres(tx, schema) => {
                let row = sqlx::query(&format!("SELECT resource, connection_generation::text, sm_session_id, capability::text, previous_capability::text, ordinal, pending, retired FROM {}.direct_fixture_stream WHERE stream_id = $1 FOR UPDATE", quote_identifier(schema))).bind(id).fetch_optional(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
                row.map(|r| {
                    let generation: String = r.try_get(1).map_err(bad)?;
                    let capability: String = r.try_get(3).map_err(bad)?;
                    let previous: Option<String> = r.try_get(4).map_err(bad)?;
                    Ok(StreamRow {
                        resource: r.try_get(0).map_err(bad)?,
                        generation: generation.parse().map_err(bad)?,
                        session: r.try_get(2).map_err(bad)?,
                        capability: capability.parse().map_err(bad)?,
                        previous: previous.map(|v| v.parse().map_err(bad)).transpose()?,
                        ordinal: r.try_get(5).map_err(bad)?,
                        pending: r.try_get(6).map_err(bad)?,
                        retired: r.try_get(7).map_err(bad)?,
                    })
                })
                .transpose()
            }
            Self::Sqlite(tx) => {
                let mut rows = tx.query("SELECT resource, connection_generation, sm_session_id, capability, previous_capability, ordinal, pending, retired FROM main.direct_fixture_stream WHERE stream_id = ?", crate::db_params![id.to_string()]).await.map_err(bad)?;
                let Some(r) = rows.next().await.map_err(bad)? else {
                    return Ok(None);
                };
                let generation: String = r.get(1).map_err(bad)?;
                let capability: String = r.get(3).map_err(bad)?;
                let previous: Option<String> = r.get(4).map_err(bad)?;
                let retired: i64 = r.get(7).map_err(bad)?;
                Ok(Some(StreamRow {
                    resource: r.get(0).map_err(bad)?,
                    generation: generation.parse().map_err(bad)?,
                    session: r.get(2).map_err(bad)?,
                    capability: capability.parse().map_err(bad)?,
                    previous: previous.map(|v| v.parse().map_err(bad)).transpose()?,
                    ordinal: r.get(5).map_err(bad)?,
                    pending: r.get(6).map_err(bad)?,
                    retired: retired != 0,
                }))
            }
        }
    }

    async fn open(
        &mut self,
        stream: &FixtureStream,
        token: uuid::Uuid,
    ) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                sqlx::query(&format!("INSERT INTO {}.direct_fixture_stream (stream_id, resource, connection_generation, sm_session_id, capability, ordinal) VALUES ($1, $2, $3, $4, $5, 0)", quote_identifier(schema))).bind(stream.id).bind(stream.connection.resource.to_string()).bind(stream.connection.generation).bind(stream.connection.sm_session.as_str()).bind(token).execute(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
            }
            Self::Sqlite(tx) => {
                tx.execute("INSERT INTO main.direct_fixture_stream (stream_id, resource, connection_generation, sm_session_id, capability, ordinal) VALUES (?, ?, ?, ?, ?, 0)", crate::db_params![stream.id.to_string(), stream.connection.resource.to_string(), stream.connection.generation.to_string(), stream.connection.sm_session.as_str().to_owned(), token.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }

    // Stream and capability evidence has a hard global bound independent of
    // slot quotas; reaching it fails closed rather than reusing a fenced token.
    async fn reserve_identity_capacity(
        &mut self,
        stream: uuid::Uuid,
        opening: bool,
    ) -> Result<(), FixtureAllocationError> {
        let (streams, capabilities, stream_capabilities): (i64, i64, i64) = match self {
            Self::Postgres(tx, schema) => {
                let q = quote_identifier(schema);
                sqlx::query(&format!(
                    "SELECT id FROM {q}.direct_admission_lab_quota WHERE id = 1 FOR UPDATE"
                ))
                .fetch_optional(&mut ***tx)
                .await
                .map_err(super::super::super::postgres_error)?
                .ok_or(DarkIngressAuthorityError::Database)?;
                sqlx::query_as(&format!("SELECT (SELECT count(*) FROM {q}.direct_fixture_stream), count(*), count(*) FILTER (WHERE stream_id = $1) FROM {q}.direct_fixture_capability")).bind(stream).fetch_one(&mut ***tx).await.map_err(super::super::super::postgres_error)?
            }
            Self::Sqlite(tx) => {
                let mut rows = tx.query("SELECT (SELECT count(*) FROM main.direct_fixture_stream), count(*), COALESCE(sum(CASE WHEN stream_id = ? THEN 1 ELSE 0 END), 0) FROM main.direct_fixture_capability", crate::db_params![stream.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                let row = rows
                    .next()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?
                    .ok_or(DarkIngressAuthorityError::Database)?;
                (
                    row.get(0)
                        .map_err(|_| DarkIngressAuthorityError::Database)?,
                    row.get(1)
                        .map_err(|_| DarkIngressAuthorityError::Database)?,
                    row.get(2)
                        .map_err(|_| DarkIngressAuthorityError::Database)?,
                )
            }
        };
        if (opening && streams >= i64::from(MAX_ROWS))
            || capabilities >= MAX_FIXTURE_CAPABILITIES
            || stream_capabilities >= MAX_CAPABILITIES_PER_STREAM
        {
            return Err(FixtureAllocationError::Capacity);
        }
        Ok(())
    }

    async fn seen_capability(
        &mut self,
        stream: uuid::Uuid,
        token: uuid::Uuid,
    ) -> Result<bool, FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => Ok(sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {}.direct_fixture_capability WHERE stream_id = $1 AND capability = $2)", quote_identifier(schema))).bind(stream).bind(token).fetch_one(&mut ***tx).await.map_err(super::super::super::postgres_error)?),
            Self::Sqlite(tx) => {
                let mut rows = tx.query("SELECT 1 FROM main.direct_fixture_capability WHERE stream_id = ? AND capability = ?", crate::db_params![stream.to_string(), token.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                Ok(rows.next().await.map_err(|_| DarkIngressAuthorityError::Database)?.is_some())
            }
        }
    }

    async fn remember_capability(
        &mut self,
        stream: uuid::Uuid,
        token: uuid::Uuid,
    ) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                sqlx::query(&format!("INSERT INTO {}.direct_fixture_capability (stream_id, capability) VALUES ($1, $2)", quote_identifier(schema))).bind(stream).bind(token).execute(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
            }
            Self::Sqlite(tx) => {
                tx.execute("INSERT INTO main.direct_fixture_capability (stream_id, capability) VALUES (?, ?)", crate::db_params![stream.to_string(), token.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }

    async fn fence(
        &mut self,
        id: uuid::Uuid,
        old: uuid::Uuid,
        next: uuid::Uuid,
    ) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                sqlx::query(&format!("UPDATE {}.direct_fixture_stream SET previous_capability = $2, capability = $3 WHERE stream_id = $1", quote_identifier(schema))).bind(id).bind(old).bind(next).execute(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
            }
            Self::Sqlite(tx) => {
                tx.execute("UPDATE main.direct_fixture_stream SET previous_capability = ?, capability = ? WHERE stream_id = ?", crate::db_params![old.to_string(), next.to_string(), id.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }

    async fn admission(
        &mut self,
        id: uuid::Uuid,
    ) -> Result<Option<DirectAdmission>, FixtureAllocationError> {
        Ok(match self {
            Self::Postgres(tx, schema) => lookup_postgres(tx, schema, id).await?,
            Self::Sqlite(tx) => lookup_sqlite(tx, id).await?,
        })
    }

    async fn slot(&mut self, id: uuid::Uuid) -> Result<Option<SlotRow>, FixtureAllocationError> {
        fn bad<E>(_: E) -> DarkIngressAuthorityError {
            DarkIngressAuthorityError::Database
        }
        match self {
            Self::Postgres(tx, schema) => {
                let row = sqlx::query(&format!("SELECT stream_id::text, ordinal, kind, payload FROM {}.direct_fixture_slot WHERE frame_id = $1", quote_identifier(schema))).bind(id).fetch_optional(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
                row.map(|r| {
                    let stream: String = r.try_get(0).map_err(bad)?;
                    Ok(SlotRow {
                        stream: stream.parse().map_err(bad)?,
                        ordinal: r.try_get(1).map_err(bad)?,
                        kind: r.try_get(2).map_err(bad)?,
                        payload: r.try_get(3).map_err(bad)?,
                    })
                })
                .transpose()
            }
            Self::Sqlite(tx) => {
                let mut rows = tx.query("SELECT stream_id, ordinal, kind, payload FROM main.direct_fixture_slot WHERE frame_id = ?", crate::db_params![id.to_string()]).await.map_err(bad)?;
                let Some(r) = rows.next().await.map_err(bad)? else {
                    return Ok(None);
                };
                let stream: String = r.get(0).map_err(bad)?;
                let kind: i64 = r.get(2).map_err(bad)?;
                Ok(Some(SlotRow {
                    stream: stream.parse().map_err(bad)?,
                    ordinal: r.get(1).map_err(bad)?,
                    kind: i32::try_from(kind).map_err(bad)?,
                    payload: r.get(3).map_err(bad)?,
                }))
            }
        }
    }

    async fn reserve_capacity(
        &mut self,
        payload: usize,
        limits: DirectAdmissionLimits,
    ) -> Result<(), FixtureAllocationError> {
        let (rows, bytes): (i64, i64) = match self {
            Self::Postgres(tx, schema) => {
                let q = quote_identifier(schema);
                sqlx::query(&format!(
                    "SELECT id FROM {q}.direct_admission_lab_quota WHERE id = 1 FOR UPDATE"
                ))
                .fetch_optional(&mut ***tx)
                .await
                .map_err(super::super::super::postgres_error)?
                .ok_or(DarkIngressAuthorityError::Database)?;
                sqlx::query_as(&format!("SELECT count(*), COALESCE(sum(octet_length(payload)), 0)::bigint FROM {q}.direct_fixture_slot")).fetch_one(&mut ***tx).await.map_err(super::super::super::postgres_error)?
            }
            Self::Sqlite(tx) => {
                let mut rows = tx.query("SELECT count(*), COALESCE(sum(length(CAST(payload AS BLOB))), 0) FROM main.direct_fixture_slot", ()).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                let row = rows
                    .next()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?
                    .ok_or(DarkIngressAuthorityError::Database)?;
                (
                    row.get(0)
                        .map_err(|_| DarkIngressAuthorityError::Database)?,
                    row.get(1)
                        .map_err(|_| DarkIngressAuthorityError::Database)?,
                )
            }
        };
        capacity(rows, bytes, payload, limits).map_err(|error| match error {
            DirectAdmissionLabError::Capacity => FixtureAllocationError::Capacity,
            other => FixtureAllocationError::Admission(other),
        })
    }

    async fn allocate(
        &mut self,
        stream: uuid::Uuid,
        ordinal: i64,
        input: &FrameInput,
    ) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                let q = quote_identifier(schema);
                let admission = if input.kind == 1 {
                    Some(input.id)
                } else {
                    None
                };
                sqlx::query(&format!("INSERT INTO {q}.direct_fixture_slot (frame_id, admission_id, stream_id, ordinal, kind, payload) VALUES ($1, $2, $3, $4, $5, $6)")).bind(input.id).bind(admission).bind(stream).bind(ordinal).bind(input.kind).bind(&input.payload).execute(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
                sqlx::query(&format!("UPDATE {q}.direct_fixture_stream SET ordinal = $2, pending = $2 WHERE stream_id = $1")).bind(stream).bind(ordinal).execute(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
            }
            Self::Sqlite(tx) => {
                let admission = if input.kind == 1 {
                    Some(input.id.to_string())
                } else {
                    None
                };
                tx.execute("INSERT INTO main.direct_fixture_slot (frame_id, admission_id, stream_id, ordinal, kind, payload) VALUES (?, ?, ?, ?, ?, ?)", crate::db_params![input.id.to_string(), admission, stream.to_string(), ordinal, input.kind, input.payload.clone()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                tx.execute("UPDATE main.direct_fixture_stream SET ordinal = ?, pending = ? WHERE stream_id = ?", crate::db_params![ordinal, ordinal, stream.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }

    async fn resolved(
        &mut self,
        stream: uuid::Uuid,
        ordinal: i64,
    ) -> Result<bool, FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => Ok(sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {}.direct_fixture_resolution WHERE stream_id = $1 AND ordinal = $2)", quote_identifier(schema))).bind(stream).bind(ordinal).fetch_one(&mut ***tx).await.map_err(super::super::super::postgres_error)?),
            Self::Sqlite(tx) => {
                let mut rows = tx.query("SELECT 1 FROM main.direct_fixture_resolution WHERE stream_id = ? AND ordinal = ?", crate::db_params![stream.to_string(), ordinal]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                Ok(rows.next().await.map_err(|_| DarkIngressAuthorityError::Database)?.is_some())
            }
        }
    }

    async fn reconcile(
        &mut self,
        stream: uuid::Uuid,
        ordinal: i64,
    ) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                let q = quote_identifier(schema);
                sqlx::query(&format!("INSERT INTO {q}.direct_fixture_resolution (stream_id, ordinal) VALUES ($1, $2)")).bind(stream).bind(ordinal).execute(&mut ***tx).await.map_err(super::super::super::postgres_error)?;
                sqlx::query(&format!(
                    "UPDATE {q}.direct_fixture_stream SET pending = NULL WHERE stream_id = $1"
                ))
                .bind(stream)
                .execute(&mut ***tx)
                .await
                .map_err(super::super::super::postgres_error)?;
            }
            Self::Sqlite(tx) => {
                tx.execute(
                    "INSERT INTO main.direct_fixture_resolution (stream_id, ordinal) VALUES (?, ?)",
                    crate::db_params![stream.to_string(), ordinal],
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Database)?;
                tx.execute(
                    "UPDATE main.direct_fixture_stream SET pending = NULL WHERE stream_id = ?",
                    crate::db_params![stream.to_string()],
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }

    async fn retire(&mut self, stream: uuid::Uuid) -> Result<(), FixtureAllocationError> {
        match self {
            Self::Postgres(tx, schema) => {
                sqlx::query(&format!(
                    "UPDATE {}.direct_fixture_stream SET retired = TRUE WHERE stream_id = $1",
                    quote_identifier(schema)
                ))
                .bind(stream)
                .execute(&mut ***tx)
                .await
                .map_err(super::super::super::postgres_error)?;
            }
            Self::Sqlite(tx) => {
                tx.execute(
                    "UPDATE main.direct_fixture_stream SET retired = 1 WHERE stream_id = ?",
                    crate::db_params![stream.to_string()],
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Database)?;
            }
        }
        Ok(())
    }
}
