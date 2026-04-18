//! Effect interpreter for [`waddle_xmpp::protocol::OutboundEvent`].
//!
//! The state machine in `waddle-xmpp::protocol` is pure and synchronous —
//! it emits outbound events that *describe* side effects but does not
//! perform them. This module is the async counterpart: it pattern-matches
//! each event and runs the real operation against the transport, the
//! connection registry, MUC rooms, MAM storage, the SFU actor, etc.
//!
//! # Current coverage
//!
//! Only a subset of variants have their interpreter wiring in place — the
//! ones needed by handlers that have been migrated so far (ping, session,
//! roster, carbons). The remaining variants are carried by a concrete
//! interpreter bundle already so later migration steps can attach real
//! async behavior without reworking the transport boundary again.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::{
    mam::LibSqlMamStorage,
    muc::MucRoomRegistry,
    protocol::{InboundEvent, OutboundEvent},
    registry::ConnectionRegistry,
};
use waddle_xmpp_xep_github::MessageEnricher;

use super::{auth::AuthState, websocket::WebSocketState};
use crate::server::AppState;

/// Async callback channel used by future two-phase interpreter effects.
pub type CallbackSender = mpsc::Sender<InboundEvent>;

/// Concrete dependency bundle for interpreting outbound effects.
///
/// The interpreter intentionally carries concrete runtime handles instead of
/// new service traits so the transport adapters can share one XMPP runtime
/// shape while migration is still in progress.
#[derive(Clone, Default)]
pub struct EffectInterpreter {
    /// Core server state used by effect implementations that need app-level
    /// resources.
    pub app_state: Option<Arc<AppState>>,
    /// Authentication/session state used by SCRAM and OAUTHBEARER callbacks.
    pub auth_state: Option<Arc<AuthState>>,
    /// Registry for routing direct stanzas to online connections.
    pub connection_registry: Option<Arc<ConnectionRegistry>>,
    /// Registry for MUC occupant lookups and room broadcasts.
    pub muc_registry: Option<Arc<MucRoomRegistry>>,
    /// Shared MAM archive storage.
    pub mam_storage: Option<Arc<LibSqlMamStorage>>,
    /// GitHub message enrichment service.
    pub github_enricher: Option<Arc<MessageEnricher>>,
    /// SFU actor for Jingle/call-related async requests.
    pub sfu_service:
        Option<kameo::actor::ActorRef<waddle_xmpp::sfu::service_actor::SfuServiceActor>>,
    /// Optional callback hook back into the owning connection runtime.
    pub callback_tx: Option<CallbackSender>,
}

impl EffectInterpreter {
    /// Build a concrete interpreter bundle from the current transport/runtime
    /// dependencies.
    pub fn new(
        app_state: Arc<AppState>,
        auth_state: Arc<AuthState>,
        connection_registry: Arc<ConnectionRegistry>,
        muc_registry: Arc<MucRoomRegistry>,
        mam_storage: Arc<LibSqlMamStorage>,
        github_enricher: Arc<MessageEnricher>,
        sfu_service: kameo::actor::ActorRef<waddle_xmpp::sfu::service_actor::SfuServiceActor>,
    ) -> Self {
        Self {
            app_state: Some(app_state),
            auth_state: Some(auth_state),
            connection_registry: Some(connection_registry),
            muc_registry: Some(muc_registry),
            mam_storage: Some(mam_storage),
            github_enricher: Some(github_enricher),
            sfu_service: Some(sfu_service),
            callback_tx: None,
        }
    }

    /// Build an interpreter bundle from the current WebSocket transport state.
    pub fn from_websocket_state(state: &WebSocketState) -> Self {
        Self::new(
            Arc::clone(&state.app_state),
            Arc::clone(&state.auth_state),
            Arc::clone(&state.connection_registry),
            Arc::clone(&state.muc_registry),
            Arc::clone(&state.mam_storage),
            Arc::clone(&state.github_enricher),
            state.sfu_service.clone(),
        )
    }

    /// Attach the async callback hook that later effect variants use to feed
    /// completions back into the connection runtime.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_callback_sender(mut self, callback_tx: CallbackSender) -> Self {
        self.callback_tx = Some(callback_tx);
        self
    }

    /// Execute the side effects described by `events`.
    ///
    /// The function is `async` because future migration steps add variants
    /// that genuinely require `.await` (registry lookups, actor calls, MAM
    /// storage). The currently-supported variants are all synchronous, so this
    /// function returns immediately for the already migrated flows.
    pub async fn interpret(&self, events: Vec<OutboundEvent>) -> InterpretOutcome {
        let mut outcome = InterpretOutcome::default();

        for event in events {
            match event {
                OutboundEvent::SendFrame(xml) => {
                    outcome.frames.push(xml);
                }
                OutboundEvent::CloseTransport => {
                    outcome.close = true;
                }
                OutboundEvent::Log { level, message } => match level {
                    tracing::Level::ERROR => error!(%message, "protocol event"),
                    tracing::Level::WARN => warn!(%message, "protocol event"),
                    tracing::Level::INFO => info!(%message, "protocol event"),
                    tracing::Level::DEBUG | tracing::Level::TRACE => {
                        debug!(%message, "protocol event")
                    }
                },

                unsupported => {
                    if self.handle_unsupported(unsupported) {
                        outcome.frames.clear();
                        outcome.close = true;
                        break;
                    }
                }
            }
        }

        outcome
    }

    fn handle_unsupported(&self, event: OutboundEvent) -> bool {
        let event_name = outbound_event_name(&event);
        let message = format!("OutboundEvent::{event_name} is not wired in EffectInterpreter yet");

        if cfg!(any(test, debug_assertions)) {
            panic!("{message}");
        }

        error!(
            event = event_name,
            has_app_state = self.app_state.is_some(),
            has_auth_state = self.auth_state.is_some(),
            has_connection_registry = self.connection_registry.is_some(),
            has_muc_registry = self.muc_registry.is_some(),
            has_mam_storage = self.mam_storage.is_some(),
            has_github_enricher = self.github_enricher.is_some(),
            has_sfu_service = self.sfu_service.is_some(),
            has_callback_sender = self.callback_tx.is_some(),
            "OutboundEvent variant not yet wired in interpreter"
        );
        true
    }
}

