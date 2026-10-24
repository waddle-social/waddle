//! Request identities, stanza correlation, and pending-request lifecycle tracking.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use minidom::Element;
use xmpp_parsers::{iq::Iq, message::Message, presence::Presence};

use crate::error::{ClientError, ClientResult};

/// Monotonic identifier for client-originated requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

impl RequestId {
    /// Return the underlying numeric request identifier.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Typed wrapper for the client-side IQ-correlation token (the
/// `<iq id='...'/>` value the client puts on outbound IQs and matches
/// replies against).
///
/// **This is NOT a XEP-0359 server-stamped `<stanza-id/>`.** Per
/// XEP-0359 §3 a stanza-id is server-stamped and inseparable from its
/// assigning archive (`by`); the canonical workspace shape for that is
/// [`waddle_xmpp_core::xep0359::StanzaId`]. The client-side
/// correlation id has no `by` context (the client cannot stamp itself)
/// and carries a non-emptiness invariant that the server-stamped type
/// does not enforce — see issue #329 for the full rationale on why
/// these two types stay distinct.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StanzaId(String);

impl StanzaId {
    /// Validate and retain a non-blank client IQ-correlation identifier.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::EmptyStanzaId` when the identifier is blank after trimming.
    pub fn try_new(stanza_id: impl Into<String>) -> ClientResult<Self> {
        let stanza_id = stanza_id.into();
        if stanza_id.trim().is_empty() {
            return Err(ClientError::EmptyStanzaId);
        }

        Ok(Self(stanza_id))
    }

    /// Borrow the stored text without allocating.
    #[must_use]
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
    /// Establish an authenticated XMPP session.
    Connect,
    /// Close the XMPP session explicitly.
    Disconnect,
    /// Send an IQ stanza and correlate its response.
    SendIq {
        /// Identifier used for stanza correlation.
        stanza_id: StanzaId,
        // Boxed because xmpp-parsers 0.22's `Iq` is now an enum with
        // embedded variant payloads (~448 bytes). Boxing keeps the
        // enclosing `ClientRequest` enum compact and satisfies clippy
        // `large_enum_variant` without a lint-suppression escape.
        /// Typed IQ stanza to send.
        iq: Box<Iq>,
    },
    /// Send an outbound message stanza.
    SendMessage {
        /// Typed message payload or human-readable failure description.
        message: Message,
    },
    /// Send an outbound presence stanza.
    SendPresence {
        /// Typed presence stanza to send.
        presence: Presence,
    },
    /// Send an already parsed XML stanza.
    SendElement {
        /// Identifier used for stanza correlation.
        stanza_id: Option<StanzaId>,
        /// Parsed XML element retained at the protocol boundary.
        element: Element,
    },
}

impl ClientRequest {
    /// Return the category used to dispatch or serialize this value.
    #[must_use]
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

    /// Borrow the request's stanza correlation identifier, if it has one.
    #[must_use]
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
    /// Establish an authenticated XMPP session.
    Connect,
    /// Close the XMPP session explicitly.
    Disconnect,
    /// Send an IQ stanza and correlate its response.
    SendIq,
    /// Send an outbound message stanza.
    SendMessage,
    /// Send an outbound presence stanza.
    SendPresence,
    /// Send an already parsed XML stanza.
    SendElement,
}

/// A queued request retained for correlation and completion.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: RequestId,
    /// Typed operation retained until it completes.
    pub request: ClientRequest,
}

impl PendingRequest {
    /// Return the category used to dispatch or serialize this value.
    #[must_use]
    pub fn kind(&self) -> RequestKind {
        self.request.kind()
    }

    /// Borrow the request's stanza correlation identifier, if it has one.
    #[must_use]
    pub fn stanza_id(&self) -> Option<&StanzaId> {
        self.request.stanza_id()
    }

    /// Return owned request and stanza identifiers for completion callbacks.
    #[must_use]
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
    /// Identifier used to correlate the pending client request.
    pub request_id: RequestId,
    /// Identifier used for stanza correlation.
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
    /// Assign a request identifier and register the request and its optional stanza correlation.
    ///
    /// # Errors
    ///
    /// Returns an error if request identifiers are exhausted or the request/stanza correlation identifier is already pending.
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

    /// Return the number of requests still awaiting completion.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Remove and return the pending request with this request identifier.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::UnknownRequest` when the request is not pending.
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

    /// Remove and return the pending request correlated with this stanza identifier.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::UnknownStanzaCorrelation` when no pending request uses this stanza identifier.
    pub fn resolve_by_stanza_id(&mut self, stanza_id: &StanzaId) -> ClientResult<PendingRequest> {
        let request_id = self.pending_by_stanza.get(stanza_id).copied().ok_or(
            ClientError::UnknownStanzaCorrelation {
                stanza_id: stanza_id.clone(),
            },
        )?;
        self.resolve(request_id)
    }

    /// Return the current session state or an owned snapshot of pending requests.
    #[must_use]
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
        let stanza_id = StanzaId::try_new("iq-1").unwrap();
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
        let stanza_id = StanzaId::try_new("iq-1").unwrap();

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
