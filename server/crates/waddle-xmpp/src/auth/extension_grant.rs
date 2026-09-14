//! Durable extension authority references; resolving a reference never creates a grant.

use jid::BareJid;
use uuid::Uuid;
use waddle_extensions::PluginId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExtensionGrantId(Uuid);

impl ExtensionGrantId {
    pub fn new(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtensionGrantScope {
    Send,
    ProviderRoom(BareJid),
}

/// An untrusted reference that admission must assert against durable storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionGrantRef {
    pub grant_id: ExtensionGrantId,
    pub plugin: PluginId,
    pub scope: ExtensionGrantScope,
}
