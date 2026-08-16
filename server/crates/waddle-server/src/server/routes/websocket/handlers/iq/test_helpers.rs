use super::*;
use waddle_xmpp::protocol::frame::{parse_frame, InboundFrame};

/// Only called from test helpers.
#[cfg(test)]
pub async fn handle_iq(
    frame: &str,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let mut carbons_enabled = phase.bound_jid().is_some_and(|jid| {
        state
            .deps
            .protocol
            .connection_registry
            .is_carbons_enabled(jid)
    });
    let mut roster_interested = false;
    let mut blocklist_interested = false;

    let iq = match parse_frame(frame) {
        Ok(InboundFrame::Stanza(stanza)) => match *stanza {
            Stanza::Iq(iq) => iq,
            _ => return vec![],
        },
        _ => return vec![],
    };

    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        blocklist_interested: &mut blocklist_interested,
        registry_owner: None,
        state_machine: None,
        ordered_relay_origin: None,
    };
    handle_iq_with_conn_state(
        *iq,
        domain,
        muc_domain,
        state,
        authenticated_session,
        phase,
        &mut conn_state,
    )
    .await
    .into_serialized_frames()
}
