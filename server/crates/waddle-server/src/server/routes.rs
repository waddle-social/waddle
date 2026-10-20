// Route modules for Waddle Server API
pub mod auth; // Provider auth broker, session management
pub mod auth_page; // Web-based auth page for XMPP credentials
mod auth_telemetry; // Unified auth rejection/success observability (#1328)
pub mod calendar_feed; // Read-only iCalendar projection of xCal community events
pub mod call_thread_end; // Durable call-thread ended summary + outbox retry
pub mod device; // OAuth Device Flow for CLI
pub mod extension_webhooks; // Extension webhook ingress
pub mod interpret; // OutboundEvent effect interpreter for sans-I/O protocol
mod livekit_grant_relay; // #1594 cross-node routing for participant_joined grant re-assertion
pub mod livekit_webhook; // LiveKit SFU webhook → MUC Muji-presence bridge
pub mod muc_muji_clear; // Shared Muji clear + broadcast for SFU/Jingle teardown
mod sfu_voice_reconcile; // periodic XEP-0045 voice → LiveKit grant convergence
pub mod uploads; // File upload endpoints (XEP-0363)
mod webhook_delivery; // Durable LiveKit webhook delivery ledger
pub mod websocket; // XMPP over WebSocket (RFC 7395)
pub mod well_known; // /.well-known/ endpoints (host-meta, etc.)
pub mod xmpp_oauth; // XMPP OAuth (XEP-0493) for standard XMPP clients

// Future route modules will be defined here:
