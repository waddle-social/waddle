use super::NodeIdentity;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// Identity for one physical connection within one node incarnation.
///
/// The random nonce keeps this reference unique across process restarts; the
/// node identity records which process incarnation created the transport.
#[derive(Debug, Clone)]
pub struct ConnectionRef {
    node: NodeIdentity,
    nonce: Uuid,
    next_binding_generation: Arc<AtomicU64>,
}

impl ConnectionRef {
    /// Mint a physical transport identity associated with `node`.
    pub fn new(node: NodeIdentity) -> Self {
        Self {
            node,
            nonce: Uuid::new_v4(),
            next_binding_generation: Arc::new(AtomicU64::new(1)),
        }
    }

    /// The node incarnation that accepted this physical transport.
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }

    /// Process-restart-safe physical transport nonce.
    pub fn nonce(&self) -> Uuid {
        self.nonce
    }

    /// Mint the next binding generation for this transport.
    ///
    /// A failed resume attempt consumes a generation just like a fresh bind.
    /// Exhaustion is reported instead of wrapping and reusing an old identity.
    pub fn next_binding_ref(&self) -> Option<BindingRef> {
        let generation = self
            .next_binding_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()?;
        Some(BindingRef {
            connection: self.clone(),
            generation,
        })
    }
}

impl PartialEq for ConnectionRef {
    fn eq(&self, other: &Self) -> bool {
        self.nonce == other.nonce && self.node.same_incarnation(&other.node)
    }
}

impl Eq for ConnectionRef {}

impl std::hash::Hash for ConnectionRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.node.node_id.hash(state);
        self.node.node_epoch.hash(state);
        self.nonce.hash(state);
    }
}

/// Identity for one authenticated resource binding on a physical connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingRef {
    connection: ConnectionRef,
    generation: u64,
}

impl BindingRef {
    /// Physical transport that owns this binding attempt.
    pub fn connection(&self) -> &ConnectionRef {
        &self.connection
    }

    /// Monotonically increasing attempt number within the physical transport.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_refs_include_nonce_and_node_incarnation() {
        let node = NodeIdentity::new("node-a", "epoch-1");
        let first = ConnectionRef::new(node.clone());
        let same_node = ConnectionRef::new(node.clone());
        let restarted = ConnectionRef::new(NodeIdentity::new("node-a", "epoch-2"));

        assert_ne!(first, same_node);
        assert_ne!(first, restarted);
        assert_eq!(first.node(), &node);
        assert_ne!(first.nonce(), same_node.nonce());
    }

    #[test]
    fn cloned_transport_mints_distinct_binding_generations() {
        let connection = ConnectionRef::new(NodeIdentity::new("node-a", "epoch-1"));
        let shared = connection.clone();
        let failed_resume = connection.next_binding_ref().expect("first generation");
        let fresh_bind = shared.next_binding_ref().expect("second generation");

        assert_eq!(failed_resume.connection(), &connection);
        assert_eq!(fresh_bind.connection(), &connection);
        assert_eq!(failed_resume.generation(), 1);
        assert_eq!(fresh_bind.generation(), 2);
        assert_ne!(failed_resume, fresh_bind);
    }
}
