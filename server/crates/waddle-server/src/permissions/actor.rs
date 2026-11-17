use kameo::actor::Spawn;
use kameo::message::Context;
use kameo::Actor;

use crate::config::ServerConfig;
use crate::db::actor::DbActor;
use crate::db::Database;
use std::sync::Arc;

use super::host_grant::{
    EnrolledScopes, Enrollment, HostGrantStore, IssuancePolicy, ListedWitness, ProjectionIntent,
    ProjectionMarker, ProjectionObservation, ProjectionWrite, RestoredPlan, ScopeLifecycleId,
    WitnessChange, HOST_GRANT_PROJECTION_OBSERVATION_TIMEOUT, HOST_GRANT_RESTORE_SCAN_TIMEOUT,
};
use super::schema::SPICEDB_SCHEMA;
use super::spicedb::SpiceDbPermissionBackend;
use super::{CheckRequest, PermissionChecker, PermissionSchema, TupleStore};
use super::{
    CheckResponse, Object, ObjectType, PermissionError, Relation, Subject, SubjectType, Tuple,
};

enum PermissionActorBackend {
    SpiceDb(Box<SpiceDbPermissionBackend>),
    Local {
        tuple_store: TupleStore,
        // Boxed to keep the enum's `Local` variant small: enabling the
        // `clustering` feature unifies the dependency graph on libp2p's
        // (larger) transitive crypto crates, which grows `PermissionChecker`
        // enough to trip `clippy::large_enum_variant` on this two-variant enum
        // (`SpiceDb` is already boxed for the same reason).
        checker: Box<PermissionChecker>,
    },
}

/// Typed permission names used by runtime actor messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Permission {
    Owner,
    Admin,
    Moderator,
    Member,
    Delete,
    ManageSettings,
    ManageRoles,
    ManageMembers,
    CreateChannel,
    CreateSpace,
    CreateMuc,
    Update,
    View,
    Manage,
    Moderate,
    SendMessage,
    Read,
    Edit,
    React,
    Send,
    AddParticipant,
    Leave,
    Assign,
    Custom(String),
}

impl Permission {
    pub fn as_str(&self) -> &str {
        match self {
            Permission::Owner => "owner",
            Permission::Admin => "admin",
            Permission::Moderator => "moderator",
            Permission::Member => "member",
            Permission::Delete => "delete",
            Permission::ManageSettings => "manage_settings",
            Permission::ManageRoles => "manage_roles",
            Permission::ManageMembers => "manage_members",
            Permission::CreateChannel => "create_channel",
            Permission::CreateSpace => "create_space",
            Permission::CreateMuc => "create_muc",
            Permission::Update => "update",
            Permission::View => "view",
            Permission::Manage => "manage",
            Permission::Moderate => "moderate",
            Permission::SendMessage => "send_message",
            Permission::Read => "read",
            Permission::Edit => "edit",
            Permission::React => "react",
            Permission::Send => "send",
            Permission::AddParticipant => "add_participant",
            Permission::Leave => "leave",
            Permission::Assign => "assign",
            Permission::Custom(value) => value.as_str(),
        }
    }
}

/// Actor that owns the permission backend directly and handles operations via
/// typed message passing.
#[derive(Actor)]
pub struct PermissionActor {
    backend: PermissionActorBackend,
    /// The canonical host-grant store, when this deployment has one. The
    /// tuple-writing messages refuse an object that belongs to one of its
    /// enrolled scopes, decided from the store on every write: only the
    /// authority's own projection writes there.
    host_grants: Option<HostGrantStore>,
    /// The replica's enrolled set. The reading messages answer nothing for
    /// an object it holds (only the host-grant authority decides there),
    /// without any PostgreSQL read in this actor's mailbox.
    enrolled: EnrolledScopes,
    /// Whose accounts may be written a privileged relation
    /// ([`privileged_relation`]): the accounts of the served domains and of
    /// the trusted homes (ADR-0020 D8). A replica that serves attaches it
    /// ([`Self::with_privileged_issuance`]); an actor without one, as the
    /// suites build, asks nothing.
    privileged_issuance: Option<IssuancePolicy>,
}

