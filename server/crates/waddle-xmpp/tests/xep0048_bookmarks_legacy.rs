//! XEP-0048: Bookmark Storage (legacy `storage:bookmarks`) dedicated suite.
//!
//! Covers the legacy wire format round trip, the autojoin attribute
//! value forms, and lossless translation to/from the native XEP-0402
//! bookmark model used internally.

use minidom::Element;
use waddle_xmpp::xep::xep0402::Bookmark;
use waddle_xmpp::xep::{
    build_legacy_bookmarks_element, from_native_bookmark, is_legacy_bookmarks_namespace,
    parse_legacy_bookmarks, to_native_bookmark, LegacyBookmark, NS_BOOKMARKS_LEGACY,
};

fn reparse(elem: &Element) -> Element {
    String::from(elem)
        .parse::<Element>()
        .expect("serialized storage is well-formed XML")
}

#[test]
fn xep0048_namespace_is_exact() {
    assert_eq!(NS_BOOKMARKS_LEGACY, "storage:bookmarks");
    assert!(is_legacy_bookmarks_namespace("storage:bookmarks"));
    assert!(!is_legacy_bookmarks_namespace("urn:xmpp:bookmarks:1"));
}

#[test]
fn xep0048_spec_example_conference_parses() {
    // Shape from XEP-0048 §3 (Example 1).
    let xml = "<storage xmlns='storage:bookmarks'>\
               <conference name='Council of Oberon' autojoin='true' \
                           jid='council@conference.underhill.org'>\
               <nick>Puck</nick>\
               <password>titania</password>\
               </conference></storage>";
    let elem: Element = xml.parse().expect("valid xml");
    let bookmarks = parse_legacy_bookmarks(&elem);

    assert_eq!(bookmarks.len(), 1);
    let bm = &bookmarks[0];
    assert_eq!(bm.jid, "council@conference.underhill.org");
    assert_eq!(bm.name.as_deref(), Some("Council of Oberon"));
    assert!(bm.autojoin);
    assert_eq!(bm.nick.as_deref(), Some("Puck"));
    assert_eq!(bm.password.as_deref(), Some("titania"));
}

#[test]
fn xep0048_build_and_reparse_round_trips_all_fields() {
    let bookmarks = vec![
        LegacyBookmark {
            jid: "council@conference.underhill.org".to_owned(),
            name: Some("Council of Oberon".to_owned()),
            autojoin: true,
            nick: Some("Puck".to_owned()),
            password: Some("titania".to_owned()),
        },
        LegacyBookmark {
            jid: "plain@muc.example.com".to_owned(),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
        },
    ];

    let elem = reparse(&build_legacy_bookmarks_element(&bookmarks));
    assert_eq!(elem.name(), "storage");
    assert_eq!(elem.ns(), NS_BOOKMARKS_LEGACY);

    let parsed = parse_legacy_bookmarks(&elem);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].jid, bookmarks[0].jid);
    assert_eq!(parsed[0].name, bookmarks[0].name);
    assert_eq!(parsed[0].autojoin, bookmarks[0].autojoin);
    assert_eq!(parsed[0].nick, bookmarks[0].nick);
    assert_eq!(parsed[0].password, bookmarks[0].password);
    assert_eq!(parsed[1].jid, bookmarks[1].jid);
    assert!(!parsed[1].autojoin);
    assert!(parsed[1].nick.is_none());
    assert!(parsed[1].password.is_none());
}

#[test]
fn xep0048_autojoin_accepts_true_and_one_wire_values() {
    for (wire, expected) in [("true", true), ("1", true), ("false", false), ("0", false)] {
        let xml = format!(
            "<storage xmlns='storage:bookmarks'>\
             <conference jid='r@muc.example.com' autojoin='{wire}'/></storage>"
        );
        let elem: Element = xml.parse().expect("valid xml");
        let bookmarks = parse_legacy_bookmarks(&elem);
        assert_eq!(bookmarks[0].autojoin, expected, "autojoin='{wire}'");
    }
}

#[test]
fn xep0048_autojoin_defaults_to_false_when_absent() {
    // XEP-0048: autojoin defaults to false.
    let xml = "<storage xmlns='storage:bookmarks'>\
               <conference jid='r@muc.example.com'/></storage>";
    let elem: Element = xml.parse().expect("valid xml");
    assert!(!parse_legacy_bookmarks(&elem)[0].autojoin);
}

#[test]
fn xep0048_conference_without_jid_is_skipped() {
    let xml = "<storage xmlns='storage:bookmarks'>\
               <conference name='No JID'/>\
               <conference jid='kept@muc.example.com'/></storage>";
    let elem: Element = xml.parse().expect("valid xml");
    let bookmarks = parse_legacy_bookmarks(&elem);
    assert_eq!(bookmarks.len(), 1);
    assert_eq!(bookmarks[0].jid, "kept@muc.example.com");
}

#[test]
fn xep0048_conference_in_wrong_namespace_is_ignored() {
    let xml = "<storage xmlns='storage:bookmarks'>\
               <conference xmlns='urn:example:other' jid='alien@muc.example.com'/>\
               </storage>";
    let elem: Element = xml.parse().expect("valid xml");
    assert!(parse_legacy_bookmarks(&elem).is_empty());
}

#[test]
fn xep0048_native_conversion_round_trips_including_password() {
    let mut native = Bookmark::new("council@conference.underhill.org".parse().expect("jid"))
        .with_name("Council of Oberon")
        .with_autojoin(true)
        .with_nick("Puck");
    native.password = Some("titania".to_owned());

    let legacy = from_native_bookmark(&native);
    assert_eq!(legacy.password.as_deref(), Some("titania"));

    let back = to_native_bookmark(&legacy).expect("valid jid converts");
    assert_eq!(back.jid, native.jid);
    assert_eq!(back.name, native.name);
    assert_eq!(back.autojoin, native.autojoin);
    assert_eq!(back.nick, native.nick);
    assert_eq!(back.password, native.password);
}

#[test]
fn xep0048_invalid_jid_fails_native_conversion() {
    let legacy = LegacyBookmark {
        jid: "not a jid @@@".to_owned(),
        name: None,
        autojoin: false,
        nick: None,
        password: None,
    };
    assert!(to_native_bookmark(&legacy).is_none());
}

#[test]
fn xep0048_full_wire_to_native_pipeline() {
    // Legacy client publishes -> parse -> convert to native -> convert
    // back -> rebuild wire element: nothing may be lost.
    let xml = "<storage xmlns='storage:bookmarks'>\
               <conference jid='theplay@conference.shakespeare.lit' name='The Play' \
                           autojoin='1'><nick>JC</nick></conference></storage>";
    let elem: Element = xml.parse().expect("valid xml");

    let natives: Vec<Bookmark> = parse_legacy_bookmarks(&elem)
        .iter()
        .filter_map(to_native_bookmark)
        .collect();
    assert_eq!(natives.len(), 1);

    let legacy_again: Vec<LegacyBookmark> = natives.iter().map(from_native_bookmark).collect();
    let rebuilt = reparse(&build_legacy_bookmarks_element(&legacy_again));
    let final_parse = parse_legacy_bookmarks(&rebuilt);

    assert_eq!(final_parse.len(), 1);
    assert_eq!(final_parse[0].jid, "theplay@conference.shakespeare.lit");
    assert_eq!(final_parse[0].name.as_deref(), Some("The Play"));
    assert!(final_parse[0].autojoin);
    assert_eq!(final_parse[0].nick.as_deref(), Some("JC"));
}
