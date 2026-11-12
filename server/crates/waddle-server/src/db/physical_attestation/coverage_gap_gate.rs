//! Test-only, fail-closed inventory of known gaps in the complete profile.
//!
//! This checks a caller-described configuration shape, not a live connection,
//! independently provisioned pins, a complete catalog, or serving authority.
//! Its known-gap list is versioned but is not an exhaustive object inventory.
//! There is deliberately no success variant: removing a listed gap cannot
//! make this prerequisite authorize startup.

use crate::config::{ServerConfig, ServerMode};
use crate::db::DatabaseDriver;
use crate::server::{ResolvedXmppDatabaseUrl, XmppConfig};

/// The seven child-store locations whose provenance is relevant to the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageTargetStore {
    Mam,
    Inbox,
    PubSub,
    SpacesMetadata,
    ChannelSpaceLinks,
    StreamManagement,
    PendingDelivery,
}

/// A caller-described target. None of these values is verified against a live
/// backend here. In particular the two owner OIDs must come from an independent
/// handoff when this is used outside a fixture; equality is unsupported for the
/// joint target even though the narrower pinned-pool contract permits it.
pub struct CoverageTarget<'a> {
    pub driver: DatabaseDriver,
    pub server_version_num: u32,
    pub global_database_url: &'a str,
    pub database_owner_role_oid: u32,
    pub schema_owner_role_oid: u32,
    pub server: &'a ServerConfig,
    pub xmpp: &'a XmppConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageTargetError {
    Postgres17Required,
    ClusteringFeatureRequired,
    ClusteringMustBeDisabled,
    HomeServerRequired,
    XmppAndNativeAuthRequired,
    SpiceDbRequired,
    ExternalAuthProvidersUnsupported,
    ExtensionsUnsupported,
    PushProvidersUnsupported,
    EmptyGlobalUrl,
    ChildMustInheritGlobal(CoverageTargetStore),
    ChildDiffersFromGlobal(CoverageTargetStore),
    OwnerPinsMustBeNonzeroAndDistinct,
}

/// Stable identifiers for known gaps, not a claim that other gaps do not exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CoverageGapId {
    JointRoleGraph,
    GlobalMigrations,
    IdentityAndSessions,
    RosterBlockingProfileUploads,
    PermissionBackend,
    Inbox,
    StreamManagement,
    DurableStreamManagement,
    AccountBindings,
    PendingDelivery,
    IngressEpoch,
    ReceiptJournal,
    PubSubProjections,
    MamAndClaimPair,
    SpacesMetadata,
    ChannelSpaceLinks,
    RoomCatalog,
    MucFixtureTarget,
    MucDestroyOutbox,
    RoomEffectOutbox,
    PushRegistrations,
    PushService,
    Vapid,
    NotificationOutbox,
    NotificationActivity,
    CallTeardown,
    ExtensionData,
}

/// A known unresolved area with representative objects and source files.
/// `examples` is intentionally not the complete object inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoverageGap {
    pub id: CoverageGapId,
    pub problem: &'static str,
    pub examples: &'static [&'static str],
    pub sources: &'static [&'static str],
}

macro_rules! gap {
    ($id:ident, $problem:literal, [$($example:literal),* $(,)?], [$($source:literal),+ $(,)?]) => {
        CoverageGap {
            id: CoverageGapId::$id,
            problem: $problem,
            examples: &[$($example),*],
            sources: &[$($source),+],
        }
    };
}

