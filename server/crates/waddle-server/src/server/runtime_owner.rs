//! Owner phase for a server that serves under the restricted PostgreSQL
//! runtime role, with or without clustering.
//!
//! The runtime role holds no DDL privilege, so every relation it attaches to
//! at startup is created here, through the schema owner, before the runtime
//! connects. [`prepare_restricted_runtime_owner`] runs the registered
//! migrations, each store's owner preparation, and the runtime grants on one
//! supplied owner database handle.
//!
//! The phases commit independently and each is idempotent, so a failure
//! leaves a completed prefix that a retry extends. Nothing here installs the
//! physical attestor (that needs the separate installer login), starts a
//! worker, or returns a store handle. Operator preconditions are the same as
//! for the individual owner APIs: old writers and other schema changes are
//! fenced for the whole call, and the schema owner already holds EXECUTE on
//! `pg_catalog.pg_control_system()`.
//!
//! A plan with a clustering section additionally prepares the claim
//! relations, the peer allowlist, the keypair-pool enrollment and the durable
//! room tables, and grants the runtime the clustered privilege set. Fenced
//! stream management uses the relations the portable preparation creates.
//! Without that section none of the clustering stores is prepared and the
//! runtime holds no privilege on any clustering relation a database carries.

use thiserror::Error;
use waddle_xmpp::inbox::storage::InboxStorageError;
use waddle_xmpp::mam::{MamSchemaBootstrapError, SqlxMamStorage};
use waddle_xmpp::pending_delivery::storage::PendingStorageError;
use waddle_xmpp::stream_management::persistence::SmPersistenceError;
use waddle_xmpp::XmppError;

use crate::call_teardown_outbox::{CallTeardownOutboxError, CallTeardownOutboxStore};
#[cfg(feature = "clustering")]
pub use crate::clustering::allowlist::AllowlistRemoval;
#[cfg(feature = "clustering")]
pub use crate::clustering::lease::LegacyKeypairSlots;
#[cfg(feature = "clustering")]
use crate::clustering::lease::{KeypoolEnrollment, KeypoolPreparation, PostgresKeypairSlotLease};
use crate::config::LineageConfig;
use crate::db::bootstrap::{bootstrap_global_owner, OwnerBootstrapError};
use crate::db::lineage::{self, AttestedLineage, PgIdentity};
use crate::db::physical_attestation::{quote_identifier, PgRoleName};
use crate::db::{Database, DatabaseDriver, DatabaseError};
use crate::inbox::bootstrap_inbox_owner_schema;
use crate::ingress_uow::{
    initialize_room_observations, prepare_dark_room_observation_schema, ObservationError,
    RoomObservationSchemaError,
};
use crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use crate::notification_activity::{NotificationActivityError, NotificationActivityStore};
use crate::pending_delivery::bootstrap_pending_delivery_owner_schema;
use crate::pubsub::DatabasePubSubStorage;
use crate::push_registrations::{DatabasePushRegistrationStore, PushRegistrationOwnerError};
use crate::room_effect_outbox::{RoomEffectOutboxError, RoomEffectOutboxStore};
use crate::sm_persistence::bootstrap_portable_sm_owner_schema;
use crate::spaces_metadata::{prepare_space_stores_owner, SpaceStoreAttachError};

use super::push_notification_owner::{
    prepare_core_push_outbox_vapid_owner, CorePushOutboxVapidOwnerPreparationError,
};
use super::routes::webhook_delivery;

/// Independently supplied inputs for the owner phase. None of the role names
/// is inferred from the owner connection.
#[derive(Clone, Debug)]
pub struct RestrictedRuntimeOwnerPlan {
    /// Deployment identity, and the enrollment or adoption action when the
    /// database has no lineage row for it yet.
    pub lineage: LineageConfig,
    /// Login that opened the owner handle. It must be the session user and
    /// the sole login able to assume `schema_owner`.
    pub migrator: PgRoleName,
    /// NOLOGIN role that owns the schema and is the handle's current role.
    pub schema_owner: PgRoleName,
    /// Restricted login the server will serve under.
    pub runtime: PgRoleName,
    /// NOLOGIN owner of the physical attestor's trusted schema.
    pub attestation_definer: PgRoleName,
    /// Root key the VAPID signing key is sealed under. The runtime must be
    /// started with the same session key.
    pub vapid_root_key: Vec<u8>,
    /// Present when the runtime serves with clustering enabled.
    #[cfg(feature = "clustering")]
    pub clustering: Option<ClusteringOwnerPlan>,
}

/// Clustering inputs for the owner phase.
#[cfg(feature = "clustering")]
#[derive(Clone, Debug)]
pub struct ClusteringOwnerPlan {
    /// PeerIds of the configured keypair pool, in pool order. They become
    /// both the keypair-pool enrollment and the exact peer allowlist.
    pub peer_ids: Vec<libp2p::PeerId>,
    /// Generation the keypair-pool enrollment is recorded under. A database
    /// already enrolled under another generation is refused.
    pub keypool_generation: uuid::Uuid,
    /// How long a newly enrolled generation stays pending. While it is
    /// pending the phase ends with
    /// [`RestrictedRuntimeOwnerError::KeypoolActivationPending`] and the
    /// runtime refuses to attach; rerunning the phase after the delay
    /// activates it.
    pub activation_delay: std::time::Duration,
    /// What to do with a keypair-slot table an unattested runtime created.
    pub legacy_keypair_slots: LegacyKeypairSlots,
    /// The keypair-slot lease lifetime the servers are configured with
    /// (`WADDLE_CLUSTERING_LEASE_TTL_MS`). A retirement refuses a
    /// `legacy_ttl` below it. If the retired deployment ran with a longer
    /// lifetime, `legacy_ttl` must be that longer value.
    pub lease_ttl: std::time::Duration,
    /// What to do with enrolled PeerIds the plan does not name.
    pub allowlist: AllowlistRemoval,
    /// Whether the phase requires the database to itself.
    pub exclusive_database: ExclusiveDatabase,
}

