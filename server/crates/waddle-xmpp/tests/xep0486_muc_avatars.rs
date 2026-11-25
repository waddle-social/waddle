//! XEP-0486: MUC Avatars - dedicated suite.
//!
//! A room avatar is a vcard-temp PHOTO published to the room's bare JID;
//! the room advertises each photo's XEP-0153 SHA-1 in the
//! `muc#roominfo_avatarhash` field of its `muc#roominfo` disco#info form.
//! The wire behaviour (authorization, storage, status 104) is covered by
//! `waddle-server/tests/xep0486_muc_avatars_ws.rs`; this suite pins the
//! pure protocol pieces.

use minidom::Element;
use waddle_xmpp::muc::roominfo::{MucRoomInfo, FORM_TYPE_MUC_ROOMINFO};
use waddle_xmpp::xep::xep0153::compute_photo_hash;
use waddle_xmpp::xep::xep0486::{
    avatar_hashes, parse_room_vcard, RoomAvatarUpdate, RoomVCardError, FIELD_ROOMINFO_AVATARHASH,
};

// The SVG avatar of xep-0486.xml's examples, and its raw bytes.
const SVG_BINVAL: &str = "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIzMiIgaGVpZ2h0PSIzMiI+CiA8cmVjdCB4PSIwIiB5PSIwIiB3aWR0aD0iMzIiIGhlaWdodD0iMzIiIGZpbGw9InJlZCIvPgo8L3N2Zz4K";

fn vcard(xml: &str) -> Element {
    xml.parse().expect("valid xml")
}

fn photo_vcard(media_type: &str, binval: &str) -> Element {
    vcard(&format!(
        "<vCard xmlns='vcard-temp'><PHOTO><TYPE>{media_type}</TYPE><BINVAL>{binval}</BINVAL></PHOTO></vCard>"
    ))
}

#[test]
fn field_name_matches_the_registry_submission() {
    assert_eq!(FIELD_ROOMINFO_AVATARHASH, "muc#roominfo_avatarhash");
}

#[test]
fn hash_is_the_xep0153_sha1_of_the_decoded_photo() {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(SVG_BINVAL)
        .expect("base64");
    assert_eq!(
        avatar_hashes(&photo_vcard("image/svg+xml", SVG_BINVAL)),
        Ok(vec![compute_photo_hash(&raw)])
    );
}

#[test]
fn an_empty_vcard_unpublishes_the_avatar() {
    // "Setting an empty vCard unpublishes the avatar."
    assert_eq!(
        parse_room_vcard(&vcard("<vCard xmlns='vcard-temp'/>")),
        Ok(RoomAvatarUpdate::Unpublish)
    );
}

#[test]
fn multiple_photos_are_all_hashed_in_order() {
    let xml = format!(
        "<vCard xmlns='vcard-temp'>\
         <PHOTO><TYPE>image/svg+xml</TYPE><BINVAL>{SVG_BINVAL}</BINVAL></PHOTO>\
         <PHOTO><TYPE>image/png</TYPE><BINVAL>iVBORw0KGgo=</BINVAL></PHOTO>\
         </vCard>"
    );
    let Ok(RoomAvatarUpdate::Publish { hashes }) = parse_room_vcard(&vcard(&xml)) else {
        panic!("expected publish");
    };
    assert_eq!(hashes.len(), 2);
}

#[test]
fn unusable_photos_are_rejected() {
    assert_eq!(
        parse_room_vcard(&vcard("<vCard xmlns='vcard-temp'><FN>Garden</FN></vCard>")),
        Err(RoomVCardError::NoPhoto)
    );
    assert_eq!(
        avatar_hashes(&vcard(
            "<vCard xmlns='vcard-temp'><PHOTO><EXTVAL>https://example.org/a.png</EXTVAL></PHOTO></vCard>"
        )),
        Err(RoomVCardError::IncompletePhoto)
    );
    assert_eq!(
        avatar_hashes(&photo_vcard("image/png", "***")),
        Err(RoomVCardError::InvalidBinval)
    );
}

#[test]
fn roominfo_form_advertises_a_text_multi_hash_field() {
    let form = MucRoomInfo {
        avatar_hashes: vec![
            "a31c4bd04de69663cfd7f424a8453f4674da37ff".to_string(),
            "b9b256f999ded52c2fa14fb007c2e5b979450cbb".to_string(),
        ],
        ..MucRoomInfo::default()
    }
    .to_form_element();
    let field = form
        .children()
        .find(|field| field.attr("var") == Some(FIELD_ROOMINFO_AVATARHASH))
        .expect("avatar hash field");
    assert_eq!(field.attr("type"), Some("text-multi"));
    let values: Vec<String> = field
        .children()
        .filter(|child| child.name() == "value")
        .map(Element::text)
        .collect();
    assert_eq!(
        values,
        vec![
            "a31c4bd04de69663cfd7f424a8453f4674da37ff",
            "b9b256f999ded52c2fa14fb007c2e5b979450cbb"
        ]
    );
    let form_type: Vec<String> = form
        .children()
        .find(|field| field.attr("var") == Some("FORM_TYPE"))
        .expect("FORM_TYPE")
        .children()
        .map(Element::text)
        .collect();
    assert_eq!(form_type, vec![FORM_TYPE_MUC_ROOMINFO.to_string()]);
}

#[test]
fn a_room_without_an_avatar_omits_the_field() {
    // "A room SHOULD NOT include a 'muc#roominfo_avatarhash' field if it
    // doesn't have an avatar set."
    let form = MucRoomInfo::default().to_form_element();
    assert!(!form
        .children()
        .any(|field| field.attr("var") == Some(FIELD_ROOMINFO_AVATARHASH)));
}