/// Known gaps at this version. Entries are evidence-backed work items, not a
/// manifest of every enabled object or a list of checked privileges.
pub const COVERAGE_GAPS_V1: &[CoverageGap] = &[
    gap!(JointRoleGraph, "The per-store role and ACL checks are not one reviewed joint graph; S and the database owner must be distinct for several stores.", [], [
        "server/crates/waddle-server/src/db/physical_attestation/pinned_hook.rs",
        "server/crates/waddle-server/src/push_service/runtime_attach.rs",
        "server/crates/waddle-server/src/notification_activity/runtime_attach.rs",
        "server/crates/waddle-server/src/notification_outbox/runtime_attach.rs",
        "server/crates/waddle-server/src/call_teardown_outbox/runtime_attach.rs",
    ]),
    gap!(GlobalMigrations, "The global ACL verifier covers lineage and ledger only, not a classified inventory of every migrated object.", ["_lineage", "_migrations"], [
        "server/crates/waddle-server/src/db/runtime_store_acl.rs",
        "server/crates/waddle-server/src/db/migrations/global.rs",
        "server/crates/waddle-server/src/db/migrations/waddle.rs",
    ]),
    gap!(IdentityAndSessions, "Global identity, authentication and session object ACLs are not mapped into the joint profile.", ["users", "auth_identities", "sessions", "native_users"], [
        "server/crates/waddle-server/src/db/migrations/global.rs",
        "server/crates/waddle-server/src/auth/identity.rs",
        "server/crates/waddle-server/src/auth/native.rs",
    ]),
    gap!(RosterBlockingProfileUploads, "Global roster, blocking, profile and upload object ACLs are not mapped into the joint profile.", ["roster_items", "roster_versions", "blocking_list", "vcard_storage", "upload_slots"], [
        "server/crates/waddle-server/src/db/migrations/global.rs",
        "server/crates/waddle-server/src/db/blocking.rs",
        "server/crates/waddle-server/src/db/roster.rs",
    ]),
    gap!(PermissionBackend, "The production permission backend is SpiceDB, while the fixture may use a local test backend; migrated permission objects still need classification.", ["permission_tuples"], [
        "server/crates/waddle-server/src/permissions/actor.rs",
        "server/crates/waddle-server/src/server/lifecycle.rs",
        "server/crates/waddle-server/src/db/migrations/global.rs",
    ]),
    gap!(Inbox, "The existing inbox attachment and fixture are not joined to a complete object/role/ACL contract.", ["inbox_entries", "groupchat_notification_recovery"], [
        "server/crates/waddle-server/src/inbox/runtime_attach.rs",
        "server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs",
    ]),
    gap!(StreamManagement, "Portable SM has separate owner and runtime checks; the all-family contract and writer conversion are incomplete.", ["sm_sessions", "sm_unacked", "sm_acknowledged_windows", "sm_ingress_appends"], [
        "server/crates/waddle-server/src/sm_persistence/runtime_attach.rs",
        "server/crates/waddle-server/src/db/runtime_store_acl.rs",
    ]),
    gap!(DurableStreamManagement, "The durable session authority has an owner initializer and a runtime attach profile; they are not joined to the all-family contract.", ["sm_durable_sessions", "sm_durable_operations"], [
        "server/crates/waddle-server/src/sm_authority/schema.rs",
        "server/crates/waddle-server/src/sm_persistence/runtime_attach.rs",
        "server/crates/waddle-server/src/db/runtime_store_acl.rs",
    ]),
    gap!(AccountBindings, "Account resource rows have an owner initializer and a runtime attach profile; they are not joined to the all-family contract.", ["account_resource_bindings", "account_resource_operations"], [
        "server/crates/waddle-server/src/account_bindings/schema.rs",
        "server/crates/waddle-server/src/account_bindings/runtime_attach.rs",
        "server/crates/waddle-server/src/db/runtime_store_acl.rs",
    ]),
    gap!(PendingDelivery, "Pending delivery is prepared separately; its complete joint object and ACL matrix is not established.", ["pending_delivery", "pending_delivery_sm_promotions"], [
        "server/crates/waddle-server/src/pending_delivery/database/runtime_attach.rs",
        "server/crates/waddle-server/src/db/runtime_store_acl.rs",
    ]),
    gap!(IngressEpoch, "The pinned diagnostic covers only a named subset; functions, triggers, indexes and other dependencies are not a complete profile.", ["ingress_messages", "ingress_protocol_epoch", "ingress_epoch_guard_manifest"], [
        "server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs",
        "server/crates/waddle-server/src/db/migrations/waddle.rs",
    ]),
    gap!(ReceiptJournal, "The V1030 journal and callable receipt functions have not been joined to a complete all-family graph.", ["p21_receipt_operation_results", "waddle_p21_record_effect_receipt"], [
        "server/crates/waddle-server/src/db/migrations/owner/p21_receipt_journal_postgres.sql",
        "server/crates/waddle-server/src/db/physical_attestation/pinned_receipt.rs",
    ]),
    gap!(PubSubProjections, "The named partial PubSub heap ACLs do not classify the complete helper, function and object graph.", ["pubsub_nodes", "pubsub_items", "dnd_projection", "notification_settings_projection"], [
        "server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs",
        "server/crates/waddle-server/src/pubsub/runtime_attach.rs",
    ]),
    gap!(MamAndClaimPair, "Attested MAM attaches without claims when clustering is disabled; a clustered attachment needs a live claim pair, which no restricted startup builds.", ["mam_messages", "mam_archive_sequences"], [
        "server/crates/waddle-server/src/server/http.rs",
        "server/crates/waddle-server/src/server/lifecycle.rs",
        "server/crates/waddle-xmpp/src/mam/storage/sqlx_store/schema.rs",
    ]),
    gap!(SpacesMetadata, "The combined fixture now prepares and attaches the paired store through test support with an identity-bound marker, but this is sequential evidence and the family is not jointly classified.", ["spaces_metadata"], [
        "server/crates/waddle-server/src/spaces_metadata/schema.rs",
        "server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs",
    ]),
    gap!(ChannelSpaceLinks, "The combined fixture now prepares and attaches the paired store through test support with an identity-bound marker, but this is sequential evidence and the family is not jointly classified.", ["channel_space_links"], [
        "server/crates/waddle-server/src/channel_space_links/schema.rs",
        "server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs",
    ]),
    gap!(RoomCatalog, "The non-clustered room and channel callers use migrated objects whose joint ACLs are not classified.", ["channels", "muc_pending_invites", "muc_invite_claims", "group_dm_archive_boundaries"], [
        "server/crates/waddle-server/src/db/migrations/waddle.rs",
        "server/crates/waddle-server/src/server/routes/websocket/muc_invites.rs",
        "server/crates/waddle-server/src/admin/channels.rs",
    ]),
    gap!(MucFixtureTarget, "The combined fixture prepares clustered MUC tables based on the build feature, while disabled runtime clustering does not select that store.", ["clustering_muc_rooms", "clustering_muc_room_affiliations", "clustering_muc_room_lifecycles"], [
        "server/crates/waddle-server/src/clustering/node.rs",
        "server/crates/waddle-server/src/server/lifecycle.rs",
        "server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs",
    ]),
    gap!(MucDestroyOutbox, "The MUC destroy outbox is initialized on the disabled-clustering path but is not part of the complete joint profile.", ["clustering_muc_destroy_outbox"], [
        "server/crates/waddle-server/src/server/lifecycle.rs",
        "server/crates/waddle-server/src/muc_destroy_completion_outbox.rs",
    ]),
    gap!(RoomEffectOutbox, "The current startup constructor rejects attested pools; the separately prepared room effect outbox is not a selected store.", ["clustering_muc_room_effects"], [
        "server/crates/waddle-server/src/server/http.rs",
        "server/crates/waddle-server/src/room_effect_outbox/store.rs",
    ]),
    gap!(PushRegistrations, "The combined fixture prepares the registration table and checks its table ACL, but current startup still invokes its owner-only constructor and the joint profile is absent.", ["push_registrations"], [
        "server/crates/waddle-server/src/server/http.rs",
        "server/crates/waddle-server/src/push_registrations.rs",
    ]),
    gap!(PushService, "The combined fixture owner-prepares and checks named Push Service tables, but current startup invokes an owner-only constructor and the function and dependency graph remains unclassified.", ["push_nodes", "push_devices", "push_publish_jobs", "push_service_schema_marker"], [
        "server/crates/waddle-server/src/server/http.rs",
        "server/crates/waddle-server/src/push_service/store.rs",
        "server/crates/waddle-server/src/push_service/runtime_attach.rs",
    ]),
    gap!(Vapid, "The combined fixture prepares the VAPID key and owner marker and checks table ACLs, but the key-handling role and dependency graph is not part of a joint profile.", ["push_service_vapid_keys", "push_service_vapid_schema_marker"], [
        "server/crates/waddle-server/src/push_service/vapid_storage.rs",
        "server/crates/waddle-server/src/push_service/runtime_attach.rs",
        "server/crates/waddle-server/src/server/push_notification_owner.rs",
    ]),
    gap!(NotificationOutbox, "The combined fixture prepares notification outbox and ancestry owner phases and checks named table ACLs, but their complete role and dependency graph is not jointly classified.", ["notification_candidates", "notification_outbox", "notification_outbox_lineage", "notification_outbox_owner_state"], [
        "server/crates/waddle-server/src/notification_outbox/schema.rs",
        "server/crates/waddle-server/src/server/push_notification_owner.rs",
    ]),
    gap!(NotificationActivity, "The combined fixture prepares activity and checks named table ACLs, but current startup invokes its owner-only constructor and the joint graph is absent.", ["notification_activity", "notification_activity_owner_state"], [
        "server/crates/waddle-server/src/server/http.rs",
        "server/crates/waddle-server/src/notification_activity.rs",
        "server/crates/waddle-server/src/notification_activity/schema.rs",
    ]),
    gap!(CallTeardown, "The combined fixture prepares call teardown and checks its table ACL, but owner preparation and runtime attachment have not been joined into a complete profile.", ["call_teardown_outbox"], [
        "server/crates/waddle-server/src/call_teardown_outbox/store.rs",
        "server/crates/waddle-server/src/call_teardown_outbox/runtime_attach.rs",
    ]),
    gap!(ExtensionData, "Disabling configured extension modules does not remove migrated grants and observation objects from the schema.", ["extension_grants", "extension_room_observers", "extension_room_observation_work"], [
        "server/crates/waddle-server/src/db/migrations/waddle.rs",
        "server/crates/waddle-server/src/server/extension_commands/mod.rs",
    ]),
];

