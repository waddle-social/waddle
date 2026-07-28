use crate::bootstrap::NS_STREAMS;

use crate::error::{ClientError, ClientResult};
use crate::event::{
    ClientEvent, ConnectionEvent, MessageDeliveryEvent, StreamErrorCondition, StreamErrorDetail,
    StreamManagementEvent,
};
use crate::state::SessionBinding;
use crate::transport::{StreamClose, TransportMessage};
use minidom::Element;

use super::{BootstrapState, SmNegotiationState, XmppRuntime};

const NS_STREAM_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-streams";

impl XmppRuntime {
    pub(super) fn handle_stream_error_element(
        &mut self,
        element: &minidom::Element,
        events: &mut Vec<ClientEvent>,
    ) {
        debug_assert_eq!(element.name(), "error");
        debug_assert_eq!(element.ns(), NS_STREAMS);

        events.push(ClientEvent::Connection(ConnectionEvent::StreamError {
            condition: stream_error_condition(element),
            detail: stream_error_detail(element),
        }));

        // RFC 6120 defines stream errors as terminal for the current XML
        // stream. During XEP-0198 resume bootstrap, recoverable resume-specific
        // failures are represented by `<failed/>`; if the server instead ends
        // the stream, keeping `previd` would replay the same failed pre-bind
        // resume on the next WebSocket connection.
        if matches!(self.bootstrap, BootstrapState::AwaitingResume) {
            self.discard_failed_prebind_resume(events);
        }
    }

