//! Waddle Story Reads: private per-user read state for XEP-0501 stories.
//!
//! A custom payload (no XEP defines a story-read shape) stored as a single
//! pubsub item on the user's own JID per XEP-0223 "Persistent Storage of
//! Private Data via PubSub". See the design spec at
//! `docs/superpowers/specs/2026-05-19-stories-media-and-reads-design.md`.
//!
//! ## XML Format
//!
//! ```xml
//! <reads xmlns='urn:waddle:story:reads:0'>
//!   <read id='story-aaa' at='2026-05-19T10:11:12Z'/>
//!   <read id='story-bbb' at='2026-05-19T10:13:44Z'/>
//! </reads>
//! ```
//!
//! ## Node options (committed via `<publish-options>` precondition)
//!
//! - `pubsub#persist_items = true`
//! - `pubsub#access_model = whitelist` (private — only owner can fetch)
//! - `pubsub#send_last_published_item = never`
//! - `pubsub#max_items = 1`
//!
//! Omitting any of these in the precondition form lets a server
//! auto-create the node with different defaults (notably an
//! `on_sub_and_presence` send mode) and silently leak read state.

use chrono::{DateTime, Utc};
use minidom::Element;
use std::collections::BTreeMap;

/// XML namespace + PEP node id for the story-reads payload. They
/// coincide deliberately, matching XEP-0163's one-node-per-namespace
/// convention; the two constants exist for call-site readability.
pub const NS_WADDLE_STORY_READS: &str = "urn:waddle:story:reads:0";

/// PEP node id for the story-reads payload. Equal to
/// [`NS_WADDLE_STORY_READS`] by design.
pub const PEP_NODE_WADDLE_STORY_READS: &str = NS_WADDLE_STORY_READS;

/// Single fixed item id, overwritten in place on every publish.
pub const PEP_ITEM_WADDLE_STORY_READS: &str = "current";

/// Defence-in-depth cap on stored entries. Stories expire at 24h and
/// pruning runs at 48h; a user opening 5000 distinct stories inside
/// 48h is implausible, so any blow-past indicates a bug or abuse and
/// the oldest entries are dropped.
pub const READ_ENTRY_MAX: usize = 5000;

/// Validated wrapper around an XEP-0501 pubsub item id.
///
/// Empty strings are rejected at construction so consumers can rely on
/// the type system to enforce non-emptiness without re-checking at
/// every call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoryId(String);

impl StoryId {
    /// Construct a [`StoryId`], rejecting empty strings.
    pub fn new(id: impl Into<String>) -> Result<Self, StoryReadsParseError> {
        let value = id.into();
        if value.is_empty() {
            return Err(StoryReadsParseError::EmptyId);
        }
        Ok(Self(value))
    }

    /// Borrow the underlying id as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the inner `String`. Used at the wasm boundary
    /// where JS-side payloads carry the id as a plain string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for StoryId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Errors produced when parsing a `<reads>` element.
#[derive(Debug, thiserror::Error)]
pub enum StoryReadsParseError {
    /// The root element name was not `reads`.
    #[error("wrong element name: expected `reads`, got `{0}`")]
    WrongElementName(String),
    /// The root element namespace was not [`NS_WADDLE_STORY_READS`].
    #[error("wrong namespace: expected `{NS_WADDLE_STORY_READS}`, got `{0}`")]
    WrongNamespace(String),
    /// A `<read>` child had no `id` attribute.
    #[error("<read> missing `id` attribute")]
    MissingId,
    /// A `<read>` child had an empty `id` attribute.
    #[error("<read> has empty `id` attribute")]
    EmptyId,
    /// A `<read>` child had no `at` attribute.
    #[error("<read> missing `at` attribute")]
    MissingAt,
    /// A `<read>` child's `at` attribute was not parseable as RFC 3339.
    #[error("<read> `at` is not RFC 3339: {0}")]
    BadTimestamp(String),
}

/// Per-user story read state.
///
/// Backed by a `BTreeMap<StoryId, DateTime<Utc>>` so uniqueness on
/// `story_id` is enforced structurally — re-marking the same story
/// simply updates the timestamp.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryReads {
    entries: BTreeMap<StoryId, DateTime<Utc>>,
}

impl StoryReads {
    /// Construct an empty read-state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no entries are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `true` when `id` is marked read.
    pub fn contains(&self, id: &StoryId) -> bool {
        self.entries.contains_key(id)
    }

    /// Mark `id` as read at `at`, replacing any prior timestamp.
    pub fn mark_read(&mut self, id: StoryId, at: DateTime<Utc>) {
        self.entries.insert(id, at);
    }

