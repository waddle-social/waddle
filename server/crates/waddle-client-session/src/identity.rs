//! Qualified identities for conversations, Spaces and messages.
//!
//! A bare localpart, a PubSub node name or a stanza id means something only
//! relative to the entity that issued it. Two communities can each have a
//! room `general`, a Space node `main` or a message id `1`, so clients key
//! caches, drafts, unread counts and reactions by these types instead of by
//! the bare strings.
//!
//! * A room is its canonical bare JID. [`BareJid`] normalises case, so two
//!   spellings of one address compare equal and equal localparts on
//!   different domains do not.
//! * A Space is the JID of the PubSub service that hosts it plus the node
//!   name, which is opaque and case sensitive (XEP-0060 §4.6).
//! * A message id is scoped by the conversation it appears in and by the
//!   authority that issued it: the XEP-0359 `by` JID of the stanza id,
//!   which in a room is the room itself.

use std::fmt;

use jid::BareJid;

/// A room, identified by its canonical bare JID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomId(BareJid);

impl RoomId {
    #[must_use]
    pub fn new(room: BareJid) -> Self {
        Self(room)
    }

    #[must_use]
    pub fn as_bare_jid(&self) -> &BareJid {
        &self.0
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A Space: the PubSub service that hosts it and its opaque node name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpaceId {
    service: BareJid,
    node: String,
}

impl SpaceId {
    /// `None` when `node` is empty: a node name carries no identity then.
    #[must_use]
    pub fn new(service: BareJid, node: impl Into<String>) -> Option<Self> {
        let node = node.into();
        if node.is_empty() {
            return None;
        }
        Some(Self { service, node })
    }

    #[must_use]
    pub fn service(&self) -> &BareJid {
        &self.service
    }

    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }
}

/// A message id together with the conversation it was seen in and the
/// authority that issued it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QualifiedMessageId {
    conversation: BareJid,
    authority: BareJid,
    id: String,
}

impl QualifiedMessageId {
    /// `None` when `id` is empty.
    #[must_use]
    pub fn new(conversation: BareJid, authority: BareJid, id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.is_empty() {
            return None;
        }
        Some(Self {
            conversation,
            authority,
            id,
        })
    }

    #[must_use]
    pub fn conversation(&self) -> &BareJid {
        &self.conversation
    }

    #[must_use]
    pub fn authority(&self) -> &BareJid {
        &self.authority
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}
