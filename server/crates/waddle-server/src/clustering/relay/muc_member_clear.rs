//! Clearing one member's affiliation in a room another node owns.
//!
//! A room's affiliation list lives in the `RoomActor` of the node holding
//! the room's claim, so removing a member everywhere (deleting a bot, D8)
//! asks each remote owner to check and apply the change through its own
//! actor, the same core the local admin path runs. Both directions are
//! typed data, and the receiver never forwards again. A peer that predates
//! this id answers `UnknownMessage` before any handler runs.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayMucMemberClear {
    pub room: jid::BareJid,
    pub member: jid::BareJid,
    pub step: RelayMucMemberClearStep,
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayMucMemberClearStep {
    /// Report whether the member is the room's only owner; change nothing.
    Check,
    /// Set the member's affiliation to `none`, unless it is the only owner.
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Reply)]
pub enum RelayMucMemberClearReply {
    /// `Check`: the member is not the only owner, so it can be cleared.
    Clearable,
    /// `Clear`: the member now holds no affiliation in the room.
    Cleared,
    /// The member is the room's only owner; nothing changed.
    SoleOwner,
    /// The receiver has no live local `RoomActor` for the room; nothing ran.
    NotOwner,
    /// The owner could not apply the change; retrying may succeed.
    Failed,
}

#[kameo::remote_message("waddle.clustering.relay.muc_member_clear.v1")]
impl Message<RelayMucMemberClear> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayMucMemberClearReply>;

    async fn handle(
        &mut self,
        msg: RelayMucMemberClear,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span(RelayDispatchKind::MucMemberClear, &msg.trace);
        span.record("jid", tracing::field::display(&msg.room));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.muc_member_clear_local(msg).await
        })
    }
}

impl RelayHandle {
    /// Both steps are idempotent, so a stale relay ref is resolved and
    /// asked once more.
    pub(crate) async fn muc_member_clear(
        &mut self,
        mut message: RelayMucMemberClear,
    ) -> Result<RelayMucMemberClearReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.muc_member_clear_inner(message) => result,
        }
    }

    async fn muc_member_clear_inner(
        &mut self,
        message: RelayMucMemberClear,
    ) -> Result<RelayMucMemberClearReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                self.resolve()
                    .await?
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }
}
