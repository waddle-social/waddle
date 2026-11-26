//! XEP-0045 voice requests and approvals for rooms owned by another node.
//!
//! A room's occupants, roles and moderators are known to the actor of the
//! node that owns the room's claim, so a session connected to any other node
//! hands its `muc#request` form to that owner over
//! `waddle.clustering.relay.muc_voice_form.v1`. The owner decides it with the
//! same function that decides a form for its own sessions
//! ([`super::muc_voice::decide_voice_form`]) and answers with the stanzas the
//! sender is owed; the origin writes them to the sender's session. The
//! approval forms for the moderators and the role-change presence leave the
//! owner through its own delivery, which reaches sessions on every node. A
//! form that reaches the owner is never forwarded again.

use std::time::Duration;

use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::super::WebSocketState;
use crate::clustering::codec::RemoteStanza;
use crate::clustering::relay::{
    RelayAskError, RelayHandle, RelayMucVoiceForm, RelayMucVoiceFormReply,
};
use crate::clustering::NodeId;

/// Receiver-side windows for one owner ask; kameo enforces them on the owner.
pub(crate) const MUC_VOICE_FORM_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const MUC_VOICE_FORM_REPLY_TIMEOUT: Duration = Duration::from_secs(8);
/// The origin's whole wait, claim reads and the re-ask included, stays inside
/// the 15 s stanza-handler backstop.
const ORIGIN_BUDGET: Duration = Duration::from_secs(11);

/// Replaces only the transport hop to the owner.
#[cfg(test)]
pub(crate) type TestVoiceOwnerRelay = std::sync::Arc<
    dyn Fn(
            waddle_xmpp::ownership::NodeIdentity,
            RelayMucVoiceForm,
        )
            -> futures::future::BoxFuture<'static, Result<RelayMucVoiceFormReply, RelayAskError>>
        + Send
        + Sync,
>;

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_VOICE_OWNER_RELAY: TestVoiceOwnerRelay;
}

pub(crate) enum ForwardedVoice {
    /// The owner decided; these are the stanzas the sender is owed.
    Frames(Vec<Stanza>),
    /// No node owns the room.
    NoOwner,
    /// The owner is unreachable or the claim is moving.
    Unavailable,
}

/// Origin side: hand `incoming`, which `sender` sent to a room with no actor
/// on this node, to the room's owner.
pub(crate) async fn forward_voice_form(
    state: &WebSocketState,
    room: &BareJid,
    sender: &FullJid,
    incoming: &Message,
) -> ForwardedVoice {
    let mut stamped = incoming.clone();
    stamped.from = Some(Jid::from(sender.clone()));
    let message = RelayMucVoiceForm {
        room: room.clone(),
        sender: sender.clone(),
        message: RemoteStanza(Stanza::Message(stamped)),
        trace: Default::default(),
    };
    let deadline = tokio::time::Instant::now() + ORIGIN_BUDGET;
    tokio::time::timeout_at(deadline, ask_current_owner(state, &message))
        .await
        .unwrap_or(ForwardedVoice::Unavailable)
}

async fn ask_current_owner(state: &WebSocketState, message: &RelayMucVoiceForm) -> ForwardedVoice {
    // The claim may move between the read and the ask; follow it once. A
    // `NotOwner` reply says nothing ran, so asking the next owner is safe.
    let mut reresolved = false;
    loop {
        let owner = match crate::server::routes::interpret::extension_room_owner(
            state,
            &message.room,
        )
        .await
        {
            Ok(None) => return ForwardedVoice::NoOwner,
            Ok(Some(owner)) => owner,
            Err(_) => return ForwardedVoice::Unavailable,
        };
        match ask_owner(state, &owner, message.clone()).await {
            Ok(RelayMucVoiceFormReply::Handled(stanzas)) => {
                return ForwardedVoice::Frames(stanzas.into_iter().map(|s| s.0).collect());
            }
            Ok(RelayMucVoiceFormReply::NotOwner) if !reresolved => reresolved = true,
            Ok(RelayMucVoiceFormReply::NotOwner) => return ForwardedVoice::Unavailable,
            Err(error) => {
                tracing::warn!(
                    room = %message.room,
                    owner = %owner.node_id,
                    %error,
                    "a voice form could not reach the room owner"
                );
                return ForwardedVoice::Unavailable;
            }
        }
    }
}

async fn ask_owner(
    state: &WebSocketState,
    owner: &waddle_xmpp::ownership::NodeIdentity,
    message: RelayMucVoiceForm,
) -> Result<RelayMucVoiceFormReply, RelayAskError> {
    #[cfg(test)]
    if let Ok(relay) = TEST_VOICE_OWNER_RELAY.try_with(std::sync::Arc::clone) {
        return relay(owner.clone(), message).await;
    }
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return Err(RelayAskError::NotFound {
            node_id: NodeId::new(owner.node_id.clone()),
        });
    };
    let mut handle: RelayHandle = bridge.muc_voice_form_handle(owner);
    handle.muc_voice_form(message).await
}
