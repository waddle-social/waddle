//! XEP-0490: Message Displayed Synchronization
//!
//! Synchronizes "last read" position across multiple clients using PEP.
//! When a user reads messages on one device, other devices update their
//! read position automatically.
//!
//! ## XML Format
//!
//! Published to PEP node `urn:xmpp:mds:displayed:0`:
//! ```xml
//! <item id='room@muc.example.com' xmlns='http://jabber.org/protocol/pubsub'>
//!   <displayed xmlns='urn:xmpp:mds:displayed:0'>
//!     <stanza-id xmlns='urn:xmpp:sid:0' id='last-read-msg-id'
//!                by='room@muc.example.com'/>
//!   </displayed>
//! </item>
//! ```
//!
//! ## Protocol Flow
//!
//! 1. User reads messages on device A
//! 2. Device A publishes last-read stanza-id to PEP
//! 3. Server notifies device B via PEP notification
//! 4. Device B updates its read position
//!
//! ## Use Cases
//!
//! - Sync read state across phone, desktop, and web
//! - Show accurate unread counts on all devices
//! - "Mark as read" propagates instantly

use minidom::Element;

/// Namespace for XEP-0490 Message Displayed Synchronization.
pub const NS_MDS_DISPLAYED: &str = "urn:xmpp:mds:displayed:0";

/// PEP node for displayed sync.
pub const PEP_NODE_MDS: &str = "urn:xmpp:mds:displayed:0";

/// Namespace for stanza-id references (XEP-0359).
const NS_SID: &str = "urn:xmpp:sid:0";

/// A displayed sync entry for one conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayedSync {
    /// The conversation JID (room or contact bare JID).
    pub jid: String,
    /// The stanza-id of the last displayed message.
    pub stanza_id: String,
    /// The entity that assigned the stanza-id (room or server JID).
    pub stanza_id_by: String,
}

impl DisplayedSync {
    /// Create a new displayed sync entry.
    pub fn new(
        jid: impl Into<String>,
        stanza_id: impl Into<String>,
        stanza_id_by: impl Into<String>,
    ) -> Self {
        Self {
            jid: jid.into(),
            stanza_id: stanza_id.into(),
            stanza_id_by: stanza_id_by.into(),
        }
    }
}

/// Collection of displayed sync entries across conversations.
#[derive(Debug, Default)]
pub struct DisplayedSyncState {
    entries: std::collections::HashMap<String, DisplayedSync>,
}

impl DisplayedSyncState {
    /// Create empty sync state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the last-displayed position for a conversation.
    pub fn update(&mut self, entry: DisplayedSync) {
        self.entries.insert(entry.jid.clone(), entry);
    }

    /// Get the last-displayed position for a conversation.
    pub fn get(&self, jid: &str) -> Option<&DisplayedSync> {
        self.entries.get(jid)
    }

    /// Get the last-read stanza-id for a conversation.
    pub fn last_read_id(&self, jid: &str) -> Option<&str> {
        self.entries.get(jid).map(|e| e.stanza_id.as_str())
    }

    /// Check if a specific stanza-id has been read in a conversation.
    pub fn is_read(&self, jid: &str, stanza_id: &str) -> bool {
        self.entries
            .get(jid)
            .is_some_and(|e| e.stanza_id == stanza_id)
    }

    /// Get all conversations with sync data.
    pub fn conversations(&self) -> Vec<&DisplayedSync> {
        self.entries.values().collect()
    }

