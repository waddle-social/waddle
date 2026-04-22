//! Client-to-server (C2S) connection metadata for the WebSocket server path.
//!
//! The HTTP/WebSocket stack owns RFC 7395 session handling; this module just
//! carries small typed records shared with the rest of the XMPP crate.

use std::net::SocketAddr;

/// C2S connection information.
#[derive(Debug, Clone)]
pub struct C2sConnection {
    /// Peer address
    pub peer_addr: SocketAddr,
    /// Connection ID (for tracking)
    pub id: uuid::Uuid,
}

impl C2sConnection {
    /// Create a new C2S connection record.
    pub fn new(peer_addr: SocketAddr) -> Self {
        Self {
            peer_addr,
            id: uuid::Uuid::new_v4(),
        }
    }
}
