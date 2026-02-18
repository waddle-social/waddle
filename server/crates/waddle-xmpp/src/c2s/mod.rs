//! Client-to-Server (C2S) connection handling.
//!
//! This module handles the C2S portion of XMPP, including:
//! - Initial stream negotiation
//! - Feature advertisement
//! - Stanza routing to/from clients

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
