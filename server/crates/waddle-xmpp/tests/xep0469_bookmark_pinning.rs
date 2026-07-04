//! XEP-0469: Bookmark Pinning — dedicated suite.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:bookmarks-pinning:0`,
//! - the wire shape: `<pinned/>` lives inside the XEP-0402
//!   `<extensions/>` container of a `urn:xmpp:bookmarks:1`
//!   `<conference/>` element,
//! - pin/unpin idempotence and container hygiene (unpinning the last
//!   extension removes the empty `<extensions/>`; other extensions
//!   survive a pin/unpin cycle),
//! - integration with the XEP-0402 parser/builder: a pinned bookmark
//!   round-trips through `parse_bookmark`/`build_bookmark_element`,
//! - pinned-first sorting stability.

use minidom::Element;
use waddle_xmpp::xep::xep0402::{build_bookmark_element, parse_bookmark, Bookmark, NS_BOOKMARKS2};
use waddle_xmpp::xep::xep0469::{
    build_pinned_element, get_pin_state, is_bookmark_pinned, is_pinned_element, pin_bookmark,
    set_pin_state, sort_bookmarks_pinned_first, unpin_bookmark, PinState, NS_BOOKMARKS_PINNING,
};

fn conference() -> Element {
    Element::builder("conference", NS_BOOKMARKS2)
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "General")
        .attr(minidom::rxml::xml_ncname!("autojoin").to_owned(), "true")
        .build()
}

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0469_namespaces_match_spec() {
    // xep-0469.xml registrar entry plus the XEP-0402 host namespace.
    assert_eq!(NS_BOOKMARKS_PINNING, "urn:xmpp:bookmarks-pinning:0");
    assert_eq!(NS_BOOKMARKS2, "urn:xmpp:bookmarks:1");
}

// ── Wire shape ───────────────────────────────────────────────────────

#[test]
fn xep0469_pin_places_pinned_inside_extensions_container() {
    // xep-0469.xml §"Pinning an item": the `<pinned/>` element is a
    // child of the bookmark's `<extensions/>` element.
    let mut conf = conference();
    pin_bookmark(&mut conf);

    let xml = String::from(&conf);
    let reparsed: Element = xml.parse().expect("pinned conference reparses");

    let extensions = reparsed
        .get_child("extensions", NS_BOOKMARKS2)
        .expect("<extensions/> container in the bookmarks namespace");
    let pinned = extensions
        .children()
        .find(|c| is_pinned_element(c))
        .expect("<pinned/> child");
    assert_eq!(pinned.ns(), NS_BOOKMARKS_PINNING);
    assert!(
        reparsed.get_child("pinned", NS_BOOKMARKS_PINNING).is_none(),
        "<pinned/> must not be a direct conference child"
    );
}

#[test]
fn xep0469_spec_example_shape_is_recognized_as_pinned() {
    let conf: Element = "<conference xmlns='urn:xmpp:bookmarks:1' name='General' autojoin='true'>\
            <extensions>\
                <pinned xmlns='urn:xmpp:bookmarks-pinning:0'/>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid xml");
    assert!(is_bookmark_pinned(&conf));
    assert_eq!(get_pin_state(&conf), PinState::Pinned);
}

#[test]
fn xep0469_pinned_element_in_wrong_namespace_is_ignored() {
    let conf: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <pinned xmlns='urn:xmpp:evil:0'/>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid xml");
    assert!(!is_bookmark_pinned(&conf));
    assert_eq!(get_pin_state(&conf), PinState::Unpinned);
}

#[test]
fn xep0469_pinned_outside_extensions_is_not_pinned() {
    // A stray `<pinned/>` that is a direct conference child does not
    // satisfy the XEP-0469 shape — only the extensions container
    // counts.
    let conf: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <pinned xmlns='urn:xmpp:bookmarks-pinning:0'/>\
        </conference>"
        .parse()
        .expect("valid xml");
    assert!(!is_bookmark_pinned(&conf));
}

// ── Mutation hygiene ─────────────────────────────────────────────────

#[test]
fn xep0469_pin_is_idempotent() {
    let mut conf = conference();
    pin_bookmark(&mut conf);
    pin_bookmark(&mut conf);
    pin_bookmark(&mut conf);

    let extensions = conf
        .get_child("extensions", NS_BOOKMARKS2)
        .expect("extensions");
    assert_eq!(
        extensions
            .children()
            .filter(|c| is_pinned_element(c))
            .count(),
        1,
        "repeated pinning must keep exactly one <pinned/>"
    );
}

