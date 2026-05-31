use minidom::Element;
use waddle_xmpp::xep::xep0249::{
    build_direct_invite, build_invite_message, parse_direct_invite_from_message, DirectInvite,
    NS_CONFERENCE,
};
use waddle_xmpp_core::mam::ThreadId;
use xmpp_parsers::message::MessageType;

#[test]
fn xep_0249_direct_invite_payload_matches_schema_shape() {
    let jid = "darkcave@macbeth.shakespeare.lit".parse().unwrap();
    let mut invite = DirectInvite::with_password(
        jid,
        Some("Hey Hecate, this is the place for all good witches!".to_string()),
        "cauldronburn",
    );
    invite.set_continue(ThreadId::new("e0ffe42b28561960c6b12b944a092794b9683a38"));

    let elem = build_direct_invite(&invite);

    assert_eq!(elem.name(), "x");
    assert_eq!(elem.ns(), NS_CONFERENCE);
    assert_eq!(elem.attr("jid"), Some("darkcave@macbeth.shakespeare.lit"));
    assert_eq!(elem.attr("password"), Some("cauldronburn"));
    assert_eq!(
        elem.attr("reason"),
        Some("Hey Hecate, this is the place for all good witches!")
    );
    assert_eq!(elem.attr("continue"), Some("true"));
    assert_eq!(
        elem.attr("thread"),
        Some("e0ffe42b28561960c6b12b944a092794b9683a38")
    );
    assert_eq!(elem.children().count(), 0);
    assert!(elem.text().is_empty());
}

#[test]
fn xep_0249_invite_message_is_typed_normal_stanza_with_only_invite_payload() {
    let from = "crone1@shakespeare.lit/desktop".parse().unwrap();
    let to = "hecate@shakespeare.lit".parse().unwrap();
    let jid = "darkcave@macbeth.shakespeare.lit".parse().unwrap();
    let invite = DirectInvite::with_reason(jid, "Join us!");

    let msg = build_invite_message(&from, &to, &invite);

    assert_eq!(msg.from.as_ref(), Some(&from));
    assert_eq!(msg.to.as_ref(), Some(&to));
    assert_eq!(msg.type_, MessageType::Normal);
    assert!(msg.bodies.is_empty());
    assert!(msg.subjects.is_empty());
    assert!(msg.thread.is_none());
    assert_eq!(msg.payloads.len(), 1);

    let parsed = parse_direct_invite_from_message(&msg).expect("direct invite payload parses");
    assert_eq!(parsed, invite);
}

#[test]
fn xep_0249_typed_serializer_escapes_body_and_payload_attributes() {
    let from = "crone1@shakespeare.lit/desktop".parse().unwrap();
    let to = "hecate@shakespeare.lit".parse().unwrap();
    let jid = "darkcave@macbeth.shakespeare.lit".parse().unwrap();
    let invite = DirectInvite::with_reason(jid, "Join <us> & \"brew\"");

    let msg = build_invite_message(&from, &to, &invite);
    let elem = Element::from(msg.clone());
    let xml = String::from(&elem);

    assert!(xml.contains("Join &lt;us&gt; &amp;"));
    assert!(xml.contains("brew"));
    assert!(!xml.contains("Join <us> &"));

    let parsed = parse_direct_invite_from_message(&msg).expect("direct invite payload parses");
    assert_eq!(parsed.reason.as_deref(), Some("Join <us> & \"brew\""));
}

#[test]
fn xep_0249_rejects_malformed_continue_boolean() {
    let msg = r#"<message xmlns='jabber:client'>
      <x xmlns='jabber:x:conference'
         jid='darkcave@macbeth.shakespeare.lit'
         continue='definitely'/>
    </message>"#;
    let elem: Element = msg.parse().unwrap();
    let parsed = xmpp_parsers::message::Message::try_from(elem).unwrap();

    assert!(parse_direct_invite_from_message(&parsed).is_none());
}

#[test]
fn xep_0249_source_has_no_manual_message_xml_construction() {
    let source = include_str!("../src/xep/xep0249.rs");
    let builder = source
        .split("pub fn build_invite_message")
        .nth(1)
        .and_then(|rest| rest.split("#[cfg(test)]").next())
        .expect("build_invite_message source");
    let forbidden_macro = ["format", "!"].join("");
    let forbidden_escape_helper = ["escape", "_xml"].join("");

    assert!(!builder.contains(&forbidden_macro));
    assert!(!builder.contains(&forbidden_escape_helper));
    assert!(!builder.contains("<message"));
    assert!(!builder.contains("<body"));
    assert!(!builder.contains("String::from"));
}
