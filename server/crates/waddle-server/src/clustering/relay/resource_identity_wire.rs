//! Frozen identity-bearing resource-registration codecs.
//!
//! These DTOs and reserved message IDs define the next server-to-server wire
//! contracts. They intentionally have no Kameo handlers yet; the current v4,
//! v2 and v1 dispatchers remain the only registered endpoints until the
//! account-resource authority cutover is reviewed.

use super::{
    NodeId, RelayTraceContext, RemoteResourceRegistrationId, RemoteResourceSocketGeneration,
    RemoteResourceStateUpdate,
};
use crate::clustering::route_bridge::RemotePresenceStateSnapshot;
use serde::{Deserialize, Serialize};
use waddle_xmpp::ownership::{BindingRef, UserClaimIdentity};
use waddle_xmpp_core::OccupancySessionGeneration;

/// Reserved message ID for the identity-bearing resource registration.
pub const REMOTE_RESOURCE_REGISTER_V5_ID: &str =
    "waddle.clustering.relay.remote_resource_register.v5";

/// Reserved message ID for identity-fenced resource state updates.
pub const REMOTE_RESOURCE_UPDATE_V2_ID: &str = "waddle.clustering.relay.remote_resource_update.v2";

/// Reserved message ID for identity-fenced resource removal.
pub const REMOTE_RESOURCE_UNREGISTER_V3_ID: &str =
    "waddle.clustering.relay.remote_resource_unregister.v3";

/// Resource metadata transferred with a v5 remote registration.
///
/// `binding_ref` includes the source transport reference, so this DTO avoids a
/// second, independently serialized connection identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteResourceStateSnapshotV5 {
    pub binding_ref: BindingRef,
    pub occupancy_session: Option<OccupancySessionGeneration>,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    pub presence_priority: i8,
    pub presence_state: Option<RemotePresenceStateSnapshot>,
}

/// Identity-bearing server-to-server remote resource registration request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRegisterRemoteUserResourceV5 {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    /// Routing hint only. A future handler must authenticate this value and
    /// the nested binding's node lease against the remote sender and lease.
    pub socket_node: NodeId,
    pub state: RemoteResourceStateSnapshotV5,
    /// Telemetry context only.
    #[serde(default)]
    pub trace: RelayTraceContext,
}

/// Result of a v5 registration. Success requires the owner's full claim
/// identity; an older status-only reply therefore cannot decode as success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRemoteResourceRegistrationV5Reply {
    pub result: RelayRemoteResourceRegistrationV5Result,
}

/// Typed outcome for identity-bearing registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RelayRemoteResourceRegistrationV5Result {
    Registered {
        user_claim_identity: UserClaimIdentity,
    },
    StaleRegistration,
    NotOwner,
    Busy,
    Unavailable,
}

/// Identity-fenced remote resource state update request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayUpdateRemoteUserResourceV2 {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub user_claim_identity: UserClaimIdentity,
    pub binding_ref: BindingRef,
    pub update: RemoteResourceStateUpdate,
    /// Telemetry context only.
    #[serde(default)]
    pub trace: RelayTraceContext,
}

/// Identity-fenced remote resource unregister request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayUnregisterRemoteUserResourceV3 {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub user_claim_identity: UserClaimIdentity,
    pub binding_ref: BindingRef,
    /// Telemetry context only.
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[cfg(test)]
mod tests;