/// Whether `relation` on `object` is a privileged or room-moderation
/// relation whose issuance this actor decides (ADR-0020, "Privileged
/// grant" and "Room moderation"):
///
/// - the channel and Space `owner` and `admin`, which administer a
///   community. Each has writers that decide issuance themselves, on what
///   they know of the request; the actor refuses them as well, so that a
///   writer that omits or misreads its own check still issues nothing to
///   an account whose home is neither served nor trusted;
/// - the channel `manager` and `moderator`, the Space `moderator` and the
///   role `manager`, which no handler, command or repair path writes by
///   name: they reach the store only through this actor's write messages,
///   so issuance is decided here alone.
fn privileged_relation(object: &Object, relation: &Relation) -> bool {
    matches!(
        (object.object_type, relation.name.as_str()),
        (
            ObjectType::Channel,
            "owner" | "admin" | "manager" | "moderator"
        ) | (ObjectType::Space, "owner" | "admin" | "moderator")
            | (ObjectType::Role, "manager")
    )
}

/// At most one warning a minute that a legacy read was refused because the
/// enrolled set has not loaded.
static READ_UNDECIDED: super::host_grant::RateLimitedWarning =
    super::host_grant::RateLimitedWarning::new();
/// At most one warning a minute that a legacy write was refused because the
/// store could not be read.
static WRITE_UNREADABLE: super::host_grant::RateLimitedWarning =
    super::host_grant::RateLimitedWarning::new();

impl PermissionActor {
    pub fn new(backend: SpiceDbPermissionBackend) -> Self {
        Self {
            backend: PermissionActorBackend::SpiceDb(Box::new(backend)),
            host_grants: None,
            enrolled: EnrolledScopes::default(),
            privileged_issuance: None,
        }
    }

    /// Fence the legacy stores against the scopes enrolled in `store`, with
    /// an enrolled set of the actor's own (see
    /// [`Self::with_host_grant_authority`]).
    pub fn with_host_grant_store(self, store: HostGrantStore) -> Self {
        let enrolled = EnrolledScopes::watch(store.clone());
        self.with_host_grant_authority(store, enrolled)
    }

    /// Fence the legacy stores against the scopes enrolled in `store`.
    ///
    /// A legacy tuple write to an object of an enrolled scope is refused,
    /// decided from the store on every write, so the refusal holds after a
    /// restart and on every replica. A legacy read of such an object answers
    /// nothing, decided from `enrolled`, the replica's in-memory enrolled set
    /// that the host-grant gate shares; until that set has loaded, a read of
    /// a channel or a Space fails.
    pub fn with_host_grant_authority(
        mut self,
        store: HostGrantStore,
        enrolled: EnrolledScopes,
    ) -> Self {
        self.host_grants = Some(store);
        self.enrolled = enrolled;
        self
    }

    pub async fn from_server_config(config: &ServerConfig) -> Result<Self, PermissionError> {
        let spicedb = config
            .spicedb
            .as_ref()
            .ok_or(PermissionError::SpiceDbConfigMissing)?;
        Self::connect(spicedb).await
    }

    /// An actor on the SpiceDB `config` names, without the server's startup
    /// steps: for an operator tool that reads and writes host-grant
    /// witnesses beside servers that ran them.
    pub async fn connect(config: &crate::config::SpiceDbConfig) -> Result<Self, PermissionError> {
        let backend = SpiceDbPermissionBackend::connect(config).await?;
        Ok(Self::new(backend))
    }

    pub fn new_for_tests(db: Arc<Database>) -> Self {
        let actor = DbActor::spawn(DbActor::new((*db).clone()));
        let tuple_store = TupleStore::new(actor.clone());
        let schema = PermissionSchema::default();
        let checker = PermissionChecker::new(actor, schema);

        Self {
            backend: PermissionActorBackend::Local {
                tuple_store,
                checker: Box::new(checker),
            },
            host_grants: None,
            enrolled: EnrolledScopes::default(),
            privileged_issuance: None,
        }
    }

