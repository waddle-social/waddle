//! XEP-0201 thread references and local conversation cache identity.
//!
//! A cache key is deliberately distinct from the XMPP destination. Every
//! send, archive request and read marker still uses the conversation JID.

use waddle_client_session::IncomingMessage;
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

/// Select content for a thread view after paging the complete archive.
/// Fastenings are routed independently to every cached timeline for the
/// destination so threadless reactions, edits and judgments still apply.
pub fn belongs_to_thread(
    message: &IncomingMessage,
    message_thread: Option<&Thread>,
    selected: Option<&Thread>,
) -> bool {
    let Some(selected) = selected else {
        return message_thread.is_none();
    };
    // The implicit root is shown separately as thread context.
    if message.reference_id.as_deref() == Some(selected.id.as_str()) {
        return false;
    }
    match message_thread {
        Some(thread) => thread.id == selected.id,
        // Older clients used only XEP-0461 to reference a thread's root.
        None => message
            .reply_to
            .as_ref()
            .is_some_and(|reply| reply.id == selected.id),
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

    fn thread(id: &str) -> Thread {
        Thread {
            id: id.into(),
            parent: None,
        }
    }

    #[test]
    fn thread_membership_keeps_only_matching_content() {
        let selected = thread("root");
        let matching = thread("root");
        let other = thread("other");
        let message = IncomingMessage {
            reference_id: Some("reply".into()),
            ..IncomingMessage::default()
        };
        assert!(belongs_to_thread(
            &message,
            Some(&matching),
            Some(&selected)
        ));
        assert!(!belongs_to_thread(&message, Some(&other), Some(&selected)));
        assert!(!belongs_to_thread(&message, None, Some(&selected)));
        assert!(!belongs_to_thread(&message, Some(&matching), None));
        assert!(belongs_to_thread(&message, None, None));
    }

    #[test]
    fn threadless_legacy_replies_belong_to_the_referenced_root() {
        let selected = thread("root");
        let mut message = IncomingMessage {
            reply_to: Some(waddle_client_session::ReplyTo {
                id: "root".into(),
                sender: None,
            }),
            ..IncomingMessage::default()
        };
        assert!(belongs_to_thread(&message, None, Some(&selected)));
        // Legacy replies remain in the main conversation as well.
        assert!(belongs_to_thread(&message, None, None));
        message.reply_to.as_mut().expect("reply").id = "other".into();
        assert!(!belongs_to_thread(&message, None, Some(&selected)));
        // An explicit different thread wins over an old reply reference.
        message.reply_to.as_mut().expect("reply").id = "root".into();
        assert!(!belongs_to_thread(
            &message,
            Some(&thread("other")),
            Some(&selected)
        ));
    }

    #[test]
    fn implicit_thread_roots_are_excluded_from_reply_rows() {
        let selected = thread("root");
        let root = IncomingMessage {
            reference_id: Some("root".into()),
            reply_to: Some(waddle_client_session::ReplyTo {
                id: "root".into(),
                sender: None,
            }),
            ..IncomingMessage::default()
        };
        assert!(!belongs_to_thread(&root, None, Some(&selected)));
        assert!(!belongs_to_thread(&root, Some(&selected), Some(&selected)));
        assert!(belongs_to_thread(&root, None, None));
    }
}
