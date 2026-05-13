use crate::bootstrap::NS_STREAMS;

use crate::error::{ClientError, ClientResult};
use crate::event::{ClientEvent, ConnectionEvent, MessageDeliveryEvent, StreamManagementEvent};
use crate::state::{SessionBinding, StreamId};
use crate::transport::{StreamClose, TransportMessage};
use minidom::Element;

use super::{BootstrapState, XmppRuntime};

const NS_STREAM_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-streams";

impl XmppRuntime {
    pub(super) fn handle_sm_element(
        &mut self,
        element: &minidom::Element,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        if crate::stream_management::SmState::is_request_ack(element) {
            let h = self.sm_state.inbound_count;
            let ack = crate::stream_management::SmState::build_ack(h);
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(ack),
            )));
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequested,
            )));
        } else if let Some(h) = crate::stream_management::SmState::parse_ack_h(element) {
            if h > self.sm_state.outbound_count {
                self.handle_sm_handled_count_too_high(h, events);
                return Ok(());
            }
            let acked = self.sm_state.process_ack(h);
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckReceived { h },
            )));
            events.extend(acked.into_iter().map(|stanza_id| {
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
            }));
        } else if element.name() == "enabled" {
            let previd = crate::stream_management::SmState::parse_enabled(element);
            self.sm_state.previd = previd.clone();
            self.sm_state.enabled = true;
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Enabled { previd },
            )));
            self.flush_pending_fallback_retries(events);
        } else if element.name() == "resumed" {
            let h = element.attr("h").and_then(|v| v.parse().ok()).unwrap_or(0);
            if h > self.sm_state.outbound_count {
                self.handle_sm_handled_count_too_high(h, events);
                return Ok(());
            }
            let acked = self.sm_state.process_ack(h);
            self.sm_state.previd = element.attr("previd").map(|s| s.to_string());
            self.sm_state.enabled = true;
            self.sm_state.outbound_enabled = true;
            if matches!(self.bootstrap, BootstrapState::AwaitingResume) {
                self.snapshot.binding = Some(self.resumed_session_binding()?);
                self.bootstrap = BootstrapState::Ready;
                self.set_phase(crate::state::SessionPhase::Established)?;
            }
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Resumed { h },
            )));
            events.extend(acked.into_iter().map(|stanza_id| {
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
            }));
            for replay in self.sm_state.mark_unhandled_for_replay() {
                events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    TransportMessage::Element(replay),
                )));
            }
        } else if element.name() == "failed" {
            let resume_failed = matches!(self.bootstrap, BootstrapState::AwaitingResume);
            if let Some(h) = element.attr("h").and_then(|value| value.parse().ok()) {
                if h > self.sm_state.outbound_count {
                    self.handle_sm_handled_count_too_high(h, events);
                    return Ok(());
                }
                let acked = self.sm_state.process_ack(h);
                events.extend(acked.into_iter().map(|stanza_id| {
                    ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                }));
            }
            if resume_failed {
                self.pending_fallback_retries
                    .extend(self.sm_state.unhandled_stanzas());
                self.sm_state.previd = None;
            }
            self.sm_state.stop();
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed,
            )));
            if resume_failed {
                self.request_resource_binding(events)?;
            }
        }

        Ok(())
    }

    fn handle_sm_handled_count_too_high(&mut self, h: u32, events: &mut Vec<ClientEvent>) {
        let error = Element::builder("error", NS_STREAMS)
            .append(Element::builder("undefined-condition", NS_STREAM_ERRORS).build())
            .append(
                Element::builder("handled-count-too-high", crate::stream_management::NS_SM)
                    .attr("h", h.to_string())
                    .attr("send-count", self.sm_state.outbound_count.to_string())
                    .build(),
            )
            .build();
        self.sm_state.stop();
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
            stream_id: self.sm_state.previd.as_ref().map(StreamId::new),
            resumable: self.sm_state.previd.is_some(),
        })
    }
}
