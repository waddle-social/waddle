//! XEP-0410: MUC Self-Ping (Extracting the Server-Side Optimization)
//!
//! Allows clients to detect whether they are still joined to a MUC room
//! by pinging their own occupant JID. This avoids silent disconnections
//! where the client thinks it's still in the room but the server has
//! removed it.
//!
//! ## Protocol Flow
//!
//! 1. Client sends a ping (XEP-0199) to its own MUC occupant JID:
//! ```xml
//! <iq type='get' to='room@muc.example.com/mynick' id='sp-1'>
//!   <ping xmlns='urn:xmpp:ping'/>
//! </iq>
//! ```
//!
//! 2. If still in room, server responds with empty result:
//! ```xml
//! <iq type='result' from='room@muc.example.com/mynick' id='sp-1'/>
//! ```
//!
//! 3. If NOT in room, server responds with error:
//! ```xml
//! <iq type='error' from='room@muc.example.com/mynick' id='sp-1'>
//!   <error type='cancel'>
//!     <not-acceptable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
//!   </error>
//! </iq>
//! ```
//!
//! ## Client Behavior
//!
//! - Periodically send self-pings (recommended: every 60 seconds)
//! - On error response: rejoin the room
//! - On timeout: assume disconnected, attempt rejoin
//!
//! ## Service Discovery
//!
//! The MUC service advertises
//! `http://jabber.org/protocol/muc#self-ping-optimization` in disco#info
//! to indicate support for the optimized self-ping flow.

use xmpp_parsers::{
    iq::{Iq, IqType},
    stanza_error::DefinedCondition,
};

/// Feature string for XEP-0410 MUC Self-Ping optimization.
pub const FEATURE_MUC_SELFPING: &str = "http://jabber.org/protocol/muc#self-ping-optimization";

/// Namespace for XEP-0199 Ping (used in self-ping).
const NS_PING: &str = "urn:xmpp:ping";

/// Result of a MUC self-ping check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfPingResult {
    /// Still connected to the room.
    Connected,
    /// No longer in the room (should rejoin).
    Disconnected,
    /// Ping timed out (connection may be lost).
    Timeout,
}

impl SelfPingResult {
    /// Returns `true` if the client should attempt to rejoin.
    pub fn should_rejoin(self) -> bool {
        matches!(self, Self::Disconnected | Self::Timeout)
    }
}

impl std::fmt::Display for SelfPingResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected => f.write_str("connected"),
            Self::Disconnected => f.write_str("disconnected"),
            Self::Timeout => f.write_str("timeout"),
        }
    }
}

/// Build a self-ping IQ to check room membership.
///
/// `occupant_jid` should be the full MUC JID: `room@muc.domain/nick`
pub fn build_self_ping(from: impl Into<Option<jid::Jid>>, occupant_jid: jid::Jid, id: &str) -> Iq {
    let ping_elem = minidom::Element::builder("ping", NS_PING).build();
    Iq {
        from: from.into(),
        to: Some(occupant_jid),
        id: id.to_owned(),
        payload: IqType::Get(ping_elem),
    }
}

/// Check if an IQ is a self-ping request (ping to a MUC occupant JID).
///
/// A self-ping is a regular XEP-0199 ping where the `to` JID has a
/// resource part (indicating it targets a specific occupant).
pub fn is_self_ping(iq: &Iq) -> bool {
    if let IqType::Get(elem) = &iq.payload {
        if elem.name() == "ping" && elem.ns() == NS_PING {
            // Check if 'to' has a resource (occupant JID)
            if let Some(ref to) = iq.to {
                return to.clone().try_into_full().is_ok();
            }
        }
    }
    false
}

/// Interpret a self-ping IQ response.
pub fn interpret_self_ping_response(response: &Iq) -> SelfPingResult {
    match &response.payload {
        IqType::Result(_) => SelfPingResult::Connected,
        IqType::Error(error) => match error.defined_condition {
            DefinedCondition::ServiceUnavailable
            | DefinedCondition::FeatureNotImplemented
            | DefinedCondition::ItemNotFound => SelfPingResult::Connected,
            DefinedCondition::RemoteServerNotFound | DefinedCondition::RemoteServerTimeout => {
                SelfPingResult::Timeout
            }
            _ => SelfPingResult::Disconnected,
        },
        _ => SelfPingResult::Timeout,
    }
}

/// Recommended self-ping interval in seconds.
pub const RECOMMENDED_INTERVAL_SECS: u64 = 60;