    /// Drop entries with `at < cutoff`.
    pub fn prune_before(&mut self, cutoff: DateTime<Utc>) {
        self.entries.retain(|_, at| *at >= cutoff);
    }

    /// Drop oldest entries (by `at`) until the count is `<= max`.
    pub fn cap_to(&mut self, max: usize) {
        if self.entries.len() <= max {
            return;
        }
        let mut by_age: Vec<(StoryId, DateTime<Utc>)> = self
            .entries
            .iter()
            .map(|(id, at)| (id.clone(), *at))
            .collect();
        by_age.sort_by_key(|(_, at)| *at);
        let drop_count = self.entries.len() - max;
        for (id, _) in by_age.into_iter().take(drop_count) {
            self.entries.remove(&id);
        }
    }

    /// Iterate entries in `StoryId` order.
    pub fn iter(&self) -> impl Iterator<Item = (&StoryId, &DateTime<Utc>)> {
        self.entries.iter()
    }

    /// Build the `<reads/>` element. Entries are emitted in `StoryId`
    /// order so the wire shape is deterministic and tests are stable.
    pub fn build_element(&self) -> Element {
        let mut root = Element::builder("reads", NS_WADDLE_STORY_READS).build();
        for (id, at) in &self.entries {
            let read = Element::builder("read", NS_WADDLE_STORY_READS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str())
                .attr(minidom::rxml::xml_ncname!("at").to_owned(), at.to_rfc3339())
                .build();
            root.append_child(read);
        }
        root
    }

