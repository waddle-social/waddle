use std::fmt;

use jid::BareJid;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp_core::mam::ThreadId;

/// Canonical per-message identity resolved by the alias substrate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageKey(Uuid);

impl MessageKey {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_storage(value: Uuid) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> Uuid {
        self.0
    }
}

impl Default for MessageKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MessageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MessageKey(..)")
    }
}

/// Fully opaque per-delivery identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeliveryKey(Uuid);

impl DeliveryKey {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Stable identity for one message's projection into an owner's conversation.
    /// Length-delimited fields keep distinct JIDs and thread ids unambiguous.
    pub fn inbox_projection(
        message_key: MessageKey,
        owner: &BareJid,
        thread: (&BareJid, Option<&ThreadId>),
    ) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"waddle.ingress.inbox_projection.v1");
        hash.update(message_key.to_storage().as_bytes());
        for field in [owner.to_string(), thread.0.to_string()] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        match thread.1 {
            Some(thread_id) => {
                hash.update([1]);
                hash.update(thread_id.as_str().as_bytes());
            }
            None => hash.update([0]),
        }
        let digest = hash.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        // RFC 9562 UUIDv8 reserves the application-defined hash derivation.
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(Uuid::from_bytes(bytes))
    }

    pub fn from_storage(value: Uuid) -> Self {
        Self(value)
    }

    pub fn to_storage(&self) -> Uuid {
        self.0
    }
}

impl Default for DeliveryKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for DeliveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeliveryKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_projection_key_is_stable_and_scoped_to_message_owner_and_thread() {
        let message = MessageKey::new();
        let owner: BareJid = "alice@example.com".parse().expect("owner");
        let peer: BareJid = "bob@example.com".parse().expect("peer");
        let thread = ThreadId::new("thread-1").expect("thread");
        let key = DeliveryKey::inbox_projection(message, &owner, (&peer, None));
        assert_eq!(
            key,
            DeliveryKey::inbox_projection(message, &owner, (&peer, None))
        );
        assert_ne!(
            key,
            DeliveryKey::inbox_projection(MessageKey::new(), &owner, (&peer, None))
        );
        assert_ne!(
            key,
            DeliveryKey::inbox_projection(message, &peer, (&peer, None))
        );
        assert_ne!(
            key,
            DeliveryKey::inbox_projection(message, &owner, (&owner, None))
        );
        assert_ne!(
            key,
            DeliveryKey::inbox_projection(message, &owner, (&peer, Some(&thread)))
        );
    }
}