#[cfg(feature = "clustering")]
impl ClusteringOwnerPlan {
    /// A plan that activates immediately, refuses an unenrolled keypair-slot
    /// table and PeerIds it does not name, assumes the default lease
    /// lifetime, and requires an exclusive database.
    /// `owner_application_name` is the `application_name` the owner handle's
    /// sessions were opened with; it is unique to this run.
    pub fn new(
        peer_ids: Vec<libp2p::PeerId>,
        keypool_generation: uuid::Uuid,
        owner_application_name: String,
    ) -> Self {
        Self {
            peer_ids,
            keypool_generation,
            activation_delay: std::time::Duration::ZERO,
            legacy_keypair_slots: LegacyKeypairSlots::Refuse,
            lease_ttl: crate::config::ClusteringLeaseConfig::default().lease_ttl,
            allowlist: AllowlistRemoval::Refuse,
            exclusive_database: ExclusiveDatabase::Required {
                owner_application_name,
            },
        }
    }
}

/// Whether the clustered owner phase checks that it has the database to
/// itself: no other client session and no prepared transaction. The check
/// runs before the phase changes anything and again, with the lease table
/// locked, before an unenrolled lease table is retired.
#[cfg(feature = "clustering")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExclusiveDatabase {
    /// Every session of the owner handle was opened with this
    /// `application_name`, generated for this run. Any other client session
    /// on the database is refused, including one of the migrator login.
    Required { owner_application_name: String },
    /// The caller vouches that concurrent sessions are harmless.
    NotChecked,
}