    /// Parse a `<reads/>` element. Unknown child elements and unknown
    /// attributes on `<read>` are ignored for forward-compat.
    pub fn parse(el: &Element) -> Result<Self, StoryReadsParseError> {
        if el.name() != "reads" {
            return Err(StoryReadsParseError::WrongElementName(el.name().to_owned()));
        }
        if el.ns() != NS_WADDLE_STORY_READS {
            return Err(StoryReadsParseError::WrongNamespace(el.ns()));
        }
        let mut out = Self::new();
        for child in el.children() {
            if child.name() != "read" || child.ns() != NS_WADDLE_STORY_READS {
                continue;
            }
            let id_attr = child
                .attr("id")
                .ok_or(StoryReadsParseError::MissingId)?
                .to_owned();
            let id = StoryId::new(id_attr)?;
            let at_attr = child.attr("at").ok_or(StoryReadsParseError::MissingAt)?;
            let at = at_attr
                .parse::<DateTime<Utc>>()
                .map_err(|_| StoryReadsParseError::BadTimestamp(at_attr.to_owned()))?;
            out.entries.insert(id, at);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .expect("valid timestamp")
    }

    #[test]
    fn story_id_rejects_empty() {
        assert!(matches!(
            StoryId::new(""),
            Err(StoryReadsParseError::EmptyId)
        ));
    }

    #[test]
    fn story_id_accepts_non_empty() {
        let id = StoryId::new("story-aaa").expect("non-empty");
        assert_eq!(id.as_str(), "story-aaa");
    }

    #[test]
    fn mark_read_inserts_entry() {
        let mut reads = StoryReads::new();
        let id = StoryId::new("story-a").expect("ok");
        reads.mark_read(id.clone(), ts(2026, 5, 19, 10));
        assert!(reads.contains(&id));
        assert_eq!(reads.len(), 1);
    }

    #[test]
    fn mark_read_updates_existing() {
        let mut reads = StoryReads::new();
        let id = StoryId::new("story-a").expect("ok");
        reads.mark_read(id.clone(), ts(2026, 5, 19, 9));
        reads.mark_read(id.clone(), ts(2026, 5, 19, 11));
        assert_eq!(reads.len(), 1);
        let (_, at) = reads.iter().next().expect("entry");
        assert_eq!(*at, ts(2026, 5, 19, 11));
    }

    #[test]
    fn prune_before_drops_old_entries() {
        let mut reads = StoryReads::new();
        reads.mark_read(StoryId::new("a").expect("ok"), ts(2026, 5, 17, 10));
        reads.mark_read(StoryId::new("b").expect("ok"), ts(2026, 5, 18, 10));
        reads.mark_read(StoryId::new("c").expect("ok"), ts(2026, 5, 19, 10));
        reads.prune_before(ts(2026, 5, 18, 0));
        let kept: Vec<&str> = reads.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(kept, vec!["b", "c"]);
    }

    #[test]
    fn cap_to_drops_oldest_first() {
        let mut reads = StoryReads::new();
        reads.mark_read(StoryId::new("a").expect("ok"), ts(2026, 5, 17, 10));
        reads.mark_read(StoryId::new("b").expect("ok"), ts(2026, 5, 18, 10));
        reads.mark_read(StoryId::new("c").expect("ok"), ts(2026, 5, 19, 10));
        reads.cap_to(2);
        let kept: Vec<&str> = reads.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(kept, vec!["b", "c"]);
    }

    #[test]
    fn cap_to_is_noop_when_under_cap() {
        let mut reads = StoryReads::new();
        reads.mark_read(StoryId::new("a").expect("ok"), ts(2026, 5, 17, 10));
        reads.cap_to(10);
        assert_eq!(reads.len(), 1);
    }

    #[test]
    fn parse_round_trip() {
        let mut reads = StoryReads::new();
        reads.mark_read(StoryId::new("story-a").expect("ok"), ts(2026, 5, 19, 10));
        reads.mark_read(StoryId::new("story-b").expect("ok"), ts(2026, 5, 19, 11));
        let elem = reads.build_element();
        let parsed = StoryReads::parse(&elem).expect("parseable");
        assert_eq!(parsed, reads);
    }

    #[test]
    fn parse_rejects_wrong_element() {
        let xml = "<readz xmlns='urn:waddle:story:reads:0'/>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(matches!(
            StoryReads::parse(&elem),
            Err(StoryReadsParseError::WrongElementName(_))
        ));
    }

    #[test]
    fn parse_rejects_wrong_namespace() {
        let xml = "<reads xmlns='urn:waddle:wrong'/>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(matches!(
            StoryReads::parse(&elem),
            Err(StoryReadsParseError::WrongNamespace(_))
        ));
    }

    #[test]
    fn parse_rejects_missing_id() {
        let xml = "<reads xmlns='urn:waddle:story:reads:0'>\
                    <read at='2026-05-19T10:11:12Z'/>\
                    </reads>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(matches!(
            StoryReads::parse(&elem),
            Err(StoryReadsParseError::MissingId)
        ));
    }

    #[test]
    fn parse_rejects_empty_id() {
        let xml = "<reads xmlns='urn:waddle:story:reads:0'>\
                    <read id='' at='2026-05-19T10:11:12Z'/>\
                    </reads>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(matches!(
            StoryReads::parse(&elem),
            Err(StoryReadsParseError::EmptyId)
        ));
    }

    #[test]
    fn parse_rejects_missing_at() {
        let xml = "<reads xmlns='urn:waddle:story:reads:0'>\
                    <read id='story-a'/>\
                    </reads>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(matches!(
            StoryReads::parse(&elem),
            Err(StoryReadsParseError::MissingAt)
        ));
    }

    #[test]
    fn parse_rejects_bad_timestamp() {
        let xml = "<reads xmlns='urn:waddle:story:reads:0'>\
                    <read id='story-a' at='not-a-date'/>\
                    </reads>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(matches!(
            StoryReads::parse(&elem),
            Err(StoryReadsParseError::BadTimestamp(_))
        ));
    }

    #[test]
    fn parse_ignores_unknown_children() {
        let xml = "<reads xmlns='urn:waddle:story:reads:0'>\
                    <future xmlns='urn:waddle:story:reads:0'/>\
                    <read id='story-a' at='2026-05-19T10:11:12Z'/>\
                    </reads>";
        let elem: Element = xml.parse().expect("valid xml");
        let parsed = StoryReads::parse(&elem).expect("ok");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_ignores_unknown_attrs_on_read() {
        let xml = "<reads xmlns='urn:waddle:story:reads:0'>\
                    <read id='story-a' at='2026-05-19T10:11:12Z' device='phone-1' version='2'/>\
                    </reads>";
        let elem: Element = xml.parse().expect("valid xml");
        let parsed = StoryReads::parse(&elem).expect("ok");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn build_element_is_deterministic_by_id() {
        let mut reads = StoryReads::new();
        reads.mark_read(StoryId::new("story-c").expect("ok"), ts(2026, 5, 19, 9));
        reads.mark_read(StoryId::new("story-a").expect("ok"), ts(2026, 5, 19, 10));
        reads.mark_read(StoryId::new("story-b").expect("ok"), ts(2026, 5, 19, 11));
        let elem = reads.build_element();
        let ids: Vec<&str> = elem
            .children()
            .filter(|c| c.name() == "read")
            .filter_map(|c| c.attr("id"))
            .collect();
        assert_eq!(ids, vec!["story-a", "story-b", "story-c"]);
    }

    #[test]
    fn empty_reads_round_trip() {
        let reads = StoryReads::new();
        let elem = reads.build_element();
        let parsed = StoryReads::parse(&elem).expect("ok");
        assert!(parsed.is_empty());
    }
}