    /// Decide issuance of the privileged relations
    /// ([`privileged_relation`]) under `policy`: such a relation is
    /// written for an account only when its home is a domain this
    /// deployment serves or a trusted home (ADR-0020 D8). Whether a stored
    /// relation is honoured is decided where it is used, from the
    /// principal's witness; this is the refusal at the write.
    pub fn with_privileged_issuance(mut self, policy: IssuancePolicy) -> Self {
        self.privileged_issuance = Some(policy);
        self
    }

    /// Refuse to write `relation` on `object` for `subject` when it is one
    /// of the privileged relations ([`privileged_relation`]) and `subject` is not an
    /// account that may hold it: an account of a served domain or of a
    /// trusted home. A subject that is no account (a Space or channel
    /// userset) is not a grant to a principal and is not asked about. A
    /// subject that names no account address is refused.
    fn refuse_untrusted_privilege(
        &self,
        object: &Object,
        relation: Option<&Relation>,
        subject: &Subject,
    ) -> Result<(), PermissionError> {
        let (Some(policy), Some(relation)) = (&self.privileged_issuance, relation) else {
            return Ok(());
        };
        if !privileged_relation(object, relation)
            || subject.subject_type != SubjectType::User
            || subject.relation.is_some()
        {
            return Ok(());
        }
        let admitted = subject
            .id
            .parse::<jid::BareJid>()
            .is_ok_and(|account| policy.admits_privileged_subject(&account));
        if admitted {
            Ok(())
        } else {
            Err(PermissionError::UntrustedPrivilegedSubject)
        }
    }

    /// Refuse a legacy write that names an object of an enrolled host-grant
    /// scope, as the tuple's object or as a Space or channel subject. A
    /// store that cannot be read refuses the write as well.
    async fn refuse_enrolled_scope(
        &self,
        object: &Object,
        subject: &Subject,
    ) -> Result<(), PermissionError> {
        let Some(store) = &self.host_grants else {
            return Ok(());
        };
        let subject_object = match subject.subject_type {
            SubjectType::Space => Some(Object::new(ObjectType::Space, subject.id.as_str())),
            SubjectType::Channel => Some(Object::new(ObjectType::Channel, subject.id.as_str())),
            SubjectType::User | SubjectType::Role => None,
        };
        for candidate in std::iter::once(object).chain(subject_object.as_ref()) {
            match store.enrolled_object(candidate).await {
                Ok(false) => {}
                Ok(true) => return Err(PermissionError::HostGrantScopeEnrolled),
                Err(error) => {
                    if WRITE_UNREADABLE.due() {
                        tracing::warn!(
                            kind = error.kind(),
                            "Refusing a permission write: the host-grant authority could not be \
                             read"
                        );
                    }
                    return Err(PermissionError::HostGrantAuthorityUnavailable);
                }
            }
        }
        Ok(())
    }

    /// Whether a legacy read of `object` may answer from the tuple store.
    ///
    /// The channel of a managed room and the Space of an enrolled scope are
    /// decided by the host-grant authority alone (`host_grant::HostGrantGate`),
    /// so a legacy read of either answers nothing: a check is not allowed and
    /// a listing is empty, whatever the projection or a stale mirror holds.
    /// The answer comes from the replica's enrolled set, in memory; until it
    /// has loaded, a read of a channel or a Space fails.
    async fn legacy_read_answers(&self, object: &Object) -> Result<bool, PermissionError> {
        if self.host_grants.is_none() {
            return Ok(true);
        }
        match self.enrolled.settled_object(object).await {
            Enrollment::Unmanaged => Ok(true),
            Enrollment::Enrolled => Ok(false),
            Enrollment::Undecided => {
                if READ_UNDECIDED.due() {
                    tracing::warn!(
                        kind = "enrollment_undecided",
                        "Refusing a permission read: the host-grant enrolled set has not loaded"
                    );
                }
                Err(PermissionError::HostGrantAuthorityUnavailable)
            }
        }
    }