    /// Number of tracked conversations.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no conversations are tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<displayed/>` sync element.
pub fn is_displayed_sync_element(elem: &Element) -> bool {
    elem.ns() == NS_MDS_DISPLAYED && elem.name() == "displayed"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a displayed sync entry from a PEP item.
///
/// `item_id` is the PEP item ID (typically the conversation JID).
pub fn parse_displayed_sync(item_id: &str, displayed_elem: &Element) -> Option<DisplayedSync> {
    if !is_displayed_sync_element(displayed_elem) {
        return None;
    }

    let stanza_id_elem = displayed_elem
        .children()
        .find(|c| c.name() == "stanza-id" && c.ns() == NS_SID)?;

    let id = stanza_id_elem
        .attr("id")
        .filter(|s| !s.is_empty())?
        .to_owned();
    let by = stanza_id_elem
        .attr("by")
        .filter(|s| !s.is_empty())?
        .to_owned();

    Some(DisplayedSync::new(item_id, id, by))
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<displayed/>` element for PEP publication.
pub fn build_displayed_sync_element(sync: &DisplayedSync) -> Element {
    let stanza_id = Element::builder("stanza-id", NS_SID)
        .attr("id", sync.stanza_id.as_str())
        .attr("by", sync.stanza_id_by.as_str())
        .build();

    Element::builder("displayed", NS_MDS_DISPLAYED)
        .append(stanza_id)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_displayed_sync_element() {
        let elem = Element::builder("displayed", NS_MDS_DISPLAYED).build();
        assert!(is_displayed_sync_element(&elem));

        let wrong = Element::builder("displayed", "jabber:client").build();
        assert!(!is_displayed_sync_element(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let sync = DisplayedSync::new("room@muc.example.com", "msg-id-42", "room@muc.example.com");
        let elem = build_displayed_sync_element(&sync);

        assert_eq!(elem.name(), "displayed");
        assert_eq!(elem.ns(), NS_MDS_DISPLAYED);

        let parsed = parse_displayed_sync("room@muc.example.com", &elem).expect("parseable");
        assert_eq!(parsed.jid, "room@muc.example.com");
        assert_eq!(parsed.stanza_id, "msg-id-42");
        assert_eq!(parsed.stanza_id_by, "room@muc.example.com");
    }

    #[test]
    fn test_parse_missing_stanza_id() {
        let elem = Element::builder("displayed", NS_MDS_DISPLAYED).build();
        assert!(parse_displayed_sync("room@muc", &elem).is_none());
    }

    #[test]
    fn test_parse_wrong_element() {
        let elem = Element::builder("other", NS_MDS_DISPLAYED).build();
        assert!(parse_displayed_sync("room@muc", &elem).is_none());
    }

    #[test]
    fn test_displayed_sync_state() {
        let mut state = DisplayedSyncState::new();
        assert!(state.is_empty());

        state.update(DisplayedSync::new("room1@muc", "msg-1", "room1@muc"));
        state.update(DisplayedSync::new("room2@muc", "msg-5", "room2@muc"));

        assert_eq!(state.len(), 2);
        assert!(!state.is_empty());

        assert_eq!(state.last_read_id("room1@muc"), Some("msg-1"));
        assert_eq!(state.last_read_id("room2@muc"), Some("msg-5"));
        assert_eq!(state.last_read_id("room3@muc"), None);

        assert!(state.is_read("room1@muc", "msg-1"));
        assert!(!state.is_read("room1@muc", "msg-2"));
    }

    #[test]
    fn test_state_update_replaces() {
        let mut state = DisplayedSyncState::new();
        state.update(DisplayedSync::new("room@muc", "msg-1", "room@muc"));
        state.update(DisplayedSync::new("room@muc", "msg-5", "room@muc"));

        assert_eq!(state.len(), 1);
        assert_eq!(state.last_read_id("room@muc"), Some("msg-5"));
    }

    #[test]
    fn test_state_conversations() {
        let mut state = DisplayedSyncState::new();
        state.update(DisplayedSync::new("a@muc", "1", "a@muc"));
        state.update(DisplayedSync::new("b@muc", "2", "b@muc"));

        let convos = state.conversations();
        assert_eq!(convos.len(), 2);
    }

    #[test]
    fn test_displayed_sync_new() {
        let s = DisplayedSync::new("room@muc", "id-1", "room@muc");
        assert_eq!(s.jid, "room@muc");
        assert_eq!(s.stanza_id, "id-1");
        assert_eq!(s.stanza_id_by, "room@muc");
    }

    #[test]
    fn test_pep_node() {
        assert_eq!(PEP_NODE_MDS, "urn:xmpp:mds:displayed:0");
    }
}
