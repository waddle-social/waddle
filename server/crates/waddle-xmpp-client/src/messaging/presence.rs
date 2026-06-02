use minidom::Element;

use super::namespaces::*;
use super::types::*;

pub(super) fn parse_presence(el: &Element) -> InboundPresence {
    let from = el.attr("from").map(String::from);
    let to = el.attr("to").map(String::from);
    let presence_type = el.attr("type").map(String::from);
    let status = el.get_child("status", NS_CLIENT).map(|e| e.text());
    let show = el.get_child("show", NS_CLIENT).map(|e| e.text());

    // XEP-0317: Hats
    let hats = el
        .get_child("hats", NS_HATS)
        .map(|hats_el| {
            hats_el
                .children()
                .filter(|c| c.name() == "hat")
                .filter_map(|hat| {
                    let uri = hat.attr("uri")?.to_string();
                    let title = hat.attr("title")?.to_string();
                    Some(PresenceHat { uri, title })
                })
                .collect()
        })
        .unwrap_or_default();
    let muc_x = el.get_child("x", NS_MUC_USER);
    let muc_item = muc_x.and_then(|x| x.get_child("item", NS_MUC_USER));
    let muc_affiliation = muc_item
        .and_then(|item| item.attr("affiliation"))
        .and_then(MucAffiliation::from_attr);
    let muc_role = muc_item
        .and_then(|item| item.attr("role"))
        .and_then(MucRole::from_attr);
    let muc_jid = muc_item
        .and_then(|item| item.attr("jid"))
        .map(str::to_string);
    // XEP-0045 §7.2.2 self-presence (110) and the other muc#user
    // status codes. Unparseable / unknown codes are preserved via
    // `MucStatus::Other` rather than dropped.
    let muc_status = muc_x
        .map(|x| {
            x.children()
                .filter(|child| child.name() == "status" && child.ns() == NS_MUC_USER)
                .filter_map(|status| status.attr("code"))
                .filter_map(|code| code.parse::<u16>().ok())
                .map(MucStatus::from_code)
                .collect()
        })
        .unwrap_or_default();
    let vcard_avatar = el
        .get_child("x", NS_VCARD_UPDATE)
        .and_then(|x| x.get_child("photo", NS_VCARD_UPDATE))
        .map(|photo| photo.text())
        .filter(|hash| !hash.is_empty());

    // XEP-0272 Muji presence: `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
    // with optional `<preparing/>` and/or `<content/>` children.
    // Active = has at least one `<content/>` (XEP-0272 §Joining).
    // Absent = the occupant has left the call (XEP-0272 §Leaving).
    let muji = el.get_child("muji", NS_MUJI).map(|muji_el| {
        let preparing = muji_el
            .children()
            .any(|c| c.name() == "preparing" && c.ns() == NS_MUJI);
        let mut active = false;
        let mut audio = false;
        let mut video = false;
        for content in muji_el
            .children()
            .filter(|c| c.name() == "content" && c.ns() == NS_MUJI)
        {
            active = true;
            for media in muji_content_media(content) {
                match media {
                    "audio" => audio = true,
                    "video" => video = true,
                    _ => {}
                }
            }
        }
        MujiPresence {
            preparing,
            active,
            audio,
            video,
        }
    });

    InboundPresence {
        from,
        to,
        presence_type,
        status,
        show,
        hats,
        muc_affiliation,
        muc_role,
        muc_jid,
        muc_status,
        vcard_avatar,
        muji,
    }
}

fn muji_content_media(content: &Element) -> impl Iterator<Item = &str> {
    content
        .get_child("description", NS_JINGLE_RTP)
        .and_then(|description| description.attr("media"))
        .into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_muji_active_with_content() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0">
                <content creator="initiator" name="audio">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                </content>
            </muji>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        let muji = p.muji.expect("muji extension parsed");
        assert!(muji.active);
        assert!(muji.audio);
        assert!(!muji.video);
        assert!(!muji.preparing);
    }

    #[test]
    fn parses_muji_video_content_media() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0">
                <content creator="initiator" name="audio">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                </content>
                <content creator="initiator" name="video">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="video"/>
                </content>
            </muji>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        let muji = p.muji.expect("muji extension parsed");
        assert!(muji.active);
        assert!(muji.audio);
        assert!(muji.video);
    }

    #[test]
    fn parses_muji_media_from_rtp_description_not_content_name() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0">
                <content creator="initiator" name="video">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                </content>
            </muji>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        let muji = p.muji.expect("muji extension parsed");
        assert!(muji.active);
        assert!(muji.audio);
        assert!(!muji.video);
    }

    #[test]
    fn parses_muji_preparing_phase() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0"><preparing/></muji>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        let muji = p.muji.expect("preparing muji parsed");
        assert!(muji.preparing);
        assert!(
            !muji.active,
            "preparing-only muji is not yet active (XEP-0272 §Joining two-phase flow)"
        );
        assert!(!muji.audio);
        assert!(!muji.video);
    }

    #[test]
    fn missing_muji_yields_none() {
        // XEP-0272 §Leaving — the absence of the `<muji/>` element is
        // itself the leave marker; the parser surfaces this as `None`.
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice"/>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(p.muji.is_none());
    }

    #[test]
    fn wrong_namespace_is_ignored() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:waddle:not-muji"/>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        assert!(parse_presence(&elem).muji.is_none());
    }

    #[test]
    fn parses_self_presence_status_code_110() {
        // XEP-0045 §7.2.2: the room's self-presence to the joining user
        // MUST carry `<status code='110'/>` so the client knows the
        // presence refers to itself and the roster is complete.
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <x xmlns="http://jabber.org/protocol/muc#user">
                <item affiliation="member" role="participant" jid="alice@test/desktop"/>
                <status code="110"/>
            </x>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(p.muc_status.contains(&MucStatus::SelfPresence));
    }

    #[test]
    fn parses_multiple_muc_status_codes() {
        // Non-anonymous room self-presence carries both 100 and 110.
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <x xmlns="http://jabber.org/protocol/muc#user">
                <item affiliation="owner" role="moderator" jid="alice@test/desktop"/>
                <status code="100"/>
                <status code="110"/>
            </x>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert_eq!(
            p.muc_status,
            vec![MucStatus::NonAnonymous, MucStatus::SelfPresence]
        );
    }

    #[test]
    fn presence_without_status_codes_has_empty_muc_status() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/bob">
            <x xmlns="http://jabber.org/protocol/muc#user">
                <item affiliation="member" role="participant"/>
            </x>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        assert!(parse_presence(&elem).muc_status.is_empty());
    }

    #[test]
    fn unknown_status_code_is_preserved_as_other() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <x xmlns="http://jabber.org/protocol/muc#user">
                <item affiliation="member" role="participant"/>
                <status code="999"/>
            </x>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert_eq!(p.muc_status, vec![MucStatus::Other(999)]);
    }

    #[test]
    fn status_outside_muc_user_x_is_not_collected() {
        // The top-level <status> is XEP-0045-unrelated presence status
        // text (jabber:client). Only `<status code>` children of the
        // muc#user <x> are MUC status codes.
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <status>Away from keyboard</status>
            <x xmlns="http://jabber.org/protocol/muc#user">
                <item affiliation="member" role="participant"/>
                <status code="110"/>
            </x>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert_eq!(p.muc_status, vec![MucStatus::SelfPresence]);
        assert_eq!(p.status.as_deref(), Some("Away from keyboard"));
    }
}
