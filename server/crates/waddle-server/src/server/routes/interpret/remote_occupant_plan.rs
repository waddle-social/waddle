//! Planning a groupchat message that an occupant at another server sent to
//! a room of this deployment (ADR-0020 D10).
//!
//! The room plans the message as it plans any occupant's: its occupancy
//! gate, archive, inbox projections and one copy for each occupant. No
//! connection state of this deployment belongs to the sender, so nothing a
//! local session's plan reads from its stream (carbons, its blocking list,
//! its stream-management position) takes part.

use jid::BareJid;
use xmpp_parsers::message::Message;

use super::effects::{EffectSink, IngressPlan, PlanSink};
use super::Deps;
use crate::ingress::IngressEffectCapture;

/// Plan `message`, a groupchat stanza from the remote occupant its `from`
/// names, for `room`.
pub(crate) async fn plan_remote_occupant_groupchat(
    deps: &Deps<'_>,
    room: BareJid,
    message: Message,
) -> IngressPlan {
    let sink = PlanSink::new();
    let sender = message
        .from
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .cloned();
    if let Some(sender) = &sender {
        sink.observe_sender(sender);
    }
    sink.observe_message(&message);
    let capture = IngressEffectCapture::new();
    let planned =
        super::build_plan_deps(deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    super::room_dispatch::dispatch_to_room(&planned, room, message.clone(), 0).await;
    super::message_plan::finish_plan(&sink, &capture, message, sender)
}
