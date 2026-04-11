//! XEP-0469: Bookmark Pinning
//!
//! Extends XEP-0402 (PEP Native Bookmarks) with a pinning flag so users
//! can pin their favorite channels to the top of their channel list.
//!
//! ## XML Format
//!
//! A pinned bookmark in PEP storage:
//! ```xml
//! <item id='room@muc.example.com' xmlns='http://jabber.org/protocol/pubsub'>
//!   <conference xmlns='urn:xmpp:bookmarks:1' name='General' autojoin='true'>
//!     <pinned xmlns='urn:xmpp:bookmarks-pinning:0'/>
//!   </conference>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - Pin frequently used channels to the top of the sidebar
//! - Visual distinction for pinned vs unpinned channels
//! - Syncs across devices via PEP

use minidom::Element;

/// Namespace for XEP-0469 Bookmark Pinning.
pub const NS_BOOKMARKS_PINNING: &str = "urn:xmpp:bookmarks-pinning:0";

/// Pin state for a bookmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PinState {
    /// Bookmark is pinned (favorite).
    Pinned,
    /// Bookmark is not pinned (normal).
    #[default]
    Unpinned,
}

impl PinState {
    /// Returns `true` if pinned.
    pub fn is_pinned(self) -> bool {
        matches!(self, Self::Pinned)
    }
}

impl std::fmt::Display for PinState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pinned => f.write_str("pinned"),
            Self::Unpinned => f.write_str("unpinned"),
        }
    }
}

/// Trait for types that can carry a pin state.
pub trait Pinnable {
    /// Get the pin state.
    fn pin_state(&self) -> PinState;

    /// Returns `true` if this item is pinned.
    fn is_pinned(&self) -> bool {
        self.pin_state().is_pinned()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<pinned/>` element.
pub fn is_pinned_element(elem: &Element) -> bool {
    elem.ns() == NS_BOOKMARKS_PINNING && elem.name() == "pinned"
}

/// Check if a bookmark conference element has a `<pinned/>` child.
pub fn is_bookmark_pinned(conference_elem: &Element) -> bool {
    conference_elem.children().any(is_pinned_element)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<pinned xmlns='urn:xmpp:bookmarks-pinning:0'/>` element.
pub fn build_pinned_element() -> Element {
    Element::builder("pinned", NS_BOOKMARKS_PINNING).build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a `<pinned/>` element to a bookmark conference element.
pub fn pin_bookmark(conference_elem: &mut Element) {
    if !is_bookmark_pinned(conference_elem) {
        conference_elem.append_child(build_pinned_element());
    }
}

/// Remove the `<pinned/>` element from a bookmark conference element.
pub fn unpin_bookmark(conference_elem: &mut Element) {
    conference_elem.remove_child("pinned", NS_BOOKMARKS_PINNING);
}

/// Set the pin state on a bookmark conference element.
pub fn set_pin_state(conference_elem: &mut Element, state: PinState) {
    match state {
        PinState::Pinned => pin_bookmark(conference_elem),
        PinState::Unpinned => unpin_bookmark(conference_elem),
    }
}

/// Extract pin state from a bookmark conference element.
pub fn get_pin_state(conference_elem: &Element) -> PinState {
    if is_bookmark_pinned(conference_elem) {
        PinState::Pinned
    } else {
        PinState::Unpinned
    }
}

/// Sort bookmarks with pinned items first.
///
/// Preserves relative order within pinned and unpinned groups.
pub fn sort_bookmarks_pinned_first(bookmarks: &mut [(&Element, PinState)]) {
    bookmarks.sort_by_key(|(_, state)| match state {
        PinState::Pinned => 0,
        PinState::Unpinned => 1,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_conference(pinned: bool) -> Element {
        let mut elem = Element::builder("conference", "urn:xmpp:bookmarks:1")
            .attr("name", "General")
            .attr("autojoin", "true")
            .build();
        if pinned {
            elem.append_child(build_pinned_element());
        }
        elem
    }

    #[test]
    fn test_is_pinned_element() {
        let elem = build_pinned_element();
        assert!(is_pinned_element(&elem));

        let wrong = Element::builder("pinned", "jabber:client").build();
        assert!(!is_pinned_element(&wrong));
    }

    #[test]
    fn test_is_bookmark_pinned() {
        assert!(is_bookmark_pinned(&make_conference(true)));
        assert!(!is_bookmark_pinned(&make_conference(false)));
    }

    #[test]
    fn test_get_pin_state() {
        assert_eq!(get_pin_state(&make_conference(true)), PinState::Pinned);
        assert_eq!(get_pin_state(&make_conference(false)), PinState::Unpinned);
    }

    #[test]
    fn test_pin_bookmark() {
        let mut elem = make_conference(false);
        assert!(!is_bookmark_pinned(&elem));

        pin_bookmark(&mut elem);
        assert!(is_bookmark_pinned(&elem));

        // Pinning again is idempotent
        pin_bookmark(&mut elem);
        assert_eq!(elem.children().filter(|c| is_pinned_element(c)).count(), 1);
    }

    #[test]
    fn test_unpin_bookmark() {
        let mut elem = make_conference(true);
        assert!(is_bookmark_pinned(&elem));

        unpin_bookmark(&mut elem);
        assert!(!is_bookmark_pinned(&elem));
        // Conference attributes preserved
        assert_eq!(elem.attr("name"), Some("General"));
    }

    #[test]
    fn test_set_pin_state() {
        let mut elem = make_conference(false);

        set_pin_state(&mut elem, PinState::Pinned);
        assert!(is_bookmark_pinned(&elem));

        set_pin_state(&mut elem, PinState::Unpinned);
        assert!(!is_bookmark_pinned(&elem));
    }

    #[test]
    fn test_pin_state_display() {
        assert_eq!(PinState::Pinned.to_string(), "pinned");
        assert_eq!(PinState::Unpinned.to_string(), "unpinned");
    }

    #[test]
    fn test_pin_state_default() {
        assert_eq!(PinState::default(), PinState::Unpinned);
    }

    #[test]
    fn test_pin_state_is_pinned() {
        assert!(PinState::Pinned.is_pinned());
        assert!(!PinState::Unpinned.is_pinned());
    }

    #[test]
    fn test_sort_bookmarks_pinned_first() {
        let pinned_elem = make_conference(true);
        let unpinned_elem = make_conference(false);
        let pinned_elem2 = make_conference(true);

        let mut bookmarks = vec![
            (&unpinned_elem, PinState::Unpinned),
            (&pinned_elem, PinState::Pinned),
            (&pinned_elem2, PinState::Pinned),
        ];

        sort_bookmarks_pinned_first(&mut bookmarks);

        assert_eq!(bookmarks[0].1, PinState::Pinned);
        assert_eq!(bookmarks[1].1, PinState::Pinned);
        assert_eq!(bookmarks[2].1, PinState::Unpinned);
    }

    #[test]
    fn test_build_pinned_element() {
        let elem = build_pinned_element();
        assert_eq!(elem.name(), "pinned");
        assert_eq!(elem.ns(), NS_BOOKMARKS_PINNING);
    }
}
