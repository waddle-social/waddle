use jid::BareJid;

/// Connection settings shared across native client implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub server: BareJid,
}

impl ConnectionConfig {
    pub fn new(server: BareJid) -> Self {
        Self { server }
    }
}