#[test]
fn xep0469_unpin_removes_empty_extensions_container() {
    let mut conf = conference();
    pin_bookmark(&mut conf);
    unpin_bookmark(&mut conf);

    assert!(!is_bookmark_pinned(&conf));
    assert!(
        conf.get_child("extensions", NS_BOOKMARKS2).is_none(),
        "an emptied <extensions/> container must be removed, not left as a ghost"
    );
    assert_eq!(conf.attr("name"), Some("General"));
    assert_eq!(conf.attr("autojoin"), Some("true"));
}

#[test]
fn xep0469_unpin_preserves_sibling_extensions() {
    // XEP-0402 extensions from other specs (e.g. notification
    // settings) share the container; unpinning must only remove the
    // `<pinned/>` child.
    let mut conf: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'><on-mention/></notify>\
                <pinned xmlns='urn:xmpp:bookmarks-pinning:0'/>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid xml");
    assert!(is_bookmark_pinned(&conf));

    unpin_bookmark(&mut conf);

    assert!(!is_bookmark_pinned(&conf));
    let extensions = conf
        .get_child("extensions", NS_BOOKMARKS2)
        .expect("container must survive while other extensions remain");
    assert!(extensions
        .get_child("notify", "urn:xmpp:notification-settings:1")
        .is_some());
}

#[test]
fn xep0469_unpin_without_extensions_is_a_no_op() {
    let mut conf = conference();
    unpin_bookmark(&mut conf);
    assert!(!is_bookmark_pinned(&conf));
    assert_eq!(conf.attr("name"), Some("General"));
}

#[test]
fn xep0469_set_pin_state_round_trip() {
    let mut conf = conference();
    set_pin_state(&mut conf, PinState::Pinned);
    assert_eq!(get_pin_state(&conf), PinState::Pinned);
    set_pin_state(&mut conf, PinState::Unpinned);
    assert_eq!(get_pin_state(&conf), PinState::Unpinned);
}

// ── XEP-0402 integration ─────────────────────────────────────────────

#[test]
fn xep0469_pinned_bookmark_round_trips_through_xep0402_parser() {
    // Build a bookmark via the XEP-0402 builder, pin it via the
    // XEP-0469 mutator, then reparse with the XEP-0402 parser: the
    // pinned extension must surface in `Bookmark::extensions`.
    let bookmark = Bookmark::new("room@muc.example.com".parse().expect("room jid"))
        .with_name("General")
        .with_autojoin(true);
    let mut payload = build_bookmark_element(&bookmark);
    pin_bookmark(&mut payload);

    let xml = String::from(&payload);
    let reparsed: Element = xml.parse().expect("reparses");
    assert!(is_bookmark_pinned(&reparsed));

    let parsed = parse_bookmark("room@muc.example.com", &reparsed).expect("bookmark parses");
    assert_eq!(parsed.name.as_deref(), Some("General"));
    assert!(parsed.autojoin);
    assert_eq!(parsed.extensions.len(), 1);
    assert!(is_pinned_element(&parsed.extensions[0]));
}

#[test]
fn xep0469_rebuilt_bookmark_with_pinned_extension_stays_pinned() {
    // The reverse direction: a Bookmark whose extensions carry
    // `<pinned/>` must serialize back into the XEP-0469 shape.
    let mut bookmark = Bookmark::new("room@muc.example.com".parse().expect("room jid"));
    bookmark.extensions.push(build_pinned_element());

    let payload = build_bookmark_element(&bookmark);
    assert!(is_bookmark_pinned(&payload));
    assert_eq!(get_pin_state(&payload), PinState::Pinned);
}

// ── Sorting ──────────────────────────────────────────────────────────

#[test]
fn xep0469_sort_is_stable_within_pin_groups() {
    let a = conference();
    let b = conference();
    let c = conference();
    let d = conference();

    // Tag order: [unpinned-a, pinned-b, unpinned-c, pinned-d].
    let mut bookmarks = vec![
        (&a, PinState::Unpinned),
        (&b, PinState::Pinned),
        (&c, PinState::Unpinned),
        (&d, PinState::Pinned),
    ];
    sort_bookmarks_pinned_first(&mut bookmarks);

    let states: Vec<PinState> = bookmarks.iter().map(|(_, s)| *s).collect();
    assert_eq!(
        states,
        vec![
            PinState::Pinned,
            PinState::Pinned,
            PinState::Unpinned,
            PinState::Unpinned
        ]
    );
    // Stability: b before d, a before c (identity comparison).
    assert!(std::ptr::eq(bookmarks[0].0, &b));
    assert!(std::ptr::eq(bookmarks[1].0, &d));
    assert!(std::ptr::eq(bookmarks[2].0, &a));
    assert!(std::ptr::eq(bookmarks[3].0, &c));
}
