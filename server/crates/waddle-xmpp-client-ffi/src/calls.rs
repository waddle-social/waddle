use waddle_xmpp_client::messaging::{
    build_finish, build_finish_migrated, build_proceed, build_propose, build_reject,
    build_reject_with_options, build_retract, build_retract_with_options, build_session_accept,
    build_session_initiate, build_session_terminate, CallMedia, JingleReason,
};

use crate::convert::jingle_reason_from_ffi;
use crate::stanza::{iq_set, message_with_jmi};
use crate::{WaddleClient, WaddleJingleReason};

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Send a XEP-0353 §5.1.1 `<propose/>` to the peer's bare JID.
    /// The bare JID lets the responder's server ring every connected
    /// resource until one of them proceeds or rejects.
    pub async fn send_call_propose(
        &self,
        peer_bare_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_propose") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_propose") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_propose(&sid, CallMedia { audio, video }),
        );
        self.send_stanza_or_error(stanza, "send_call_propose").await
    }

    /// Send a XEP-0353 §5.1.2 `<proceed/>` to the *full* JID of the
    /// originator (preserved from the propose `from` per §0.6).
    pub async fn send_call_proceed(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_proceed") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_proceed") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_proceed(&sid));
        self.send_stanza_or_error(stanza, "send_call_proceed").await
    }

    /// Send a XEP-0353 §5.1.3 `<reject/>` to the originator's full JID.
    pub async fn send_call_reject(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_reject") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_reject") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_reject(&sid));
        self.send_stanza_or_error(stanza, "send_call_reject").await
    }

    /// Send a XEP-0353 tie-break `<reject/>` carrying
    /// `<reason><expired/></reason>` plus `<tie-break/>`.
    pub async fn send_call_reject_tie_break(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_reject_tie_break") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_reject_tie_break") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_reject_with_options(&sid, Some(JingleReason::Expired), true),
        );
        self.send_stanza_or_error(stanza, "send_call_reject_tie_break")
            .await
    }

    /// Send a XEP-0353 §5.1.4 `<retract/>` to cancel a ringing call
    /// before the peer answers. Addressed to the responder's *bare*
    /// JID so every resource that may have been ringing receives the
    /// cancellation (XEP-0353 §5.1.4: a retract is addressed to the
    /// callee's bare JID, exactly like the originating propose).
    pub async fn send_call_retract(&self, peer_bare_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_retract") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_retract") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_retract(&sid));
        self.send_stanza_or_error(stanza, "send_call_retract").await
    }

    /// Send a XEP-0353 tie-break `<retract/>` carrying
    /// `<reason><expired/></reason>` plus `<tie-break/>`.
    pub async fn send_call_retract_tie_break(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_retract_tie_break") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_retract_tie_break") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_retract_with_options(&sid, Some(JingleReason::Expired), true),
        );
        self.send_stanza_or_error(stanza, "send_call_retract_tie_break")
            .await
    }

    /// Send a `<finish/>` Waddle JMI extension signaling clean
    /// teardown after a call ended. Addressed to the peer's full JID
    /// so the originating resource sees the finish notice.
    pub async fn send_call_finish(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_finish") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_finish(&sid));
        self.send_stanza_or_error(stanza, "send_call_finish").await
    }

    /// Send Waddle's XEP-0353-compatible migration marker:
    /// `<finish/>` with `<reason><expired/></reason>` and
    /// `<migrated to='new-sid'/>`.
    pub async fn send_call_finish_migrated(
        &self,
        peer_full_jid: String,
        old_sid: String,
        new_sid: String,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish_migrated") else {
            return false;
        };
        let Some(old_sid) = self.parse_session_id(old_sid, "send_call_finish_migrated") else {
            return false;
        };
        let Some(new_sid) = self.parse_session_id(new_sid, "send_call_finish_migrated") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_finish_migrated(&old_sid, JingleReason::Expired, &new_sid),
        );
        self.send_stanza_or_error(stanza, "send_call_finish_migrated")
            .await
    }

    /// Send a XEP-0166 §6.4 `session-initiate` IQ to the peer's full
    /// JID. `initiator_full_jid` names the call originator per §7.1.
    pub async fn send_call_session_initiate(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_initiate") else {
            return false;
        };
        let Some(initiator) =
            self.parse_full_jid(&initiator_full_jid, "send_call_session_initiate")
        else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_initiate") else {
            return false;
        };
        let payload = build_session_initiate(&sid, &initiator, CallMedia { audio, video });
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_initiate")
            .await
    }

    /// Send a XEP-0166 §7.2 `session-accept` IQ.
    pub async fn send_call_session_accept(
        &self,
        peer_full_jid: String,
        responder_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_accept") else {
            return false;
        };
        let Some(responder) = self.parse_full_jid(&responder_full_jid, "send_call_session_accept")
        else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_accept") else {
            return false;
        };
        let payload = build_session_accept(&sid, &responder, CallMedia { audio, video });
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_accept").await
    }

    /// Send a XEP-0166 §7.4 `session-terminate` IQ.
    pub async fn send_call_session_terminate(
        &self,
        peer_full_jid: String,
        sid: String,
        reason: Option<WaddleJingleReason>,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_terminate") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_terminate") else {
            return false;
        };
        let typed_reason = reason.map(jingle_reason_from_ffi);
        let payload = build_session_terminate(&sid, typed_reason);
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_terminate")
            .await
    }
}
