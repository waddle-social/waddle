//! #1803 asking side: the eviction guard must clear with EVERY unexpired peer.
use std::sync::Mutex;

use super::*;

fn target() -> FullJid {
    "juliet@example.test/web-1803".parse().expect("full jid")
}

fn node(id: &str) -> NodeIdentity {
    NodeIdentity::new(id, "epoch")
}

/// Membership that answers with a fixed roster, or refuses to answer.
struct StaticMembership(Result<Vec<NodeIdentity>, ()>);

#[async_trait]
impl ClusterMembership for StaticMembership {
    async fn peers(&self) -> Result<Vec<NodeIdentity>, ClaimError> {
        self.0
            .clone()
            .map_err(|()| ClaimError::Backend("membership unreadable".to_string()))
    }
}

/// One scripted answer per node id; an unlisted node fails the ask, exactly as
/// an unreachable peer does.
struct ScriptedPeers {
    answers: Vec<(&'static str, RelayResourcePresenceReply)>,
    asked: Mutex<Vec<(String, FullJid)>>,
}

impl ScriptedPeers {
    fn new(answers: Vec<(&'static str, RelayResourcePresenceReply)>) -> Self {
        Self {
            answers,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<(String, FullJid)> {
        self.asked.lock().expect("probe log").clone()
    }
}

#[async_trait]
impl ResourcePresenceAsker for ScriptedPeers {
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError> {
        self.asked
            .lock()
            .expect("probe log")
            .push((peer.node_id.clone(), target.clone()));
        self.answers
            .iter()
            .find(|(node_id, _)| *node_id == peer.node_id)
            .map(|(_, reply)| *reply)
            .ok_or_else(|| RelayAskError::Send {
                // Exactly what kameo reports for a peer that does not know the
                // message id — a rolling update's old replica.
                failure: RelaySendFailure::Codec,
                effect: RelaySendEffect::NoEffect,
                message: "peer does not know waddle.clustering.relay.resource_presence.v1"
                    .to_string(),
            })
    }
}

/// A single-node cluster: there is no peer that could be holding the socket,
/// so absence is proven without asking anyone.
#[tokio::test]
async fn no_peers_proves_absence_without_asking() {
    let membership = StaticMembership(Ok(Vec::new()));
    let asker = ScriptedPeers::new(Vec::new());
    assert_eq!(
        peer_resource_reachability(&membership, &asker, &target()).await,
        PeerResourceReachability::AbsentOnEveryPeer
    );
    assert!(asker.asked().is_empty());
}

/// The production ghost shape: every peer is asked about the EXACT full JID
/// and every one denies it.
#[tokio::test]
async fn every_peer_absent_proves_absence() {
    let membership = StaticMembership(Ok(vec![node("b"), node("c")]));
    let asker = ScriptedPeers::new(vec![
        ("b", RelayResourcePresenceReply::Absent),
        ("c", RelayResourcePresenceReply::Absent),
    ]);
    assert_eq!(
        peer_resource_reachability(&membership, &asker, &target()).await,
        PeerResourceReachability::AbsentOnEveryPeer
    );
    let asked = asker.asked();
    assert_eq!(asked.len(), 2, "every peer is asked: {asked:?}");
    assert!(
        asked.iter().all(|(_, jid)| jid == &target()),
        "the peers must be asked about the EXACT full JID: {asked:?}"
    );
}

/// The live-socket case the claim-shaped guard used to miss: the socket lives
/// on a peer that holds no claim for the account at all.
#[tokio::test]
async fn one_peer_present_leaves_absence_unproven() {
    let membership = StaticMembership(Ok(vec![node("b"), node("c")]));
    let asker = ScriptedPeers::new(vec![
        ("b", RelayResourcePresenceReply::Absent),
        ("c", RelayResourcePresenceReply::Present),
    ]);
    assert_eq!(
        peer_resource_reachability(&membership, &asker, &target()).await,
        PeerResourceReachability::NotProven
    );
}

/// Fail closed on a peer that cannot answer — an old replica mid-rolling
/// update, a timeout, or a transport failure.
#[tokio::test]
async fn a_failing_peer_ask_leaves_absence_unproven() {
    let membership = StaticMembership(Ok(vec![node("b"), node("unreachable")]));
    let asker = ScriptedPeers::new(vec![("b", RelayResourcePresenceReply::Absent)]);
    assert_eq!(
        peer_resource_reachability(&membership, &asker, &target()).await,
        PeerResourceReachability::NotProven
    );
}

/// Fail closed on a membership read that cannot answer: an unknown cluster is
/// not an empty one.
#[tokio::test]
async fn an_unreadable_membership_leaves_absence_unproven() {
    let membership = StaticMembership(Err(()));
    let asker = ScriptedPeers::new(vec![("b", RelayResourcePresenceReply::Absent)]);
    assert_eq!(
        peer_resource_reachability(&membership, &asker, &target()).await,
        PeerResourceReachability::NotProven
    );
    assert!(
        asker.asked().is_empty(),
        "no peer may be asked without a membership answer"
    );
}

/// A peer that never answers must not hold the whole stalled-row attempt: the
/// fan-out resolves inside its own budget and fails closed.
#[tokio::test(start_paused = true)]
async fn a_hanging_peer_is_bounded_by_the_fanout_budget() {
    struct Hangs;

    #[async_trait]
    impl ResourcePresenceAsker for Hangs {
        async fn resource_presence(
            &self,
            _peer: &NodeIdentity,
            _target: &FullJid,
        ) -> Result<RelayResourcePresenceReply, RelayAskError> {
            std::future::pending().await
        }
    }

    let membership = StaticMembership(Ok(vec![node("b")]));
    assert_eq!(
        peer_resource_reachability(&membership, &Hangs, &target()).await,
        PeerResourceReachability::NotProven
    );
}

/// A truncated membership page would silently shrink the set an eviction has
/// to clear with, so the production membership refuses to answer at all.
#[tokio::test]
async fn a_truncated_membership_page_is_an_error() {
    struct FloodLease(usize);

    #[async_trait]
    impl NodeLeaseStore for FloodLease {
        async fn register(
            &self,
            _me: &NodeIdentity,
            _pod_template_hash: Option<String>,
        ) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn heartbeat(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }
        async fn expire(
            &self,
            _owner: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(false)
        }
        async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn count_other_live_nodes(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<usize, ClaimError> {
            Ok(self.0)
        }
        async fn list_other_unexpired_nodes(
            &self,
            _me: &NodeIdentity,
            limit: usize,
        ) -> Result<Vec<NodeIdentity>, ClaimError> {
            Ok((0..self.0.min(limit))
                .map(|index| node(Box::leak(format!("peer-{index}").into_boxed_str())))
                .collect())
        }
        async fn reconcile(
            &self,
            _me: &NodeIdentity,
            _locally_owned: &[waddle_xmpp::ownership::Entity],
        ) -> Result<Vec<waddle_xmpp::ownership::Entity>, ClaimError> {
            Ok(Vec::new())
        }
        async fn report_steal_intent(
            &self,
            _entity: &waddle_xmpp::ownership::Entity,
            _reporter: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn owner_steal_intents(
            &self,
            _me: &NodeIdentity,
        ) -> Result<
            Vec<(
                waddle_xmpp::ownership::Entity,
                waddle_xmpp::ownership::ClaimEpoch,
            )>,
            ClaimError,
        > {
            Ok(Vec::new())
        }
        async fn clear_steal_intent(
            &self,
            _entity: &waddle_xmpp::ownership::Entity,
            _me: &NodeIdentity,
            _mine: waddle_xmpp::ownership::ClaimEpoch,
        ) -> Result<u64, ClaimError> {
            Ok(0)
        }
        async fn list_orphaned_sm_session_claims(
            &self,
        ) -> Result<Vec<crate::clustering::claims::OrphanedSmSessionClaim>, ClaimError> {
            Ok(Vec::new())
        }
        async fn list_orphaned_room_actor_claims_page(
            &self,
            _after: Option<crate::clustering::claims::RoomOrphanScanCursor>,
            _limit: usize,
        ) -> Result<crate::clustering::claims::OrphanedRoomActorClaimPage, ClaimError> {
            Ok(crate::clustering::claims::OrphanedRoomActorClaimPage {
                candidates: Vec::new(),
                next_cursor: None,
                has_more: false,
                quarantined: 0,
            })
        }
        async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
            Ok(None)
        }
    }

    let identity = SharedNodeIdentity::new(node("me"));
    let small = NodeLeaseClusterMembership::new(Arc::new(FloodLease(2)), identity.clone());
    assert_eq!(small.peers().await.expect("small cluster").len(), 2);

    let flooded = NodeLeaseClusterMembership::new(Arc::new(FloodLease(4096)), identity);
    assert!(
        flooded.peers().await.is_err(),
        "a full page must not be mistaken for the whole cluster"
    );
}