/// Ping timeout in seconds before assuming disconnection.
pub const PING_TIMEOUT_SECS: u64 = 10;

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    #[test]
    fn test_build_self_ping() {
        let occupant: jid::Jid = "room@muc.example.com/mynick".parse().expect("valid jid");
        let iq = build_self_ping(None::<jid::Jid>, occupant.clone(), "sp-1");

        assert_eq!(iq.id, "sp-1");
        assert_eq!(iq.to, Some(occupant));
        if let IqType::Get(elem) = &iq.payload {
            assert_eq!(elem.name(), "ping");
            assert_eq!(elem.ns(), NS_PING);
        } else {
            panic!("Expected Get payload");
        }
    }

    #[test]
    fn test_is_self_ping_true() {
        let occupant: jid::Jid = "room@muc.example.com/mynick".parse().expect("valid jid");
        let iq = build_self_ping(None::<jid::Jid>, occupant, "sp-1");
        assert!(is_self_ping(&iq));
    }

    #[test]
    fn test_is_self_ping_false_bare_jid() {
        let bare: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let ping_elem = Element::builder("ping", NS_PING).build();
        let iq = Iq {
            from: None,
            to: Some(bare),
            id: "sp-2".to_owned(),
            payload: IqType::Get(ping_elem),
        };
        // Bare JID → not a self-ping (regular server ping)
        assert!(!is_self_ping(&iq));
    }

    #[test]
    fn test_is_self_ping_false_not_ping() {
        let occupant: jid::Jid = "room@muc.example.com/mynick".parse().expect("valid jid");
        let other_elem = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq {
            from: None,
            to: Some(occupant),
            id: "sp-3".to_owned(),
            payload: IqType::Get(other_elem),
        };
        assert!(!is_self_ping(&iq));
    }

    #[test]
    fn test_interpret_result_connected() {
        let iq = Iq {
            from: Some("room@muc.example.com/mynick".parse().expect("valid")),
            to: None,
            id: "sp-1".to_owned(),
            payload: IqType::Result(None),
        };
        assert_eq!(interpret_self_ping_response(&iq), SelfPingResult::Connected);
    }

    fn error_iq(condition: DefinedCondition) -> Iq {
        Iq {
            from: Some("room@muc.example.com/mynick".parse().expect("valid")),
            to: Some("juliet@example.com/client".parse().expect("valid")),
            id: "sp-err".to_owned(),
            payload: IqType::Error(StanzaError::new(ErrorType::Cancel, condition, "en", "")),
        }
    }

    #[test]
    fn test_interpret_not_acceptable_as_disconnected() {
        assert_eq!(
            interpret_self_ping_response(&error_iq(DefinedCondition::NotAcceptable)),
            SelfPingResult::Disconnected
        );
    }

    #[test]
    fn test_interpret_service_unavailable_as_connected() {
        assert_eq!(
            interpret_self_ping_response(&error_iq(DefinedCondition::ServiceUnavailable)),
            SelfPingResult::Connected
        );
        assert_eq!(
            interpret_self_ping_response(&error_iq(DefinedCondition::FeatureNotImplemented)),
            SelfPingResult::Connected
        );
        assert_eq!(
            interpret_self_ping_response(&error_iq(DefinedCondition::ItemNotFound)),
            SelfPingResult::Connected
        );
    }

    #[test]
    fn test_interpret_remote_server_errors_as_timeout() {
        assert_eq!(
            interpret_self_ping_response(&error_iq(DefinedCondition::RemoteServerNotFound)),
            SelfPingResult::Timeout
        );
        assert_eq!(
            interpret_self_ping_response(&error_iq(DefinedCondition::RemoteServerTimeout)),
            SelfPingResult::Timeout
        );
    }

    #[test]
    fn test_self_ping_result_should_rejoin() {
        assert!(!SelfPingResult::Connected.should_rejoin());
        assert!(SelfPingResult::Disconnected.should_rejoin());
        assert!(SelfPingResult::Timeout.should_rejoin());
    }

    #[test]
    fn test_self_ping_result_display() {
        assert_eq!(SelfPingResult::Connected.to_string(), "connected");
        assert_eq!(SelfPingResult::Disconnected.to_string(), "disconnected");
        assert_eq!(SelfPingResult::Timeout.to_string(), "timeout");
    }

    #[test]
    fn test_constants() {
        assert_eq!(RECOMMENDED_INTERVAL_SECS, 60);
        assert_eq!(PING_TIMEOUT_SECS, 10);
        assert_eq!(
            FEATURE_MUC_SELFPING,
            "http://jabber.org/protocol/muc#self-ping-optimization"
        );
    }
}
