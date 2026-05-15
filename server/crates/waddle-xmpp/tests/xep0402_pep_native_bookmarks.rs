//! XEP-0402: PEP Native Bookmarks dedicated suite.

use minidom::Element;
use waddle_xmpp::xep::xep0402::{build_bookmark_element, parse_bookmark, Bookmark, NS_BOOKMARKS2};

#[test]
fn xep0402_extensions_container_is_parsed_as_extension_payloads() {
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'><on-mention /></notify>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid XEP-0402 bookmark");

    let bookmark = parse_bookmark("room@muc.example.com", &payload).expect("bookmark parses");

    assert_eq!(bookmark.extensions.len(), 1);
    assert!(bookmark.extensions[0].is("notify", "urn:xmpp:notification-settings:1"));
}

#[test]
fn xep0402_builder_wraps_extensions_in_bookmark_extensions_element() {
    let mut bookmark = Bookmark::new("room@muc.example.com".parse().expect("room JID"));
    bookmark.extensions.push(
        "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>"
            .parse()
            .expect("notify payload"),
    );

    let payload = build_bookmark_element(&bookmark);
    let extensions = payload
        .get_child("extensions", NS_BOOKMARKS2)
        .expect("extensions container");

    assert!(extensions
        .get_child("notify", "urn:xmpp:notification-settings:1")
        .is_some());
    assert!(payload
        .get_child("notify", "urn:xmpp:notification-settings:1")
        .is_none());
}