/// The owner phase that failed. Earlier phases have committed.
#[derive(Debug, Error)]
pub enum RestrictedRuntimeOwnerError {
    #[error("the runtime owner phase requires an unattested PostgreSQL main-lane owner handle")]
    OwnerHandleRequired,
    #[error("the owner database has no PostgreSQL physical identity")]
    MissingPostgresIdentity,
    #[error("lineage and migration owner bootstrap failed")]
    Global(#[source] OwnerBootstrapError),
    #[error("could not attest the owner database lineage")]
    Lineage(#[source] DatabaseError),
    #[error("could not apply the runtime grant for {object}")]
    Grant {
        object: String,
        #[source]
        source: DatabaseError,
    },
    #[error("could not inspect the prepared schema for runtime grants")]
    GrantInventory(#[source] DatabaseError),
    #[error(
        "PUBLIC holds privileges on schema {schema}; the restricted runtime needs a dedicated \
         schema without PUBLIC access"
    )]
    PublicSchemaPrivilege { schema: String },
    #[error("relation {relation} has no runtime grant classification")]
    UnclassifiedRelation { relation: String },
    #[error("the runtime role already holds {privilege} on {relation}, which the grant matrix does not allow")]
    ExcessRuntimePrivilege { relation: String, privilege: String },
    #[error(
        "the runtime role is not restricted at {component}; every store attachment would refuse it"
    )]
    UnsafeRuntimeRole { component: &'static str },
    #[error("room observation table preparation failed")]
    RoomObservationTables(#[source] ObservationError),
    #[error("room observation owner preparation failed")]
    RoomObservation(#[source] RoomObservationSchemaError),
    #[error("pending delivery owner preparation failed")]
    PendingDelivery(#[source] PendingStorageError),
    #[error("inbox owner preparation failed")]
    Inbox(#[source] InboxStorageError),
    #[error("stream management owner preparation failed")]
    StreamManagement(#[source] SmPersistenceError),
    #[error("PubSub owner preparation failed")]
    PubSub(#[source] XmppError),
    #[error("space store owner preparation failed")]
    SpaceStores(#[source] SpaceStoreAttachError),
    #[error("push registration owner preparation failed")]
    PushRegistrations(#[source] PushRegistrationOwnerError),
    #[error("notification activity owner preparation failed")]
    NotificationActivity(#[source] NotificationActivityError),
    #[error("call teardown outbox owner preparation failed")]
    CallTeardown(#[source] CallTeardownOutboxError),
    #[error("Push Service, notification outbox and VAPID owner preparation failed")]
    PushNotification(#[source] CorePushOutboxVapidOwnerPreparationError),
    #[error("room effect outbox owner preparation failed")]
    RoomEffect(#[source] RoomEffectOutboxError),
    #[error("MUC destroy completion outbox owner preparation failed")]
    MucDestroy(#[source] DatabaseError),
    #[error("could not open a dedicated owner session for the MAM schema")]
    MamSession(#[source] DatabaseError),
    #[error("MAM owner preparation failed and may have changed schema")]
    Mam(#[source] MamSchemaBootstrapError),
    #[error("webhook delivery ledger owner preparation failed")]
    WebhookDeliveries(#[source] DatabaseError),
    #[error("could not inspect database sessions and prepared transactions")]
    ExclusiveDatabaseInspection(#[source] DatabaseError),
    #[error(
        "the owner handle's session does not carry the application_name the plan names, so \
         its sessions cannot be told apart from another client's"
    )]
    OwnerSessionUntagged,
    #[error(
        "the clustered owner phase needs the database to itself: {sessions} other client \
         session(s) and {prepared_transactions} prepared transaction(s) are present"
    )]
    DatabaseNotExclusive {
        sessions: i64,
        prepared_transactions: i64,
    },
    #[error(
        "relation {relation} belongs to a keypair-pool enrollment, but the plan has no \
         clustering section; a database enrolled for clustering is served with clustering"
    )]
    KeypoolEnrolledWithoutClusteringPlan { relation: String },
    #[cfg(feature = "clustering")]
    #[error("claim owner preparation failed")]
    Claims(#[source] crate::clustering::claims::ClaimOwnerPreparationError),
    #[cfg(feature = "clustering")]
    #[error("peer allowlist owner preparation failed")]
    PeerAllowlist(#[source] crate::clustering::allowlist::AllowlistOwnerPreparationError),
    #[cfg(feature = "clustering")]
    #[error("keypair-pool owner preparation failed")]
    Keypool(#[source] crate::clustering::lease::KeypoolOwnerError),
    #[cfg(feature = "clustering")]
    #[error("durable room owner preparation failed")]
    MucRooms(#[source] crate::muc_durable::MucOwnerPreparationError),
    #[error(
        "every schema is prepared, but the keypair-pool generation is still pending; rerun the \
         owner phase after its activation delay"
    )]
    KeypoolActivationPending,
}

/// Evidence that every owner phase and runtime grant completed. It carries the
/// lineage the runtime handoff must name and no database handle.
#[must_use]
pub struct RestrictedRuntimeOwnerEvidence {
    lineage: AttestedLineage,
}

impl RestrictedRuntimeOwnerEvidence {
    /// The attested lineage and physical identity the phases were bound to.
    pub fn attested_lineage(&self) -> &AttestedLineage {
        &self.lineage
    }
}

/// A classified relation the runtime holds no privilege on.
const NONE: &str = "";
const READ: &str = "SELECT";
const READ_INSERT: &str = "SELECT, INSERT";
const READ_INSERT_UPDATE: &str = "SELECT, INSERT, UPDATE";
const READ_INSERT_DELETE: &str = "SELECT, INSERT, DELETE";
const READ_WRITE: &str = "SELECT, INSERT, UPDATE, DELETE";

/// The runtime's table privileges. Every table in the schema is listed here
/// or in [`OWNER_GRANTED_TABLES`]; a table in neither fails the phase, so a
/// new relation is classified before the runtime can touch it.
///
/// Each entry is the set the runtime's statements need. Where a store's
/// attachment verifies an exact set, the entry is that set. Row locks
/// (`FOR UPDATE`, `FOR SHARE`) and `ON CONFLICT DO UPDATE` count as UPDATE.
/// Deletes that only happen through `ON DELETE CASCADE` need no privilege:
/// the referential action runs as the table owner.
const TABLE_GRANTS: &[(&str, &str)] = &[
    // Lineage and migration ledger are read-only to the runtime, as are the
    // owner-state and marker tables each store's owner preparation writes.
    ("_lineage", READ),
    ("_migrations", READ),
    ("_waddle_space_store_owner", READ),
    ("notification_activity_owner_state", READ),
    ("notification_outbox_owner_state", READ),
    ("pending_delivery_startup_migrations", READ),
    ("pubsub_schema_marker", READ),
    ("pubsub_schema_version", READ),
    ("push_service_schema_marker", READ),
    ("push_service_vapid_schema_marker", READ),
    // The epoch row takes a column grant below; the manifest is read-only.
    ("ingress_protocol_epoch", READ),
    ("ingress_epoch_guard_manifest", READ),
    // Accounts, sessions and authentication handshakes.
    ("users", READ_INSERT_UPDATE),
    ("native_users", READ_WRITE),
    ("auth_identities", READ_INSERT_UPDATE),
    ("sessions", READ_WRITE),
    ("device_auth", READ_WRITE),
    ("pending_auth", READ_INSERT_DELETE),
    ("xmpp_auth_codes", READ_INSERT_DELETE),
    ("permission_tuples", READ_WRITE),
    // Roster, blocking, profile and uploads.
    ("roster_items", READ_WRITE),
    ("roster_versions", READ_WRITE),
    ("blocking_list", READ_INSERT_DELETE),
    ("vcard_storage", READ_WRITE),
    ("private_xml_storage", READ_INSERT_UPDATE),
    ("user_avatar_source", READ_INSERT_UPDATE),
    ("user_avatar_fetch_state", READ_INSERT_UPDATE),
    ("upload_slots", READ_INSERT_UPDATE),
    ("link_preview_media_refs", READ_INSERT_UPDATE),
    // Rooms, invitations and extensions.
    ("channels", READ_WRITE),
    ("muc_pending_invites", READ_WRITE),
    ("muc_invite_claims", READ_INSERT),
    ("group_dm_archive_boundaries", READ_WRITE),
    ("xmpp_occupancy_authority", READ_INSERT_UPDATE),
    ("extension_grants", READ_INSERT_UPDATE),
    ("extension_bot_rooms", READ_INSERT_DELETE),
    ("provider_webhook_deliveries", READ_INSERT_UPDATE),
    ("webhook_deliveries", READ_WRITE),
    // Ingress substrate. The owner-rejection ledger is append-only for the
    // runtime, and its grant verifier refuses UPDATE or DELETE.
    ("ingress_messages", READ_WRITE),
    ("ingress_origin_aliases", READ_WRITE),
    ("ingress_sm_refs", READ_INSERT_DELETE),
    ("ingress_sm_streams", READ_WRITE),
    ("ingress_deliveries", READ_INSERT_DELETE),
    ("ingress_effect_intents", READ_INSERT_UPDATE),
    ("ingress_effect_receipts", READ_INSERT),
    ("ingress_effect_descendants", READ_INSERT_UPDATE),
    ("ingress_carbon_receipts", READ_INSERT),
    ("ingress_delivery_receipts", READ_INSERT),
    ("ingress_archive_dispatch", READ_INSERT),
    ("ingress_send_attempts", READ_WRITE),
    ("ingress_owner_rejections", READ_INSERT),
    // Inbox, stream management and pending delivery.
    ("inbox_entries", READ_INSERT_UPDATE),
    ("groupchat_notification_recovery", READ_WRITE),
    ("sm_sessions", READ_WRITE),
    ("sm_unacked", READ_INSERT_DELETE),
    ("sm_acknowledged_windows", READ_INSERT_DELETE),
    ("sm_ingress_appends", READ_WRITE),
    ("pending_delivery", READ_WRITE),
    ("pending_delivery_sm_promotions", READ_INSERT_DELETE),
    // PubSub and its projections.
    ("pubsub_nodes", READ_WRITE),
    ("pubsub_items", READ_WRITE),
    ("pubsub_subscriptions", READ_INSERT_DELETE),
    ("pubsub_affiliations", READ_WRITE),
    ("pubsub_owner_configured", READ_INSERT),
    ("pubsub_push_publications", READ_INSERT_UPDATE),
    ("dnd_projection", READ_WRITE),
    ("dnd_projection_source_version", READ_INSERT_UPDATE),
    ("notification_settings_projection", READ_WRITE),
    (
        "notification_settings_projection_source_version",
        READ_INSERT_UPDATE,
    ),
    // Spaces.
    ("spaces_metadata", READ_WRITE),
    ("channel_space_links", READ_WRITE),
    // Push and notifications.
    ("push_registrations", READ_WRITE),
    ("notification_activity", READ_INSERT_UPDATE),
    ("notification_candidates", READ_WRITE),
    ("notification_outbox", READ_WRITE),
    ("notification_outbox_lineage", READ_WRITE),
    ("push_owner_locks", READ_INSERT_UPDATE),
    ("push_nodes", READ_WRITE),
    ("push_node_locks", READ_INSERT_UPDATE),
    ("push_devices", READ_WRITE),
    ("push_delivery_attempts", READ_INSERT_DELETE),
    ("push_publish_jobs", READ_WRITE),
    ("push_publication_orders", READ_INSERT_UPDATE),
    ("push_service_vapid_keys", READ),
    // Outboxes. These two carry a clustering prefix but serve without it.
    ("call_teardown_outbox", READ_WRITE),
    ("clustering_muc_room_effects", READ_WRITE),
    ("clustering_muc_destroy_outbox", READ_WRITE),
    // MAM: append-only rows, with the column updates granted below.
    ("mam_messages", READ_INSERT),
    ("mam_archive_sequences", READ_INSERT),
    // Tables no runtime statement touches: the pre-MAM message tables and
    // the receipt operation journal.
    ("messages", NONE),
    ("attachments", NONE),
    ("reactions", NONE),
    ("p21_receipt_operation_results", NONE),
    // Clustering and fenced stream management relations that an upgraded
    // database can hold. A runtime that serves without clustering uses none;
    // [`CLUSTERED_TABLE_GRANTS`] replaces the clustering rows when it does.
    // No serving path selects the revisioned stream management store or the
    // inter-server-resume tables, so those stay without privileges.
    ("clustering_claims", NONE),
    ("clustering_nodes", NONE),
    ("clustering_keypair_slots", NONE),
    ("clustering_peer_allowlist", NONE),
    ("clustering_steal_intents", NONE),
    ("clustering_orphan_reaper_cursors", NONE),
    ("clustering_isr_tokens", NONE),
    ("clustering_isr_revocation_fences", NONE),
    ("clustering_isr_sweep_state", NONE),
    ("clustering_muc_rooms", NONE),
    ("clustering_muc_room_affiliations", NONE),
    ("clustering_muc_room_lifecycles", NONE),
    ("clustering_muc_admin_receipts", NONE),
    ("sm_revision_authority", NONE),
    ("sm_revision_generations", NONE),
    ("sm_revision_operations", NONE),
];

/// The rows that replace their [`TABLE_GRANTS`] entry when the plan has a
/// clustering section, plus the keypair-pool enrollment tables, which only a
/// clustered database holds.
///
/// Each entry is the exact set the store's attachment verifies. Claims,
/// steal intents and rooms are inserted, updated, locked and deleted. Node
/// and reaper-cursor rows and keypair leases are upserted and never deleted
/// (the lease guard forbids it). Affiliations are upserted and deleted. The
/// allowlist and the enrollment are owner-controlled and read-only. Admin
/// receipts are inserted and deleted, never updated. Lifecycles are
/// inserted, updated and row-locked; a destroyed room's lifecycle is
/// tombstoned, not deleted.
const CLUSTERED_TABLE_GRANTS: &[(&str, &str)] = &[
    ("clustering_claims", READ_WRITE),
    ("clustering_nodes", READ_INSERT_UPDATE),
    ("clustering_steal_intents", READ_WRITE),
    ("clustering_orphan_reaper_cursors", READ_INSERT_UPDATE),
    ("clustering_peer_allowlist", READ),
    (KEYPOOL_TABLES[0], READ),
    (KEYPOOL_TABLES[1], READ),
    ("clustering_keypair_slots", READ_INSERT_UPDATE),
    ("clustering_muc_rooms", READ_WRITE),
    ("clustering_muc_room_affiliations", READ_WRITE),
    ("clustering_muc_room_lifecycles", READ_INSERT_UPDATE),
    ("clustering_muc_admin_receipts", READ_INSERT_DELETE),
];

/// The keypair-pool enrollment tables. They exist only on a database
/// enrolled for clustering, which a plan without a clustering section
/// refuses to prepare.
const KEYPOOL_TABLES: [&str; 2] = ["clustering_keypool", "clustering_keypool_slots"];

/// Claim epochs come from the sequence, and the claim attachment reads its
/// position to check it against retained claims.
const CLUSTERED_SEQUENCE_GRANTS: &[(&str, &str)] =
    &[("clustering_claim_epoch_seq", "SELECT, USAGE")];

/// The matrix in force for one plan: the clustered rows take precedence when
/// the plan has a clustering section.
#[derive(Clone, Copy)]
struct GrantMatrix {
    clustered: bool,
}

impl GrantMatrix {
    fn table(self, name: &str) -> Option<&'static str> {
        let clustered: &[(&str, &str)] = if self.clustered {
            CLUSTERED_TABLE_GRANTS
        } else {
            &[]
        };
        clustered
            .iter()
            .chain(TABLE_GRANTS)
            .find(|(table, _)| *table == name)
            .map(|(_, privileges)| *privileges)
    }

    fn sequence(self, name: &str) -> Option<&'static str> {
        let clustered: &[(&str, &str)] = if self.clustered {
            CLUSTERED_SEQUENCE_GRANTS
        } else {
            &[]
        };
        clustered
            .iter()
            .chain(SEQUENCE_GRANTS)
            .find(|(sequence, _)| *sequence == name)
            .map(|(_, privileges)| *privileges)
    }

    /// Whether the matrix grants `privilege` on `relation`. A column
    /// privilege is spelled `UPDATE(column)`.
    fn classifies(self, relation: &str, privilege: &str) -> bool {
        if OWNER_GRANTED_TABLES.contains(&relation) {
            return true;
        }
        let granted = self
            .table(relation)
            .or_else(|| self.sequence(relation))
            .is_some_and(|privileges| {
                privileges
                    .split(", ")
                    .any(|candidate| candidate == privilege)
            });
        granted
            || COLUMN_UPDATE_GRANTS.iter().any(|(name, columns)| {
                *name == relation
                    && columns
                        .iter()
                        .any(|column| privilege == format!("UPDATE({column})"))
            })
    }
}

/// Column-level UPDATE, each verified exactly by an attachment.
///
/// The registered epoch guard and the ingress substrate read the epoch row
/// `FOR SHARE`, which PostgreSQL permits only with an UPDATE privilege. The
/// grant is on `id` alone: `CHECK (id = 1)` and the guard trigger keep it
/// from changing the epoch, and the pinned preflight requires exactly this
/// item. MAM takes the tombstone columns and the per-archive counter.
const COLUMN_UPDATE_GRANTS: &[(&str, &[&str])] = &[
    ("ingress_protocol_epoch", &["id"]),
    (
        "mam_messages",
        &[
            "body",
            "stanza_xml",
            "thread_id",
            "parent_thread_id",
            "reply_to_id",
            "reply_to_jid",
            "rich_payload",
        ],
    ),
    ("mam_archive_sequences", &["next_seq"]),
];

/// Tables whose runtime privileges their own owner preparation grants and
/// verifies.
const OWNER_GRANTED_TABLES: &[&str] = &[
    "extension_room_observation_owner_state",
    "extension_room_observers",
    "extension_room_sources",
    "extension_room_source_revisions",
    "extension_room_observation_work",
    "extension_room_observation_receipts",
    "extension_room_publications",
];

/// The runtime's sequence privileges. Serial keys the runtime inserts take
/// USAGE; the PubSub item sequence is read for ordering and never advanced
/// directly. Every sequence in the schema is listed.
const SEQUENCE_GRANTS: &[(&str, &str)] = &[
    ("blocking_list_id_seq", "USAGE"),
    ("native_users_id_seq", "USAGE"),
    ("roster_items_id_seq", "USAGE"),
    (
        "pending_delivery_notification_recovery_ordinal_seq",
        "USAGE",
    ),
    ("pubsub_items_seq_seq", "SELECT"),
    ("reactions_id_seq", NONE),
    ("clustering_claim_epoch_seq", NONE),
];

/// Prepare every schema the restricted runtime attaches to and grant the
/// runtime role its privileges, in dependency order, on `owner`.
///
/// `owner` is an unattested PostgreSQL main-lane handle whose session user is
/// `plan.migrator` and whose current role is `plan.schema_owner`, with the
/// target schema first on its `search_path`.
pub async fn prepare_restricted_runtime_owner(
    owner: &Database,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<RestrictedRuntimeOwnerEvidence, RestrictedRuntimeOwnerError> {
    if owner.driver() != DatabaseDriver::Postgres
        || !owner.is_main_lane()
        || owner.uses_attested_postgres_pool()
    {
        return Err(RestrictedRuntimeOwnerError::OwnerHandleRequired);
    }

    refuse_public_schema_privileges(owner).await?;
    #[cfg(feature = "clustering")]
    if let Some(ClusteringOwnerPlan {
        exclusive_database:
            ExclusiveDatabase::Required {
                owner_application_name,
            },
        ..
    }) = &plan.clustering
    {
        require_exclusive_database(owner, owner_application_name).await?;
    }

    bootstrap_global_owner(owner, &plan.lineage, false)
        .await
        .map_err(RestrictedRuntimeOwnerError::Global)?;
    let expected = lineage::verify(
        owner,
        &LineageConfig {
            deployment_uuid: plan.lineage.deployment_uuid,
            action: None,
        },
    )
    .await
    .map_err(RestrictedRuntimeOwnerError::Lineage)?;
    let identity = expected
        .postgres_identity
        .clone()
        .ok_or(RestrictedRuntimeOwnerError::MissingPostgresIdentity)?;

    // The attestor's definer reads `_lineage` through the schema, and the
    // room observation preparation checks both grants before it runs.
    grant_attestation_access(owner, &identity, plan).await?;
    // Refuse an unrestricted runtime role before preparing stores for it,
    // and again after the grants, so the phase never succeeds on a database
    // the runtime's attachments would refuse.
    verify_runtime_role(owner, &identity, plan).await?;

    // A keypair-pool enrollment created with owner-privileged guards would
    // fail the room observation preparation below, which refuses such
    // triggers on relations the runtime writes.
    #[cfg(feature = "clustering")]
    if plan.clustering.is_some() {
        PostgresKeypairSlotLease::convert_definer_guards_owner(
            owner,
            &identity,
            plan.runtime.as_str(),
        )
        .await
        .map_err(RestrictedRuntimeOwnerError::Keypool)?;
    }

    // The registered migrations create the observation tables; the two
    // scheduling indexes come from the store's own initializer.
    initialize_room_observations(owner)
        .await
        .map_err(RestrictedRuntimeOwnerError::RoomObservationTables)?;
    prepare_dark_room_observation_schema(
        owner,
        &expected,
        &plan.migrator,
        &plan.attestation_definer,
        &plan.runtime,
    )
    .await
    .map_err(RestrictedRuntimeOwnerError::RoomObservation)?;

    bootstrap_pending_delivery_owner_schema(owner)
        .await
        .map_err(RestrictedRuntimeOwnerError::PendingDelivery)?;
    bootstrap_inbox_owner_schema(owner)
        .await
        .map_err(RestrictedRuntimeOwnerError::Inbox)?;
    bootstrap_portable_sm_owner_schema(owner)
        .await
        .map_err(RestrictedRuntimeOwnerError::StreamManagement)?;
    DatabasePubSubStorage::initialize_owner(owner.clone(), &identity)
        .await
        .map_err(RestrictedRuntimeOwnerError::PubSub)?;
    prepare_space_stores_owner(owner.clone(), &identity)
        .await
        .map_err(RestrictedRuntimeOwnerError::SpaceStores)?;
    DatabasePushRegistrationStore::initialize_owner(owner.clone())
        .await
        .map_err(RestrictedRuntimeOwnerError::PushRegistrations)?;
    NotificationActivityStore::initialize_owner(owner.clone())
        .await
        .map_err(RestrictedRuntimeOwnerError::NotificationActivity)?;
    CallTeardownOutboxStore::initialize_owner(owner, &identity)
        .await
        .map_err(RestrictedRuntimeOwnerError::CallTeardown)?;
    let _core_push = prepare_core_push_outbox_vapid_owner(owner, &identity, &plan.vapid_root_key)
        .await
        .map_err(RestrictedRuntimeOwnerError::PushNotification)?;
    RoomEffectOutboxStore::initialize_owner(owner, &identity)
        .await
        .map_err(RestrictedRuntimeOwnerError::RoomEffect)?;
    MucDestroyCompletionOutboxStore::try_new(owner.clone())
        .await
        .map_err(RestrictedRuntimeOwnerError::MucDestroy)?;
    #[cfg(feature = "clustering")]
    let keypool = match &plan.clustering {
        Some(clustering) => Some(prepare_clustering(owner, &identity, plan, clustering).await?),
        None => None,
    };
    prepare_mam(owner, &identity, plan).await?;
    webhook_delivery::initialize_owner_schema(owner)
        .await
        .map_err(RestrictedRuntimeOwnerError::WebhookDeliveries)?;

    grant_runtime(owner, &identity, plan).await?;
    verify_runtime_role(owner, &identity, plan).await?;

    // Reported last so the grants are in place when the delay elapses: the
    // rerun then only activates the generation.
    #[cfg(feature = "clustering")]
    if keypool == Some(KeypoolPreparation::Pending) {
        return Err(RestrictedRuntimeOwnerError::KeypoolActivationPending);
    }

    Ok(RestrictedRuntimeOwnerEvidence { lineage: expected })
}

fn plan_is_clustered(plan: &RestrictedRuntimeOwnerPlan) -> bool {
    #[cfg(feature = "clustering")]
    {
        plan.clustering.is_some()
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = plan;
        false
    }
}

/// Prepare the stores only a clustered runtime attaches to. Retained claims,
/// nodes, steal intents, reaper cursors and durable rooms are adopted in
/// place; the allowlist is reconciled to the plan; the keypair pool is
/// enrolled, adopted or, under an explicit plan, rebuilt from an unenrolled
/// lease table.
#[cfg(feature = "clustering")]
async fn prepare_clustering(
    owner: &Database,
    identity: &PgIdentity,
    plan: &RestrictedRuntimeOwnerPlan,
    clustering: &ClusteringOwnerPlan,
) -> Result<KeypoolPreparation, RestrictedRuntimeOwnerError> {
    crate::clustering::claims::prepare_claims_owner(owner, identity)
        .await
        .map_err(RestrictedRuntimeOwnerError::Claims)?;
    // The enrollment is checked first: a plan that names another generation
    // or PeerId order is refused before the allowlist is reconciled to it.
    let keypool = PostgresKeypairSlotLease::prepare_keypool_owner(
        owner,
        identity,
        &KeypoolEnrollment {
            generation: clustering.keypool_generation,
            peer_ids: &clustering.peer_ids,
            runtime_role: plan.runtime.as_str(),
            activation_delay: clustering.activation_delay,
            legacy: clustering.legacy_keypair_slots,
            lease_ttl: clustering.lease_ttl,
            exclusive_owner_tag: match &clustering.exclusive_database {
                ExclusiveDatabase::Required {
                    owner_application_name,
                } => Some(owner_application_name),
                ExclusiveDatabase::NotChecked => None,
            },
        },
    )
    .await
    .map_err(RestrictedRuntimeOwnerError::Keypool)?;
    crate::clustering::allowlist::prepare_peer_allowlist_owner(
        owner,
        identity,
        &clustering.peer_ids,
        clustering.allowlist,
    )
    .await
    .map_err(RestrictedRuntimeOwnerError::PeerAllowlist)?;
    crate::muc_durable::PostgresMucRoomStore::prepare_owner(owner, identity)
        .await
        .map_err(RestrictedRuntimeOwnerError::MucRooms)?;
    Ok(keypool)
}

/// Refuse while any other client session is connected to the database or a
/// prepared transaction is pending in it: either could hold or later commit
/// state the phase is about to adopt or retire. The owner handle's own
/// sessions are recognised by the `application_name` generated for this run.
#[cfg(feature = "clustering")]
async fn require_exclusive_database(
    owner: &Database,
    owner_application_name: &str,
) -> Result<(), RestrictedRuntimeOwnerError> {
    let inspect = async {
        let mut tx = owner.begin().await?;
        let mut tagged = tx
            .query(
                "SELECT pg_catalog.current_setting('application_name') = ?",
                crate::db_params![owner_application_name],
            )
            .await?;
        let tagged = match tagged.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        let counts =
            crate::clustering::lease::foreign_sessions(&mut tx, owner_application_name).await?;
        tx.rollback().await?;
        Ok::<_, DatabaseError>((tagged, counts))
    };
    match inspect
        .await
        .map_err(RestrictedRuntimeOwnerError::ExclusiveDatabaseInspection)?
    {
        (false, _) => Err(RestrictedRuntimeOwnerError::OwnerSessionUntagged),
        (true, (0, 0)) => Ok(()),
        (true, (sessions, prepared_transactions)) => {
            Err(RestrictedRuntimeOwnerError::DatabaseNotExclusive {
                sessions,
                prepared_transactions,
            })
        }
    }
}

/// The MAM initializer autocommits each statement under a session lock, so
/// it runs on a connection detached from the owner pool and dropped on error.
async fn prepare_mam(
    owner: &Database,
    identity: &PgIdentity,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<(), RestrictedRuntimeOwnerError> {
    let session = owner
        .detached_postgres_startup_connection()
        .await
        .map_err(RestrictedRuntimeOwnerError::MamSession)?;
    let session = SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(
        session,
        &identity.schema.name.0,
        plan.schema_owner.as_str(),
    )
    .await
    .map_err(RestrictedRuntimeOwnerError::Mam)?;
    drop(session);
    Ok(())
}

async fn grant_attestation_access(
    owner: &Database,
    identity: &PgIdentity,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<(), RestrictedRuntimeOwnerError> {
    let schema = quote_identifier(&identity.schema.name.0);
    let runtime = quote_identifier(plan.runtime.as_str());
    let definer = quote_identifier(plan.attestation_definer.as_str());
    for (object, statement) in [
        (
            "schema usage",
            format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}, {definer}"),
        ),
        (
            "_lineage",
            format!("GRANT SELECT ON TABLE {schema}.\"_lineage\" TO {runtime}, {definer}"),
        ),
    ] {
        execute_grant(owner, object, &statement).await?;
    }
    Ok(())
}

async fn grant_runtime(
    owner: &Database,
    identity: &PgIdentity,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<(), RestrictedRuntimeOwnerError> {
    let schema_name = &identity.schema.name.0;
    let schema = quote_identifier(schema_name);
    let runtime = quote_identifier(plan.runtime.as_str());

    // Classify the whole schema before the first grant: a relation this
    // matrix does not name fails the phase rather than receive a default.
    let matrix = GrantMatrix {
        clustered: plan_is_clustered(plan),
    };
    let tables = relation_names(owner, schema_name, &["r", "p", "v", "m", "f"]).await?;
    let sequences = relation_names(owner, schema_name, &["S"]).await?;
    for table in &tables {
        if !matrix.clustered && KEYPOOL_TABLES.contains(&table.as_str()) {
            return Err(
                RestrictedRuntimeOwnerError::KeypoolEnrolledWithoutClusteringPlan {
                    relation: table.clone(),
                },
            );
        }
        if !OWNER_GRANTED_TABLES.contains(&table.as_str()) && matrix.table(table).is_none() {
            return Err(RestrictedRuntimeOwnerError::UnclassifiedRelation {
                relation: table.clone(),
            });
        }
    }
    for sequence in &sequences {
        if matrix.sequence(sequence).is_none() {
            return Err(RestrictedRuntimeOwnerError::UnclassifiedRelation {
                relation: sequence.clone(),
            });
        }
    }
    // A privilege the runtime already holds beyond this matrix is reported,
    // not revoked: it was granted outside this phase and needs a decision.
    for (relation, privilege) in runtime_privileges(owner, schema_name, plan).await? {
        if !matrix.classifies(&relation, &privilege) {
            return Err(RestrictedRuntimeOwnerError::ExcessRuntimePrivilege {
                relation,
                privilege,
            });
        }
    }

    for table in &tables {
        let Some(privileges) = matrix.table(table) else {
            continue;
        };
        if privileges.is_empty() {
            continue;
        }
        let relation = quote_identifier(table);
        execute_grant(
            owner,
            table,
            &format!("GRANT {privileges} ON TABLE {schema}.{relation} TO {runtime}"),
        )
        .await?;
    }
    for (table, columns) in COLUMN_UPDATE_GRANTS {
        if !tables.iter().any(|name| name == table) {
            continue;
        }
        let columns = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        execute_grant(
            owner,
            table,
            &format!("GRANT UPDATE ({columns}) ON TABLE {schema}.\"{table}\" TO {runtime}"),
        )
        .await?;
    }
    for sequence in &sequences {
        let Some(privilege) = matrix.sequence(sequence) else {
            continue;
        };
        if privilege.is_empty() {
            continue;
        }
        let relation = quote_identifier(sequence);
        execute_grant(
            owner,
            sequence,
            &format!("GRANT {privilege} ON SEQUENCE {schema}.{relation} TO {runtime}"),
        )
        .await?;
    }
    // PubSub attachment takes its attach lock through this owner-created
    // function; only the runtime role may call it.
    execute_grant(
        owner,
        "pubsub_runtime_attach_lock()",
        &format!("GRANT EXECUTE ON FUNCTION {schema}.pubsub_runtime_attach_lock() TO {runtime}"),
    )
    .await
}

/// Every effective privilege the runtime role holds on the schema's tables
/// and sequences, including those reaching it through PUBLIC. A privilege
/// held with the grant option is listed again as `... WITH GRANT OPTION`,
/// which the matrix never grants: the runtime must not be able to re-grant.
async fn runtime_privileges(
    owner: &Database,
    schema: &str,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<Vec<(String, String)>, RestrictedRuntimeOwnerError> {
    let inventory = async {
        let connection = owner.guard().await?;
        let mut rows = connection
            .query(
                "SELECT c.relname::text, p.privilege \
                   FROM pg_catalog.pg_class c \
                   JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                   JOIN pg_catalog.pg_roles r ON r.rolname = ? \
                   CROSS JOIN pg_catalog.unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', \
                     'TRUNCATE', 'REFERENCES', 'TRIGGER', 'MAINTAIN']) AS p(privilege) \
                  WHERE n.nspname = ? AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                    AND CASE WHEN p.privilege = 'MAINTAIN' \
                          AND pg_catalog.current_setting('server_version_num')::integer < 170000 \
                        THEN false \
                        ELSE pg_catalog.has_table_privilege(r.oid, c.oid, p.privilege) END \
                 UNION ALL \
                 SELECT c.relname::text, p.privilege || ' WITH GRANT OPTION' \
                   FROM pg_catalog.pg_class c \
                   JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                   JOIN pg_catalog.pg_roles r ON r.rolname = ? \
                   CROSS JOIN pg_catalog.unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', \
                     'TRUNCATE', 'REFERENCES', 'TRIGGER', 'MAINTAIN']) AS p(privilege) \
                  WHERE n.nspname = ? AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                    AND CASE WHEN p.privilege = 'MAINTAIN' \
                          AND pg_catalog.current_setting('server_version_num')::integer < 170000 \
                        THEN false \
                        ELSE pg_catalog.has_table_privilege( \
                          r.oid, c.oid, p.privilege || ' WITH GRANT OPTION') \
                          OR (p.privilege IN ('SELECT', 'INSERT', 'UPDATE', 'REFERENCES') \
                            AND pg_catalog.has_any_column_privilege( \
                              r.oid, c.oid, p.privilege || ' WITH GRANT OPTION')) END \
                 UNION ALL \
                 SELECT c.relname::text, p.privilege || ' WITH GRANT OPTION' \
                   FROM pg_catalog.pg_class c \
                   JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                   JOIN pg_catalog.pg_roles r ON r.rolname = ? \
                   CROSS JOIN pg_catalog.unnest(ARRAY['USAGE', 'SELECT', 'UPDATE']) \
                     AS p(privilege) \
                  WHERE n.nspname = ? AND c.relkind = 'S' \
                    AND pg_catalog.has_sequence_privilege( \
                      r.oid, c.oid, p.privilege || ' WITH GRANT OPTION') \
                 UNION ALL \
                 SELECT c.relname::text, p.privilege || '(' || a.attname::text || ')' \
                   FROM pg_catalog.pg_class c \
                   JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                   JOIN pg_catalog.pg_roles r ON r.rolname = ? \
                   JOIN pg_catalog.pg_attribute a \
                     ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
                   CROSS JOIN pg_catalog.unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', \
                     'REFERENCES']) AS p(privilege) \
                  WHERE n.nspname = ? AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                    AND NOT pg_catalog.has_table_privilege(r.oid, c.oid, p.privilege) \
                    AND pg_catalog.has_column_privilege(r.oid, c.oid, a.attnum, p.privilege) \
                 UNION ALL \
                 SELECT c.relname::text, p.privilege \
                   FROM pg_catalog.pg_class c \
                   JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                   JOIN pg_catalog.pg_roles r ON r.rolname = ? \
                   CROSS JOIN pg_catalog.unnest(ARRAY['USAGE', 'SELECT', 'UPDATE']) \
                     AS p(privilege) \
                  WHERE n.nspname = ? AND c.relkind = 'S' \
                    AND pg_catalog.has_sequence_privilege(r.oid, c.oid, p.privilege)",
                crate::db_params![
                    plan.runtime.as_str(),
                    schema,
                    plan.runtime.as_str(),
                    schema,
                    plan.runtime.as_str(),
                    schema,
                    plan.runtime.as_str(),
                    schema,
                    plan.runtime.as_str(),
                    schema
                ],
            )
            .await?;
        let mut privileges = Vec::new();
        while let Some(row) = rows.next().await? {
            privileges.push((row.get::<String>(0)?, row.get::<String>(1)?));
        }
        Ok::<_, DatabaseError>(privileges)
    };
    inventory
        .await
        .map_err(RestrictedRuntimeOwnerError::GrantInventory)
}

/// Check the runtime role the way the store attachments will: a login with
/// no elevated attribute, no role membership in either direction, no CREATE
/// or TEMP on the database, no CREATE on the schema, no function it owns
/// there, and no default privilege that would hand it future objects. The
/// phase reports success only for a database the runtime will accept.
async fn verify_runtime_role(
    owner: &Database,
    identity: &PgIdentity,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<(), RestrictedRuntimeOwnerError> {
    const COMPONENTS: [&str; 7] = [
        "login attributes",
        "ownership",
        "role membership",
        "database privileges",
        "schema privileges",
        "owned functions",
        "default privileges",
    ];
    let inspect = async {
        let connection = owner.guard().await?;
        let mut rows = connection
            .query(
                "SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole \
                     AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls, \
                   r.oid <> d.datdba AND r.oid <> n.nspowner \
                     AND NOT pg_catalog.pg_has_role(r.oid, n.nspowner, 'SET'), \
                   NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m \
                     WHERE m.member = r.oid OR m.roleid = r.oid), \
                   NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'CREATE') \
                     AND NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'TEMP'), \
                   pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE') \
                     AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE'), \
                   NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p \
                     WHERE p.pronamespace = n.oid AND p.proowner = r.oid), \
                   NOT EXISTS (SELECT 1 FROM pg_catalog.pg_default_acl da \
                     CROSS JOIN LATERAL pg_catalog.aclexplode(da.defaclacl) acl \
                     WHERE (da.defaclnamespace = n.oid OR da.defaclnamespace = 0) \
                       AND (acl.grantee = r.oid OR acl.grantee = 0 \
                         OR da.defaclrole = r.oid)) \
                 FROM pg_catalog.pg_roles r \
                 JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
                 JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database() \
                 WHERE r.rolname = ?",
                crate::db_params![identity.schema.name.0.as_str(), plan.runtime.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok::<_, DatabaseError>(Some(COMPONENTS[0]));
        };
        for (index, component) in COMPONENTS.into_iter().enumerate() {
            if !row.get::<bool>(index)? {
                return Ok(Some(component));
            }
        }
        Ok(None)
    };
    match inspect
        .await
        .map_err(RestrictedRuntimeOwnerError::GrantInventory)?
    {
        None => Ok(()),
        Some(component) => Err(RestrictedRuntimeOwnerError::UnsafeRuntimeRole { component }),
    }
}

/// PUBLIC privileges on the target schema reach every role, and each store's
/// attachment refuses them. The default `public` schema carries them, so a
/// deployment must use a dedicated schema; refuse before any DDL runs.
async fn refuse_public_schema_privileges(
    owner: &Database,
) -> Result<(), RestrictedRuntimeOwnerError> {
    let inspect = async {
        let connection = owner.guard().await?;
        let mut rows = connection
            .query(
                "SELECT n.nspname::text, EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                   COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) acl \
                   WHERE acl.grantee = 0) \
                 FROM pg_catalog.pg_namespace n \
                 WHERE n.nspname = pg_catalog.current_schema()",
                (),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok::<_, DatabaseError>(Some((row.get::<String>(0)?, row.get::<bool>(1)?))),
            None => Ok(None),
        }
    };
    match inspect
        .await
        .map_err(RestrictedRuntimeOwnerError::GrantInventory)?
    {
        Some((_, false)) => Ok(()),
        Some((schema, true)) => Err(RestrictedRuntimeOwnerError::PublicSchemaPrivilege { schema }),
        None => Err(RestrictedRuntimeOwnerError::OwnerHandleRequired),
    }
}

async fn relation_names(
    owner: &Database,
    schema: &str,
    kinds: &[&str],
) -> Result<Vec<String>, RestrictedRuntimeOwnerError> {
    let inventory = async {
        let connection = owner.guard().await?;
        let mut names = Vec::new();
        for kind in kinds {
            let mut rows = connection
                .query(
                    "SELECT c.relname::text FROM pg_catalog.pg_class c \
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = ? AND c.relkind::text = ? AND NOT c.relispartition \
                     ORDER BY c.relname",
                    crate::db_params![schema, *kind],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                names.push(row.get::<String>(0)?);
            }
        }
        Ok::<_, DatabaseError>(names)
    };
    inventory
        .await
        .map_err(RestrictedRuntimeOwnerError::GrantInventory)
}

async fn execute_grant(
    owner: &Database,
    object: &str,
    statement: &str,
) -> Result<(), RestrictedRuntimeOwnerError> {
    let grant = async {
        owner.guard().await?.execute(statement, ()).await?;
        Ok::<_, DatabaseError>(())
    };
    grant
        .await
        .map_err(|source| RestrictedRuntimeOwnerError::Grant {
            object: object.to_owned(),
            source,
        })
}