    /// The SpiceDB backend, if this actor uses one.
    fn spicedb_backend(&self) -> Option<SpiceDbPermissionBackend> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => Some(backend.as_ref().clone()),
            PermissionActorBackend::Local { .. } => None,
        }
    }

    /// The backend a host-grant projection is read from and written to.
    /// The local backend is not a projection target: it establishes neither
    /// SpiceDB nor cross-store behaviour.
    fn host_grant_projection_backend(&self) -> Result<SpiceDbPermissionBackend, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => Ok(backend.as_ref().clone()),
            PermissionActorBackend::Local { .. } => {
                Err(PermissionError::UnsupportedOperation("local"))
            }
        }
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            PermissionActorBackend::SpiceDb(_) => "spicedb",
            PermissionActorBackend::Local { .. } => "local",
        }
    }

    async fn check(
        &self,
        subject: &Subject,
        permission: &str,
        object: &Object,
    ) -> Result<CheckResponse, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.check(subject, permission, object).await
            }
            PermissionActorBackend::Local { checker, .. } => {
                checker
                    .check(CheckRequest {
                        subject: subject.clone(),
                        permission: permission.to_string(),
                        object: object.clone(),
                    })
                    .await
            }
        }
    }

    async fn write_tuple(&self, tuple: Tuple) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.write_tuple(tuple).await,
            PermissionActorBackend::Local { tuple_store, .. } => tuple_store.write(tuple).await,
        }
    }

    async fn delete_tuple(&self, tuple: &Tuple) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.delete_tuple(tuple).await,
            PermissionActorBackend::Local { tuple_store, .. } => tuple_store.delete(tuple).await,
        }
    }

    async fn replace_exclusive_relation(
        &self,
        msg: ReplaceExclusiveRelation,
    ) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.replace_exclusive_relation(msg).await
            }
            PermissionActorBackend::Local { tuple_store, .. } => {
                tuple_store
                    .replace_exclusive_relation(
                        &msg.object,
                        &msg.subject,
                        &msg.family,
                        msg.replacement.as_ref(),
                    )
                    .await
            }
        }
    }

    async fn swap_exclusive_relation(
        &self,
        msg: SwapExclusiveRelation,
    ) -> Result<ExclusiveRelationSwap, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.swap_exclusive_relation(msg).await,
            PermissionActorBackend::Local { tuple_store, .. } => {
                let swapped = tuple_store
                    .swap_exclusive_relation(
                        &msg.object,
                        &msg.subject,
                        &msg.family,
                        msg.expected.as_ref(),
                        msg.replacement.as_ref(),
                    )
                    .await?;
                Ok(if swapped {
                    ExclusiveRelationSwap::Swapped
                } else {
                    ExclusiveRelationSwap::Mismatch
                })
            }
        }
    }

    async fn list_relations(
        &self,
        subject: &Subject,
        object: &Object,
    ) -> Result<Vec<String>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.list_relations(subject, object).await
            }
            PermissionActorBackend::Local { tuple_store, .. } => {
                tuple_store.list_relations(subject, object).await
            }
        }
    }

    async fn list_subjects(
        &self,
        object: &Object,
        relation: &str,
    ) -> Result<Vec<Subject>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.list_subjects(object, relation).await
            }
            PermissionActorBackend::Local { tuple_store, .. } => {
                tuple_store.list_subjects(object, relation).await
            }
        }
    }

    async fn lookup_resources(
        &self,
        subject: &Subject,
        permission: &str,
        object_type: ObjectType,
    ) -> Result<Vec<Object>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend
                    .lookup_resources(subject, permission, object_type)
                    .await
            }
            PermissionActorBackend::Local { .. } => {
                Err(PermissionError::UnsupportedOperation("local"))
            }
        }
    }

    async fn lookup_subjects(
        &self,
        object: &Object,
        permission: &str,
        subject_type: SubjectType,
    ) -> Result<Vec<Subject>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend
                    .lookup_subjects(object, permission, subject_type)
                    .await
            }
            PermissionActorBackend::Local { .. } => {
                Err(PermissionError::UnsupportedOperation("local"))
            }
        }
    }

    async fn clear_cache(&self) {
        if let PermissionActorBackend::Local { checker, .. } = &self.backend {
            checker.clear_cache().await;
        }
    }

    async fn ensure_schema(&self) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.ensure_schema(SPICEDB_SCHEMA).await,
            PermissionActorBackend::Local { .. } => Ok(()),
        }
    }

    async fn record_schema_version(&self) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.record_schema_version(SPICEDB_SCHEMA).await
            }
            // The local backend evaluates the compiled schema directly and
            // stores no schema to version.
            PermissionActorBackend::Local { .. } => Ok(()),
        }
    }

    async fn ensure_codec_incarnation(&self) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.ensure_codec_incarnation().await,
            // The local backend stores identifiers unencoded in its own
            // tables, so it has no codec to bind.
            PermissionActorBackend::Local { .. } => Ok(()),
        }
    }
}

