// Route modules for Waddle Server API
pub mod auth; // Provider auth broker, session management
pub mod auth_page; // Web-based auth page for XMPP credentials
pub mod channels; // Channel CRUD and permissions
pub mod device; // OAuth Device Flow for CLI
pub mod messages; // Channel message history
pub mod permissions; // Zanzibar-style permission system
pub mod uploads; // File upload endpoints (XEP-0363)
pub mod users; // Authenticated user search
pub mod waddles; // Waddle (community) CRUD operations
pub mod websocket; // XMPP over WebSocket (RFC 7395)
pub mod well_known; // /.well-known/ endpoints (host-meta, etc.)
pub mod xmpp_oauth; // XEP-0493 OAuth Client Login (RFC 7628 + RFC 8414 + RFC 7591)

// Future route modules will be defined here:
