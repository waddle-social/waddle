use std::{collections::VecDeque, str::FromStr};

use xmpp_parsers::{
    minidom::Element,
    sm::{A, Enable, Nonza, Resume},
};

use crate::error::ConnectionError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamManagementState {
    #[default]
    Disabled,
    Enabling,
    Enabled,
    Resuming,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamManagementAction {
    SendNonza(Nonza),
    ReplayStanzas(Vec<Vec<u8>>),
}

#[derive(Debug, Default)]
pub struct StreamManager {
    state: StreamManagementState,
    inbound_handled: u32,
    last_acked_by_server: u32,
    resume_supported: bool,
    stream_id: Option<xmpp_parsers::sm::StreamId>,
    unacked_stanzas: VecDeque<Vec<u8>>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> StreamManagementState {
        self.state
    }

    pub fn on_stream_started(&mut self) -> Option<Nonza> {
        if matches!(self.state, StreamManagementState::Resuming) {
            if let Some(stream_id) = self.stream_id.clone() {
                return Some(Nonza::Resume(Resume {
                    h: self.inbound_handled,
                    previd: stream_id,
                }));
            }
            self.state = StreamManagementState::Disabled;
        }

        if matches!(self.state, StreamManagementState::Disabled) {
            self.state = StreamManagementState::Enabling;
            return Some(Nonza::Enable(Enable::new().with_resume()));
        }

        None
    }

    pub fn on_connect_attempt_failed(&mut self) {
        if matches!(self.state, StreamManagementState::Enabling) {
            self.state = StreamManagementState::Disabled;
        }
    }

    pub fn prepare_for_reconnect(&mut self) {
        if self.is_resumable() {
            self.state = StreamManagementState::Resuming;
        } else {
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        self.state = StreamManagementState::Disabled;
        self.inbound_handled = 0;
        self.last_acked_by_server = 0;
        self.resume_supported = false;
        self.stream_id = None;
        self.unacked_stanzas.clear();
    }

    pub fn mark_inbound_handled(&mut self) {
        if matches!(
            self.state,
            StreamManagementState::Enabled | StreamManagementState::Resuming
        ) {
            self.inbound_handled = self.inbound_handled.wrapping_add(1);
        }
    }

    pub fn track_outbound_stanza(&mut self, stanza: &[u8]) {
        if !matches!(
            self.state,
            StreamManagementState::Enabled | StreamManagementState::Resuming
        ) {
            return;
        }

        self.unacked_stanzas.push_back(stanza.to_vec());
    }

    pub fn process_nonza(
        &mut self,
        nonza: Nonza,
    ) -> Result<Vec<StreamManagementAction>, ConnectionError> {
        match nonza {
            Nonza::Enabled(enabled) => {
                self.state = StreamManagementState::Enabled;
                self.resume_supported = enabled.resume;
                self.stream_id = if enabled.resume { enabled.id } else { None };
                Ok(Vec::new())
            }
            Nonza::Ack(ack) => {
                self.apply_ack(ack.h)?;
                Ok(Vec::new())
            }
            Nonza::Req(_) => Ok(vec![StreamManagementAction::SendNonza(Nonza::Ack(A::new(
                self.inbound_handled,
            )))]),
            Nonza::Resumed(resumed) => {
                let expected_id = self.stream_id.as_ref().ok_or_else(|| {
                    ConnectionError::StreamError(
                        "received <resumed/> without a tracked stream id".to_string(),
                    )
                })?;

                if resumed.previd != *expected_id {
                    return Err(ConnectionError::StreamError(format!(
                        "received <resumed/> for unexpected stream id '{}'",
                        resumed.previd.0
                    )));
                }

                self.apply_ack(resumed.h)?;
                self.state = StreamManagementState::Enabled;
                Ok(vec![StreamManagementAction::ReplayStanzas(
                    self.unacked_stanzas.iter().cloned().collect(),
                )])
            }
            Nonza::Failed(failed) => {
                if let Some(handled) = failed.h {
                    self.apply_ack(handled)?;
                }
                self.reset();
                Err(ConnectionError::StreamError(
                    "stream management negotiation failed".to_string(),
                ))
            }
            Nonza::Enable(_) | Nonza::Resume(_) => Err(ConnectionError::StreamError(
                "received unexpected client stream-management nonza from server".to_string(),
            )),
        }
    }

    fn is_resumable(&self) -> bool {
        self.resume_supported && self.stream_id.is_some()
    }

    fn apply_ack(&mut self, handled: u32) -> Result<(), ConnectionError> {
        let newly_acked = handled.wrapping_sub(self.last_acked_by_server) as usize;
        if newly_acked > self.unacked_stanzas.len() {
            return Err(ConnectionError::StreamError(format!(
                "server acknowledged {newly_acked} stanza(s), but only {} are pending",
                self.unacked_stanzas.len()
            )));
        }

        for _ in 0..newly_acked {
            self.unacked_stanzas.pop_front();
        }
        self.last_acked_by_server = handled;
        Ok(())
    }
}

pub fn decode_nonza(frame: &[u8]) -> Option<Nonza> {
    let xml = std::str::from_utf8(frame).ok()?.trim();
    if xml.is_empty() {
        return None;
    }

    let element = Element::from_str(xml).ok()?;
    Nonza::try_from(element).ok()
}

pub fn encode_nonza(nonza: Nonza) -> Result<Vec<u8>, ConnectionError> {
    let element: Element = nonza.into();
    let mut payload = Vec::new();
    element.write_to(&mut payload).map_err(|error| {
        ConnectionError::StreamError(format!(
            "failed to serialize stream-management nonza: {error}"
        ))
    })?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use xmpp_parsers::sm::{Enabled, Resumed, StreamId};

    use super::*;

    #[test]
    fn first_stream_start_requests_enable_with_resume() {
        let mut manager = StreamManager::new();
        let nonza = manager
            .on_stream_started()
            .expect("expected an initial <enable/> request");

        assert!(matches!(nonza, Nonza::Enable(Enable { resume: true, .. })));
        assert_eq!(manager.state(), StreamManagementState::Enabling);
    }

    #[test]
    fn resumed_stream_replays_only_unacked_stanzas() {
        let mut manager = StreamManager::new();
        let _ = manager.on_stream_started();

        manager
            .process_nonza(Nonza::Enabled(Enabled {
                id: Some(StreamId("stream-1".to_string())),
                location: None,
                max: None,
                resume: true,
            }))
            .expect("failed to process <enabled/>");

        manager.track_outbound_stanza(b"<message id='one'/>");
        manager.track_outbound_stanza(b"<message id='two'/>");
        manager.mark_inbound_handled();

        manager.prepare_for_reconnect();
        let resume_request = manager
            .on_stream_started()
            .expect("expected a <resume/> request");
        assert!(matches!(resume_request, Nonza::Resume(Resume { h: 1, .. })));

        let actions = manager
            .process_nonza(Nonza::Resumed(Resumed {
                h: 1,
                previd: StreamId("stream-1".to_string()),
            }))
            .expect("failed to process <resumed/>");

        assert_eq!(manager.state(), StreamManagementState::Enabled);
        assert_eq!(
            actions,
            vec![StreamManagementAction::ReplayStanzas(vec![
                b"<message id='two'/>".to_vec()
            ])]
        );
    }

    #[test]
    fn ack_requests_receive_current_handled_count() {
        let mut manager = StreamManager::new();
        let _ = manager.on_stream_started();
        manager
            .process_nonza(Nonza::Enabled(Enabled {
                id: Some(StreamId("stream-1".to_string())),
                location: None,
                max: None,
                resume: true,
            }))
            .expect("failed to process <enabled/>");

        manager.mark_inbound_handled();
        manager.mark_inbound_handled();

        let actions = manager
            .process_nonza(Nonza::Req(xmpp_parsers::sm::R))
            .expect("failed to process <r/>");

        assert_eq!(
            actions,
            vec![StreamManagementAction::SendNonza(Nonza::Ack(A::new(2)))]
        );
    }
}
