//! Mutation tripwire for the configured MUC durable backend.
use std::sync::Arc;
use waddle_xmpp::muc::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomCommitFuture,
    RoomDurableMutation, RoomMutationEffects,
};
use waddle_xmpp::ownership::InProcessClaimStore;

pub(super) struct PoisonMuc(super::super::room_subject::SubjectMutationStore);

impl PoisonMuc {
    pub(super) fn new(claims: Arc<InProcessClaimStore>) -> Self {
        Self(super::super::room_subject::SubjectMutationStore::new(
            claims,
        ))
    }
}

impl MucDurableStore for PoisonMuc {
    fn load_room_state_fenced<'a>(
        &'a self,
        room: &'a jid::BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        self.0.load_room_state_fenced(room, fence)
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room: &'a jid::BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        self.0.check_exact_claim_fence(room, fence)
    }

    fn check_fenced_fanout<'a>(&'a self, room: &'a jid::BareJid) -> MucDurableFuture<'a, bool> {
        self.0.check_fenced_fanout(room)
    }

    fn current_claim_fence(&self, room: &jid::BareJid) -> Option<RoomClaimFenceContext> {
        self.0.current_claim_fence(room)
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a jid::BareJid,
        _fence: &'a RoomClaimFenceContext,
        _intent: RoomDurableMutation,
        _effects: RoomMutationEffects,
    ) -> RoomCommitFuture<'a> {
        panic!("Phases A/B wrote MucDurableStore::commit_room_mutation")
    }
}
