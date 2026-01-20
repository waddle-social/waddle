//! Server-to-Server (S2S) federation.
//!
//! This module will handle S2S federation in Phase 5, including:
//! - Server dialback (XEP-0220)
//! - TLS for S2S connections
//! - Remote JID routing
//! - DNS SRV record discovery

// S2S is planned for Phase 5
// Placeholder module for future implementation

/// S2S connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sState {
    /// Initial connection
    Initial,
    /// Dialback in progress
    Dialback,
    /// Authenticated and ready
    Established,
    /// Connection closed
    Closed,
}
