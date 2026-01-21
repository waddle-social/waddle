// Route modules for Waddle Server API
pub mod auth; // ATProto OAuth, session management
pub mod channels; // Channel CRUD and permissions
pub mod device; // OAuth Device Flow for CLI
pub mod permissions; // Zanzibar-style permission system
pub mod waddles; // Waddle (community) CRUD operations

// Future route modules will be defined here:
// pub mod messages;  // Message operations
// pub mod uploads;   // File upload endpoints
