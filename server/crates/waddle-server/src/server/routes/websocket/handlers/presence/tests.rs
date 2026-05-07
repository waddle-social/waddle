use super::*;

mod tests {
    use super::*;

    #[test]
    fn muc_join_presence_includes_owner_and_moderator_hats() {
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
        });

        assert!(
            xml.contains("xmlns=\"urn:xmpp:hats:0\"") || xml.contains("xmlns='urn:xmpp:hats:0'")
        );
        assert!(
            xml.contains("uri=\"urn:xmpp:hats:owner\"")
                || xml.contains("uri='urn:xmpp:hats:owner'")
        );
        assert!(
            xml.contains("uri=\"urn:xmpp:hats:moderator\"")
                || xml.contains("uri='urn:xmpp:hats:moderator'")
        );
        assert!(
            xml.contains("<occupant-id") && xml.contains("urn:xmpp:occupant-id:0"),
            "typed join presence builder must stamp XEP-0421 occupant-id: {xml}"
        );
    }
}