/// Ensure the external authorization backend has Waddle's built-in schema.
///
/// The schema is written only into an unversioned store or one recorded at an
/// older schema version; a store at this version is left as it is, and a
/// store at a newer or unrecognised version is refused without writing. See
/// `permissions::authz_schema_version`.
pub struct EnsureSchema;

impl kameo::message::Message<EnsureSchema> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        _msg: EnsureSchema,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ensure_schema().await
    }
}

/// Bind the external authorization store to the current ID codec, or refuse
/// it, before the server serves any request. Run after [`EnsureSchema`]: the
/// marker relationship lives in the built-in schema. See
/// `permissions::authz_incarnation` for the protocol.
pub struct EnsureAuthzCodecIncarnation;

impl kameo::message::Message<EnsureAuthzCodecIncarnation> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        _msg: EnsureAuthzCodecIncarnation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ensure_codec_incarnation().await
    }
}

/// Record the built-in schema version in the external authorization store, or
/// refuse a store recorded at a newer or unrecognised version. Run after
/// [`EnsureAuthzCodecIncarnation`], so that a store the codec guard refuses is
/// left without a version record. See `permissions::authz_schema_version`.
pub struct RecordAuthzSchemaVersion;

impl kameo::message::Message<RecordAuthzSchemaVersion> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        _msg: RecordAuthzSchemaVersion,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.record_schema_version().await
    }
}

/// Check whether a subject has a permission on an object.
pub struct CheckPermission {
    pub subject: Subject,
    pub permission: Permission,
    pub object: Object,
}

impl kameo::message::Message<CheckPermission> for PermissionActor {
    type Reply = Result<CheckResponse, PermissionError>;

    async fn handle(
        &mut self,
        msg: CheckPermission,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.legacy_read_answers(&msg.object).await? {
            return Ok(CheckResponse {
                allowed: false,
                reason: None,
            });
        }
        self.check(&msg.subject, msg.permission.as_str(), &msg.object)
            .await
    }
}

/// Write a permission tuple.
pub struct WriteTuple {
    pub tuple: Tuple,
}

impl kameo::message::Message<WriteTuple> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        msg: WriteTuple,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.refuse_enrolled_scope(&msg.tuple.object, &msg.tuple.subject)
            .await?;
        self.refuse_untrusted_privilege(
            &msg.tuple.object,
            Some(&msg.tuple.relation),
            &msg.tuple.subject,
        )?;
        self.write_tuple(msg.tuple).await?;
        self.clear_cache().await;
        Ok(())
    }
}

/// Delete a permission tuple.
pub struct DeleteTuple {
    pub tuple: Tuple,
}

