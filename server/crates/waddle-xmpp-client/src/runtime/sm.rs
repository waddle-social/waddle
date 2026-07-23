use crate::bootstrap::NS_STREAMS;

use crate::error::{ClientError, ClientResult};
use crate::event::{
    ClientEvent, ConnectionEvent, MessageDeliveryEvent, StreamErrorCondition, StreamErrorDetail,
    StreamManagementEvent,
};
use crate::state::{SessionBinding, StreamId};
use crate::transport::{StreamClose, TransportMessage};
use minidom::Element;

use super::{BootstrapState, XmppRuntime};

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

        if !matches!(self.bootstrap, BootstrapState::Ready | BootstrapState::Idle) {
            self.discard_fallback_resume_state();
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

    fn discard_fallback_resume_state(&mut self) {
        self.fallback_resume_state = None;
        self.pending_fallback_retries.clear();
        self.fallback_retry_writes_in_flight.clear();
    }

    pub(super) fn handle_sm_element(
        &mut self,
        element: &minidom::Element,
        now_ms: u64,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        if crate::stream_management::SmState::is_request_ack(element) {
            // RFC 7395 permits no XML after our confirmed close half. The
            // final peer `<a/>` remains useful below, but a late `<r/>`
            // cannot be answered without violating the close fence.
            if self.stream_close_sent_confirmed() {
                return Ok(());
            }
            let h = self.sm_state.inbound_count;
            let ack = crate::stream_management::SmState::build_ack(h);
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(ack),
            )));
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequested,
            )));
        } else if let Some(h) = crate::stream_management::SmState::parse_ack_h(element) {
            let processed = match self.sm_state.process_ack_at(h, now_ms) {
                Ok(processed) => processed,
                Err(_) => {
                    self.handle_sm_handled_count_too_high(h, events);
                    return Ok(());
                }
            };
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckReceived { h },
            )));
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckObserved {
                    progressed: processed.observation.progressed,
                    latency_ms: processed.observation.latency_ms,
                    unacked: processed.observation.unacked,
                },
            )));
            events.extend(processed.acked.into_iter().map(|stanza_id| {
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
            }));
        } else if element.name() == "enabled" {
            self.pending_ack_request = None;
            // <enabled/> is only legal as the answer to <enable/>. A
            // duplicate on a live SM session is a protocol violation
            // (mirroring unexpected <resumed/>): silently re-running the
            // XEP-0198 §5 counter reset below would drive the next
            // <a h/> backwards on the wire.
            if self.sm_state.enabled {
                self.handle_sm_protocol_violation(events);
                return Ok(());
            }
            let previd = crate::stream_management::SmState::parse_enabled(element);
            let max_resume_seconds = crate::stream_management::SmState::parse_enabled_max(element);
            self.sm_state.previd = previd.clone();
            self.sm_state.max_resume_seconds = max_resume_seconds;
            self.sm_state.enabled = true;
            // <enabled/> always establishes a NEW SM session (a resumed
            // one answers with <resumed/> instead), so the received-
            // stanza counter restarts from zero here (XEP-0198 §5,
            // issue #1181).
            self.sm_state.start_inbound();
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Enabled { previd },
            )));
            self.flush_pending_fallback_retries(events);
        } else if element.name() == "resumed" {
            self.pending_ack_request = None;
            let Some(h) = element.attr("h").and_then(|v| v.parse().ok()) else {
                self.handle_sm_protocol_violation(events);
                return Ok(());
            };
            let Some(previd) = element
                .attr("previd")
                .and_then(|value| crate::stream_management::SmSessionId::new(value).ok())
            else {
                self.handle_sm_protocol_violation(events);
                return Ok(());
            };
            if !matches!(self.bootstrap, BootstrapState::AwaitingResume)
                || self.sm_state.previd.as_ref() != Some(&previd)
            {
                self.handle_sm_protocol_violation(events);
                return Ok(());
            }
            let processed = match self.sm_state.process_ack_at(h, now_ms) {
                Ok(processed) => processed,
                Err(_) => {
                    self.handle_sm_handled_count_too_high(h, events);
                    return Ok(());
                }
            };
            self.sm_state.previd = Some(previd);
            self.sm_state.enabled = true;
            self.sm_state.outbound_enabled = true;
            self.snapshot.binding = Some(self.resumed_session_binding()?);
            self.bootstrap = BootstrapState::Ready;
            self.set_phase(crate::state::SessionPhase::Established)?;
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Resumed { h },
            )));
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckObserved {
                    progressed: processed.observation.progressed,
                    latency_ms: processed.observation.latency_ms,
                    unacked: processed.observation.unacked,
                },
            )));
            events.extend(processed.acked.into_iter().map(|stanza_id| {
                ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
            }));
            self.sm_state.begin_replay_transition_at(now_ms);
            for replay in self.sm_state.mark_unhandled_for_replay() {
                events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    TransportMessage::Element(replay),
                )));
            }
        } else if element.name() == "failed" {
            self.pending_ack_request = None;
            let resume_failed = matches!(self.bootstrap, BootstrapState::AwaitingResume);
            if let Some(h) = element.attr("h").and_then(|value| value.parse().ok()) {
                let processed = match self.sm_state.process_ack_at(h, now_ms) {
                    Ok(processed) => processed,
                    Err(_) => {
                        self.handle_sm_handled_count_too_high(h, events);
                        return Ok(());
                    }
                };
                events.extend(processed.acked.into_iter().map(|stanza_id| {
                    ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id })
                }));
            }
            if resume_failed {
                let failed = self.sm_state.unhandled_message_stanza_ids();
                // Keep the queue resumable until fallback retries are written to the new stream.
                self.fallback_resume_state = self.sm_state.resume_state();
                self.pending_fallback_retries
                    .extend(self.sm_state.unhandled_stanzas_for_fallback_retry());
                self.sm_state.previd = None;
                events.extend(failed.into_iter().map(|stanza_id| {
                    ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id })
                }));
            }
            self.sm_state.stop();
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
            .append(Element::builder("bad-request", NS_STREAM_ERRORS).build())
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
            stream_id: self
                .sm_state
                .previd
                .as_ref()
                .map(|previd| StreamId::new(previd.as_str())),
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
