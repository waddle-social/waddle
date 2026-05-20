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
    let muc_item = el
        .get_child("x", NS_MUC_USER)
        .and_then(|x| x.get_child("item", NS_MUC_USER));
    let muc_affiliation = muc_item
        .and_then(|item| item.attr("affiliation"))
        .and_then(MucAffiliation::from_attr);
    let muc_role = muc_item
        .and_then(|item| item.attr("role"))
        .and_then(MucRole::from_attr);
    let muc_jid = muc_item
        .and_then(|item| item.attr("jid"))
        .map(str::to_string);
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
        let preparing = muji_el.children().any(|c| c.name() == "preparing");
        let active = muji_el.children().any(|c| c.name() == "content");
        MujiPresence { preparing, active }
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
        vcard_avatar,
        muji,
    }
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
        assert!(!muji.preparing);
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
}
