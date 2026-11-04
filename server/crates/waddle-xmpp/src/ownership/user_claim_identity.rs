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
    use super::{UserActorIncarnation, UserClaimIdentity};
    use crate::ownership::{ClaimEpoch, NodeIdentity, NodeLeaseIdentity};

    fn identity_with(
        bare_jid: &str,
        owner: NodeLeaseIdentity,
        claim_epoch: i64,
        actor_incarnation: UserActorIncarnation,
    ) -> UserClaimIdentity {
        UserClaimIdentity {
            bare_jid: bare_jid.parse().expect("bare JID"),
            owner,
            claim_epoch: ClaimEpoch(claim_epoch),
            actor_incarnation,
        }
    }

    fn clustered_owner(node_id: &str, node_epoch: &str) -> NodeLeaseIdentity {
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(node_id, node_epoch))
    }

    fn actor_incarnation(value: &str) -> UserActorIncarnation {
        UserActorIncarnation(uuid::Uuid::parse_str(value).expect("actor incarnation UUID"))
    }

    #[test]
    fn user_claim_identity_round_trip_preserves_account_and_all_generations() {
        let original = UserClaimIdentity::fresh(
            "alice@example.test".parse().expect("bare JID"),
            clustered_owner("node-a", "epoch-a"),
            ClaimEpoch(7),
        );
        let encoded = serde_json::to_vec(&original).expect("encode claim identity");
        let decoded: UserClaimIdentity =
            serde_json::from_slice(&encoded).expect("decode claim identity");

        assert_eq!(decoded, original);
        assert_eq!(decoded.bare_jid().as_str(), "alice@example.test");
        assert_eq!(decoded.claim_epoch(), ClaimEpoch(7));
    }

    #[test]
    fn actor_and_claim_epoch_changes_produce_distinct_claim_identities() {
        let actor = actor_incarnation("00000000-0000-0000-0000-000000000001");
        let first = identity_with(
            "alice@example.test",
            clustered_owner("node-a", "epoch-a"),
            7,
            actor,
        );
        let replacement_actor = identity_with(
            "alice@example.test",
            clustered_owner("node-a", "epoch-a"),
            7,
            actor_incarnation("00000000-0000-0000-0000-000000000002"),
        );
        let replacement_claim = identity_with(
            "alice@example.test",
            clustered_owner("node-a", "epoch-a"),
            8,
            actor,
        );

        assert_ne!(first, replacement_actor);
        assert_eq!(first.claim_epoch(), replacement_actor.claim_epoch());
        assert_ne!(
            first.actor_incarnation(),
            replacement_actor.actor_incarnation()
        );
        assert_ne!(first, replacement_claim);
    }

    #[test]
    fn every_claim_identity_field_changes_equality_independently() {
        let actor = actor_incarnation("00000000-0000-0000-0000-000000000001");
        let owner = clustered_owner("node-a", "epoch-a");
        let original = identity_with("alice@example.test", owner.clone(), 7, actor);

        let other_account = identity_with("bob@example.test", owner.clone(), 7, actor);
        let other_node_id = identity_with(
            "alice@example.test",
            clustered_owner("node-b", "epoch-a"),
            7,
            actor,
        );
        let other_node_epoch = identity_with(
            "alice@example.test",
            clustered_owner("node-a", "epoch-b"),
            7,
            actor,
        );
        let other_claim_epoch = identity_with("alice@example.test", owner.clone(), 8, actor);
        let other_actor = identity_with(
            "alice@example.test",
            owner,
            7,
            actor_incarnation("00000000-0000-0000-0000-000000000002"),
        );

        assert_ne!(original, other_account, "bare JID participates in equality");
        assert_ne!(
            original, other_node_id,
            "owner node ID participates in equality"
        );
        assert_ne!(
            original, other_node_epoch,
            "owner node epoch participates in equality"
        );
        assert_ne!(
            original, other_claim_epoch,
            "claim epoch participates in equality"
        );
        assert_ne!(
            original, other_actor,
            "actor incarnation participates in equality"
        );
    }

    #[test]
    fn standalone_process_incarnation_participates_in_claim_identity_equality() {
        let local_process_a: NodeLeaseIdentity = serde_json::from_str(
            r#"{"LocalProcess":{"process_incarnation":"00000000-0000-0000-0000-00000000000a"}}"#,
        )
        .expect("first standalone process identity");
        let local_process_b: NodeLeaseIdentity = serde_json::from_str(
            r#"{"LocalProcess":{"process_incarnation":"00000000-0000-0000-0000-00000000000b"}}"#,
        )
        .expect("second standalone process identity");
        let actor = actor_incarnation("00000000-0000-0000-0000-000000000001");
        let first = identity_with("alice@example.test", local_process_a, 7, actor);
        let second = identity_with("alice@example.test", local_process_b, 7, actor);

        assert_ne!(first, second);
    }
}