/// The diagnostic cannot succeed: supported shape still has known gaps. There
/// is no `Complete` variant or conversion to a pool or serving capability.
#[derive(Debug, Eq, PartialEq)]
pub enum CoverageRefusal {
    Unsupported(CoverageTargetError),
    Incomplete(&'static [CoverageGap]),
}

/// Reject unsupported caller-described shapes, otherwise return every known V1
/// gap. This does not open a database, resolve URLs, or verify supplied OIDs.
pub fn inspect_coverage_gap_gate_v1(target: &CoverageTarget<'_>) -> CoverageRefusal {
    let unsupported = if target.driver != DatabaseDriver::Postgres
        || !(170_000..180_000).contains(&target.server_version_num)
    {
        Some(CoverageTargetError::Postgres17Required)
    } else if !cfg!(feature = "clustering") {
        Some(CoverageTargetError::ClusteringFeatureRequired)
    } else if target.server.clustering.enabled {
        Some(CoverageTargetError::ClusteringMustBeDisabled)
    } else if target.server.mode != ServerMode::HomeServer {
        Some(CoverageTargetError::HomeServerRequired)
    } else if !target.xmpp.enabled || !target.xmpp.native_auth_enabled {
        Some(CoverageTargetError::XmppAndNativeAuthRequired)
    } else if target.server.spicedb.is_none() {
        Some(CoverageTargetError::SpiceDbRequired)
    } else if !target.server.auth.providers.is_empty() {
        Some(CoverageTargetError::ExternalAuthProvidersUnsupported)
    } else if target.server.extensions.enabled || !target.server.extensions.modules.is_empty() {
        Some(CoverageTargetError::ExtensionsUnsupported)
    } else if target.server.apns.is_some() || target.server.fcm.is_some() {
        Some(CoverageTargetError::PushProvidersUnsupported)
    } else if target.global_database_url.trim().is_empty() {
        Some(CoverageTargetError::EmptyGlobalUrl)
    } else if target.database_owner_role_oid == 0
        || target.schema_owner_role_oid == 0
        || target.database_owner_role_oid == target.schema_owner_role_oid
    {
        Some(CoverageTargetError::OwnerPinsMustBeNonzeroAndDistinct)
    } else {
        None
    };
    if let Some(reason) = unsupported {
        return CoverageRefusal::Unsupported(reason);
    }
    for (store, location) in [
        (CoverageTargetStore::Mam, &target.xmpp.mam_database_url),
        (CoverageTargetStore::Inbox, &target.xmpp.inbox_database_url),
        (
            CoverageTargetStore::PubSub,
            &target.xmpp.pubsub_database_url,
        ),
        (
            CoverageTargetStore::SpacesMetadata,
            &target.xmpp.spaces_metadata_database_url,
        ),
        (
            CoverageTargetStore::ChannelSpaceLinks,
            &target.xmpp.channel_space_links_database_url,
        ),
        (
            CoverageTargetStore::StreamManagement,
            &target.xmpp.sm_database_url,
        ),
        (
            CoverageTargetStore::PendingDelivery,
            &target.xmpp.pending_delivery_database_url,
        ),
    ] {
        match location {
            ResolvedXmppDatabaseUrl::InheritedGlobal(url) if url == target.global_database_url => {}
            ResolvedXmppDatabaseUrl::InheritedGlobal(_) => {
                return CoverageRefusal::Unsupported(CoverageTargetError::ChildDiffersFromGlobal(
                    store,
                ));
            }
            ResolvedXmppDatabaseUrl::Explicit(_) | ResolvedXmppDatabaseUrl::EphemeralFallback => {
                return CoverageRefusal::Unsupported(CoverageTargetError::ChildMustInheritGlobal(
                    store,
                ));
            }
        }
    }
    CoverageRefusal::Incomplete(COVERAGE_GAPS_V1)
}
