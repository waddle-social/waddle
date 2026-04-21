use std::collections::{BTreeMap, HashMap};
use std::fmt;

use minidom::Element;
use xmpp_parsers::{iq::Iq, message::Message, presence::Presence};

use crate::error::{ClientError, ClientResult};

/// Monotonic identifier for client-originated requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Typed wrapper for stanza ids used in IQ correlation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StanzaId(String);

impl StanzaId {
    pub fn new(stanza_id: impl Into<String>) -> ClientResult<Self> {
        let stanza_id = stanza_id.into();
        if stanza_id.trim().is_empty() {
            return Err(ClientError::EmptyStanzaId);
        }

        Ok(Self(stanza_id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StanzaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StanzaId").field(&self.0).finish()
    }
}

impl fmt::Display for StanzaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// High-level client requests queued through the runtime boundary.
#[derive(Debug, Clone)]
pub enum ClientRequest {
    Connect,
    Disconnect,
    SendIq {
        stanza_id: StanzaId,
        iq: Iq,
    },
    SendMessage {
        message: Message,
    },
    SendPresence {
        presence: Presence,
    },
    SendElement {
        stanza_id: Option<StanzaId>,
        element: Element,
    },
}

impl ClientRequest {
    pub fn kind(&self) -> RequestKind {
        match self {
            Self::Connect => RequestKind::Connect,
            Self::Disconnect => RequestKind::Disconnect,
            Self::SendIq { .. } => RequestKind::SendIq,
            Self::SendMessage { .. } => RequestKind::SendMessage,
            Self::SendPresence { .. } => RequestKind::SendPresence,
            Self::SendElement { .. } => RequestKind::SendElement,
        }
    }

    pub fn stanza_id(&self) -> Option<&StanzaId> {
        match self {
            Self::SendIq { stanza_id, .. } => Some(stanza_id),
            Self::SendElement {
                stanza_id: Some(stanza_id),
                ..
            } => Some(stanza_id),
            _ => None,
        }
    }
}

/// Stable request categories exposed in diagnostics and event routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Connect,
    Disconnect,
    SendIq,
    SendMessage,
    SendPresence,
    SendElement,
}

/// A queued request retained for correlation and completion.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    pub id: RequestId,
    pub request: ClientRequest,
}

impl PendingRequest {
    pub fn kind(&self) -> RequestKind {
        self.request.kind()
    }

    pub fn stanza_id(&self) -> Option<&StanzaId> {
        self.request.stanza_id()
    }

    pub fn correlation(&self) -> RequestCorrelation {
        RequestCorrelation {
            request_id: self.id,
            stanza_id: self.stanza_id().cloned(),
        }
    }
}

/// Public correlation token used across runtime and transport callbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCorrelation {
    pub request_id: RequestId,
    pub stanza_id: Option<StanzaId>,
}

/// Tracks in-flight requests and their stanza ids.
#[derive(Debug, Default)]
pub struct RequestTracker {
    next_id: u64,
    pending: BTreeMap<RequestId, PendingRequest>,
    pending_by_stanza: HashMap<StanzaId, RequestId>,
}

impl RequestTracker {
    pub fn register(&mut self, request: ClientRequest) -> ClientResult<PendingRequest> {
        let id = self.next_request_id()?;
        if self.pending.contains_key(&id) {
            return Err(ClientError::DuplicateRequest { request_id: id });
        }

        if let Some(stanza_id) = request.stanza_id() {
            if self.pending_by_stanza.contains_key(stanza_id) {
                return Err(ClientError::DuplicateStanzaCorrelation {
                    stanza_id: stanza_id.clone(),
                });
            }
        }

        let pending = PendingRequest { id, request };

        if let Some(stanza_id) = pending.stanza_id() {
            self.pending_by_stanza.insert(stanza_id.clone(), id);
        }

        self.pending.insert(id, pending.clone());
        Ok(pending)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn resolve(&mut self, request_id: RequestId) -> ClientResult<PendingRequest> {
        let pending = self
            .pending
            .remove(&request_id)
            .ok_or(ClientError::UnknownRequest { request_id })?;

        if let Some(stanza_id) = pending.stanza_id() {
            self.pending_by_stanza.remove(stanza_id);
        }

        Ok(pending)
    }

    pub fn resolve_by_stanza_id(&mut self, stanza_id: &StanzaId) -> ClientResult<PendingRequest> {
        let request_id = self.pending_by_stanza.get(stanza_id).copied().ok_or(
            ClientError::UnknownStanzaCorrelation {
                stanza_id: stanza_id.clone(),
            },
        )?;
        self.resolve(request_id)
    }

    pub fn snapshot(&self) -> Vec<PendingRequest> {
        self.pending.values().cloned().collect()
    }

    fn next_request_id(&mut self) -> ClientResult<RequestId> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ClientError::RequestIdExhausted)?;
        Ok(RequestId(self.next_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_resolves_requests_by_stanza_id() {
        let mut tracker = RequestTracker::default();
        let stanza_id = StanzaId::new("iq-1").unwrap();
        let pending = tracker
            .register(ClientRequest::SendElement {
                stanza_id: Some(stanza_id.clone()),
                element: Element::builder("iq", "jabber:client").build(),
            })
            .unwrap();

        let resolved = tracker.resolve_by_stanza_id(&stanza_id).unwrap();
        assert_eq!(resolved.id, pending.id);
        assert_eq!(tracker.pending_len(), 0);
    }

    #[test]
    fn tracker_rejects_duplicate_stanza_ids() {
        let mut tracker = RequestTracker::default();
        let stanza_id = StanzaId::new("iq-1").unwrap();

        tracker
            .register(ClientRequest::SendElement {
                stanza_id: Some(stanza_id.clone()),
                element: Element::builder("iq", "jabber:client").build(),
            })
            .unwrap();

        let error = tracker
            .register(ClientRequest::SendElement {
                stanza_id: Some(stanza_id),
                element: Element::builder("iq", "jabber:client").build(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ClientError::DuplicateStanzaCorrelation { .. }
        ));
    }
}