impl kameo::message::Message<DeleteTuple> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        msg: DeleteTuple,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.refuse_enrolled_scope(&msg.tuple.object, &msg.tuple.subject)
            .await?;
        self.delete_tuple(&msg.tuple).await?;
        self.clear_cache().await;
        Ok(())
    }
}

/// Atomically replace all relations in a mutually exclusive family. Normal
/// writes and conditional rollback swaps serialize against the same scope.
pub struct ReplaceExclusiveRelation {
    pub object: Object,
    pub subject: Subject,
    pub family: Vec<Relation>,
    pub replacement: Option<Relation>,
}

impl kameo::message::Message<ReplaceExclusiveRelation> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        msg: ReplaceExclusiveRelation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.refuse_enrolled_scope(&msg.object, &msg.subject)
            .await?;
        self.refuse_untrusted_privilege(&msg.object, msg.replacement.as_ref(), &msg.subject)?;
        self.replace_exclusive_relation(msg).await?;
        self.clear_cache().await;
        Ok(())
    }
}

/// Outcome of [`SwapExclusiveRelation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusiveRelationSwap {
    /// The family held exactly `expected` and now holds `replacement`.
    Swapped,
    /// The family no longer held exactly `expected`; nothing was written.
    Mismatch,
}

/// Atomically replace the relation a subject holds on an object within a
/// mutually exclusive relation family (for example the channel affiliation
/// family `owner`/`admin`/`member`/`outcast`), only while the family still
/// holds exactly `expected`. `None` means the subject holds no relation of
/// the family. SpiceDB evaluates the preconditions and the writes in one
/// request; the local backend issues one conditional SQL statement.
pub struct SwapExclusiveRelation {
    pub object: Object,
    pub subject: Subject,
    pub family: Vec<Relation>,
    pub expected: Option<Relation>,
    pub replacement: Option<Relation>,
}

impl kameo::message::Message<SwapExclusiveRelation> for PermissionActor {
    type Reply = Result<ExclusiveRelationSwap, PermissionError>;

    async fn handle(
        &mut self,
        msg: SwapExclusiveRelation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.refuse_enrolled_scope(&msg.object, &msg.subject)
            .await?;
        self.refuse_untrusted_privilege(&msg.object, msg.replacement.as_ref(), &msg.subject)?;
        let outcome = self.swap_exclusive_relation(msg).await?;
        self.clear_cache().await;
        Ok(outcome)
    }
}

/// Read the SpiceDB projection of one enrolled host-grant scope: its marker
/// and the affiliation family of its rooms. See
/// `permissions::host_grant::projection`.
pub struct ObserveHostGrantProjection {
    pub lifecycle: ScopeLifecycleId,
    /// The authorization object of each of the scope's rooms.
    pub channels: Vec<String>,
}

/// The projection RPCs run on a task of their own, not in the actor's
/// mailbox turn: one observation is up to 33 reads, and permission checks
/// and tuple writes queued behind it must not wait for them. The whole
/// observation is bounded as well as each of its reads.
impl kameo::message::Message<ObserveHostGrantProjection> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<ProjectionObservation, PermissionError>>;

    async fn handle(
        &mut self,
        msg: ObserveHostGrantProjection,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.host_grant_projection_backend();
        ctx.spawn(async move {
            let backend = backend?;
            tokio::time::timeout(
                HOST_GRANT_PROJECTION_OBSERVATION_TIMEOUT,
                backend.observe_host_grant_projection(msg.lifecycle, &msg.channels),
            )
            .await
            .unwrap_or_else(|_| {
                Err(PermissionError::SpiceDbError(
                    "the projection observation exceeded its bound".to_owned(),
                ))
            })
        })
    }
}

/// Write a scope's intended projection and its marker in one request,
/// conditioned on the exact marker `previous`, or on a fresh namespace when
/// there is none. An error leaves the outcome unknown. See
/// `permissions::host_grant::projection`.
pub struct WriteHostGrantProjection {
    pub previous: Option<ProjectionMarker>,
    pub intent: ProjectionIntent,
}