    pub(super) fn discard_failed_prebind_resume(&mut self, events: &mut Vec<ClientEvent>) {
        if !matches!(self.bootstrap, BootstrapState::AwaitingResume) {
            return;
        }
        if self.sm_state.previd.is_none() {
            return;
        }

        let failed = self.sm_state.unhandled_message_stanza_ids();
        self.sm_state.previd = None;
        self.fallback_resume_state = None;
        self.pending_fallback_retries.clear();
        self.fallback_retry_writes_in_flight.clear();
        events.extend(failed.into_iter().map(|stanza_id| {
            ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
        }));
        events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed,
        )));
    }

    pub(super) fn handle_sm_element(
        &mut self,
        element: &minidom::Element,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        let control = match crate::stream_management::SmState::parse_inbound_control(element) {
            Ok(control) => control,
            Err(_) => {
                self.handle_sm_protocol_violation(events);
                return Ok(());
            }
        };

        match control {
            crate::stream_management::SmInboundControl::RequestAck => {
                if !matches!(self.sm_negotiation, SmNegotiationState::Enabled) {
                    self.handle_sm_protocol_violation(events);
                    return Ok(());
                }
                let h = self.sm_state.inbound_count;
                let ack = crate::stream_management::SmState::build_ack(h);
                events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    TransportMessage::Element(ack),
                )));
                events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::AckRequested {
                        reason: crate::event::SmAckRequestReason::PeerRequest,
                    },
                )));
            }
            crate::stream_management::SmInboundControl::Ack { h } => {
                if !matches!(self.sm_negotiation, SmNegotiationState::Enabled) {
                    self.handle_sm_protocol_violation(events);
                    return Ok(());
                }
                if self.sm_state.handled_count_too_high(h) {
                    self.handle_sm_handled_count_too_high(h, events);
                    return Ok(());
                }
                let progressed = h != self.sm_state.server_h;
                let acked = self.sm_state.process_ack(h);
                events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::AckReceived { h, progressed },
                )));
                events.extend(acked.into_iter().map(|stanza_id| {
                    ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                }));
            }
            crate::stream_management::SmInboundControl::Enabled {
                previd,
                max_resume_seconds,
            } => {
                // <enabled/> is only legal as the answer to <enable/>. A
                // duplicate on a live SM session is a protocol violation
                // (mirroring unexpected <resumed/>): silently re-running the
                // XEP-0198 §5 counter reset below would drive the next
                // <a h/> backwards on the wire.
                if !matches!(
                    self.sm_negotiation,
                    SmNegotiationState::AwaitingEnableResponse
                ) {
                    self.handle_sm_protocol_violation(events);
                    return Ok(());
                }
                self.sm_state.previd = previd.clone();
                self.sm_state.max_resume_seconds = max_resume_seconds;
                self.sm_state.enabled = true;
                self.sm_negotiation = SmNegotiationState::Enabled;
                // <enabled/> always establishes a NEW SM session (a resumed
                // one answers with <resumed/> instead), so the received-
                // stanza counter restarts from zero here (XEP-0198 §5,
                // issue #1181).
                self.sm_state.start_inbound();
                events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::Enabled { previd },
                )));
                self.flush_pending_fallback_retries(events);
            }
            crate::stream_management::SmInboundControl::Resumed { h, previd } => {
                if !matches!(self.bootstrap, BootstrapState::AwaitingResume)
                    || self.sm_state.previd.as_ref() != Some(&previd)
                {
                    self.handle_sm_protocol_violation(events);
                    return Ok(());
                }
                if self.sm_state.handled_count_too_high(h) {
                    self.handle_sm_handled_count_too_high(h, events);
                    return Ok(());
                }
                let acked = self.sm_state.process_ack(h);
                self.sm_state.previd = Some(previd);
                self.sm_state.enabled = true;
                self.sm_state.outbound_enabled = true;
                self.sm_negotiation = SmNegotiationState::Enabled;
                self.snapshot.binding = Some(self.resumed_session_binding()?);
                self.bootstrap = BootstrapState::Ready;
                self.set_phase(crate::state::SessionPhase::Established)?;
                events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::Resumed { h },
                )));
                events.extend(acked.into_iter().map(|stanza_id| {
                    ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                }));
                // XEP-0198 §5 retains the unacknowledged tail across a successful
                // resume. Queue those writes before requesting their acknowledgement
                // so the serial driver preserves replay order on the wire.
                for replay in self.sm_state.mark_unhandled_for_replay() {
                    events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                        TransportMessage::Element(replay),
                    )));
                }
                if self.sm_state.acknowledgement_clock_pending() {
                    events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                        TransportMessage::Element(
                            crate::stream_management::SmState::build_request_ack(),
                        ),
                    )));
                    events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                        StreamManagementEvent::AckRequested {
                            reason: crate::event::SmAckRequestReason::ResumedUnackedTail,
                        },
                    )));
                }
            }
            crate::stream_management::SmInboundControl::Failed { h } => {
                let resume_failed = matches!(self.bootstrap, BootstrapState::AwaitingResume);
                let enable_failed = matches!(
                    self.sm_negotiation,
                    SmNegotiationState::AwaitingEnableResponse
                );
                if !resume_failed && !enable_failed {
                    self.handle_sm_protocol_violation(events);
                    return Ok(());
                }
                if let Some(h) = h {
                    if self.sm_state.handled_count_too_high(h) {
                        self.handle_sm_handled_count_too_high(h, events);
                        return Ok(());
                    }
                    let acked = self.sm_state.process_ack(h);
                    events.extend(acked.into_iter().map(|stanza_id| {
                        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                    }));
                }
                if resume_failed {
                    // Keep the queue resumable until fallback retries are written to the new stream.
                    self.prepare_fresh_stream_fallback(events, true);
                }
                self.sm_state.stop();
                self.sm_negotiation = SmNegotiationState::Inactive;
                events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::Failed,
                )));
                if resume_failed {
                    self.request_resource_binding(events)?;
                } else {
                    self.sm_advertised = false;
                    self.flush_pending_fallback_retries(events);
                }
            }
        }

        Ok(())
    }

    fn handle_sm_handled_count_too_high(&mut self, h: u32, events: &mut Vec<ClientEvent>) {
        let error = Element::builder("error", NS_STREAMS)
            .append(Element::builder("undefined-condition", NS_STREAM_ERRORS).build())
            .append(
                Element::builder("handled-count-too-high", crate::stream_management::NS_SM)
                    .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
                    .attr(
                        minidom::rxml::xml_ncname!("send-count").to_owned(),
                        self.sm_state.outbound_count.to_string(),
                    )
                    .build(),
            )
            .build();
        self.sm_state.stop();
        self.sm_negotiation = SmNegotiationState::Inactive;
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(error),
        )));
        events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed,
        )));
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Close(StreamClose),
        )));
    }

    fn handle_sm_protocol_violation(&mut self, events: &mut Vec<ClientEvent>) {
        let error = Element::builder("error", NS_STREAMS)
            .append(Element::builder("policy-violation", NS_STREAM_ERRORS).build())
            .build();
        self.sm_state.stop();
        self.sm_negotiation = SmNegotiationState::Inactive;
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(error),
        )));
        events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed,
        )));
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Close(StreamClose),
        )));
    }

    fn resumed_session_binding(&self) -> ClientResult<SessionBinding> {
        let auth = self.oauth_config();
        let jid = auth
            .account
            .with_resource_str(auth.resource.as_str())
            .map_err(|_| ClientError::InvalidBindResponse)?;
        Ok(SessionBinding {
            jid,
            stream_id: self.sm_state.previd.clone(),
            resumable: self.sm_state.previd.is_some(),
        })
    }
}

fn stream_error_condition(element: &Element) -> StreamErrorCondition {
    element
        .children()
        .filter(|child| child.ns() == NS_STREAM_ERRORS && child.name() != "text")
        .find_map(|child| StreamErrorCondition::from_name(child.name()))
        .unwrap_or(StreamErrorCondition::UndefinedCondition)
}

fn stream_error_detail(element: &Element) -> Option<StreamErrorDetail> {
    let handled_count =
        element.get_child("handled-count-too-high", crate::stream_management::NS_SM)?;
    let h = handled_count.attr("h")?.parse().ok()?;
    let send_count = handled_count.attr("send-count")?.parse().ok()?;
    Some(StreamErrorDetail::HandledCountTooHigh { h, send_count })
}
