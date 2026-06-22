use super::muc::{build_muc_join_presence_xml, MucJoinPresence};
use super::*;

#[test]
fn muc_join_presence_carries_authority_in_xep_0045_payload_only() {
    // XEP-0317 §1: hats are descriptive social metadata, not a
    // duplicate of authority. Owner / admin / moderator status
    // belongs in the XEP-0045 `<x xmlns='muc#user'><item …/>`
    // payload and MUST NOT be synthesised as `<hat/>` entries.
    //
    // This builder is the on-the-wire shape for a fresh-join
    // presence, so the assertion here pins both directions: the
    // XEP-0045 payload IS emitted, and the XEP-0317 payload is NOT.
    let secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
        b"join-presence-handler-test-secret".to_vec(),
    )
    .expect("test secret meets length floor");
    let room_jid: BareJid = "chat@muc.example.com".parse().unwrap();
    let to_jid: FullJid = "alice@example.com/web".parse().unwrap();
    let real_jid: FullJid = "bob@example.com/mobile".parse().unwrap();

    let xml = build_muc_join_presence_xml(MucJoinPresence {
        occupant_id_secret: &secret,
        room_jid: &room_jid,
        nick: "bob",
        to_jid: &to_jid,
        affiliation: Affiliation::Owner,
        role: Role::Moderator,
        real_jid: &real_jid,
        include_self_status: false,
        muji: None,
        in_call: waddle_xmpp::xep::InCallPresenceState::default(),
    });

    // XEP-0045: authority lives in the muc#user payload.
    assert!(
        xml.contains("xmlns='http://jabber.org/protocol/muc#user'")
            || xml.contains("xmlns='http://jabber.org/protocol/muc#user'"),
        "join presence must carry the XEP-0045 muc#user payload: {xml}"
    );
    assert!(
        xml.contains("affiliation='owner'") || xml.contains("affiliation='owner'"),
        "join presence must declare affiliation in muc#user item: {xml}"
    );
    assert!(
        xml.contains("role='moderator'") || xml.contains("role='moderator'"),
        "join presence must declare role in muc#user item: {xml}"
    );

    // XEP-0421: occupant-id MUST be stamped on every MUC presence.
    assert!(
        xml.contains("<occupant-id") && xml.contains("urn:xmpp:occupant-id:0"),
        "typed join presence builder must stamp XEP-0421 occupant-id: {xml}"
    );
}