/// Runs on a task of its own, like the observation: the write is one RPC
/// with its own deadline, and the mailbox does not wait for it. The backend
/// that accepts a projection keeps no permission cache, so there is none to
/// invalidate.
impl kameo::message::Message<WriteHostGrantProjection> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<ProjectionWrite, PermissionError>>;

    async fn handle(
        &mut self,
        msg: WriteHostGrantProjection,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.host_grant_projection_backend();
        ctx.spawn(async move {
            backend?
                .write_host_grant_projection(msg.previous.as_ref(), &msg.intent)
                .await
        })
    }
}

/// Write the projection of a host-grant scope that a restore transition
/// closed: its plan's intent conditioned on the exact marker `previous`,
/// which another authority incarnation wrote, or, without one, on a fresh
/// namespace for a scope never projected; and apply `head`, which moves the
/// scope's journal witness to the plan's operation, in the same request.
/// Only the store produces a [`RestoredPlan`], so no other plan can take a
/// projection over or move a journal witness with it. An error leaves the
/// outcome unknown. See `permissions::host_grant::restore`.
pub struct TakeOverHostGrantProjection {
    pub previous: Option<ProjectionMarker>,
    pub plan: RestoredPlan,
    pub head: Option<WitnessChange>,
}

/// Runs on a task of its own, like [`WriteHostGrantProjection`].
impl kameo::message::Message<TakeOverHostGrantProjection> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<ProjectionWrite, PermissionError>>;

    async fn handle(
        &mut self,
        msg: TakeOverHostGrantProjection,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.host_grant_projection_backend();
        ctx.spawn(async move {
            backend?
                .take_over_host_grant_projection(
                    msg.previous.as_ref(),
                    &msg.plan,
                    msg.head.as_ref(),
                )
                .await
        })
    }
}

/// Read every host-grant projection marker and witness the store holds, for
/// the restore fence. `None` when the backend holds no projection (the
/// local backend). See `permissions::host_grant::restore`.
pub struct ListHostGrantWitnesses;

/// Runs on a task of its own: the read is bounded as a whole, and the
/// mailbox does not wait for it.
impl kameo::message::Message<ListHostGrantWitnesses> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<Option<Vec<ListedWitness>>, PermissionError>>;

    async fn handle(
        &mut self,
        _msg: ListHostGrantWitnesses,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.spicedb_backend();
        ctx.spawn(async move {
            let Some(backend) = backend else {
                return Ok(None);
            };
            tokio::time::timeout(
                HOST_GRANT_RESTORE_SCAN_TIMEOUT,
                backend.list_host_grant_witnesses(),
            )
            .await
            .unwrap_or_else(|_| {
                Err(PermissionError::SpiceDbError(
                    "the projection marker scan exceeded its bound".to_owned(),
                ))
            })
            .map(Some)
        })
    }
}

/// Read one host-grant witness resource. `None` when the backend holds no
/// projection. See `permissions::host_grant::witness`.
pub struct ReadHostGrantWitness {
    pub resource: String,
}

/// Runs on a task of its own, like [`ObserveHostGrantProjection`].
impl kameo::message::Message<ReadHostGrantWitness> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<Option<ListedWitness>, PermissionError>>;

    async fn handle(
        &mut self,
        msg: ReadHostGrantWitness,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.spicedb_backend();
        ctx.spawn(async move {
            let Some(backend) = backend else {
                return Ok(None);
            };
            backend
                .read_host_grant_witness(&msg.resource)
                .await
                .map(Some)
        })
    }
}

/// Apply one write of host-grant witnesses: each change's resource must
/// hold exactly its `previous` stamp (or nothing), and holds its `next`
/// stamp (or nothing) after the request, in one request. `None` when the
/// backend holds no projection. An error leaves the outcome unknown. See
/// `permissions::host_grant::witness`.
pub struct ReplaceHostGrantWitnesses {
    pub changes: Vec<WitnessChange>,
}

