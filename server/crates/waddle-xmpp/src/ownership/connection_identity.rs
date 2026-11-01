//! Physical connection identity carried by local and remote resource indexes.
//!
//! This identity is deliberately separate from a full JID alias, a local
//! registry binding, the ingress generation, and durable stream-management
//! generations. It is equality evidence only; it does not authorize writes or
//! publication.

use super::NodeIdentity;

/// The incarnation that minted a physical connection reference.
///
/// Clustered nodes use the current node ID and lease epoch. Standalone
/// processes use a process-scoped random incarnation because
/// [`NodeIdentity::local`] is intentionally fixed and therefore cannot fence a
/// process restart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NodeLeaseIdentity(NodeLeaseIdentityKind);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum NodeLeaseIdentityKind {
    Clustered { node_id: String, node_epoch: String },
    LocalProcess { process_incarnation: uuid::Uuid },
}

impl NodeLeaseIdentity {
    /// Capture the identity's node incarnation without copying its current
    /// publication authority. Authority remains owned by `SharedNodeIdentity`.
    pub fn from_node_identity(identity: &NodeIdentity) -> Self {
        Self(NodeLeaseIdentityKind::Clustered {
            node_id: identity.node_id.clone(),
            node_epoch: identity.node_epoch.clone(),
        })
    }

    /// Return the stable incarnation shared by connections in this standalone
    /// process. The value changes when the process restarts.
    pub fn local_process() -> Self {
        static PROCESS_INCARNATION: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();

        Self(NodeLeaseIdentityKind::LocalProcess {
            process_incarnation: *PROCESS_INCARNATION.get_or_init(uuid::Uuid::new_v4),
        })
    }
}

/// Opaque identity for one physical transport generation.
///
/// This is a random equality token, not an ordered counter and not the durable
/// P3.3 `ConnectionGeneration` used to fence resumable-session storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PhysicalTransportGeneration(uuid::Uuid);

impl PhysicalTransportGeneration {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Immutable identity of one physical WebSocket transport.
///
/// A successful resume on a newly accepted socket gets a new reference. A
/// failed resume followed by a fresh bind on the same socket keeps the
/// reference and receives a separate `LocalConnectionBindingId` for its new
/// published JID binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ConnectionRef {
    node_lease: NodeLeaseIdentity,
    transport_generation: PhysicalTransportGeneration,
}

impl ConnectionRef {
    /// Mint a new physical transport reference under the supplied node
    /// incarnation. Clone the returned value when propagating the same socket
    /// through its registry, remote mirror, and live SM state.
    pub fn fresh(node_lease: NodeLeaseIdentity) -> Self {
        Self {
            node_lease,
            transport_generation: PhysicalTransportGeneration::fresh(),
        }
    }

    pub fn node_lease(&self) -> &NodeLeaseIdentity {
        &self.node_lease
    }

    pub fn transport_generation(&self) -> PhysicalTransportGeneration {
        self.transport_generation
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionRef, NodeLeaseIdentity};
    use crate::ownership::{NodeIdentity, SharedNodeIdentity};

    #[tokio::test]
    async fn node_lease_identity_tracks_incarnation_not_authority() {
        let active = NodeIdentity::new("node-a", "epoch-a");
        let shared = SharedNodeIdentity::new(active.clone());
        let active_incarnation = NodeLeaseIdentity::from_node_identity(&active);

        shared.disable().await;

        assert_eq!(
            active_incarnation,
            NodeLeaseIdentity::from_node_identity(&shared.current())
        );
        assert_ne!(
            active_incarnation,
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-b"))
        );
        assert_ne!(
            active_incarnation,
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-b", "epoch-a"))
        );
    }

    #[test]
    fn local_identity_is_shared_by_the_process_but_not_fixed_to_the_local_sentinel() {
        let first = NodeLeaseIdentity::local_process();
        let second = NodeLeaseIdentity::local_process();

        assert_eq!(first, second);
        assert_ne!(
            first,
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::local())
        );
    }

    #[test]
    fn connection_ref_equality_includes_node_incarnation_and_transport_generation() {
        let node = NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a"));
        let first = ConnectionRef::fresh(node.clone());
        let same_transport = first.clone();
        let next_transport = ConnectionRef::fresh(node);
        let next_incarnation = ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
            &NodeIdentity::new("node-a", "epoch-b"),
        ));

        assert_eq!(first, same_transport);
        assert_ne!(first, next_transport);
        assert_ne!(first, next_incarnation);
        assert_ne!(
            first.transport_generation(),
            next_transport.transport_generation()
        );
    }

    #[test]
    fn connection_ref_serialization_preserves_all_identity_components() {
        let reference = ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
            &NodeIdentity::new("node-a", "epoch-a"),
        ));

        let serialized = serde_json::to_vec(&reference).expect("serialize connection ref");
        let deserialized: ConnectionRef =
            serde_json::from_slice(&serialized).expect("deserialize connection ref");

        assert_eq!(reference, deserialized);
    }
}
