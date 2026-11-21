use super::{
    handle_iq_with_conn_state, ConnectionPhase, IqConnState, Session, Stanza, WebSocketState,
};
use crate::permissions::host_grant::SpaceReplyAuthority;
use waddle_xmpp::protocol::frame::{parse_frame, InboundFrame};

/// Only called from test helpers.
#[cfg(test)]
pub async fn handle_iq(
    frame: &str,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: Option<&Session>,
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
    let registry_owner = phase.bound_jid().and_then(|jid| {
        state
            .deps
            .protocol
            .connection_registry
            .entry(jid)
            .map(|entry| entry.binding_id())
    });

    let iq = match parse_frame(frame) {
        Ok(InboundFrame::Stanza(stanza)) => match *stanza {
            Stanza::Iq(iq) => iq,
            _ => return vec![],
        },
        _ => return vec![],
    };

    let occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        blocklist_interested: &mut blocklist_interested,
        occupancy_session: &occupancy_session,
        registry_owner: registry_owner.as_ref(),
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
    .frames
    .into_iter()
    .filter_map(|frame| {
        let Some(reply) = frame.protected_space_reply() else {
            return Some(frame.into_serialized_xml());
        };
        let SpaceReplyAuthority::Unmanaged(guard) = &reply.grant else {
            panic!("managed Space replies must be exercised through the WebSocket writer");
        };
        let recipient = phase
            .bound_jid()
            .expect("protected reply to a bound resource");
        let element = reply.payload.as_ref().to_element();
        assert!(
            guard.permits_reply(&element, recipient),
            "test reply must match its guard"
        );
        guard.admit(|| reply.payload.wire().to_owned())
    })
    .collect()
}
