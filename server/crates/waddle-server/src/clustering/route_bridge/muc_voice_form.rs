use super::{NodeId, NodeIdentity, OrderedRelayDeliveryBridge, RelayHandle};
use crate::clustering::relay::{RelayMucVoiceForm, RelayMucVoiceFormReply};
use crate::server::routes::websocket::{
    MUC_VOICE_FORM_MAILBOX_TIMEOUT, MUC_VOICE_FORM_REPLY_TIMEOUT,
};

impl OrderedRelayDeliveryBridge {
    /// Origin side of a forwarded voice request or approval.
    pub(crate) fn muc_voice_form_handle(&self, owner: &NodeIdentity) -> RelayHandle {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(MUC_VOICE_FORM_MAILBOX_TIMEOUT, MUC_VOICE_FORM_REPLY_TIMEOUT)
    }

    /// Owner side: decide from the local room actor. It never forwards again.
    pub(crate) async fn muc_voice_form_local(
        &self,
        message: RelayMucVoiceForm,
    ) -> RelayMucVoiceFormReply {
        match self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        {
            Some(state) => {
                crate::server::routes::websocket::handlers::muc_voice::relayed_voice_form(
                    &state, message,
                )
                .await
            }
            // A node still wiring its services owns no live room actor yet.
            None => RelayMucVoiceFormReply::NotOwner,
        }
    }
}
