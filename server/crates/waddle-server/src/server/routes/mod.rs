// Route modules for Waddle Server API
pub mod auth; // ATProto OAuth, session management
pub mod auth_page; // Web-based auth page for XMPP credentials
pub mod channels; // Channel CRUD and permissions
pub mod device; // OAuth Device Flow for CLI
pub mod permissions; // Zanzibar-style permission system
pub mod uploads; // File upload endpoints (XEP-0363)
pub mod waddles; // Waddle (community) CRUD operations
pub mod websocket; // XMPP over WebSocket (RFC 7395)
pub mod well_known; // /.well-known/ endpoints (host-meta, etc.)
pub mod xmpp_oauth; // XMPP OAuth (XEP-0493) for standard XMPP clients

// Future route modules will be defined here:
// pub mod messages;  // Message operations
