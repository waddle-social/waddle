// Route modules for Waddle Server API
pub mod auth; // Provider auth broker, session management
pub mod auth_page; // Web-based auth page for XMPP credentials
pub mod device; // OAuth Device Flow for CLI
pub mod interpret; // OutboundEvent effect interpreter for sans-I/O protocol
pub mod uploads; // File upload endpoints (XEP-0363)
pub mod websocket; // XMPP over WebSocket (RFC 7395)
pub mod well_known; // /.well-known/ endpoints (host-meta, etc.)
pub mod xmpp_oauth; // XMPP OAuth (XEP-0493) for standard XMPP clients

// Future route modules will be defined here:
