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

    let muc_call = el.get_child("call", NS_WADDLE_MUC_CALL).and_then(|call| {
        let state = call.attr("state").and_then(MucCallPresenceState::parse)?;
        let call_id = call.attr("call-id")?.to_string();
        Some(MucCallPresence { state, call_id })
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
        muc_call,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_muc_call_extension_active() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <call xmlns="urn:waddle:muc-call:0" state="active" call-id="room@muc.test"/>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        let call = p.muc_call.expect("call extension parsed");
        assert_eq!(call.state, MucCallPresenceState::Active);
        assert_eq!(call.call_id, "room@muc.test");
    }

    #[test]
    fn parses_muc_call_extension_inactive() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <call xmlns="urn:waddle:muc-call:0" state="inactive" call-id="room@muc.test"/>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert_eq!(p.muc_call.unwrap().state, MucCallPresenceState::Inactive);
    }

    #[test]
    fn missing_extension_yields_none() {
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice"/>"#;
        let elem: Element = xml.parse().unwrap();
        let p = parse_presence(&elem);
        assert!(p.muc_call.is_none());
    }

    #[test]
    fn malformed_state_is_dropped() {
        // No `state` attr → extension parser returns None, so the
        // whole field stays None. Permissive parsing as documented.
        let xml = r#"<presence xmlns="jabber:client" from="room@muc.test/alice">
            <call xmlns="urn:waddle:muc-call:0" call-id="room@muc.test"/>
        </presence>"#;
        let elem: Element = xml.parse().unwrap();
        assert!(parse_presence(&elem).muc_call.is_none());
    }
}
