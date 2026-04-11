//! XEP-0431: Full Text Search in MAM
//!
//! Extends XEP-0313 (MAM) with full-text search queries. Allows
//! clients to search message archives by keyword.
//!
//! ## XML Format
//!
//! Search query:
//! ```xml
//! <iq type='set' to='room@muc.example.com' id='search-1'>
//!   <query xmlns='urn:xmpp:mam:2' queryid='q1'>
//!     <x xmlns='jabber:x:data' type='submit'>
//!       <field var='FORM_TYPE' type='hidden'>
//!         <value>urn:xmpp:mam:2</value>
//!       </field>
//!       <field var='fulltext'>
//!         <value>search terms</value>
//!       </field>
//!     </x>
//!   </query>
//! </iq>
//! ```

/// MAM data form field for full-text search.
pub const FIELD_FULLTEXT: &str = "fulltext";

/// A MAM search query with optional filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamSearchQuery {
    /// Full-text search terms.
    pub fulltext: String,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// Search within a specific JID (room or contact).
    pub with: Option<String>,
}

impl MamSearchQuery {
    /// Create a new search query.
    pub fn new(fulltext: impl Into<String>) -> Self {
        Self {
            fulltext: fulltext.into(),
            max: None,
            with: None,
        }
    }

    /// Limit results.
    pub fn with_max(mut self, max: u32) -> Self {
        self.max = Some(max);
        self
    }

    /// Filter to a specific JID.
    pub fn with_jid(mut self, jid: impl Into<String>) -> Self {
        self.with = Some(jid.into());
        self
    }

    /// Returns `true` if the query is empty.
    pub fn is_empty(&self) -> bool {
        self.fulltext.trim().is_empty()
    }
}

/// A search result entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Archive ID of the message.
    pub archive_id: String,
    /// Sender JID or nick.
    pub from: String,
    /// Message body.
    pub body: String,
    /// Timestamp.
    pub timestamp: String,
    /// Room JID (for MUC results).
    pub room_jid: Option<String>,
}

impl SearchResult {
    /// Create a new search result.
    pub fn new(
        archive_id: impl Into<String>,
        from: impl Into<String>,
        body: impl Into<String>,
        timestamp: impl Into<String>,
    ) -> Self {
        Self {
            archive_id: archive_id.into(),
            from: from.into(),
            body: body.into(),
            timestamp: timestamp.into(),
            room_jid: None,
        }
    }

    /// Set the room JID.
    pub fn with_room(mut self, room: impl Into<String>) -> Self {
        self.room_jid = Some(room.into());
        self
    }

    /// Extract a text snippet around the search term (context window).
    pub fn snippet(&self, query: &str, context_chars: usize) -> String {
        let lower_body = self.body.to_lowercase();
        let lower_query = query.to_lowercase();

        if let Some(pos) = lower_body.find(&lower_query) {
            let start = pos.saturating_sub(context_chars);
            let end = (pos + query.len() + context_chars).min(self.body.len());

            // Snap to char boundaries
            let start = self.body.floor_char_boundary(start);
            let end = self.body.ceil_char_boundary(end);

            let mut snippet = String::new();
            if start > 0 {
                snippet.push_str("...");
            }
            snippet.push_str(&self.body[start..end]);
            if end < self.body.len() {
                snippet.push_str("...");
            }
            snippet
        } else {
            // No match found, return truncated body
            let end = self.body.len().min(context_chars * 2);
            let end = self.body.ceil_char_boundary(end);
            let mut s = self.body[..end].to_string();
            if end < self.body.len() {
                s.push_str("...");
            }
            s
        }
    }
}

/// Simple in-memory full-text matcher for testing/small archives.
pub fn matches_fulltext(body: &str, query: &str) -> bool {
    let lower = body.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|term| lower.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_new() {
        let q = MamSearchQuery::new("hello world");
        assert_eq!(q.fulltext, "hello world");
        assert!(!q.is_empty());
    }

    #[test]
    fn test_query_empty() {
        assert!(MamSearchQuery::new("").is_empty());
        assert!(MamSearchQuery::new("  ").is_empty());
    }

    #[test]
    fn test_query_with_max() {
        let q = MamSearchQuery::new("test").with_max(20);
        assert_eq!(q.max, Some(20));
    }

    #[test]
    fn test_query_with_jid() {
        let q = MamSearchQuery::new("test").with_jid("room@muc");
        assert_eq!(q.with.as_deref(), Some("room@muc"));
    }

    #[test]
    fn test_search_result_new() {
        let r = SearchResult::new("arc-1", "alice", "Hello world", "2024-06-01T12:00:00Z")
            .with_room("room@muc");
        assert_eq!(r.archive_id, "arc-1");
        assert_eq!(r.from, "alice");
        assert_eq!(r.room_jid.as_deref(), Some("room@muc"));
    }

    #[test]
    fn test_snippet_middle() {
        let r = SearchResult::new("1", "a", "The quick brown fox jumps over the lazy dog", "t");
        let s = r.snippet("fox", 10);
        assert!(s.contains("fox"));
        assert!(s.starts_with("..."));
    }

    #[test]
    fn test_snippet_start() {
        let r = SearchResult::new("1", "a", "Hello world this is a test", "t");
        let s = r.snippet("Hello", 10);
        assert!(s.starts_with("Hello"));
    }

    #[test]
    fn test_snippet_no_match() {
        let r = SearchResult::new("1", "a", "Short message", "t");
        let s = r.snippet("xyz", 20);
        assert_eq!(s, "Short message");
    }

    #[test]
    fn test_matches_fulltext_single() {
        assert!(matches_fulltext("Hello world", "hello"));
        assert!(matches_fulltext("Hello world", "WORLD"));
        assert!(!matches_fulltext("Hello world", "foo"));
    }

    #[test]
    fn test_matches_fulltext_multi() {
        assert!(matches_fulltext("The quick brown fox", "quick fox"));
        assert!(!matches_fulltext("The quick brown fox", "quick cat"));
    }

    #[test]
    fn test_field_constant() {
        assert_eq!(FIELD_FULLTEXT, "fulltext");
    }
}
