//! Effect interpreter for [`waddle_xmpp::protocol::OutboundEvent`].
//!
//! The state machine in `waddle-xmpp::protocol` is pure and synchronous —
//! it emits typed outbound events that *describe* side effects but does
//! not perform them. This module is the async counterpart: it
//! pattern-matches each event and runs the real operation against the
//! transport, the connection registry, MUC rooms, MAM storage, the SFU
//! actor, etc.
//!
//! # Typed payloads at the I/O boundary
//!
//! Per the project's typed-payloads hard rule, stanzas travel through
//! the state machine as typed values (`Stanza`, `Iq`, `Message`).
//! Serialization to the XML wire format happens here, exactly once,
//! when we hand bytes off to the transport.
//!
//! # Current coverage
//!
//! Only a subset of variants have their interpreter wiring in place — the
//! ones needed by handlers that have been migrated so far (ping, session).
//! The remaining variants (`SendDirect`, `BroadcastToRoom`, `QueryMam`,
//! `AskSfu`, `RequestEnrichment`, etc.) are defined in the event enum so
//! future handlers can emit them, but land in the interpreter as later
//! migration steps pull their XEP into the sans-I/O world.

use tracing::{debug, error, info, warn};
use waddle_xmpp::parser::stanza_to_string;
use waddle_xmpp::protocol::OutboundEvent;

/// Outcome of interpreting a batch of [`OutboundEvent`]s.
///
/// The WebSocket transport uses `frames` to decide what to write back to
/// the client. `close` signals the main loop should drop the connection.
#[derive(Debug, Default)]
pub struct InterpretOutcome {
    /// Serialized XML frames to write to the transport, in order.
    pub frames: Vec<String>,
    /// Set to true when the state machine asked us to close the transport.
    pub close: bool,
}

/// Execute the side effects described by `events`.
///
/// The function is `async` because future migration steps add variants
/// that genuinely require `.await` (registry lookups, actor calls, MAM
/// storage). The currently-supported variants are all synchronous, so this
/// function will return immediately for the ping/session flow.
pub async fn interpret(events: Vec<OutboundEvent>) -> InterpretOutcome {
    let mut outcome = InterpretOutcome::default();

    for event in events {
        match event {
            OutboundEvent::SendStanza(stanza) => match stanza.to_element_string() {
                Ok(xml) => outcome.frames.push(xml),
                Err(err) => {
                    error!(error = %err, "failed to serialize outbound stanza; dropping frame");
                }
            },
            OutboundEvent::CloseTransport => {
                outcome.close = true;
            }
            OutboundEvent::Log { level, message } => {
                // Route the log back through tracing so it ends up in the
                // application's log pipeline. Using a runtime level is
                // slightly noisier than a static one, but it keeps the
                // test-time assertion-on-events capability intact (tests
                // inspect the Vec<OutboundEvent> before interpretation).
                match level {
                    tracing::Level::ERROR => error!(message),
                    tracing::Level::WARN => warn!(message),
                    tracing::Level::INFO => info!(message),
                    tracing::Level::DEBUG | tracing::Level::TRACE => debug!(message),
                }
            }

            // -------------------------------------------------------
            // Variants defined for future migration steps. Logged so the
            // test that emits them can see them appear in traces without
            // accidentally being silently dropped.
            // -------------------------------------------------------
            OutboundEvent::SendDirect { .. }
            | OutboundEvent::BroadcastToRoom { .. }
            | OutboundEvent::RegisterConnection(_)
            | OutboundEvent::UnregisterConnection(_)
            | OutboundEvent::ArchiveGroupchat { .. }
            | OutboundEvent::ArchiveDirect { .. }
            | OutboundEvent::RequestEnrichment { .. }
            | OutboundEvent::AskSfu { .. }
            | OutboundEvent::QueryMam { .. }
            | OutboundEvent::LoadScramCredentials { .. }
            | OutboundEvent::ValidateOAuthBearer { .. }
            | OutboundEvent::SetTimer { .. }
            | OutboundEvent::CancelTimer(_) => {
                warn!(event = ?event, "OutboundEvent variant not yet wired in interpreter");
            }
        }
    }

    outcome
}

/// Helper trait so the interpreter has a single, typed serialization
/// entry point for any `Stanza` leaving the state machine. Keeping it
/// private to this module prevents callers from serializing stanzas in
/// other spots — the I/O boundary stays narrow.
trait ToElementString {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError>;
}

impl ToElementString for waddle_xmpp::connection::Stanza {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError> {
        use waddle_xmpp::connection::Stanza;
        match self {
            Stanza::Iq(iq) => stanza_to_string(iq.clone()),
            Stanza::Message(msg) => stanza_to_string(msg.clone()),
            Stanza::Presence(p) => stanza_to_string(p.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::connection::Stanza;
    use xmpp_parsers::iq::{Iq, IqType};
    use xmpp_parsers::minidom::Element;

    fn result_iq(id: &str) -> Iq {
        Iq {
            from: None,
            to: None,
            id: id.to_string(),
            payload: IqType::Result(Some(Element::builder("query", "jabber:iq:roster").build())),
        }
    }

    #[tokio::test]
    async fn interprets_send_stanza() {
        let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq(
            "x",
        ))))];
        let outcome = interpret(events).await;
        assert_eq!(outcome.frames.len(), 1);
        assert!(outcome.frames[0].contains("type=\"result\""));
        assert!(outcome.frames[0].contains("id=\"x\""));
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn interprets_close_transport() {
        let events = vec![OutboundEvent::CloseTransport];
        let outcome = interpret(events).await;
        assert!(outcome.close);
        assert!(outcome.frames.is_empty());
    }

    #[tokio::test]
    async fn interprets_log_is_noop_for_caller() {
        let events = vec![OutboundEvent::Log {
            level: tracing::Level::INFO,
            message: "hello".to_string(),
        }];
        let outcome = interpret(events).await;
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn preserves_frame_order_across_multiple_events() {
        let events = vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("a")))),
            OutboundEvent::Log {
                level: tracing::Level::DEBUG,
                message: "between".to_string(),
            },
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("b")))),
        ];
        let outcome = interpret(events).await;
        assert_eq!(outcome.frames.len(), 2);
        assert!(outcome.frames[0].contains("id=\"a\""));
        assert!(outcome.frames[1].contains("id=\"b\""));
    }
}