/// Runs on a task of its own, like [`WriteHostGrantProjection`].
impl kameo::message::Message<ReplaceHostGrantWitnesses> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<Option<ProjectionWrite>, PermissionError>>;

    async fn handle(
        &mut self,
        msg: ReplaceHostGrantWitnesses,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.spicedb_backend();
        ctx.spawn(async move {
            let Some(backend) = backend else {
                return Ok(None);
            };
            backend
                .apply_host_grant_witness_changes(&msg.changes)
                .await
                .map(Some)
        })
    }
}

/// Remove every relationship each of `resources` holds in the host-grant
/// projection definition, one request per resource in order, each
/// conditioned on `condition`. `None` when the backend holds no projection.
/// An error leaves the outcome unknown. See
/// `permissions::host_grant::witness`.
pub struct ClearHostGrantWitnesses {
    pub resources: Vec<String>,
    pub condition: WitnessChange,
}

/// Runs on a task of its own, like [`WriteHostGrantProjection`].
impl kameo::message::Message<ClearHostGrantWitnesses> for PermissionActor {
    type Reply = kameo::reply::DelegatedReply<Result<Option<ProjectionWrite>, PermissionError>>;

    async fn handle(
        &mut self,
        msg: ClearHostGrantWitnesses,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let backend = self.spicedb_backend();
        ctx.spawn(async move {
            let Some(backend) = backend else {
                return Ok(None);
            };
            backend
                .clear_host_grant_witnesses(&msg.resources, &msg.condition)
                .await
                .map(Some)
        })
    }
}

/// List direct relations a subject has on an object.
pub struct ListRelations {
    pub subject: Subject,
    pub object: Object,
}

impl kameo::message::Message<ListRelations> for PermissionActor {
    type Reply = Result<Vec<Relation>, PermissionError>;

    async fn handle(
        &mut self,
        msg: ListRelations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.legacy_read_answers(&msg.object).await? {
            return Ok(Vec::new());
        }
        let relations = self.list_relations(&msg.subject, &msg.object).await?;
        Ok(relations.into_iter().map(Relation::new).collect())
    }
}

/// List subjects that have a relation on an object.
pub struct ListSubjects {
    pub object: Object,
    pub relation: Relation,
}

impl kameo::message::Message<ListSubjects> for PermissionActor {
    type Reply = Result<Vec<Subject>, PermissionError>;

    async fn handle(
        &mut self,
        msg: ListSubjects,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.legacy_read_answers(&msg.object).await? {
            return Ok(Vec::new());
        }
        self.list_subjects(&msg.object, &msg.relation.name).await
    }
}

/// Lookup resources a subject may access for a permission.
pub struct LookupResources {
    pub subject: Subject,
    pub permission: Permission,
    pub object_type: ObjectType,
}

impl kameo::message::Message<LookupResources> for PermissionActor {
    type Reply = Result<Vec<Object>, PermissionError>;

    async fn handle(
        &mut self,
        msg: LookupResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let resources = self
            .lookup_resources(&msg.subject, msg.permission.as_str(), msg.object_type)
            .await?;
        let mut answered = Vec::with_capacity(resources.len());
        for resource in resources {
            if self.legacy_read_answers(&resource).await? {
                answered.push(resource);
            }
        }
        Ok(answered)
    }
}

/// Lookup subjects that may access an object for a permission.
pub struct LookupSubjects {
    pub object: Object,
    pub permission: Permission,
    pub subject_type: SubjectType,
}

impl kameo::message::Message<LookupSubjects> for PermissionActor {
    type Reply = Result<Vec<Subject>, PermissionError>;

    async fn handle(
        &mut self,
        msg: LookupSubjects,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.legacy_read_answers(&msg.object).await? {
            return Ok(Vec::new());
        }
        self.lookup_subjects(&msg.object, msg.permission.as_str(), msg.subject_type)
            .await
    }
}

#[cfg(test)]
mod tests;
