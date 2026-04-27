use waddle_xmpp::xep::xep0338::{
    build_group, ContentName, GroupSemantics, FEATURE_RFC5888_GROUPING, NS_JINGLE_GROUPING,
};

#[test]
fn xep0338_builds_bundle_group() {
    let names = [
        ContentName::new("audio").expect("content name"),
        ContentName::new("video").expect("content name"),
    ];
    let elem = build_group(GroupSemantics::Bundle, &names);
    assert_eq!(elem.name(), "group");
    assert_eq!(elem.ns(), NS_JINGLE_GROUPING);
    assert_eq!(elem.attr("semantics"), Some("BUNDLE"));
    assert_eq!(elem.children().count(), 2);
    assert_eq!(
        elem.children().next().and_then(|child| child.attr("name")),
        Some("audio")
    );
}

#[test]
fn xep0338_exposes_rfc5888_feature() {
    assert_eq!(FEATURE_RFC5888_GROUPING, "urn:ietf:rfc:5888");
}
