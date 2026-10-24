//! Connection identity and endpoint configuration shared by client transports.

use jid::BareJid;

/// Connection settings shared across native client implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Bare JID of the XMPP server this connection targets.
    pub server: BareJid,
}

impl ConnectionConfig {
    /// Select the bare server JID used when opening XMPP streams.
    #[must_use]
    pub fn new(server: BareJid) -> Self {
        Self { server }
    }
}
