//! Exact identity for one account actor's resource authority.

use super::{ClaimEpoch, NodeLeaseIdentity};
use jid::BareJid;

/// Opaque incarnation of one `UserActor` authority activation.
///
/// A replacement actor receives a fresh value even when it self-reacquires the
/// same node claim and therefore observes the same claim epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserActorIncarnation(uuid::Uuid);

impl UserActorIncarnation {
    /// Mint a fresh actor incarnation.
    pub fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Equality identity for one account actor's authority to mutate resources.
///
/// This value is identity evidence, not publication authority. Repository
/// writes must compare the complete value in the transaction that performs the
/// write. The actor incarnation fences replacement actors that share a node
/// incarnation and claim epoch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserClaimIdentity {
    bare_jid: BareJid,
    owner: NodeLeaseIdentity,
    claim_epoch: ClaimEpoch,
    actor_incarnation: UserActorIncarnation,
}

impl UserClaimIdentity {
    /// Mint a claim identity for a newly activated account actor.
    pub fn fresh(bare_jid: BareJid, owner: NodeLeaseIdentity, claim_epoch: ClaimEpoch) -> Self {
        Self {
            bare_jid,
            owner,
            claim_epoch,
            actor_incarnation: UserActorIncarnation::fresh(),
        }
    }

    pub fn bare_jid(&self) -> &BareJid {
        &self.bare_jid
    }

    pub fn owner(&self) -> &NodeLeaseIdentity {
        &self.owner
    }

    pub fn claim_epoch(&self) -> ClaimEpoch {
        self.claim_epoch
    }

    pub fn actor_incarnation(&self) -> UserActorIncarnation {
        self.actor_incarnation
    }
}

#[cfg(test)]
mod tests {
    use super::UserClaimIdentity;
    use crate::ownership::{ClaimEpoch, NodeIdentity, NodeLeaseIdentity};

    fn identity(claim_epoch: i64) -> UserClaimIdentity {
        UserClaimIdentity::fresh(
            "alice@example.test".parse().expect("bare JID"),
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a")),
            ClaimEpoch(claim_epoch),
        )
    }

    #[test]
    fn user_claim_identity_round_trip_preserves_account_and_all_generations() {
        let original = identity(7);
        let encoded = serde_json::to_vec(&original).expect("encode claim identity");
        let decoded: UserClaimIdentity =
            serde_json::from_slice(&encoded).expect("decode claim identity");

        assert_eq!(decoded, original);
        assert_eq!(decoded.bare_jid().as_str(), "alice@example.test");
        assert_eq!(decoded.claim_epoch(), ClaimEpoch(7));
    }

    #[test]
    fn actor_and_claim_epoch_changes_produce_distinct_claim_identities() {
        let first = identity(7);
        let replacement_actor = identity(7);
        let replacement_claim = identity(8);

        assert_ne!(first, replacement_actor);
        assert_eq!(first.claim_epoch(), replacement_actor.claim_epoch());
        assert_ne!(
            first.actor_incarnation(),
            replacement_actor.actor_incarnation()
        );
        assert_ne!(first, replacement_claim);
    }

    #[test]
    fn owner_incarnation_and_account_are_part_of_claim_identity() {
        let original = identity(7);
        let other_owner = UserClaimIdentity::fresh(
            "alice@example.test".parse().expect("bare JID"),
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-b")),
            ClaimEpoch(7),
        );
        let other_account = UserClaimIdentity::fresh(
            "bob@example.test".parse().expect("bare JID"),
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a")),
            ClaimEpoch(7),
        );

        assert_ne!(original, other_owner);
        assert_ne!(original, other_account);
    }
}
