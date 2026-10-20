//! XEP-0201 thread references and local conversation cache identity.
//!
//! A cache key is deliberately distinct from the XMPP destination. Every
//! send, archive request and read marker still uses the conversation JID.

use waddle_xmpp_client_ffi::WaddleThreadTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    pub parent: Option<String>,
}

impl Thread {
    pub fn from_wire(id: Option<String>, parent: Option<String>) -> Option<Self> {
        let id = id.filter(|id| !id.trim().is_empty())?;
        Some(Self {
            id,
            parent: parent.filter(|parent| !parent.trim().is_empty()),
        })
    }

    pub fn wire(&self) -> WaddleThreadTarget {
        WaddleThreadTarget {
            id: self.id.clone(),
            parent: self.parent.clone(),
        }
    }
}

pub fn conversation_key(jid: &str, thread: Option<&Thread>) -> String {
    match thread {
        // A bare JID cannot contain a newline, so neither a room's own
        // key nor another room/thread pair can collide with this key.
        Some(thread) => format!("{jid}\n{}", thread.id),
        None => jid.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_and_thread_cache_keys_are_isolated() {
        let thread = Thread {
            id: "reply\nchild".into(),
            parent: Some("parent".into()),
        };
        let room = "general@muc.waddle.social";
        assert_eq!(conversation_key(room, None), room);
        assert_ne!(conversation_key(room, Some(&thread)), room);
        assert_ne!(
            conversation_key(room, Some(&thread)),
            conversation_key("random@muc.waddle.social", Some(&thread))
        );
        // Parent metadata does not create a second cache for one thread.
        let other_parent = Thread {
            parent: None,
            ..thread.clone()
        };
        assert_eq!(
            conversation_key(room, Some(&thread)),
            conversation_key(room, Some(&other_parent))
        );
    }

    #[test]
    fn nested_thread_wire_reference_preserves_its_parent() {
        let thread = Thread::from_wire(Some("child".into()), Some("outer".into()))
            .expect("thread reference");
        let wire = thread.wire();
        assert_eq!(wire.id, "child");
        assert_eq!(wire.parent.as_deref(), Some("outer"));
        assert_eq!(Thread::from_wire(Some(" ".into()), None), None);
        assert_eq!(Thread::from_wire(None, Some("outer".into())), None);
    }
}
