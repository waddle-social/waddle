use minidom::Element;
use xmpp_parsers::presence::{Presence, Show, Type as PresenceType};

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

    // Waddle in-call presence state (#1029 raised hand / #1030 mute): an
    // `<in-call xmlns='urn:waddle:in-call:0'>` sibling of `<muji/>` whose
    // marker children advertise each sub-state. A missing child (or a
    // missing `<in-call/>`) means that sub-state is off.
    let in_call = el.get_child("in-call", NS_WADDLE_IN_CALL);
    let in_call_has = |name: &str| {
        in_call.is_some_and(|el| {
            el.children()
                .any(|c| c.name() == name && c.ns() == NS_WADDLE_IN_CALL)
        })
    };
    let hand_raised = in_call_has("hand-raised");
    let muted = in_call_has("muted");

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
        hand_raised,
        muted,
    }
}

fn muji_content_media(content: &Element) -> impl Iterator<Item = &str> {
    content
        .get_child("description", NS_JINGLE_RTP)
        .and_then(|description| description.attr("media"))
        .into_iter()
}

/// Build the user's own outbound presence (RFC 6121 §4.7) as a typed
/// [`xmpp_parsers::Presence`]. `show` is an RFC 6121 `<show>` token
/// (`away` / `xa` / `dnd` / `chat`); an unknown or absent token is plain
/// Available (no `<show>`, never an invalid one). An optional free-text
/// `<status>` is set, and the presence advertises XEP-0115 caps.
///
/// The signature mirrors the server's
/// `waddle_xmpp_core::presence::subscription::build_available_presence`, so
/// client and server presence builders stay consistent. Shared by the
/// native [`super::MessagingExt::send_presence`] and the wasm
/// `send_presence` binding; the caller serialises to an [`Element`] at the
/// I/O boundary (`send_stanza`).
pub fn build_presence_stanza(status: Option<&str>, show: Option<&str>) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.show = show.and_then(|token| match token {
        "away" => Some(Show::Away),
        "chat" => Some(Show::Chat),
        "dnd" => Some(Show::Dnd),
        "xa" => Some(Show::Xa),
        _ => None,
    });
    if let Some(text) = status {
        presence
            .statuses
            .insert(xmpp_parsers::message::Lang::new(), text.to_string());
    }
    presence
        .payloads
        .push(crate::caps::build_client_caps_element());
    presence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_presence_with_show_status_and_caps() {
        let presence = build_presence_stanza(Some("Out for lunch"), Some("away"));
        assert_eq!(presence.type_, PresenceType::None);
        assert_eq!(presence.show, Some(Show::Away));
        assert_eq!(
            presence
                .statuses
                .get(&xmpp_parsers::message::Lang::new())
                .map(String::as_str),
            Some("Out for lunch")
        );
        assert!(
            presence.payloads.iter().any(|p| p.name() == "c"),
            "presence advertises XEP-0115 caps"
        );
    }

    #[test]
    fn available_presence_omits_show() {
        // RFC 6121 §4.7.2.1: Available is the absence of a <show>.
        let presence = build_presence_stanza(None, None);
        assert_eq!(presence.type_, PresenceType::None);
        assert!(presence.show.is_none());
        assert!(presence.statuses.is_empty());
        assert!(
            presence.payloads.iter().any(|p| p.name() == "c"),
            "even bare Available advertises caps"
        );
    }

    #[test]
    fn unknown_show_token_degrades_to_available() {
        // A bogus token must never produce an RFC-invalid `<show>`.
        let presence = build_presence_stanza(None, Some("bogus"));
        assert!(presence.show.is_none());
    }

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
    fn parses_in_call_hand_raised_alongside_muji() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0">
                <content creator="initiator" name="audio">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                </content>
            </muji>
            <in-call xmlns="urn:waddle:in-call:0"><hand-raised/></in-call>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(p.muji.expect("muji parsed").active);
        assert!(p.hand_raised, "raised-hand presence state parsed");
    }

    #[test]
    fn lowered_hand_presence_has_no_in_call_child() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0">
                <content creator="initiator" name="audio">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                </content>
            </muji>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(!p.hand_raised, "no in-call child means hand lowered");
        assert!(!p.muted, "no in-call child means unmuted");
    }

    #[test]
    fn parses_in_call_muted_alongside_muji() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <muji xmlns="urn:xmpp:jingle:muji:0">
                <content creator="initiator" name="audio">
                    <description xmlns="urn:xmpp:jingle:apps:rtp:1" media="audio"/>
                </content>
            </muji>
            <in-call xmlns="urn:waddle:in-call:0"><muted/></in-call>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(p.muted, "muted presence state parsed (#1030)");
        assert!(!p.hand_raised, "mute alone does not raise the hand");
    }

    #[test]
    fn parses_in_call_hand_raised_and_muted_together() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <in-call xmlns="urn:waddle:in-call:0"><hand-raised/><muted/></in-call>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(p.hand_raised, "hand parsed when both markers present");
        assert!(p.muted, "mute parsed when both markers present");
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