/// Outcome of interpreting a batch of [`OutboundEvent`]s.
///
/// The transport uses `frames` to decide what to write back to the client.
/// `close` signals the main loop should drop the connection.
#[derive(Debug, Default)]
pub struct InterpretOutcome {
    /// Serialized XML frames to write to the transport, in order.
    pub frames: Vec<String>,
    /// Set to true when the state machine asked us to close the transport.
    pub close: bool,
}

fn outbound_event_name(event: &OutboundEvent) -> &'static str {
    match event {
        OutboundEvent::SendFrame(_) => "SendFrame",
        OutboundEvent::CloseTransport => "CloseTransport",
        OutboundEvent::SendDirect { .. } => "SendDirect",
        OutboundEvent::BroadcastToRoom { .. } => "BroadcastToRoom",
        OutboundEvent::RegisterConnection(_) => "RegisterConnection",
        OutboundEvent::UnregisterConnection(_) => "UnregisterConnection",
        OutboundEvent::ArchiveGroupchat { .. } => "ArchiveGroupchat",
        OutboundEvent::ArchiveDirect { .. } => "ArchiveDirect",
        OutboundEvent::RequestEnrichment { .. } => "RequestEnrichment",
        OutboundEvent::AskSfu { .. } => "AskSfu",
        OutboundEvent::QueryMam { .. } => "QueryMam",
        OutboundEvent::LoadScramCredentials { .. } => "LoadScramCredentials",
        OutboundEvent::ValidateOAuthBearer { .. } => "ValidateOAuthBearer",
        OutboundEvent::SetTimer { .. } => "SetTimer",
        OutboundEvent::CancelTimer(_) => "CancelTimer",
        OutboundEvent::Log { .. } => "Log",
    }
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;

    use futures::FutureExt;
    use waddle_xmpp::protocol::CallbackId;

    use super::*;

    #[tokio::test]
    async fn interprets_send_frame() {
        let interpreter = EffectInterpreter::default();
        let events = vec![OutboundEvent::SendFrame(
            "<iq type=\"result\"/>".to_string(),
        )];
        let outcome = interpreter.interpret(events).await;
        assert_eq!(outcome.frames, vec!["<iq type=\"result\"/>"]);
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn interprets_close_transport() {
        let interpreter = EffectInterpreter::default();
        let events = vec![OutboundEvent::CloseTransport];
        let outcome = interpreter.interpret(events).await;
        assert!(outcome.close);
        assert!(outcome.frames.is_empty());
    }

    #[tokio::test]
    async fn interprets_log_is_noop_for_caller() {
        let interpreter = EffectInterpreter::default();
        let events = vec![OutboundEvent::Log {
            level: tracing::Level::INFO,
            message: "hello".to_string(),
        }];
        let outcome = interpreter.interpret(events).await;
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn preserves_frame_order_across_multiple_events() {
        let interpreter = EffectInterpreter::default();
        let events = vec![
            OutboundEvent::SendFrame("<a/>".to_string()),
            OutboundEvent::Log {
                level: tracing::Level::DEBUG,
                message: "between".to_string(),
            },
            OutboundEvent::SendFrame("<b/>".to_string()),
        ];
        let outcome = interpreter.interpret(events).await;
        assert_eq!(outcome.frames, vec!["<a/>", "<b/>"]);
    }

    #[tokio::test]
    async fn callback_sender_can_be_attached() {
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let interpreter = EffectInterpreter::default().with_callback_sender(callback_tx);
        assert!(interpreter.callback_tx.is_some());
    }

    #[tokio::test]
    async fn unsupported_events_fail_loudly_without_leaking_payloads() {
        let interpreter = EffectInterpreter::default();
        let panic =
            AssertUnwindSafe(
                interpreter.interpret(vec![OutboundEvent::ValidateOAuthBearer {
                    id: CallbackId(7),
                    token: "secret-token".to_string(),
                }]),
            )
            .catch_unwind()
            .await
            .expect_err("unsupported event should panic in test builds");

        let panic_message = panic_message(panic);
        assert!(panic_message.contains("ValidateOAuthBearer"));
        assert!(!panic_message.contains("secret-token"));
    }

    #[test]
    fn unsupported_event_names_do_not_expose_sensitive_payloads() {
        let event = OutboundEvent::ValidateOAuthBearer {
            id: CallbackId(7),
            token: "secret-token".to_string(),
        };
        assert_eq!(outbound_event_name(&event), "ValidateOAuthBearer");
    }

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => (*message).to_string(),
                Err(_) => "non-string panic payload".to_string(),
            },
        }
    }
}
